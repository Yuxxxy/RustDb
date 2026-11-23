//! MySQL / MariaDB 驱动：会话独占物理连接 + Text Protocol 行读取 +
//! information_schema 元数据 + KILL QUERY 取消。

use async_trait::async_trait;
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, OptsBuilder, Pool, PoolOpts, Row as MyRow, SslOpts, Value as MyValue};
use parking_lot::Mutex;
use rustdb_db_api::{
    dialect::quote_backtick, CancelStyle, Capabilities, ColumnInfo, ColumnMeta, DbKind, Dialect,
    DriverError, DriverErrorKind, DriverFactory, ExecuteRequest, ExplainStyle, ForeignKeyInfo,
    IndexInfo, IsolationLevel, JsonStyle, LogicalType, MetadataProvider, Namespace, ObjectKind,
    ObjectKindMask, ObjectPath, ObjectSummary, QueryHandle, QueryId, Row, ServerInfo, Session,
    SessionId, TableDetail, TriggerInfo, TxId, TxOptions, UpsertStyle, Value,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

/// 按 `ssl_mode` 生成 TLS 参数；`None` 表示不启用 TLS。
///
/// - `disable`：明文
/// - `prefer`：先试 TLS，失败回退明文（见 `connect_with`）
/// - `require`：强制加密，但不校验证书（libmysql 语义）
/// - `verify-ca`：校验证书链，跳过主机名
/// - `verify-full`：校验证书链 + 主机名（默认）
fn mysql_ssl_opts(
    mode: &str,
    params: &std::collections::BTreeMap<String, String>,
) -> Result<Option<SslOpts>, DriverError> {
    let mut ssl = match mode {
        "disable" | "disabled" | "off" | "none" => return Ok(None),
        "prefer" | "require" | "required" | "verify-ca" | "verify-full" => SslOpts::default(),
        other => {
            return Err(DriverError::new(
                DriverErrorKind::Connection,
                format!(
                    "未知的 ssl_mode：{other}（可选 disable/prefer/require/verify-ca/verify-full）"
                ),
            ))
        }
    };
    match mode {
        // libmysql 的 PREFERRED 只要求“能用 TLS 就用”，同样不校验证书；
        // 需要校验时显式写 verify-ca / verify-full。
        "prefer" | "require" | "required" => {
            ssl = ssl
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true);
        }
        "verify-ca" => {
            ssl = ssl.with_danger_skip_domain_validation(true);
        }
        _ => {}
    }
    // 自定义根证书：连接参数 `ssl_root_cert` / `ssl_ca` 指向 PEM 文件。
    if let Some(path) = params.get("ssl_root_cert").or_else(|| params.get("ssl_ca")) {
        // `PathOrBuf` 没有公开导出，用 `From<PathBuf>` 让类型推断落到它上面。
        ssl = ssl.with_root_certs(vec![std::path::PathBuf::from(path).into()]);
    }
    Ok(Some(ssl))
}

fn build_opts(opts: &rustdb_db_api::ConnectOptions, ssl: Option<SslOpts>) -> OptsBuilder {
    let mut builder = OptsBuilder::default()
        .ip_or_hostname(opts.host.clone().unwrap_or_else(|| "127.0.0.1".into()))
        .tcp_port(opts.port.unwrap_or(3306))
        .user(Some(opts.username.clone().unwrap_or_else(|| "root".into())))
        .pass(opts.secret.clone())
        .prefer_socket(Some(false))
        .stmt_cache_size(Some(0))
        // 空闲连接 20s 回收：mysql_async 默认 `inactive_connection_ttl = 0`（永不回收），
        // 会话空闲超过服务端 `wait_timeout` 后池里会留下死 socket，下一次查询直接报 IO 错误。
        // 20s 远小于任何合理的 wait_timeout，代价只是重连一次。
        .pool_opts(PoolOpts::default().with_inactive_connection_ttl(Duration::from_secs(20)))
        // 连接建立即声明 utf8mb4，否则中文会按 latin1 解码成乱码。
        .init(vec!["SET NAMES utf8mb4"]);
    if let Some(db) = &opts.database {
        builder = builder.db_name(Some(db.clone()));
    }
    if let Some(ssl) = ssl {
        builder = builder.ssl_opts(ssl);
    }
    builder
}

pub struct MySqlFactory {
    kind: DbKind,
    caps: Capabilities,
}

impl MySqlFactory {
    pub fn new() -> Self {
        Self::with_kind(DbKind::Mysql)
    }
}

impl Default for MySqlFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl MySqlFactory {
    fn with_kind(kind: DbKind) -> Self {
        let mut caps = Capabilities::minimal();
        caps.multi_statement = false;
        caps.cte = true;
        caps.window_functions = true;
        caps.upsert = UpsertStyle::OnDuplicateKey;
        caps.cancel = CancelStyle::KillQuery;
        caps.explain = ExplainStyle::Explain;
        caps.json = JsonStyle::Native;
        caps.max_ident_len = 64;
        caps.read_only_txn = true;
        caps.streaming_rows = false;
        caps.dml_preview = true; // F-63：核心改写器 + 本方言标识符引用
        caps.routine_source_edit = true; // F-82：视图 / 过程 / 函数 / 触发器源码
        caps.routine_dependencies = true; // F-83：information_schema 使用关系
        caps.foreign_key_pivot = true; // F-73：information_schema 外键可用于跳转
                                       // MariaDB 10.5+ 支持 INSERT … RETURNING
        caps.returning = kind == DbKind::Mariadb;
        Self { kind, caps }
    }
}

pub struct MariaDbFactory;

impl MariaDbFactory {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MariaDbFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DriverFactory for MariaDbFactory {
    fn kind(&self) -> DbKind {
        DbKind::Mariadb
    }

    fn display_name(&self) -> &'static str {
        "MariaDB"
    }

    fn baseline_capabilities(&self) -> Capabilities {
        MySqlFactory::with_kind(DbKind::Mariadb).caps
    }

    async fn connect(
        &self,
        opts: &rustdb_db_api::ConnectOptions,
    ) -> Result<Box<dyn Session>, DriverError> {
        connect_with(DbKind::Mariadb, opts).await
    }
}

#[async_trait]
impl DriverFactory for MySqlFactory {
    fn kind(&self) -> DbKind {
        self.kind
    }

    fn display_name(&self) -> &'static str {
        "MySQL"
    }

    fn baseline_capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    async fn connect(
        &self,
        opts: &rustdb_db_api::ConnectOptions,
    ) -> Result<Box<dyn Session>, DriverError> {
        connect_with(self.kind, opts).await
    }
}

async fn connect_with(
    kind: DbKind,
    opts: &rustdb_db_api::ConnectOptions,
) -> Result<Box<dyn Session>, DriverError> {
    let ssl_mode = opts
        .ssl_mode
        .as_deref()
        .unwrap_or("prefer")
        .to_ascii_lowercase();
    let ssl = mysql_ssl_opts(&ssl_mode, &opts.params)?;

    // prefer：先试 TLS，失败回退明文（与 libmysql / libpq 语义一致）。
    let mut pool: Option<Arc<Pool>> = None;
    let mut conn = None;
    let mut plain_fallback = false;
    if ssl_mode == "prefer" {
        if let Some(ssl) = ssl.clone() {
            let candidate = Arc::new(Pool::new(build_opts(opts, Some(ssl))));
            match candidate.get_conn().await {
                Ok(ready) => {
                    pool = Some(candidate);
                    conn = Some(ready);
                }
                // 只有连接级失败（TLS 不可用 / IO）才回退明文；认证失败直接上报，
                // 避免同一次连接把口令验证两遍。
                Err(error) if error.is_fatal() => {
                    log::warn!("MySQL TLS 连接失败（sslmode=prefer），回退明文：{error}");
                    plain_fallback = true;
                }
                Err(error) => return Err(map_mysql_error(error)),
            }
        }
    }
    if pool.is_none() {
        // 回退必须真的去掉 SslOpts：否则只是用同一套 TLS 参数重连一次，
        // 等于把 prefer 悄悄变成 require（P2 live 验证发现）。
        let effective_ssl = if plain_fallback { None } else { ssl };
        let candidate = Arc::new(Pool::new(build_opts(opts, effective_ssl)));
        let ready = candidate.get_conn().await.map_err(map_mysql_error)?;
        pool = Some(candidate);
        conn = Some(ready);
    }
    let pool = pool.expect("连接池已建立");
    let mut conn = conn.expect("物理连接已建立");
    let row: MyRow = conn
        .query_first("SELECT VERSION(), DATABASE(), CURRENT_USER()")
        .await
        .map_err(map_mysql_error)?
        .ok_or_else(|| DriverError::protocol("连接成功但读不到版本信息"))?;
    let version: String = row.get(0).unwrap_or_default();
    let current_db: Option<String> = row.get(1);
    let user: String = row.get(2).unwrap_or_default();
    // 这条连接就是会话的物理连接：事务、临时表、会话变量和 USE 都绑定在它上面。
    // 连接池只留给元数据 / 取消等旁路查询，不再让每条语句随机换连接。
    let conn_id = conn.id();

    let flavor = if version.to_ascii_lowercase().contains("mariadb") {
        Some("MariaDB".to_string())
    } else {
        Some("MySQL".to_string())
    };
    let mut server = ServerInfo::new(format!("{version}"));
    server.flavor = flavor;
    server.current_db = current_db;
    server.charset = Some("utf8mb4".into());
    server.max_ident_len = 64;
    server.read_only = opts.read_only;

    let caps = MySqlFactory::with_kind(if kind == DbKind::Mariadb {
        DbKind::Mariadb
    } else {
        DbKind::Mysql
    })
    .caps;

    Ok(Box::new(MySqlSession {
        id: SessionId::new(),
        pool: pool.clone(),
        conn: AsyncMutex::new(conn),
        server,
        caps,
        dialect: Arc::new(MySqlDialect { kind }),
        meta: MySqlMetadata { pool },
        active_conn_id: AtomicU32::new(conn_id),
        active_query: Mutex::new(None),
        user,
    }))
}

struct MySqlDialect {
    kind: DbKind,
}

impl Dialect for MySqlDialect {
    fn kind(&self) -> DbKind {
        self.kind
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_backtick(ident)
    }

    fn placeholder(&self, _index: usize) -> String {
        "?".into()
    }

    fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
        format!("{sql} LIMIT {limit} OFFSET {offset}")
    }

    fn order_clause(&self, column: &str, descending: bool, nulls_last: bool) -> String {
        let direction = if descending { "DESC" } else { "ASC" };
        // MySQL 没有 NULLS LAST/FIRST；用布尔表达式显式控制 NULL 的位置。
        let null_direction = if nulls_last { "ASC" } else { "DESC" };
        let quoted = self.quote_ident(column);
        format!("{quoted} IS NULL {null_direction}, {quoted} {direction}")
    }

    fn explain_sql(&self, sql: &str, analyze: bool) -> String {
        if analyze {
            format!("EXPLAIN ANALYZE {sql}")
        } else {
            format!("EXPLAIN {sql}")
        }
    }
}

struct MySqlSession {
    id: SessionId,
    /// 会话独占的物理连接；事务、临时表和会话变量都依赖它。
    conn: AsyncMutex<Conn>,
    /// 元数据 / KILL QUERY 使用独立连接池，不参与会话事务。
    pool: Arc<Pool>,
    server: ServerInfo,
    caps: Capabilities,
    dialect: Arc<dyn Dialect>,
    meta: MySqlMetadata,
    /// 正在执行查询的连接 id（KILL QUERY 用）。
    active_conn_id: AtomicU32,
    /// 仅用于校验取消请求是否仍指向当前查询。
    active_query: Mutex<Option<QueryId>>,
    #[allow(dead_code)]
    user: String,
}

#[async_trait]
impl Session for MySqlSession {
    fn id(&self) -> SessionId {
        self.id.clone()
    }

    fn server(&self) -> &ServerInfo {
        &self.server
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn dialect(&self) -> Arc<dyn Dialect> {
        self.dialect.clone()
    }

    async fn ping(&self) -> Result<Duration, DriverError> {
        let started = Instant::now();
        let mut conn = self.conn.lock().await;
        conn.query_drop("SELECT 1").await.map_err(map_mysql_error)?;
        Ok(started.elapsed())
    }

    async fn execute(&self, req: ExecuteRequest) -> Result<QueryHandle, DriverError> {
        let query_id = req.query_id.clone().unwrap_or_else(QueryId::new);
        let max_rows = req.fetch.max_rows.max(1) as usize;
        let mut conn = self.conn.lock().await;
        self.active_conn_id.store(conn.id(), Ordering::Relaxed);
        *self.active_query.lock() = Some(query_id.clone());

        let outcome = async {
            let mut result = conn.query_iter(&req.sql).await.map_err(map_mysql_error)?;
            // 列元数据按 `ColumnType` 还原：表头类型名、类型感知编辑、数值排序都靠它。
            // 之前这里把每一列都硬编码成 `text`，导致 MySQL 上所有列都被当成字符串。
            let column_meta: Vec<ColumnMeta> = result
                .columns()
                .map(|cols| cols.iter().map(column_meta_of).collect())
                .unwrap_or_default();

            let mut rows: Vec<Row> = Vec::new();
            let mut truncated = false;
            while let Some(row) = result.next().await.map_err(map_mysql_error)? {
                if rows.len() >= max_rows {
                    // 真早停：不要把剩余行全部读回再丢弃。mysql_async 的
                    // QueryResult drop 会 drain 协议帧，连接可继续复用
                    // （由 live 用例 max_rows_truncation_keeps_connection_usable 验证）。
                    truncated = true;
                    break;
                }
                let mut values = Vec::with_capacity(row.len());
                for i in 0..row.len() {
                    let logical = column_meta.get(i).map(|meta| &meta.logical_type);
                    values.push(convert_value(row.as_ref(i), logical));
                }
                rows.push(values);
            }
            drop(result);
            let affected = conn.affected_rows();
            Ok::<_, DriverError>((column_meta, rows, affected, truncated))
        }
        .await;

        *self.active_query.lock() = None;
        let (column_meta, rows, affected, truncated) = outcome?;

        let has_columns = !column_meta.is_empty();
        let column_meta: Arc<[ColumnMeta]> = column_meta.into();

        let mut warnings = Vec::new();
        if truncated {
            warnings.push(rustdb_db_api::session::DbWarning {
                level: rustdb_db_api::session::WarningLevel::Warning,
                message: format!("结果已截断：仅返回前 {max_rows} 行"),
            });
        }

        Ok(QueryHandle {
            query_id,
            columns: column_meta,
            stream: rustdb_db_api::chunked_row_stream(rows, &req.fetch),
            affected: if has_columns { None } else { Some(affected) },
            warnings,
        })
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<(), DriverError> {
        let active = self.active_query.lock().clone();
        if active.as_ref() != Some(query_id) {
            return Err(DriverError::internal("该查询已结束，无需取消"));
        }
        let conn_id = self.active_conn_id.load(Ordering::Relaxed);
        if conn_id == 0 {
            return Err(DriverError::internal("当前没有正在执行的查询"));
        }
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        conn.query_drop(format!("KILL QUERY {conn_id}"))
            .await
            .map_err(map_mysql_error)
    }

    async fn begin(&self, opts: TxOptions) -> Result<TxId, DriverError> {
        let mut conn = self.conn.lock().await;
        if let Some(level) = opts.isolation {
            let level = match level {
                IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
                IsolationLevel::ReadCommitted => "READ COMMITTED",
                IsolationLevel::RepeatableRead => "REPEATABLE READ",
                IsolationLevel::Serializable => "SERIALIZABLE",
            };
            conn.query_drop(format!("SET TRANSACTION ISOLATION LEVEL {level}"))
                .await
                .map_err(map_mysql_error)?;
        }
        let sql = if opts.read_only {
            "START TRANSACTION READ ONLY"
        } else {
            "START TRANSACTION"
        };
        conn.query_drop(sql).await.map_err(map_mysql_error)?;
        Ok(TxId::new())
    }

    async fn commit(&self, _tx: TxId) -> Result<(), DriverError> {
        let mut conn = self.conn.lock().await;
        conn.query_drop("COMMIT").await.map_err(map_mysql_error)
    }

    async fn rollback(&self, _tx: TxId) -> Result<(), DriverError> {
        let mut conn = self.conn.lock().await;
        conn.query_drop("ROLLBACK").await.map_err(map_mysql_error)
    }

    async fn use_namespace(&self, ns: &Namespace) -> Result<(), DriverError> {
        let Some(catalog) = &ns.catalog else {
            return Ok(());
        };
        let mut conn = self.conn.lock().await;
        let sql = format!("USE {}", quote_backtick(catalog));
        conn.query_drop(sql).await.map_err(map_mysql_error)?;
        Ok(())
    }

    fn metadata(&self) -> &dyn MetadataProvider {
        &self.meta
    }
}

/// MySQL 列类型 → （逻辑类型，原生类型名）。
///
/// 这是 MySQL 侧唯一一处「列类型事实来源」：表头的类型标签、类型感知编辑、
/// 数值排序都从 `logical_type` 出发。之前整列硬编码成 `text`，后果是
/// `id` 列升序排成 `1, 10, 11, …, 2, 20`（字典序）。
///
/// `binary` 指列的字符集是 `binary`（63）：MySQL 的 `TEXT` 列同样报
/// `MYSQL_TYPE_BLOB`，只能靠字符集区分「文本大字段」和「二进制大字段」。
fn map_column_type(ty: ColumnType, unsigned: bool, binary: bool) -> (LogicalType, &'static str) {
    use ColumnType::*;
    match ty {
        MYSQL_TYPE_TINY => (LogicalType::Int { bits: 8, unsigned }, "TINYINT"),
        MYSQL_TYPE_SHORT => (LogicalType::Int { bits: 16, unsigned }, "SMALLINT"),
        MYSQL_TYPE_INT24 => (LogicalType::Int { bits: 24, unsigned }, "MEDIUMINT"),
        MYSQL_TYPE_LONG => (LogicalType::Int { bits: 32, unsigned }, "INT"),
        MYSQL_TYPE_LONGLONG => (LogicalType::Int { bits: 64, unsigned }, "BIGINT"),
        MYSQL_TYPE_YEAR => (
            LogicalType::Int {
                bits: 16,
                unsigned: false,
            },
            "YEAR",
        ),
        MYSQL_TYPE_FLOAT => (LogicalType::Float { bits: 32 }, "FLOAT"),
        MYSQL_TYPE_DOUBLE => (LogicalType::Float { bits: 64 }, "DOUBLE"),
        // 定点数留住精度：显示/排序都按十进制字符串处理，绝不转 f64。
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => (
            LogicalType::Decimal {
                precision: None,
                scale: None,
            },
            "DECIMAL",
        ),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => (LogicalType::Date, "DATE"),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => (LogicalType::Time, "TIME"),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => (LogicalType::Timestamp, "DATETIME"),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => (LogicalType::Timestamp, "TIMESTAMP"),
        MYSQL_TYPE_JSON => (LogicalType::Json, "JSON"),
        MYSQL_TYPE_BIT => (LogicalType::Bytes, "BIT"),
        MYSQL_TYPE_GEOMETRY => (LogicalType::Bytes, "GEOMETRY"),
        MYSQL_TYPE_VECTOR => (LogicalType::Bytes, "VECTOR"),
        // TEXT 与 BLOB 共用 MYSQL_TYPE_BLOB，靠字符集分流。
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                (LogicalType::Bytes, "BLOB")
            } else {
                (LogicalType::Text { max_len: None }, "TEXT")
            }
        }
        MYSQL_TYPE_STRING | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            if binary {
                (LogicalType::Bytes, "VARBINARY")
            } else {
                (LogicalType::Text { max_len: None }, "VARCHAR")
            }
        }
        MYSQL_TYPE_ENUM => (LogicalType::Text { max_len: None }, "ENUM"),
        MYSQL_TYPE_SET => (LogicalType::Text { max_len: None }, "SET"),
        MYSQL_TYPE_NULL => (LogicalType::Unknown("NULL".into()), "NULL"),
        MYSQL_TYPE_TYPED_ARRAY | MYSQL_TYPE_UNKNOWN => {
            (LogicalType::Unknown("text".into()), "TEXT")
        }
    }
}

/// 字符集 63 是 MySQL 的 `binary`：用它区分「文本」与「字节」。
const MYSQL_BINARY_CHARSET: u16 = 63;

fn column_meta_of(column: &mysql_async::Column) -> ColumnMeta {
    let flags = column.flags();
    let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
    let binary = column.character_set() == MYSQL_BINARY_CHARSET;
    let (logical_type, native) = map_column_type(column.column_type(), unsigned, binary);
    ColumnMeta {
        name: Arc::from(column.name_str().as_ref()),
        logical_type,
        native_type: native.into(),
        // 计算结果集里 MySQL 不一定回填 NOT_NULL，缺省按可空处理（宁可多让改一次）。
        nullable: !flags.contains(ColumnFlags::NOT_NULL_FLAG),
    }
}

/// 把协议层的一个值转成 `Value`。
///
/// `query_iter` 走的是**文本协议**：整数、定点数、日期在线上都是字节串。
/// 光看字节没法区分 `1`（INT）和 `"1"`（VARCHAR），所以必须借列的逻辑类型还原，
/// 否则排序退化成字典序、编辑器也做不了类型校验。
fn convert_value(value: Option<&MyValue>, logical: Option<&LogicalType>) -> Value {
    /// 文本协议下的字节串按列类型还原；还原失败一律退回文本，不丢数据。
    fn coerce(text: &str, logical: Option<&LogicalType>) -> Value {
        let Some(logical) = logical else {
            return Value::Text(Arc::from(text));
        };
        match logical {
            LogicalType::Bool => match text {
                "0" => Value::Bool(false),
                "1" => Value::Bool(true),
                _ => Value::Text(Arc::from(text)),
            },
            LogicalType::Int { unsigned, .. } => {
                if *unsigned {
                    text.parse::<u64>()
                        .map(Value::UInt)
                        .unwrap_or_else(|_| Value::Text(Arc::from(text)))
                } else {
                    text.parse::<i64>()
                        .map(Value::Int)
                        .unwrap_or_else(|_| Value::Text(Arc::from(text)))
                }
            }
            LogicalType::Float { .. } => text
                .parse::<f64>()
                .map(Value::Float)
                .unwrap_or_else(|_| Value::Text(Arc::from(text))),
            LogicalType::Decimal { .. } => Value::Decimal(Arc::from(text)),
            LogicalType::Date => parse_date(text)
                .map(Value::Date)
                .unwrap_or_else(|| invalid_temporal("DATE", text)),
            LogicalType::Time => parse_time(text)
                .map(Value::Time)
                .unwrap_or_else(|| invalid_temporal("TIME", text)),
            LogicalType::Timestamp | LogicalType::TimestampTz => parse_timestamp(text)
                .map(Value::Timestamp)
                .unwrap_or_else(|| invalid_temporal("DATETIME", text)),
            LogicalType::Json => Value::Json(Arc::from(text)),
            LogicalType::Bytes => Value::Bytes(Arc::from(text.as_bytes())),
            _ => Value::Text(Arc::from(text)),
        }
    }

    match value {
        None | Some(MyValue::NULL) => Value::Null,
        Some(MyValue::Int(v)) => Value::Int(*v),
        Some(MyValue::UInt(v)) => Value::UInt(*v),
        Some(MyValue::Float(v)) => Value::Float(*v as f64),
        Some(MyValue::Double(v)) => Value::Float(*v),
        // BLOB / VARBINARY / BIT 原样保留字节：合法 UTF-8 也不当文本，否则 0x01 会变成控制字符。
        Some(MyValue::Bytes(bytes)) if matches!(logical, Some(LogicalType::Bytes)) => {
            Value::Bytes(Arc::from(bytes.as_slice()))
        }
        Some(MyValue::Bytes(bytes)) => match std::str::from_utf8(bytes) {
            Ok(text) => coerce(text, logical),
            Err(_) => Value::Bytes(Arc::from(bytes.as_slice())),
        },
        Some(MyValue::Date(y, m, d, h, mi, s, us)) => {
            match chrono::NaiveDate::from_ymd_opt(*y as i32, *m as u32, *d as u32)
                .and_then(|date| date.and_hms_micro_opt(*h as u32, *mi as u32, *s as u32, *us))
            {
                Some(value) => Value::Timestamp(value),
                // MySQL 的零日期（0000-00-00）无法映射成 chrono 日期。
                // 保留原文，不要静默变成 1970-01-01。
                None => Value::Unknown {
                    native_type: "DATE".into(),
                    text: Arc::from(format!(
                        "{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}"
                    )),
                },
            }
        }
        Some(MyValue::Time(neg, days, h, mi, s, us)) => {
            let sign = if *neg { "-" } else { "" };
            let text = format!("{sign}{days}d {h:02}:{mi:02}:{s:02}.{us:06}");
            coerce(&text, logical)
        }
    }
}

/// 无法解析的日期 / 时间：保留原文并标成未知类型，绝不静默改值。
fn invalid_temporal(native_type: &str, text: &str) -> Value {
    Value::Unknown {
        native_type: native_type.to_string(),
        text: Arc::from(text),
    }
}

fn parse_date(text: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

fn parse_time(text: &str) -> Option<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(text, "%H:%M:%S%.f").ok()
}

fn parse_timestamp(text: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .or_else(|| parse_date(text).and_then(|date| date.and_hms_opt(0, 0, 0)))
}

struct MySqlMetadata {
    pool: Arc<Pool>,
}

#[async_trait]
impl MetadataProvider for MySqlMetadata {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError> {
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let rows: Vec<MyRow> = conn
            .query(
                "SELECT schema_name FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('information_schema','performance_schema','mysql','sys') \
                 ORDER BY schema_name",
            )
            .await
            .map_err(map_mysql_error)?;
        Ok(rows
            .into_iter()
            .map(|row| Namespace {
                catalog: Some(Arc::from(row.get::<String, _>(0).unwrap_or_default())),
                schema: None,
            })
            .collect())
    }

    async fn objects(
        &self,
        ns: &Namespace,
        kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError> {
        let Some(schema) = ns.catalog.as_deref().map(|s| s.to_string()) else {
            return Ok(Vec::new());
        };
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        // 注释 / 行数估计一起取：对象树要靠它们显示副标题与行数。
        // 之前只查了表名和类型，`comment` 恒为 None ⇒ 侧栏的注释永远不显示。
        //
        // 注意别在这里加「是否存在主键」的相关子查询：`information_schema.statistics`
        // 很大，36 张表就能把对象列表拖到十几秒（实测会一直转骨架屏）。
        let rows: Vec<MyRow> = conn
            .exec(
                "SELECT table_name, table_type, table_comment, table_rows \
                 FROM information_schema.tables \
                 WHERE table_schema = ? ORDER BY table_name",
                (schema.clone(),),
            )
            .await
            .map_err(map_mysql_error)?;

        let mut out = Vec::new();
        for row in rows {
            let name: String = row.get(0).unwrap_or_default();
            let table_type: String = row.get(1).unwrap_or_default();
            // 一律走 `get_opt`：mysql_common 的 `Row::get::<u64>` 遇到 NULL 会 panic
            // （视图的 table_rows 就是 NULL），而这里跑在 db 工作线程上，panic 之后
            // 界面只会一直停在骨架屏，排查成本极高。
            let comment: Option<String> = row
                .get_opt::<String, _>(2)
                .and_then(|text| text.ok())
                .and_then(|text| (!text.trim().is_empty()).then_some(text));
            // InnoDB 的 table_rows 是估算值，视图为 NULL。
            let row_estimate: Option<u64> = row.get_opt::<u64, _>(3).and_then(|value| value.ok());
            let kind = if table_type.contains("VIEW") {
                ObjectKind::View
            } else {
                ObjectKind::Table
            };
            if !kinds.contains(kind) {
                continue;
            }
            out.push(ObjectSummary {
                path: ObjectPath::new(ns.clone(), name, kind),
                row_estimate,
                comment,
                // 主键探测成本太高（见上），交给 `columns()` 按需回答。
                has_primary_key: None,
            });
        }

        if kinds.contains(ObjectKind::Procedure) || kinds.contains(ObjectKind::Function) {
            let routines: Vec<MyRow> = conn
                .exec(
                    "SELECT routine_name, routine_type FROM information_schema.routines \
                     WHERE routine_schema = ? ORDER BY routine_name",
                    (schema.clone(),),
                )
                .await
                .map_err(map_mysql_error)?;
            for row in routines {
                let name: String = row.get(0).unwrap_or_default();
                let routine_type: String = row.get(1).unwrap_or_default();
                let kind = if routine_type.eq_ignore_ascii_case("PROCEDURE") {
                    ObjectKind::Procedure
                } else {
                    ObjectKind::Function
                };
                if kinds.contains(kind) {
                    out.push(ObjectSummary {
                        path: ObjectPath::new(ns.clone(), name, kind),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: None,
                    });
                }
            }
        }

        if kinds.contains(ObjectKind::Trigger) {
            let triggers: Vec<MyRow> = conn
                .exec(
                    "SELECT trigger_name FROM information_schema.triggers \
                     WHERE trigger_schema = ? ORDER BY trigger_name",
                    (schema,),
                )
                .await
                .map_err(map_mysql_error)?;
            for row in triggers {
                let name: String = row.get(0).unwrap_or_default();
                out.push(ObjectSummary {
                    path: ObjectPath::new(ns.clone(), name, ObjectKind::Trigger),
                    row_estimate: None,
                    comment: None,
                    has_primary_key: None,
                });
            }
        }

        out.sort_by(|a, b| {
            a.path
                .kind
                .sort_rank()
                .cmp(&b.path.kind.sort_rank())
                .then_with(|| a.path.name.cmp(&b.path.name))
        });
        Ok(out)
    }

    async fn columns(&self, path: &ObjectPath) -> Result<Vec<ColumnInfo>, DriverError> {
        let Some(schema) = path.ns.catalog.as_deref() else {
            return Ok(Vec::new());
        };
        let table = path.name.to_string();
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let rows: Vec<MyRow> = conn
            .exec(
                // information_schema 的 column_default / column_key / extra 允许为 NULL；
                // mysql_common 把 NULL 直接按 String 取会 panic，先归一化成空串。
                "SELECT column_name, column_type, is_nullable, \
                        IFNULL(column_default, ''), IFNULL(column_key, ''), IFNULL(extra, '') \
                 FROM information_schema.columns \
                 WHERE table_schema = ? AND table_name = ? ORDER BY ordinal_position",
                (schema, table),
            )
            .await
            .map_err(map_mysql_error)?;

        let mut out = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            let name: String = row.get(0).unwrap_or_default();
            let native: String = row.get(1).unwrap_or_default();
            let nullable: String = row.get(2).unwrap_or_default();
            let default: Option<String> = row.get::<String, _>(3).filter(|s| !s.is_empty());
            let key: String = row.get(4).unwrap_or_default();
            let extra: String = row.get(5).unwrap_or_default();
            let mut col = ColumnInfo::simple(name, logical_from_mysql(&native), index as u32 + 1);
            col.meta.native_type = native;
            col.meta.nullable = nullable.eq_ignore_ascii_case("YES");
            col.default = default;
            col.is_primary_key = key.eq_ignore_ascii_case("PRI");
            col.is_auto_increment = extra.to_ascii_lowercase().contains("auto_increment");
            out.push(col);
        }
        Ok(out)
    }

    async fn indexes(&self, path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        let Some(schema) = path.ns.catalog.as_deref() else {
            return Ok(Vec::new());
        };
        let table = path.name.to_string();
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let rows: Vec<MyRow> = conn
            .exec(
                "SELECT index_name, non_unique, column_name, seq_in_index \
                 FROM information_schema.statistics \
                 WHERE table_schema = ? AND table_name = ? ORDER BY index_name, seq_in_index",
                (schema, table),
            )
            .await
            .map_err(map_mysql_error)?;

        let mut grouped: std::collections::BTreeMap<String, IndexInfo> =
            std::collections::BTreeMap::new();
        for row in rows {
            let name: String = row.get(0).unwrap_or_default();
            let non_unique: i64 = row.get(1).unwrap_or(1);
            let column: String = row.get(2).unwrap_or_default();
            let entry = grouped.entry(name.clone()).or_insert_with(|| IndexInfo {
                name: name.clone(),
                columns: Vec::new(),
                unique: non_unique == 0,
                primary: name.eq_ignore_ascii_case("PRIMARY"),
                method: Some("btree".into()),
            });
            entry.columns.push(column);
        }
        Ok(grouped.into_values().collect())
    }

    /// 外键（F-73）：`KEY_COLUMN_USAGE` + `REFERENTIAL_CONSTRAINTS`，按约束名合并复合列。
    async fn foreign_keys(&self, path: &ObjectPath) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        let Some(schema) = path.ns.catalog.as_deref() else {
            return Ok(Vec::new());
        };
        let table = path.name.to_string();
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let rows: Vec<MyRow> = conn
            .exec(foreign_keys_sql(), (schema, table))
            .await
            .map_err(map_mysql_error)?;

        let mut grouped: Vec<ForeignKeyInfo> = Vec::new();
        for row in rows {
            let name: String = row.get(0).unwrap_or_default();
            let column: String = row.get(1).unwrap_or_default();
            let referenced_table: String = row.get(2).unwrap_or_default();
            let referenced_column: String = row.get(3).unwrap_or_default();
            let on_delete: Option<String> = row.get(4);
            let on_update: Option<String> = row.get(5);
            match grouped.iter_mut().find(|fk| fk.name == name) {
                Some(existing) => {
                    existing.columns.push(column);
                    existing.referenced_columns.push(referenced_column);
                }
                None => grouped.push(ForeignKeyInfo {
                    name,
                    columns: vec![column],
                    referenced_table,
                    referenced_columns: vec![referenced_column],
                    on_delete,
                    on_update,
                }),
            }
        }
        Ok(grouped)
    }

    /// Table triggers (`information_schema.triggers`).
    async fn triggers(&self, path: &ObjectPath) -> Result<Option<Vec<TriggerInfo>>, DriverError> {
        let Some(schema) = path.ns.catalog.as_deref() else {
            return Ok(Some(Vec::new()));
        };
        let table = path.name.to_string();
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let rows: Vec<MyRow> = conn
            .exec(
                "SELECT trigger_name, action_timing, event_manipulation, action_statement \
                 FROM information_schema.triggers \
                 WHERE event_object_schema = ? AND event_object_table = ? \
                 ORDER BY trigger_name",
                (schema, table),
            )
            .await
            .map_err(map_mysql_error)?;
        Ok(Some(
            rows.into_iter()
                .map(|row| TriggerInfo {
                    name: row.get(0).unwrap_or_default(),
                    timing: row.get(1).unwrap_or(None),
                    event: row.get(2).unwrap_or(None),
                    statement: row.get(3).unwrap_or(None),
                })
                .collect(),
        ))
    }

    /// 直接依赖（F-83）：`information_schema` 的视图 / 例程使用关系，双向查询。
    ///
    /// MySQL 不记录例程之间的调用关系，反查只覆盖「视图 / 例程 → 表」。
    async fn dependencies(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::DependencyList>, DriverError> {
        use rustdb_db_api::DependencyList;
        let Some(schema) = path.ns.catalog.as_deref() else {
            return Ok(None);
        };
        let name = path.name.to_string();
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let mut out = DependencyList::default();

        if matches!(path.kind, ObjectKind::View | ObjectKind::MaterializedView) {
            let rows: Vec<MyRow> = conn
                .exec(
                    "SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.VIEW_TABLE_USAGE \
                     WHERE VIEW_SCHEMA = ? AND VIEW_NAME = ? ORDER BY TABLE_SCHEMA, TABLE_NAME",
                    (schema, name.as_str()),
                )
                .await
                .map_err(map_mysql_error)?;
            for row in rows {
                let Some(info) = mysql_dependency(
                    path,
                    row.get(0).unwrap_or_default(),
                    row.get(1).unwrap_or_default(),
                    ObjectKind::Table,
                    "视图引用",
                ) else {
                    continue;
                };
                out.push_depends_on(info);
            }
        }

        if matches!(path.kind, ObjectKind::Function | ObjectKind::Procedure) {
            // 低版本服务端没有 ROUTINE_TABLE_USAGE：查不到就只返回空列表。
            let rows: Vec<MyRow> = conn
                .exec(
                    "SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.ROUTINE_TABLE_USAGE \
                     WHERE ROUTINE_SCHEMA = ? AND ROUTINE_NAME = ? ORDER BY TABLE_SCHEMA, TABLE_NAME",
                    (schema, name.as_str()),
                )
                .await
                .unwrap_or_default();
            for row in rows {
                let Some(info) = mysql_dependency(
                    path,
                    row.get(0).unwrap_or_default(),
                    row.get(1).unwrap_or_default(),
                    ObjectKind::Table,
                    "例程引用",
                ) else {
                    continue;
                };
                out.push_depends_on(info);
            }
        }

        if matches!(
            path.kind,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        ) {
            let views: Vec<MyRow> = conn
                .exec(
                    "SELECT VIEW_SCHEMA, VIEW_NAME FROM information_schema.VIEW_TABLE_USAGE \
                     WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY VIEW_SCHEMA, VIEW_NAME",
                    (schema, name.as_str()),
                )
                .await
                .map_err(map_mysql_error)?;
            for row in views {
                let Some(info) = mysql_dependency(
                    path,
                    row.get(0).unwrap_or_default(),
                    row.get(1).unwrap_or_default(),
                    ObjectKind::View,
                    "被视图引用",
                ) else {
                    continue;
                };
                out.push_depended_by(info);
            }
            let routines: Vec<MyRow> = conn
                .exec(
                    "SELECT ROUTINE_SCHEMA, ROUTINE_NAME, ROUTINE_TYPE \
                     FROM information_schema.ROUTINE_TABLE_USAGE \
                     WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
                     ORDER BY ROUTINE_SCHEMA, ROUTINE_NAME",
                    (schema, name.as_str()),
                )
                .await
                .unwrap_or_default();
            for row in routines {
                let routine_type: String = row.get(2).unwrap_or_default();
                let kind = if routine_type.eq_ignore_ascii_case("FUNCTION") {
                    ObjectKind::Function
                } else {
                    ObjectKind::Procedure
                };
                let Some(info) = mysql_dependency(
                    path,
                    row.get(0).unwrap_or_default(),
                    row.get(1).unwrap_or_default(),
                    kind,
                    "被例程引用",
                ) else {
                    continue;
                };
                out.push_depended_by(info);
            }
        }

        out.note = Some("仅直接依赖（information_schema），例程间调用关系不可见".into());
        Ok(Some(out))
    }

    async fn table(&self, path: &ObjectPath) -> Result<TableDetail, DriverError> {
        let columns = self.columns(path).await?;
        let indexes = self.indexes(path).await.unwrap_or_default();
        let triggers = self.triggers(path).await.unwrap_or(None);
        Ok(TableDetail {
            summary: ObjectSummary {
                path: path.clone(),
                row_estimate: None,
                comment: None,
                has_primary_key: Some(indexes.iter().any(|i| i.primary)),
            },
            columns,
            indexes,
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers,
            engine: Some("InnoDB".into()),
            collation: None,
            comment: None,
            ddl_source: rustdb_db_api::DdlSource::Unavailable,
        })
    }

    /// `SHOW CREATE TABLE/VIEW/PROCEDURE/FUNCTION/TRIGGER`：第二列就是服务端原生 DDL。
    async fn object_ddl(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::ObjectDdl>, DriverError> {
        use rustdb_db_api::ObjectKind;
        let Some(schema) = path.ns.catalog.as_deref() else {
            return Ok(None);
        };
        let keyword = match path.kind {
            ObjectKind::Table => "TABLE",
            ObjectKind::View | ObjectKind::MaterializedView => "VIEW",
            ObjectKind::Procedure => "PROCEDURE",
            ObjectKind::Function => "FUNCTION",
            ObjectKind::Trigger => "TRIGGER",
            _ => return Ok(None),
        };
        let name = path.name.to_string();
        let mut conn = self.pool.get_conn().await.map_err(map_mysql_error)?;
        let sql = format!(
            "SHOW CREATE {keyword} `{}`.`{}`",
            schema.replace('`', "``"),
            name.replace('`', "``")
        );
        let rows: Vec<MyRow> = conn.query(sql).await.map_err(map_mysql_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        // 第一列是对象名，第二列是 DDL 文本。
        let text: Option<String> = row.get(1);
        Ok(text.map(|text| rustdb_db_api::ObjectDdl {
            text,
            source: rustdb_db_api::DdlSource::Native,
        }))
    }
}

/// 依赖查询结果 → 统一结构；跨库时标记 `remote`。
fn mysql_dependency(
    path: &ObjectPath,
    ns: String,
    name: String,
    kind: ObjectKind,
    dependency_type: &str,
) -> Option<rustdb_db_api::DependencyInfo> {
    if name.is_empty() {
        return None;
    }
    Some(rustdb_db_api::DependencyInfo {
        path: ObjectPath::new(Namespace::new(Some(ns.clone()), None::<String>), name, kind),
        dependency_type: dependency_type.to_string(),
        remote: (Some(ns.as_str()) != path.ns.catalog.as_deref()).then_some(ns),
    })
}

fn logical_from_mysql(native: &str) -> LogicalType {
    let lower = native.to_ascii_lowercase();
    if lower.starts_with("tinyint(1)") || lower == "boolean" || lower == "bool" {
        LogicalType::Bool
    } else if lower.contains("int") {
        LogicalType::Int {
            bits: if lower.contains("bigint") { 64 } else { 32 },
            unsigned: lower.contains("unsigned"),
        }
    } else if lower.contains("decimal") || lower.contains("numeric") {
        LogicalType::Decimal {
            precision: None,
            scale: None,
        }
    } else if lower.contains("double") || lower.contains("float") {
        LogicalType::Float { bits: 64 }
    } else if lower.contains("blob") {
        LogicalType::Bytes
    } else if lower.contains("json") {
        LogicalType::Json
    } else if lower.contains("date") && !lower.contains("datetime") {
        LogicalType::Date
    } else if lower.contains("datetime") || lower.contains("timestamp") {
        LogicalType::Timestamp
    } else if lower.contains("time") {
        LogicalType::Time
    } else if lower.contains("text") || lower.contains("char") {
        LogicalType::Text { max_len: None }
    } else {
        LogicalType::Unknown(native.to_string())
    }
}

/// 外键查询（F-73）：按列展开，复合外键由调用方按约束名合并。
fn foreign_keys_sql() -> &'static str {
    "SELECT k.constraint_name, k.column_name, k.referenced_table_name, \
            k.referenced_column_name, rc.delete_rule, rc.update_rule \
     FROM information_schema.key_column_usage AS k \
     JOIN information_schema.referential_constraints AS rc \
       ON rc.constraint_schema = k.constraint_schema \
      AND rc.constraint_name = k.constraint_name \
     WHERE k.table_schema = ? AND k.table_name = ? \
       AND k.referenced_table_name IS NOT NULL \
     ORDER BY k.constraint_name, k.ordinal_position"
}

fn map_mysql_error(err: mysql_async::Error) -> DriverError {
    if let mysql_async::Error::Server(server) = &err {
        let kind = match server.code {
            1064 | 1149 => DriverErrorKind::Syntax,
            1045 => DriverErrorKind::Auth,
            1044 => DriverErrorKind::Permission,
            1049 | 1146 | 1054 => DriverErrorKind::NotFound,
            1062 | 1451 | 1452 => DriverErrorKind::Constraint,
            1213 => DriverErrorKind::Deadlock,
            1205 => DriverErrorKind::Timeout,
            1317 => DriverErrorKind::Cancelled,
            1290 => DriverErrorKind::ReadOnly,
            _ => DriverErrorKind::Protocol,
        };
        return DriverError::new(kind, server.message.clone())
            .with_code(server.code as i64)
            .with_sqlstate(server.state.clone());
    }
    DriverError::connection(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_rows_mark_cross_schema_refs() {
        let path = ObjectPath::new(
            Namespace::new(Some("shop"), None::<String>),
            "orders",
            ObjectKind::Function,
        );
        let same = mysql_dependency(
            &path,
            "shop".into(),
            "users".into(),
            ObjectKind::Table,
            "例程引用",
        )
        .expect("应生成依赖项");
        assert!(same.remote.is_none());
        assert_eq!(same.dependency_type, "例程引用");

        let other = mysql_dependency(
            &path,
            "audit".into(),
            "events".into(),
            ObjectKind::Table,
            "例程引用",
        )
        .expect("应生成依赖项");
        assert_eq!(other.remote.as_deref(), Some("audit"));

        assert!(
            mysql_dependency(&path, "shop".into(), String::new(), ObjectKind::Table, "x").is_none()
        );
    }

    #[test]
    fn foreign_keys_sql_reads_information_schema() {
        let sql = foreign_keys_sql();
        assert!(sql.contains("key_column_usage"), "{sql}");
        assert!(sql.contains("referential_constraints"), "{sql}");
        assert!(sql.contains("referenced_table_name IS NOT NULL"), "{sql}");
        assert!(sql.contains("ORDER BY k.constraint_name"), "{sql}");
    }

    #[test]
    fn null_ordering_uses_mysql_compatible_syntax() {
        let dialect = MySqlDialect {
            kind: DbKind::Mysql,
        };
        assert_eq!(
            dialect.order_clause("name", false, true),
            "`name` IS NULL ASC, `name` ASC"
        );
        assert_eq!(
            dialect.order_clause("name", true, false),
            "`name` IS NULL DESC, `name` DESC"
        );
    }

    #[test]
    fn zero_dates_are_preserved_instead_of_becoming_epoch() {
        let value = convert_value(Some(&MyValue::Date(0, 0, 0, 0, 0, 0, 0)), None);
        match value {
            Value::Unknown { text, .. } => assert!(text.starts_with("0000-00-00")),
            other => panic!("零日期应保留原文，实际 {other:?}"),
        }
    }
}

#[cfg(test)]
mod column_type_tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    fn logical(ty: ColumnType, unsigned: bool) -> LogicalType {
        map_column_type(ty, unsigned, false).0
    }

    /// 回归：结果列曾一律被标成 `text`，导致 `id` 升序排成 1,10,11,…,2,20。
    #[test]
    fn numeric_columns_map_to_numeric_logical_types() {
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_LONG, false),
            LogicalType::Int { bits: 32, .. }
        ));
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_LONGLONG, true),
            LogicalType::Int { bits: 64, .. }
        ));
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_NEWDECIMAL, false),
            LogicalType::Decimal { .. }
        ));
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_DOUBLE, false),
            LogicalType::Float { bits: 64 }
        ));
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_DATETIME, false),
            LogicalType::Timestamp
        ));
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_JSON, false),
            LogicalType::Json
        ));
        assert!(matches!(
            logical(ColumnType::MYSQL_TYPE_VAR_STRING, false),
            LogicalType::Text { .. }
        ));
    }

    fn text(v: &str) -> MyValue {
        MyValue::Bytes(v.as_bytes().to_vec())
    }

    /// 文本协议下整列都是字节串，靠列类型还原。
    #[test]
    fn text_protocol_values_are_coerced_by_column_type() {
        let int_ty = LogicalType::Int {
            bits: 32,
            unsigned: false,
        };
        assert_eq!(
            convert_value(Some(&text("42")), Some(&int_ty)),
            Value::Int(42)
        );
        // 同一串 "42" 在 VARCHAR 列必须还是文本，不能被当成数字。
        let text_ty = LogicalType::Text { max_len: None };
        assert_eq!(
            convert_value(Some(&text("42")), Some(&text_ty)),
            Value::Text(Arc::from("42"))
        );
        assert_eq!(
            convert_value(
                Some(&text("12.30")),
                Some(&LogicalType::Decimal {
                    precision: None,
                    scale: None
                })
            ),
            Value::Decimal(Arc::from("12.30")),
            "定点数必须原样保留，不能转 f64"
        );
        assert_eq!(
            convert_value(Some(&text("{\"k\": 1}")), Some(&LogicalType::Json)),
            Value::Json(Arc::from("{\"k\": 1}"))
        );
        match convert_value(Some(&text("2024-01-02")), Some(&LogicalType::Date)) {
            Value::Date(date) => {
                assert_eq!((date.year(), date.month(), date.day()), (2024, 1, 2))
            }
            other => panic!("DATE 列应还原成日期，实际 {other:?}"),
        }
        match convert_value(
            Some(&text("2024-01-02 03:04:05.5")),
            Some(&LogicalType::Timestamp),
        ) {
            Value::Timestamp(ts) => {
                assert_eq!((ts.hour(), ts.minute(), ts.second()), (3, 4, 5))
            }
            other => panic!("DATETIME 列应还原成时间戳，实际 {other:?}"),
        }
        // 超范围 TIME（MySQL 允许到 838 小时）还原失败时保原文，不静默清零。
        match convert_value(Some(&text("838:59:59")), Some(&LogicalType::Time)) {
            Value::Unknown { text, .. } => assert_eq!(text.as_ref(), "838:59:59"),
            other => panic!("超范围 TIME 应保留原文，实际 {other:?}"),
        }
    }

    /// MySQL 的 TEXT 与 BLOB 都报 MYSQL_TYPE_BLOB，只能靠字符集分流：
    /// 分错就会把长文本渲染成十六进制，或把二进制当文本显示控制字符。
    #[test]
    fn text_and_blob_are_distinguished_by_charset() {
        let (text_ty, text_native) = map_column_type(ColumnType::MYSQL_TYPE_BLOB, false, false);
        assert!(
            matches!(text_ty, LogicalType::Text { .. }),
            "TEXT 列应是文本"
        );
        assert_eq!(text_native, "TEXT");
        let (blob_ty, blob_native) = map_column_type(ColumnType::MYSQL_TYPE_BLOB, false, true);
        assert_eq!(blob_ty, LogicalType::Bytes, "binary 字符集的 BLOB 应是字节");
        assert_eq!(blob_native, "BLOB");
    }

    /// 二进制列即使内容是合法 UTF-8，也不能被当文本（否则 0x01 会变控制字符）。
    #[test]
    fn binary_columns_keep_raw_bytes() {
        let value = convert_value(Some(&text("A")), Some(&LogicalType::Bytes));
        assert_eq!(value, Value::Bytes(Arc::from(b"A".as_slice())));
    }
}
