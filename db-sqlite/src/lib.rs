//! SQLite 驱动（M0 垂直切片）。
//!
//! 实现范围：
//! * 连接（文件 / `:memory:`）、ping、单语句执行、取消、基础事务；
//! * 元数据：命名空间、表/视图、列、索引；
//! * 演示库：`:memory:` 首次连接自动建表并灌入示例数据。
//!
//! 已知限制（M0，见文档第 14 章）：
//! * 结果先全量收集再交回（`streaming_rows = false`），M2 改为通道流式；
//! * 类型推断按声明类型做粗映射，不做完整亲和性解析。

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use parking_lot::Mutex;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, InterruptHandle};
use rustdb_db_api::{
    build_page_sql, quote_string_literal, CancelStyle, Capabilities, ColumnInfo, ColumnMeta,
    DbKind, Dialect, DriverError, DriverErrorKind, DriverFactory, ExecuteRequest, ExplainStyle,
    ForeignKeyInfo, IndexInfo, IsolationLevel, JsonStyle, LogicalType, MetadataProvider, Namespace,
    ObjectKind, ObjectKindMask, ObjectPath, ObjectSummary, PageRequest, PageResult, QueryHandle,
    QueryId, Row, ServerInfo, Session, SessionId, TableDetail, TriggerInfo, TxId, TxOptions,
    UpsertStyle, Value,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct SqliteFactory {
    caps: Capabilities,
}

impl Default for SqliteFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl SqliteFactory {
    pub fn new() -> Self {
        let mut caps = Capabilities::minimal();
        caps.multi_statement = true;
        caps.cte = true;
        caps.window_functions = true;
        caps.upsert = UpsertStyle::OnConflict;
        caps.cancel = CancelStyle::Interrupt;
        caps.explain = ExplainStyle::QueryPlan;
        caps.ddl_transactional = true;
        caps.multiple_catalogs = true; // attached databases
        caps.json = JsonStyle::JsonFunctions;
        caps.max_ident_len = 255;
        caps.read_only_txn = true;
        caps.streaming_rows = false;
        caps.notices = false;
        caps.dml_preview = true; // F-63
        caps.routine_source_edit = true; // F-82：视图 / 触发器源码可读可改
        caps.server_side_filter = true; // X-02
        Self { caps }
    }
}

#[async_trait]
impl DriverFactory for SqliteFactory {
    fn kind(&self) -> DbKind {
        DbKind::Sqlite
    }

    fn display_name(&self) -> &'static str {
        "SQLite"
    }

    fn default_port(&self) -> Option<u16> {
        None
    }

    fn baseline_capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    async fn connect(
        &self,
        opts: &rustdb_db_api::driver::ConnectOptions,
    ) -> Result<Box<dyn Session>, DriverError> {
        let path = opts
            .database
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| ":memory:".to_string());
        let is_memory = path == ":memory:";
        let caps = self.caps.clone();
        let path_for_open = path.clone();

        let (conn, version) = tokio::task::spawn_blocking(move || -> Result<_, DriverError> {
            let conn = Connection::open(&path_for_open).map_err(map_sqlite_error)?;
            if is_memory {
                seed_demo(&conn).map_err(map_sqlite_error)?;
            }
            Ok((conn, rusqlite::version().to_string()))
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))??;

        let interrupt = conn.get_interrupt_handle();
        let mut server = ServerInfo::new(format!("SQLite {version}"));
        server.current_db = Some(if is_memory {
            "main (memory)".into()
        } else {
            path
        });
        server.max_ident_len = caps.max_ident_len;
        server.read_only = opts.read_only;

        Ok(Box::new(SqliteSession::new(conn, interrupt, server, caps)))
    }
}

struct SqliteDialect;

impl Dialect for SqliteDialect {
    fn kind(&self) -> DbKind {
        DbKind::Sqlite
    }

    fn quote_ident(&self, ident: &str) -> String {
        rustdb_db_api::dialect::quote_double(ident)
    }

    fn placeholder(&self, _index: usize) -> String {
        "?".into()
    }

    fn explain_sql(&self, sql: &str, _analyze: bool) -> String {
        format!("EXPLAIN QUERY PLAN {sql}")
    }
}

struct SqliteSession {
    id: SessionId,
    conn: Arc<Mutex<Connection>>,
    interrupt: InterruptHandle,
    server: ServerInfo,
    caps: Capabilities,
    dialect: Arc<dyn Dialect>,
    meta: SqliteMetadata,
    active: Mutex<Option<QueryId>>,
}

impl SqliteSession {
    fn new(
        conn: Connection,
        interrupt: InterruptHandle,
        server: ServerInfo,
        caps: Capabilities,
    ) -> Self {
        let conn = Arc::new(Mutex::new(conn));
        let meta = SqliteMetadata { conn: conn.clone() };
        Self {
            id: SessionId::new(),
            conn,
            interrupt,
            server,
            caps,
            dialect: Arc::new(SqliteDialect),
            meta,
            active: Mutex::new(None),
        }
    }
}

#[async_trait]
impl Session for SqliteSession {
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
        let conn = self.conn.clone();
        let started = Instant::now();
        tokio::task::spawn_blocking(move || -> Result<(), DriverError> {
            let conn = conn.lock();
            conn.query_row("SELECT 1", [], |_| Ok(()))
                .map_err(map_sqlite_error)
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))??;
        Ok(started.elapsed())
    }

    async fn execute(&self, req: ExecuteRequest) -> Result<QueryHandle, DriverError> {
        // 沿用 core 分配的 id，取消才能命中。
        let query_id = req.query_id.clone().unwrap_or_else(QueryId::new);
        *self.active.lock() = Some(query_id.clone());

        let conn = self.conn.clone();
        let sql = req.sql;
        let max_rows = req.fetch.max_rows.max(1) as usize;
        let blocking = tokio::task::spawn_blocking(move || run_statement(conn, &sql, max_rows))
            .await
            .map_err(|e| DriverError::internal(e.to_string()))?;

        *self.active.lock() = None;

        let mut result = blocking?;
        let columns: Arc<[ColumnMeta]> = result
            .columns
            .drain(..)
            .map(|(name, ty)| ColumnMeta {
                name: Arc::from(name),
                native_type: ty.label(),
                logical_type: ty,
                nullable: true,
            })
            .collect::<Vec<_>>()
            .into();

        let mut warnings = Vec::new();
        if result.truncated {
            warnings.push(rustdb_db_api::session::DbWarning {
                level: rustdb_db_api::session::WarningLevel::Warning,
                message: format!("结果已截断：仅加载前 {max_rows} 行"),
            });
        }

        let stream =
            rustdb_db_api::chunked_row_stream(std::mem::take(&mut result.rows), &req.fetch);

        Ok(QueryHandle {
            query_id,
            columns,
            stream,
            affected: result.affected,
            warnings,
        })
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<(), DriverError> {
        let active = self.active.lock().clone();
        if active.as_ref() == Some(query_id) {
            // SQLite 的 interrupt 作用于该连接上正在执行的语句。
            self.interrupt.interrupt();
            return Ok(());
        }
        Err(DriverError::internal("该查询已结束，无需取消"))
    }

    async fn begin(&self, opts: TxOptions) -> Result<TxId, DriverError> {
        let mut sql = String::from("BEGIN");
        if let Some(level) = opts.isolation {
            let clause = match level {
                IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
                IsolationLevel::ReadCommitted => "READ COMMITTED",
                IsolationLevel::RepeatableRead => "REPEATABLE READ",
                IsolationLevel::Serializable => "SERIALIZABLE",
            };
            sql.push_str(&format!(" {clause}"));
        }
        self.exec_batch(sql).await?;
        Ok(TxId::new())
    }

    async fn commit(&self, _tx: TxId) -> Result<(), DriverError> {
        self.exec_batch("COMMIT".into()).await
    }

    async fn rollback(&self, _tx: TxId) -> Result<(), DriverError> {
        self.exec_batch("ROLLBACK".into()).await
    }

    async fn use_namespace(&self, ns: &Namespace) -> Result<(), DriverError> {
        match ns.catalog.as_deref() {
            None | Some("main") => Ok(()),
            Some(other) => Err(DriverError::unsupported(format!(
                "SQLite 暂不支持切换到 {other}（M2 接入 ATTACH）"
            ))),
        }
    }

    fn metadata(&self) -> &dyn MetadataProvider {
        &self.meta
    }

    async fn fetch_page(&self, req: &PageRequest) -> Result<PageResult, DriverError> {
        req.validate()?;
        let conn = self.conn.clone();
        let dialect = self.dialect.clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            fetch_page_blocking(&conn, dialect.as_ref(), &req)
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
    }
}

impl SqliteSession {
    async fn exec_batch(&self, sql: String) -> Result<(), DriverError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), DriverError> {
            conn.lock().execute_batch(&sql).map_err(map_sqlite_error)
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
    }
}

struct BlockingResult {
    columns: Vec<(String, LogicalType)>,
    rows: Vec<Row>,
    truncated: bool,
    affected: Option<u64>,
}

fn run_statement(
    conn: Arc<Mutex<Connection>>,
    sql: &str,
    max_rows: usize,
) -> Result<BlockingResult, DriverError> {
    let conn = conn.lock();
    let mut stmt = conn.prepare(sql).map_err(map_sqlite_error)?;
    let col_count = stmt.column_count();

    if col_count == 0 {
        let affected = stmt.execute([]).map_err(map_sqlite_error)? as u64;
        return Ok(BlockingResult {
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
            affected: Some(affected),
        });
    }

    let names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
        .collect();

    let mut rows_out: Vec<Row> = Vec::new();
    let mut inferred: Vec<LogicalType> = vec![LogicalType::Unknown("unknown".into()); col_count];
    let mut truncated = false;
    let mut cursor = stmt.query([]).map_err(map_sqlite_error)?;

    while let Some(row) = cursor.next().map_err(map_sqlite_error)? {
        if rows_out.len() >= max_rows {
            truncated = true;
            break;
        }
        let mut values = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let raw = row.get_ref(i).map_err(map_sqlite_error)?;
            let value = convert_value(raw);
            if matches!(inferred[i], LogicalType::Unknown(_)) && !value.is_null() {
                inferred[i] = infer_type(&value);
            }
            values.push(value);
        }
        rows_out.push(values);
    }

    Ok(BlockingResult {
        columns: names.into_iter().zip(inferred).collect(),
        rows: rows_out,
        truncated,
        affected: None,
    })
}

fn convert_value(raw: ValueRef<'_>) -> Value {
    match raw {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => Value::Int(v),
        ValueRef::Real(v) => Value::Float(v),
        ValueRef::Text(bytes) => {
            Value::Text(Arc::from(String::from_utf8_lossy(bytes).into_owned()))
        }
        ValueRef::Blob(bytes) => Value::Bytes(Arc::from(bytes)),
    }
}

fn infer_type(value: &Value) -> LogicalType {
    match value {
        Value::Bool(_) => LogicalType::Bool,
        Value::Int(_) => LogicalType::Int {
            bits: 64,
            unsigned: false,
        },
        Value::UInt(_) => LogicalType::Int {
            bits: 64,
            unsigned: true,
        },
        Value::Float(_) => LogicalType::Float { bits: 64 },
        Value::Decimal(_) => LogicalType::Decimal {
            precision: None,
            scale: None,
        },
        Value::Text(_) => LogicalType::Text { max_len: None },
        Value::Bytes(_) => LogicalType::Bytes,
        Value::Json(_) => LogicalType::Json,
        _ => LogicalType::Unknown("value".into()),
    }
}

fn fetch_page_blocking(
    conn: &Connection,
    dialect: &dyn Dialect,
    req: &PageRequest,
) -> Result<PageResult, DriverError> {
    let table = req.path.name.to_string();
    let (columns, pk) = table_columns_and_pk(conn, &table)?;
    let built = build_page_sql(dialect, req, &pk)?;
    let params: Vec<rusqlite::types::Value> = built
        .params
        .iter()
        .map(to_sqlite_value)
        .collect::<Result<_, _>>()?;

    let mut stmt = conn.prepare(&built.sql).map_err(map_sqlite_error)?;
    let limit = req.page_size as usize;
    let mut rows = Vec::with_capacity(limit.min(1024));
    let mut has_more = false;
    {
        let mut cursor = stmt
            .query(rusqlite::params_from_iter(params))
            .map_err(map_sqlite_error)?;
        while let Some(row) = cursor.next().map_err(map_sqlite_error)? {
            // 多取的一行只用来判断 has_more，不返回。
            if rows.len() >= limit {
                has_more = true;
                break;
            }
            let mut values = Vec::with_capacity(columns.len());
            for i in 0..columns.len() {
                values.push(convert_value(row.get_ref(i).map_err(map_sqlite_error)?));
            }
            rows.push(values);
        }
    }

    Ok(PageResult {
        columns,
        rows,
        page: req.page,
        page_size: req.page_size,
        offset: req.offset(),
        has_more,
    })
}

fn table_columns_and_pk(
    conn: &Connection,
    table: &str,
) -> Result<(Arc<[ColumnMeta]>, Vec<String>), DriverError> {
    let pragma = format!("PRAGMA table_info({})", quote_string_literal(table));
    let mut stmt = conn.prepare(&pragma).map_err(map_sqlite_error)?;
    let mut columns = Vec::new();
    let mut pk: Vec<(i64, String)> = Vec::new();
    let mut rows = stmt.query([]).map_err(map_sqlite_error)?;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let name: String = row.get(1).map_err(map_sqlite_error)?;
        let declared: String = row.get(2).unwrap_or_default();
        let notnull: i64 = row.get(3).unwrap_or(0);
        let pk_pos: i64 = row.get(5).unwrap_or(0);
        let mut meta = ColumnMeta::new(name.clone(), logical_type_from_declared(&declared));
        meta.native_type = declared;
        meta.nullable = notnull == 0;
        columns.push(meta);
        if pk_pos > 0 {
            pk.push((pk_pos, name));
        }
    }
    pk.sort_by_key(|(pos, _)| *pos);
    Ok((
        Arc::from(columns),
        pk.into_iter().map(|(_, name)| name).collect(),
    ))
}

fn to_sqlite_value(value: &Value) -> Result<rusqlite::types::Value, DriverError> {
    use rusqlite::types::Value as Sql;
    Ok(match value {
        Value::Null => Sql::Null,
        Value::Bool(v) => Sql::Integer(*v as i64),
        Value::Int(v) => Sql::Integer(*v),
        Value::UInt(v) => Sql::Integer(
            i64::try_from(*v)
                .map_err(|_| DriverError::unsupported("SQLite 无法绑定超出 i64 范围的整数"))?,
        ),
        Value::Float(v) => Sql::Real(*v),
        Value::Decimal(v) | Value::Text(v) | Value::Json(v) => Sql::Text(v.to_string()),
        Value::Bytes(v) => Sql::Blob(v.to_vec()),
        Value::Date(v) => Sql::Text(v.format("%Y-%m-%d").to_string()),
        Value::Time(v) => Sql::Text(v.format("%H:%M:%S%.f").to_string()),
        Value::Timestamp(v) => Sql::Text(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
        Value::TimestampTz(v) => Sql::Text(v.to_rfc3339()),
        Value::Uuid(bytes) => {
            let mut text = String::with_capacity(36);
            for (i, byte) in bytes.iter().enumerate() {
                if matches!(i, 4 | 6 | 8 | 10) {
                    text.push('-');
                }
                text.push_str(&format!("{byte:02x}"));
            }
            Sql::Text(text)
        }
        _ => return Err(DriverError::unsupported("SQLite 不支持该过滤值类型")),
    })
}

struct SqliteMetadata {
    conn: Arc<Mutex<Connection>>,
}

#[async_trait]
impl MetadataProvider for SqliteMetadata {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError> {
        Ok(vec![Namespace {
            catalog: Some(Arc::from("main")),
            schema: None,
        }])
    }

    async fn objects(
        &self,
        _ns: &Namespace,
        kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<ObjectSummary>, DriverError> {
            let conn = conn.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT name, type FROM sqlite_master \
                     WHERE type IN ('table','view','trigger','index') \
                       AND name NOT LIKE 'sqlite_%' \
                       AND name NOT LIKE 'sqlite_autoindex%' \
                     ORDER BY name",
                )
                .map_err(map_sqlite_error)?;
            let mut out = Vec::new();
            let mut rows = stmt.query([]).map_err(map_sqlite_error)?;
            while let Some(row) = rows.next().map_err(map_sqlite_error)? {
                let name: String = row.get(0).map_err(map_sqlite_error)?;
                let ty: String = row.get(1).map_err(map_sqlite_error)?;
                let kind = match ty.as_str() {
                    "view" => ObjectKind::View,
                    "trigger" => ObjectKind::Trigger,
                    "index" => ObjectKind::Index,
                    _ => ObjectKind::Table,
                };
                if !kinds.contains(kind) {
                    continue;
                }
                out.push(ObjectSummary {
                    path: ObjectPath::new(
                        Namespace {
                            catalog: Some(Arc::from("main")),
                            schema: None,
                        },
                        name,
                        kind,
                    ),
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
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
    }

    async fn columns(&self, path: &ObjectPath) -> Result<Vec<ColumnInfo>, DriverError> {
        let conn = self.conn.clone();
        let table = path.name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<ColumnInfo>, DriverError> {
            let conn = conn.lock();
            let pragma = format!("PRAGMA table_info({})", quote_string_literal(&table));
            let mut stmt = conn.prepare(&pragma).map_err(map_sqlite_error)?;
            let mut out = Vec::new();
            let mut rows = stmt.query([]).map_err(map_sqlite_error)?;
            while let Some(row) = rows.next().map_err(map_sqlite_error)? {
                let ordinal: i64 = row.get(0).map_err(map_sqlite_error)?;
                let name: String = row.get(1).map_err(map_sqlite_error)?;
                let declared: String = row.get(2).unwrap_or_default();
                let notnull: i64 = row.get(3).unwrap_or(0);
                let default: Option<String> = row.get(4).ok();
                let pk: i64 = row.get(5).unwrap_or(0);
                let mut col =
                    ColumnInfo::simple(name, logical_type_from_declared(&declared), ordinal as u32);
                col.meta.native_type = declared;
                col.meta.nullable = notnull == 0;
                col.default = default;
                col.is_primary_key = pk > 0;
                out.push(col);
            }
            Ok(out)
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
    }

    async fn indexes(&self, path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        let conn = self.conn.clone();
        let table = path.name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<IndexInfo>, DriverError> {
            let conn = conn.lock();
            let pragma = format!("PRAGMA index_list({})", quote_string_literal(&table));
            let mut stmt = conn.prepare(&pragma).map_err(map_sqlite_error)?;
            let mut out = Vec::new();
            let mut rows = stmt.query([]).map_err(map_sqlite_error)?;
            while let Some(row) = rows.next().map_err(map_sqlite_error)? {
                let name: String = row.get(1).map_err(map_sqlite_error)?;
                let unique: i64 = row.get(2).unwrap_or(0);
                let origin: String = row.get(3).unwrap_or_default();
                let info_sql = format!("PRAGMA index_info({})", quote_string_literal(&name));
                let mut info_stmt = conn.prepare(&info_sql).map_err(map_sqlite_error)?;
                let mut cols = Vec::new();
                let mut info_rows = info_stmt.query([]).map_err(map_sqlite_error)?;
                while let Some(info) = info_rows.next().map_err(map_sqlite_error)? {
                    if let Ok(col) = info.get::<_, String>(2) {
                        cols.push(col);
                    }
                }
                out.push(IndexInfo {
                    name,
                    columns: cols,
                    unique: unique != 0,
                    primary: origin == "pk",
                    method: Some("btree".into()),
                });
            }
            Ok(out)
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
    }

    /// Table triggers (raw `sqlite_master` rows with `type='trigger'`).
    async fn triggers(&self, path: &ObjectPath) -> Result<Option<Vec<TriggerInfo>>, DriverError> {
        let conn = self.conn.clone();
        let table = path.name.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Vec<TriggerInfo>>, DriverError> {
            let conn = conn.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1",
                )
                .map_err(map_sqlite_error)?;
            let mut rows = stmt.query([table.as_str()]).map_err(map_sqlite_error)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_sqlite_error)? {
                let name: String = row.get(0).map_err(map_sqlite_error)?;
                let sql: Option<String> = row.get(1).map_err(map_sqlite_error)?;
                let (timing, event) = sqlite_trigger_shape(sql.as_deref().unwrap_or_default());
                out.push(TriggerInfo {
                    name,
                    timing,
                    event,
                    statement: sql,
                });
            }
            Ok(Some(out))
        })
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
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
            foreign_keys: Vec::<ForeignKeyInfo>::new(),
            checks: Vec::new(),
            triggers,
            // SQLite 没有表级存储引擎；报一个「SQLite」会让设计器把元数据当成用户改动。
            engine: None,
            collation: None,
            comment: None,
            ddl_source: rustdb_db_api::DdlSource::Unavailable,
        })
    }

    /// SQLite 把建表 / 建视图 / 建索引 / 触发器的原文都存在 `sqlite_master.sql`。
    async fn object_ddl(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::ObjectDdl>, DriverError> {
        let conn = self.conn.clone();
        let name = path.name.to_string();
        tokio::task::spawn_blocking(
            move || -> Result<Option<rustdb_db_api::ObjectDdl>, DriverError> {
                let conn = conn.lock();
                let mut stmt = conn
                    .prepare("SELECT sql FROM sqlite_master WHERE name = ?1 AND sql IS NOT NULL")
                    .map_err(map_sqlite_error)?;
                let mut rows = stmt.query([name.as_str()]).map_err(map_sqlite_error)?;
                let Some(row) = rows.next().map_err(map_sqlite_error)? else {
                    return Ok(None);
                };
                let text: String = row.get(0).map_err(map_sqlite_error)?;
                Ok(Some(rustdb_db_api::ObjectDdl {
                    text,
                    source: rustdb_db_api::DdlSource::Native,
                }))
            },
        )
        .await
        .map_err(|e| DriverError::internal(e.to_string()))?
    }
}

/// SQLite 的触发器只有 `sqlite_master.sql` 原文，触发时机与事件从头部文本解析。
fn sqlite_trigger_shape(sql: &str) -> (Option<String>, Option<String>) {
    let upper = sql.to_uppercase();
    // 只看 `... ON 表名` 之前的头部，避免触发体里的关键字干扰。
    let header = upper.split(" ON ").next().unwrap_or(upper.as_str());
    let timing = if header.contains("INSTEAD OF") {
        Some("INSTEAD OF".to_string())
    } else if header.contains("BEFORE") {
        Some("BEFORE".to_string())
    } else if header.contains("AFTER") {
        Some("AFTER".to_string())
    } else {
        None
    };
    let event = ["INSERT", "UPDATE", "DELETE"]
        .into_iter()
        .find(|keyword| header.contains(keyword))
        .map(|keyword| keyword.to_string());
    (timing, event)
}

fn logical_type_from_declared(declared: &str) -> LogicalType {
    let upper = declared.to_ascii_uppercase();
    if upper.contains("INT") {
        LogicalType::Int {
            bits: 64,
            unsigned: false,
        }
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        LogicalType::Text { max_len: None }
    } else if upper.contains("BLOB") {
        LogicalType::Bytes
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        LogicalType::Float { bits: 64 }
    } else if upper.contains("NUM") || upper.contains("DEC") {
        LogicalType::Decimal {
            precision: None,
            scale: None,
        }
    } else if upper.contains("BOOL") {
        LogicalType::Bool
    } else if upper.contains("DATE") && !upper.contains("DATETIME") {
        LogicalType::Date
    } else if upper.contains("DATETIME") || upper.contains("TIMESTAMP") {
        LogicalType::Timestamp
    } else {
        LogicalType::Unknown(declared.to_string())
    }
}

fn map_sqlite_error(err: rusqlite::Error) -> DriverError {
    let text = err.to_string();
    let lower = text.to_ascii_lowercase();
    let kind = if lower.contains("constraint") {
        DriverErrorKind::Constraint
    } else if lower.contains("readonly") || lower.contains("read-only") {
        DriverErrorKind::ReadOnly
    } else if lower.contains("interrupted") {
        DriverErrorKind::Cancelled
    } else if lower.contains("syntax") {
        DriverErrorKind::Syntax
    } else if lower.contains("no such table") || lower.contains("no such column") {
        DriverErrorKind::NotFound
    } else if lower.contains("locked") || lower.contains("busy") {
        DriverErrorKind::Timeout
    } else if lower.contains("unable to open") {
        DriverErrorKind::Connection
    } else {
        DriverErrorKind::Protocol
    };
    DriverError::new(kind, text)
}

/// `:memory:` 演示库：让首个版本打开就能看到表结构、跑通查询。
fn seed_demo(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS channels (
            id      INTEGER PRIMARY KEY,
            name    TEXT NOT NULL,
            type    TEXT NOT NULL DEFAULT 'organic'
        );
        CREATE TABLE IF NOT EXISTS orders (
            id          INTEGER PRIMARY KEY,
            channel_id  INTEGER NOT NULL REFERENCES channels(id),
            total       NUMERIC(12,2) NOT NULL DEFAULT 0,
            status      TEXT NOT NULL DEFAULT 'paid',
            created_at  TEXT NOT NULL
        );
        INSERT INTO channels (id, name, type)
        SELECT 1, '自然搜索', 'organic'
        WHERE NOT EXISTS (SELECT 1 FROM channels WHERE id = 1);
        INSERT INTO channels (id, name, type)
        SELECT 2, '付费广告', 'paid'
        WHERE NOT EXISTS (SELECT 1 FROM channels WHERE id = 2);
        INSERT INTO channels (id, name, type)
        SELECT 3, '社交媒体', 'social'
        WHERE NOT EXISTS (SELECT 1 FROM channels WHERE id = 3);
        INSERT INTO orders (id, channel_id, total, status, created_at)
        SELECT 1001, 1, 1280.50, 'paid', '2026-10-12 10:20:00'
        WHERE NOT EXISTS (SELECT 1 FROM orders WHERE id = 1001);
        INSERT INTO orders (id, channel_id, total, status, created_at)
        SELECT 1002, 2, 980.00, 'paid', '2026-10-13 14:05:00'
        WHERE NOT EXISTS (SELECT 1 FROM orders WHERE id = 1002);
        INSERT INTO orders (id, channel_id, total, status, created_at)
        SELECT 1003, 1, 2310.75, 'refunded', '2026-10-14 09:41:00'
        WHERE NOT EXISTS (SELECT 1 FROM orders WHERE id = 1003);
        INSERT INTO orders (id, channel_id, total, status, created_at)
        SELECT 1004, 3, 450.25, 'paid', '2026-10-15 18:12:00'
        WHERE NOT EXISTS (SELECT 1 FROM orders WHERE id = 1004);
        "#,
    )
}

/// 供 `db-api` 的日期类型占位（避免未使用 import 警告）。
#[allow(dead_code)]
fn _type_anchor(v: NaiveDateTime) -> (NaiveDate, NaiveTime, DateTime<Utc>) {
    (v.date(), v.time(), Utc.from_utc_datetime(&v))
}
