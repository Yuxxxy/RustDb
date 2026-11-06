//! 核心编排层：驱动注册表、连接、会话 Actor、事件总线、结果存储。
//!
//! 不依赖任何具体驱动（只依赖 `db-api`），也不依赖 GPUI。

use crate::events::{
    AppEvent, ConnEvent, DataCompareEvent, DumpEvent, EventBus, EventStream, MetaEvent, QueryEvent,
    SystemEvent, TransferEvent,
};
use crate::profile::ConnectionProfile;
use crate::results::{ResultSetId, ResultStore, PAGE_ROWS};
use crate::session_actor::{self, SessionCmd, SessionHandle, SessionLiveState};
use futures_util::StreamExt as _;
use parking_lot::RwLock;
use rustdb_db_api::{
    DbKind, DriverError, DriverRegistry, ExecuteRequest, FetchOptions, ForeignKeyInfo, Namespace,
    ObjectKind, ObjectKindMask, ObjectSummary, PageOrder, PageRequest, PageResult, QueryId, Row,
    ServerInfo, Session, SessionId, Value,
};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

/// 服务端分页的驱动兜底（从本文件拆出）。
mod page;

pub const RESULT_BUDGET_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_ROWS_PER_QUERY: u64 = 10_000;

pub struct Core {
    drivers: DriverRegistry,
    profiles: RwLock<Vec<ConnectionProfile>>,
    /// 连接配置落盘仓库（F-02）；`in_memory` 时不写文件。
    profile_store: crate::profiles::ProfileStore,
    sessions: RwLock<HashMap<SessionId, SessionHandle>>,
    /// F-91：会话对应的本地端口转发（隧道）；断开时随会话一起释放。
    tunnels: RwLock<HashMap<SessionId, Arc<rustdb_db_tunnel::LocalForward>>>,
    /// F-91：等待用户确认的主机密钥（profile_id → 待确认信息）。
    pending_host_keys: RwLock<HashMap<String, PendingHostKey>>,
    results: Arc<ResultStore>,
    /// 查询历史（按数据源分桶、可固定）。
    history: Arc<crate::history::HistoryStore>,
    /// 运行记录（F-65）：每次执行一条，固定项不参与淘汰。
    runs: Arc<crate::runs::RunLog>,
    events: EventBus,
    /// 按会话 + 命名空间缓存对象列表：`(session, display_label)` → 对象。
    ///
    /// 不能只用 `display_label`：两个 SQLite 文件的默认库都叫 `main`，
    /// 两个 Oracle 连接也可能有同名 schema，否则会互相串对象。
    catalog: RwLock<HashMap<(SessionId, String), Vec<ObjectSummary>>>,
    /// 正在加载的会话 + 命名空间键（避免重复请求）。
    loading: RwLock<HashSet<(SessionId, String)>>,
    namespaces: RwLock<Vec<Namespace>>,
    current_ns: RwLock<Option<Namespace>>,
    /// F-89：生产连接的临时写解锁（profile_id → 截止时间），不落盘。
    production_unlocks: RwLock<HashMap<String, Instant>>,
    runtime: Runtime,
    queries: AtomicU64,
}

impl Core {
    pub fn new(drivers: DriverRegistry) -> Arc<Self> {
        Self::with_history(
            drivers,
            crate::history::HistoryStore::new(crate::history::DEFAULT_HISTORY_LIMIT),
        )
    }

    /// 以指定历史存储构建（F-68：磁盘持久化由组合根注入）。
    pub fn with_history(
        drivers: DriverRegistry,
        history: crate::history::HistoryStore,
    ) -> Arc<Self> {
        Self::with_config(drivers, history, crate::profiles::ProfileStore::in_memory())
    }

    /// 以指定历史与连接配置仓库构建（F-02：连接配置落盘）。
    ///
    /// 仓库里没有配置时使用内置示例（本地 SQLite + 可选的开发库）。
    pub fn with_config(
        drivers: DriverRegistry,
        history: crate::history::HistoryStore,
        profile_store: crate::profiles::ProfileStore,
    ) -> Arc<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("rustdb-db")
            .enable_all()
            .build()
            .expect("创建 tokio 运行时失败");

        let events = EventBus::new(4096);
        for kind in drivers.kinds() {
            events.publish(
                AppEvent::System(SystemEvent::DriverRegistered { driver: kind }),
                None,
            );
        }

        let stored = profile_store.load();
        let profiles = if stored.is_empty() {
            default_profiles()
        } else {
            stored
        };

        Arc::new(Self {
            drivers,
            profiles: RwLock::new(profiles),
            profile_store,
            sessions: RwLock::new(HashMap::new()),
            tunnels: RwLock::new(HashMap::new()),
            pending_host_keys: RwLock::new(HashMap::new()),
            results: Arc::new(ResultStore::new(RESULT_BUDGET_BYTES)),
            history: Arc::new(history),
            runs: Arc::new(crate::runs::RunLog::new(crate::runs::RunLog::DEFAULT_LIMIT)),
            events,
            catalog: RwLock::new(HashMap::new()),
            loading: RwLock::new(HashSet::new()),
            namespaces: RwLock::new(Vec::new()),
            current_ns: RwLock::new(None),
            production_unlocks: RwLock::new(HashMap::new()),
            runtime,
            queries: AtomicU64::new(1),
        })
    }

    pub fn subscribe(&self) -> EventStream {
        self.events.subscribe()
    }

    pub fn results(&self) -> Arc<ResultStore> {
        self.results.clone()
    }

    /// 查询历史仓库。
    pub fn history(&self) -> Arc<crate::history::HistoryStore> {
        self.history.clone()
    }

    /// 运行记录仓库（F-65）。
    pub fn runs(&self) -> Arc<crate::runs::RunLog> {
        self.runs.clone()
    }

    pub fn profiles(&self) -> Vec<ConnectionProfile> {
        self.profiles.read().clone()
    }

    /// 新增连接配置（F-02）；id 重复时报错。
    pub fn add_profile(&self, profile: ConnectionProfile) -> Result<(), String> {
        if profile.id.trim().is_empty() {
            return Err("连接 id 不能为空".into());
        }
        {
            let mut profiles = self.profiles.write();
            if profiles.iter().any(|item| item.id == profile.id) {
                return Err(format!("连接 {} 已存在", profile.id));
            }
            profiles.push(profile);
        }
        self.persist_profiles()
    }

    /// 修改连接配置（F-02）：按 id 覆盖。
    pub fn update_profile(&self, profile: ConnectionProfile) -> Result<(), String> {
        {
            let mut profiles = self.profiles.write();
            let slot = profiles
                .iter_mut()
                .find(|item| item.id == profile.id)
                .ok_or_else(|| format!("找不到连接 {}", profile.id))?;
            *slot = profile;
        }
        self.persist_profiles()
    }

    /// 临时注入口令（只写内存，**不落盘**）。
    ///
    /// `dev_secret` 是 `#[serde(skip)]` 字段，落盘时会被丢弃；这里连文件都不写，
    /// 用于 CLI / keyring 落地前的过渡：口令只在本次进程内存里存在。
    pub fn set_profile_secret(
        &self,
        profile_id: &str,
        secret: Option<String>,
    ) -> Result<(), String> {
        let mut profiles = self.profiles.write();
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| format!("找不到连接 {profile_id}"))?;
        profile.dev_secret = secret;
        Ok(())
    }

    /// 删除连接配置（F-02）。已建立的会话不受影响（仍可断开）。
    pub fn remove_profile(&self, profile_id: &str) -> Result<(), String> {
        {
            let mut profiles = self.profiles.write();
            let before = profiles.len();
            profiles.retain(|item| item.id != profile_id);
            if profiles.len() == before {
                return Err(format!("找不到连接 {profile_id}"));
            }
        }
        self.production_unlocks.write().remove(profile_id);
        self.pending_host_keys.write().remove(profile_id);
        self.persist_profiles()
    }

    /// 连接配置落盘；失败只记日志（内存里的配置仍然有效）。
    fn persist_profiles(&self) -> Result<(), String> {
        let profiles = self.profiles.read().clone();
        match self.profile_store.save(&profiles) {
            Ok(()) => Ok(()),
            Err(error) => {
                log::warn!("保存连接配置失败：{error}");
                Err(format!("保存连接配置失败：{error}"))
            }
        }
    }

    /// F-89：标记 / 取消生产连接（随连接配置一起落盘）。
    pub fn set_profile_production(&self, profile_id: &str, production: bool) -> Result<(), String> {
        {
            let mut profiles = self.profiles.write();
            let profile = profiles
                .iter_mut()
                .find(|profile| profile.id == profile_id)
                .ok_or_else(|| format!("找不到连接 {profile_id}"))?;
            profile.production = production;
        }
        // 标记变化时清掉旧解锁，避免「关掉再打开」继承剩余窗口。
        self.production_unlocks.write().remove(profile_id);
        self.persist_profiles()
    }

    /// F-89：临时解锁生产连接的写操作（桌面端 1 / 5 分钟）。
    pub fn unlock_writes(&self, profile_id: &str, window: Duration) -> Result<(), String> {
        let profile = self
            .profiles
            .read()
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| format!("找不到连接 {profile_id}"))?;
        if profile.read_only {
            return Err("只读连接不可解锁写操作".into());
        }
        if !profile.production {
            return Err("该连接未标记为生产，无需解锁".into());
        }
        if window.is_zero() {
            return Err("解锁时长必须大于 0".into());
        }
        self.production_unlocks
            .write()
            .insert(profile_id.to_string(), Instant::now() + window);
        Ok(())
    }

    /// F-89：立即回锁。
    pub fn lock_writes(&self, profile_id: &str) {
        self.production_unlocks.write().remove(profile_id);
    }

    /// F-89：剩余解锁时间；已过期自动清理。
    pub fn write_unlock_remaining(&self, profile_id: &str) -> Option<Duration> {
        let until = *self.production_unlocks.read().get(profile_id)?;
        let now = Instant::now();
        if until <= now {
            self.production_unlocks.write().remove(profile_id);
            return None;
        }
        Some(until - now)
    }

    /// 某个会话的命名空间已缓存对象；未加载时返回空。
    pub fn objects(&self, session_id: &SessionId, ns: &Namespace) -> Vec<ObjectSummary> {
        self.catalog
            .read()
            .get(&(session_id.clone(), ns.display_label()))
            .cloned()
            .unwrap_or_default()
    }

    /// 该会话是否已加载过该命名空间（区分「空库」与「未加载」）。
    pub fn has_objects(&self, session_id: &SessionId, ns: &Namespace) -> bool {
        self.catalog
            .read()
            .contains_key(&(session_id.clone(), ns.display_label()))
    }

    pub fn is_loading_objects(&self, session_id: &SessionId, ns: &Namespace) -> bool {
        self.loading
            .read()
            .contains(&(session_id.clone(), ns.display_label()))
    }

    pub fn loaded_namespace_count(&self, session_id: &SessionId) -> usize {
        self.catalog
            .read()
            .keys()
            .filter(|(sid, _)| sid == session_id)
            .count()
    }

    pub fn namespaces(&self) -> Vec<Namespace> {
        self.namespaces.read().clone()
    }

    pub fn current_namespace(&self) -> Option<Namespace> {
        self.current_ns.read().clone()
    }

    pub fn has_session(&self, id: &SessionId) -> bool {
        self.sessions.read().contains_key(id)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.read().len()
    }

    pub fn drivers(&self) -> Vec<DbKind> {
        self.drivers.kinds().collect()
    }

    /// 发起连接：立即返回，结果通过 `ConnEvent::Ready / Failed` 事件回来。
    pub fn connect(self: &Arc<Self>, profile_id: &str) {
        let Some(profile) = self
            .profiles
            .read()
            .iter()
            .find(|p| p.id == profile_id)
            .cloned()
        else {
            self.events.publish(
                AppEvent::Conn(ConnEvent::Failed {
                    profile_id: profile_id.to_string(),
                    error_kind: rustdb_db_api::DriverErrorKind::NotFound,
                    message: format!("找不到连接配置 {profile_id}"),
                }),
                None,
            );
            return;
        };

        self.events.publish(
            AppEvent::Conn(ConnEvent::Connecting {
                profile_id: profile.id.clone(),
                attempt: 1,
            }),
            None,
        );

        let this = self.clone();
        self.runtime.spawn(async move {
            this.connect_inner(profile).await;
        });
    }

    async fn connect_inner(self: Arc<Self>, profile: ConnectionProfile) {
        let Some(factory) = self.drivers.get(profile.driver) else {
            self.events.publish(
                AppEvent::Conn(ConnEvent::Failed {
                    profile_id: profile.id.clone(),
                    error_kind: rustdb_db_api::DriverErrorKind::Unsupported,
                    message: format!("驱动未注册：{}", profile.driver.display_name()),
                }),
                None,
            );
            return;
        };

        let mut opts = match profile.to_connect_options() {
            Ok(opts) => opts,
            Err(message) => {
                self.events.publish(
                    AppEvent::Conn(ConnEvent::Failed {
                        profile_id: profile.id.clone(),
                        error_kind: rustdb_db_api::DriverErrorKind::Connection,
                        message,
                    }),
                    None,
                );
                return;
            }
        };
        // F-91 / F-91a：先建隧道 / 代理，成功后把驱动指向本地转发端口（驱动本身不感知）。
        let mut forward: Option<Arc<rustdb_db_tunnel::LocalForward>> = None;
        let has_tunnel = profile.tunnel.as_ref().is_some_and(|spec| !spec.is_empty());
        if has_tunnel || profile.proxy.is_some() {
            let bind = if profile.forward_lan {
                rustdb_db_tunnel::ForwardBind::Lan
            } else {
                rustdb_db_tunnel::ForwardBind::Loopback
            };
            let started = if has_tunnel {
                let spec = profile.tunnel.clone().unwrap_or_default();
                self.start_tunnel(&profile, &spec, bind).await
            } else {
                self.start_proxy(&profile, bind).await
            };
            match started {
                Ok(local) => {
                    opts.host = Some(local.host());
                    opts.port = Some(local.port());
                    forward = Some(Arc::new(local));
                }
                Err(TunnelStart::HostKey) => return,
                Err(TunnelStart::Failed(message)) => {
                    self.events.publish(
                        AppEvent::Conn(ConnEvent::Failed {
                            profile_id: profile.id.clone(),
                            error_kind: rustdb_db_api::DriverErrorKind::Connection,
                            message,
                        }),
                        None,
                    );
                    return;
                }
            }
        }

        let session: Arc<dyn rustdb_db_api::Session> = match factory.connect(&opts).await {
            Ok(s) => Arc::from(s),
            Err(e) => {
                // 建链失败时隧道也要收掉，不留后台监听。
                drop(forward);
                self.events.publish(
                    AppEvent::Conn(ConnEvent::Failed {
                        profile_id: profile.id.clone(),
                        error_kind: e.kind,
                        message: e.user_message(),
                    }),
                    None,
                );
                return;
            }
        };

        let id = session.id();
        let server = session.server().clone();
        let capabilities = session.capabilities().clone();

        let handle = session_actor::spawn(
            session.clone(),
            profile.id.clone(),
            self.events.clone(),
            self.results.clone(),
            self.history.clone(),
            self.runs.clone(),
            self.runtime.handle(),
        );
        self.sessions.write().insert(id.clone(), handle);
        // 隧道随会话存活：断开连接时一并释放。
        if let Some(forward) = forward {
            self.tunnels.write().insert(id.clone(), forward);
        }

        self.events.publish(
            AppEvent::Conn(ConnEvent::Ready {
                session_id: id.clone(),
                server: server.clone(),
                capabilities,
            }),
            Some(id.clone()),
        );

        // 命名空间列表：即使失败也不影响连接可用。
        let namespaces = session.metadata().namespaces().await.unwrap_or_default();
        let current = pick_current(&namespaces, &server);
        *self.namespaces.write() = namespaces.clone();
        *self.current_ns.write() = Some(current.clone());
        self.events.publish(
            AppEvent::Meta(MetaEvent::NamespacesLoaded {
                namespaces,
                current: current.clone(),
            }),
            Some(id.clone()),
        );

        self.load_objects(&id, current).await;
    }

    /// F-91：建立 SSH 隧道并启动本地端口转发。
    ///
    /// 主机密钥未知 / 变更时**先中止**（尚未发送任何凭据），把待确认信息
    /// 交给用户；用户确认后由 [`Core::confirm_tunnel_host_key`] 写回 known_hosts。
    async fn start_tunnel(
        &self,
        profile: &ConnectionProfile,
        spec: &rustdb_db_api::TunnelSpec,
        bind: rustdb_db_tunnel::ForwardBind,
    ) -> Result<rustdb_db_tunnel::LocalForward, TunnelStart> {
        let host = profile
            .host
            .clone()
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| TunnelStart::Failed("缺少目标库地址，无法建立隧道".into()))?;
        let port = profile
            .port
            .or_else(|| profile.driver.default_port())
            .ok_or_else(|| TunnelStart::Failed("缺少目标库端口，无法建立隧道".into()))?;

        let mut config = crate::tunnel::build_config(spec).map_err(TunnelStart::Failed)?;
        if let Some(proxy) = &profile.proxy {
            let mut proxy =
                crate::tunnel::build_proxy_config(proxy).map_err(TunnelStart::Failed)?;
            proxy.connect_timeout = Duration::from_millis(profile.connect_timeout_ms.max(1000));
            config.proxy = Some(proxy);
        }
        let target = rustdb_db_tunnel::TunnelTarget {
            host: host.clone(),
            port,
        };

        match rustdb_db_tunnel::start_forward_on(&config, &target, bind).await {
            Ok(local) => {
                self.events.publish(
                    AppEvent::Conn(ConnEvent::TunnelReady {
                        profile_id: profile.id.clone(),
                        chain: route_summary(
                            spec,
                            profile.proxy.as_ref(),
                            &format!("{host}:{port}"),
                        ),
                    }),
                    None,
                );
                Ok(local)
            }
            Err(rustdb_db_tunnel::TunnelError::HostKeyUnknown {
                layer,
                host,
                port,
                fingerprint,
                public_key,
            }) => {
                let (hop_host, hop_port) = hop_endpoint(profile, layer);
                self.pending_host_keys.write().insert(
                    profile.id.clone(),
                    PendingHostKey {
                        layer,
                        host,
                        port,
                        fingerprint: fingerprint.clone(),
                        public_key: public_key.clone(),
                        changed: false,
                        recorded: None,
                        known_hosts: self.known_hosts_path(profile, layer),
                    },
                );
                self.events.publish(
                    AppEvent::Conn(ConnEvent::TunnelHostKey {
                        profile_id: profile.id.clone(),
                        layer,
                        host: hop_host,
                        port: hop_port,
                        fingerprint,
                        public_key,
                        changed: false,
                        recorded: None,
                    }),
                    None,
                );
                Err(TunnelStart::HostKey)
            }
            Err(rustdb_db_tunnel::TunnelError::HostKeyChanged {
                layer,
                host,
                port,
                recorded,
                actual,
                public_key,
            }) => {
                let (hop_host, hop_port) = hop_endpoint(profile, layer);
                self.pending_host_keys.write().insert(
                    profile.id.clone(),
                    PendingHostKey {
                        layer,
                        host,
                        port,
                        fingerprint: actual.clone(),
                        public_key: public_key.clone(),
                        changed: true,
                        recorded: Some(recorded.clone()),
                        known_hosts: self.known_hosts_path(profile, layer),
                    },
                );
                self.events.publish(
                    AppEvent::Conn(ConnEvent::TunnelHostKey {
                        profile_id: profile.id.clone(),
                        layer,
                        host: hop_host,
                        port: hop_port,
                        fingerprint: actual,
                        public_key,
                        changed: true,
                        recorded: Some(recorded),
                    }),
                    None,
                );
                Err(TunnelStart::HostKey)
            }
            Err(error) => Err(TunnelStart::Failed(error.to_string())),
        }
    }

    /// F-91a：仅经代理建立本地转发（没有 SSH 跳板）。
    async fn start_proxy(
        &self,
        profile: &ConnectionProfile,
        bind: rustdb_db_tunnel::ForwardBind,
    ) -> Result<rustdb_db_tunnel::LocalForward, TunnelStart> {
        let spec = profile
            .proxy
            .as_ref()
            .ok_or_else(|| TunnelStart::Failed("未配置代理".into()))?;
        let host = profile
            .host
            .clone()
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| TunnelStart::Failed("缺少目标库地址，无法通过代理连接".into()))?;
        let port = profile
            .port
            .or_else(|| profile.driver.default_port())
            .ok_or_else(|| TunnelStart::Failed("缺少目标库端口，无法通过代理连接".into()))?;

        let mut config = crate::tunnel::build_proxy_config(spec).map_err(TunnelStart::Failed)?;
        config.connect_timeout = Duration::from_millis(profile.connect_timeout_ms.max(1000));
        let target = rustdb_db_tunnel::TunnelTarget {
            host: host.clone(),
            port,
        };
        match rustdb_db_tunnel::start_proxy_forward(&config, &target, bind).await {
            Ok(local) => {
                self.events.publish(
                    AppEvent::Conn(ConnEvent::TunnelReady {
                        profile_id: profile.id.clone(),
                        chain: format!("代理 {} → 目标 {host}:{port}", spec.summary()),
                    }),
                    None,
                );
                Ok(local)
            }
            Err(error) => Err(TunnelStart::Failed(error.to_string())),
        }
    }

    /// 该层跳板使用的 known_hosts 路径。
    fn known_hosts_path(
        &self,
        profile: &ConnectionProfile,
        layer: usize,
    ) -> Option<std::path::PathBuf> {
        let hop = profile
            .tunnel
            .as_ref()
            .and_then(|spec| spec.hops.get(layer.saturating_sub(1)))?;
        crate::tunnel::known_hosts_path(&hop.host_keys)
    }

    /// F-91：用户确认主机密钥。
    ///
    /// * `accept = true`：写回 known_hosts（首次连接追加、密钥变更替换旧记录）；
    /// * `accept = false`：放弃本次连接。
    ///
    /// 返回一句给用户看的结果；调用方随后可重新发起连接。
    pub fn confirm_tunnel_host_key(
        &self,
        profile_id: &str,
        accept: bool,
    ) -> Result<String, String> {
        let pending = self.pending_host_keys.write().remove(profile_id);
        if !accept {
            // 拒绝永远安全：没有待确认记录也算拒绝成功。
            return Ok("已放弃本次连接".into());
        }
        let pending = pending.ok_or_else(|| "没有待确认的主机密钥".to_string())?;
        let path = pending
            .known_hosts
            .ok_or_else(|| "找不到 known_hosts 文件路径".to_string())?;
        let result = if pending.changed {
            rustdb_db_tunnel::known_hosts::replace_host(
                &path,
                &pending.host,
                pending.port,
                &pending.public_key,
            )
        } else {
            rustdb_db_tunnel::known_hosts::record_openssh(
                &path,
                &pending.host,
                pending.port,
                &pending.public_key,
            )
        };
        result.map_err(|error| format!("写入 known_hosts 失败：{error}"))?;
        Ok(if pending.changed {
            format!("已更新 {} 的主机密钥记录", pending.host)
        } else {
            format!("已信任 {} 的主机密钥", pending.host)
        })
    }

    /// F-91：等待确认的主机密钥（供 UI 展示）。
    pub fn pending_host_key(&self, profile_id: &str) -> Option<PendingHostKey> {
        self.pending_host_keys.read().get(profile_id).cloned()
    }

    /// 懒加载某个命名空间的对象列表（表 / 视图 / 存储过程 …）。
    ///
    /// 已缓存或正在加载时直接返回；结果通过 `MetaEvent::Loaded` 广播。
    /// 树节点展开、SQL 标签页切库都走这里。
    pub async fn load_objects(self: &Arc<Self>, session_id: &SessionId, ns: Namespace) {
        let label = ns.display_label();
        let key = (session_id.clone(), label.clone());
        if self.catalog.read().contains_key(&key) {
            return;
        }
        if !self.loading.write().insert(key.clone()) {
            return;
        }
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.loading.write().remove(&key);
            return;
        };
        self.events.publish(
            AppEvent::Meta(MetaEvent::LoadStarted {
                namespace: label.clone(),
            }),
            Some(session_id.clone()),
        );
        let result = handle
            .session
            .metadata()
            .objects(&ns, ObjectKindMask::ALL)
            .await;
        self.loading.write().remove(&key);
        // 加载期间连接可能已断开；不要再把已关闭会话的对象写回缓存。
        if !self.sessions.read().contains_key(session_id) {
            return;
        }
        match result {
            Ok(objects) => {
                self.catalog.write().insert(key, objects.clone());
                self.events.publish(
                    AppEvent::Meta(MetaEvent::Loaded {
                        namespace: label,
                        objects,
                    }),
                    Some(session_id.clone()),
                );
            }
            Err(e) => {
                self.events.publish(
                    AppEvent::Meta(MetaEvent::LoadFailed {
                        namespace: label,
                        message: e.user_message(),
                    }),
                    Some(session_id.clone()),
                );
            }
        }
    }

    /// 取对象的 DDL（右键「查看 DDL」）。异步：结果通过 `MetaEvent::DdlLoaded` 回来。
    pub fn request_ddl(self: &Arc<Self>, session_id: &SessionId, path: rustdb_db_api::ObjectPath) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::DdlFailed {
                    path: path.display_label(),
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let label = path.display_label();
            match handle.session.metadata().object_ddl(&path).await {
                Ok(Some(ddl)) => this.events.publish(
                    AppEvent::Meta(MetaEvent::DdlLoaded { path: label, ddl }),
                    Some(sid),
                ),
                Ok(None) => this.events.publish(
                    AppEvent::Meta(MetaEvent::DdlFailed {
                        path: label,
                        message: "该对象不支持查看 DDL".into(),
                    }),
                    Some(sid),
                ),
                Err(e) => this.events.publish(
                    AppEvent::Meta(MetaEvent::DdlFailed {
                        path: label,
                        message: e.user_message(),
                    }),
                    Some(sid),
                ),
            }
        });
    }

    /// 取例程源码（F-82：视图 / 存储过程 / 函数 / 触发器 / 程序包）。
    ///
    /// 异步：结果通过 `MetaEvent::RoutineSourceLoaded` / `RoutineSourceFailed` 回来。
    pub fn request_routine_source(
        self: &Arc<Self>,
        session_id: &SessionId,
        path: rustdb_db_api::ObjectPath,
    ) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::RoutineSourceFailed {
                    path: path.display_label(),
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let label = path.display_label();
            match handle.session.metadata().object_ddl(&path).await {
                Ok(Some(ddl)) => this.events.publish(
                    AppEvent::Meta(MetaEvent::RoutineSourceLoaded {
                        path: label,
                        text: ddl.text,
                        source: ddl.source,
                    }),
                    Some(sid),
                ),
                Ok(None) => this.events.publish(
                    AppEvent::Meta(MetaEvent::RoutineSourceFailed {
                        path: label,
                        message: "该驱动不支持导出例程源码（可能是权限不足）".into(),
                    }),
                    Some(sid),
                ),
                Err(e) => this.events.publish(
                    AppEvent::Meta(MetaEvent::RoutineSourceFailed {
                        path: label,
                        message: e.user_message(),
                    }),
                    Some(sid),
                ),
            }
        });
    }

    /// 取对象依赖（F-83：例程详情里的「依赖 / 被依赖」，只直接依赖）。
    ///
    /// 异步：结果通过 `MetaEvent::DependenciesLoaded` / `DependenciesFailed` 回来。
    pub fn request_dependencies(
        self: &Arc<Self>,
        session_id: &SessionId,
        path: rustdb_db_api::ObjectPath,
    ) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::DependenciesFailed {
                    path: path.display_label(),
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let label = path.display_label();
            match handle.session.metadata().dependencies(&path).await {
                Ok(Some(dependencies)) => this.events.publish(
                    AppEvent::Meta(MetaEvent::DependenciesLoaded {
                        path: label,
                        dependencies,
                    }),
                    Some(sid),
                ),
                Ok(None) => this.events.publish(
                    AppEvent::Meta(MetaEvent::DependenciesFailed {
                        path: label,
                        message: "该驱动不支持查看对象依赖".into(),
                    }),
                    Some(sid),
                ),
                Err(e) => this.events.publish(
                    AppEvent::Meta(MetaEvent::DependenciesFailed {
                        path: label,
                        message: e.user_message(),
                    }),
                    Some(sid),
                ),
            }
        });
    }

    /// 取分区列表（F-85）。异步：结果通过 `MetaEvent::PartitionsLoaded` / `PartitionsFailed` 回来。
    pub fn request_partitions(
        self: &Arc<Self>,
        session_id: &SessionId,
        path: rustdb_db_api::ObjectPath,
    ) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::PartitionsFailed {
                    path: path.display_label(),
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let label = path.display_label();
            match handle.session.metadata().partitions(&path).await {
                Ok(Some(partitions)) => this.events.publish(
                    AppEvent::Meta(MetaEvent::PartitionsLoaded {
                        path: label,
                        partitions,
                    }),
                    Some(sid),
                ),
                Ok(None) => this.events.publish(
                    AppEvent::Meta(MetaEvent::PartitionsFailed {
                        path: label,
                        message: "该驱动不支持分区管理".into(),
                    }),
                    Some(sid),
                ),
                Err(e) => this.events.publish(
                    AppEvent::Meta(MetaEvent::PartitionsFailed {
                        path: label,
                        message: e.user_message(),
                    }),
                    Some(sid),
                ),
            }
        });
    }

    /// 取表详情（右键「设计表」）。异步：结果通过 `MetaEvent::TableDetailLoaded` 回来。
    pub fn request_table_detail(
        self: &Arc<Self>,
        session_id: &SessionId,
        path: rustdb_db_api::ObjectPath,
    ) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::TableDetailFailed {
                    path: path.display_label(),
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let label = path.display_label();
            match handle.session.metadata().table(&path).await {
                Ok(detail) => this.events.publish(
                    AppEvent::Meta(MetaEvent::TableDetailLoaded {
                        path: label,
                        detail,
                    }),
                    Some(sid),
                ),
                Err(e) => this.events.publish(
                    AppEvent::Meta(MetaEvent::TableDetailFailed {
                        path: label,
                        message: e.user_message(),
                    }),
                    Some(sid),
                ),
            }
        });
    }

    /// 汇总某个命名空间的表结构（F-87 数据库文档）。只读：只调元数据接口。
    ///
    /// 结果通过 `MetaEvent::DocumentLoaded` 回来；超过 [`crate::docs::MAX_DOC_TABLES`]
    /// 的表只计数，不静默隐藏。
    pub fn request_document(self: &Arc<Self>, session_id: &SessionId, ns: Namespace) {
        let label = ns.display_label();
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::DocumentFailed {
                    namespace: label,
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let metadata = handle.session.metadata();
            let objects = match metadata
                .objects(&ns, ObjectKindMask::only(rustdb_db_api::ObjectKind::Table))
                .await
            {
                Ok(objects) => objects,
                Err(error) => {
                    this.events.publish(
                        AppEvent::Meta(MetaEvent::DocumentFailed {
                            namespace: ns.display_label(),
                            message: error.user_message(),
                        }),
                        Some(sid),
                    );
                    return;
                }
            };
            let mut summaries: Vec<ObjectSummary> = objects
                .into_iter()
                .filter(|object| object.path.kind == rustdb_db_api::ObjectKind::Table)
                .collect();
            summaries.sort_by(|a, b| a.path.name.to_lowercase().cmp(&b.path.name.to_lowercase()));
            let skipped = summaries.len().saturating_sub(crate::docs::MAX_DOC_TABLES);
            summaries.truncate(crate::docs::MAX_DOC_TABLES);
            let mut input = crate::docs::DocInput {
                database: ns.display_label(),
                tables: Vec::new(),
                skipped,
            };
            for summary in summaries {
                match metadata.table(&summary.path).await {
                    Ok(detail) => input.tables.push(crate::docs::DocTable {
                        summary: detail.summary.clone(),
                        detail: Some(detail),
                        error: None,
                    }),
                    Err(error) => input.tables.push(crate::docs::DocTable {
                        summary,
                        detail: None,
                        error: Some(error.user_message()),
                    }),
                }
            }
            this.events.publish(
                AppEvent::Meta(MetaEvent::DocumentLoaded {
                    namespace: ns.display_label(),
                    input,
                }),
                Some(sid),
            );
        });
    }

    /// F-86：导出某个命名空间的结构快照。只读；结果通过
    /// `MetaEvent::SchemaSnapshotLoaded` 回来，`slot` 原样带回。
    ///
    /// 超过 [`crate::compare::MAX_COMPARE_TABLES`] 的表只计数；单表失败记录在
    /// `snapshot.failed`，不静默丢。
    pub fn request_schema_snapshot(
        self: &Arc<Self>,
        session_id: &SessionId,
        ns: Namespace,
        slot: String,
    ) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::SchemaSnapshotFailed {
                    slot,
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let Some(driver) = self.session_kind(session_id) else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::SchemaSnapshotFailed {
                    slot,
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let metadata = handle.session.metadata();
            let fail = |this: &Arc<Self>, sid: &SessionId, message: String| {
                this.events.publish(
                    AppEvent::Meta(MetaEvent::SchemaSnapshotFailed {
                        slot: slot.clone(),
                        message,
                    }),
                    Some(sid.clone()),
                );
            };
            let objects = match metadata
                .objects(&ns, ObjectKindMask::only(rustdb_db_api::ObjectKind::Table))
                .await
            {
                Ok(objects) => objects,
                Err(error) => {
                    fail(&this, &sid, error.user_message());
                    return;
                }
            };
            let mut summaries: Vec<ObjectSummary> = objects
                .into_iter()
                .filter(|object| object.path.kind == rustdb_db_api::ObjectKind::Table)
                .collect();
            summaries.sort_by(|a, b| a.path.name.to_lowercase().cmp(&b.path.name.to_lowercase()));
            let skipped = summaries
                .len()
                .saturating_sub(crate::compare::MAX_COMPARE_TABLES);
            summaries.truncate(crate::compare::MAX_COMPARE_TABLES);
            let mut snapshot = crate::compare::SchemaSnapshot {
                driver,
                namespace: ns.clone(),
                tables: Vec::with_capacity(summaries.len()),
                skipped,
                failed: Vec::new(),
            };
            for summary in summaries {
                let name = summary.path.name.to_string();
                match metadata.table(&summary.path).await {
                    Ok(detail) => snapshot.tables.push(crate::compare::TableSnapshot {
                        name,
                        columns: detail.columns,
                        indexes: detail.indexes,
                        foreign_keys: detail.foreign_keys,
                    }),
                    Err(error) => snapshot
                        .failed
                        .push(format!("{name}：{}", error.user_message())),
                }
            }
            this.events.publish(
                AppEvent::Meta(MetaEvent::SchemaSnapshotLoaded { slot, snapshot }),
                Some(sid),
            );
        });
    }

    /// F-76：整库逻辑导出。流式分页读取，写文件（可选 gzip），进度只发计数事件。
    ///
    /// `tables` 为空表示导出当前命名空间下全部表；`dest` 由调用方选择。
    /// 导出不是一致性快照；失败按 `options.error_mode` 写注释或终止。
    pub fn request_dump(
        self: &Arc<Self>,
        session_id: &SessionId,
        ns: Namespace,
        tables: Vec<String>,
        options: crate::dump::DumpOptions,
        dest: std::path::PathBuf,
    ) {
        let fail = |this: &Arc<Self>, message: String| {
            this.events.publish(
                AppEvent::Dump(DumpEvent::Failed { message }),
                Some(session_id.clone()),
            );
        };
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            fail(self, "连接尚未建立".into());
            return;
        };
        let Some(driver) = self.session_kind(session_id) else {
            fail(self, "连接尚未建立".into());
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            if let Err(message) = run_dump(
                &this,
                &handle.session,
                &sid,
                driver,
                ns,
                tables,
                options,
                dest,
            )
            .await
            {
                this.events.publish(
                    AppEvent::Dump(DumpEvent::Failed { message }),
                    Some(sid.clone()),
                );
            }
        });
    }

    /// F-76：多库 / 多 Schema 分文件导出：每个命名空间顺序写一个文件。
    ///
    /// 文件名为 `<命名空间>.sql[.gz]`（非法字符替换为 `_`）；任一文件失败即停止后续。
    pub fn request_dump_namespaces(
        self: &Arc<Self>,
        session_id: &SessionId,
        namespaces: Vec<Namespace>,
        options: crate::dump::DumpOptions,
        dir: std::path::PathBuf,
    ) {
        let fail = |this: &Arc<Self>, message: String| {
            this.events.publish(
                AppEvent::Dump(DumpEvent::Failed { message }),
                Some(session_id.clone()),
            );
        };
        if namespaces.is_empty() {
            fail(self, "没有可导出的库".into());
            return;
        }
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            fail(self, "连接尚未建立".into());
            return;
        };
        let Some(driver) = self.session_kind(session_id) else {
            fail(self, "连接尚未建立".into());
            return;
        };
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let extension = if options.gzip { "sql.gz" } else { "sql" };
            for ns in namespaces {
                let dest = dir.join(format!("{}.{extension}", dump_file_stem(&ns)));
                if let Err(message) = run_dump(
                    &this,
                    &handle.session,
                    &sid,
                    driver,
                    ns,
                    Vec::new(),
                    options.clone(),
                    dest,
                )
                .await
                {
                    this.events.publish(
                        AppEvent::Dump(DumpEvent::Failed { message }),
                        Some(sid.clone()),
                    );
                    return;
                }
            }
        });
    }

    /// F-75：跨库数据传输。源用当前会话，目标按配置**临时建链**，结束后释放。
    ///
    /// 目标连接的只读 / 生产写门槛在这里先判定；`重建` 会在 DDL 前自动导出备份，
    /// 备份失败即中止（不删除目标表）。行数据分页读取、按批写入，不经过事件总线。
    pub fn request_transfer(
        self: &Arc<Self>,
        request: crate::transfer::TransferRequest,
    ) -> Result<(), String> {
        request.validate()?;
        let source = self
            .sessions
            .read()
            .get(&request.source_session)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let source_kind = self
            .session_kind(&request.source_session)
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let target_profile = self.checked_transfer_target(&request.target_profile)?;

        let this = self.clone();
        let sid = request.source_session.clone();
        self.runtime.spawn(async move {
            if let Err(message) = this
                .run_transfer(&source, source_kind, &target_profile, &request)
                .await
            {
                this.events.publish(
                    AppEvent::Transfer(TransferEvent::Failed { message }),
                    Some(sid),
                );
            }
        });
        Ok(())
    }

    /// F-75：执行前预览。临时连目标读结构后生成脚本，不写任何数据。
    pub fn request_transfer_preview(self: &Arc<Self>, request: crate::transfer::TransferRequest) {
        let sid = request.source_session.clone();
        let prepared = (|| {
            request.validate()?;
            let source = self
                .sessions
                .read()
                .get(&request.source_session)
                .cloned()
                .ok_or_else(|| "连接尚未建立".to_string())?;
            let source_kind = self
                .session_kind(&request.source_session)
                .ok_or_else(|| "连接尚未建立".to_string())?;
            let target_profile = self.checked_transfer_target(&request.target_profile)?;
            Ok::<_, String>((source, source_kind, target_profile))
        })();
        let (source, source_kind, target_profile) = match prepared {
            Ok(prepared) => prepared,
            Err(message) => {
                self.events.publish(
                    AppEvent::Transfer(TransferEvent::Failed { message }),
                    Some(sid),
                );
                return;
            }
        };
        let this = self.clone();
        self.runtime.spawn(async move {
            if let Err(message) = this
                .preview_transfer(&source, source_kind, &target_profile, &request)
                .await
            {
                this.events.publish(
                    AppEvent::Transfer(TransferEvent::Failed { message }),
                    Some(sid),
                );
            }
        });
    }

    /// 目标连接门槛：存在、非只读、生产已解锁、非隧道 / 代理（临时直连）。
    fn checked_transfer_target(&self, target_profile: &str) -> Result<ConnectionProfile, String> {
        self.checked_write_target(target_profile, "跨库传输")
    }

    fn checked_write_target(
        &self,
        target_profile: &str,
        feature: &str,
    ) -> Result<ConnectionProfile, String> {
        let target_profile = self.checked_direct_target(target_profile, feature)?;
        if target_profile.read_only {
            return Err("目标连接为只读模式，写操作已被拦截".into());
        }
        if target_profile.production && self.write_unlock_remaining(&target_profile.id).is_none() {
            return Err("目标生产连接的写操作未解锁；请先临时解锁（1 / 5 分钟）".into());
        }
        Ok(target_profile)
    }

    /// 只读目标门槛：存在且非隧道 / 代理（预览只读，不要求写权限）。
    fn checked_compare_target(&self, target_profile: &str) -> Result<ConnectionProfile, String> {
        self.checked_direct_target(target_profile, "数据对比")
    }

    fn checked_direct_target(
        &self,
        target_profile: &str,
        feature: &str,
    ) -> Result<ConnectionProfile, String> {
        let target_profile = self
            .profiles
            .read()
            .iter()
            .find(|profile| profile.id == target_profile)
            .cloned()
            .ok_or_else(|| format!("找不到目标连接 {target_profile}"))?;
        if target_profile
            .tunnel
            .as_ref()
            .is_some_and(|spec| !spec.is_empty())
            || target_profile.proxy.is_some()
        {
            return Err(format!(
                "目标连接配置了 SSH 隧道 / 代理，{feature}暂不支持；请改用直连连接"
            ));
        }
        Ok(target_profile)
    }

    async fn preview_transfer(
        self: &Arc<Self>,
        source: &SessionHandle,
        source_kind: DbKind,
        target_profile: &ConnectionProfile,
        request: &crate::transfer::TransferRequest,
    ) -> Result<(), String> {
        let sid = request.source_session.clone();
        let factory = self
            .drivers
            .get(target_profile.driver)
            .ok_or_else(|| format!("驱动未注册：{}", target_profile.driver.display_name()))?;
        let target: Arc<dyn Session> = factory
            .connect(&target_profile.to_connect_options()?)
            .await
            .map(Arc::from)
            .map_err(|error| format!("目标连接失败：{}", error.user_message()))?;
        let plans = build_transfer_plans(
            source,
            source_kind,
            &target,
            target_profile.driver,
            &request.source_ns,
            &request.target_ns,
            &request.tables,
            &request.options,
        )
        .await?;
        let (mut lines, mut warnings) = crate::transfer::preview_lines(&plans);
        if target_profile.driver == DbKind::Sqlite
            && target_profile.database.as_deref() == Some(":memory:")
        {
            warnings.push("目标是内存数据库（:memory:），临时连接释放后数据会丢失".into());
        }
        let batch = request.options.effective_batch(target_profile.driver);
        lines.insert(
            0,
            format!(
                "目标：{} · {} · 每批最多 {batch} 行",
                target_profile.name,
                request.target_ns.display_label()
            ),
        );
        lines.insert(
            1,
            format!(
                "模式：{} · 大小写：{}",
                request.options.mode.label(),
                request.options.case.label()
            ),
        );
        if request.options.mode == crate::transfer::TransferMode::Recreate && request.options.backup
        {
            lines.insert(2, "重建前将自动备份目标表（备份失败即中止）".to_string());
        }
        self.events.publish(
            AppEvent::Transfer(TransferEvent::Preview { lines, warnings }),
            Some(sid),
        );
        Ok(())
    }

    async fn run_transfer(
        self: &Arc<Self>,
        source: &SessionHandle,
        source_kind: DbKind,
        target_profile: &ConnectionProfile,
        request: &crate::transfer::TransferRequest,
    ) -> Result<(), String> {
        let started = Instant::now();
        let sid = request.source_session.clone();
        let options = &request.options;
        let backup = options.mode == crate::transfer::TransferMode::Recreate && options.backup;
        self.events.publish(
            AppEvent::Transfer(TransferEvent::Started {
                source: source.profile_id.clone(),
                target: target_profile.name.clone(),
                tables: request.tables.len(),
                mode: options.mode.label().to_string(),
                backup,
            }),
            Some(sid.clone()),
        );

        let factory = self
            .drivers
            .get(target_profile.driver)
            .ok_or_else(|| format!("驱动未注册：{}", target_profile.driver.display_name()))?;
        let target_session: Arc<dyn Session> = factory
            .connect(&target_profile.to_connect_options()?)
            .await
            .map(Arc::from)
            .map_err(|error| format!("目标连接失败：{}", error.user_message()))?;
        let target_kind = target_profile.driver;

        let plans = build_transfer_plans(
            source,
            source_kind,
            &target_session,
            target_kind,
            &request.source_ns,
            &request.target_ns,
            &request.tables,
            options,
        )
        .await?;

        if backup {
            let existing: Vec<String> = plans
                .iter()
                .filter(|plan| plan.target_exists && plan.error.is_none())
                .map(|plan| plan.target.name.to_string())
                .collect();
            if !existing.is_empty() {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                let path = request
                    .backup_dir
                    .join(format!("rustdb-transfer-backup-{secs}.sql"));
                let dump_options = crate::dump::DumpOptions {
                    include_structure: true,
                    include_data: true,
                    include_objects: false,
                    include_drops: true,
                    gzip: false,
                    ..crate::dump::DumpOptions::default()
                };
                let backup_sid = SessionId(format!("transfer-backup-{secs}"));
                run_dump(
                    self,
                    &target_session,
                    &backup_sid,
                    target_kind,
                    request.target_ns.clone(),
                    existing,
                    dump_options,
                    path.clone(),
                )
                .await
                .map_err(|error| format!("重建前备份失败，已中止：{error}"))?;
                let bytes = std::fs::metadata(&path)
                    .map(|meta| meta.len())
                    .unwrap_or_default();
                self.events.publish(
                    AppEvent::Transfer(TransferEvent::BackupDone {
                        path: path.display().to_string(),
                        bytes,
                    }),
                    Some(sid.clone()),
                );
            }
        }

        let mut total_rows = 0u64;
        let mut failed = 0usize;
        for (index, plan) in plans.iter().enumerate() {
            let table = plan.source.name.to_string();
            self.events.publish(
                AppEvent::Transfer(TransferEvent::TableStarted {
                    table: table.clone(),
                    index: index + 1,
                }),
                Some(sid.clone()),
            );
            if let Some(error) = plan.error.clone() {
                failed += 1;
                self.events.publish(
                    AppEvent::Transfer(TransferEvent::TableDone {
                        table,
                        rows: 0,
                        error: Some(error),
                    }),
                    Some(sid.clone()),
                );
                continue;
            }
            match transfer_one_table(
                &source.session,
                &target_session,
                target_kind,
                plan,
                options,
                &sid,
                &self.events,
            )
            .await
            {
                Ok(rows) => {
                    total_rows += rows;
                    self.events.publish(
                        AppEvent::Transfer(TransferEvent::TableDone {
                            table,
                            rows,
                            error: None,
                        }),
                        Some(sid.clone()),
                    );
                }
                Err(error) => {
                    failed += 1;
                    self.events.publish(
                        AppEvent::Transfer(TransferEvent::TableDone {
                            table,
                            rows: 0,
                            error: Some(error),
                        }),
                        Some(sid.clone()),
                    );
                }
            }
        }
        self.events.publish(
            AppEvent::Transfer(TransferEvent::Finished {
                tables: plans.len(),
                rows: total_rows,
                failed,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
            Some(sid),
        );
        Ok(())
    }

    /// F-78：数据对比（只读，不写目标）。源用当前会话，目标按配置临时建链。
    pub fn request_data_compare(
        self: &Arc<Self>,
        request: crate::data_compare::DataCompareRequest,
    ) {
        self.request_data_compare_run(request, false);
    }

    /// F-78：对比后把差异同步到目标（INSERT / UPDATE / DELETE）。
    pub fn request_data_compare_sync(
        self: &Arc<Self>,
        request: crate::data_compare::DataCompareRequest,
    ) {
        self.request_data_compare_run(request, true);
    }

    fn request_data_compare_run(
        self: &Arc<Self>,
        request: crate::data_compare::DataCompareRequest,
        apply: bool,
    ) {
        let sid = request.source_session.clone();
        let prepared = (|| {
            request.validate()?;
            let source = self
                .sessions
                .read()
                .get(&request.source_session)
                .cloned()
                .ok_or_else(|| "连接尚未建立".to_string())?;
            let target_profile = if apply {
                self.checked_write_target(&request.target_profile, "数据对比同步")?
            } else {
                self.checked_compare_target(&request.target_profile)?
            };
            Ok::<_, String>((source, target_profile))
        })();
        let (source, target_profile) = match prepared {
            Ok(prepared) => prepared,
            Err(message) => {
                self.events.publish(
                    AppEvent::DataCompare(DataCompareEvent::Failed { message }),
                    Some(sid),
                );
                return;
            }
        };
        let this = self.clone();
        self.runtime.spawn(async move {
            if let Err(message) = this
                .run_data_compare(&source, &target_profile, &request, apply)
                .await
            {
                this.events.publish(
                    AppEvent::DataCompare(DataCompareEvent::Failed { message }),
                    Some(sid),
                );
            }
        });
    }

    async fn run_data_compare(
        self: &Arc<Self>,
        source: &SessionHandle,
        target_profile: &ConnectionProfile,
        request: &crate::data_compare::DataCompareRequest,
        apply: bool,
    ) -> Result<(), String> {
        let started = Instant::now();
        let sid = request.source_session.clone();
        self.events.publish(
            AppEvent::DataCompare(DataCompareEvent::Started {
                source: source.profile_id.clone(),
                target: target_profile.name.clone(),
                tables: request.tables.len(),
                apply,
            }),
            Some(sid.clone()),
        );

        let factory = self
            .drivers
            .get(target_profile.driver)
            .ok_or_else(|| format!("驱动未注册：{}", target_profile.driver.display_name()))?;
        let target: Arc<dyn Session> = factory
            .connect(&target_profile.to_connect_options()?)
            .await
            .map(Arc::from)
            .map_err(|error| format!("目标连接失败：{}", error.user_message()))?;
        let target_kind = target_profile.driver;
        let target_objects = target
            .metadata()
            .objects(&request.target_ns, ObjectKindMask::only(ObjectKind::Table))
            .await
            .map_err(|error| format!("读取目标表失败：{}", error.user_message()))?;
        let target_names: Vec<String> = target_objects
            .iter()
            .map(|object| object.path.name.to_string())
            .collect();

        let mut outputs = Vec::with_capacity(request.tables.len());
        let mut results = Vec::with_capacity(request.tables.len());
        for (index, config) in request.tables.iter().enumerate() {
            let table = config.source.clone();
            self.events.publish(
                AppEvent::DataCompare(DataCompareEvent::TableStarted {
                    table: table.clone(),
                    index: index + 1,
                }),
                Some(sid.clone()),
            );
            match compare_one_table(
                &source.session,
                &request.source_ns,
                &target,
                target_kind,
                &request.target_ns,
                &target_names,
                config,
                &request.options,
            )
            .await
            {
                Ok((plan, diff)) => {
                    let mut warnings = plan.warnings.clone();
                    warnings.extend(diff.warnings.clone());
                    let (insert, update, delete, same) = diff.counts();
                    let result = crate::data_compare::TableCompareResult {
                        table: table.clone(),
                        insert,
                        update,
                        delete,
                        same,
                        warnings,
                        error: None,
                    };
                    self.events.publish(
                        AppEvent::DataCompare(DataCompareEvent::TableDone {
                            table,
                            insert,
                            update,
                            delete,
                            same,
                            warnings: result.warnings.clone(),
                            error: None,
                        }),
                        Some(sid.clone()),
                    );
                    results.push(result);
                    outputs.push(Some((plan, diff)));
                }
                Err(message) => {
                    self.events.publish(
                        AppEvent::DataCompare(DataCompareEvent::TableDone {
                            table: table.clone(),
                            insert: 0,
                            update: 0,
                            delete: 0,
                            same: 0,
                            warnings: Vec::new(),
                            error: Some(message.clone()),
                        }),
                        Some(sid.clone()),
                    );
                    results.push(crate::data_compare::TableCompareResult {
                        table,
                        error: Some(message),
                        ..crate::data_compare::TableCompareResult::default()
                    });
                    outputs.push(None);
                }
            }
        }

        let mut scripts: Vec<Vec<String>> = Vec::with_capacity(outputs.len());
        for (index, output) in outputs.iter().enumerate() {
            let Some((plan, diff)) = output else {
                scripts.push(Vec::new());
                continue;
            };
            match crate::data_compare::sync_statements(
                target_kind,
                &plan.target,
                &plan.target_columns(),
                &plan.key_indexes,
                diff,
                &request.options,
            ) {
                Ok(statements) => scripts.push(statements),
                Err(message) => {
                    results[index].error = Some(message);
                    scripts.push(Vec::new());
                }
            }
        }

        let insert_total: u64 = results.iter().map(|result| result.insert).sum();
        let update_total: u64 = results.iter().map(|result| result.update).sum();
        let delete_total: u64 = results.iter().map(|result| result.delete).sum();
        let mut warnings = Vec::new();
        for result in &results {
            for warning in &result.warnings {
                warnings.push(format!("{}：{warning}", result.table));
            }
        }
        let mut actions = Vec::new();
        if request.options.insert {
            actions.push("INSERT");
        }
        if request.options.update {
            actions.push("UPDATE");
        }
        if request.options.delete {
            actions.push("DELETE");
        }
        let mut lines = vec![
            format!(
                "目标：{} · {}",
                target_profile.name,
                request.target_ns.display_label()
            ),
            format!("动作：{} · 共 {} 张表", actions.join(" / "), results.len()),
            format!("合计：新增 {insert_total} · 更新 {update_total} · 删除 {delete_total}"),
        ];
        lines.extend(crate::data_compare::preview_lines(
            &results,
            &scripts,
            &warnings,
            crate::data_compare::MAX_PREVIEW_STATEMENTS,
        ));
        self.events.publish(
            AppEvent::DataCompare(DataCompareEvent::Preview {
                lines,
                warnings,
                tables: results.len(),
                insert: insert_total,
                update: update_total,
                delete: delete_total,
            }),
            Some(sid.clone()),
        );

        let mut failed = results
            .iter()
            .filter(|result| result.error.is_some())
            .count();
        if apply {
            let total_statements: u64 = scripts.iter().map(|items| items.len() as u64).sum();
            self.events.publish(
                AppEvent::DataCompare(DataCompareEvent::SyncStarted {
                    statements: total_statements,
                }),
                Some(sid.clone()),
            );
            let timeout = request
                .options
                .timeout_ms
                .filter(|value| *value > 0)
                .map(Duration::from_millis);
            let mut applied = 0u64;
            for (index, statements) in scripts.iter().enumerate() {
                if statements.is_empty() {
                    continue;
                }
                let table = results[index].table.clone();
                let total = statements.len() as u64;
                let mut table_failed = false;
                for statement in statements {
                    match run_transfer_sql(&target, statement, timeout).await {
                        Ok(()) => {
                            applied += 1;
                            if applied % 25 == 0 {
                                self.events.publish(
                                    AppEvent::DataCompare(DataCompareEvent::SyncProgress {
                                        table: table.clone(),
                                        applied,
                                        statements: total,
                                    }),
                                    Some(sid.clone()),
                                );
                            }
                        }
                        Err(message) => {
                            failed += 1;
                            table_failed = true;
                            self.events.publish(
                                AppEvent::DataCompare(DataCompareEvent::SyncTableFailed {
                                    table: table.clone(),
                                    message,
                                }),
                                Some(sid.clone()),
                            );
                            break;
                        }
                    }
                }
                if !table_failed {
                    self.events.publish(
                        AppEvent::DataCompare(DataCompareEvent::SyncProgress {
                            table,
                            applied,
                            statements: total,
                        }),
                        Some(sid.clone()),
                    );
                }
            }
            self.events.publish(
                AppEvent::DataCompare(DataCompareEvent::SyncDone { applied, failed }),
                Some(sid.clone()),
            );
        }

        self.events.publish(
            AppEvent::DataCompare(DataCompareEvent::Finished {
                tables: results.len(),
                insert: insert_total,
                update: update_total,
                delete: delete_total,
                elapsed_ms: started.elapsed().as_millis() as u64,
                failed,
            }),
            Some(sid),
        );
        Ok(())
    }

    /// F-88：汇总当前库的字段血缘输入（表列 / 外键 / 视图定义 / 历史 SQL）。只读。
    pub fn request_lineage(
        self: &Arc<Self>,
        session_id: &SessionId,
        ns: Namespace,
        profile_id: Option<String>,
    ) {
        let label = ns.display_label();
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Meta(MetaEvent::LineageFailed {
                    namespace: label,
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let history = self.history.clone();
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            let metadata = handle.session.metadata();
            let objects = match metadata.objects(&ns, ObjectKindMask::ALL).await {
                Ok(objects) => objects,
                Err(error) => {
                    this.events.publish(
                        AppEvent::Meta(MetaEvent::LineageFailed {
                            namespace: ns.display_label(),
                            message: error.user_message(),
                        }),
                        Some(sid),
                    );
                    return;
                }
            };
            let mut tables: Vec<rustdb_db_api::ObjectSummary> = objects
                .iter()
                .filter(|object| object.path.kind == rustdb_db_api::ObjectKind::Table)
                .cloned()
                .collect();
            let mut views: Vec<rustdb_db_api::ObjectSummary> = objects
                .iter()
                .filter(|object| object.path.kind == rustdb_db_api::ObjectKind::View)
                .cloned()
                .collect();
            tables.sort_by(|a, b| a.path.name.to_lowercase().cmp(&b.path.name.to_lowercase()));
            views.sort_by(|a, b| a.path.name.to_lowercase().cmp(&b.path.name.to_lowercase()));
            let mut input = crate::lineage::LineageInput {
                database: ns.display_label(),
                skipped_tables: tables
                    .len()
                    .saturating_sub(crate::lineage::MAX_LINEAGE_TABLES),
                skipped_views: views
                    .len()
                    .saturating_sub(crate::lineage::MAX_LINEAGE_VIEWS),
                ..crate::lineage::LineageInput::default()
            };
            tables.truncate(crate::lineage::MAX_LINEAGE_TABLES);
            views.truncate(crate::lineage::MAX_LINEAGE_VIEWS);

            for summary in tables {
                match metadata.table(&summary.path).await {
                    Ok(detail) => input.tables.push(crate::lineage::LineageTable {
                        path: detail.summary.path.clone(),
                        columns: detail
                            .columns
                            .iter()
                            .map(|column| column.meta.name.to_string())
                            .collect(),
                        foreign_keys: detail.foreign_keys,
                    }),
                    Err(_) => input.failed_tables += 1,
                }
            }
            for summary in views {
                match metadata.object_ddl(&summary.path).await {
                    Ok(Some(ddl)) => input.views.push(crate::lineage::LineageView {
                        path: summary.path,
                        definition: ddl.text,
                    }),
                    Ok(None) => input.failed_views += 1,
                    Err(_) => input.failed_views += 1,
                }
            }
            if let Some(profile_id) = profile_id {
                let entries = history.list(Some(&profile_id), None);
                input.skipped_queries = entries
                    .len()
                    .saturating_sub(crate::lineage::MAX_LINEAGE_QUERIES);
                input.queries = entries
                    .into_iter()
                    .filter(|entry| entry.ok != Some(false))
                    .take(crate::lineage::MAX_LINEAGE_QUERIES)
                    .map(|entry| crate::lineage::LineageQuery {
                        sql: entry.sql,
                        label: format!("#{}", entry.id),
                    })
                    .collect();
            }
            this.events.publish(
                AppEvent::Meta(MetaEvent::LineageLoaded {
                    namespace: ns.display_label(),
                    input,
                }),
                Some(sid),
            );
        });
    }

    /// 表维护语句（Navicat「维护表」）：各方言的轻量维护命令。
    pub fn maintenance_sql(kind: DbKind, path: &rustdb_db_api::ObjectPath) -> Option<String> {
        let from = object_from_clause(kind, path);
        match kind {
            DbKind::Mysql | DbKind::Mariadb => Some(format!("OPTIMIZE TABLE {from};")),
            // 注释必须写在语句前：分号后的注释会被多语句切分器当成下一条语句。
            DbKind::Sqlite => Some(format!("-- 维护 {from}\nVACUUM;")),
            DbKind::Postgres => Some(format!("VACUUM ANALYZE {from};")),
            _ => None,
        }
    }

    /// 「转储 SQL」的骨架：DDL 用「查看 DDL」，数据用查询导出（写进编辑器，不落盘）。
    pub fn dump_sql(
        &self,
        session_id: &SessionId,
        path: &rustdb_db_api::ObjectPath,
    ) -> Option<String> {
        let kind = self.session_kind(session_id)?;
        let from = object_from_clause(kind, path);
        Some(format!(
            "-- {label}\n-- 转储骨架：DDL 用「查看 DDL」取得，数据查询可导出为 CSV\nSELECT * FROM {from};",
            label = path.display_label()
        ))
    }

    /// 「新建表 / 视图 / 函数」的 SQL 模板（Navicat 风格：直接给出可改的骨架）。
    pub fn new_object_template(kind: DbKind, object: rustdb_db_api::ObjectKind) -> String {
        let quote = |ident: &str| match kind {
            DbKind::Mysql | DbKind::Mariadb => rustdb_db_api::dialect::quote_backtick(ident),
            DbKind::Mssql => rustdb_db_api::dialect::quote_bracket(ident),
            _ => rustdb_db_api::dialect::quote_double(ident),
        };
        match object {
            rustdb_db_api::ObjectKind::View | rustdb_db_api::ObjectKind::MaterializedView => {
                format!("CREATE VIEW {} AS\nSELECT 1;\n", quote("new_view"))
            }
            rustdb_db_api::ObjectKind::Function => format!(
                "CREATE FUNCTION {}(x INTEGER)\nRETURNS INTEGER\nBEGIN\n    RETURN x;\nEND;\n",
                quote("new_function")
            ),
            rustdb_db_api::ObjectKind::Procedure => format!(
                "CREATE PROCEDURE {}()\nBEGIN\n    -- TODO\nEND;\n",
                quote("new_procedure")
            ),
            _ => format!(
                "CREATE TABLE {} (\n    {} INTEGER PRIMARY KEY,\n    {} TEXT\n);\n",
                quote("new_table"),
                quote("id"),
                quote("name")
            ),
        }
    }

    /// 重命名对象的 SQL（表 / 视图）。
    pub fn rename_sql(
        kind: DbKind,
        path: &rustdb_db_api::ObjectPath,
        new_name: &str,
    ) -> Option<String> {
        let from = object_from_clause(kind, path);
        let ident = match kind {
            DbKind::Mysql | DbKind::Mariadb => rustdb_db_api::dialect::quote_backtick(new_name),
            DbKind::Mssql => rustdb_db_api::dialect::quote_bracket(new_name),
            _ => rustdb_db_api::dialect::quote_double(new_name),
        };
        let is_view = matches!(
            path.kind,
            rustdb_db_api::ObjectKind::View | rustdb_db_api::ObjectKind::MaterializedView
        );
        match (kind, is_view) {
            // MySQL 的 RENAME TABLE 同时适用于表与视图。
            (DbKind::Mysql | DbKind::Mariadb, _) => {
                Some(format!("RENAME TABLE {from} TO {ident};"))
            }
            (DbKind::Postgres, true) => Some(format!("ALTER VIEW {from} RENAME TO {ident};")),
            (DbKind::Postgres, false) => Some(format!("ALTER TABLE {from} RENAME TO {ident};")),
            // SQLite 不支持重命名视图。
            (DbKind::Sqlite, true) => None,
            (DbKind::Sqlite, false) => Some(format!("ALTER TABLE {from} RENAME TO {ident};")),
            _ => None,
        }
    }

    /// 供 UI 调用的非阻塞版本（树节点展开 / 切库）。
    pub fn request_objects(self: &Arc<Self>, session_id: &SessionId, ns: Namespace) {
        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            this.load_objects(&sid, ns).await;
        });
    }

    /// 切换当前 catalog / schema（查询框的「切库」入口）。
    ///
    /// 命令走会话 Actor（保证与该连接上的语句串行），成功后重新加载对象列表。
    pub fn switch_namespace(self: &Arc<Self>, session_id: &SessionId, ns: Namespace) {
        let Some(handle) = self.sessions.read().get(session_id).cloned() else {
            self.events.publish(
                AppEvent::Conn(ConnEvent::NamespaceSwitchFailed {
                    session_id: session_id.clone(),
                    message: "连接尚未建立".into(),
                }),
                Some(session_id.clone()),
            );
            return;
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if !handle.send(SessionCmd::UseNamespace {
            ns: ns.clone(),
            reply: reply_tx,
        }) {
            return;
        }

        let this = self.clone();
        let sid = session_id.clone();
        self.runtime.spawn(async move {
            match reply_rx.await {
                Ok(Ok(())) => {
                    *this.current_ns.write() = Some(ns.clone());
                    this.events.publish(
                        AppEvent::Conn(ConnEvent::NamespaceChanged {
                            session_id: sid.clone(),
                            namespace: ns.clone(),
                        }),
                        Some(sid.clone()),
                    );
                    this.load_objects(&sid, ns).await;
                }
                Ok(Err(e)) => {
                    this.events.publish(
                        AppEvent::Conn(ConnEvent::NamespaceSwitchFailed {
                            session_id: sid.clone(),
                            message: e.user_message(),
                        }),
                        Some(sid),
                    );
                }
                Err(_) => {
                    this.events.publish(
                        AppEvent::Conn(ConnEvent::NamespaceSwitchFailed {
                            session_id: sid.clone(),
                            message: "会话已关闭".into(),
                        }),
                        Some(sid),
                    );
                }
            }
        });
    }

    pub fn disconnect(&self, session_id: &SessionId) {
        if let Some(handle) = self.sessions.write().remove(session_id) {
            self.production_unlocks.write().remove(&handle.profile_id);
            // 隧道随会话一起收掉，不留本地监听。
            self.tunnels.write().remove(session_id);
            // 对象缓存也按会话释放，避免会话反复建立后内存只增不减。
            self.catalog.write().retain(|(sid, _), _| sid != session_id);
            self.loading.write().retain(|(sid, _)| sid != session_id);
            let _ = handle.send(SessionCmd::Shutdown);
            self.events.publish(
                AppEvent::Conn(ConnEvent::Closed {
                    session_id: session_id.clone(),
                    by_user: true,
                }),
                Some(session_id.clone()),
            );
        }
    }

    pub fn execute(&self, session_id: &SessionId, sql: &str) -> Result<QueryId, String> {
        self.execute_with_autocommit(session_id, sql, true)
    }

    /// DML 执行前预览（F-63）：保守改写为等价的只读 SELECT。
    ///
    /// * 只读、零写入：构建器只产出 `SELECT`（见 `preview::build` 的断言）；
    /// * 能力位 `dml_preview` 为 false 时返回 `Unsupported`，由 UI 说明原因；
    /// * 不可安全改写（多表 UPDATE、INSERT … SELECT 等）同样返回 `Unsupported`，
    ///   但**不阻止**用户执行原语句。
    pub fn preview_dml(
        &self,
        session_id: &SessionId,
        sql: &str,
    ) -> Result<crate::preview::PreviewOutcome, String> {
        let kind = self
            .session_kind(session_id)
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let supported = self
            .sessions
            .read()
            .get(session_id)
            .map(|h| h.session.capabilities().dml_preview)
            .unwrap_or(false);
        if !supported {
            return Ok(crate::preview::PreviewOutcome::Unsupported {
                kind: rustdb_sql_lang::classify(sql),
                reason: "该方言未声明 DML 预览能力".to_string(),
            });
        }
        Ok(crate::preview::build(kind, sql))
    }

    /// 导出结果集到文件（F-72）：从 `ResultStore` 按页拉取 → 流式写盘。
    ///
    /// 返回写入的行数。内存占用与结果集大小无关（R-05）；不改变结果集本身。
    pub fn export_result_to(
        &self,
        set_id: ResultSetId,
        path: &std::path::Path,
        format: crate::export::ExportFormat,
        options: &crate::export::ExportOptions,
    ) -> Result<u64, String> {
        let meta = self
            .results
            .meta(set_id)
            .ok_or_else(|| "结果集不存在或已被淘汰".to_string())?;
        let columns: Vec<String> = meta.columns.iter().map(|c| c.name.to_string()).collect();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建导出目录失败：{e}"))?;
            }
        }
        let file = std::fs::File::create(path).map_err(|e| format!("创建文件失败：{e}"))?;
        let mut writer = std::io::BufWriter::new(file);
        let rows = ResultRows::new(self.results.clone(), set_id);
        crate::export::write_rows(&mut writer, format, &columns, rows, options)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        writer.flush().map_err(|e| format!("写入文件失败：{e}"))?;
        Ok(meta.loaded_rows)
    }

    /// 执行 SQL。`auto_commit = false` 时，若该连接还没有事务，先隐式 `BEGIN`
    /// 并让后续语句复用它（对应工具栏的自动提交开关）。
    pub fn execute_with_autocommit(
        &self,
        session_id: &SessionId,
        sql: &str,
        auto_commit: bool,
    ) -> Result<QueryId, String> {
        self.execute_with_source(
            session_id,
            sql,
            auto_commit,
            crate::history::HistorySource::Manual,
        )
    }

    /// 同 [`Self::execute_with_autocommit`]，但指定历史来源（手动 / 文件 / 外部工具）。
    pub fn execute_with_source(
        &self,
        session_id: &SessionId,
        sql: &str,
        auto_commit: bool,
        source: crate::history::HistorySource,
    ) -> Result<QueryId, String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Err("SQL 不能为空".into());
        }

        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let kind = self
            .session_kind(session_id)
            .ok_or_else(|| "连接尚未建立".to_string())?;

        // 多语句：按方言切分后交给会话 Actor 串行执行，每条产出独立结果集。
        // 这里必须用方言感知的切分器，否则 PG dollar quote / MySQL DELIMITER
        // 内部的封号会被拆成独立语句。
        let statements =
            rustdb_sql_lang::split_statements_for(trimmed, crate::params::flavor_for(kind));
        if statements.is_empty() {
            return Err("无法解析 SQL".to_string());
        }
        let first_kind = rustdb_sql_lang::classify(&statements[0].text);
        let kind_label = if statements.len() == 1 {
            format!("{first_kind:?}")
        } else {
            format!("Batch({})", statements.len())
        };

        // 安全门禁用“明确只读”白名单，而不是“明确写入”黑名单：
        // CALL / EXEC / SELECT INTO / data-modifying CTE 等都必须先过写门槛。
        if statements
            .iter()
            .any(|s| !rustdb_sql_lang::is_read_only(&s.text))
        {
            self.ensure_manual_write_allowed(session_id)?;
        }

        let query_id = QueryId::new();
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.events.publish(
            AppEvent::Query(QueryEvent::Submitted {
                request_id: format!("req-{query_id}"),
                session_id: session_id.clone(),
                sql_preview: statements
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
                    .chars()
                    .take(200)
                    .collect(),
                statement_kind: kind_label,
            }),
            Some(session_id.clone()),
        );

        let sent = handle.send(SessionCmd::Execute {
            query_id: query_id.clone(),
            request_id: format!("req-{query_id}"),
            statements: statements.into_iter().map(|s| s.text).collect(),
            max_rows: MAX_ROWS_PER_QUERY,
            auto_begin: !auto_commit,
            source,
        });
        if !sent {
            return Err("会话已关闭".into());
        }
        Ok(query_id)
    }

    /// 提交变更集（F-36/F-38）：按方言生成 DML，用**单事务**批量执行。
    ///
    /// 返回 `(query_id, 计划语句)`：调用方拿计划语句的下标去比对
    /// `QueryEvent::Completed.results[i].affected`，实现乐观并发判定（F-37）。
    ///
    /// 只读连接直接拦截（R5：显式报错，不静默丢弃）。
    pub fn apply_changes(
        &self,
        session_id: &SessionId,
        set: &crate::edit::ChangeSet,
    ) -> Result<(QueryId, Vec<crate::edit::PlannedStatement>), String> {
        if set.is_empty() {
            return Err("没有待提交的变更".into());
        }
        let kind = self
            .session_kind(session_id)
            .ok_or_else(|| "连接尚未建立".to_string())?;
        self.ensure_manual_write_allowed(session_id)?;
        let plan = set.plan(kind)?;
        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;

        let query_id = QueryId::new();
        self.queries.fetch_add(1, Ordering::Relaxed);
        let preview = plan
            .iter()
            .map(|s| s.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.events.publish(
            AppEvent::Query(QueryEvent::Submitted {
                request_id: format!("req-{query_id}"),
                session_id: session_id.clone(),
                sql_preview: preview.chars().take(200).collect(),
                statement_kind: format!("ChangeSet({})", plan.len()),
            }),
            Some(session_id.clone()),
        );

        let sent = handle.send(SessionCmd::EditBatch {
            query_id: query_id.clone(),
            request_id: format!("req-{query_id}"),
            statements: plan.iter().map(|s| s.sql.clone()).collect(),
            expect_rows: plan.iter().map(|s| s.expects_row).collect(),
            max_rows: MAX_ROWS_PER_QUERY,
        });
        if !sent {
            return Err("会话已关闭".into());
        }
        Ok((query_id, plan))
    }

    /// 执行导入计划（F-74）：清空（可选）+ 逐批 INSERT。
    ///
    /// 批次之间不共享事务，单批失败只进入失败清单并继续后续批次
    /// （需求 R-15：不承诺跨批回滚）。只读连接直接拦截。
    pub fn import(
        &self,
        session_id: &SessionId,
        plan: &crate::import::ImportPlan,
    ) -> Result<QueryId, String> {
        if plan.batches.is_empty() || plan.total_rows == 0 {
            return Err("导入计划为空".into());
        }
        self.ensure_manual_write_allowed(session_id)?;
        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;

        let query_id = QueryId::new();
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.events.publish(
            AppEvent::Query(QueryEvent::Submitted {
                request_id: format!("req-{query_id}"),
                session_id: session_id.clone(),
                sql_preview: format!(
                    "导入 {} · {} 行 / {} 批{}",
                    plan.table,
                    plan.total_rows,
                    plan.batches.len(),
                    if plan.truncate_sql.is_some() {
                        " · 先清空"
                    } else {
                        ""
                    }
                ),
                statement_kind: "Import".into(),
            }),
            Some(session_id.clone()),
        );

        let sent = handle.send(SessionCmd::Import {
            query_id: query_id.clone(),
            request_id: format!("req-{query_id}"),
            truncate_sql: plan.truncate_sql.clone(),
            batches: plan.batches.clone(),
            total_rows: plan.total_rows as u64,
        });
        if !sent {
            return Err("会话已关闭".into());
        }
        Ok(query_id)
    }

    /// 执行迁移计划（F-40）：整份 DDL 走单事务通道，失败整体回滚。
    ///
    /// 方言不支持事务性 DDL 时（MySQL/MariaDB），状态里会显式标注「不可回滚」。
    pub fn apply_migration(
        &self,
        session_id: &SessionId,
        plan: &crate::migration::MigrationPlan,
    ) -> Result<QueryId, String> {
        let label = format!("Migration({})", plan.statements.len());
        self.apply_statements(session_id, plan.executable_sql(), &label)
    }

    /// 保存例程源码（F-82）：语句走同一个 DDL 通道，写门槛与迁移一致。
    pub fn apply_routine_save(
        &self,
        session_id: &SessionId,
        plan: &crate::routine::RoutineSavePlan,
    ) -> Result<QueryId, String> {
        if let Some(error) = &plan.error {
            return Err(error.clone());
        }
        if plan.statements.is_empty() {
            return Err("没有可执行的语句".into());
        }
        let label = format!("Routine({})", plan.statements.len());
        self.apply_statements(session_id, plan.statements.clone(), &label)
    }

    /// 执行分区 DDL（F-85）：与例程保存共用同一写门槛。
    pub fn apply_partition_plan(
        &self,
        session_id: &SessionId,
        plan: &crate::partition::PartitionPlan,
    ) -> Result<QueryId, String> {
        if let Some(error) = &plan.error {
            return Err(error.clone());
        }
        if plan.statements.is_empty() {
            return Err("没有可执行的语句".into());
        }
        let label = format!("Partition({})", plan.statements.len());
        self.apply_statements(session_id, plan.statements.clone(), &label)
    }

    /// 把一串 DDL 语句交给会话执行（写门槛 + 事件在这层统一处理）。
    fn apply_statements(
        &self,
        session_id: &SessionId,
        statements: Vec<String>,
        label: &str,
    ) -> Result<QueryId, String> {
        if statements.is_empty() {
            return Err("没有可执行的语句".into());
        }
        self.ensure_manual_write_allowed(session_id)?;
        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let query_id = QueryId::new();
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.events.publish(
            AppEvent::Query(QueryEvent::Submitted {
                request_id: format!("req-{query_id}"),
                session_id: session_id.clone(),
                sql_preview: statements.join(" ").chars().take(200).collect::<String>(),
                statement_kind: label.to_string(),
            }),
            Some(session_id.clone()),
        );
        let sent = handle.send(SessionCmd::EditBatch {
            query_id: query_id.clone(),
            request_id: format!("req-{query_id}"),
            expect_rows: vec![false; statements.len()],
            statements,
            max_rows: 0,
        });
        if !sent {
            return Err("会话已关闭".into());
        }
        Ok(query_id)
    }

    /// 发布一条 AI 事件（Agent 引擎用；AI 事件不带会话作用域）。
    pub fn emit_ai(&self, event: crate::events::AiEvent) {
        self.events.publish(AppEvent::Ai(event), None);
    }

    /// 供 Agent 等后台任务提交到同一个 tokio 运行时。
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// 会话对应的连接档案（会话不存在时返回 `None`）。
    fn session_profile(&self, session_id: &SessionId) -> Option<ConnectionProfile> {
        let profile_id = self.sessions.read().get(session_id)?.profile_id.clone();
        self.profiles
            .read()
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
    }

    /// 会话对应的数据源是否为只读模式（AI 安全审查复用）。
    pub fn is_read_only_session(&self, session_id: &SessionId) -> bool {
        self.session_profile(session_id)
            .is_some_and(|profile| profile.read_only)
    }

    /// F-89：该会话是否生产连接（AI 审查用；忽略临时解锁）。
    pub fn is_production_session(&self, session_id: &SessionId) -> bool {
        self.session_profile(session_id)
            .is_some_and(|profile| profile.production)
    }

    /// 人工写入的统一门槛：只读连接一律拒绝；生产连接需临时解锁。
    fn ensure_manual_write_allowed(&self, session_id: &SessionId) -> Result<(), String> {
        let profile = self
            .session_profile(session_id)
            .ok_or_else(|| "连接尚未建立".to_string())?;
        if profile.read_only {
            return Err("该连接为只读模式，写操作已被拦截".into());
        }
        if profile.production && self.write_unlock_remaining(&profile.id).is_none() {
            return Err("生产连接的写操作未解锁；请先临时解锁（1 / 5 分钟）".into());
        }
        Ok(())
    }

    /// 取消是旁路：直接调驱动的 cancel，不排进会话 Actor 的语句队列，
    /// 否则长查询会把取消命令堵在队列里。
    pub fn cancel(&self, session_id: &SessionId, query_id: &QueryId) -> Result<(), String> {
        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;
        self.events.publish(
            AppEvent::Query(QueryEvent::Cancelling {
                query_id: query_id.clone(),
            }),
            Some(session_id.clone()),
        );
        let session = handle.session.clone();
        let qid = query_id.clone();
        self.runtime.spawn(async move {
            if let Err(e) = session.cancel(&qid).await {
                log::warn!("取消失败（{qid}）：{e}");
            }
        });
        Ok(())
    }

    /// 手动开启事务（工具栏「开始事务」）。命令排在会话队列里，保证与语句串行。
    pub fn begin_tx(&self, session_id: &SessionId, read_only: bool) -> Result<(), String> {
        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        if !handle.send(SessionCmd::BeginTx { read_only, reply }) {
            return Err("会话已关闭".into());
        }
        let events = self.events.clone();
        self.runtime.spawn(async move {
            if let Ok(Err(e)) = rx.await {
                events.publish(
                    AppEvent::Tx(crate::events::TxEvent::Failed {
                        action: "开始事务".into(),
                        message: e.user_message(),
                    }),
                    None,
                );
            }
        });
        Ok(())
    }

    /// 提交当前事务。
    pub fn commit_tx(&self, session_id: &SessionId) -> Result<(), String> {
        self.finish_tx(session_id, false)
    }

    /// 回滚当前事务。
    pub fn rollback_tx(&self, session_id: &SessionId) -> Result<(), String> {
        self.finish_tx(session_id, true)
    }

    fn finish_tx(&self, session_id: &SessionId, rollback: bool) -> Result<(), String> {
        let handle = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| "连接尚未建立".to_string())?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        let cmd = if rollback {
            SessionCmd::RollbackTx { reply }
        } else {
            SessionCmd::CommitTx { reply }
        };
        if !handle.send(cmd) {
            return Err("会话已关闭".into());
        }
        let events = self.events.clone();
        let action = if rollback { "回滚" } else { "提交" };
        self.runtime.spawn(async move {
            if let Ok(Err(e)) = rx.await {
                events.publish(
                    AppEvent::Tx(crate::events::TxEvent::Failed {
                        action: action.into(),
                        message: e.user_message(),
                    }),
                    None,
                );
            }
        });
        Ok(())
    }

    /// 服务端分页（X-02 / F-70）：优先向会话驱动取一页，不进语句队列。
    ///
    /// 驱动未实现 `fetch_page` 时，用 `build_page_sql_literal` 生成方言 SQL，
    /// 再走一次 execute 取 `page_size + 1` 行；这样 dump / transfer /
    /// data_compare 不要求每个驱动都先实现分页。
    pub async fn fetch_page(
        &self,
        session_id: &SessionId,
        req: &PageRequest,
    ) -> Result<PageResult, DriverError> {
        let session = self
            .sessions
            .read()
            .get(session_id)
            .cloned()
            .ok_or_else(|| DriverError::unsupported("连接尚未建立"))?
            .session;
        let kind = self
            .session_kind(session_id)
            .ok_or_else(|| DriverError::unsupported("连接尚未建立"))?;
        let req = req.clone();
        let req_for_driver = req.clone();
        let session_for_fallback = session.clone();
        // UI 会直接 await 这个 future；驱动内部用 tokio 原语（如 spawn_blocking），
        // 必须切到 core 的运行时里执行，否则会因「没有 reactor」panic。
        let fetched = self
            .runtime
            .spawn(async move { session.fetch_page(&req_for_driver).await })
            .await
            .map_err(|err| DriverError::internal(err.to_string()))?;
        match fetched {
            Ok(page) => Ok(page),
            Err(error) if error.kind == rustdb_db_api::DriverErrorKind::Unsupported => {
                self.fetch_page_fallback(session_for_fallback, kind, &req)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    /// 把驱动错误转成用户可读文案（供 UI 直接显示）。
    pub fn describe_error(err: &DriverError) -> String {
        err.user_message()
    }

    /// 会话对应的驱动种类（生成方言相关 SQL 时用；未连接返回 `None`）。
    pub fn session_kind(&self, session_id: &SessionId) -> Option<DbKind> {
        let profile_id = self.sessions.read().get(session_id)?.profile_id.clone();
        self.profiles
            .read()
            .iter()
            .find(|profile| profile.id == profile_id)
            .map(|profile| profile.driver)
    }

    /// 会话实时状态快照（同步读取，不排进会话命令队列）。
    ///
    /// UI 在事件广播 `Lagged` 后用它校正「执行中 / 事务中」，
    /// 避免标签页永久停在“正在执行”。
    pub fn session_state(&self, session_id: &SessionId) -> Option<SessionLiveState> {
        let handle = self.sessions.read().get(session_id).cloned()?;
        Some(SessionLiveState {
            active_query: handle.state.active_query(),
            tx: handle.state.tx(),
        })
    }

    /// 会话对应的连接配置 id；UI 用来丢弃迟到的连接事件。
    pub fn session_profile_id(&self, session_id: &SessionId) -> Option<String> {
        self.sessions
            .read()
            .get(session_id)
            .map(|handle| handle.profile_id.clone())
    }

    /// 「查看数据」：按会话方言生成限定名的 SELECT 语句。
    pub fn object_select_sql(
        &self,
        session_id: &SessionId,
        path: &rustdb_db_api::ObjectPath,
        limit: u64,
    ) -> Option<String> {
        let kind = self.session_kind(session_id)?;
        Some(object_select_sql(kind, path, limit))
    }

    /// 对象的方言限定名（`db`.`tbl` / "schema"."tbl"）。
    pub fn object_from_clause(
        &self,
        session_id: &SessionId,
        path: &rustdb_db_api::ObjectPath,
    ) -> Option<String> {
        let kind = self.session_kind(session_id)?;
        Some(object_from_clause(kind, path))
    }

    /// F-73：生成批量更新计划（只生成 SQL，不执行）。
    pub fn plan_bulk_update(
        &self,
        session_id: &SessionId,
        request: &crate::bulk_update::BulkUpdateRequest,
    ) -> Result<crate::bulk_update::BulkUpdatePlan, String> {
        let kind = self
            .session_kind(session_id)
            .ok_or_else(|| "连接尚未建立".to_string())?;
        crate::bulk_update::plan(kind, request)
    }

    /// F-73：执行已预览的批量更新；只读与生产写门控照常生效。
    pub fn apply_bulk_update(
        &self,
        session_id: &SessionId,
        plan: &crate::bulk_update::BulkUpdatePlan,
        auto_commit: bool,
    ) -> Result<QueryId, String> {
        self.ensure_manual_write_allowed(session_id)?;
        self.execute_with_source(
            session_id,
            &plan.update_sql,
            auto_commit,
            crate::history::HistorySource::Manual,
        )
    }

    /// F-73：外键跳转查询（沿用源对象所在命名空间）。
    pub fn foreign_key_lookup_sql(
        &self,
        session_id: &SessionId,
        source_ns: &Namespace,
        foreign_key: &ForeignKeyInfo,
        values: &[Value],
    ) -> Result<String, String> {
        let kind = self
            .session_kind(session_id)
            .ok_or_else(|| "连接尚未建立".to_string())?;
        crate::bulk_update::foreign_key_lookup_sql(kind, source_ns, foreign_key, values)
    }
}

/// 连接树上某个对象的限定名（已按方言加好引用符）。
pub fn object_from_clause(kind: DbKind, path: &rustdb_db_api::ObjectPath) -> String {
    let quote = |ident: &str| match kind {
        DbKind::Mysql | DbKind::Mariadb => rustdb_db_api::dialect::quote_backtick(ident),
        DbKind::Mssql => rustdb_db_api::dialect::quote_bracket(ident),
        _ => rustdb_db_api::dialect::quote_double(ident),
    };
    let mut parts: Vec<String> = Vec::new();
    // SQLite 的 `main` 是库名而非 schema，不能进 `FROM` 的限定名。
    let skip_catalog = matches!(kind, DbKind::Sqlite);
    if !skip_catalog {
        if let Some(catalog) = &path.ns.catalog {
            parts.push(quote(catalog));
        }
    }
    if let Some(schema) = &path.ns.schema {
        parts.push(quote(schema));
    }
    parts.push(quote(&path.name));
    parts.join(".")
}

/// 内置示例连接配置（没有落盘配置时使用）。
///
/// 本机 Docker Desktop 的 IPv4 端口转发失效：实测 `127.0.0.1:55432` TCP 可连但
/// PG 协议无响应（黑洞），而 `localhost` / `::1:55432` 能完成 SCRAM-SHA-256 握手。
/// 因此默认用 `localhost`（双栈解析，兼容 IPv4/IPv6 回环），并允许用环境变量覆盖，
/// 供实机点击测试指向自定义主机。此改动不改变默认语义，只修双栈兼容。
fn default_profiles() -> Vec<ConnectionProfile> {
    let mut profiles = vec![ConnectionProfile::demo_sqlite()];
    if std::env::var("RUSTDB_DEV_DB").as_deref() == Ok("1") {
        // 开发环境主机：默认 localhost，可用 RUSTDB_DEV_PG_HOST / RUSTDB_DEV_MYSQL_HOST 覆盖。
        let dev_pg_host = std::env::var("RUSTDB_DEV_PG_HOST")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "localhost".to_string());
        let dev_mysql_host = std::env::var("RUSTDB_DEV_MYSQL_HOST")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "localhost".to_string());
        profiles.push(ConnectionProfile {
            id: "dev-postgres".into(),
            name: "PostgreSQL".into(),
            folder: Some("本地开发".into()),
            driver: DbKind::Postgres,
            host: Some(dev_pg_host),
            port: Some(55432),
            database: Some("rustdb_test".into()),
            username: Some("postgres".into()),
            secret_ref: None,
            dev_secret: Some("rustdb".into()),
            ssl_mode: Some("disable".into()),
            params: Default::default(),
            read_only: false,
            production: false,
            tunnel: None,
            proxy: None,
            forward_lan: false,
            connect_timeout_ms: 5_000,
            color: Some(0x336791),
            last_used_at: None,
        });
        profiles.push(ConnectionProfile {
            id: "dev-mysql".into(),
            name: "MySQL".into(),
            folder: Some("本地开发".into()),
            driver: DbKind::Mysql,
            host: Some(dev_mysql_host),
            port: Some(53306),
            database: Some("rustdb_test".into()),
            username: Some("root".into()),
            secret_ref: None,
            dev_secret: Some("rustdb".into()),
            ssl_mode: None,
            params: Default::default(),
            read_only: false,
            production: false,
            tunnel: None,
            proxy: None,
            forward_lan: false,
            connect_timeout_ms: 5_000,
            color: Some(0xE48E00),
            last_used_at: None,
        });
    }
    profiles
}

/// 隧道启动的失败分类。
enum TunnelStart {
    /// 主机密钥待用户确认（已发出事件）。
    HostKey,
    /// 其它失败（已带可读原因）。
    Failed(String),
}

/// 某层跳板的主机与端口（供事件展示）。
fn hop_endpoint(profile: &ConnectionProfile, layer: usize) -> (Option<String>, Option<u16>) {
    let hop = profile
        .tunnel
        .as_ref()
        .and_then(|spec| spec.hops.get(layer.saturating_sub(1)));
    (hop.map(|hop| hop.host.clone()), hop.map(|hop| hop.port))
}

/// 建链摘要：代理按 `order` 放在 SSH 之前或之后。
fn route_summary(
    spec: &rustdb_db_api::TunnelSpec,
    proxy: Option<&rustdb_db_api::ProxySpec>,
    target: &str,
) -> String {
    let hops = spec.hops_summary();
    match proxy {
        Some(proxy) if proxy.order == rustdb_db_api::ProxyOrder::BeforeTunnel => {
            format!("代理 {} → {hops} → 目标 {target}", proxy.summary())
        }
        Some(proxy) => format!("{hops} → 代理 {} → 目标 {target}", proxy.summary()),
        None => format!("{hops} → 目标 {target}"),
    }
}

/// F-91：等待用户确认的主机密钥。
#[derive(Debug, Clone)]
pub struct PendingHostKey {
    /// 第几层跳板。
    pub layer: usize,
    pub host: String,
    pub port: u16,
    /// 本次拿到的指纹（SHA-256）。
    pub fingerprint: String,
    /// 本次的 OpenSSH 公钥行；确认后写回 known_hosts。
    pub public_key: String,
    /// true = 与已有记录不一致（可能是中间人替换）。
    pub changed: bool,
    /// 变更时的旧指纹。
    pub recorded: Option<String>,
    /// 写回用的 known_hosts 路径。
    pub known_hosts: Option<std::path::PathBuf>,
}

/// 连接树上某个对象的「查看数据」SQL。
pub fn object_select_sql(kind: DbKind, path: &rustdb_db_api::ObjectPath, limit: u64) -> String {
    format!(
        "SELECT *\nFROM {}\nLIMIT {limit};",
        object_from_clause(kind, path)
    )
}

/// 选一个「当前命名空间」：优先匹配服务端当前库，其次 public，最后取第一个。
fn pick_current(list: &[Namespace], server: &ServerInfo) -> Namespace {
    if let Some(db) = &server.current_db {
        if let Some(ns) = list
            .iter()
            .find(|n| n.catalog.as_deref() == Some(db.as_str()) && n.schema.is_none())
        {
            return ns.clone();
        }
        if let Some(ns) = list.iter().find(|n| {
            n.catalog.as_deref() == Some(db.as_str()) && n.schema.as_deref() == Some("public")
        }) {
            return ns.clone();
        }
        if let Some(ns) = list
            .iter()
            .find(|n| n.catalog.as_deref() == Some(db.as_str()))
        {
            return ns.clone();
        }
    }
    list.first().cloned().unwrap_or(Namespace {
        catalog: None,
        schema: None,
    })
}

/// 结果集 → 行迭代器：按 `PAGE_ROWS` 分页从 `ResultStore` 取，供导出流式消费。
struct ResultRows {
    store: Arc<ResultStore>,
    id: ResultSetId,
    offset: usize,
    buffer: std::vec::IntoIter<Row>,
    eof: bool,
}

impl ResultRows {
    fn new(store: Arc<ResultStore>, id: ResultSetId) -> Self {
        Self {
            store,
            id,
            offset: 0,
            buffer: Vec::new().into_iter(),
            eof: false,
        }
    }
}

impl Iterator for ResultRows {
    type Item = Row;

    fn next(&mut self) -> Option<Row> {
        loop {
            if let Some(row) = self.buffer.next() {
                return Some(row);
            }
            if self.eof {
                return None;
            }
            let page = self.store.rows(self.id, self.offset, PAGE_ROWS);
            if page.is_empty() {
                self.eof = true;
                return None;
            }
            self.offset += page.len();
            self.buffer = page.into_iter();
        }
    }
}

/// 导出写盘目标：普通文件或 gzip 流；`finish` 必须显式调用以写完压缩尾。
enum DumpSink {
    Plain(std::io::BufWriter<std::fs::File>),
    Gzip(flate2::write::GzEncoder<std::io::BufWriter<std::fs::File>>),
}

impl DumpSink {
    fn finish(self) -> std::io::Result<()> {
        match self {
            Self::Plain(mut writer) => writer.flush(),
            Self::Gzip(writer) => writer.finish().map(|_| ()),
        }
    }
}

impl std::io::Write for DumpSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(writer) => writer.write(buf),
            Self::Gzip(writer) => writer.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(writer) => writer.flush(),
            Self::Gzip(writer) => writer.flush(),
        }
    }
}

/// 多库导出时的文件名主干：只保留字母数字、下划线与短横线。
fn dump_file_stem(ns: &Namespace) -> String {
    let raw = ns.display_label();
    let stem: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "database".to_string()
    } else {
        stem
    }
}

/// F-75：为每张源表生成传输计划（预览与执行共用，不写任何数据）。
#[allow(clippy::too_many_arguments)]
async fn build_transfer_plans(
    source: &SessionHandle,
    source_kind: DbKind,
    target: &Arc<dyn Session>,
    target_kind: DbKind,
    source_ns: &Namespace,
    target_ns: &Namespace,
    tables: &[String],
    options: &crate::transfer::TransferOptions,
) -> Result<Vec<crate::transfer::TransferTablePlan>, String> {
    let target_objects = target
        .metadata()
        .objects(target_ns, ObjectKindMask::only(ObjectKind::Table))
        .await
        .map_err(|error| format!("读取目标表失败：{}", error.user_message()))?;
    let target_names: Vec<String> = target_objects
        .iter()
        .map(|object| object.path.name.to_string())
        .collect();

    let mut plans = Vec::with_capacity(tables.len());
    for table in tables {
        let source_path =
            rustdb_db_api::ObjectPath::new(source_ns.clone(), table.clone(), ObjectKind::Table);
        let source_columns = match source.session.metadata().columns(&source_path).await {
            Ok(columns) => columns,
            Err(error) => {
                let found = crate::transfer::find_target_table(table, options, &target_names);
                let target_name = found
                    .cloned()
                    .unwrap_or_else(|| crate::transfer::target_table_name(table, options));
                let target_path = rustdb_db_api::ObjectPath::new(
                    target_ns.clone(),
                    target_name,
                    ObjectKind::Table,
                );
                plans.push(crate::transfer::plan_failure(
                    &source_path,
                    &target_path,
                    options.mode,
                    found.is_some(),
                    format!("读取源表结构失败：{}", error.user_message()),
                ));
                continue;
            }
        };
        let found = crate::transfer::find_target_table(table, options, &target_names).cloned();
        let target_name =
            found.unwrap_or_else(|| crate::transfer::target_table_name(table, options));
        let target_path =
            rustdb_db_api::ObjectPath::new(target_ns.clone(), target_name, ObjectKind::Table);
        let target_columns = if target_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(target_path.name.as_ref()))
        {
            Some(
                target
                    .metadata()
                    .columns(&target_path)
                    .await
                    .unwrap_or_default(),
            )
        } else {
            None
        };
        plans.push(crate::transfer::plan_table(
            &source_path,
            &source_columns,
            &target_path,
            target_columns.as_deref(),
            source_kind,
            target_kind,
            options,
        ));
    }
    Ok(plans)
}

#[allow(clippy::too_many_arguments)]
/// 执行一条传输语句并等结果流结束。
async fn run_transfer_sql(
    session: &Arc<dyn Session>,
    sql: &str,
    timeout: Option<Duration>,
) -> Result<(), String> {
    let request = ExecuteRequest {
        sql: sql.to_string(),
        params: Vec::new(),
        fetch: FetchOptions {
            max_rows: 0,
            ..FetchOptions::default()
        },
        tx: None,
        read_only: false,
        request_id: None,
        query_id: Some(QueryId::new()),
    };
    let run = async {
        let handle = session
            .execute(request)
            .await
            .map_err(|error| error.user_message())?;
        let mut stream = handle.stream;
        while let Some(item) = stream.next().await {
            item.map_err(|error| error.user_message())?;
        }
        Ok::<(), String>(())
    };
    match timeout {
        Some(limit) => tokio::time::timeout(limit, run)
            .await
            .map_err(|_| format!("语句超时（{} ms）", limit.as_millis()))?,
        None => run.await,
    }
}

/// 单表传输：DDL → 分页读源 → 分批写目标；返回写入行数。
#[allow(clippy::too_many_arguments)]
async fn transfer_one_table(
    source: &Arc<dyn Session>,
    target: &Arc<dyn Session>,
    target_kind: DbKind,
    plan: &crate::transfer::TransferTablePlan,
    options: &crate::transfer::TransferOptions,
    sid: &SessionId,
    events: &EventBus,
) -> Result<u64, String> {
    let timeout = options
        .timeout_ms
        .filter(|value| *value > 0)
        .map(Duration::from_millis);
    for sql in plan.structure_sql() {
        run_transfer_sql(target, &sql, timeout)
            .await
            .map_err(|error| format!("结构变更失败：{error}"))?;
    }

    let batch = options
        .effective_batch(target_kind)
        .clamp(1, rustdb_db_api::MAX_PAGE_SIZE as usize);
    let target_columns = plan.target_columns();
    let order: Vec<PageOrder> = plan
        .columns
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| PageOrder::asc(column.source.clone()))
        .collect();
    let mut page = 1u64;
    let mut total = 0u64;
    loop {
        let mut request = PageRequest::new(plan.source.clone());
        request.page = page;
        request.page_size = batch as u32;
        request.order_by = order.clone();
        let result = match timeout {
            Some(limit) => tokio::time::timeout(limit, source.fetch_page(&request))
                .await
                .map_err(|_| format!("读取源表超时（{} ms）", limit.as_millis()))?
                .map_err(|error| format!("读取源表失败：{}", error.user_message()))?,
            None => source
                .fetch_page(&request)
                .await
                .map_err(|error| format!("读取源表失败：{}", error.user_message()))?,
        };
        if result.rows.is_empty() {
            break;
        }
        let positions = plan
            .columns
            .iter()
            .map(|column| {
                result
                    .columns
                    .iter()
                    .position(|meta| meta.name.as_ref() == column.source)
            })
            .collect::<Vec<_>>();
        if positions.iter().any(Option::is_none) {
            return Err("源表列已变化，请重新打开传输面板".into());
        }
        let positions = positions.into_iter().flatten().collect::<Vec<_>>();
        let rows = result
            .rows
            .iter()
            .map(|row| {
                positions
                    .iter()
                    .map(|index| row.get(*index).cloned().unwrap_or(Value::Null))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let sql = crate::transfer::insert_sql(
            target_kind,
            &plan.target,
            &target_columns,
            &rows,
            plan.mode,
            &plan.key_columns,
        )?;
        run_transfer_sql(target, &sql, timeout)
            .await
            .map_err(|error| format!("写入失败：{error}"))?;
        total += rows.len() as u64;
        events.publish(
            AppEvent::Transfer(TransferEvent::TableProgress {
                table: plan.source.name.to_string(),
                rows: total,
            }),
            Some(sid.clone()),
        );
        if !result.has_more {
            break;
        }
        page += 1;
    }
    Ok(total)
}

/// F-78：对比单表。先读目标全部行建立键索引，再分页读源表喂给差异器。
#[allow(clippy::too_many_arguments)]
async fn compare_one_table(
    source: &Arc<dyn Session>,
    source_ns: &Namespace,
    target: &Arc<dyn Session>,
    target_kind: DbKind,
    target_ns: &Namespace,
    target_names: &[String],
    config: &crate::data_compare::TableCompareConfig,
    options: &crate::data_compare::DataCompareOptions,
) -> Result<
    (
        crate::data_compare::DataCompareTablePlan,
        crate::data_compare::TableDiff,
    ),
    String,
> {
    let source_path =
        rustdb_db_api::ObjectPath::new(source_ns.clone(), config.source.clone(), ObjectKind::Table);
    let source_columns = source
        .metadata()
        .columns(&source_path)
        .await
        .map_err(|error| format!("读取源表结构失败：{}", error.user_message()))?;
    let wanted = config
        .target
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&config.source)
        .to_string();
    let Some(target_name) = target_names
        .iter()
        .find(|name| name.eq_ignore_ascii_case(&wanted))
        .cloned()
    else {
        return Err(format!(
            "目标表 {} 不存在；请先用结构对比 / 跨库传输建表",
            wanted
        ));
    };
    let target_path =
        rustdb_db_api::ObjectPath::new(target_ns.clone(), target_name, ObjectKind::Table);
    let target_columns = target
        .metadata()
        .columns(&target_path)
        .await
        .map_err(|error| format!("读取目标表结构失败：{}", error.user_message()))?;
    let plan = crate::data_compare::plan_compare_table(
        &source_path,
        &source_columns,
        &target_path,
        &target_columns,
        config,
        options,
    );
    if let Some(error) = plan.error.clone() {
        return Err(error);
    }

    let timeout = options
        .timeout_ms
        .filter(|value| *value > 0)
        .map(Duration::from_millis);
    let batch = options
        .effective_batch(target_kind)
        .clamp(1, rustdb_db_api::MAX_PAGE_SIZE as usize);
    let target_order: Vec<PageOrder> = plan
        .target_key_columns()
        .into_iter()
        .map(PageOrder::asc)
        .collect();
    let target_rows = fetch_compare_rows(
        target,
        &plan.target,
        &target_order,
        &plan.target_columns(),
        batch,
        timeout,
        "目标表",
    )
    .await?;
    let mut differ = crate::data_compare::RowDiffer::new(plan.key_indexes.clone(), target_rows);

    let source_order: Vec<PageOrder> = plan.key_columns().into_iter().map(PageOrder::asc).collect();
    let wanted_source = plan.source_columns();
    let mut page = 1u64;
    loop {
        let (rows, has_more) = fetch_compare_page(
            source,
            &plan.source,
            page,
            batch,
            &source_order,
            &wanted_source,
            timeout,
            "源表",
        )
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            differ.push(row);
        }
        if !has_more {
            break;
        }
        page += 1;
    }
    Ok((plan, differ.finish()))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_compare_page(
    session: &Arc<dyn Session>,
    path: &rustdb_db_api::ObjectPath,
    page: u64,
    batch: usize,
    order: &[PageOrder],
    wanted: &[String],
    timeout: Option<Duration>,
    label: &str,
) -> Result<(Vec<Vec<Value>>, bool), String> {
    let mut request = PageRequest::new(path.clone());
    request.page = page;
    request.page_size = batch as u32;
    request.order_by = order.to_vec();
    let result = match timeout {
        Some(limit) => tokio::time::timeout(limit, session.fetch_page(&request))
            .await
            .map_err(|_| format!("读取{label}超时（{} ms）", limit.as_millis()))?
            .map_err(|error| format!("读取{label}失败：{}", error.user_message()))?,
        None => session
            .fetch_page(&request)
            .await
            .map_err(|error| format!("读取{label}失败：{}", error.user_message()))?,
    };
    if result.rows.is_empty() {
        return Ok((Vec::new(), false));
    }
    let positions = wanted
        .iter()
        .map(|name| {
            result
                .columns
                .iter()
                .position(|meta| meta.name.as_ref() == name)
        })
        .collect::<Vec<_>>();
    if positions.iter().any(Option::is_none) {
        return Err(format!("{label}列已变化，请重新打开对比面板"));
    }
    let positions = positions.into_iter().flatten().collect::<Vec<_>>();
    let rows = result
        .rows
        .iter()
        .map(|row| {
            positions
                .iter()
                .map(|index| row.get(*index).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .collect();
    Ok((rows, result.has_more))
}

async fn fetch_compare_rows(
    session: &Arc<dyn Session>,
    path: &rustdb_db_api::ObjectPath,
    order: &[PageOrder],
    wanted: &[String],
    batch: usize,
    timeout: Option<Duration>,
    label: &str,
) -> Result<Vec<Vec<Value>>, String> {
    let mut rows = Vec::new();
    let mut page = 1u64;
    loop {
        let (chunk, has_more) =
            fetch_compare_page(session, path, page, batch, order, wanted, timeout, label).await?;
        if chunk.is_empty() {
            break;
        }
        rows.extend(chunk);
        if !has_more {
            break;
        }
        page += 1;
    }
    Ok(rows)
}

async fn run_dump(
    core: &Arc<Core>,
    session: &Arc<dyn rustdb_db_api::Session>,
    sid: &SessionId,
    driver: DbKind,
    ns: Namespace,
    tables: Vec<String>,
    options: crate::dump::DumpOptions,
    dest: std::path::PathBuf,
) -> Result<(), String> {
    let metadata = session.metadata();
    let objects = metadata
        .objects(&ns, ObjectKindMask::only(ObjectKind::Table))
        .await
        .map_err(|error| error.user_message())?;
    let wanted: HashSet<String> = tables
        .iter()
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    let mut summaries: Vec<ObjectSummary> = objects
        .into_iter()
        .filter(|object| {
            object.path.kind == ObjectKind::Table
                && (wanted.is_empty() || wanted.contains(&object.path.name.to_lowercase()))
        })
        .collect();
    summaries.sort_by(|a, b| a.path.name.to_lowercase().cmp(&b.path.name.to_lowercase()));
    let skipped = summaries.len().saturating_sub(crate::dump::MAX_DUMP_TABLES);
    summaries.truncate(crate::dump::MAX_DUMP_TABLES);

    core.events.publish(
        AppEvent::Dump(DumpEvent::Started {
            namespace: ns.display_label(),
            tables: summaries.len(),
            path: dest.display().to_string(),
            gzip: options.gzip,
        }),
        Some(sid.clone()),
    );

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建导出目录失败（{}）：{error}", parent.display()))?;
    }
    let file_name = dest
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "dump.sql".into());
    let temp = dest.with_file_name(format!(".{file_name}.part"));
    let result = write_dump(
        core, session, sid, driver, &ns, &summaries, skipped, &options, &temp,
    )
    .await;
    match result {
        Ok((summary, bytes)) => {
            if let Err(error) = std::fs::rename(&temp, &dest) {
                let _ = std::fs::remove_file(&temp);
                return Err(format!("保存 {} 失败：{error}", dest.display()));
            }
            core.events.publish(
                AppEvent::Dump(DumpEvent::Finished {
                    path: dest.display().to_string(),
                    tables: summary.tables,
                    objects: summary.objects,
                    rows: summary.rows,
                    failed: summary.failed_tables,
                    bytes,
                }),
                Some(sid.clone()),
            );
            Ok(())
        }
        Err(message) => {
            let _ = std::fs::remove_file(&temp);
            Err(message)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_dump(
    core: &Arc<Core>,
    session: &Arc<dyn rustdb_db_api::Session>,
    sid: &SessionId,
    driver: DbKind,
    ns: &Namespace,
    summaries: &[ObjectSummary],
    skipped: usize,
    options: &crate::dump::DumpOptions,
    temp: &std::path::Path,
) -> Result<(crate::dump::DumpSummary, u64), String> {
    let file = std::fs::File::create(temp)
        .map_err(|error| format!("创建导出文件失败（{}）：{error}", temp.display()))?;
    let sink = if options.gzip {
        DumpSink::Gzip(flate2::write::GzEncoder::new(
            std::io::BufWriter::new(file),
            flate2::Compression::default(),
        ))
    } else {
        DumpSink::Plain(std::io::BufWriter::new(file))
    };
    let mut writer = crate::dump::DumpWriter::new(sink, driver, options.clone());
    writer
        .header(ns, skipped)
        .map_err(|error| error.to_string())?;

    let metadata = session.metadata();
    for summary in summaries {
        let path = summary.path.clone();
        let (ddl, ddl_error) = match metadata.object_ddl(&path).await {
            Ok(Some(ddl)) => (Some(ddl.text), None),
            Ok(None) => (None, None),
            Err(error) => (None, Some(error.user_message())),
        };
        let head = crate::dump::DumpTableInput {
            path: path.clone(),
            ddl,
            ddl_error: ddl_error.clone(),
            columns: Vec::new(),
            rows: Vec::new(),
            data_error: None,
        };
        writer
            .begin_table(&head)
            .map_err(|error| error.to_string())?;

        let mut table_rows = 0u64;
        let mut data_error = None;
        if options.include_data {
            let batch = options.batch_limit(driver);
            let mut columns: Vec<String> = Vec::new();
            let mut buffered: Vec<Vec<Value>> = Vec::with_capacity(batch);
            let mut page = 1u64;
            loop {
                let mut request = PageRequest::new(path.clone());
                request.page = page;
                request.page_size = batch as u32;
                match session.fetch_page(&request).await {
                    Ok(result) => {
                        let has_more = result.has_more;
                        if columns.is_empty() {
                            columns = result
                                .columns
                                .iter()
                                .map(|column| column.name.to_string())
                                .collect();
                        }
                        for row in result.rows {
                            buffered.push(row);
                            if buffered.len() >= batch {
                                let outcome = writer
                                    .insert_rows(&path, &columns, &buffered)
                                    .map_err(|error| error.to_string())?;
                                table_rows += outcome.rows;
                                buffered.clear();
                            }
                        }
                        if !has_more {
                            break;
                        }
                    }
                    Err(error) => {
                        data_error = Some(error.user_message());
                        break;
                    }
                }
                page += 1;
            }
            if !buffered.is_empty() {
                let outcome = writer
                    .insert_rows(&path, &columns, &buffered)
                    .map_err(|error| error.to_string())?;
                table_rows += outcome.rows;
            }
        }
        if let Some(error) = &data_error {
            writer
                .note_failure(&format!(
                    "表 {} 的数据读取失败：{error}",
                    path.display_label()
                ))
                .map_err(|error| error.to_string())?;
        }
        writer.end_table().map_err(|error| error.to_string())?;
        core.events.publish(
            AppEvent::Dump(DumpEvent::TableDone {
                table: path.name.to_string(),
                rows: table_rows,
                error: data_error.or(ddl_error),
            }),
            Some(sid.clone()),
        );
    }

    if options.include_objects {
        match metadata.objects(ns, ObjectKindMask::ALL).await {
            Ok(objects) => {
                let mut others: Vec<ObjectSummary> = objects
                    .into_iter()
                    .filter(|object| {
                        !matches!(object.path.kind, ObjectKind::Table | ObjectKind::Index)
                    })
                    .collect();
                others.sort_by(|a, b| {
                    crate::dump::object_order(a.path.kind)
                        .cmp(&crate::dump::object_order(b.path.kind))
                        .then_with(|| a.path.name.to_lowercase().cmp(&b.path.name.to_lowercase()))
                });
                let overflow = others.len().saturating_sub(crate::dump::MAX_DUMP_OBJECTS);
                others.truncate(crate::dump::MAX_DUMP_OBJECTS);
                if overflow > 0 {
                    writer
                        .note_failure(&format!(
                            "有 {overflow} 个对象因单次上限（{}）未导出",
                            crate::dump::MAX_DUMP_OBJECTS
                        ))
                        .map_err(|error| error.to_string())?;
                }
                for object in others {
                    let (ddl, error) = match metadata.object_ddl(&object.path).await {
                        Ok(Some(ddl)) => (Some(ddl.text), None),
                        Ok(None) => (None, None),
                        Err(error) => (None, Some(error.user_message())),
                    };
                    let input = crate::dump::DumpObjectInput {
                        path: object.path.clone(),
                        ddl,
                        error: error.clone(),
                    };
                    writer.object(&input).map_err(|error| error.to_string())?;
                    core.events.publish(
                        AppEvent::Dump(DumpEvent::ObjectDone {
                            object: object.path.display_label(),
                            error,
                        }),
                        Some(sid.clone()),
                    );
                }
            }
            Err(error) => {
                writer
                    .note_failure(&format!("读取其它对象失败：{}", error.user_message()))
                    .map_err(|error| error.to_string())?;
            }
        }
    }

    writer.footer().map_err(|error| error.to_string())?;
    let summary = writer.summary();
    writer
        .into_inner()
        .finish()
        .map_err(|error| format!("写入导出文件失败：{error}"))?;
    let bytes = std::fs::metadata(temp).map(|meta| meta.len()).unwrap_or(0);
    Ok((summary, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::ObjectKind;

    fn path(catalog: Option<&str>, schema: Option<&str>, name: &str) -> rustdb_db_api::ObjectPath {
        rustdb_db_api::ObjectPath::new(Namespace::new(catalog, schema), name, ObjectKind::Table)
    }

    #[test]
    fn select_sql_uses_driver_quoting() {
        // PG / SQLite：双引号。
        assert_eq!(
            object_select_sql(
                DbKind::Postgres,
                &path(Some("app"), Some("public"), "orders"),
                500
            ),
            "SELECT *\nFROM \"app\".\"public\".\"orders\"\nLIMIT 500;"
        );
        // MySQL：反引号。
        assert_eq!(
            object_select_sql(DbKind::Mysql, &path(Some("shop"), None, "orders"), 100),
            "SELECT *\nFROM `shop`.`orders`\nLIMIT 100;"
        );
        // SQL Server：方括号。
        assert_eq!(
            object_select_sql(DbKind::Mssql, &path(Some("dw"), Some("dbo"), "orders"), 50),
            "SELECT *\nFROM [dw].[dbo].[orders]\nLIMIT 50;"
        );
    }

    #[test]
    fn sqlite_catalog_is_not_qualified() {
        // SQLite 的 `main` 是库名不是 schema，进 FROM 会直接语法错。
        assert_eq!(
            object_select_sql(DbKind::Sqlite, &path(Some("main"), None, "orders"), 200),
            "SELECT *\nFROM \"orders\"\nLIMIT 200;"
        );
    }

    #[test]
    fn rename_sql_follows_dialect_and_kind() {
        let table = path(Some("shop"), None, "orders");
        assert_eq!(
            Core::rename_sql(DbKind::Mysql, &table, "orders_v2").as_deref(),
            Some("RENAME TABLE `shop`.`orders` TO `orders_v2`;")
        );
        assert_eq!(
            Core::rename_sql(DbKind::Postgres, &table, "orders_v2").as_deref(),
            Some("ALTER TABLE \"shop\".\"orders\" RENAME TO \"orders_v2\";")
        );

        let view = rustdb_db_api::ObjectPath::new(
            Namespace::new(Some("shop"), None::<&str>),
            "v_orders",
            ObjectKind::View,
        );
        assert_eq!(
            Core::rename_sql(DbKind::Postgres, &view, "v2").as_deref(),
            Some("ALTER VIEW \"shop\".\"v_orders\" RENAME TO \"v2\";")
        );
        assert!(
            Core::rename_sql(DbKind::Sqlite, &view, "v2").is_none(),
            "SQLite 不能重命名视图"
        );
    }

    #[test]
    fn maintenance_sql_has_no_trailing_comment() {
        let table = path(Some("main"), None, "orders");
        let sql = Core::maintenance_sql(DbKind::Sqlite, &table).expect("sqlite 维护语句");
        assert_eq!(sql, "-- 维护 \"orders\"\nVACUUM;");
        assert!(
            sql.ends_with("VACUUM;"),
            "分号后不能跟注释，否则会被切成下一条语句"
        );

        assert_eq!(
            Core::maintenance_sql(DbKind::Mysql, &path(Some("shop"), None, "orders")).as_deref(),
            Some("OPTIMIZE TABLE `shop`.`orders`;")
        );
        assert_eq!(
            Core::maintenance_sql(
                DbKind::Postgres,
                &path(Some("shop"), Some("public"), "orders")
            )
            .as_deref(),
            Some("VACUUM ANALYZE \"shop\".\"public\".\"orders\";")
        );
    }

    #[test]
    fn new_object_template_uses_dialect_quotes() {
        let mysql = Core::new_object_template(DbKind::Mysql, ObjectKind::Table);
        assert!(mysql.contains("CREATE TABLE `new_table`"), "{mysql}");
        let pg = Core::new_object_template(DbKind::Postgres, ObjectKind::View);
        assert!(pg.contains("CREATE VIEW \"new_view\""), "{pg}");
    }

    #[test]
    fn ident_with_quote_is_escaped() {
        assert_eq!(
            object_select_sql(DbKind::Postgres, &path(None, Some("pub\"lic"), "a\"b"), 10),
            "SELECT *\nFROM \"pub\"\"lic\".\"a\"\"b\"\nLIMIT 10;"
        );
    }

    fn core_with(store: crate::profiles::ProfileStore) -> Arc<Core> {
        Core::with_config(
            DriverRegistry::new(),
            crate::history::HistoryStore::new(crate::history::DEFAULT_HISTORY_LIMIT),
            store,
        )
    }

    fn profile(id: &str) -> ConnectionProfile {
        let mut profile = ConnectionProfile::demo_sqlite();
        profile.id = id.into();
        profile.name = id.into();
        profile
    }

    #[test]
    fn profiles_can_be_added_updated_and_removed() {
        let core = core_with(crate::profiles::ProfileStore::in_memory());
        core.add_profile(profile("a")).expect("新增");
        assert!(core.add_profile(profile("a")).is_err(), "重复 id 应报错");
        assert_eq!(core.profiles().len(), 2, "示例 + 新增");

        let mut updated = profile("a");
        updated.name = "改名".into();
        core.update_profile(updated).expect("修改");
        let found = core
            .profiles()
            .into_iter()
            .find(|item| item.id == "a")
            .expect("应仍存在");
        assert_eq!(found.name, "改名");

        core.remove_profile("a").expect("删除");
        assert!(core.profiles().iter().all(|item| item.id != "a"));
        assert!(core.remove_profile("a").is_err(), "重复删除应报错");
    }

    #[test]
    fn profiles_survive_a_restart() {
        let path =
            std::env::temp_dir().join(format!("rustdb-core-profiles-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let core = core_with(crate::profiles::ProfileStore::with_path(path.clone()));
        core.add_profile(profile("persisted")).expect("新增");

        // 新进程读同一份文件。
        let reopened = core_with(crate::profiles::ProfileStore::with_path(path.clone()));
        assert!(
            reopened
                .profiles()
                .iter()
                .any(|item| item.id == "persisted"),
            "连接配置应从磁盘恢复"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn production_flag_is_persisted() {
        let path = std::env::temp_dir().join(format!(
            "rustdb-core-production-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let core = core_with(crate::profiles::ProfileStore::with_path(path.clone()));
        core.add_profile(profile("prod")).expect("新增");
        core.set_profile_production("prod", true).expect("标记");

        let reopened = core_with(crate::profiles::ProfileStore::with_path(path.clone()));
        let found = reopened
            .profiles()
            .into_iter()
            .find(|item| item.id == "prod")
            .expect("应仍存在");
        assert!(found.production, "生产标记应落盘");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn confirming_an_unknown_host_key_is_rejected() {
        let core = core_with(crate::profiles::ProfileStore::in_memory());
        assert!(core.confirm_tunnel_host_key("demo-sqlite", true).is_err());
    }

    #[test]
    fn route_summary_names_proxy_position() {
        let spec = rustdb_db_api::TunnelSpec {
            hops: vec![rustdb_db_api::TunnelHopSpec {
                host: "bastion".into(),
                port: 22,
                user: "ops".into(),
                auth: rustdb_db_api::TunnelAuthSpec::single(rustdb_db_api::TunnelAuthMethod::None),
                host_keys: rustdb_db_api::HostKeySpec::KnownHostsDefault,
            }],
        };
        let mut proxy = rustdb_db_api::ProxySpec {
            host: "10.0.0.2".into(),
            port: 1080,
            ..Default::default()
        };

        let before = route_summary(&spec, Some(&proxy), "db:5432");
        assert!(before.starts_with("代理 SOCKS5 10.0.0.2:1080"), "{before}");
        assert!(before.contains("ops@bastion"), "{before}");

        proxy.order = rustdb_db_api::ProxyOrder::AfterTunnel;
        let after = route_summary(&spec, Some(&proxy), "db:5432");
        assert!(after.starts_with("1. ops@bastion"), "{after}");
        assert!(after.contains("→ 代理 SOCKS5 10.0.0.2:1080"), "{after}");

        let plain = route_summary(&spec, None, "db:5432");
        assert!(plain.ends_with("→ 目标 db:5432"), "{plain}");
        assert!(!plain.contains("代理"), "{plain}");
    }

    #[test]
    fn dump_file_stem_keeps_safe_characters() {
        assert_eq!(
            dump_file_stem(&Namespace::new(Some("shop"), None::<&str>)),
            "shop"
        );
        // 库.模式 里的点会变成下划线，避免和扩展名混淆。
        assert_eq!(
            dump_file_stem(&Namespace::new(Some("app"), Some("public"))),
            "app_public"
        );
        assert_eq!(
            dump_file_stem(&Namespace::new(None::<&str>, None::<&str>)),
            "default"
        );
    }
}
