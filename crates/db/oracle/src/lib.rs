//! Oracle 驱动（M3）：`oracle` crate（ODPI-C）。
//!
//! 关于构建与运行前提（R5：显式、不静默）：
//! - **构建期**：`odpic-sys` 自带 ODPI-C 源码，`cargo build` 不需要 Oracle Instant
//!   Client（本机已实测可编译、可链接）。
//! - **运行期**：本 crate 以 **thin 模式**编译（未启用 `thick` 特性），连接串是
//!   `host:port/service`，**不需要安装 Instant Client**；只有显式传入完整
//!   EZConnect 描述符（`connect_string` 参数）才需要客户端库。
//!   若真的用到 thick 路径而机器上没有 `oci.dll` / `libclntsh.so`，错误原文
//!   （如 DPI-1047）会带 hint 透传给用户，而不是静默返回空会话。
//!
//! 已实现：连接、`SELECT 1`（`ping`）、查询与 DML、事务、`ALTER SESSION SET
//! CURRENT_SCHEMA`、元数据（`all_users` / `all_objects` / `all_tab_columns` /
//! `all_indexes` / `all_constraints`）、原生 DDL（`DBMS_METADATA.GET_DDL`）。
//!
//! 显式降级：取消。`oracle::Connection` 是 `Send` 但**非** `Sync`，
//! `break_execution()` 需要 `&Connection`，无法从旁路线程打断阻塞中的查询，
//! 故 `Session::cancel` 返回 `Unsupported`，能力矩阵里 `cancel = None`。

use async_trait::async_trait;
use oracle::sql_type::OracleType;
use oracle::{Connection as OraConnection, Row as OraRow};
use rustdb_db_api::{
    dialect::{quote_double, quote_string_literal},
    CancelStyle, Capabilities, ColumnInfo, ColumnMeta, DbKind, Dialect, DriverError,
    DriverErrorKind, DriverFactory, ExecuteRequest, ExplainStyle, ForeignKeyInfo, IndexInfo,
    JsonStyle, LogicalType, MetadataProvider, Namespace, ObjectKind, ObjectKindMask, ObjectPath,
    ObjectSummary, QueryHandle, QueryId, Row, ServerInfo, Session, SessionId, TableDetail,
    TriggerInfo, TxId, TxOptions, UpsertStyle, Value,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Conn = Arc<Mutex<OraConnection>>;

/// 单元结构体：`app` 的注册表按值注册（`drivers.register(OracleFactory)`）。
pub struct OracleFactory;

impl Default for OracleFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl OracleFactory {
    pub fn new() -> Self {
        Self
    }
}

/// 静态能力矩阵；连接后可据服务端版本修正。
fn capabilities() -> Capabilities {
    let mut caps = Capabilities::minimal();
    caps.multi_statement = false; // OCI 一次一条语句
    caps.returning = true; // RETURNING … INTO
    caps.cte = true;
    caps.window_functions = true;
    caps.upsert = UpsertStyle::Merge;
    // 显式降级：见文件头「显式降级」。
    caps.cancel = CancelStyle::None;
    caps.explain = ExplainStyle::ExplainPlan;
    // Oracle DDL 隐式提交，不可回滚。
    caps.ddl_transactional = false;
    caps.schemas = true;
    caps.multiple_catalogs = false;
    caps.json = JsonStyle::TextJson;
    caps.sequences = true;
    caps.materialized_views = true;
    caps.max_ident_len = 128;
    caps.read_only_txn = true;
    caps.streaming_rows = false; // 结果物化成一个 RowBatch 后交回
    caps.notices = true;
    caps.dml_preview = true; // F-63
    caps.routine_source_edit = true; // F-82：DBMS_METADATA.GET_DDL 支持视图 / 过程 / 函数 / 触发器 / 程序包
    caps.routine_dependencies = true; // F-83：ALL_DEPENDENCIES
    caps.foreign_key_pivot = true; // ALL_CONSTRAINTS / ALL_CONS_COLUMNS 已实现
    caps
}

#[async_trait]
impl DriverFactory for OracleFactory {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn display_name(&self) -> &'static str {
        "Oracle"
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
        let user_for_task = user.clone();
        let connect_string_for_task = connect_string.clone();
        // ODPI-C 的连接是阻塞调用：放到 blocking 线程池，绝不占用 GPUI 主线程（R5）。
        let connected = tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || {
                OraConnection::connect(
                    user_for_task.as_str(),
                    password.as_str(),
                    connect_string_for_task.as_str(),
                )
            }),
        )
        .await
        .map_err(|_| {
            DriverError::new(
                DriverErrorKind::Timeout,
                format!(
                    "连接 Oracle 超时（{connect_string}，{} ms）",
                    timeout.as_millis()
                ),
            )
        })?
        .map_err(|e| DriverError::internal(format!("Oracle 连接任务失败：{e}")))?
        .map_err(map_oracle_error)?;

        let conn: Conn = Arc::new(Mutex::new(connected));
        let mut server = ServerInfo::new("Oracle");
        if let Ok((_version, banner)) = sql::with_conn(conn.clone(), |c| {
            c.server_version().map_err(map_oracle_error)
        })
        .await
        {
            if !banner.is_empty() {
                server.version = banner;
            }
        }
        if let Ok(schema) = sql::with_conn(conn.clone(), |c| {
            c.current_schema().map_err(map_oracle_error)
        })
        .await
        {
            if !schema.is_empty() {
                server.current_db = Some(schema);
            }
        }
        server.charset = Some("AL32UTF8".into());
        server.max_ident_len = 128;
        server.read_only = opts.read_only;

        let caps = capabilities();
        Ok(Box::new(OracleSession {
            id: SessionId::new(),
            conn: conn.clone(),
            server,
            caps,
            dialect: Arc::new(OracleDialect),
            meta: OracleMetadata { conn },
            running: Arc::new(AtomicBool::new(false)),
        }))
    }
}

struct OracleDialect;

impl Dialect for OracleDialect {
    fn kind(&self) -> DbKind {
        DbKind::Oracle
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_double(ident)
    }

    fn placeholder(&self, index: usize) -> String {
        format!(":{index}")
    }

    /// 用 `ROWNUM` 包裹，兼容 11g R2（`OFFSET … FETCH` 需要 12c+）。
    fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
        sql::paginate(sql, limit, offset)
    }

    fn explain_sql(&self, sql: &str, _analyze: bool) -> String {
        // `EXPLAIN PLAN FOR` 只写 PLAN_TABLE，真正的计划要再查回来；
        // 这里给出固定两步语句，由 core 决定是否读取。
        format!("EXPLAIN PLAN FOR {sql}")
    }
}

struct OracleSession {
    id: SessionId,
    conn: Conn,
    server: ServerInfo,
    caps: Capabilities,
    dialect: Arc<dyn Dialect>,
    meta: OracleMetadata,
    running: Arc<AtomicBool>,
}

#[async_trait]
impl Session for OracleSession {
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
        sql::with_conn(self.conn.clone(), |conn| {
            conn.query_row(sql::select_one(), &[])
                .map_err(map_oracle_error)?;
            Ok(())
        })
        .await?;
        Ok(started.elapsed())
    }

    async fn execute(&self, req: ExecuteRequest) -> Result<QueryHandle, DriverError> {
        let query_id = req.query_id.clone().unwrap_or_else(QueryId::new);
        let max_rows = req.fetch.max_rows.max(1) as usize;
        let sql_text = req.sql.clone();

        self.running.store(true, Ordering::SeqCst);
        let outcome = sql::with_conn(self.conn.clone(), move |conn| {
            run_blocking(conn, &sql_text, max_rows)
        })
        .await;
        self.running.store(false, Ordering::SeqCst);

        let outcome = outcome?;
        let (columns, rows, affected, truncated) = outcome;

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

    /// 显式降级：`oracle::Connection` 非 `Sync`，`break_execution()` 无法在查询
    /// 阻塞期间从旁路线程调用（OCI 的 break 语义要求另一个线程/进程持有连接句柄）。
    /// 换成「专用 worker 线程 + 命令通道」可以做到，但那要求驱动层接管线程模型，
    /// 留到后续版本；这里明确报错而不是假装取消成功。
    async fn cancel(&self, _query_id: &QueryId) -> Result<(), DriverError> {
        let msg = if self.running.load(Ordering::SeqCst) {
            "Oracle 取消：oracle crate 的 Connection 非 Sync，无法在查询阻塞期间调用 break_execution"
        } else {
            "Oracle 取消：当前没有正在执行的查询"
        };
        Err(DriverError::unsupported(msg))
    }

    async fn begin(&self, opts: TxOptions) -> Result<TxId, DriverError> {
        let read_only = opts.read_only;
        sql::with_conn(self.conn.clone(), move |conn| {
            conn.set_autocommit(false);
            if read_only {
                // Oracle 的只读事务是 `SET TRANSACTION READ ONLY`，必须在事务首条
                // 语句前设置。
                conn.execute("SET TRANSACTION READ ONLY", &[])
                    .map_err(map_oracle_error)?;
            }
            Ok(())
        })
        .await?;
        Ok(TxId::new())
    }

    async fn commit(&self, _tx: TxId) -> Result<(), DriverError> {
        sql::with_conn(self.conn.clone(), |conn| {
            conn.commit().map_err(map_oracle_error)?;
            conn.set_autocommit(true);
            Ok(())
        })
        .await
    }

    async fn rollback(&self, _tx: TxId) -> Result<(), DriverError> {
        sql::with_conn(self.conn.clone(), |conn| {
            conn.rollback().map_err(map_oracle_error)?;
            conn.set_autocommit(true);
            Ok(())
        })
        .await
    }

    async fn use_namespace(&self, ns: &Namespace) -> Result<(), DriverError> {
        if let Some(catalog) = &ns.catalog {
            // Oracle 没有跨库切换：catalog 只是 UI 上的「实例」概念。
            return Err(DriverError::unsupported(format!(
                "Oracle 不支持切换 catalog（{catalog}）；请通过 schema/user 切换"
            )));
        }
        let Some(schema) = &ns.schema else {
            return Ok(());
        };
        let schema = schema.to_string();
        let sql_text = sql::alter_session_schema_sql(&schema);
        sql::with_conn(self.conn.clone(), move |conn| {
            conn.execute(&sql_text, &[]).map_err(map_oracle_error)?;
            Ok(())
        })
        .await?;
        Ok(())
    }

    fn metadata(&self) -> &dyn MetadataProvider {
        &self.meta
    }
}

/// 在 blocking 线程池里跑一次 OCI 调用（OCI 全同步）。
fn run_blocking(
    conn: &mut OraConnection,
    sql_text: &str,
    max_rows: usize,
) -> Result<
    (
        Vec<(String, LogicalType, String, bool)>,
        Vec<Row>,
        u64,
        bool,
    ),
    DriverError,
> {
    let mut stmt = conn.statement(sql_text).build().map_err(map_oracle_error)?;
    if stmt.is_query() {
        let result_set = stmt.query(&[]).map_err(map_oracle_error)?;
        let mut columns: Vec<(String, LogicalType, String, bool)> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let mut truncated = false;
        for item in result_set {
            let row = item.map_err(map_oracle_error)?;
            if columns.is_empty() {
                columns = row
                    .column_info()
                    .iter()
                    .map(|info| {
                        (
                            info.name().to_string(),
                            sql::logical_from_oracle_type(info.oracle_type()),
                            info.oracle_type().to_string(),
                            info.nullable(),
                        )
                    })
                    .collect();
            }
            if rows.len() >= max_rows {
                truncated = true;
                continue;
            }
            rows.push(convert_row(&row));
        }
        Ok((columns, rows, 0, truncated))
    } else {
        stmt.execute(&[]).map_err(map_oracle_error)?;
        let affected = stmt.row_count().unwrap_or(0);
        Ok((Vec::new(), Vec::new(), affected, false))
    }
}

fn convert_row(row: &OraRow) -> Row {
    // `oracle::sql_type::SqlValue` 未公开导出，闭包参数类型由编译器推断。
    row.sql_values()
        .iter()
        .map(|value| {
            if value.is_null().unwrap_or(true) {
                return Value::Null;
            }
            let Ok(oracle_type) = value.oracle_type() else {
                return Value::Null;
            };
            match oracle_type {
                OracleType::Varchar2(_)
                | OracleType::NVarchar2(_)
                | OracleType::Char(_)
                | OracleType::NChar(_)
                | OracleType::CLOB
                | OracleType::NCLOB
                | OracleType::Long
                | OracleType::Rowid => value
                    .get::<String>()
                    .map(|text: String| Value::Text(Arc::from(text.as_str())))
                    .unwrap_or(Value::Null),
                // NUMBER 一律先取精确十进制字符串，再判整/定点 —— 金额不能过 f64。
                OracleType::Number(_, _) | OracleType::Float(_) => value
                    .get::<String>()
                    .map(|text: String| sql::parse_number(text.as_str()))
                    .unwrap_or(Value::Null),
                OracleType::BinaryFloat => value
                    .get::<f32>()
                    .map(|v| Value::Float(v as f64))
                    .unwrap_or(Value::Null),
                OracleType::BinaryDouble => {
                    value.get::<f64>().map(Value::Float).unwrap_or(Value::Null)
                }
                OracleType::Raw(_) | OracleType::LongRaw | OracleType::BLOB => value
                    .get::<Vec<u8>>()
                    .map(|bytes| Value::Bytes(Arc::from(bytes.as_slice())))
                    .unwrap_or(Value::Null),
                OracleType::Date | OracleType::Timestamp(_) | OracleType::TimestampLTZ(_) => value
                    .get::<chrono::NaiveDateTime>()
                    .map(Value::Timestamp)
                    .unwrap_or(Value::Null),
                OracleType::TimestampTZ(_) => value
                    .get::<chrono::DateTime<chrono::Utc>>()
                    .map(Value::TimestampTz)
                    .unwrap_or(Value::Null),
                OracleType::Boolean => value.get::<bool>().map(Value::Bool).unwrap_or(Value::Null),
                other => value
                    .get::<String>()
                    .map(|text: String| Value::Unknown {
                        native_type: other.to_string(),
                        text: Arc::from(text.as_str()),
                    })
                    .unwrap_or(Value::Null),
            }
        })
        .collect()
}

struct OracleMetadata {
    conn: Conn,
}

#[async_trait]
impl MetadataProvider for OracleMetadata {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError> {
        let rows = sql::with_conn(self.conn.clone(), |conn| {
            rows_of(conn, sql::namespaces_sql().to_string())
        })
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| Namespace {
                catalog: None,
                schema: Some(Arc::from(sql::text_of(&row, 0).as_str())),
            })
            .collect())
    }

    async fn objects(
        &self,
        ns: &Namespace,
        kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError> {
        let Some(schema) = ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(Vec::new());
        };
        let rows = sql::with_conn(self.conn.clone(), move |conn| {
            rows_of(conn, sql::objects_sql(&schema))
        })
        .await?;

        let mut out = Vec::new();
        for row in rows {
            let name = sql::text_of(&row, 0);
            let object_type = sql::text_of(&row, 1);
            let Some(kind) = sql::object_kind_from_type(&object_type) else {
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
        let Some(schema) = path.ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(Vec::new());
        };
        let table = path.name.to_string();
        let rows = sql::with_conn(self.conn.clone(), move |conn| {
            rows_of(conn, sql::columns_sql(&schema, &table))
        })
        .await?;

        let mut out = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            let name = sql::text_of(&row, 0);
            let data_type = sql::text_of(&row, 1);
            let length = sql::int_of(&row, 2) as i64;
            let precision = sql::int_of(&row, 3) as u8;
            let scale = sql::int_of(&row, 4) as u8;
            let nullable = sql::text_of(&row, 5).eq_ignore_ascii_case("Y");
            let default = {
                let value = sql::text_of(&row, 6);
                if value.is_empty() {
                    None
                } else {
                    Some(value)
                }
            };
            let logical = sql::logical_from_data_type(&data_type, precision, scale, length);
            let mut col = ColumnInfo::simple(name, logical, index as u32 + 1);
            col.meta.native_type = sql::native_type_label(&data_type, precision, scale, length);
            col.meta.nullable = nullable;
            col.is_auto_increment = default
                .as_deref()
                .map(sql::is_sequence_default)
                .unwrap_or(false);
            col.default = default.filter(|value| !sql::is_sequence_default(value));
            out.push(col);
        }
        Ok(out)
    }

    async fn indexes(&self, path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        let Some(schema) = path.ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(Vec::new());
        };
        let table = path.name.to_string();
        let rows = sql::with_conn(self.conn.clone(), move |conn| {
            rows_of(conn, sql::indexes_sql(&schema, &table))
        })
        .await?;

        let mut grouped: Vec<IndexInfo> = Vec::new();
        for row in rows {
            let name = sql::text_of(&row, 0);
            let uniqueness = sql::text_of(&row, 1);
            let index_type = sql::text_of(&row, 2);
            let column = sql::text_of(&row, 3);
            match grouped.iter_mut().find(|i| i.name == name) {
                Some(existing) => existing.columns.push(column),
                None => grouped.push(IndexInfo {
                    name,
                    columns: vec![column],
                    unique: uniqueness.eq_ignore_ascii_case("UNIQUE"),
                    primary: false,
                    method: Some(index_type),
                }),
            }
        }
        Ok(grouped)
    }

    async fn foreign_keys(&self, path: &ObjectPath) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        let Some(schema) = path.ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(Vec::new());
        };
        let table = path.name.to_string();
        let rows = sql::with_conn(self.conn.clone(), move |conn| {
            rows_of(conn, sql::foreign_keys_sql(&schema, &table))
        })
        .await?;

        let mut grouped: Vec<ForeignKeyInfo> = Vec::new();
        for row in rows {
            let name = sql::text_of(&row, 0);
            let column = sql::text_of(&row, 1);
            let ref_table = sql::text_of(&row, 2);
            let ref_column = sql::text_of(&row, 3);
            let on_delete = sql::text_of(&row, 4);
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
                    // Oracle 外键没有 ON UPDATE 规则。
                    on_update: None,
                }),
            }
        }
        Ok(grouped)
    }

    /// Table triggers (`all_triggers`, current schema only).
    async fn triggers(&self, path: &ObjectPath) -> Result<Option<Vec<TriggerInfo>>, DriverError> {
        let Some(schema) = path.ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(Some(Vec::new()));
        };
        let table = path.name.to_string();
        let rows = sql::with_conn(self.conn.clone(), move |conn| {
            rows_of(conn, sql::triggers_sql(&schema, &table))
        })
        .await?;
        Ok(Some(
            rows.into_iter()
                .map(|row| TriggerInfo {
                    name: sql::text_of(&row, 0),
                    timing: Some(sql::text_of(&row, 1)),
                    event: Some(sql::text_of(&row, 2)),
                    statement: None,
                })
                .collect(),
        ))
    }

    /// 直接依赖（F-83）：`ALL_DEPENDENCIES` 双向查询。
    async fn dependencies(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::DependencyList>, DriverError> {
        use rustdb_db_api::{DependencyInfo, DependencyList};
        let Some(schema) = path.ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(None);
        };
        let name = path.name.to_string();
        let Some(query) = sql::dependencies_sql(&schema, &name, path.kind) else {
            return Ok(None);
        };
        let rows = sql::with_conn(self.conn.clone(), move |conn| rows_of(conn, query)).await?;
        let mut out = DependencyList::default();
        for row in rows {
            let dep = sql::text_of(&row, 0) == "dep";
            let owner = sql::text_of(&row, 1);
            let object_name = sql::text_of(&row, 2);
            let Some(kind) = sql::object_kind_from_type(&sql::text_of(&row, 3)) else {
                continue;
            };
            if object_name.is_empty() || owner.is_empty() {
                continue;
            }
            let info = DependencyInfo {
                path: ObjectPath::new(
                    Namespace::new(path.ns.catalog.clone(), Some(owner.clone())),
                    object_name,
                    kind,
                ),
                dependency_type: match sql::text_of(&row, 4).as_str() {
                    "HARD" => "硬依赖".into(),
                    "SOFT" => "软依赖".into(),
                    other => other.to_string(),
                },
                remote: (owner != schema).then_some(owner),
            };
            if dep {
                out.push_depends_on(info);
            } else {
                out.push_depended_by(info);
            }
        }
        out.note = Some("仅直接依赖（ALL_DEPENDENCIES），需要对象可见权限".into());
        Ok(Some(out))
    }

    async fn table(&self, path: &ObjectPath) -> Result<TableDetail, DriverError> {
        let columns = self.columns(path).await?;
        let indexes = self.indexes(path).await.unwrap_or_default();
        let foreign_keys = self.foreign_keys(path).await.unwrap_or_default();
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
            ddl_source: rustdb_db_api::DdlSource::Native,
        })
    }

    /// 原生 DDL：`DBMS_METADATA.GET_DDL`（需要 `SELECT_CATALOG_ROLE` 或对象属主权限）。
    async fn object_ddl(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::ObjectDdl>, DriverError> {
        let Some(schema) = path.ns.schema.as_deref().map(|s| s.to_string()) else {
            return Ok(None);
        };
        let Some(object_type) = sql::dbms_metadata_type(path.kind) else {
            return Ok(None);
        };
        let name = path.name.to_string();
        let query = sql::ddl_sql(object_type, &name, &schema);
        let rows = sql::with_conn(self.conn.clone(), move |conn| rows_of(conn, query)).await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let text = sql::text_of(row, 0);
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(rustdb_db_api::ObjectDdl {
            text,
            source: rustdb_db_api::DdlSource::Native,
        }))
    }
}

fn rows_of(conn: &mut OraConnection, query: String) -> Result<Vec<OraRow>, DriverError> {
    let result_set = conn.query(&query, &[]).map_err(map_oracle_error)?;
    let mut out = Vec::new();
    for item in result_set {
        out.push(item.map_err(map_oracle_error)?);
    }
    Ok(out)
}

fn map_oracle_error(err: oracle::Error) -> DriverError {
    let message = err
        .db_error()
        .map(|db| db.message().to_string())
        .unwrap_or_else(|| err.to_string());
    if let Some(code) = err.oci_code() {
        let kind = match code {
            1 | 2291 | 2292 | 1400 | 1555 => DriverErrorKind::Constraint,
            54 | 60 | 8177 => DriverErrorKind::Deadlock,
            900 | 901 | 904 | 905 | 906 | 907 | 908 | 909 | 910 | 911 | 917 | 918 | 920 | 921
            | 922 | 923 | 924 | 926 | 927 | 928 | 936 | 970 | 971 | 1008 | 1009 | 1035 => {
                DriverErrorKind::Syntax
            }
            942 | 1418 => DriverErrorKind::NotFound,
            1017 | 28000 | 28001 => DriverErrorKind::Auth,
            1031 | 1032 | 1045 => DriverErrorKind::Permission,
            1013 => DriverErrorKind::Cancelled,
            12170 | 12505 | 12514 | 12541 | 12545 => DriverErrorKind::Connection,
            _ => DriverErrorKind::Protocol,
        };
        return DriverError::new(kind, message)
            .with_code(code as i64)
            .with_sqlstate(format!("ORA-{code:05}"));
    }
    // 没有 OCI 错误码 = 客户端侧问题（Instant Client 缺失、参数非法等）。
    let mut mapped = DriverError::connection(message);
    if let Some(code) = err.dpi_code() {
        mapped = mapped.with_code(code as i64);
        if code == 1047 {
            mapped = mapped.with_hint(
                "未找到 Oracle 客户端库：请安装 Oracle Instant Client 并加入 PATH / LD_LIBRARY_PATH",
            );
        }
    }
    mapped
}

/// 纯函数区：连接串、SQL 生成、类型映射。全部可单测（不碰网络）。
pub mod sql {
    use super::*;

    pub fn select_one() -> &'static str {
        "SELECT 1 FROM dual"
    }

    /// 连接串：
    /// - `params["connect_string"]` 优先（支持完整 `(DESCRIPTION=…)`）
    /// - `database` 以 `(` 开头视为完整描述符，原样使用
    /// - `params["sid"]` 存在时用 SID 描述符（老式实例）
    /// - 否则按 `//host:port/service_name` 拼 EZConnect
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
        // thin 模式（默认编译形态，无需 Instant Client）用 `host:port/service`；
        // 带 `//` 前缀是 thick/EZConnect 写法，thin 解析器不认。
        match database {
            Some(service) if !service.is_empty() => format!("{host}:{port}/{service}"),
            _ => format!("{host}:{port}"),
        }
    }

    /// 切 schema：Oracle 的「schema」就是用户，改变当前 schema 要 `ALTER SESSION`。
    pub fn alter_session_schema_sql(schema: &str) -> String {
        format!(
            "ALTER SESSION SET CURRENT_SCHEMA = {}",
            quote_double(schema)
        )
    }

    /// Oracle 11g 也支持的 `ROWNUM` 分页（12c 的 `OFFSET/FETCH` 不作默认）。
    pub fn paginate(sql: &str, limit: u64, offset: u64) -> String {
        let upper = offset.saturating_add(limit);
        format!(
            "SELECT * FROM (SELECT rustdb_inner.*, ROWNUM AS rustdb_rnum FROM ({sql}) rustdb_inner \
             WHERE ROWNUM <= {upper}) WHERE rustdb_rnum > {offset}"
        )
    }

    pub fn namespaces_sql() -> &'static str {
        "SELECT username FROM all_users ORDER BY username"
    }

    pub fn objects_sql(schema: &str) -> String {
        format!(
            "SELECT object_name, object_type FROM all_objects \
             WHERE owner = {owner} \
               AND object_type IN ('TABLE', 'VIEW', 'MATERIALIZED VIEW', 'SEQUENCE', 'PROCEDURE', \
                                   'FUNCTION', 'TRIGGER', 'TYPE', 'SYNONYM', 'PACKAGE') \
               AND object_name NOT LIKE 'BIN$%' \
             ORDER BY object_type, object_name",
            owner = quote_string_literal(schema)
        )
    }

    pub fn columns_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT column_name, data_type, data_length, data_precision, data_scale, nullable, \
                    data_default, column_id \
             FROM all_tab_columns \
             WHERE owner = {owner} AND table_name = {table} \
             ORDER BY column_id",
            owner = quote_string_literal(schema),
            table = quote_string_literal(table)
        )
    }

    pub fn indexes_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT i.index_name, i.uniqueness, i.index_type, c.column_name \
             FROM all_indexes i \
             JOIN all_ind_columns c \
               ON c.index_owner = i.owner AND c.index_name = i.index_name \
             WHERE i.table_owner = {owner} AND i.table_name = {table} \
             ORDER BY i.index_name, c.column_position",
            owner = quote_string_literal(schema),
            table = quote_string_literal(table)
        )
    }

    pub fn foreign_keys_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT c.constraint_name, cc.column_name, r.table_name, rcc.column_name, c.delete_rule \
             FROM all_constraints c \
             JOIN all_cons_columns cc \
               ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
             JOIN all_constraints r \
               ON r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name \
             JOIN all_cons_columns rcc \
               ON rcc.owner = r.owner AND rcc.constraint_name = r.constraint_name \
              AND rcc.position = cc.position \
             WHERE c.owner = {owner} AND c.table_name = {table} AND c.constraint_type = 'R' \
             ORDER BY c.constraint_name, cc.position",
            owner = quote_string_literal(schema),
            table = quote_string_literal(table)
        )
    }

    /// 表上的触发器（`all_triggers`，只取当前 owner 可见的）。
    pub fn triggers_sql(schema: &str, table: &str) -> String {
        format!(
            "SELECT trigger_name, trigger_type, triggering_event FROM all_triggers \
             WHERE owner = {owner} AND table_name = {table} ORDER BY trigger_name",
            owner = quote_string_literal(schema),
            table = quote_string_literal(table)
        )
    }

    /// 对象依赖（F-83）：`ALL_DEPENDENCIES` 双向查询；按对象类型过滤，避免同名对象串台。
    pub fn dependencies_sql(schema: &str, name: &str, kind: ObjectKind) -> Option<String> {
        let target_type = match kind {
            ObjectKind::Table => "TABLE",
            ObjectKind::View => "VIEW",
            ObjectKind::MaterializedView => "MATERIALIZED VIEW",
            ObjectKind::Sequence => "SEQUENCE",
            ObjectKind::Function => "FUNCTION",
            ObjectKind::Procedure => "PROCEDURE",
            ObjectKind::Trigger => "TRIGGER",
            ObjectKind::Type => "TYPE",
            ObjectKind::Package => "PACKAGE",
            ObjectKind::Synonym => "SYNONYM",
            ObjectKind::Index => return None,
        };
        let owner = quote_string_literal(schema);
        let object_name = quote_string_literal(name);
        let object_type = quote_string_literal(target_type);
        Some(format!(
            "SELECT 'dep' AS dir, referenced_owner, referenced_name, referenced_type, \
                    dependency_type \
             FROM all_dependencies \
             WHERE owner = {owner} AND name = {object_name} AND type = {object_type} \
               AND referenced_owner IS NOT NULL \
             UNION ALL \
             SELECT 'rev', owner, name, type, dependency_type \
             FROM all_dependencies \
             WHERE referenced_owner = {owner} AND referenced_name = {object_name} \
               AND referenced_type = {object_type} \
             ORDER BY 2, 3"
        ))
    }

    /// `DBMS_METADATA.GET_DDL` 的对象类型参数（注意是单数、带空格的大写串）。
    pub fn dbms_metadata_type(kind: ObjectKind) -> Option<&'static str> {
        match kind {
            ObjectKind::Table => Some("TABLE"),
            ObjectKind::View => Some("VIEW"),
            ObjectKind::MaterializedView => Some("MATERIALIZED_VIEW"),
            ObjectKind::Index => Some("INDEX"),
            ObjectKind::Sequence => Some("SEQUENCE"),
            ObjectKind::Procedure => Some("PROCEDURE"),
            ObjectKind::Function => Some("FUNCTION"),
            ObjectKind::Trigger => Some("TRIGGER"),
            ObjectKind::Type => Some("TYPE"),
            ObjectKind::Package => Some("PACKAGE"),
            ObjectKind::Synonym => Some("SYNONYM"),
        }
    }

    pub fn ddl_sql(object_type: &str, name: &str, owner: &str) -> String {
        format!(
            "SELECT DBMS_METADATA.GET_DDL({object_type}, {name}, {owner}) AS ddl FROM dual",
            object_type = quote_string_literal(object_type),
            name = quote_string_literal(name),
            owner = quote_string_literal(owner)
        )
    }

    pub fn object_kind_from_type(object_type: &str) -> Option<ObjectKind> {
        match object_type.trim().to_ascii_uppercase().as_str() {
            "TABLE" => Some(ObjectKind::Table),
            "VIEW" => Some(ObjectKind::View),
            "MATERIALIZED VIEW" => Some(ObjectKind::MaterializedView),
            "SEQUENCE" => Some(ObjectKind::Sequence),
            "PROCEDURE" => Some(ObjectKind::Procedure),
            "FUNCTION" => Some(ObjectKind::Function),
            "TRIGGER" => Some(ObjectKind::Trigger),
            "TYPE" => Some(ObjectKind::Type),
            "SYNONYM" => Some(ObjectKind::Synonym),
            "PACKAGE" => Some(ObjectKind::Package),
            _ => None,
        }
    }

    /// `all_tab_columns.data_type` → 逻辑类型。
    pub fn logical_from_data_type(
        data_type: &str,
        precision: u8,
        scale: u8,
        length: i64,
    ) -> LogicalType {
        match data_type.trim().to_ascii_uppercase().as_str() {
            "CHAR" | "VARCHAR" | "VARCHAR2" | "NCHAR" | "NVARCHAR2" | "CLOB" | "NCLOB" | "LONG" => {
                LogicalType::Text {
                    max_len: if length > 0 {
                        Some(length as u32)
                    } else {
                        None
                    },
                }
            }
            "NUMBER" => {
                if scale == 0 && precision > 0 && precision <= 18 {
                    LogicalType::Int {
                        bits: if precision <= 4 {
                            16
                        } else if precision <= 9 {
                            32
                        } else {
                            64
                        },
                        unsigned: false,
                    }
                } else if precision == 0 && scale == 0 {
                    // 不带精度的 NUMBER：可能装任意数值，按 decimal 处理。
                    LogicalType::Decimal {
                        precision: None,
                        scale: None,
                    }
                } else {
                    LogicalType::Decimal {
                        precision: Some(precision),
                        scale: Some(scale),
                    }
                }
            }
            "FLOAT" => LogicalType::Float { bits: 64 },
            "BINARY_FLOAT" => LogicalType::Float { bits: 32 },
            "BINARY_DOUBLE" => LogicalType::Float { bits: 64 },
            "DATE" => LogicalType::Timestamp, // Oracle DATE 含时分秒
            "TIMESTAMP" | "TIMESTAMP WITH TIME ZONE" | "TIMESTAMP WITH LOCAL TIME ZONE" => {
                LogicalType::Timestamp
            }
            "RAW" | "LONG RAW" | "BLOB" => LogicalType::Bytes,
            "ROWID" | "UROWID" => LogicalType::Text { max_len: None },
            "XMLTYPE" | "JSON" => LogicalType::Json,
            other => LogicalType::Unknown(other.to_string()),
        }
    }

    pub fn native_type_label(data_type: &str, precision: u8, scale: u8, length: i64) -> String {
        let upper = data_type.trim().to_ascii_uppercase();
        match upper.as_str() {
            "NUMBER" if precision > 0 && scale > 0 => format!("NUMBER({precision},{scale})"),
            "NUMBER" if precision > 0 => format!("NUMBER({precision})"),
            "CHAR" | "VARCHAR" | "VARCHAR2" | "NCHAR" | "NVARCHAR2" | "RAW" if length > 0 => {
                format!("{upper}({length})")
            }
            _ => upper,
        }
    }

    /// 序列/identity 的默认值（`"SCHEMA"."SEQ".nextval`）→ 视为自增列。
    pub fn is_sequence_default(default: &str) -> bool {
        let lower = default.to_ascii_lowercase();
        lower.contains("nextval") || lower.contains("identity")
    }

    /// 数字字符串 → `Int` / `Decimal`（绝不走 f64，避免金额精度丢失）。
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

    /// `all_tab_columns` 里 `NUMBER(10,0)` 之类映射到逻辑类型（结果集侧）。
    pub fn logical_from_oracle_type(oracle_type: &OracleType) -> LogicalType {
        match oracle_type {
            OracleType::Varchar2(size)
            | OracleType::NVarchar2(size)
            | OracleType::Char(size)
            | OracleType::NChar(size) => LogicalType::Text {
                max_len: if *size > 0 { Some(*size) } else { None },
            },
            OracleType::Number(precision, scale) => logical_from_data_type(
                "NUMBER",
                *precision,
                if *scale < 0 { 0 } else { *scale as u8 },
                0,
            ),
            OracleType::Float(_) => LogicalType::Float { bits: 64 },
            OracleType::BinaryFloat => LogicalType::Float { bits: 32 },
            OracleType::BinaryDouble => LogicalType::Float { bits: 64 },
            OracleType::Date | OracleType::Timestamp(_) | OracleType::TimestampLTZ(_) => {
                LogicalType::Timestamp
            }
            OracleType::TimestampTZ(_) => LogicalType::TimestampTz,
            OracleType::Raw(_) | OracleType::LongRaw | OracleType::BLOB => LogicalType::Bytes,
            OracleType::Boolean => LogicalType::Bool,
            other => LogicalType::Unknown(other.to_string()),
        }
    }

    pub fn text_of(row: &OraRow, idx: usize) -> String {
        row.get::<_, Option<String>>(idx)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn int_of(row: &OraRow, idx: usize) -> i64 {
        row.get::<_, Option<i64>>(idx).ok().flatten().unwrap_or(0)
    }

    /// 在 blocking 线程池里执行一次 OCI 调用并转成 `DriverError`。
    ///
    /// OCI 全程同步，不能跑在 async worker 上（会阻塞整个运行时线程），所以统一
    /// 走 `spawn_blocking`；`Arc<Mutex<Connection>>` 保证同一时刻只有一条语句在
    /// 用这条连接（OCI 的 session 不是 Sync）。
    pub async fn with_conn<T, F>(conn: Conn, f: F) -> Result<T, DriverError>
    where
        T: Send + 'static,
        F: FnOnce(&mut OraConnection) -> Result<T, DriverError> + Send + 'static,
    {
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| DriverError::internal("Oracle 连接锁已损坏（前一次调用 panic）"))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| DriverError::internal(format!("Oracle 后台任务失败：{e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::sql;
    use rustdb_db_api::{DbKind, Dialect, LogicalType, ObjectKind, Value};

    #[test]
    fn connect_string_supports_ezconnect_descriptor_and_sid() {
        let empty = std::collections::BTreeMap::new();
        assert_eq!(
            sql::build_connect_string(Some("db1"), None, Some("ORCLPDB1"), &empty),
            "db1:1521/ORCLPDB1"
        );
        assert_eq!(
            sql::build_connect_string(Some("db1"), Some(1522), Some("ORCL"), &empty),
            "db1:1522/ORCL"
        );
        let descriptor = "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=h)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=s)))";
        assert_eq!(
            sql::build_connect_string(Some("ignored"), None, Some(descriptor), &empty),
            descriptor
        );
        let mut params = std::collections::BTreeMap::new();
        params.insert("sid".to_string(), "ORCL".to_string());
        assert!(
            sql::build_connect_string(Some("h"), Some(1521), None, &params).contains("(SID=ORCL)")
        );
        params.insert("connect_string".to_string(), "myalias".to_string());
        assert_eq!(
            sql::build_connect_string(Some("h"), None, Some("svc"), &params),
            "myalias"
        );
    }

    #[test]
    fn dialect_paginates_with_rownum_and_quotes_uppercase_idents() {
        assert_eq!(super::OracleDialect.kind(), DbKind::Oracle);
        assert_eq!(super::OracleDialect.quote_ident("emp"), "\"emp\"");
        assert_eq!(super::OracleDialect.placeholder(2), ":2");

        let paged = sql::paginate("SELECT * FROM emp", 10, 20);
        assert!(paged.contains("ROWNUM <= 30"));
        assert!(paged.contains("rustdb_rnum > 20"));
        assert!(paged.contains("FROM (SELECT * FROM emp) rustdb_inner"));
        assert_eq!(
            sql::alter_session_schema_sql("SCOTT"),
            "ALTER SESSION SET CURRENT_SCHEMA = \"SCOTT\""
        );
    }

    #[test]
    fn dependency_sql_filters_by_object_type() {
        let query =
            sql::dependencies_sql("SCOTT", "EMP", ObjectKind::Table).expect("表应支持依赖查询");
        assert!(query.contains("all_dependencies"));
        assert!(query.contains("owner = 'SCOTT'"));
        assert!(query.contains("type = 'TABLE'"));
        assert!(query.contains("referenced_type = 'TABLE'"));
        assert!(query.contains("ORDER BY 2, 3"));
        assert!(sql::dependencies_sql("SCOTT", "EMP", ObjectKind::Index).is_none());
    }

    #[test]
    fn metadata_sql_targets_oracle_data_dictionary() {
        let objects = sql::objects_sql("SCOTT");
        assert!(objects.contains("all_objects"));
        assert!(objects.contains("owner = 'SCOTT'"));
        assert!(objects.contains("'MATERIALIZED VIEW'"));
        assert!(objects.contains("NOT LIKE 'BIN$%'"));

        let columns = sql::columns_sql("SCOTT", "EMP");
        assert!(columns.contains("all_tab_columns"));
        assert!(columns.contains("table_name = 'EMP'"));
        assert!(columns.contains("ORDER BY column_id"));

        let indexes = sql::indexes_sql("SCOTT", "EMP");
        assert!(indexes.contains("all_indexes"));
        assert!(indexes.contains("all_ind_columns"));

        let fks = sql::foreign_keys_sql("SCOTT", "EMP");
        assert!(fks.contains("constraint_type = 'R'"));
        assert!(fks.contains("delete_rule"));

        // 单引号转义
        assert!(sql::columns_sql("O'BRIEN", "T").contains("'O''BRIEN'"));
        assert_eq!(
            sql::namespaces_sql(),
            "SELECT username FROM all_users ORDER BY username"
        );
    }

    #[test]
    fn ddl_uses_dbms_metadata_for_known_kinds() {
        assert_eq!(sql::dbms_metadata_type(ObjectKind::Table), Some("TABLE"));
        assert_eq!(
            sql::dbms_metadata_type(ObjectKind::MaterializedView),
            Some("MATERIALIZED_VIEW")
        );
        let query = sql::ddl_sql("TABLE", "EMP", "SCOTT");
        assert!(query.contains("DBMS_METADATA.GET_DDL('TABLE', 'EMP', 'SCOTT')"));
        assert!(query.contains("FROM dual"));
    }

    #[test]
    fn type_mapping_covers_number_text_and_time() {
        assert_eq!(
            sql::logical_from_data_type("VARCHAR2", 0, 0, 50),
            LogicalType::Text { max_len: Some(50) }
        );
        assert_eq!(
            sql::logical_from_data_type("NUMBER", 9, 0, 22),
            LogicalType::Int {
                bits: 32,
                unsigned: false
            }
        );
        // NUMBER(10) 上限 9_999_999_999 超出 i32，必须落到 64 位。
        assert_eq!(
            sql::logical_from_data_type("NUMBER", 10, 0, 22),
            LogicalType::Int {
                bits: 64,
                unsigned: false
            }
        );
        assert_eq!(
            sql::logical_from_data_type("NUMBER", 18, 2, 22),
            LogicalType::Decimal {
                precision: Some(18),
                scale: Some(2)
            }
        );
        assert_eq!(
            sql::logical_from_data_type("DATE", 0, 0, 7),
            LogicalType::Timestamp
        );
        assert_eq!(
            sql::logical_from_data_type("BLOB", 0, 0, 4000),
            LogicalType::Bytes
        );
        assert_eq!(sql::native_type_label("NUMBER", 18, 2, 22), "NUMBER(18,2)");
        assert_eq!(
            sql::native_type_label("VARCHAR2", 0, 0, 100),
            "VARCHAR2(100)"
        );
    }

    #[test]
    fn numbers_stay_exact_and_object_types_map_to_kinds() {
        assert_eq!(sql::parse_number("42"), Value::Int(42));
        assert_eq!(
            sql::parse_number("123.4500"),
            Value::Decimal(std::sync::Arc::from("123.4500"))
        );
        assert_eq!(
            sql::parse_number("1E5"),
            Value::Decimal(std::sync::Arc::from("1E5"))
        );
        assert_eq!(sql::parse_number(""), Value::Null);

        assert_eq!(
            sql::object_kind_from_type("MATERIALIZED VIEW"),
            Some(ObjectKind::MaterializedView)
        );
        assert_eq!(
            sql::object_kind_from_type("package"),
            Some(ObjectKind::Package)
        );
        assert_eq!(sql::object_kind_from_type("LOB"), None);

        assert!(sql::is_sequence_default("\"SCOTT\".\"EMP_SEQ\".nextval"));
        assert!(!sql::is_sequence_default("SYSDATE"));
    }

    #[test]
    fn capabilities_mark_cancel_as_explicitly_unsupported() {
        use rustdb_db_api::DriverFactory;
        let caps = super::OracleFactory::new().baseline_capabilities();
        assert_eq!(caps.cancel, rustdb_db_api::CancelStyle::None);
        assert_eq!(caps.upsert, rustdb_db_api::UpsertStyle::Merge);
        assert!(!caps.multiple_catalogs);
        assert!(!caps.ddl_transactional);
    }
}
