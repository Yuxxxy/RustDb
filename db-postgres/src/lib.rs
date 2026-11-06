//! PostgreSQL 驱动：simple_query_raw 真惰性流取行 + information_schema 元数据 + CancelToken 取消。

mod tls;

use async_trait::async_trait;
use futures_util::{stream, StreamExt};
use parking_lot::RwLock;
use rustdb_db_api::{
    dialect::quote_double, CancelStyle, Capabilities, ColumnInfo, ColumnMeta, DbKind, Dialect,
    DriverError, DriverErrorKind, DriverFactory, ExecuteRequest, ExplainStyle, ForeignKeyInfo,
    IndexInfo, JsonStyle, LogicalType, MetadataProvider, Namespace, ObjectKind, ObjectKindMask,
    ObjectPath, ObjectSummary, QueryHandle, QueryId, Row, RowBatch, ServerInfo, Session, SessionId,
    TableDetail, TriggerInfo, TxId, TxOptions, UpsertStyle, Value,
};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};

pub struct PostgresFactory {
    caps: Capabilities,
}

impl Default for PostgresFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl PostgresFactory {
    pub fn new() -> Self {
        let mut caps = Capabilities::minimal();
        caps.multi_statement = true; // simple query 支持多语句
        caps.returning = true;
        caps.cte = true;
        caps.window_functions = true;
        caps.upsert = UpsertStyle::OnConflict;
        caps.cancel = CancelStyle::CancelToken;
        caps.explain = ExplainStyle::ExplainAnalyze;
        caps.ddl_transactional = true;
        caps.schemas = true;
        caps.multiple_catalogs = true;
        caps.arrays = true;
        caps.json = JsonStyle::Native;
        caps.enum_types = true;
        caps.sequences = true;
        caps.materialized_views = true;
        caps.max_ident_len = 63;
        caps.read_only_txn = true;
        // P2：execute 走 simple_query_raw，按 page_size 逐批交付，不再物化整表。
        caps.streaming_rows = true;
        caps.notices = true;
        // F-63：DML 预览由 core 的保守改写器生成，方言侧只需声明能力位。
        caps.dml_preview = true;
        caps.routine_source_edit = true; // F-82：视图 / 函数 / 过程 / 触发器源码
        caps.routine_dependencies = true; // F-83：pg_depend 直接依赖
        caps.partition_admin = true; // F-85：声明式分区（PG 10+）
        caps.foreign_key_pivot = true; // F-73：pg_constraint 外键可用于跳转
        Self { caps }
    }
}

#[async_trait]
impl DriverFactory for PostgresFactory {
    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn display_name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn baseline_capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    async fn connect(
        &self,
        opts: &rustdb_db_api::ConnectOptions,
    ) -> Result<Box<dyn Session>, DriverError> {
        let mut config = tokio_postgres::Config::new();
        config.host(opts.host.as_deref().unwrap_or("127.0.0.1"));
        config.port(opts.port.unwrap_or(5432));
        config.user(opts.username.as_deref().unwrap_or("postgres"));
        config.dbname(opts.database.as_deref().unwrap_or("postgres"));
        config.application_name("RustDb");
        if let Some(secret) = &opts.secret {
            config.password(secret);
        }
        config.connect_timeout(Duration::from_millis(opts.connect_timeout_ms.max(1000)));

        let ssl_mode = opts
            .ssl_mode
            .as_deref()
            .unwrap_or("prefer")
            .to_ascii_lowercase();
        let client = match ssl_mode.as_str() {
            "disable" | "disabled" | "off" | "none" => open(config, NoTls).await?,
            // prefer：先尝试 TLS，失败再回退明文，语义与 libpq 一致。
            // 与 libpq 相同，prefer 不校验证书；需要校验时用 verify-ca/verify-full。
            "prefer" => match tls::RustlsMakeTlsConnect::new(false) {
                Ok(tls) => match open(config.clone(), tls).await {
                    Ok(client) => client,
                    // 只有“TLS 不可用”才回退明文；认证失败重试一次只会多记一次失败，
                    // 还可能触发账号锁定。
                    Err(error)
                        if !matches!(
                            error.kind,
                            DriverErrorKind::Auth | DriverErrorKind::Permission
                        ) =>
                    {
                        log::warn!(
                            "PostgreSQL TLS 连接失败（sslmode=prefer），回退明文：{}",
                            error.user_message()
                        );
                        open(config, NoTls).await?
                    }
                    Err(error) => return Err(error),
                },
                Err(error) => {
                    log::warn!("PostgreSQL TLS 不可用（sslmode=prefer），使用明文：{error}");
                    open(config, NoTls).await?
                }
            },
            // require：加密但不校验证书（libpq 语义）。
            "require" => {
                let tls = tls::RustlsMakeTlsConnect::new(false)
                    .map_err(|error| DriverError::new(DriverErrorKind::Tls, error))?;
                open(config, tls).await?
            }
            // verify-ca 也做完整校验（含主机名），比 libpq 更严格。
            "verify-ca" | "verify-full" => {
                let tls = tls::RustlsMakeTlsConnect::new(true)
                    .map_err(|error| DriverError::new(DriverErrorKind::Tls, error))?;
                open(config, tls).await?
            }
            other => {
                return Err(DriverError::new(
                    DriverErrorKind::Connection,
                    format!(
                    "未知的 ssl_mode：{other}（可选 disable/prefer/require/verify-ca/verify-full）"
                ),
                ))
            }
        };

        let row = client
            .query_one(
                "select version(), current_database(), current_schema(), current_user",
                &[],
            )
            .await
            .map_err(map_pg_error)?;
        let version: String = row.get(0);
        let current_db: String = row.get(1);
        let current_schema: String = row.get(2);
        let _user: String = row.get(3);

        let mut server = ServerInfo::new(version);
        server.current_db = Some(current_db.clone());
        server.charset = Some("UTF8".into());
        server.max_ident_len = 63;
        server.read_only = opts.read_only;

        let caps = self.caps.clone();
        let cancel = client.cancel_token();
        let client = Arc::new(client);
        Ok(Box::new(PgSession {
            id: SessionId::new(),
            client: client.clone(),
            cancel,
            server,
            caps,
            dialect: Arc::new(PgDialect),
            meta: PgMetadata { client, current_db },
            current_schema: RwLock::new(current_schema),
            active_query: Arc::new(RwLock::new(None)),
        }))
    }
}

struct PgDialect;

impl Dialect for PgDialect {
    fn kind(&self) -> DbKind {
        DbKind::Postgres
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_double(ident)
    }

    fn placeholder(&self, index: usize) -> String {
        format!("${index}")
    }

    fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
        format!("{sql} LIMIT {limit} OFFSET {offset}")
    }

    fn explain_sql(&self, sql: &str, analyze: bool) -> String {
        if analyze {
            format!("EXPLAIN (ANALYZE, BUFFERS) {sql}")
        } else {
            format!("EXPLAIN {sql}")
        }
    }
}

struct PgSession {
    id: SessionId,
    client: Arc<Client>,
    cancel: tokio_postgres::CancelToken,
    server: ServerInfo,
    caps: Capabilities,
    dialect: Arc<dyn Dialect>,
    meta: PgMetadata,
    current_schema: RwLock<String>,
    /// 当前查询 id；取消请求必须与它匹配，避免迟到取消误杀下一条查询。
    /// 用 Arc 包围：RowStream 是 'static，需要在流被 drop 时清除当前查询 id。
    active_query: Arc<RwLock<Option<QueryId>>>,
}

/// RowStream 被消费完（或调用方提前 drop）时清掉 active_query，
/// 避免 cancel 命中一条已经结束的查询。
struct ActiveQueryGuard {
    active: Arc<RwLock<Option<QueryId>>>,
    query_id: QueryId,
}

impl Drop for ActiveQueryGuard {
    fn drop(&mut self) {
        let mut active = self.active.write();
        if active.as_ref() == Some(&self.query_id) {
            *active = None;
        }
    }
}

/// 流被读到底时立即清 active_query；调用方提前 drop 流则由
/// ActiveQueryGuard 兜底。两者都做，才能覆盖「消费完但 handle 未 drop」的窗口。
struct ClearActiveOnEnd<S> {
    inner: Pin<Box<S>>,
    active: Arc<RwLock<Option<QueryId>>>,
    query_id: QueryId,
}

impl<S: stream::Stream> stream::Stream for ClearActiveOnEnd<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(None) => {
                let mut active = this.active.write();
                if active.as_ref() == Some(&this.query_id) {
                    *active = None;
                }
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

/// text 协议下所有值都是可选字符串；保留文本，不做二进制类型映射。
fn convert_simple_row(row: &tokio_postgres::SimpleQueryRow) -> Row {
    (0..row.len())
        .map(|i| match row.get(i) {
            Some(text) => Value::Text(Arc::from(text.to_string())),
            None => Value::Null,
        })
        .collect()
}

#[async_trait]
impl Session for PgSession {
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
        self.client
            .simple_query("SELECT 1")
            .await
            .map_err(map_pg_error)?;
        Ok(started.elapsed())
    }

    async fn execute(&self, req: ExecuteRequest) -> Result<QueryHandle, DriverError> {
        let query_id = req.query_id.clone().unwrap_or_else(QueryId::new);
        let max_rows = req.fetch.max_rows.max(1) as usize;
        let page_size = req.fetch.page_size.max(1) as usize;
        *self.active_query.write() = Some(query_id.clone());

        // simple_query_raw 返回 owned 'static 流：这里只等首条消息（列描述或
        // CommandComplete），不再把整张结果表物化成 Vec 之后才交回 core。
        let mut raw = match self.client.simple_query_raw(&req.sql).await {
            Ok(stream) => Box::pin(stream),
            Err(error) => {
                *self.active_query.write() = None;
                return Err(map_pg_error(error));
            }
        };

        let mut columns: Vec<String> = Vec::new();
        let mut affected: u64 = 0;
        let mut pending: Vec<Result<SimpleQueryMessage, tokio_postgres::Error>> = Vec::new();
        // 无结果集（DDL/DML）时 loop 会把 raw 读到底；此时不能再把它 chain 进
        // 新流，否则组合子会重复 poll 已结束的 Responses 并报 connection closed。
        let mut exhausted = false;
        loop {
            match raw.next().await {
                Some(Ok(SimpleQueryMessage::RowDescription(cols))) => {
                    columns = cols.iter().map(|c| c.name().to_string()).collect();
                    break;
                }
                Some(Ok(message @ SimpleQueryMessage::Row(_))) => {
                    // 协议保证 RowDescription 在前；兜底把这一行放回流中。
                    pending.push(Ok(message));
                    break;
                }
                Some(Ok(SimpleQueryMessage::CommandComplete(n))) => affected += n,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    *self.active_query.write() = None;
                    return Err(map_pg_error(error));
                }
                None => {
                    exhausted = true;
                    break;
                }
            }
        }

        if exhausted {
            *self.active_query.write() = None;
            return Ok(QueryHandle {
                query_id,
                columns: Arc::from(Vec::<ColumnMeta>::new()),
                stream: Box::pin(stream::empty()),
                affected: Some(affected),
                warnings: Vec::new(),
            });
        }

        let has_columns = !columns.is_empty();
        let column_meta: Arc<[ColumnMeta]> = columns
            .into_iter()
            .map(|name| ColumnMeta {
                name: Arc::from(name),
                // simple query 走文本协议：类型以文本保留。
                logical_type: LogicalType::Unknown("text".into()),
                native_type: "text".into(),
                nullable: true,
            })
            .collect::<Vec<_>>()
            .into();

        // QueryHandle 会 move 走 query_id，先给流留一份克隆。
        let stream_query_id = query_id.clone();
        let guard = ActiveQueryGuard {
            active: self.active_query.clone(),
            query_id: query_id.clone(),
        };
        // 必须 fuse：chunks/take 等组合子在结束边缘可能再 poll 一次底层流，
        // 而 SimpleQueryStream 的 Responses 在 ReadyForQuery 之后被重复 poll
        // 会返回 "connection closed"（DML 路径实测）。
        let messages = stream::iter(pending).chain(raw).fuse();
        let batches = messages
            .filter_map(move |message| {
                // 闭包持有 guard：流 drop 时触发 ActiveQueryGuard::drop。
                let _keep_alive = &guard;
                futures_util::future::ready(match message {
                    Ok(SimpleQueryMessage::Row(row)) => Some(Ok(convert_simple_row(&row))),
                    // 后续语句的 RowDescription / CommandComplete 不改变首批列定义。
                    Ok(_) => None,
                    Err(error) => Some(Err(map_pg_error(error))),
                })
            })
            .take(max_rows)
            .chunks(page_size)
            .map(|chunk| {
                let mut rows = Vec::with_capacity(chunk.len());
                for item in chunk {
                    rows.push(item?);
                }
                Ok(RowBatch::new(rows))
            });

        Ok(QueryHandle {
            query_id,
            columns: column_meta,
            stream: Box::pin(ClearActiveOnEnd {
                inner: Box::pin(batches),
                active: self.active_query.clone(),
                query_id: stream_query_id,
            }),
            affected: if has_columns { None } else { Some(affected) },
            // 流式下返回时还不知道总行数，截断由 core 的 mark_truncated 判定。
            warnings: Vec::new(),
        })
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<(), DriverError> {
        let active = self.active_query.read().clone();
        if active.as_ref() != Some(query_id) {
            return Err(DriverError::internal("该查询已结束，无需取消"));
        }
        self.cancel.cancel_query(NoTls).await.map_err(map_pg_error)
    }

    async fn begin(&self, opts: TxOptions) -> Result<TxId, DriverError> {
        let mut sql = String::from("BEGIN");
        if opts.read_only {
            sql.push_str(" READ ONLY");
        }
        self.client
            .batch_execute(&sql)
            .await
            .map_err(map_pg_error)?;
        Ok(TxId::new())
    }

    async fn commit(&self, _tx: TxId) -> Result<(), DriverError> {
        self.client
            .batch_execute("COMMIT")
            .await
            .map_err(map_pg_error)
    }

    async fn rollback(&self, _tx: TxId) -> Result<(), DriverError> {
        self.client
            .batch_execute("ROLLBACK")
            .await
            .map_err(map_pg_error)
    }

    async fn use_namespace(&self, ns: &Namespace) -> Result<(), DriverError> {
        if let Some(catalog) = &ns.catalog {
            if catalog.as_ref() != self.meta.current_db {
                return Err(DriverError::unsupported(
                    "PostgreSQL 跨数据库切换需要新建连接（当前仅支持同库切 schema）",
                ));
            }
        }
        if let Some(schema) = &ns.schema {
            let sql = format!("SET search_path TO {}", quote_double(schema));
            self.client
                .batch_execute(&sql)
                .await
                .map_err(map_pg_error)?;
            *self.current_schema.write() = schema.to_string();
        }
        Ok(())
    }

    fn metadata(&self) -> &dyn MetadataProvider {
        &self.meta
    }
}

struct PgMetadata {
    client: Arc<Client>,
    current_db: String,
}

#[async_trait]
impl MetadataProvider for PgMetadata {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError> {
        let rows = self
            .client
            .query(
                "select nspname from pg_namespace \
                 where nspname not like 'pg_%' and nspname <> 'information_schema' \
                 order by nspname",
                &[],
            )
            .await
            .map_err(map_pg_error)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let schema: String = row.get(0);
                Namespace {
                    catalog: Some(Arc::from(self.current_db.as_str())),
                    schema: Some(Arc::from(schema)),
                }
            })
            .collect())
    }

    async fn objects(
        &self,
        ns: &Namespace,
        kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError> {
        let schema = ns.schema.as_deref().unwrap_or("public");
        let mut out: Vec<ObjectSummary> = Vec::new();

        let relational = kinds.contains(ObjectKind::Table)
            || kinds.contains(ObjectKind::View)
            || kinds.contains(ObjectKind::MaterializedView)
            || kinds.contains(ObjectKind::Sequence);
        if relational {
            // relkind: r=表 p=分区表 v=视图 m=物化视图 S=序列
            let rows = self
                .client
                .query(
                    "select c.relname, c.relkind::text from pg_class c \
                     join pg_namespace n on n.oid = c.relnamespace \
                     where n.nspname = $1 and c.relkind in ('r','p','v','m','S') \
                     order by c.relname",
                    &[&schema],
                )
                .await
                .map_err(map_pg_error)?;
            for row in rows {
                let name: String = row.get(0);
                let relkind: String = row.get(1);
                let kind = match relkind.as_str() {
                    "v" => ObjectKind::View,
                    "m" => ObjectKind::MaterializedView,
                    "S" => ObjectKind::Sequence,
                    _ => ObjectKind::Table,
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

        if kinds.contains(ObjectKind::Procedure) || kinds.contains(ObjectKind::Function) {
            // prokind: f=函数 p=存储过程 a=聚合 w=窗口
            let rows = self
                .client
                .query(
                    "select p.proname, p.prokind::text from pg_proc p \
                     join pg_namespace n on n.oid = p.pronamespace \
                     where n.nspname = $1 and p.prokind in ('f','p') \
                     order by p.proname",
                    &[&schema],
                )
                .await
                .map_err(map_pg_error)?;
            for row in rows {
                let name: String = row.get(0);
                let prokind: String = row.get(1);
                let kind = if prokind == "p" {
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

        if kinds.contains(ObjectKind::Type) {
            // 只取用户自定义枚举/域/独立复合类型（表的行类型不重复展示）。
            let rows = self
                .client
                .query(
                    "select t.typname from pg_type t \
                     join pg_namespace n on n.oid = t.typnamespace \
                     where n.nspname = $1 \
                       and (t.typtype in ('d','e') or (t.typtype = 'c' and t.typrelid = 0)) \
                     order by t.typname",
                    &[&schema],
                )
                .await
                .map_err(map_pg_error)?;
            for row in rows {
                let name: String = row.get(0);
                out.push(ObjectSummary {
                    path: ObjectPath::new(ns.clone(), name, ObjectKind::Type),
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
        let schema = path.ns.schema.as_deref().unwrap_or("public");
        let table = path.name.as_ref();
        let rows = self
            .client
            .query(
                "select column_name, data_type, is_nullable, column_default \
                 from information_schema.columns \
                 where table_schema = $1 and table_name = $2 \
                 order by ordinal_position",
                &[&schema, &table],
            )
            .await
            .map_err(map_pg_error)?;

        let mut out = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            let name: String = row.get(0);
            let data_type: String = row.get(1);
            let nullable: String = row.get(2);
            let default: Option<String> = row.get(3);
            let logical = logical_from_pg(&data_type);
            let mut col = ColumnInfo::simple(name, logical, index as u32 + 1);
            col.meta.native_type = data_type;
            col.meta.nullable = nullable.eq_ignore_ascii_case("YES");
            col.default = default;
            out.push(col);
        }
        Ok(out)
    }

    /// 外键（F-73）：`pg_constraint` 按列展开，复合外键按约束名合并。
    async fn foreign_keys(&self, path: &ObjectPath) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        let schema = path.ns.schema.as_deref().unwrap_or("public");
        let table = path.name.as_ref();
        let rows = self
            .client
            .query(foreign_keys_sql(), &[&schema, &table])
            .await
            .map_err(map_pg_error)?;

        let mut grouped: Vec<ForeignKeyInfo> = Vec::new();
        for row in rows {
            let name: String = row.get(0);
            let column: String = row.get(1);
            let referenced_table: String = row.get(2);
            let referenced_column: String = row.get(3);
            let on_delete: String = row.get(4);
            let on_update: String = row.get(5);
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
                    on_delete: Some(on_delete),
                    on_update: Some(on_update),
                }),
            }
        }
        Ok(grouped)
    }

    async fn indexes(&self, path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        let schema = path.ns.schema.as_deref().unwrap_or("public");
        let table = path.name.as_ref();
        let rows = self
            .client
            .query(
                "select i.relname, ix.indisunique, ix.indisprimary, \
                        array_agg(a.attname order by k.ord) \
                 from pg_class t \
                 join pg_namespace n on n.oid = t.relnamespace \
                 join pg_index ix on ix.indrelid = t.oid \
                 join pg_class i on i.oid = ix.indexrelid \
                 join lateral unnest(ix.indkey) with ordinality k(attnum, ord) on true \
                 join pg_attribute a on a.attrelid = t.oid and a.attnum = k.attnum \
                 where n.nspname = $1 and t.relname = $2 \
                 group by i.relname, ix.indisunique, ix.indisprimary \
                 order by i.relname",
                &[&schema, &table],
            )
            .await
            .map_err(map_pg_error)?;

        Ok(rows
            .into_iter()
            .map(|row| IndexInfo {
                name: row.get(0),
                unique: row.get(1),
                primary: row.get(2),
                columns: row.get(3),
                method: Some("btree".into()),
            })
            .collect())
    }

    /// Table triggers (`pg_trigger`, internal constraint triggers excluded).
    async fn triggers(&self, path: &ObjectPath) -> Result<Option<Vec<TriggerInfo>>, DriverError> {
        let schema = path.ns.schema.as_deref().unwrap_or("public");
        let table = path.name.as_ref();
        let rows = self
            .client
            .query(
                "select t.tgname, \
                        case when (t.tgtype & 2) = 2 then 'BEFORE' \
                             when (t.tgtype & 64) = 64 then 'INSTEAD OF' \
                             else 'AFTER' end, \
                        case when (t.tgtype & 4) = 4 then 'INSERT' \
                             when (t.tgtype & 8) = 8 then 'DELETE' \
                             when (t.tgtype & 16) = 16 then 'UPDATE' \
                             else 'TRUNCATE' end, \
                        pg_get_triggerdef(t.oid) \
                 from pg_trigger t \
                 join pg_class c on c.oid = t.tgrelid \
                 join pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = $1 and c.relname = $2 and not t.tgisinternal \
                 order by t.tgname",
                &[&schema, &table],
            )
            .await
            .map_err(map_pg_error)?;
        Ok(Some(
            rows.into_iter()
                .map(|row| TriggerInfo {
                    name: row.get(0),
                    timing: row.get(1),
                    event: row.get(2),
                    statement: row.get(3),
                })
                .collect(),
        ))
    }

    /// 一级分区（F-85）：`pg_partitioned_table` 取策略与分区键，`pg_inherits` 取直接子表。
    async fn partitions(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::PartitionList>, DriverError> {
        use rustdb_db_api::{PartitionInfo, PartitionList};
        let schema = path.ns.schema.as_deref().unwrap_or("public");
        let table = path.name.to_string();
        let parent = self
            .client
            .query(
                "select pt.partstrat::text, pg_get_partkeydef(pt.partrelid) \
                 from pg_partitioned_table pt \
                 join pg_class c on c.oid = pt.partrelid \
                 join pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = $1 and c.relname = $2",
                &[&schema, &table],
            )
            .await
            .map_err(map_pg_error)?;
        let Some(row) = parent.first() else {
            return Ok(Some(PartitionList {
                strategy: None,
                key: None,
                partitions: Vec::new(),
                truncated: false,
                note: Some("该表不是分区表；PostgreSQL 需先有分区父表".into()),
            }));
        };
        let strategy_code: String = row.get(0);
        let strategy = match strategy_code.as_str() {
            "r" => "range",
            "l" => "list",
            "h" => "hash",
            other => other,
        };
        let mut list = PartitionList {
            strategy: Some(strategy.to_string()),
            key: row.get(1),
            ..PartitionList::default()
        };
        let rows = self
            .client
            .query(
                "select child.relname, \
                        pg_get_expr(child.relpartbound, child.oid, true), \
                        greatest(child.reltuples, 0)::bigint \
                 from pg_inherits i \
                 join pg_class parent on parent.oid = i.inhparent \
                 join pg_class child on child.oid = i.inhrelid \
                 join pg_namespace n on n.oid = parent.relnamespace \
                 where n.nspname = $1 and parent.relname = $2 \
                 order by child.relname \
                 limit 501",
                &[&schema, &table],
            )
            .await
            .map_err(map_pg_error)?;
        if rows.len() > rustdb_db_api::MAX_PARTITIONS {
            list.truncated = true;
        }
        for row in rows.into_iter().take(rustdb_db_api::MAX_PARTITIONS) {
            let bounds: String = row.get(1);
            let estimate: i64 = row.get(2);
            list.push(PartitionInfo {
                name: row.get(0),
                is_default: bounds.trim().eq_ignore_ascii_case("DEFAULT"),
                bounds,
                row_estimate: (estimate >= 0).then_some(estimate as u64),
            });
        }
        list.note = Some("只列一级分区；子分区请展开对应分区查看".into());
        Ok(Some(list))
    }

    /// 直接依赖（F-83）：`pg_depend` 双向查询，`pg_identify_object` 解析引用对象。
    async fn dependencies(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::DependencyList>, DriverError> {
        use rustdb_db_api::{DependencyInfo, DependencyList};
        let Some(schema) = path.ns.schema.as_deref() else {
            return Ok(None);
        };
        let classid = match path.kind {
            ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Table
            | ObjectKind::Sequence
            | ObjectKind::Index => "pg_class",
            ObjectKind::Function | ObjectKind::Procedure => "pg_proc",
            ObjectKind::Trigger => "pg_trigger",
            _ => return Ok(None),
        };
        let name = path.name.to_string();
        let rows = self
            .client
            .query(
                "with target as ( \
                   select c.oid, 'pg_class'::regclass as classid \
                   from pg_class c join pg_namespace n on n.oid = c.relnamespace \
                   where $3 = 'pg_class' and n.nspname = $1 and c.relname = $2 \
                   union all \
                   select p.oid, 'pg_proc'::regclass \
                   from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
                   where $3 = 'pg_proc' and n.nspname = $1 and p.proname = $2 \
                   union all \
                   select t.oid, 'pg_trigger'::regclass \
                   from pg_trigger t join pg_class c on c.oid = t.tgrelid \
                   join pg_namespace n on n.oid = c.relnamespace \
                   where $3 = 'pg_trigger' and n.nspname = $1 and t.tgname = $2 \
                 ) \
                 select 'dep' as dir, io.type, io.schema, io.name, d.deptype \
                 from target \
                 join pg_depend d on d.classid = target.classid and d.objid = target.oid \
                 join lateral pg_identify_object(d.refclassid, d.refobjid, d.refobjsubid) io on true \
                 where d.deptype in ('n','a') \
                 union all \
                 select 'rev', io.type, io.schema, io.name, d.deptype \
                 from target \
                 join pg_depend d on d.refclassid = target.classid and d.refobjid = target.oid \
                 join lateral pg_identify_object(d.classid, d.objid, d.objsubid) io on true \
                 where d.deptype in ('n','a') \
                 limit 400",
                &[&schema, &name, &classid],
            )
            .await
            .map_err(map_pg_error)?;
        let mut out = DependencyList::default();
        for row in rows {
            let dir: String = row.get(0);
            let label: String = row.get(1);
            let Some(kind) = pg_dependency_kind(&label) else {
                continue;
            };
            let Some(ns_name) = row.get::<_, Option<String>>(2) else {
                continue;
            };
            if ns_name.starts_with("pg_") || ns_name == "information_schema" {
                continue;
            }
            let object_name: String = row.get(3);
            let dep_type: String = row.get(4);
            let info = DependencyInfo {
                path: ObjectPath::new(
                    Namespace::new(path.ns.catalog.clone(), Some(ns_name.clone())),
                    object_name,
                    kind,
                ),
                dependency_type: pg_dependency_type(&dep_type),
                remote: (ns_name != schema).then_some(ns_name),
            };
            if dir == "dep" {
                out.push_depends_on(info);
            } else {
                out.push_depended_by(info);
            }
        }
        out.note = Some("仅直接依赖（pg_depend），系统 schema 已忽略".into());
        Ok(Some(out))
    }

    async fn table(&self, path: &ObjectPath) -> Result<TableDetail, DriverError> {
        let mut columns = self.columns(path).await?;
        let indexes = self.indexes(path).await.unwrap_or_default();
        // 主键只出现在索引里；回填到列上，UI 才能据此生成行定位条件。
        let primary: Vec<&str> = indexes
            .iter()
            .filter(|index| index.primary)
            .flat_map(|index| index.columns.iter().map(String::as_str))
            .collect();
        for column in &mut columns {
            if primary
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&column.meta.name))
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
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers,
            engine: None,
            collation: None,
            comment: None,
            ddl_source: rustdb_db_api::DdlSource::Unavailable,
        })
    }

    /// PG 没有「取出建表语句」的内置函数：
    /// 视图 / 函数 / 物化视图走 `pg_get_*def`（原生），表按列信息合成。
    async fn object_ddl(
        &self,
        path: &ObjectPath,
    ) -> Result<Option<rustdb_db_api::ObjectDdl>, DriverError> {
        use rustdb_db_api::{DdlSource, ObjectDdl, ObjectKind};
        let Some(schema) = path.ns.schema.as_deref() else {
            return Ok(None);
        };
        let name = path.name.to_string();

        let query = match path.kind {
            ObjectKind::View => {
                Some("select 'CREATE OR REPLACE VIEW ' || quote_ident($1) || '.' || quote_ident($2) \
                       || ' AS ' || pg_get_viewdef(format('%I.%I', $1::text, $2::text)::regclass, true) || ';'")
            }
            ObjectKind::MaterializedView => {
                Some("select 'CREATE MATERIALIZED VIEW ' || quote_ident($1) || '.' || quote_ident($2) \
                       || ' AS ' || pg_get_viewdef(format('%I.%I', $1::text, $2::text)::regclass, true) || ';'")
            }
            ObjectKind::Function | ObjectKind::Procedure => {
                Some("select pg_get_functiondef(p.oid) \
                      from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
                      where n.nspname = $1 and p.proname = $2 limit 1")
            }
            ObjectKind::Trigger => {
                Some("select pg_get_triggerdef(t.oid) \
                      from pg_trigger t \
                      join pg_class c on c.oid = t.tgrelid \
                      join pg_namespace n on n.oid = c.relnamespace \
                      where n.nspname = $1 and t.tgname = $2 limit 1")
            }
            _ => None,
        };

        if let Some(sql) = query {
            let rows = self
                .client
                .query(sql, &[&schema, &name])
                .await
                .map_err(map_pg_error)?;
            let text: Option<String> = rows.first().map(|row| row.get(0));
            if let Some(text) = text {
                return Ok(Some(ObjectDdl {
                    text,
                    source: DdlSource::Native,
                }));
            }
            return Ok(None);
        }

        // 表 / 序列 / 类型：按元数据合成。
        if path.kind != ObjectKind::Table {
            return Ok(None);
        }
        let columns = self.columns(path).await?;
        if columns.is_empty() {
            return Ok(None);
        }
        let quote = |ident: &str| format!("\"{}\"", ident.replace('"', "\"\""));
        let body: Vec<String> = columns
            .iter()
            .map(|column| {
                let mut line = format!(
                    "    {} {}",
                    quote(&column.meta.name),
                    column.meta.native_type
                );
                if column.is_auto_increment {
                    line.push_str(" GENERATED BY DEFAULT AS IDENTITY");
                }
                if let Some(default) = &column.default {
                    line.push_str(&format!(" DEFAULT {default}"));
                }
                if !column.meta.nullable {
                    line.push_str(" NOT NULL");
                }
                line
            })
            .collect();
        let text = format!(
            "CREATE TABLE {}.{} (\n{}\n);",
            quote(schema),
            quote(&name),
            body.join(",\n")
        );
        Ok(Some(ObjectDdl {
            text,
            source: DdlSource::Synthesized,
        }))
    }
}

/// `pg_identify_object` 的类型标签 → 统一对象种类；未知标签直接忽略。
fn pg_dependency_kind(label: &str) -> Option<ObjectKind> {
    Some(match label {
        "table" | "partitioned table" | "foreign table" => ObjectKind::Table,
        "view" => ObjectKind::View,
        "materialized view" => ObjectKind::MaterializedView,
        "sequence" => ObjectKind::Sequence,
        "index" => ObjectKind::Index,
        "function" => ObjectKind::Function,
        "procedure" => ObjectKind::Procedure,
        "trigger" => ObjectKind::Trigger,
        "type" | "domain" | "composite type" => ObjectKind::Type,
        _ => return None,
    })
}

/// `pg_depend.deptype` → 展示用依赖类别。
fn pg_dependency_type(deptype: &str) -> String {
    match deptype {
        "n" => "普通",
        "a" => "自动",
        "i" => "内部",
        "e" => "扩展",
        "p" => "分区",
        other => other,
    }
    .to_string()
}

fn logical_from_pg(data_type: &str) -> LogicalType {
    match data_type {
        "boolean" => LogicalType::Bool,
        "smallint" => LogicalType::Int {
            bits: 16,
            unsigned: false,
        },
        "integer" => LogicalType::Int {
            bits: 32,
            unsigned: false,
        },
        "bigint" => LogicalType::Int {
            bits: 64,
            unsigned: false,
        },
        "real" => LogicalType::Float { bits: 32 },
        "double precision" => LogicalType::Float { bits: 64 },
        "numeric" | "decimal" => LogicalType::Decimal {
            precision: None,
            scale: None,
        },
        "text" | "character varying" | "character" => LogicalType::Text { max_len: None },
        "bytea" => LogicalType::Bytes,
        "date" => LogicalType::Date,
        "time without time zone" | "time with time zone" => LogicalType::Time,
        "timestamp without time zone" => LogicalType::Timestamp,
        "timestamp with time zone" => LogicalType::TimestampTz,
        "uuid" => LogicalType::Uuid,
        "json" | "jsonb" => LogicalType::Json,
        "ARRAY" => LogicalType::Array(Box::new(LogicalType::Unknown("element".into()))),
        other => LogicalType::Unknown(other.to_string()),
    }
}

/// 建立连接并在后台驱动协议 IO；TLS 与明文共用同一段收尾逻辑。
async fn open<T>(config: tokio_postgres::Config, tls: T) -> Result<Client, DriverError>
where
    T: tokio_postgres::tls::MakeTlsConnect<tokio_postgres::Socket> + Send + 'static,
    T::TlsConnect: Send,
    T::Stream: Send + 'static,
{
    let (client, connection) = config.connect(tls).await.map_err(map_pg_error)?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            log::warn!("PostgreSQL 连接断开：{error}");
        }
    });
    Ok(client)
}

fn map_pg_error(err: tokio_postgres::Error) -> DriverError {
    if let Some(db) = err.as_db_error() {
        let code = db.code().code().to_string();
        let kind = if code.starts_with("42") {
            DriverErrorKind::Syntax
        } else if code.starts_with("23") {
            DriverErrorKind::Constraint
        } else if code == "42501" {
            DriverErrorKind::Permission
        } else if code == "3D000" || code == "42P01" {
            DriverErrorKind::NotFound
        } else if code.starts_with("40") {
            DriverErrorKind::Deadlock
        } else if code == "57014" {
            DriverErrorKind::Cancelled
        } else if code == "25006" {
            DriverErrorKind::ReadOnly
        } else {
            DriverErrorKind::Protocol
        };
        let mut mapped =
            DriverError::new(kind, db.message().to_string()).with_sqlstate(code.clone());
        if let Some(detail) = db.detail() {
            mapped = mapped.with_hint(detail.to_string());
        }
        // `P` 字段是原始语句内的 1-based 字符位置；内部生成语句的位置对编辑器没有意义。
        if let Some(tokio_postgres::error::ErrorPosition::Original(position)) = db.position() {
            mapped.position = Some(*position);
        }
        return mapped;
    }
    if err.is_closed() {
        DriverError::connection(err.to_string())
    } else {
        DriverError::protocol(err.to_string())
    }
}

/// 外键查询（F-73）：按列展开，复合外键由调用方按约束名合并。
fn foreign_keys_sql() -> &'static str {
    "select c.conname, a.attname, rt.relname, ra.attname, \
            case c.confdeltype when 'a' then 'NO ACTION' when 'r' then 'RESTRICT' \
                 when 'c' then 'CASCADE' when 'n' then 'SET NULL' \
                 when 'd' then 'SET DEFAULT' else 'NO ACTION' end, \
            case c.confupdtype when 'a' then 'NO ACTION' when 'r' then 'RESTRICT' \
                 when 'c' then 'CASCADE' when 'n' then 'SET NULL' \
                 when 'd' then 'SET DEFAULT' else 'NO ACTION' end \
     from pg_constraint c \
     join pg_class t on t.oid = c.conrelid \
     join pg_namespace n on n.oid = t.relnamespace \
     join pg_class rt on rt.oid = c.confrelid \
     join lateral unnest(c.conkey) with ordinality k(attnum, ord) on true \
     join lateral unnest(c.confkey) with ordinality rk(attnum, ord) on rk.ord = k.ord \
     join pg_attribute a on a.attrelid = t.oid and a.attnum = k.attnum \
     join pg_attribute ra on ra.attrelid = rt.oid and ra.attnum = rk.attnum \
     where c.contype = 'f' and n.nspname = $1 and t.relname = $2 \
     order by c.conname, k.ord"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_labels_map_to_object_kinds() {
        assert_eq!(pg_dependency_kind("view"), Some(ObjectKind::View));
        assert_eq!(
            pg_dependency_kind("materialized view"),
            Some(ObjectKind::MaterializedView)
        );
        assert_eq!(pg_dependency_kind("foreign table"), Some(ObjectKind::Table));
        assert_eq!(pg_dependency_kind("function"), Some(ObjectKind::Function));
        assert_eq!(pg_dependency_kind("domain"), Some(ObjectKind::Type));
        assert_eq!(pg_dependency_kind("operator"), None);

        assert_eq!(pg_dependency_type("n"), "普通");
        assert_eq!(pg_dependency_type("p"), "分区");
        assert_eq!(pg_dependency_type("x"), "x");
    }

    #[test]
    fn foreign_keys_sql_targets_pg_constraint_and_keeps_pairing() {
        let sql = foreign_keys_sql();
        assert!(sql.contains("pg_constraint"), "{sql}");
        assert!(sql.contains("c.contype = 'f'"), "{sql}");
        assert!(sql.contains("conkey"), "{sql}");
        assert!(sql.contains("confkey"), "{sql}");
        assert!(sql.contains("n.nspname = $1 and t.relname = $2"), "{sql}");
    }
}
