//! SSH 隧道（X-05 / F-91）：用 `russh` 建链，产出可当 TCP 流使用的 `AsyncRead + AsyncWrite`。
//!
//! 安全边界（F-91）：
//! - 主机密钥校验发生在握手阶段，**先于任何认证凭据发送**；未知 / 变更一律中止。
//! - 多层跳板按配置顺序建链，错误带层号与目标地址。
//! - 本 crate 不保存密码；凭据由调用方在内存中传入。
//!
//! 用法：
//! ```no_run
//! # async fn run() -> Result<(), rustdb_db_tunnel::TunnelError> {
//! use rustdb_db_tunnel::{AuthMethod, HostKeyPolicy, SshHop, TunnelConfig, TunnelTarget};
//! let config = TunnelConfig {
//!     hops: vec![SshHop {
//!         host: "bastion.example.com".into(),
//!         port: 22,
//!         user: "ops".into(),
//!         auth: vec![AuthMethod::KeyFile {
//!             path: "~/.ssh/id_ed25519".into(),
//!             passphrase: None,
//!         }],
//!         host_keys: HostKeyPolicy::KnownHostsDefault,
//!     }],
//!     ..TunnelConfig::default()
//! };
//! let stream = rustdb_db_tunnel::connect(&config, &TunnelTarget { host: "db.internal".into(), port: 5432 }).await?;
//! # let _ = stream;
//! # Ok(())
//! # }
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, AuthResult, Handle, Handler};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub mod known_hosts;
pub mod proxy;

use known_hosts::HostKeyVerdict;
pub use proxy::{ProxyAuth, ProxyConfig, ProxyKind, ProxyOrder};

/// 本地转发监听范围。局域网监听**必须显式开启**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardBind {
    Loopback,
    Lan,
}

impl ForwardBind {
    fn addr(self) -> SocketAddr {
        match self {
            Self::Loopback => SocketAddr::from(([127, 0, 0, 1], 0)),
            Self::Lan => SocketAddr::from(([0, 0, 0, 0], 0)),
        }
    }
}

/// 可当作双向 TCP 流使用的抽象。
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("隧道未配置任何跳板")]
    Empty,
    #[error("SSH 协议错误：{0}")]
    Ssh(#[from] russh::Error),
    #[error("第 {layer} 层 {host}:{port}：{message}")]
    Layer {
        layer: usize,
        host: String,
        port: u16,
        message: String,
    },
    #[error("第 {layer} 层 {host}:{port} 的主机密钥未知（{fingerprint}），已中止；确认前不会发送任何凭据")]
    HostKeyUnknown {
        layer: usize,
        host: String,
        port: u16,
        fingerprint: String,
        /// OpenSSH 公钥行（`ssh-ed25519 AAAA…`），用户确认后写回 known_hosts。
        public_key: String,
    },
    #[error("第 {layer} 层 {host}:{port} 的主机密钥已变更（记录 {recorded}，本次 {actual}），连接被阻断")]
    HostKeyChanged {
        layer: usize,
        host: String,
        port: u16,
        recorded: String,
        actual: String,
        /// 本次拿到的 OpenSSH 公钥行；用户确认后用它替换旧记录。
        public_key: String,
    },
    #[error("第 {layer} 层 {host}:{port} 认证失败（剩余方式：{methods}）")]
    Auth {
        layer: usize,
        host: String,
        port: u16,
        methods: String,
    },
    #[error("读取私钥 {path} 失败：{message}")]
    Key { path: String, message: String },
    #[error("known_hosts 读取失败：{0}")]
    KnownHosts(String),
    #[error("本地端口转发失败：{message}")]
    Forward { message: String },
    #[error("代理 {endpoint} 失败：{message}")]
    Proxy { endpoint: String, message: String },
    #[error("不支持：{0}")]
    Unsupported(String),
}

/// 认证方式（凭据只保存在内存）。
#[derive(Clone, Debug)]
pub enum AuthMethod {
    Password(String),
    KeyFile {
        path: PathBuf,
        passphrase: Option<String>,
    },
    /// `none` 认证（少数测试服务端）。
    None,
    /// SSH agent 认证（尚未接入，见 X-05 后续）。
    Agent,
}

impl AuthMethod {
    /// 界面上的短标签。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Password(_) => "密码",
            Self::KeyFile { passphrase, .. } => {
                if passphrase.is_some() {
                    "私钥 + 口令"
                } else {
                    "私钥"
                }
            }
            Self::None => "无认证",
            Self::Agent => "SSH Agent",
        }
    }
}

/// 主机密钥策略。
#[derive(Clone, Debug)]
pub enum HostKeyPolicy {
    /// 读取并校验指定 known_hosts 文件；未知或变更都中止。
    KnownHosts(PathBuf),
    /// 使用 `$HOME/.ssh/known_hosts`（取不到时按未知处理）。
    KnownHostsDefault,
    /// 信任任意主机密钥：仅供本地测试，UI 不应暴露。
    InsecureAcceptAny,
}

/// 一层 SSH 跳板。
#[derive(Clone, Debug)]
pub struct SshHop {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 认证方式链：**按顺序尝试**，前一个失败才试下一个（私钥 → 私钥+口令 → 密码 → none）。
    pub auth: Vec<AuthMethod>,
    pub host_keys: HostKeyPolicy,
}

/// 隧道配置：跳板按顺序建链，最后一段从最内层跳到目标库。
#[derive(Clone, Debug)]
pub struct TunnelConfig {
    pub hops: Vec<SshHop>,
    /// F-91a：可选代理；`order` 决定它在 SSH 之前还是之后。
    pub proxy: Option<ProxyConfig>,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            hops: Vec::new(),
            proxy: None,
            connect_timeout: Duration::from_secs(15),
            handshake_timeout: Duration::from_secs(20),
        }
    }
}

/// 目标库地址（隧道内网地址）。
#[derive(Clone, Debug)]
pub struct TunnelTarget {
    pub host: String,
    pub port: u16,
}

/// 隧道流：保持各层 SSH 连接存活，关闭即断开整条链路。
pub struct TunnelStream {
    inner: Box<dyn AsyncStream>,
    _chain: Option<Arc<TunnelChain>>,
}

impl std::fmt::Debug for TunnelStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelStream").finish_non_exhaustive()
    }
}

impl AsyncRead for TunnelStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TunnelStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut *self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.inner).poll_shutdown(cx)
    }
}

/// 已建好的跳板链：最后一跳可以反复开通道（每个数据库连接一条）。
pub struct TunnelChain {
    handles: Vec<Arc<Handle<HostKeyHandler>>>,
    /// F-91a：「SSH → 代理」时从最后一跳再经代理连目标。
    proxy: Option<ProxyConfig>,
}

impl TunnelChain {
    /// 按配置顺序建立整条链（握手 + 认证）。
    ///
    /// 主机密钥在这一步校验，**早于任何凭据发送**。
    pub async fn establish(config: &TunnelConfig) -> Result<Self, TunnelError> {
        if config.hops.is_empty() {
            return Err(TunnelError::Empty);
        }
        let mut handles: Vec<Arc<Handle<HostKeyHandler>>> = Vec::new();
        for (index, hop) in config.hops.iter().enumerate() {
            let layer = index + 1;
            let handler = HostKeyHandler {
                layer,
                host: hop.host.clone(),
                port: hop.port,
                policy: hop.host_keys.clone(),
            };
            let ssh_config = Arc::new(client::Config {
                inactivity_timeout: Some(Duration::from_secs(60)),
                keepalive_interval: Some(Duration::from_secs(20)),
                keepalive_max: 3,
                ..client::Config::default()
            });

            let mut handle = if let Some(previous) = handles.last() {
                let channel = previous
                    .channel_open_direct_tcpip(hop.host.clone(), hop.port as u32, "127.0.0.1", 0)
                    .await
                    .map_err(|error| TunnelError::Layer {
                        layer,
                        host: hop.host.clone(),
                        port: hop.port,
                        message: format!("打开上一层转发通道失败：{error}"),
                    })?;
                open_session(
                    ssh_config,
                    channel.into_stream(),
                    handler,
                    config,
                    layer,
                    hop,
                )
                .await?
            } else {
                // 第一层：可选「先代理」。
                let stream: Box<dyn AsyncStream> = match config
                    .proxy
                    .as_ref()
                    .filter(|proxy| proxy.order == ProxyOrder::BeforeTunnel)
                {
                    Some(proxy) => {
                        let target = TunnelTarget {
                            host: hop.host.clone(),
                            port: hop.port,
                        };
                        let stream = proxy::connect(proxy, &target)
                            .await
                            .map_err(|error| proxy_error(proxy, "先于 SSH 跳板", error))?;
                        Box::new(stream)
                    }
                    None => {
                        let tcp = tokio::time::timeout(
                            config.connect_timeout,
                            TcpStream::connect((hop.host.as_str(), hop.port)),
                        )
                        .await
                        .map_err(|_| TunnelError::Layer {
                            layer,
                            host: hop.host.clone(),
                            port: hop.port,
                            message: format!(
                                "TCP 连接超时（{} ms）",
                                config.connect_timeout.as_millis()
                            ),
                        })?
                        .map_err(|error| TunnelError::Layer {
                            layer,
                            host: hop.host.clone(),
                            port: hop.port,
                            message: format!("TCP 连接失败：{error}"),
                        })?;
                        Box::new(tcp)
                    }
                };
                open_session(ssh_config, stream, handler, config, layer, hop).await?
            };

            authenticate(&mut handle, hop, layer).await?;
            handles.push(Arc::new(handle));
        }
        Ok(Self {
            handles,
            proxy: config.proxy.clone(),
        })
    }

    /// 在最内层跳板上开一条到目标的通道（「SSH → 代理」时先到代理）。
    pub async fn open(
        self: &Arc<Self>,
        target: &TunnelTarget,
    ) -> Result<TunnelStream, TunnelError> {
        let layer = self.handles.len();
        let last = self.handles.last().ok_or(TunnelError::Empty)?;
        let after_proxy = self
            .proxy
            .as_ref()
            .filter(|proxy| proxy.order == ProxyOrder::AfterTunnel);
        let (dest_host, dest_port) = match after_proxy {
            Some(proxy) => (proxy.host.trim().to_string(), proxy.port),
            None => (target.host.clone(), target.port),
        };
        let channel = last
            .channel_open_direct_tcpip(dest_host.clone(), dest_port as u32, "127.0.0.1", 0)
            .await
            .map_err(|error| TunnelError::Layer {
                layer,
                host: dest_host,
                port: dest_port,
                message: format!("打开数据库转发通道失败：{error}"),
            })?;
        let stream: Box<dyn AsyncStream> = Box::new(channel.into_stream());
        let stream: Box<dyn AsyncStream> = match after_proxy {
            Some(proxy) => {
                let stream = proxy::handshake(stream, proxy, target)
                    .await
                    .map_err(|error| proxy_error(proxy, "SSH 跳板之后", error))?;
                Box::new(stream)
            }
            None => stream,
        };
        Ok(TunnelStream {
            inner: stream,
            _chain: Some(self.clone()),
        })
    }
}

/// 代理失败统一带上端点与所处阶段，避免只看到底层 IO 错误。
fn proxy_error(config: &ProxyConfig, stage: &str, error: proxy::ProxyError) -> TunnelError {
    TunnelError::Proxy {
        endpoint: config.endpoint(),
        message: if stage.is_empty() {
            error.to_string()
        } else {
            format!("{stage}：{error}")
        },
    }
}

/// 建立隧道并返回目标库的流。
pub async fn connect(
    config: &TunnelConfig,
    target: &TunnelTarget,
) -> Result<TunnelStream, TunnelError> {
    let chain = Arc::new(TunnelChain::establish(config).await?);
    chain.open(target).await
}

/// 本地端口转发：监听本机地址的随机端口，每来一条连接就通过隧道转到目标库。
///
/// 调用方把驱动的 `host` / `port` 改写成这里的地址即可，**无需驱动层感知隧道 / 代理**。
pub struct LocalForward {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl LocalForward {
    /// 实际监听地址（`Lan` 时是 `0.0.0.0:<端口>`）。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 本机客户端应连接的地址；监听在 `0.0.0.0` 时仍返回回环地址。
    pub fn host(&self) -> String {
        if self.addr.ip().is_unspecified() {
            "127.0.0.1".to_string()
        } else {
            self.addr.ip().to_string()
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// 显式开启局域网监听时给出对外地址，供连接提示使用。
    pub fn lan_addr(&self) -> Option<SocketAddr> {
        self.addr.ip().is_unspecified().then_some(self.addr)
    }

    /// 停止转发（会话断开时调用）。
    pub fn stop(&self) {
        let _ = self.stop.send(true);
        self.task.abort();
    }
}

impl Drop for LocalForward {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 启动本地转发（仅回环）。**握手在返回前完成**：主机密钥有问题时直接返回错误，交给上层阻断。
pub async fn start_forward(
    config: &TunnelConfig,
    target: &TunnelTarget,
) -> Result<LocalForward, TunnelError> {
    start_forward_on(config, target, ForwardBind::Loopback).await
}

/// 启动 SSH 隧道本地转发，可显式选择是否对局域网开放。
pub async fn start_forward_on(
    config: &TunnelConfig,
    target: &TunnelTarget,
    bind: ForwardBind,
) -> Result<LocalForward, TunnelError> {
    if config.hops.is_empty() {
        return Err(TunnelError::Empty);
    }
    let chain = Arc::new(TunnelChain::establish(config).await?);
    // 链路可能被服务端断开：这里保存当前链，用不上了就重建。
    let slot = Arc::new(tokio::sync::Mutex::new(Some(chain)));
    let tunnel = config.clone();
    let connect = move |target: TunnelTarget| {
        let slot = slot.clone();
        let config = tunnel.clone();
        async move {
            let mut guard = slot.lock().await;
            let chain = guard.clone().ok_or_else(|| TunnelError::Forward {
                message: "隧道链路已释放".into(),
            })?;
            match chain.open(&target).await {
                Ok(stream) => Ok(Box::new(stream) as Box<dyn AsyncStream>),
                Err(first) => {
                    // 链路可能已断：重建一次再试；重建也失败就报原始错误。
                    let rebuilt = match TunnelChain::establish(&config).await {
                        Ok(chain) => Arc::new(chain),
                        Err(_) => return Err(first),
                    };
                    let stream = rebuilt.open(&target).await?;
                    *guard = Some(rebuilt);
                    Ok(Box::new(stream) as Box<dyn AsyncStream>)
                }
            }
        }
    };
    spawn_forward(connect, target.clone(), bind).await
}

/// 仅走代理（没有 SSH 跳板）的本地转发。
pub async fn start_proxy_forward(
    config: &ProxyConfig,
    target: &TunnelTarget,
    bind: ForwardBind,
) -> Result<LocalForward, TunnelError> {
    let proxy = config.clone();
    let connect = move |target: TunnelTarget| {
        let config = proxy.clone();
        async move {
            let stream = proxy::connect(&config, &target)
                .await
                .map_err(|error| proxy_error(&config, "", error))?;
            Ok(Box::new(stream) as Box<dyn AsyncStream>)
        }
    };
    spawn_forward(connect, target.clone(), bind).await
}

async fn spawn_forward<F, Fut>(
    connect: F,
    target: TunnelTarget,
    bind: ForwardBind,
) -> Result<LocalForward, TunnelError>
where
    F: Fn(TunnelTarget) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Box<dyn AsyncStream>, TunnelError>> + Send + 'static,
{
    let listener = TcpListener::bind(bind.addr())
        .await
        .map_err(|error| TunnelError::Forward {
            message: format!("监听本地端口失败：{error}"),
        })?;
    let addr = listener
        .local_addr()
        .map_err(|error| TunnelError::Forward {
            message: format!("读取本地端口失败：{error}"),
        })?;

    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(forward_loop(listener, stop_rx, connect, target));

    Ok(LocalForward { addr, stop, task })
}

async fn forward_loop<F, Fut>(
    listener: TcpListener,
    mut stop_rx: watch::Receiver<bool>,
    connect: F,
    target: TunnelTarget,
) where
    F: Fn(TunnelTarget) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<Box<dyn AsyncStream>, TunnelError>> + Send + 'static,
{
    loop {
        tokio::select! {
            _ = stop_rx.changed() => break,
            accepted = listener.accept() => {
                let Ok((mut tcp, _peer)) = accepted else { continue };
                let connect = connect.clone();
                let target = target.clone();
                tokio::spawn(async move {
                    match connect(target).await {
                        Ok(mut stream) => {
                            if let Err(error) = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await {
                                log::warn!("隧道转发失败：{error}");
                            }
                        }
                        Err(error) => log::warn!("隧道转发失败：{error}"),
                    }
                });
            }
        }
    }
}

async fn open_session(
    config: Arc<client::Config>,
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    handler: HostKeyHandler,
    tunnel: &TunnelConfig,
    layer: usize,
    hop: &SshHop,
) -> Result<Handle<HostKeyHandler>, TunnelError> {
    let handshake = client::connect_stream(config, stream, handler);
    match tokio::time::timeout(tunnel.handshake_timeout, handshake).await {
        Ok(Ok(handle)) => Ok(handle),
        Ok(Err(error)) => Err(with_layer(error, layer, hop)),
        Err(_) => Err(TunnelError::Layer {
            layer,
            host: hop.host.clone(),
            port: hop.port,
            message: format!(
                "SSH 握手超时（{} ms）",
                tunnel.handshake_timeout.as_millis()
            ),
        }),
    }
}

/// 主机密钥错误保留原样（带指纹），其他错误补上层号。
fn with_layer(error: TunnelError, layer: usize, hop: &SshHop) -> TunnelError {
    match error {
        error @ (TunnelError::HostKeyUnknown { .. }
        | TunnelError::HostKeyChanged { .. }
        | TunnelError::KnownHosts(_)) => error,
        other => TunnelError::Layer {
            layer,
            host: hop.host.clone(),
            port: hop.port,
            message: other.to_string(),
        },
    }
}

/// 按顺序尝试认证方式；全部失败才报错。
async fn authenticate(
    handle: &mut Handle<HostKeyHandler>,
    hop: &SshHop,
    layer: usize,
) -> Result<(), TunnelError> {
    let mut last_methods = String::new();
    let mut last_error: Option<TunnelError> = None;

    for method in &hop.auth {
        let result = match method {
            AuthMethod::Password(password) => handle
                .authenticate_password(&hop.user, password)
                .await
                .map_err(|error| auth_error(error, layer, hop)),
            AuthMethod::KeyFile { path, passphrase } => {
                let key = match russh::keys::load_secret_key(path, passphrase.as_deref()) {
                    Ok(key) => key,
                    Err(error) => {
                        // 口令不对 / 私钥读不出来：记下来试下一种方式。
                        last_error = Some(TunnelError::Key {
                            path: path.display().to_string(),
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                let hash_alg = if key.algorithm().is_rsa() {
                    handle
                        .best_supported_rsa_hash()
                        .await
                        .ok()
                        .flatten()
                        .flatten()
                } else {
                    None
                };
                handle
                    .authenticate_publickey(
                        &hop.user,
                        PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
                    )
                    .await
                    .map_err(|error| auth_error(error, layer, hop))
            }
            AuthMethod::None => handle
                .authenticate_none(&hop.user)
                .await
                .map_err(|error| auth_error(error, layer, hop)),
            AuthMethod::Agent => {
                return Err(TunnelError::Unsupported(
                    "SSH agent 认证暂未接入（X-05 后续）".into(),
                ));
            }
        };
        match result {
            Ok(AuthResult::Success) => return Ok(()),
            Ok(AuthResult::Failure {
                remaining_methods, ..
            }) => {
                last_methods = format!("{remaining_methods:?}");
                last_error = Some(TunnelError::Auth {
                    layer,
                    host: hop.host.clone(),
                    port: hop.port,
                    methods: last_methods.clone(),
                });
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or(TunnelError::Auth {
        layer,
        host: hop.host.clone(),
        port: hop.port,
        methods: last_methods,
    }))
}

fn auth_error(error: russh::Error, layer: usize, hop: &SshHop) -> TunnelError {
    TunnelError::Layer {
        layer,
        host: hop.host.clone(),
        port: hop.port,
        message: format!("认证请求失败：{error}"),
    }
}

/// 握手阶段的回调：校验主机密钥，未通过即中止（此时尚未发送凭据）。
struct HostKeyHandler {
    layer: usize,
    host: String,
    port: u16,
    policy: HostKeyPolicy,
}

impl Handler for HostKeyHandler {
    type Error = TunnelError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(certificate) => ssh_key_of_certificate(certificate),
        };
        match &self.policy {
            HostKeyPolicy::InsecureAcceptAny => Ok(true),
            HostKeyPolicy::KnownHosts(path) => self.verify_known_hosts(Some(path.clone()), &key),
            HostKeyPolicy::KnownHostsDefault => {
                self.verify_known_hosts(known_hosts::default_path(), &key)
            }
        }
    }
}

impl HostKeyHandler {
    fn verify_known_hosts(
        &self,
        path: Option<PathBuf>,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, TunnelError> {
        let Some(path) = path else {
            return Err(TunnelError::KnownHosts(
                "找不到 $HOME / %USERPROFILE%，无法校验 known_hosts".into(),
            ));
        };
        match known_hosts::check(&path, &self.host, self.port, key)
            .map_err(TunnelError::KnownHosts)?
        {
            HostKeyVerdict::Match => Ok(true),
            HostKeyVerdict::Unknown => Err(TunnelError::HostKeyUnknown {
                layer: self.layer,
                host: self.host.clone(),
                port: self.port,
                fingerprint: known_hosts::fingerprint(key),
                public_key: key.to_openssh().unwrap_or_default(),
            }),
            HostKeyVerdict::Changed { recorded } => Err(TunnelError::HostKeyChanged {
                layer: self.layer,
                host: self.host.clone(),
                port: self.port,
                recorded,
                actual: known_hosts::fingerprint(key),
                public_key: key.to_openssh().unwrap_or_default(),
            }),
        }
    }
}

fn ssh_key_of_certificate(
    certificate: &russh::keys::ssh_key::Certificate,
) -> russh::keys::ssh_key::PublicKey {
    russh::keys::ssh_key::PublicKey::from(certificate.public_key().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hop() -> SshHop {
        SshHop {
            host: "bastion.example.com".into(),
            port: 22,
            user: "ops".into(),
            auth: vec![AuthMethod::None],
            host_keys: HostKeyPolicy::KnownHostsDefault,
        }
    }

    #[tokio::test]
    async fn empty_hops_are_rejected() {
        let config = TunnelConfig::default();
        let error = connect(
            &config,
            &TunnelTarget {
                host: "db".into(),
                port: 5432,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TunnelError::Empty));
    }

    #[tokio::test]
    async fn tcp_failure_points_at_the_layer() {
        let mut config = TunnelConfig {
            connect_timeout: Duration::from_millis(500),
            ..TunnelConfig::default()
        };
        let mut first = hop();
        // 保留一个连不上的端口，错误必须指明是第 1 层。
        first.port = 1;
        config.hops.push(first);
        let error = connect(
            &config,
            &TunnelTarget {
                host: "db".into(),
                port: 5432,
            },
        )
        .await
        .unwrap_err();
        match error {
            TunnelError::Layer { layer, host, .. } => {
                assert_eq!(layer, 1);
                assert_eq!(host, "bastion.example.com");
            }
            other => panic!("应是 Layer 错误，实际 {other:?}"),
        }
    }

    #[test]
    fn forward_bind_ranges_are_explicit() {
        assert_eq!(ForwardBind::Loopback.addr().ip().to_string(), "127.0.0.1");
        assert!(ForwardBind::Lan.addr().ip().is_unspecified());
    }

    #[tokio::test]
    async fn proxy_forward_relays_through_socks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = proxy_listener.accept().await.unwrap();
            let mut greeting = [0u8; 2];
            socket.read_exact(&mut greeting).await.unwrap();
            let mut methods = vec![0u8; greeting[1] as usize];
            socket.read_exact(&mut methods).await.unwrap();
            socket.write_all(&[0x05, 0x00]).await.unwrap();

            let mut head = [0u8; 4];
            socket.read_exact(&mut head).await.unwrap();
            assert_eq!(head[3], 0x03, "测试用例应发域名");
            let mut len = [0u8; 1];
            socket.read_exact(&mut len).await.unwrap();
            let mut name = vec![0u8; len[0] as usize];
            socket.read_exact(&mut name).await.unwrap();
            assert_eq!(name, b"db");
            let mut port = [0u8; 2];
            socket.read_exact(&mut port).await.unwrap();
            assert_eq!(u16::from_be_bytes(port), 5432);
            socket
                .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x1f, 0x90])
                .await
                .unwrap();

            let mut chunk = [0u8; 32];
            let read = socket.read(&mut chunk).await.unwrap();
            socket.write_all(&chunk[..read]).await.unwrap();
        });

        let config = ProxyConfig {
            kind: ProxyKind::Socks5,
            host: proxy_addr.ip().to_string(),
            port: proxy_addr.port(),
            auth: None,
            remote_dns: true,
            order: ProxyOrder::BeforeTunnel,
            connect_timeout: Duration::from_secs(2),
        };
        let forward = start_proxy_forward(
            &config,
            &TunnelTarget {
                host: "db".into(),
                port: 5432,
            },
            ForwardBind::Loopback,
        )
        .await
        .expect("代理转发应启动");
        assert_eq!(forward.host(), "127.0.0.1");
        assert!(forward.lan_addr().is_none());

        let mut client = TcpStream::connect((forward.host().as_str(), forward.port()))
            .await
            .unwrap();
        client.write_all(b"hi").await.unwrap();
        let mut echo = [0u8; 2];
        client.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"hi");
    }

    #[test]
    fn auth_labels_describe_the_chain() {
        assert_eq!(AuthMethod::None.label(), "无认证");
        assert_eq!(AuthMethod::Password("x".into()).label(), "密码");
        assert_eq!(
            AuthMethod::KeyFile {
                path: "k".into(),
                passphrase: Some("p".into())
            }
            .label(),
            "私钥 + 口令"
        );
        assert_eq!(
            AuthMethod::KeyFile {
                path: "k".into(),
                passphrase: None
            }
            .label(),
            "私钥"
        );
    }
}
