//! SQL Server 驱动（M3）：TDS 协议（`tiberius`）+ `sys.*` 目录视图元数据。
//!
//! 能力概览：
//! - 连接 / `SELECT 1` / 单批多语句（`simple_query` 发 SQL batch）
//! - 事务：TDS Transaction Manager 请求（`begin_transaction` / `commit_transaction` / `rollback_transaction`）
//! - 切换数据库：`USE [db]`；schema 只记录（T-SQL 没有会话级默认 schema 开关）
//! - 取消：查询执行期间监听 `watch` 通道，收到取消即先丢弃结果流、再发 TDS
//!   `Attention`（`Client::cancel_query`）—— 见 `MssqlSession::cancel` 注释
//! - 元数据：`sys.schemas` / `sys.objects` / `sys.columns` / `sys.indexes` / `sys.foreign_keys`

use async_trait::async_trait;
use futures_util::StreamExt;
use parking_lot::RwLock;
use rustdb_db_api::{
    dialect::{quote_bracket, quote_string_literal},
    CancelStyle, Capabilities, ColumnInfo, ColumnMeta, DbKind, Dialect, DriverError,
    DriverErrorKind, DriverFactory, ExecuteRequest, ExplainStyle, ForeignKeyInfo, IndexInfo,
    JsonStyle, LogicalType, MetadataProvider, Namespace, ObjectKind, ObjectKindMask, ObjectPath,
    ObjectSummary, QueryHandle, QueryId, Row, ServerInfo, Session, SessionId, TableDetail,
    TriggerInfo, TxId, TxOptions, UpsertStyle, Value,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiberius::{AuthMethod, Client, ColumnType, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio::sync::{watch, Mutex};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type TdsClient = Client<Compat<TcpStream>>;

/// 单元结构体：`app` 的注册表按值注册（`drivers.register(MssqlFactory)`）。
pub struct MssqlFactory;

impl Default for MssqlFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl MssqlFactory {
    pub fn new() -> Self {
        Self
    }
}

/// 静态能力矩阵；连接后可据服务端版本修正。
fn capabilities() -> Capabilities {
    let mut caps = Capabilities::minimal();
    caps.multi_statement = true;
    caps.returning = true; // OUTPUT 子句
    caps.cte = true;
    caps.window_functions = true;
    caps.upsert = UpsertStyle::Merge;
    caps.cancel = CancelStyle::Attention;
    caps.explain = ExplainStyle::ShowplanXml;
    caps.ddl_transactional = true;
    caps.schemas = true;
    caps.multiple_catalogs = true;
    caps.json = JsonStyle::JsonFunctions;
    caps.sequences = true;
    caps.max_ident_len = 128;
    caps.read_only_txn = false; // T-SQL 无只读事务
                                // 结果先物化成一个 RowBatch 再交回（见 MssqlSession::execute）。
    caps.streaming_rows = false;
    caps.dml_preview = true; // F-63
    caps.routine_source_edit = true; // F-82：视图 / 过程源码
    caps.routine_dependencies = true; // F-83：sys.sql_expression_dependencies
    caps.foreign_key_pivot = true; // sys.foreign_keys 已实现（F-73 跳转有据可依）
    caps
}

#[async_trait]
impl DriverFactory for MssqlFactory {
    fn kind(&self) -> DbKind {
        DbKind::Mssql
    }

    fn display_name(&self) -> &'static str {
        "SQL Server"
    }

    fn baseline_capabilities(&self) -> Capabilities {
        capabilities()
    }

    async fn connect(
        &self,
        opts: &rustdb_db_api::ConnectOptions,
    ) -> Result<Box<dyn Session>, DriverError> {
        let mut config = Config::new();
        config.host(opts.host.as_deref().unwrap_or("127.0.0.1"));
        config.port(opts.port.unwrap_or(1433));
        if let Some(db) = &opts.database {
            config.database(db);
        }
        config.application_name("RustDb");
        config.authentication(AuthMethod::sql_server(
            opts.username.as_deref().unwrap_or("sa"),
            opts.secret.as_deref().unwrap_or(""),
        ));
        // 实例名（可选）：命名实例走 SQL Browser 解析。
        if let Some(instance) = opts.params.get("instance") {
            config.instance_name(instance);
        }
        // ssl_mode: disable / prefer(默认，登录段加密) / require / strict。
        match opts.ssl_mode.as_deref() {
            Some("disable") | Some("off") => {
                config.encryption(EncryptionLevel::NotSupported);
            }
            Some("require") | Some("required") => {
                // 自签证书的常见场景：加密但不校验证书链。
                config.encryption(EncryptionLevel::Required);
                config.trust_cert();
            }
            Some("strict") => {
                config.encryption(EncryptionLevel::Strict);
            }
            _ => {
                config.encryption(EncryptionLevel::Off);
                config.trust_cert();
            }
        }

        let addr = config.get_addr();
        let timeout = Duration::from_millis(opts.connect_timeout_ms.max(1000));
        let tcp = tokio::time::timeout(timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Timeout,
                    format!("连接 SQL Server 超时（{addr}，{} ms）", timeout.as_millis()),
                )
            })?
            .map_err(|e| DriverError::connection(format!("连接 {addr} 失败：{e}")))?;
        tcp.set_nodelay(true).ok();

        let client = tokio::time::timeout(timeout, Client::connect(config, tcp.compat_write()))
            .await
            .map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Timeout,
                    format!("TDS 登录超时（{addr}，{} ms）", timeout.as_millis()),
                )
            })?
            .map_err(map_tds_error)?;

        let client = Arc::new(Mutex::new(client));

        // 服务端信息：版本 / 当前库 / 当前 schema。
        let mut server = ServerInfo::new("SQL Server");
        let mut current_schema: Option<String> = None;
        if let Ok(rows) = fetch_rows(client.clone(), sql::server_info_sql().to_string()).await {
            if let Some(row) = rows.first() {
                let version = text(row, 0);
                if !version.is_empty() {
                    server.version = version;
                }
                let db = text(row, 1);
                if !db.is_empty() {
                    server.current_db = Some(db);
                }
                let schema = text(row, 2);
                if !schema.is_empty() {
                    current_schema = Some(schema);
                }
            }
        }
        server.charset = Some("UTF-16LE".into());
        server.max_ident_len = 128;
        server.read_only = opts.read_only;

        let current_db = server.current_db.clone().unwrap_or_default();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let caps = capabilities();
        Ok(Box::new(MssqlSession {
            id: SessionId::new(),
            client: client.clone(),
            server,
            caps,
            dialect: Arc::new(MssqlDialect),
            meta: MssqlMetadata {
                client,
                current_db: RwLock::new(current_db),
                default_schema: RwLock::new(current_schema.clone()),
            },
            cancel_tx,
            cancel_rx: parking_lot::Mutex::new(cancel_rx),
            running: Arc::new(AtomicBool::new(false)),
            active_query: parking_lot::Mutex::new(None),
        }))
    }
}

struct MssqlDialect;

impl Dialect for MssqlDialect {
    fn kind(&self) -> DbKind {
        DbKind::Mssql
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_bracket(ident)
    }

    fn placeholder(&self, index: usize) -> String {
        format!("@P{index}")
    }

    /// SQL Server 2012+ 的分页写法；`ORDER BY` 是 `OFFSET … FETCH` 的语法前提，
    /// 调用方给的 SQL 必须自带 `ORDER BY`（sql-lang 生成分页语句时保证）。
    fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
        sql::paginate(sql, limit, offset)
    }

    fn order_clause(&self, column: &str, descending: bool, nulls_last: bool) -> String {
        let direction = if descending { "DESC" } else { "ASC" };
        // T-SQL 没有 NULLS FIRST/LAST；用 CASE 显式把 NULL 放到要求的位置。
        let null_rank = if nulls_last {
            "CASE WHEN {0} IS NULL THEN 1 ELSE 0 END ASC"
        } else {
            "CASE WHEN {0} IS NULL THEN 0 ELSE 1 END ASC"
        };
        let quoted = self.quote_ident(column);
        format!(
            "{}, {quoted} {direction}",
            null_rank.replace("{0}", &quoted)
        )
    }

    fn explain_sql(&self, sql: &str, analyze: bool) -> String {
        if analyze {
            // 真正执行并返回统计信息。
            format!("SET STATISTICS PROFILE ON; {sql}; SET STATISTICS PROFILE OFF")
        } else {
            format!("SET SHOWPLAN_XML ON; {sql}; SET SHOWPLAN_XML OFF")
        }
    }
}

struct MssqlSession {
    id: SessionId,
    client: Arc<Mutex<TdsClient>>,
    server: ServerInfo,
    caps: Capabilities,
    dialect: Arc<dyn Dialect>,
    meta: MssqlMetadata,
    cancel_tx: watch::Sender<bool>,
    /// 取消监听端：每轮查询开始时 clone 一份（`changed()` 需要 `&mut`，而
    /// `Session::execute` 只有 `&self`），锁只在 clone 瞬间持有。
    cancel_rx: parking_lot::Mutex<watch::Receiver<bool>>,
    running: Arc<AtomicBool>,
    /// 当前查询 id；取消必须与它匹配，避免迟到取消影响下一条语句。
    active_query: parking_lot::Mutex<Option<QueryId>>,
}

#[async_trait]
impl Session for MssqlSession {
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
        let mut client = self.client.lock().await;
        let stream = client
            .simple_query(sql::select_one())
            .await
            .map_err(map_tds_error)?;
        stream.into_first_result().await.map_err(map_tds_error)?;
        Ok(started.elapsed())
    }

    async fn execute(&self, req: ExecuteRequest) -> Result<QueryHandle, DriverError> {
        let query_id = req.query_id.clone().unwrap_or_else(QueryId::new);
        let max_rows = req.fetch.max_rows.max(1) as usize;

        // 清掉上一轮可能残留的取消信号。
        self.cancel_tx.send_replace(false);
        self.running.store(true, Ordering::SeqCst);
        *self.active_query.lock() = Some(query_id.clone());

        let outcome = self.run_query(&req.sql, max_rows).await;

        *self.active_query.lock() = None;
        self.running.store(false, Ordering::SeqCst);
        let (columns, rows, affected, truncated) = outcome?;

        let has_columns = !columns.is_empty();
        let column_meta: Arc<[ColumnMeta]> = columns
            .iter()
            .map(|column| ColumnMeta {
                name: Arc::from(column.name()),
                logical_type: sql::logical_from_column_type(column.column_type()),
                native_type: sql::native_type_name(column.column_type()).to_string(),
                // TDS 的列元数据不带 nullability，结果集里一律按可空展示。
                nullable: true,
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

    /// TDS 的取消是「在连接上补发 `Attention` 报文」。
    ///
    /// `tiberius::Client::cancel_query()` 需要 `&mut Client`，而查询期间 `Client`
    /// 正被结果流借用，所以不能从旁路线程直接调用。这里的做法是**协作式**：
    /// `cancel()` 只置位 `watch` 通道；`execute()` 在流式读取每一行时同时监听它，
    /// 一旦触发就先 `drop(stream)` 释放借用，再调用 `cancel_query()` 真正发
    /// `Attention`（由服务端确认后丢弃剩余结果）。
    ///
    /// 因此这里不返回 `Unsupported`。已知边界：置位到下一轮 `select!` 之间有一个
    /// 轮询间隔，取消是「尽快中断」而不是硬中断；另外 `Attention` 只能在语句处于
    /// 可中断状态时被服务端确认，回滚/提交等原子操作仍会跑完。
    async fn cancel(&self, query_id: &QueryId) -> Result<(), DriverError> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(DriverError::internal("当前没有正在执行的查询"));
        }
        let active = self.active_query.lock().clone();
        if active.as_ref() != Some(query_id) {
            return Err(DriverError::internal("该查询已结束，无需取消"));
        }
        self.cancel_tx.send_replace(true);
        Ok(())
    }

    async fn begin(&self, _opts: TxOptions) -> Result<TxId, DriverError> {
        // 走 TDS Transaction Manager 请求，后续语句自动带上事务描述符。
        // T-SQL 没有只读事务（`read_only` 被忽略，能力矩阵里 read_only_txn = false）。
        let mut client = self.client.lock().await;
        client.begin_transaction().await.map_err(map_tds_error)?;
        Ok(TxId::new())
    }

    async fn commit(&self, _tx: TxId) -> Result<(), DriverError> {
        let mut client = self.client.lock().await;
        client.commit_transaction().await.map_err(map_tds_error)
    }

    async fn rollback(&self, _tx: TxId) -> Result<(), DriverError> {
        let mut client = self.client.lock().await;
        client.rollback_transaction().await.map_err(map_tds_error)
    }

    async fn use_namespace(&self, ns: &Namespace) -> Result<(), DriverError> {
        if let Some(catalog) = &ns.catalog {
            let sql = sql::use_database_sql(catalog);
            let mut client = self.client.lock().await;
            client
                .simple_query(sql)
                .await
                .map_err(map_tds_error)?
                .into_first_result()
                .await
                .map_err(map_tds_error)?;
            self.meta.set_current_db(catalog.to_string());
            log::debug!("SQL Server 已切换到数据库 {catalog}");
        }
        if let Some(schema) = &ns.schema {
            // T-SQL 没有 `SET SCHEMA`：会话默认 schema 由登录名的 DEFAULT_SCHEMA
            // 决定，改它需要 ALTER USER（跨会话副作用）。这里只记录，用于元数据
            // 默认值；不带 schema 前缀的对象名仍按登录名默认 schema 解析。
            *self.meta.default_schema.write() = Some(schema.to_string());
        }
        Ok(())
    }

    fn metadata(&self) -> &dyn MetadataProvider {
        &self.meta
    }
}

impl MssqlSession {
    /// 执行一次 SQL batch，返回（列定义, 行, 影响行数, 是否截断）。
    async fn run_query(
        &self,
        sql: &str,
        max_rows: usize,
    ) -> Result<(Vec<tiberius::Column>, Vec<Row>, u64, bool), DriverError> {
        let mut client = self.client.lock().await;
        let mut stream = client.simple_query(sql).await.map_err(map_tds_error)?;
        let mut cancel_rx = self.cancel_rx.lock().clone();

        let mut columns: Vec<tiberius::Column> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut affected: u64 = 0;
        let mut truncated = false;
        let mut cancelled = false;

        loop {
            tokio::select! {
                biased;
                changed = cancel_rx.changed() => {
                    // 发送端被销毁（会话回收）时 `changed()` 会立即返回 Err，
                    // 必须跳出，否则会变成忙循环。
                    if changed.is_err() {
                        break;
                    }
                    if *cancel_rx.borrow() {
                        cancelled = true;
                        break;
                    }
                    // 收到的是「复位」通知（非取消），继续读结果。
                }
                item = stream.next() => {
                    match item {
                        None => break,
                        Some(Ok(QueryItem::Metadata(meta))) => {
                            if columns.is_empty() {
                                columns = meta.columns().to_vec();
                            }
                        }
                        Some(Ok(QueryItem::Row(row))) => {
                            if rows.len() >= max_rows {
                                truncated = true;
                                continue;
                            }
                            if columns.is_empty() {
                                columns = row.columns().to_vec();
                            }
                            rows.push(convert_row(&row));
                        }
                        Some(Err(err)) => return Err(map_tds_error(err)),
                    }
                }
            }
        }

        // 先释放结果流对 `client` 的独占借用，才能发送 `Attention`。
        drop(stream);
        if cancelled {
            client
                .cancel_query()
                .await
                .map_err(|e| DriverError::protocol(format!("发送 Attention 失败：{e}")))?;
            return Err(DriverError::cancelled());
        }
        // DML 的影响行数：simple_query 不带 DoneToken 计数，用 @@ROWCOUNT 补一次。
        if columns.is_empty() {
            affected = self.row_count_locked(&mut client).await.unwrap_or(0);
        }
        Ok((columns, rows, affected, truncated))
    }

    async fn row_count_locked(&self, client: &mut TdsClient) -> Result<u64, DriverError> {
        let stream = client
            .simple_query("SELECT @@ROWCOUNT")
            .await
            .map_err(map_tds_error)?;
        let rows = stream.into_first_result().await.map_err(map_tds_error)?;
        let count = rows
            .first()
            .and_then(|row| row.try_get::<i32, _>(0).ok().flatten())
            .unwrap_or(0);
        Ok(count.max(0) as u64)
    }
}

struct MssqlMetadata {
    client: Arc<Mutex<TdsClient>>,
    current_db: RwLock<String>,
    /// 元数据查询的默认 schema（`objects()` 未显式给 schema 时用）。
    default_schema: RwLock<Option<String>>,
}

impl MssqlMetadata {
    fn set_current_db(&self, db: String) {
        *self.current_db.write() = db;
    }

    fn schema_or_default(&self, ns: &Namespace) -> String {
        if let Some(schema) = &ns.schema {
            return schema.to_string();
        }
        if let Some(schema) = self.default_schema.read().clone() {
            return schema;
        }
        "dbo".to_string()
    }
}

#[async_trait]
impl MetadataProvider for MssqlMetadata {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError> {
        let rows = fetch_rows(self.client.clone(), sql::namespaces_sql().to_string()).await?;
        let current_db = self.current_db.read().clone();
        Ok(rows
            .into_iter()
            .map(|row| Namespace {
                catalog: Some(Arc::from(current_db.as_str())),
                schema: Some(Arc::from(text(&row, 0).as_str())),
            })
            .collect())
    }

    async fn objects(
        &self,
        ns: &Namespace,
        kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError> {
        let schema = self.schema_or_default(ns);
        let mut out = Vec::new();

        let rows = fetch_rows(self.client.clone(), sql::objects_sql(&schema)).await?;
        for row in rows {
            let name = text(&row, 0);
            let type_code = text(&row, 1);
            let Some(kind) = sql::object_kind_from_code(&type_code) else {
                continue;
            };
            if !kinds.contains(kind) {
                continue;
            }
            out.push(ObjectSummary {
                path: ObjectPath::new(ns.clone(), name, kind),
                row_estimate: None,
                comment: None,
                has_primary_key: None,
            });
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
        let schema = self.schema_or_default(&path.ns);
        let table = path.name.to_string();
        let rows = fetch_rows(self.client.clone(), sql::columns_sql(&schema, &table)).await?;

        let mut out = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            let name = text(&row, 0);
            let type_name = text(&row, 1);
            let max_length = int(&row, 2);
            let precision = int(&row, 3) as u8;
            let scale = int(&row, 4) as u8;
            let nullable = flag(&row, 5);
            let default = {
                let value = text(&row, 6);
                if value.is_empty() {
                    None
                } else {
                    Some(value)
                }
            };
            let identity = flag(&row, 7);

            let logical = sql::logical_from_type_name(&type_name, precision, scale, max_length);
            let mut col = ColumnInfo::simple(name, logical, index as u32 + 1);
            col.meta.native_type = sql::native_type_label(&type_name, precision, scale, max_length);
            col.meta.nullable = nullable;
            col.default = default;
            col.is_auto_increment = identity;
            out.push(col);
        }
        Ok(out)
    }

    async fn indexes(&self, path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        let schema = self.schema_or_default(&path.ns);
        let table = path.name.to_string();
        let rows = fetch_rows(self.client.clone(), sql::indexes_sql(&schema, &table)).await?;

        let mut grouped: Vec<IndexInfo> = Vec::new();
        for row in rows {
            let name = text(&row, 0);
            let unique = flag(&row, 1);
            let primary = flag(&row, 2);
            let method = text(&row, 3);
            let column = text(&row, 4);
            match grouped.iter_mut().find(|i| i.name == name) {
                Some(existing) => existing.columns.push(column),
                None => grouped.push(IndexInfo {
                    name,
                    columns: vec![column],
                    unique,
                    primary,
                    method: Some(method),
                }),
            }
        }
        Ok(grouped)
    }

    async fn foreign_keys(&self, path: &ObjectPath) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        let schema = self.schema_or_default(&path.ns);
        let table = path.name.to_string();
        let rows = fetch_rows(self.client.clone(), sql::foreign_keys_sql(&schema, &table)).await?;

        let mut grouped: Vec<ForeignKeyInfo> = Vec::new();
        for row in rows {
            let name = text(&row, 0);
            let column = text(&row, 1);
            let ref_table = text(&row, 2);
            let ref_column = text(&row, 3);
            let on_delete = text(&row, 4);
            let on_update = text(&row, 5);
            match grouped.iter_mut().find(|f| f.name == name) {
                Some(existing) => {
                    existing.columns.push(column);
                    existing.referenced_columns.push(ref_column);
                }
                None => grouped.push(ForeignKeyInfo {
                    name,
                    columns: vec![column],
                    referenced_table: ref_table,
                    referenced_columns: vec![ref_column],
                    on_delete: Some(on_delete),
                    on_update: Some(on_update),
                }),
            }
        }
        Ok(grouped)
    }

    /// Table triggers (`sys.triggers` + `sys.trigger_events`).
    async fn triggers(&self, path: &ObjectPath) -> Result<Option<Vec<TriggerInfo>>, DriverError> {
        let schema = self.schema_or_default(&path.ns);
        let table = path.name.to_string();
        let rows = fetch_rows(self.client.clone(), sql::triggers_sql(&schema, &table)).await?;
        Ok(Some(
            rows.into_iter()
                .map(|row| TriggerInfo {
                    name: text(&row, 0),
                    timing: Some(text(&row, 1)),
                    event: Some(text(&row, 2)),
                    statement: Some(text(&row, 3)),
                })
                .collect(),
        ))
    }

    /// 直接依赖（F-83）：`sys.sql_expression_dependencies` 双向查询。
    async fn dependencies(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::DependencyList>, DriverError> {
        use rustdb_db_api::{DependencyInfo, DependencyList};
        if !matches!(
            path.kind,
            ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::Procedure
                | ObjectKind::Function
                | ObjectKind::Trigger
                | ObjectKind::Sequence
                | ObjectKind::Synonym
        ) {
            return Ok(None);
        }
        let schema = self.schema_or_default(&path.ns);
        let name = path.name.to_string();
        let rows = fetch_rows(self.client.clone(), sql::dependencies_sql(&schema, &name)).await?;
        let mut out = DependencyList::default();
        for row in rows {
            let Some(kind) = sql::object_kind_from_code(&text(&row, 3)) else {
                continue;
            };
            let ns = text(&row, 1);
            let object_name = text(&row, 2);
            if object_name.is_empty() || ns == "sys" || ns == "INFORMATION_SCHEMA" {
                continue;
            }
            let dep = text(&row, 0) == "dep";
            let info = DependencyInfo {
                path: ObjectPath::new(
                    Namespace::new(path.ns.catalog.clone(), Some(ns.clone())),
                    object_name,
                    kind,
                ),
                dependency_type: if dep { "引用" } else { "被引用" }.into(),
                remote: (ns != schema).then_some(ns),
            };
            if dep {
                out.push_depends_on(info);
            } else {
                out.push_depended_by(info);
            }
        }
        out.note = Some("仅直接依赖（sys.sql_expression_dependencies），跨库引用不可见".into());
        Ok(Some(out))
    }

    async fn table(&self, path: &ObjectPath) -> Result<TableDetail, DriverError> {
        let mut columns = self.columns(path).await?;
        let indexes = self.indexes(path).await.unwrap_or_default();
        let foreign_keys = self.foreign_keys(path).await.unwrap_or_default();
        // 主键列：索引里带 is_primary_key 的那些列（列信息里没有这一位）。
        let pk_columns: Vec<String> = indexes
            .iter()
            .filter(|index| index.primary)
            .flat_map(|index| index.columns.clone())
            .collect();
        for column in columns.iter_mut() {
            if pk_columns
                .iter()
                .any(|name| name.eq_ignore_ascii_case(column.meta.name.as_ref()))
            {
                column.is_primary_key = true;
            }
        }
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
            foreign_keys,
            checks: Vec::new(),
            triggers,
            engine: None,
            collation: None,
            comment: None,
            ddl_source: rustdb_db_api::DdlSource::Synthesized,
        })
    }

    /// SQL Server 没有「取建表语句」的内置函数（`SCRIPT` 之类要 SMO 或 SQL 项目），
    /// 因此按元数据合成 `CREATE TABLE`，并显式标记 `DdlSource::Synthesized`。
    ///
    /// 例程（视图 / 过程 / 函数 / 触发器）走 `OBJECT_DEFINITION`：它只返回例程体，
    /// 没有 `CREATE` 头，保存时由 core 按方言补签名（F-82）。
    async fn object_ddl(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::ObjectDdl>, DriverError> {
        if path.kind != ObjectKind::Table {
            if !matches!(
                path.kind,
                ObjectKind::View
                    | ObjectKind::MaterializedView
                    | ObjectKind::Procedure
                    | ObjectKind::Function
                    | ObjectKind::Trigger
            ) {
                return Ok(None);
            }
            let schema = self.schema_or_default(&path.ns);
            let query = sql::routine_source_sql(&schema, &path.name);
            let rows = fetch_rows(self.client.clone(), query).await?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let text = text(row, 0);
            if text.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(rustdb_db_api::ObjectDdl {
                text,
                source: rustdb_db_api::DdlSource::Native,
            }));
        }
        let columns = self.columns(path).await?;
        if columns.is_empty() {
            return Ok(None);
        }
        let indexes = self.indexes(path).await.unwrap_or_default();
        let schema = self.schema_or_default(&path.ns);
        let text = sql::synthesize_create_table(&schema, &path.name, &columns, &indexes);
        Ok(Some(rustdb_db_api::ObjectDdl {
            text,
            source: rustdb_db_api::DdlSource::Synthesized,
        }))
    }
}

async fn fetch_rows(
    client: Arc<Mutex<TdsClient>>,
    sql: String,
) -> Result<Vec<tiberius::Row>, DriverError> {
    let mut client = client.lock().await;
    let stream = client.simple_query(sql).await.map_err(map_tds_error)?;
    stream.into_first_result().await.map_err(map_tds_error)
}

fn text(row: &tiberius::Row, idx: usize) -> String {
    row.try_get::<&str, _>(idx)
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn int(row: &tiberius::Row, idx: usize) -> i32 {
    row.try_get::<i32, _>(idx)
        .ok()
        .flatten()
        .or_else(|| row.try_get::<i16, _>(idx).ok().flatten().map(|v| v as i32))
        .or_else(|| row.try_get::<u8, _>(idx).ok().flatten().map(|v| v as i32))
        .or_else(|| row.try_get::<i64, _>(idx).ok().flatten().map(|v| v as i32))
        .unwrap_or(0)
}

fn flag(row: &tiberius::Row, idx: usize) -> bool {
    row.try_get::<bool, _>(idx).ok().flatten().unwrap_or(false)
}

fn convert_row(row: &tiberius::Row) -> Row {
    row.cells()
        .enumerate()
        .map(|(idx, (column, data))| convert_cell(row, idx, column.column_type(), data))
        .collect()
}

fn convert_cell(
    row: &tiberius::Row,
    idx: usize,
    column_type: ColumnType,
    data: &tiberius::ColumnData<'_>,
) -> Value {
    use tiberius::ColumnData as D;
    match data {
        D::U8(Some(v)) => Value::Int(*v as i64),
        D::I16(Some(v)) => Value::Int(*v as i64),
        D::I32(Some(v)) => Value::Int(*v as i64),
        D::I64(Some(v)) => Value::Int(*v),
        D::F32(Some(v)) => Value::Float(*v as f64),
        D::F64(Some(v)) => Value::Float(*v),
        D::Bit(Some(v)) => Value::Bool(*v),
        D::String(Some(v)) => Value::Text(Arc::from(v.as_ref())),
        D::Guid(Some(v)) => Value::Text(Arc::from(v.to_string().as_str())),
        D::Binary(Some(v)) => Value::Bytes(Arc::from(v.as_ref())),
        D::Numeric(Some(n)) => Value::Decimal(Arc::from(
            sql::numeric_to_string(n.int_part(), n.scale()).as_str(),
        )),
        D::Xml(Some(v)) => Value::Text(Arc::from(v.to_string().as_str())),
        D::DateTime(_) | D::SmallDateTime(_) | D::DateTime2(_) => row
            .try_get::<chrono::NaiveDateTime, _>(idx)
            .ok()
            .flatten()
            .map(Value::Timestamp)
            .unwrap_or(Value::Null),
        D::Date(_) => row
            .try_get::<chrono::NaiveDate, _>(idx)
            .ok()
            .flatten()
            .map(Value::Date)
            .unwrap_or(Value::Null),
        D::Time(_) => row
            .try_get::<chrono::NaiveTime, _>(idx)
            .ok()
            .flatten()
            .map(Value::Time)
            .unwrap_or(Value::Null),
        D::DateTimeOffset(_) => row
            .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
            .ok()
            .flatten()
            .map(Value::TimestampTz)
            .unwrap_or(Value::Null),
        // NULL（含所有 `Some` 未覆盖的变体里的 None 分支）。
        _ => {
            let _ = column_type;
            Value::Null
        }
    }
}

fn map_tds_error(err: tiberius::error::Error) -> DriverError {
    use tiberius::error::Error;
    match err {
        Error::Server(server) => {
            let code = server.code();
            let state = server.state();
            let kind = match code {
                18456 | 18452 => DriverErrorKind::Auth,
                229 | 230 | 262 => DriverErrorKind::Permission,
                208 | 207 | 2812 => DriverErrorKind::NotFound,
                102 | 105 | 156 => DriverErrorKind::Syntax,
                2627 | 2601 | 547 => DriverErrorKind::Constraint,
                1205 => DriverErrorKind::Deadlock,
                1222 => DriverErrorKind::Timeout,
                3906 => DriverErrorKind::ReadOnly,
                4060 => DriverErrorKind::Connection,
                _ => DriverErrorKind::Protocol,
            };
            let mut mapped =
                DriverError::new(kind, server.message().to_string()).with_code(code as i64);
            // TDS 的 State 不是 SQLSTATE，这里按 5 位数字保留原值便于检索。
            mapped = mapped.with_sqlstate(format!("{state:05}"));
            mapped
        }
        Error::Io { message, .. } => DriverError::connection(message),
        other => DriverError::protocol(other.to_string()),
    }
}

/// 纯函数区：SQL 生成 + 类型映射。全部可单测（不碰网络）。
pub mod sql {
    use super::*;

    pub fn select_one() -> &'static str {
        "SELECT 1"
    }

    pub fn server_info_sql() -> &'static str {
        "SELECT @@VERSION AS version, DB_NAME() AS db, SCHEMA_NAME() AS schema"
    }

    /// 当前库里的用户 schema 列表（`sys.schemas`，排除系统 schema）。
    pub fn namespaces_sql() -> &'static str {
        "SELECT s.name \
         FROM sys.schemas AS s \
         WHERE s.schema_id < 16384 \
           AND s.name NOT IN ('sys', 'INFORMATION_SCHEMA', 'guest') \
         ORDER BY s.name"
    }

    pub fn use_database_sql(db: &str) -> String {
        format!("USE {}", quote_bracket(db))
    }

    /// 对象列表：U=表 V=视图 P=存储过程 FN/IF/TF=函数 SO=序列 TR=触发器 SN=同义词
    pub fn objects_sql(schema: &str) -> String {
        format!(
            "SELECT o.name, RTRIM(o.type) AS type_code \
             FROM sys.objects AS o \
             JOIN sys.schemas AS s ON s.schema_id = o.schema_id \
             WHERE s.name = {schema} \
               AND o.type IN ('U', 'V', 'P', 'FN', 'IF', 'TF', 'SO', 'TR', 'SN') \
               AND o.is_ms_shipped = 0 \
             ORDER BY o.type, o.name",
            schema = quote_string_literal(schema)
        )
    }

    /// 列：类型名 + 长度/精度/小数位 + 可空 + 默认值 + 自增。
    pub fn columns_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT c.name AS column_name, \
                    t.name AS type_name, \
                    c.max_length, \
                    CAST(c.precision AS int) AS precision, \
                    CAST(c.scale AS int) AS scale, \
                    c.is_nullable, \
                    dc.definition AS default_definition, \
                    c.is_identity \
             FROM sys.columns AS c \
             JOIN sys.types AS t ON t.user_type_id = c.user_type_id \
             LEFT JOIN sys.default_constraints AS dc \
                    ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id \
             WHERE c.object_id = OBJECT_ID({object}) \
             ORDER BY c.column_id",
            object = quote_string_literal(&format!("{schema}.{table}"))
        )
    }

    /// 索引：`sys.indexes` + `sys.index_columns`（按 key_ordinal 排）。
    pub fn indexes_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT i.name AS index_name, \
                    i.is_unique, \
                    i.is_primary_key, \
                    i.type_desc, \
                    c.name AS column_name \
             FROM sys.indexes AS i \
             JOIN sys.index_columns AS ic \
                  ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
             JOIN sys.columns AS c \
                  ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE i.object_id = OBJECT_ID({object}) \
               AND i.name IS NOT NULL \
               AND ic.is_included_column = 0 \
             ORDER BY i.name, ic.key_ordinal",
            object = quote_string_literal(&format!("{schema}.{table}"))
        )
    }

    /// 外键：`sys.foreign_keys` + `sys.foreign_key_columns`，含删除/更新规则。
    pub fn foreign_keys_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT fk.name AS fk_name, \
                    pc.name AS parent_column, \
                    rt.name AS referenced_table, \
                    rc.name AS referenced_column, \
                    fk.delete_referential_action_desc, \
                    fk.update_referential_action_desc \
             FROM sys.foreign_keys AS fk \
             JOIN sys.foreign_key_columns AS fkc \
                  ON fkc.constraint_object_id = fk.object_id \
             JOIN sys.tables AS pt ON pt.object_id = fk.parent_object_id \
             JOIN sys.schemas AS ps ON ps.schema_id = pt.schema_id \
             JOIN sys.columns AS pc \
                  ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN sys.tables AS rt ON rt.object_id = fk.referenced_object_id \
             JOIN sys.columns AS rc \
                  ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE ps.name = {schema} AND pt.name = {table} \
             ORDER BY fk.name, fkc.constraint_column_id",
            schema = quote_string_literal(schema),
            table = quote_string_literal(table)
        )
    }

    /// 触发器：`sys.triggers` + `sys.trigger_events`（事件列可能为空，如 DDL 触发器）。
    pub fn triggers_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT tr.name AS trigger_name, \
                    CASE WHEN tr.is_instead_of_trigger = 1 THEN 'INSTEAD OF' \
                         WHEN tr.is_disabled = 1 THEN 'DISABLED' ELSE 'AFTER' END AS timing, \
                    ISNULL(te.type_desc, '') AS event_desc, \
                    ISNULL(object_definition(tr.object_id), '') AS definition \
             FROM sys.triggers AS tr \
             JOIN sys.tables AS tb ON tb.object_id = tr.parent_id \
             JOIN sys.schemas AS sc ON sc.schema_id = tb.schema_id \
             LEFT JOIN sys.trigger_events AS te ON te.object_id = tr.object_id \
             WHERE sc.name = {schema} AND tb.name = {table} \
             ORDER BY tr.name",
            schema = quote_string_literal(schema),
            table = quote_string_literal(table)
        )
    }

    /// 分页：SQL Server 2012+ 的 `OFFSET … FETCH NEXT`（必须先有 `ORDER BY`）。
    pub fn paginate(sql: &str, limit: u64, offset: u64) -> String {
        format!("{sql} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
    }

    /// 对象依赖（F-83）：`sys.sql_expression_dependencies` 双向查询。
    ///
    /// 只覆盖同库引用；跨库引用（`referenced_id` 为空）查不到，由调用方给出提示。
    pub fn dependencies_sql(schema: &str, name: &str) -> String {
        format!(
            "SELECT DISTINCT 'dep' AS dir, sc.name AS schema_name, ob.name AS object_name, \
                    ob.type AS type_code \
             FROM sys.sql_expression_dependencies AS d \
             JOIN sys.objects AS ob ON ob.object_id = d.referenced_id \
             JOIN sys.schemas AS sc ON sc.schema_id = ob.schema_id \
             WHERE d.referencing_id = OBJECT_ID({object}) AND d.referenced_id IS NOT NULL \
             UNION ALL \
             SELECT DISTINCT 'rev', sc.name, ob.name, ob.type \
             FROM sys.sql_expression_dependencies AS d \
             JOIN sys.objects AS ob ON ob.object_id = d.referencing_id \
             JOIN sys.schemas AS sc ON sc.schema_id = ob.schema_id \
             WHERE d.referenced_id = OBJECT_ID({object}) \
             ORDER BY schema_name, object_name",
            object = quote_string_literal(&format!(
                "{}.{}",
                quote_bracket(schema),
                quote_bracket(name)
            ))
        )
    }

    pub fn object_kind_from_code(code: &str) -> Option<ObjectKind> {
        match code.trim() {
            "U" => Some(ObjectKind::Table),
            "V" => Some(ObjectKind::View),
            "P" => Some(ObjectKind::Procedure),
            "FN" | "IF" | "TF" => Some(ObjectKind::Function),
            "SO" => Some(ObjectKind::Sequence),
            "TR" => Some(ObjectKind::Trigger),
            "SN" => Some(ObjectKind::Synonym),
            _ => None,
        }
    }

    /// 元数据里的 T-SQL 类型名 → 逻辑类型。
    pub fn logical_from_type_name(
        name: &str,
        precision: u8,
        scale: u8,
        max_length: i32,
    ) -> LogicalType {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "bit" => LogicalType::Bool,
            "tinyint" => LogicalType::Int {
                bits: 8,
                unsigned: true,
            },
            "smallint" => LogicalType::Int {
                bits: 16,
                unsigned: false,
            },
            "int" => LogicalType::Int {
                bits: 32,
                unsigned: false,
            },
            "bigint" => LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            "real" => LogicalType::Float { bits: 32 },
            "float" => LogicalType::Float { bits: 64 },
            "decimal" | "numeric" | "money" | "smallmoney" => LogicalType::Decimal {
                precision: Some(precision),
                scale: Some(scale),
            },
            "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" => LogicalType::Text {
                // nchar/nvarchar 的 max_length 是字节数，字符数要除以 2；-1 表示 max。
                max_len: if max_length < 0 {
                    None
                } else if lower.starts_with('n') {
                    Some((max_length / 2) as u32)
                } else {
                    Some(max_length as u32)
                },
            },
            "binary" | "varbinary" | "image" | "rowversion" | "timestamp" => LogicalType::Bytes,
            "date" => LogicalType::Date,
            "time" => LogicalType::Time,
            "datetime" | "datetime2" | "smalldatetime" => LogicalType::Timestamp,
            "datetimeoffset" => LogicalType::TimestampTz,
            "uniqueidentifier" => LogicalType::Uuid,
            "xml" => LogicalType::Text { max_len: None },
            "sql_variant" => LogicalType::Unknown("sql_variant".into()),
            other => LogicalType::Unknown(other.to_string()),
        }
    }

    /// 网格/DDL 用的原生类型标签，例如 `nvarchar(50)`、`decimal(18,2)`。
    pub fn native_type_label(name: &str, precision: u8, scale: u8, max_length: i32) -> String {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "char" | "varchar" | "binary" | "varbinary" => {
                if max_length < 0 {
                    format!("{lower}(max)")
                } else {
                    format!("{lower}({max_length})")
                }
            }
            "nchar" | "nvarchar" => {
                if max_length < 0 {
                    format!("{lower}(max)")
                } else {
                    format!("{lower}({})", max_length / 2)
                }
            }
            "decimal" | "numeric" => format!("{lower}({precision},{scale})"),
            other => other.to_string(),
        }
    }

    /// TDS 结果集列类型 → 逻辑类型（结果集只有线类型，没有长度细节）。
    pub fn logical_from_column_type(column_type: ColumnType) -> LogicalType {
        match column_type {
            ColumnType::Bit | ColumnType::Bitn => LogicalType::Bool,
            ColumnType::Int1 => LogicalType::Int {
                bits: 8,
                unsigned: true,
            },
            ColumnType::Int2 => LogicalType::Int {
                bits: 16,
                unsigned: false,
            },
            ColumnType::Int4 | ColumnType::Intn => LogicalType::Int {
                bits: 32,
                unsigned: false,
            },
            ColumnType::Int8 => LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            ColumnType::Float4 => LogicalType::Float { bits: 32 },
            ColumnType::Float8 | ColumnType::Floatn => LogicalType::Float { bits: 64 },
            ColumnType::Money
            | ColumnType::Money4
            | ColumnType::Decimaln
            | ColumnType::Numericn => LogicalType::Decimal {
                precision: None,
                scale: None,
            },
            ColumnType::Guid => LogicalType::Uuid,
            ColumnType::Daten => LogicalType::Date,
            ColumnType::Timen => LogicalType::Time,
            ColumnType::Datetime
            | ColumnType::Datetime4
            | ColumnType::Datetimen
            | ColumnType::Datetime2 => LogicalType::Timestamp,
            ColumnType::DatetimeOffsetn => LogicalType::TimestampTz,
            ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image | ColumnType::Udt => {
                LogicalType::Bytes
            }
            ColumnType::Xml => LogicalType::Text { max_len: None },
            ColumnType::BigVarChar
            | ColumnType::BigChar
            | ColumnType::NVarchar
            | ColumnType::NChar
            | ColumnType::Text
            | ColumnType::NText => LogicalType::Text { max_len: None },
            ColumnType::Null | ColumnType::SSVariant => LogicalType::Unknown("sql_variant".into()),
        }
    }

    pub fn native_type_name(column_type: ColumnType) -> &'static str {
        match column_type {
            ColumnType::Bit | ColumnType::Bitn => "bit",
            ColumnType::Int1 => "tinyint",
            ColumnType::Int2 => "smallint",
            ColumnType::Int4 => "int",
            ColumnType::Int8 => "bigint",
            ColumnType::Intn => "int",
            ColumnType::Float4 => "real",
            ColumnType::Float8 | ColumnType::Floatn => "float",
            ColumnType::Money | ColumnType::Money4 => "money",
            ColumnType::Decimaln | ColumnType::Numericn => "decimal",
            ColumnType::Guid => "uniqueidentifier",
            ColumnType::Daten => "date",
            ColumnType::Timen => "time",
            ColumnType::Datetime | ColumnType::Datetime4 | ColumnType::Datetimen => "datetime",
            ColumnType::Datetime2 => "datetime2",
            ColumnType::DatetimeOffsetn => "datetimeoffset",
            ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Udt => "varbinary",
            ColumnType::Image => "image",
            ColumnType::Xml => "xml",
            ColumnType::BigVarChar => "varchar",
            ColumnType::BigChar => "char",
            ColumnType::NVarchar => "nvarchar",
            ColumnType::NChar => "nchar",
            ColumnType::Text => "text",
            ColumnType::NText => "ntext",
            ColumnType::Null | ColumnType::SSVariant => "sql_variant",
        }
    }

    /// `Numeric` 的内部表示（整数部分 + 小数位数）还原成精确十进制字符串。
    /// 金额/定点数绝不能过 f64，所以这里自己补小数点。
    pub fn numeric_to_string(int_part: i128, scale: u8) -> String {
        if scale == 0 {
            return int_part.to_string();
        }
        let negative = int_part < 0;
        let digits = int_part.abs().to_string();
        let scale = scale as usize;
        let padded = if digits.len() <= scale {
            format!("{}{}", "0".repeat(scale - digits.len() + 1), digits)
        } else {
            digits
        };
        let split = padded.len() - scale;
        let (int, frac) = padded.split_at(split);
        format!("{}{int}.{frac}", if negative { "-" } else { "" })
    }

    /// 按元数据合成 `CREATE TABLE`（SQL Server 无原生 DDL 导出接口）。
    /// 例程源码：`OBJECT_DEFINITION` 返回例程体（视图是 SELECT，过程是 AS 之后的块）。
    pub fn routine_source_sql(schema: &str, name: &str) -> String {
        format!(
            "select object_definition(object_id({})) as def",
            quote_string_literal(&format!(
                "{}.{}",
                quote_bracket(schema),
                quote_bracket(name)
            ))
        )
    }

    pub fn synthesize_create_table(
        schema: &str,
        table: &str,
        columns: &[ColumnInfo],
        indexes: &[IndexInfo],
    ) -> String {
        let mut lines: Vec<String> = Vec::with_capacity(columns.len() + indexes.len());
        for column in columns {
            let mut line = format!(
                "    {} {}",
                quote_bracket(&column.meta.name),
                column.meta.native_type
            );
            if column.is_auto_increment {
                line.push_str(" IDENTITY(1,1)");
            }
            if !column.meta.nullable {
                line.push_str(" NOT NULL");
            }
            if let Some(default) = &column.default {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            lines.push(line);
        }
        if let Some(primary) = indexes.iter().find(|index| index.primary) {
            let cols: Vec<String> = primary
                .columns
                .iter()
                .map(|column| quote_bracket(column))
                .collect();
            lines.push(format!(
                "    CONSTRAINT {} PRIMARY KEY ({})",
                quote_bracket(&primary.name),
                cols.join(", ")
            ));
        }
        for index in indexes.iter().filter(|index| !index.primary) {
            let cols: Vec<String> = index
                .columns
                .iter()
                .map(|column| quote_bracket(column))
                .collect();
            lines.push(format!(
                "    INDEX {} ({})",
                quote_bracket(&index.name),
                cols.join(", ")
            ));
        }
        format!(
            "CREATE TABLE {}.{} (\n{}\n);",
            quote_bracket(schema),
            quote_bracket(table),
            lines.join(",\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::sql;
    use super::MssqlFactory;
    use rustdb_db_api::{ColumnInfo, DbKind, Dialect, DriverFactory, IndexInfo, LogicalType};

    fn column(name: &str, native: &str, nullable: bool, identity: bool) -> ColumnInfo {
        let mut column = ColumnInfo::simple(name, LogicalType::Text { max_len: None }, 1);
        column.meta.native_type = native.to_string();
        column.meta.nullable = nullable;
        column.is_auto_increment = identity;
        column
    }

    #[test]
    fn paginate_uses_offset_fetch() {
        assert_eq!(
            sql::paginate("SELECT * FROM t ORDER BY id", 50, 100),
            "SELECT * FROM t ORDER BY id OFFSET 100 ROWS FETCH NEXT 50 ROWS ONLY"
        );
        assert!(sql::paginate("SELECT 1", 10, 0).contains("OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"));
    }

    #[test]
    fn identifiers_are_bracket_quoted_and_escaped() {
        assert_eq!(MssqlFactory::new().kind(), DbKind::Mssql);
        let dialect = super::MssqlDialect;
        assert_eq!(dialect.quote_ident("order"), "[order]");
        assert_eq!(dialect.quote_ident("we]ird"), "[we]]ird]");
        assert_eq!(dialect.placeholder(3), "@P3");
        assert_eq!(sql::use_database_sql("sales]db"), "USE [sales]]db]");
    }

    #[test]
    fn metadata_sql_hits_the_expected_catalog_views() {
        let objects = sql::objects_sql("dbo");
        assert!(objects.contains("sys.objects"));
        assert!(objects.contains("sys.schemas"));
        assert!(objects.contains("s.name = 'dbo'"));

        let columns = sql::columns_sql("dbo", "users");
        assert!(columns.contains("sys.columns"));
        assert!(columns.contains("sys.types"));
        assert!(columns.contains("sys.default_constraints"));
        assert!(columns.contains("OBJECT_ID('dbo.users')"));
        assert!(columns.contains("c.is_identity"));

        let indexes = sql::indexes_sql("dbo", "users");
        assert!(indexes.contains("sys.indexes"));
        assert!(indexes.contains("sys.index_columns"));
        assert!(indexes.contains("ORDER BY i.name, ic.key_ordinal"));

        let fks = sql::foreign_keys_sql("dbo", "users");
        assert!(fks.contains("sys.foreign_keys"));
        assert!(fks.contains("sys.foreign_key_columns"));
        assert!(fks.contains("delete_referential_action_desc"));

        // 单引号必须转义，避免拼接出坏 SQL。
        assert!(sql::columns_sql("d'bo", "t").contains("OBJECT_ID('d''bo.t')"));
    }

    #[test]
    fn dependency_sql_queries_both_directions() {
        let query = sql::dependencies_sql("dbo", "v_orders");
        assert!(query.contains("sys.sql_expression_dependencies"));
        assert!(query.contains("OBJECT_ID('[dbo].[v_orders]')"));
        assert!(query.contains("'dep'"));
        assert!(query.contains("'rev'"));
        assert!(query.contains("ORDER BY schema_name, object_name"));
    }

    #[test]
    fn type_mapping_covers_numbers_strings_and_times() {
        assert_eq!(
            sql::logical_from_type_name("int", 10, 0, 4),
            LogicalType::Int {
                bits: 32,
                unsigned: false
            }
        );
        assert_eq!(
            sql::logical_from_type_name("nvarchar", 0, 0, 100),
            LogicalType::Text { max_len: Some(50) }
        );
        assert_eq!(
            sql::logical_from_type_name("decimal", 18, 2, 9),
            LogicalType::Decimal {
                precision: Some(18),
                scale: Some(2)
            }
        );
        assert_eq!(
            sql::logical_from_type_name("datetimeoffset", 0, 0, 10),
            LogicalType::TimestampTz
        );
        assert_eq!(
            sql::logical_from_type_name("uniqueidentifier", 0, 0, 16),
            LogicalType::Uuid
        );
        assert_eq!(
            sql::native_type_label("nvarchar", 0, 0, 100),
            "nvarchar(50)"
        );
        assert_eq!(sql::native_type_label("decimal", 18, 2, 9), "decimal(18,2)");
        assert_eq!(sql::native_type_label("varchar", 0, 0, -1), "varchar(max)");
        assert_eq!(
            sql::object_kind_from_code("U"),
            Some(rustdb_db_api::ObjectKind::Table)
        );
        assert_eq!(
            sql::object_kind_from_code("SN"),
            Some(rustdb_db_api::ObjectKind::Synonym)
        );
        assert_eq!(sql::object_kind_from_code("XX"), None);
    }

    #[test]
    fn numeric_keeps_decimal_precision() {
        assert_eq!(sql::numeric_to_string(12345, 2), "123.45");
        assert_eq!(sql::numeric_to_string(-5, 2), "-0.05");
        assert_eq!(sql::numeric_to_string(7, 0), "7");
        assert_eq!(sql::numeric_to_string(1234567890123, 4), "123456789.0123");
    }

    #[test]
    fn synthesized_ddl_contains_columns_keys_and_indexes() {
        let columns = vec![
            column("id", "int", false, true),
            column("name", "nvarchar(50)", true, false),
        ];
        let indexes = vec![
            IndexInfo {
                name: "PK_users".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                method: Some("CLUSTERED".into()),
            },
            IndexInfo {
                name: "IX_users_name".into(),
                columns: vec!["name".into()],
                unique: false,
                primary: false,
                method: Some("NONCLUSTERED".into()),
            },
        ];
        let ddl = sql::synthesize_create_table("dbo", "users", &columns, &indexes);
        assert!(ddl.starts_with("CREATE TABLE [dbo].[users] ("));
        assert!(ddl.contains("[id] int IDENTITY(1,1) NOT NULL"));
        assert!(ddl.contains("[name] nvarchar(50)"));
        assert!(ddl.contains("CONSTRAINT [PK_users] PRIMARY KEY ([id])"));
        assert!(ddl.contains("INDEX [IX_users_name] ([name])"));
        assert!(ddl.ends_with(");"));
    }

    #[test]
    fn capabilities_declare_attention_cancel_and_merge_upsert() {
        let caps = MssqlFactory::new().baseline_capabilities();
        assert_eq!(caps.cancel, rustdb_db_api::CancelStyle::Attention);
        assert_eq!(caps.upsert, rustdb_db_api::UpsertStyle::Merge);
        assert!(caps.schemas);
        assert_eq!(caps.max_ident_len, 128);
    }
}
