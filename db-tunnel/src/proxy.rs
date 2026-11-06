//! SOCKS5 / HTTP CONNECT 代理（F-91a / F-08）。
//!
//! 只负责把「到代理的 TCP 连接」升级成「到目标库的隧道流」，不参与数据库认证。
//! 口令只存在于内存中的 [`ProxyAuth`]，`Debug` 输出会打码。

use std::net::IpAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{AsyncStream, TunnelTarget};

/// 代理类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Socks5,
    HttpConnect,
}

impl ProxyKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Socks5 => "SOCKS5",
            Self::HttpConnect => "HTTP CONNECT",
        }
    }

    pub fn scheme(self) -> &'static str {
        match self {
            Self::Socks5 => "socks5",
            Self::HttpConnect => "http",
        }
    }
}

/// 代理相对 SSH 跳板的位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyOrder {
    BeforeTunnel,
    AfterTunnel,
}

/// 代理认证信息（**只在内存**）。
#[derive(Clone)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyAuth")
            .field("username", &self.username)
            .field("password", &"Secret(***)")
            .finish()
    }
}

/// 已校验的代理建链参数。
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub auth: Option<ProxyAuth>,
    /// SOCKS5 是否把域名交给代理解析；HTTP CONNECT 始终传域名。
    pub remote_dns: bool,
    pub order: ProxyOrder,
    pub connect_timeout: Duration,
}

impl ProxyConfig {
    /// `socks5://host:port` / `http://host:port`（IPv6 自动加方括号）。
    pub fn endpoint(&self) -> String {
        format!(
            "{}://{}:{}",
            self.kind.scheme(),
            bracket_ipv6(self.host.trim()),
            self.port
        )
    }

    /// 建链摘要，例如 `SOCKS5 proxy.internal:1080（先于 SSH）`。
    pub fn label(&self) -> String {
        let stage = match self.order {
            ProxyOrder::BeforeTunnel => "先于 SSH",
            ProxyOrder::AfterTunnel => "SSH 之后",
        };
        format!(
            "{} {}:{}（{stage}）",
            self.kind.label(),
            bracket_ipv6(self.host.trim()),
            self.port
        )
    }

    fn validate(&self) -> Result<(), ProxyError> {
        if self.host.trim().is_empty() {
            return Err(ProxyError::Empty);
        }
        if self.port == 0 {
            return Err(ProxyError::EmptyPort);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("未填写代理主机")]
    Empty,
    #[error("代理端口无效")]
    EmptyPort,
    #[error("连接代理 {host}:{port} 失败：{message}")]
    Tcp {
        host: String,
        port: u16,
        message: String,
    },
    #[error("代理 {host}:{port} 握手超时（{timeout_ms} ms）")]
    Timeout {
        host: String,
        port: u16,
        timeout_ms: u128,
    },
    #[error("代理 {host}:{port} 认证失败：{message}")]
    Auth {
        host: String,
        port: u16,
        message: String,
    },
    #[error("代理 {host}:{port} 拒绝了请求（{code}）{message}")]
    Rejected {
        host: String,
        port: u16,
        code: String,
        message: String,
    },
    #[error("代理 {host}:{port} 握手失败：{message}")]
    Handshake {
        host: String,
        port: u16,
        message: String,
    },
    #[error("代理响应超过 {limit} 字节上限")]
    ResponseTooLarge { limit: usize },
    #[error("代理 IO 错误：{0}")]
    Io(#[from] std::io::Error),
}

/// 代理握手完成后的流；`prefix` 保存握手时多读到的字节（HTTP 代理可能出现）。
pub struct ProxyStream {
    prefix: Vec<u8>,
    offset: usize,
    inner: Box<dyn AsyncStream>,
}

impl ProxyStream {
    fn with_prefix(inner: Box<dyn AsyncStream>, prefix: Vec<u8>) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

impl std::fmt::Debug for ProxyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyStream").finish_non_exhaustive()
    }
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.offset < self.prefix.len() {
            let rest = &self.prefix[self.offset..];
            let take = rest.len().min(buf.remaining());
            buf.put_slice(&rest[..take]);
            self.offset += take;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProxyStream {
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

/// 直接连代理并完成握手。
pub async fn connect(
    config: &ProxyConfig,
    target: &TunnelTarget,
) -> Result<ProxyStream, ProxyError> {
    config.validate()?;
    let host = config.host.trim().to_string();
    let port = config.port;
    let tcp = tokio::time::timeout(
        config.connect_timeout,
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| ProxyError::Timeout {
        host: host.clone(),
        port,
        timeout_ms: config.connect_timeout.as_millis(),
    })?
    .map_err(|error| ProxyError::Tcp {
        host: host.clone(),
        port,
        message: error.to_string(),
    })?;
    let _ = tcp.set_nodelay(true);
    handshake(Box::new(tcp), config, target).await
}

/// 在已有流上完成代理握手（用于「SSH → 代理」或在测试里复用连接）。
pub async fn handshake(
    mut stream: Box<dyn AsyncStream>,
    config: &ProxyConfig,
    target: &TunnelTarget,
) -> Result<ProxyStream, ProxyError> {
    config.validate()?;
    let host = config.host.trim().to_string();
    let port = config.port;
    let result = tokio::time::timeout(config.connect_timeout, async {
        match config.kind {
            ProxyKind::Socks5 => socks5(&mut stream, config, target).await,
            ProxyKind::HttpConnect => http_connect(&mut stream, config, target).await,
        }
    })
    .await
    .map_err(|_| ProxyError::Timeout {
        host: host.clone(),
        port,
        timeout_ms: config.connect_timeout.as_millis(),
    })?;

    let leftover = result.map_err(|error| match error {
        error @ (ProxyError::Auth { .. }
        | ProxyError::Rejected { .. }
        | ProxyError::Handshake { .. }
        | ProxyError::ResponseTooLarge { .. }) => error,
        other => ProxyError::Handshake {
            host,
            port,
            message: other.to_string(),
        },
    })?;
    Ok(ProxyStream::with_prefix(stream, leftover))
}

async fn socks5(
    stream: &mut Box<dyn AsyncStream>,
    config: &ProxyConfig,
    target: &TunnelTarget,
) -> Result<Vec<u8>, ProxyError> {
    let host = config.host.trim().to_string();
    let port = config.port;
    let with_auth = config.auth.is_some();
    let methods: &[u8] = if with_auth { &[0x00, 0x02] } else { &[0x00] };
    let mut greeting = Vec::with_capacity(2 + methods.len());
    greeting.push(0x05);
    greeting.push(methods.len() as u8);
    greeting.extend_from_slice(methods);
    stream.write_all(&greeting).await?;

    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 0x05 {
        return Err(ProxyError::Handshake {
            host,
            port,
            message: format!("SOCKS 版本不符：{}", reply[0]),
        });
    }
    match reply[1] {
        0x00 => {}
        0x02 => {
            let Some(auth) = &config.auth else {
                return Err(ProxyError::Auth {
                    host,
                    port,
                    message: "代理要求用户名 / 口令，但未配置".into(),
                });
            };
            socks_auth(stream, auth, &host, port).await?;
        }
        0xff => {
            return Err(ProxyError::Auth {
                host,
                port,
                message: "代理没有可接受的认证方式".into(),
            });
        }
        other => {
            return Err(ProxyError::Handshake {
                host,
                port,
                message: format!("未知的认证方式：{other:#04x}"),
            });
        }
    }

    let mut request = vec![0x05, 0x01, 0x00];
    match target.host.trim().parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) if config.remote_dns => {
            let name = target.host.trim();
            if name.is_empty() || name.len() > 255 {
                return Err(ProxyError::Handshake {
                    host,
                    port,
                    message: "目标主机名长度无效".into(),
                });
            }
            request.push(0x03);
            request.push(name.len() as u8);
            request.extend_from_slice(name.as_bytes());
        }
        Err(_) => {
            // 本地解析：把域名换成 IP 再交给代理。
            let resolved = tokio::net::lookup_host((target.host.trim(), target.port))
                .await
                .map_err(|error| ProxyError::Handshake {
                    host: host.clone(),
                    port,
                    message: format!("本地解析目标主机失败：{error}"),
                })?
                .next()
                .ok_or_else(|| ProxyError::Handshake {
                    host: host.clone(),
                    port,
                    message: "本地解析目标主机没有结果".into(),
                })?;
            match resolved.ip() {
                IpAddr::V4(ip) => {
                    request.push(0x01);
                    request.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    request.push(0x04);
                    request.extend_from_slice(&ip.octets());
                }
            }
        }
    }
    request.extend_from_slice(&target.port.to_be_bytes());
    stream.write_all(&request).await?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(ProxyError::Handshake {
            host,
            port,
            message: format!("SOCKS 应答版本不符：{}", head[0]),
        });
    }
    if head[1] != 0x00 {
        return Err(ProxyError::Rejected {
            host,
            port,
            code: socks_code(head[1]).to_string(),
            message: String::new(),
        });
    }
    // 吃掉绑定地址。
    match head[3] {
        0x01 => {
            let mut skip = [0u8; 6];
            stream.read_exact(&mut skip).await?;
        }
        0x04 => {
            let mut skip = [0u8; 18];
            stream.read_exact(&mut skip).await?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut skip = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut skip).await?;
        }
        other => {
            return Err(ProxyError::Handshake {
                host,
                port,
                message: format!("未知的绑定地址类型：{other:#04x}"),
            });
        }
    }
    Ok(Vec::new())
}

async fn socks_auth(
    stream: &mut Box<dyn AsyncStream>,
    auth: &ProxyAuth,
    host: &str,
    port: u16,
) -> Result<(), ProxyError> {
    let user = auth.username.as_bytes();
    let pass = auth.password.as_bytes();
    if user.len() > 255 || pass.len() > 255 {
        return Err(ProxyError::Auth {
            host: host.to_string(),
            port,
            message: "用户名或口令超过 255 字节".into(),
        });
    }
    let mut request = Vec::with_capacity(3 + user.len() + pass.len());
    request.push(0x01);
    request.push(user.len() as u8);
    request.extend_from_slice(user);
    request.push(pass.len() as u8);
    request.extend_from_slice(pass);
    stream.write_all(&request).await?;
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await?;
    if reply[1] != 0x00 {
        return Err(ProxyError::Auth {
            host: host.to_string(),
            port,
            message: format!("用户名或口令被拒绝（{:#04x}）", reply[1]),
        });
    }
    Ok(())
}

async fn http_connect(
    stream: &mut Box<dyn AsyncStream>,
    config: &ProxyConfig,
    target: &TunnelTarget,
) -> Result<Vec<u8>, ProxyError> {
    let host = config.host.trim().to_string();
    let port = config.port;
    let authority = format!("{}:{}", bracket_ipv6(target.host.trim()), target.port);
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = &config.auth {
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", auth.username, auth.password));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    const LIMIT: usize = 16 * 1024;
    let mut buffer = Vec::with_capacity(512);
    let mut chunk = [0u8; 512];
    let end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(ProxyError::Handshake {
                host,
                port,
                message: "代理在返回响应头前关闭了连接".into(),
            });
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if buffer.len() > LIMIT {
            return Err(ProxyError::ResponseTooLarge { limit: LIMIT });
        }
    };

    let head = String::from_utf8_lossy(&buffer[..end]).to_string();
    let status_line = head.lines().next().unwrap_or_default().trim().to_string();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    if !code.starts_with('2') {
        return Err(ProxyError::Rejected {
            host,
            port,
            code,
            message: status_line,
        });
    }
    Ok(buffer[end..].to_vec())
}

fn socks_code(code: u8) -> &'static str {
    match code {
        0x01 => "一般故障",
        0x02 => "连接不被允许",
        0x03 => "网络不可达",
        0x04 => "主机不可达",
        0x05 => "连接被拒绝",
        0x06 => "TTL 过期",
        0x07 => "不支持的命令",
        0x08 => "不支持的地址类型",
        _ => "未知错误",
    }
}

fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn config(kind: ProxyKind, addr: std::net::SocketAddr, auth: Option<ProxyAuth>) -> ProxyConfig {
        ProxyConfig {
            kind,
            host: addr.ip().to_string(),
            port: addr.port(),
            auth,
            remote_dns: true,
            order: ProxyOrder::BeforeTunnel,
            connect_timeout: Duration::from_secs(2),
        }
    }

    fn target() -> TunnelTarget {
        TunnelTarget {
            host: "db.internal".into(),
            port: 5432,
        }
    }

    /// 极简 SOCKS5 服务端：校验 greeting / 可选认证 / CONNECT，然后回显。
    async fn socks_server(
        listener: TcpListener,
        expect_auth: Option<(&'static str, &'static str)>,
    ) {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = [0u8; 2];
        socket.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x05);
        let mut methods = vec![0u8; head[1] as usize];
        socket.read_exact(&mut methods).await.unwrap();
        if expect_auth.is_some() {
            assert!(methods.contains(&0x02));
            socket.write_all(&[0x05, 0x02]).await.unwrap();
            let mut auth_head = [0u8; 2];
            socket.read_exact(&mut auth_head).await.unwrap();
            assert_eq!(auth_head[0], 0x01);
            let mut user = vec![0u8; auth_head[1] as usize];
            socket.read_exact(&mut user).await.unwrap();
            let mut plen = [0u8; 1];
            socket.read_exact(&mut plen).await.unwrap();
            let mut pass = vec![0u8; plen[0] as usize];
            socket.read_exact(&mut pass).await.unwrap();
            let (eu, ep) = expect_auth.unwrap();
            let ok = user == eu.as_bytes() && pass == ep.as_bytes();
            socket
                .write_all(&[0x01, if ok { 0x00 } else { 0x01 }])
                .await
                .unwrap();
            if !ok {
                return;
            }
        } else {
            socket.write_all(&[0x05, 0x00]).await.unwrap();
        }

        let mut request = [0u8; 4];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(request[0], 0x05);
        assert_eq!(request[1], 0x01);
        assert_eq!(request[3], 0x03, "测试用例应发域名");
        let mut len = [0u8; 1];
        socket.read_exact(&mut len).await.unwrap();
        let mut name = vec![0u8; len[0] as usize];
        socket.read_exact(&mut name).await.unwrap();
        assert_eq!(name, b"db.internal");
        let mut port = [0u8; 2];
        socket.read_exact(&mut port).await.unwrap();
        assert_eq!(u16::from_be_bytes(port), 5432);
        socket
            .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x1f, 0x90])
            .await
            .unwrap();

        let mut chunk = [0u8; 64];
        let read = socket.read(&mut chunk).await.unwrap();
        socket.write_all(&chunk[..read]).await.unwrap();
    }

    #[tokio::test]
    async fn socks5_round_trip_and_echo() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(socks_server(listener, None));
        let mut stream = connect(&config(ProxyKind::Socks5, addr, None), &target())
            .await
            .expect("SOCKS5 握手应成功");
        stream.write_all(b"ping").await.unwrap();
        let mut echo = [0u8; 4];
        stream.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"ping");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_username_password_auth() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(socks_server(listener, Some(("ops", "s3cret"))));
        let auth = ProxyAuth {
            username: "ops".into(),
            password: "s3cret".into(),
        };
        let mut stream = connect(&config(ProxyKind::Socks5, addr, Some(auth)), &target())
            .await
            .expect("带认证的 SOCKS5 应成功");
        stream.write_all(b"x").await.unwrap();
        let mut echo = [0u8; 1];
        stream.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"x");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_sends_basic_auth_and_keeps_leftover() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 256];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).to_string();
            assert!(
                text.starts_with("CONNECT db.internal:5432 HTTP/1.1"),
                "{text}"
            );
            assert!(text.contains("Proxy-Authorization: Basic "), "{text}");
            // 响应头 + 紧跟一个字节的隧道数据，验证前缀不会被吞掉。
            socket
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nZ")
                .await
                .unwrap();
            let mut chunk = [0u8; 16];
            let read = socket.read(&mut chunk).await.unwrap();
            socket.write_all(&chunk[..read]).await.unwrap();
        });
        let auth = ProxyAuth {
            username: "ops".into(),
            password: "s3cret".into(),
        };
        let mut stream = connect(&config(ProxyKind::HttpConnect, addr, Some(auth)), &target())
            .await
            .expect("HTTP CONNECT 应成功");
        let mut leftover = [0u8; 1];
        stream.read_exact(&mut leftover).await.unwrap();
        assert_eq!(&leftover, b"Z");
        stream.write_all(b"hey").await.unwrap();
        let mut echo = [0u8; 3];
        stream.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"hey");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_rejection_reports_status() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut chunk = [0u8; 256];
            let _ = socket.read(&mut chunk).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let error = connect(&config(ProxyKind::HttpConnect, addr, None), &target())
            .await
            .unwrap_err();
        match error {
            ProxyError::Rejected { code, .. } => assert_eq!(code, "407"),
            other => panic!("应是拒绝错误，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_host_is_rejected_before_io() {
        let mut config = config(ProxyKind::Socks5, "127.0.0.1:1".parse().unwrap(), None);
        config.host = "  ".into();
        let error = connect(&config, &target()).await.unwrap_err();
        assert!(matches!(error, ProxyError::Empty));
    }
}
