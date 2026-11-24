use crate::capabilities::Capabilities;
use crate::error::DriverError;
use crate::session::{ServerInfo, Session};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbKind {
    Mysql,
    Mariadb,
    Postgres,
    Sqlite,
    Mssql,
    Oracle,
}

impl DbKind {
    pub const ALL: [DbKind; 6] = [
        DbKind::Mysql,
        DbKind::Mariadb,
        DbKind::Postgres,
        DbKind::Sqlite,
        DbKind::Mssql,
        DbKind::Oracle,
    ];

    /// 持久化用的稳定键：不随界面语言变化。
    pub fn key(self) -> &'static str {
        match self {
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
            Self::Mssql => "mssql",
            Self::Oracle => "oracle",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Mysql => "MySQL",
            Self::Mariadb => "MariaDB",
            Self::Postgres => "PostgreSQL",
            Self::Sqlite => "SQLite",
            Self::Mssql => "SQL Server",
            Self::Oracle => "Oracle",
        }
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            Self::Mysql | Self::Mariadb => Some(3306),
            Self::Postgres => Some(5432),
            Self::Sqlite => None,
            Self::Mssql => Some(1433),
            Self::Oracle => Some(1521),
        }
    }
}

/// 由 core 组装、交给驱动的纯连接参数（不含分组 / 颜色等 UI 概念）。
#[derive(Clone)]
pub struct ConnectOptions {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub secret: Option<String>,
    pub ssl_mode: Option<String>,
    pub params: BTreeMap<String, String>,
    pub connect_timeout_ms: u64,
    pub read_only: bool,
}

impl std::fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("username", &self.username)
            .field("secret", &self.secret.as_ref().map(|_| "Secret(***)"))
            .field("ssl_mode", &self.ssl_mode)
            .field("read_only", &self.read_only)
            .finish()
    }
}

#[async_trait::async_trait]
pub trait DriverFactory: Send + Sync {
    fn kind(&self) -> DbKind;
    fn display_name(&self) -> &'static str;
    fn default_port(&self) -> Option<u16> {
        self.kind().default_port()
    }
    /// 静态能力；连接后可用服务端版本进一步修正。
    fn baseline_capabilities(&self) -> Capabilities;
    async fn connect(&self, opts: &ConnectOptions) -> Result<Box<dyn Session>, DriverError>;
}

/// 驱动注册表：组合根负责把所有驱动注册进来，core 只按 `DbKind` 取。
#[derive(Default)]
pub struct DriverRegistry {
    factories: BTreeMap<DbKind, Arc<dyn DriverFactory>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F: DriverFactory + 'static>(&mut self, factory: F) {
        self.factories.insert(factory.kind(), Arc::new(factory));
    }

    pub fn register_arc(&mut self, factory: Arc<dyn DriverFactory>) {
        self.factories.insert(factory.kind(), factory);
    }

    pub fn get(&self, kind: DbKind) -> Option<Arc<dyn DriverFactory>> {
        self.factories.get(&kind).cloned()
    }

    pub fn kinds(&self) -> impl Iterator<Item = DbKind> + '_ {
        self.factories.keys().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

/// 驱动返回的会话元信息（`ServerInfo` 的别名入口，便于驱动 crate 少 import 一个模块）。
pub type DriverServerInfo = ServerInfo;
