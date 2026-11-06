//! 连接配置（持久化时**不含密钥**，密钥只存 keyring 引用键）。

use rustdb_db_api::{ConnectOptions, DbKind, Namespace, ProxySpec, TunnelSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    /// 连接树中的文件夹名；`None` 或空串表示「未分组」。
    #[serde(default)]
    pub folder: Option<String>,
    pub driver: DbKind,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// SQLite: 文件路径 / `:memory:`；Oracle: SERVICE_NAME。
    pub database: Option<String>,
    pub username: Option<String>,
    /// keyring 条目的键；口令本身绝不落盘。
    pub secret_ref: Option<String>,
    /// **仅开发/本地验证**：内存中的临时口令（`#[serde(skip)]`，永不写盘）。
    #[serde(skip)]
    pub dev_secret: Option<String>,
    pub ssl_mode: Option<String>,
    pub params: BTreeMap<String, String>,
    pub read_only: bool,
    /// F-89：生产连接；写操作需临时解锁，AI 永不自动执行。
    #[serde(default)]
    pub production: bool,
    /// F-91：SSH 隧道（跳板按顺序建链）；`None` 表示直连。
    #[serde(default)]
    pub tunnel: Option<TunnelSpec>,
    /// F-91a：可选代理（SOCKS5 / HTTP CONNECT），`order` 决定与隧道的先后。
    #[serde(default)]
    pub proxy: Option<ProxySpec>,
    /// F-91a：本地转发是否对局域网开放；默认关闭。
    #[serde(default)]
    pub forward_lan: bool,
    pub connect_timeout_ms: u64,
    pub color: Option<u32>,
    pub last_used_at: Option<i64>,
}

impl ConnectionProfile {
    pub fn demo_sqlite() -> Self {
        Self {
            id: "demo-sqlite".into(),
            name: "本地示例".into(),
            folder: Some("示例".into()),
            driver: DbKind::Sqlite,
            host: None,
            port: None,
            database: Some(":memory:".into()),
            username: None,
            secret_ref: None,
            dev_secret: None,
            ssl_mode: None,
            params: BTreeMap::new(),
            read_only: false,
            production: false,
            tunnel: None,
            proxy: None,
            forward_lan: false,
            connect_timeout_ms: 10_000,
            color: Some(0x58A6FF),
            last_used_at: None,
        }
    }

    /// 构造驱动连接参数；`secret_ref` 在这里解析成真正的口令。
    ///
    /// 解析失败必须显式上抛：静默当成空口令去连，只会得到一条难以理解的
    /// 「认证失败」，用户无法判断是口令错了还是根本没读到口令。
    pub fn to_connect_options(&self) -> Result<ConnectOptions, String> {
        let secret = match (&self.dev_secret, &self.secret_ref) {
            // `dev_secret` 仅供本地 Docker 验证，优先级高于引用键。
            (Some(secret), _) => Some(secret.clone()),
            (None, Some(reference)) => Some(crate::secrets::resolve(reference)?),
            (None, None) => None,
        };
        Ok(ConnectOptions {
            host: self.host.clone(),
            port: self.port,
            database: self.database.clone(),
            username: self.username.clone(),
            secret,
            ssl_mode: self.ssl_mode.clone(),
            params: self.params.clone(),
            connect_timeout_ms: self.connect_timeout_ms,
            read_only: self.read_only,
        })
    }

    pub fn endpoint_label(&self) -> String {
        match self.driver {
            DbKind::Sqlite => self
                .database
                .clone()
                .unwrap_or_else(|| "(未选择文件)".into()),
            _ => format!(
                "{}:{}",
                self.host.clone().unwrap_or_else(|| "localhost".into()),
                self.port.or(self.driver.default_port()).unwrap_or(0)
            ),
        }
    }

    /// 连接树右侧的短标签，例如 `PG` / `MySQL` / `SQLite`。
    pub fn driver_badge(&self) -> &'static str {
        match self.driver {
            DbKind::Postgres => "PG",
            DbKind::Mysql => "MySQL",
            DbKind::Mariadb => "MariaDB",
            DbKind::Mssql => "MSSQL",
            DbKind::Oracle => "Oracle",
            DbKind::Sqlite => "SQLite",
        }
    }

    /// 文件夹名（裁剪空白），空串视为未分组。
    pub fn folder_name(&self) -> Option<&str> {
        self.folder
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// F-75：该连接默认写入的命名空间（跨库传输目标用）。
    ///
    /// SQLite 固定 `main`；PostgreSQL 用配置库的 `public`；SQL Server 用 `dbo`；
    /// Oracle 以登录用户为 schema（catalog 在 Oracle 只作 UI 概念，不参与切换）；
    /// MySQL / MariaDB 直接用配置里的 database。
    pub fn default_namespace(&self) -> Namespace {
        match self.driver {
            DbKind::Sqlite => Namespace::new(Some("main"), None::<String>),
            DbKind::Postgres => Namespace::new(self.database.clone(), Some("public")),
            DbKind::Mssql => Namespace::new(self.database.clone(), Some("dbo")),
            DbKind::Oracle => Namespace::new(None::<String>, self.username.clone()),
            DbKind::Mysql | DbKind::Mariadb => {
                Namespace::new(self.database.clone(), None::<String>)
            }
        }
    }
}

/// 连接树的一个文件夹节点；`name = None` 表示「未分组」。
#[derive(Debug, Clone)]
pub struct ConnectionFolder {
    pub name: Option<String>,
    pub profiles: Vec<ConnectionProfile>,
}

impl ConnectionFolder {
    /// 折叠状态的稳定键（不随显示语言变化）。
    pub fn key(&self) -> &str {
        self.name.as_deref().unwrap_or("\u{0}ungrouped")
    }

    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("未分组")
    }
}

/// 按文件夹分组：文件夹按名称排序，未分组固定排最后；组内保持注册顺序。
pub fn group_by_folder(profiles: &[ConnectionProfile]) -> Vec<ConnectionFolder> {
    let mut named: BTreeMap<&str, Vec<ConnectionProfile>> = BTreeMap::new();
    let mut ungrouped = Vec::new();
    for profile in profiles {
        match profile.folder_name() {
            Some(name) => named.entry(name).or_default().push(profile.clone()),
            None => ungrouped.push(profile.clone()),
        }
    }
    let mut groups: Vec<ConnectionFolder> = named
        .into_iter()
        .map(|(name, profiles)| ConnectionFolder {
            name: Some(name.to_string()),
            profiles,
        })
        .collect();
    if !ungrouped.is_empty() {
        groups.push(ConnectionFolder {
            name: None,
            profiles: ungrouped,
        });
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, folder: Option<&str>) -> ConnectionProfile {
        let mut p = ConnectionProfile::demo_sqlite();
        p.id = id.into();
        p.name = id.into();
        p.folder = folder.map(str::to_string);
        p
    }

    #[test]
    fn folders_are_sorted_and_ungrouped_last() {
        let list = vec![
            profile("a", Some("生产")),
            profile("b", None),
            profile("c", Some("本地")),
            profile("d", Some("  ")),
            profile("e", Some("本地")),
        ];
        let groups = group_by_folder(&list);
        let labels: Vec<_> = groups.iter().map(|g| g.label()).collect();
        assert_eq!(labels, vec!["本地", "生产", "未分组"]);
        assert_eq!(groups[0].profiles.len(), 2);
        assert_eq!(groups[2].profiles.len(), 2);
        assert_eq!(groups[2].key(), "\u{0}ungrouped");
    }

    #[test]
    fn empty_input_has_no_folders() {
        assert!(group_by_folder(&[]).is_empty());
    }

    #[test]
    fn default_namespace_follows_engine_layout() {
        let mut sqlite = ConnectionProfile::demo_sqlite();
        assert_eq!(sqlite.default_namespace().display_label(), "main");

        let mut pg = ConnectionProfile::demo_sqlite();
        pg.driver = DbKind::Postgres;
        pg.database = Some("app".into());
        assert_eq!(pg.default_namespace().display_label(), "app.public");

        let mut mssql = ConnectionProfile::demo_sqlite();
        mssql.driver = DbKind::Mssql;
        mssql.database = Some("dw".into());
        assert_eq!(mssql.default_namespace().display_label(), "dw.dbo");

        let mut oracle = ConnectionProfile::demo_sqlite();
        oracle.driver = DbKind::Oracle;
        oracle.database = Some("ORCL".into());
        oracle.username = Some("shop".into());
        let ns = oracle.default_namespace();
        assert_eq!(ns.display_label(), "shop");
        assert!(ns.catalog.is_none());

        let mut mysql = ConnectionProfile::demo_sqlite();
        mysql.driver = DbKind::Mysql;
        mysql.database = Some("shop".into());
        assert_eq!(mysql.default_namespace().display_label(), "shop");

        sqlite.database = Some("x.db".into());
        assert_eq!(sqlite.default_namespace().display_label(), "main");
    }
}
