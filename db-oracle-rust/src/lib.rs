//! Oracle 驱动 spike（X-01）：官方纯 Rust `oracledb`，免 Instant Client。
//!
//! 目的不是替换现有 `rustdb-db-oracle`（ODPI-C），而是先验证三件事：
//!
//! 1. **线程模型**：`oracledb::Connection` 是同步 API。这里由一条专用 OS 线程
//!    独占连接，异步侧只通过 `oneshot` 取结果，主线程 / GPUI 永不阻塞。
//! 2. **类型往返**：NUMBER（精确十进制，不过 f64）、CLOB / BLOB、DATE /
//!    TIMESTAMP / TIMESTAMP WITH TIME ZONE 的读取口径与现有驱动保持一致。
//! 3. **取消**：crate 没有旁路中断接口，`Connection` 也不是 `Sync`；能做的只有
//!    `set_call_timeout`（客户端调用超时）。因此 `cancel` 仍显式
//!    `Unsupported`、能力位维持 `None`，超时映射为 `DriverErrorKind::Timeout`。
//!
//! 元数据（库 / 对象 / 列 / 索引）**未移植**：spike 只覆盖连接、查询、DML、
//! 事务与 `ALTER SESSION`。注册到 app 之前需要补齐，或在切换时保留旧实现
//! 通过 feature 回退（R-21）。

use async_trait::async_trait;
use chrono::{FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use oracledb::{
    Config, Connection, DbType, ErrorKind, Lob, Metadata, OracleNumber, OracleTimestamp, Row,
    Statement,
};
use rustdb_db_api::{
    dialect::quote_double, CancelStyle, Capabilities, ColumnInfo, ColumnMeta, DbKind, Dialect,
    DriverError, DriverErrorKind, DriverFactory, ExplainStyle, ForeignKeyInfo, IndexInfo,
    JsonStyle, LogicalType, MetadataProvider, Namespace, ObjectKindMask, ObjectPath, ObjectSummary,
    QueryHandle, QueryId, ServerInfo, Session, SessionId, TableDetail, TxId, TxOptions,
    UpsertStyle, Value,
};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc as async_mpsc, oneshot};

/// dry 连接：`oracledb` 是同步 API，所有调用都排进专用线程。
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

struct Worker {
    tx: async_mpsc::UnboundedSender<Job>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Worker {
    /// 起一条专用线程并在其中建连：`Connection` 不必跨线程移动。
    fn start<F>(make: F) -> (Self, oneshot::Receiver<Result<(), DriverError>>)
    where
        F: FnOnce() -> Result<Connection, DriverError> + Send + 'static,
    {
        let (job_tx, mut job_rx) = async_mpsc::unbounded_channel::<Job>();
        let (init_tx, init_rx) = oneshot::channel();
        let handle = std::thread::Builder::new()
            .name("oracledb-worker".into())
            .spawn(move || {
                let mut conn = match make() {
                    Ok(conn) => conn,
                    Err(err) => {
                        let _ = init_tx.send(Err(err));
                        return;
                    }
                };
                if init_tx.send(Ok(())).is_err() {
                    return;
                }
                while let Some(job) = job_rx.blocking_recv() {
                    job(&mut conn);
                }
            })
            .expect("oracledb worker thread");
        (
            Self {
                tx: job_tx,
                thread: Mutex::new(Some(handle)),
            },
            init_rx,
        )
    }

    async fn call<T, F>(&self, job: F) -> Result<T, DriverError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DriverError> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = reply_tx.send(job(conn));
            }))
            .map_err(|_| DriverError::connection("Oracle worker 线程已退出"))?;
        reply_rx
            .await
            .map_err(|_| DriverError::connection("Oracle worker 线程已退出"))?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // 关掉命令通道即可让线程在处理完当前语句后退出；不 join，避免挂住 UI。
        if let Ok(mut slot) = self.thread.lock() {
            slot.take();
        }
    }
}

pub struct OracleRustFactory;

impl Default for OracleRustFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl OracleRustFactory {
    pub fn new() -> Self {
        Self
    }
}

fn capabilities() -> Capabilities {
    let mut caps = Capabilities::minimal();
    caps.multi_statement = false;
    caps.returning = true;
    caps.cte = true;
    caps.window_functions = true;
    caps.upsert = UpsertStyle::Merge;
    // 见文件头：`oracledb` 没有旁路中断，只提供客户端调用超时。
    caps.cancel = CancelStyle::None;
    caps.explain = ExplainStyle::ExplainPlan;
    caps.ddl_transactional = false;
    caps.schemas = true;
    caps.json = JsonStyle::TextJson;
    caps.sequences = true;
    caps.materialized_views = true;
    caps.max_ident_len = 128;
    caps.read_only_txn = true;
    caps.streaming_rows = false;
    caps.notices = true;
    caps
}

#[async_trait]
impl DriverFactory for OracleRustFactory {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn display_name(&self) -> &'static str {
        "Oracle (oracledb)"
    }

    fn baseline_capabilities(&self) -> Capabilities {
        capabilities()
    }

    async fn connect(
        &self,
        opts: &rustdb_db_api::ConnectOptions,
    ) -> Result<Box<dyn Session>, DriverError> {
        let user = opts.username.clone().unwrap_or_else(|| "system".into());
        let password = opts.secret.clone().unwrap_or_default();
        let connect_string = sql::build_connect_string(
            opts.host.as_deref(),
            opts.port,
            opts.database.as_deref(),
            &opts.params,
        );
        let timeout = Duration::from_millis(opts.connect_timeout_ms.max(1000));
        let display_target = connect_string.clone();

        let (worker, init) = Worker::start(move || {
            let config = Config::default()
                .set_credentials(&user, &password)
                .set_connect_string(&connect_string)
                .map_err(map_error)?;
            oracledb::connect(config).map_err(map_error)
        });

        tokio::time::timeout(timeout, init)
            .await
            .map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Timeout,
                    format!(
                        "连接 Oracle 超时（{display_target}，{} ms）",
                        timeout.as_millis()
                    ),
                )
            })?
            .map_err(|_| DriverError::connection("Oracle worker 线程已退出"))??;

        let mut server = worker
            .call(|conn| {
                let mut server = ServerInfo::new(
                    conn.version()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|_| "Oracle".into()),
                );
                if let Ok(service) = conn.service_name() {
                    if !service.is_empty() {
                        server.current_db = Some(service.to_string());
                    }
                }
                server.max_ident_len = conn.max_identifier_length().unwrap_or(128);
                Ok(server)
            })
            .await?;
        server.charset = Some("AL32UTF8".into());
        server.read_only = opts.read_only;

        Ok(Box::new(OracleRustSession {
            id: SessionId::new(),
            worker,
            server,
            caps: capabilities(),
            dialect: Arc::new(OracleRustDialect),
            meta: OracleRustMetadata,
            running: Arc::new(AtomicBool::new(false)),
        }))
    }
}

struct OracleRustSession {
    id: SessionId,
    worker: Worker,
    server: ServerInfo,
    caps: Capabilities,
    dialect: Arc<dyn Dialect>,
    meta: OracleRustMetadata,
    running: Arc<AtomicBool>,
}

#[async_trait]
impl Session for OracleRustSession {
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
        self.worker
            .call(|conn| conn.ping().map_err(map_error))
            .await?;
        Ok(started.elapsed())
    }

    async fn execute(
        &self,
        req: rustdb_db_api::ExecuteRequest,
    ) -> Result<QueryHandle, DriverError> {
        let query_id = req.query_id.clone().unwrap_or_else(QueryId::new);
        let max_rows = req.fetch.max_rows.max(1) as usize;
        let timeout = req.fetch.timeout_ms.map(Duration::from_millis);
        let sql_text = req.sql.clone();

        self.running.store(true, Ordering::SeqCst);
        let outcome = self
            .worker
            .call(move |conn| {
                conn.set_call_timeout(timeout).map_err(map_error)?;
                run_blocking(conn, &sql_text, max_rows)
            })
            .await;
        self.running.store(false, Ordering::SeqCst);

        let (columns, rows, affected, truncated) = outcome?;
        let has_columns = !columns.is_empty();
        let column_meta: Arc<[ColumnMeta]> = columns
            .into_iter()
            .map(|(name, logical, native, nullable)| ColumnMeta {
                name: Arc::from(name),
                logical_type: logical,
                native_type: native,
                nullable,
            })
            .collect::<Vec<_>>()
            .into();

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

    /// `oracledb` 没有 OCI 的 `break_execution`，连接也不是 `Sync`，无法从旁路
    /// 线程打断执行。能做的只有 `set_call_timeout`（见 `execute`）。
    async fn cancel(&self, _query_id: &QueryId) -> Result<(), DriverError> {
        let msg = if self.running.load(Ordering::SeqCst) {
            "Oracle 取消：oracledb 没有旁路中断接口，请用查询超时或等待返回"
        } else {
            "Oracle 取消：当前没有正在执行的查询"
        };
        Err(DriverError::unsupported(msg))
    }

    async fn begin(&self, opts: TxOptions) -> Result<TxId, DriverError> {
        let read_only = opts.read_only;
        self.worker
            .call(move |conn| {
                if read_only {
                    conn.execute("SET TRANSACTION READ ONLY", &[])
                        .map_err(map_error)?;
                }
                Ok(())
            })
            .await?;
        Ok(TxId::new())
    }

    async fn commit(&self, _tx: TxId) -> Result<(), DriverError> {
        self.worker
            .call(|conn| conn.commit().map_err(map_error))
            .await
    }

    async fn rollback(&self, _tx: TxId) -> Result<(), DriverError> {
        self.worker
            .call(|conn| conn.rollback().map_err(map_error))
            .await
    }

    async fn use_namespace(&self, ns: &Namespace) -> Result<(), DriverError> {
        if let Some(catalog) = &ns.catalog {
            return Err(DriverError::unsupported(format!(
                "Oracle 不支持切换 catalog（{catalog}）；请通过 schema/user 切换"
            )));
        }
        let Some(schema) = ns.schema.as_deref().map(str::to_string) else {
            return Ok(());
        };
        let sql_text = sql::alter_session_schema_sql(&schema);
        self.worker
            .call(move |conn| {
                conn.execute(&sql_text, &[]).map_err(map_error)?;
                Ok(())
            })
            .await
    }

    fn metadata(&self) -> &dyn MetadataProvider {
        &self.meta
    }
}

type Outcome = (
    Vec<(String, LogicalType, String, bool)>,
    Vec<rustdb_db_api::Row>,
    u64,
    bool,
);

/// 同步执行一条语句（在 worker 线程里调用）。
fn run_blocking(
    conn: &mut Connection,
    sql_text: &str,
    max_rows: usize,
) -> Result<Outcome, DriverError> {
    let mut statement: Statement = conn
        .statement(sql_text)
        .map_err(map_error)?
        .fetch_lobs()
        .build()
        .map_err(map_error)?;

    if statement.is_query() {
        let mut cursor = statement.query(&[]).map_err(map_error)?;
        let columns: Vec<(String, LogicalType, String, bool)> = cursor
            .columns()
            .iter()
            .map(|meta| {
                (
                    meta.name().to_string(),
                    logical_of(meta),
                    meta.data_type(),
                    meta.nullable(),
                )
            })
            .collect();
        let mut rows = Vec::new();
        let mut truncated = false;
        for item in cursor.by_ref() {
            if rows.len() >= max_rows {
                truncated = true;
                break;
            }
            let mut row = item.map_err(map_error)?;
            rows.push(read_row(&mut row)?);
        }
        Ok((columns, rows, 0, truncated))
    } else {
        let result = statement.execute(&[]).map_err(map_error)?;
        Ok((Vec::new(), Vec::new(), result.rows_affected(), false))
    }
}

fn read_row(row: &mut Row) -> Result<rustdb_db_api::Row, DriverError> {
    let metas = row.columns().to_vec();
    let mut out = Vec::with_capacity(metas.len());
    for (index, meta) in metas.iter().enumerate() {
        out.push(read_column(row, index, meta)?);
    }
    Ok(out)
}

fn read_column(row: &mut Row, index: usize, meta: &Metadata) -> Result<Value, DriverError> {
    let db_type = meta.db_type();
    let same = |target: &'static DbType| std::ptr::eq(db_type, target);

    if same(oracledb::DB_TYPE_NUMBER) {
        return Ok(
            match row.get::<Option<OracleNumber>>(index).map_err(map_error)? {
                Some(number) => sql::parse_number(&number.to_string()),
                None => Value::Null,
            },
        );
    }
    if same(oracledb::DB_TYPE_BINARY_FLOAT) {
        return Ok(row
            .get::<Option<f32>>(index)
            .map_err(map_error)?
            .map(|v| Value::Float(v as f64))
            .unwrap_or(Value::Null));
    }
    if same(oracledb::DB_TYPE_BINARY_DOUBLE) {
        return Ok(row
            .get::<Option<f64>>(index)
            .map_err(map_error)?
            .map(Value::Float)
            .unwrap_or(Value::Null));
    }
    if same(oracledb::DB_TYPE_BOOLEAN) {
        return Ok(row
            .get::<Option<bool>>(index)
            .map_err(map_error)?
            .map(Value::Bool)
            .unwrap_or(Value::Null));
    }
    if same(oracledb::DB_TYPE_DATE)
        || same(oracledb::DB_TYPE_TIMESTAMP)
        || same(oracledb::DB_TYPE_TIMESTAMP_LTZ)
    {
        return Ok(row
            .get::<Option<OracleTimestamp>>(index)
            .map_err(map_error)?
            .map(timestamp_value)
            .unwrap_or(Value::Null));
    }
    if same(oracledb::DB_TYPE_TIMESTAMP_TZ) {
        return Ok(row
            .get::<Option<OracleTimestamp>>(index)
            .map_err(map_error)?
            .map(timestamp_tz_value)
            .unwrap_or(Value::Null));
    }
    if same(oracledb::DB_TYPE_CLOB) || same(oracledb::DB_TYPE_NCLOB) {
        let Some(mut lob) = row.take::<Option<Lob>>(index).map_err(map_error)? else {
            return Ok(Value::Null);
        };
        let bytes = read_lob(&mut lob)?;
        return Ok(Value::Text(Arc::from(
            String::from_utf8_lossy(&bytes).into_owned().as_str(),
        )));
    }
    if same(oracledb::DB_TYPE_BLOB) || same(oracledb::DB_TYPE_BFILE) {
        let Some(mut lob) = row.take::<Option<Lob>>(index).map_err(map_error)? else {
            return Ok(Value::Null);
        };
        let bytes = read_lob(&mut lob)?;
        return Ok(Value::Bytes(Arc::from(bytes.as_slice())));
    }
    if same(oracledb::DB_TYPE_VARCHAR)
        || same(oracledb::DB_TYPE_NVARCHAR)
        || same(oracledb::DB_TYPE_CHAR)
        || same(oracledb::DB_TYPE_NCHAR)
        || same(oracledb::DB_TYPE_LONG)
        || same(oracledb::DB_TYPE_LONG_NVARCHAR)
        || same(oracledb::DB_TYPE_ROWID)
        || same(oracledb::DB_TYPE_UROWID)
    {
        return Ok(row
            .get::<Option<String>>(index)
            .map_err(map_error)?
            .map(|text| Value::Text(Arc::from(text.as_str())))
            .unwrap_or(Value::Null));
    }
    if same(oracledb::DB_TYPE_RAW) || same(oracledb::DB_TYPE_LONG_RAW) {
        return Ok(row
            .get::<Option<Vec<u8>>>(index)
            .map_err(map_error)?
            .map(|bytes| Value::Bytes(Arc::from(bytes.as_slice())))
            .unwrap_or(Value::Null));
    }

    // 其余类型（JSON / VECTOR / 对象 / 游标 / 间隔）先按文本尽力读取。
    if let Ok(value) = row.get::<Option<String>>(index) {
        return Ok(value
            .map(|text| Value::Text(Arc::from(text.as_str())))
            .unwrap_or(Value::Null));
    }
    Ok(Value::Unknown {
        native_type: meta.data_type(),
        text: Arc::from("<oracledb 暂不支持的列类型>"),
    })
}

fn read_lob(lob: &mut Lob) -> Result<Vec<u8>, DriverError> {
    let mut bytes = Vec::new();
    lob.read_to_end(&mut bytes)
        .map_err(|err| DriverError::new(DriverErrorKind::Io, format!("读取 LOB 失败：{err}")))?;
    Ok(bytes)
}

fn timestamp_value(ts: OracleTimestamp) -> Value {
    match naive_of(ts) {
        Some(value) => Value::Timestamp(value),
        None => Value::Unknown {
            native_type: "DATE/TIMESTAMP".into(),
            text: Arc::from(ts.to_string().as_str()),
        },
    }
}

fn timestamp_tz_value(ts: OracleTimestamp) -> Value {
    let Some(naive) = naive_of(ts) else {
        return Value::Unknown {
            native_type: "TIMESTAMP WITH TIME ZONE".into(),
            text: Arc::from(ts.to_string().as_str()),
        };
    };
    let offset_seconds = ts.tz_hour_offset() as i32 * 3600 + ts.tz_minute_offset() as i32 * 60;
    match FixedOffset::east_opt(offset_seconds)
        .and_then(|offset| offset.from_local_datetime(&naive).single())
    {
        Some(value) => Value::TimestampTz(value.with_timezone(&Utc)),
        None => Value::Unknown {
            native_type: "TIMESTAMP WITH TIME ZONE".into(),
            text: Arc::from(ts.to_string().as_str()),
        },
    }
}

fn naive_of(ts: OracleTimestamp) -> Option<NaiveDateTime> {
    let date = NaiveDate::from_ymd_opt(ts.year() as i32, ts.month() as u32, ts.day() as u32)?;
    date.and_hms_nano_opt(
        ts.hour() as u32,
        ts.minute() as u32,
        ts.second() as u32,
        ts.nanoseconds(),
    )
}

fn logical_of(meta: &Metadata) -> LogicalType {
    let db_type = meta.db_type();
    let same = |target: &'static DbType| std::ptr::eq(db_type, target);

    if same(oracledb::DB_TYPE_NUMBER) {
        let precision = meta.precision();
        let scale = meta.scale();
        return if scale == 0 && (1..=18).contains(&precision) {
            LogicalType::Int {
                bits: if precision > 10 { 64 } else { 32 },
                unsigned: false,
            }
        } else {
            LogicalType::Decimal {
                precision: (precision > 0).then_some(precision),
                scale: (scale >= 0).then_some(scale as u8),
            }
        };
    }
    if same(oracledb::DB_TYPE_BINARY_FLOAT) {
        return LogicalType::Float { bits: 32 };
    }
    if same(oracledb::DB_TYPE_BINARY_DOUBLE) {
        return LogicalType::Float { bits: 64 };
    }
    if same(oracledb::DB_TYPE_BOOLEAN) {
        return LogicalType::Bool;
    }
    if same(oracledb::DB_TYPE_DATE)
        || same(oracledb::DB_TYPE_TIMESTAMP)
        || same(oracledb::DB_TYPE_TIMESTAMP_LTZ)
    {
        return LogicalType::Timestamp;
    }
    if same(oracledb::DB_TYPE_TIMESTAMP_TZ) {
        return LogicalType::TimestampTz;
    }
    if same(oracledb::DB_TYPE_CLOB) || same(oracledb::DB_TYPE_NCLOB) {
        return LogicalType::Text { max_len: None };
    }
    if same(oracledb::DB_TYPE_BLOB) || same(oracledb::DB_TYPE_BFILE) {
        return LogicalType::Bytes;
    }
    if same(oracledb::DB_TYPE_RAW) || same(oracledb::DB_TYPE_LONG_RAW) {
        return LogicalType::Bytes;
    }
    if same(oracledb::DB_TYPE_JSON) {
        return LogicalType::Json;
    }
    if same(oracledb::DB_TYPE_VARCHAR)
        || same(oracledb::DB_TYPE_NVARCHAR)
        || same(oracledb::DB_TYPE_CHAR)
        || same(oracledb::DB_TYPE_NCHAR)
        || same(oracledb::DB_TYPE_LONG)
        || same(oracledb::DB_TYPE_LONG_NVARCHAR)
        || same(oracledb::DB_TYPE_ROWID)
        || same(oracledb::DB_TYPE_UROWID)
    {
        let max_len = meta.max_size();
        return LogicalType::Text {
            max_len: (max_len > 0).then_some(max_len),
        };
    }
    LogicalType::Unknown(meta.data_type())
}

fn map_error(err: oracledb::Error) -> DriverError {
    match err.kind() {
        ErrorKind::DbError(db) => {
            let code = db.code();
            let kind = match code {
                1 | 2291 | 2292 | 1400 | 1555 => DriverErrorKind::Constraint,
                54 | 60 | 8177 => DriverErrorKind::Deadlock,
                900 | 901 | 904 | 905 | 906 | 907 | 908 | 909 | 910 | 911 | 917 | 918 | 920
                | 921 | 922 | 923 | 924 | 926 | 927 | 928 | 936 | 970 | 971 | 1008 | 1009
                | 1035 => DriverErrorKind::Syntax,
                942 | 1418 => DriverErrorKind::NotFound,
                1017 | 28000 | 28001 => DriverErrorKind::Auth,
                1031 | 1032 | 1045 => DriverErrorKind::Permission,
                1013 => DriverErrorKind::Cancelled,
                12170 | 12505 | 12514 | 12541 | 12545 => DriverErrorKind::Connection,
                _ => DriverErrorKind::Protocol,
            };
            let mut mapped = DriverError::new(kind, db.message().to_string())
                .with_code(code as i64)
                .with_sqlstate(format!("ORA-{code:05}"));
            if db.offset() > 0 {
                mapped.position = Some(db.offset() as u32);
            }
            mapped
        }
        ErrorKind::CallTimeoutExceeded => DriverError::new(
            DriverErrorKind::Timeout,
            "Oracle 查询超过客户端调用超时（set_call_timeout）",
        ),
        ErrorKind::DeadConnection => DriverError::connection("Oracle 连接已被服务端或网络关闭"),
        ErrorKind::NoCredentials
        | ErrorKind::NoConnectString
        | ErrorKind::InvalidConnectString(_, _) => {
            DriverError::new(DriverErrorKind::Connection, err.to_string())
        }
        _ => DriverError::new(DriverErrorKind::Protocol, err.to_string()),
    }
}

struct OracleRustDialect;

impl Dialect for OracleRustDialect {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_double(ident)
    }

    fn placeholder(&self, index: usize) -> String {
        format!(":{index}")
    }

    fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
        sql::paginate(sql, limit, offset)
    }

    fn explain_sql(&self, sql: &str, _analyze: bool) -> String {
        format!("EXPLAIN PLAN FOR {sql}")
    }
}

/// spike 未移植元数据：显式 `Unsupported`，不返回空列表冒充成功（R5）。
struct OracleRustMetadata;

fn metadata_unsupported(feature: &str) -> DriverError {
    DriverError::unsupported(format!(
        "oracledb spike 未移植{feature}，请使用 ODPI-C 驱动"
    ))
}

#[async_trait]
impl MetadataProvider for OracleRustMetadata {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError> {
        Err(metadata_unsupported("命名空间查询"))
    }

    async fn objects(
        &self,
        _ns: &Namespace,
        _kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError> {
        Err(metadata_unsupported("对象列表"))
    }

    async fn columns(&self, _path: &ObjectPath) -> Result<Vec<ColumnInfo>, DriverError> {
        Err(metadata_unsupported("列信息"))
    }

    async fn indexes(&self, _path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        Err(metadata_unsupported("索引"))
    }

    async fn foreign_keys(&self, _path: &ObjectPath) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        Err(metadata_unsupported("外键"))
    }

    async fn table(&self, _path: &ObjectPath) -> Result<TableDetail, DriverError> {
        Err(metadata_unsupported("表详情"))
    }
}

/// 纯函数区：连接串、分页、数字解析。可单测，不碰网络。
pub mod sql {
    use super::*;
    use rustdb_db_api::dialect::quote_string_literal;

    pub fn select_one() -> &'static str {
        "SELECT 1 FROM dual"
    }

    pub fn build_connect_string(
        host: Option<&str>,
        port: Option<u16>,
        database: Option<&str>,
        params: &BTreeMap<String, String>,
    ) -> String {
        if let Some(full) = params.get("connect_string") {
            return full.clone();
        }
        if let Some(db) = database {
            if db.trim_start().starts_with('(') {
                return db.to_string();
            }
        }
        let host = host.unwrap_or("127.0.0.1");
        let port = port.unwrap_or(1521);
        if let Some(sid) = params.get("sid") {
            return format!(
                "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST={host})(PORT={port}))(CONNECT_DATA=(SID={sid})))"
            );
        }
        match database {
            Some(service) if !service.is_empty() => format!("{host}:{port}/{service}"),
            _ => format!("{host}:{port}"),
        }
    }

    pub fn alter_session_schema_sql(schema: &str) -> String {
        format!(
            "ALTER SESSION SET CURRENT_SCHEMA = {}",
            quote_double(schema)
        )
    }

    pub fn paginate(sql: &str, limit: u64, offset: u64) -> String {
        let upper = offset.saturating_add(limit);
        format!(
            "SELECT * FROM (SELECT rustdb_inner.*, ROWNUM AS rustdb_rnum FROM ({sql}) rustdb_inner \
             WHERE ROWNUM <= {upper}) WHERE rustdb_rnum > {offset}"
        )
    }

    /// NUMBER 先取精确十进制字符串：整数走 `Int`，其余保留原文（金额不过 f64）。
    pub fn parse_number(text: &str) -> Value {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Value::Null;
        }
        if !trimmed.contains('.') && !trimmed.contains('e') && !trimmed.contains('E') {
            if let Ok(value) = trimmed.parse::<i64>() {
                return Value::Int(value);
            }
        }
        Value::Decimal(Arc::from(trimmed))
    }

    pub fn quote_literal(value: &str) -> String {
        quote_string_literal(value)
    }
}

impl fmt::Display for OracleRustFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Oracle (oracledb)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_keeps_exact_decimal() {
        assert!(matches!(sql::parse_number("123"), Value::Int(123)));
        assert!(matches!(sql::parse_number("  -7 "), Value::Int(-7)));
        match sql::parse_number("12345678901234567890.123") {
            Value::Decimal(text) => assert_eq!(&*text, "12345678901234567890.123"),
            other => panic!("expected Decimal, got {other:?}"),
        }
        match sql::parse_number("1.0E+5") {
            Value::Decimal(text) => assert_eq!(&*text, "1.0E+5"),
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn timestamp_rounds_to_naive() {
        let ts = OracleTimestamp::new_timestamp(2026, 1, 2, 3, 4, 5, 123_456_789);
        match timestamp_value(ts) {
            Value::Timestamp(value) => {
                assert_eq!(
                    value,
                    NaiveDate::from_ymd_opt(2026, 1, 2)
                        .unwrap()
                        .and_hms_nano_opt(3, 4, 5, 123_456_789)
                        .unwrap()
                );
            }
            other => panic!("expected Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn timestamp_tz_applies_offset() {
        let ts = OracleTimestamp::new_timestamp_tz(2026, 1, 2, 11, 4, 5, 0, 8, 0);
        match timestamp_tz_value(ts) {
            Value::TimestampTz(value) => {
                assert_eq!(value, Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap());
            }
            other => panic!("expected TimestampTz, got {other:?}"),
        }
    }

    #[test]
    fn connect_string_variants() {
        let empty = BTreeMap::new();
        assert_eq!(
            sql::build_connect_string(Some("db"), Some(1522), Some("ORCL"), &empty),
            "db:1522/ORCL"
        );
        assert_eq!(
            sql::build_connect_string(Some("db"), None, None, &empty),
            "db:1521"
        );
        let mut params = BTreeMap::new();
        params.insert("sid".into(), "XE".into());
        assert!(
            sql::build_connect_string(Some("db"), Some(1521), None, &params).contains("SID=XE")
        );
    }

    #[test]
    fn paginate_uses_rownum_window() {
        let sql_text = sql::paginate("SELECT * FROM t", 20, 40);
        assert!(sql_text.contains("ROWNUM <= 60"));
        assert!(sql_text.contains("rustdb_rnum > 40"));
    }
}
