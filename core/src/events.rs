//! 统一领域事件。
//!
//! 事件是**既成事实**、纯数据、可序列化；UI 只订阅 `AppEvent` 并按 `source` 分派。
//! 行数据**不进入事件**：只传 `ResultSetId` 与计数，UI 按需从 `ResultStore` 取页。
//! 设计依据见需求文档第 9 章。

use chrono::Utc;
use parking_lot::Mutex;
use rustdb_db_api::{Capabilities, DbKind, DriverErrorKind, QueryId, ServerInfo, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// 全局单调，仅用于诊断排序。
    pub seq: u64,
    /// 会话内单调；缺失即说明该会话丢过事件。
    pub session_seq: Option<u64>,
    pub ts_ms: i64,
    pub session_id: Option<SessionId>,
    pub event: AppEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventError {
    pub kind: DriverErrorKind,
    pub message: String,
    pub sqlstate: Option<String>,
    pub code: Option<i64>,
    /// 驱动给出的错误位置（1-based 字符偏移，相对本次执行的语句）。
    #[serde(default)]
    pub position: Option<u32>,
    pub hint: Option<String>,
    pub retryable: bool,
}

impl From<&rustdb_db_api::DriverError> for EventError {
    fn from(e: &rustdb_db_api::DriverError) -> Self {
        Self {
            kind: e.kind,
            message: e.message.clone(),
            sqlstate: e.sqlstate.clone(),
            code: e.code,
            position: e.position,
            hint: e.hint.clone(),
            retryable: e.retryable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnEvent {
    Connecting {
        profile_id: String,
        attempt: u32,
    },
    Ready {
        session_id: SessionId,
        server: ServerInfo,
        capabilities: Capabilities,
    },
    Failed {
        profile_id: String,
        error_kind: DriverErrorKind,
        message: String,
    },
    Lost {
        session_id: SessionId,
        reason: String,
    },
    NamespaceChanged {
        session_id: SessionId,
        namespace: rustdb_db_api::Namespace,
    },
    NamespaceSwitchFailed {
        session_id: SessionId,
        message: String,
    },
    Closed {
        session_id: SessionId,
        by_user: bool,
    },
    /// F-91：隧道主机密钥待确认（首次连接或密钥变更）；确认前不会发送任何凭据。
    TunnelHostKey {
        profile_id: String,
        /// 第几层跳板（1 起）。
        layer: usize,
        host: Option<String>,
        port: Option<u16>,
        /// 本次拿到的 SHA-256 指纹。
        fingerprint: String,
        /// 本次的 OpenSSH 公钥行。
        public_key: String,
        /// true = 与已有记录不一致（可能是中间人替换）。
        changed: bool,
        /// 变更时的旧指纹。
        recorded: Option<String>,
    },
    /// F-91：隧道已建立，本地端口转发就绪。
    TunnelReady {
        profile_id: String,
        /// 建链顺序摘要。
        chain: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultSummary {
    pub set_id: crate::results::ResultSetId,
    pub columns: usize,
    pub rows: u64,
    pub truncated: bool,
    /// 该语句影响的行数（DML 才有）。变更集提交后用它判定乐观并发冲突。
    pub affected: Option<u64>,
}

/// 导入失败批次（F-74）：`row` 区间为 1-based 数据行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFailure {
    pub index: usize,
    pub start_row: usize,
    pub end_row: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryEvent {
    Submitted {
        request_id: String,
        session_id: SessionId,
        sql_preview: String,
        statement_kind: String,
    },
    Started {
        query_id: QueryId,
        request_id: String,
    },
    /// 批量执行时，即将开始第 `index` 条（1 起）；单条执行也会发一次。
    StatementStarted {
        query_id: QueryId,
        index: usize,
        total: usize,
    },
    RowsAppended {
        query_id: QueryId,
        set_id: crate::results::ResultSetId,
        loaded: u64,
        total_hint: Option<u64>,
        bytes: usize,
    },
    /// 服务端告警 / `NOTICE` / `RAISE NOTICE`：原文保留，一条一个事件。
    Notice {
        query_id: QueryId,
        level: rustdb_db_api::WarningLevel,
        message: String,
    },
    Completed {
        query_id: QueryId,
        elapsed_ms: u64,
        affected: Option<u64>,
        results: Vec<ResultSummary>,
        /// 本次执行的告警条数（原文见同 `query_id` 的 `Notice` 事件）。
        warnings: u32,
    },
    Failed {
        query_id: QueryId,
        error: EventError,
    },
    Cancelling {
        query_id: QueryId,
    },
    Cancelled {
        query_id: QueryId,
        elapsed_ms: u64,
        /// 被中断的语句序号（1-based）；0 表示取消发生在语句之间或未知。
        #[serde(default)]
        at: usize,
        /// 本批总语句数；0 表示旧事件或未知。
        #[serde(default)]
        total: usize,
    },
    /// 导入（F-74）：每批开始前发一次，UI 显示行进度。
    ImportProgress {
        query_id: QueryId,
        batch: usize,
        total_batches: usize,
        done_rows: u64,
        total_rows: u64,
    },
    /// 导入结束（成功 / 部分失败 / 取消）。失败批次逐条列出，不静默跳过。
    ImportFinished {
        query_id: QueryId,
        elapsed_ms: u64,
        total_rows: u64,
        inserted_rows: u64,
        failed_rows: u64,
        failed_batches: Vec<ImportFailure>,
        /// 是否执行了清空（`ImportMode::Truncate`）。
        truncated: bool,
        cancelled: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TxEvent {
    Began {
        tx_id: String,
        read_only: bool,
    },
    Committed {
        tx_id: String,
    },
    RolledBack {
        tx_id: String,
        reason: String,
    },
    /// 事务命令失败（驱动不支持、连接已断等）。
    Failed {
        action: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetaEvent {
    NamespacesLoaded {
        namespaces: Vec<rustdb_db_api::Namespace>,
        current: rustdb_db_api::Namespace,
    },
    LoadStarted {
        namespace: String,
    },
    Loaded {
        namespace: String,
        /// 该命名空间下的全部对象（表 / 视图 / 存储过程 …）。
        objects: Vec<rustdb_db_api::ObjectSummary>,
    },
    LoadFailed {
        namespace: String,
        message: String,
    },
    /// 对象的 DDL 已取到（`source` 标注原生 / 合成）。
    DdlLoaded {
        path: String,
        ddl: rustdb_db_api::ObjectDdl,
    },
    DdlFailed {
        path: String,
        message: String,
    },
    /// 例程源码（视图 / 存储过程 / 函数 / 触发器 / 程序包）已取到（F-82）。
    RoutineSourceLoaded {
        path: String,
        text: String,
        source: rustdb_db_api::DdlSource,
    },
    RoutineSourceFailed {
        path: String,
        message: String,
    },
    /// 对象依赖（F-83）：只含直接依赖，不递归。
    DependenciesLoaded {
        path: String,
        dependencies: rustdb_db_api::DependencyList,
    },
    DependenciesFailed {
        path: String,
        message: String,
    },
    /// 分区列表（F-85）：只含一级分区。
    PartitionsLoaded {
        path: String,
        partitions: rustdb_db_api::PartitionList,
    },
    PartitionsFailed {
        path: String,
        message: String,
    },
    /// 表详情（列 / 索引 / 外键）已取到，用于「设计表」面板。
    TableDetailLoaded {
        path: String,
        detail: rustdb_db_api::TableDetail,
    },
    /// 数据库文档输入（F-87）：表 / 列 / 索引 / 外键已汇总；只读，不执行 SQL。
    DocumentLoaded {
        namespace: String,
        input: crate::docs::DocInput,
    },
    DocumentFailed {
        namespace: String,
        message: String,
    },
    /// 字段血缘输入（F-88）：表 / 列 / 外键、视图定义与历史 SQL；只读。
    LineageLoaded {
        namespace: String,
        input: crate::lineage::LineageInput,
    },
    LineageFailed {
        namespace: String,
        message: String,
    },
    /// 结构对比快照（F-86）：表 / 列 / 索引 / 外键；只读。
    SchemaSnapshotLoaded {
        /// 请求方指定的槽位（source / target），原样回传，避免并发请求串台。
        slot: String,
        snapshot: crate::compare::SchemaSnapshot,
    },
    SchemaSnapshotFailed {
        slot: String,
        message: String,
    },
    TableDetailFailed {
        path: String,
        message: String,
    },
}

/// 整库逻辑导出（F-76）：事件只带计数与表名，行数据始终走文件，不进事件总线。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DumpEvent {
    Started {
        namespace: String,
        tables: usize,
        path: String,
        gzip: bool,
    },
    TableDone {
        table: String,
        rows: u64,
        error: Option<String>,
    },
    ObjectDone {
        object: String,
        error: Option<String>,
    },
    Finished {
        path: String,
        tables: usize,
        objects: usize,
        rows: u64,
        failed: usize,
        bytes: u64,
    },
    Failed {
        message: String,
    },
}

/// 跨库数据传输（F-75）：只带进度与失败文案，行数据不经过事件总线。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransferEvent {
    Started {
        source: String,
        target: String,
        tables: usize,
        mode: String,
        backup: bool,
    },
    /// 执行前预览：逐表结构 SQL、列映射与警告，不写任何数据。
    Preview {
        lines: Vec<String>,
        warnings: Vec<String>,
    },
    BackupDone {
        path: String,
        bytes: u64,
    },
    TableStarted {
        table: String,
        index: usize,
    },
    TableProgress {
        table: String,
        rows: u64,
    },
    TableDone {
        table: String,
        rows: u64,
        error: Option<String>,
    },
    Finished {
        tables: usize,
        rows: u64,
        failed: usize,
        elapsed_ms: u64,
    },
    Failed {
        message: String,
    },
}

/// 数据对比与同步（F-78）：差异计数、脚本预览与逐表进度；行数据不经过事件总线。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataCompareEvent {
    Started {
        source: String,
        target: String,
        tables: usize,
        apply: bool,
    },
    TableStarted {
        table: String,
        index: usize,
    },
    TableDone {
        table: String,
        insert: u64,
        update: u64,
        delete: u64,
        same: u64,
        warnings: Vec<String>,
        error: Option<String>,
    },
    /// 对比结束后的同步脚本预览；不写任何数据。
    Preview {
        lines: Vec<String>,
        warnings: Vec<String>,
        tables: usize,
        insert: u64,
        update: u64,
        delete: u64,
    },
    SyncStarted {
        statements: u64,
    },
    SyncProgress {
        table: String,
        applied: u64,
        statements: u64,
    },
    SyncTableFailed {
        table: String,
        message: String,
    },
    SyncDone {
        applied: u64,
        failed: usize,
    },
    Finished {
        tables: usize,
        insert: u64,
        update: u64,
        delete: u64,
        elapsed_ms: u64,
        failed: usize,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiEvent {
    Started {
        run_id: String,
    },
    Thinking {
        run_id: String,
    },
    /// 模型文本增量（UI 追加到当前助手气泡）。
    Delta {
        run_id: String,
        text: String,
    },
    ToolStarted {
        run_id: String,
        call_id: String,
        tool: String,
        risk: crate::agent::RiskLevel,
    },
    ToolCompleted {
        run_id: String,
        call_id: String,
        ok: bool,
        summary: String,
    },
    /// 写/破坏性工具：循环在此挂起，等 UI 发确认命令。
    ConfirmationRequired {
        run_id: String,
        call_id: String,
        tool: String,
        risk: crate::agent::RiskLevel,
        summary: String,
    },
    Completed {
        run_id: String,
    },
    Failed {
        run_id: String,
        message: String,
    },
    Cancelled {
        run_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SystemEvent {
    AppReady { cold_start_ms: u64 },
    DriverRegistered { driver: DbKind },
    StorageWarning { path: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AppEvent {
    Conn(ConnEvent),
    Query(QueryEvent),
    Tx(TxEvent),
    Meta(MetaEvent),
    Dump(DumpEvent),
    Transfer(TransferEvent),
    DataCompare(DataCompareEvent),
    Ai(AiEvent),
    System(SystemEvent),
}

impl AppEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Query(QueryEvent::Completed { .. })
                | Self::Query(QueryEvent::Failed { .. })
                | Self::Query(QueryEvent::Cancelled { .. })
                | Self::Query(QueryEvent::ImportFinished { .. })
                | Self::Conn(ConnEvent::Failed { .. })
                | Self::Conn(ConnEvent::Closed { .. })
                | Self::Ai(AiEvent::Completed { .. })
                | Self::Ai(AiEvent::Failed { .. })
                | Self::Ai(AiEvent::Cancelled { .. })
                | Self::Dump(DumpEvent::Finished { .. })
                | Self::Dump(DumpEvent::Failed { .. })
                | Self::Transfer(TransferEvent::Finished { .. })
                | Self::Transfer(TransferEvent::Failed { .. })
                | Self::DataCompare(DataCompareEvent::Finished { .. })
                | Self::DataCompare(DataCompareEvent::Failed { .. })
        )
    }

    /// 状态栏/通知条的一行描述。
    pub fn describe(&self) -> String {
        match self {
            Self::Conn(ConnEvent::Connecting { .. }) => "正在连接…".into(),
            Self::Conn(ConnEvent::Ready { server, .. }) => {
                format!(
                    "已连接：{} {}",
                    server.version,
                    server.current_db.clone().unwrap_or_default()
                )
            }
            Self::Conn(ConnEvent::Failed { message, .. }) => format!("连接失败：{message}"),
            Self::Conn(ConnEvent::Lost { reason, .. }) => format!("连接已断开：{reason}"),
            Self::Conn(ConnEvent::NamespaceChanged { namespace, .. }) => {
                format!("已切换到 {}", namespace.display_label())
            }
            Self::Conn(ConnEvent::NamespaceSwitchFailed { message, .. }) => {
                format!("切换数据库失败：{message}")
            }
            Self::Conn(ConnEvent::Closed { .. }) => "连接已关闭".into(),
            Self::Conn(ConnEvent::TunnelHostKey {
                layer,
                fingerprint,
                changed,
                ..
            }) => {
                if *changed {
                    format!("第 {layer} 层主机密钥已变更（{fingerprint}），请确认后继续")
                } else {
                    format!("第 {layer} 层主机密钥未知（{fingerprint}），请确认后继续")
                }
            }
            Self::Conn(ConnEvent::TunnelReady { chain, .. }) => format!("链路已建立：{chain}"),
            Self::Query(QueryEvent::Submitted { .. }) => "查询已提交".into(),
            Self::Query(QueryEvent::Started { .. }) => "正在执行…".into(),
            Self::Query(QueryEvent::StatementStarted { index, total, .. }) => {
                if *total > 1 {
                    format!("正在执行第 {index}/{total} 条")
                } else {
                    "正在执行…".into()
                }
            }
            Self::Query(QueryEvent::RowsAppended { loaded, .. }) => {
                format!("已加载 {loaded} 行")
            }
            Self::Query(QueryEvent::Notice { message, .. }) => {
                format!("服务端消息：{message}")
            }
            Self::Query(QueryEvent::Completed { elapsed_ms, warnings, .. }) => {
                if *warnings == 0 {
                    format!("执行完成（{elapsed_ms} ms）")
                } else {
                    format!("执行完成（{elapsed_ms} ms，{warnings} 条消息）")
                }
            }
            Self::Query(QueryEvent::Failed { error, .. }) => {
                format!("执行失败：{}", error.message)
            }
            Self::Query(QueryEvent::Cancelling { .. }) => "正在取消…".into(),
            Self::Query(QueryEvent::Cancelled { at, total, .. }) if *at > 0 && *total > 0 => {
                let done = at.saturating_sub(1);
                let remaining = total.saturating_sub(*at);
                format!("已取消（成功 {done} 条 · 未执行 {remaining} 条）")
            }
            Self::Query(QueryEvent::Cancelled { .. }) => "查询已取消".into(),
            Self::Query(QueryEvent::ImportProgress {
                batch,
                total_batches,
                done_rows,
                total_rows,
                ..
            }) => format!("正在导入第 {batch}/{total_batches} 批（{done_rows}/{total_rows} 行）"),
            Self::Query(QueryEvent::ImportFinished {
                total_rows,
                inserted_rows,
                failed_rows,
                cancelled,
                ..
            }) if *cancelled => format!("导入已取消（成功 {inserted_rows}/{total_rows} 行）"),
            Self::Query(QueryEvent::ImportFinished {
                total_rows,
                inserted_rows,
                failed_rows,
                ..
            }) if *failed_rows > 0 => format!(
                "导入完成：成功 {inserted_rows} 行，失败 {failed_rows} 行（共 {total_rows} 行）"
            ),
            Self::Query(QueryEvent::ImportFinished { inserted_rows, .. }) => {
                format!("导入完成：{inserted_rows} 行")
            }
            Self::Tx(TxEvent::Began { .. }) => "事务已开始".into(),
            Self::Tx(TxEvent::Committed { .. }) => "事务已提交".into(),
            Self::Tx(TxEvent::RolledBack { .. }) => "事务已回滚".into(),
            Self::Tx(TxEvent::Failed { action, message }) => {
                format!("{action}失败：{message}")
            }
            Self::Meta(MetaEvent::NamespacesLoaded { .. }) => "已加载数据库列表".into(),
            Self::Meta(MetaEvent::LoadStarted { .. }) => "正在加载元数据…".into(),
            Self::Meta(MetaEvent::Loaded { objects, .. }) => {
                format!("元数据已加载（{} 个对象）", objects.len())
            }
            Self::Meta(MetaEvent::LoadFailed { message, .. }) => {
                format!("元数据加载失败：{message}")
            }
            Self::Meta(MetaEvent::DdlLoaded { path, ddl }) => {
                format!("已取到 {path} 的 DDL（{}）", ddl.source.label())
            }
            Self::Meta(MetaEvent::DdlFailed { path, message }) => {
                format!("{path} 的 DDL 获取失败：{message}")
            }
            Self::Meta(MetaEvent::RoutineSourceLoaded { path, source, .. }) => {
                format!("已取到 {path} 的源码（{}）", source.label())
            }
            Self::Meta(MetaEvent::RoutineSourceFailed { path, message }) => {
                format!("{path} 的源码获取失败：{message}")
            }
            Self::Meta(MetaEvent::DependenciesLoaded { path, dependencies }) => {
                format!(
                    "{path}：{} 项依赖 / {} 项被依赖",
                    dependencies.depends_on.len(),
                    dependencies.depended_by.len()
                )
            }
            Self::Meta(MetaEvent::DependenciesFailed { path, message }) => {
                format!("{path} 的依赖获取失败：{message}")
            }
            Self::Meta(MetaEvent::PartitionsLoaded { path, partitions }) => {
                format!(
                    "{path}：{} 个分区（{}）",
                    partitions.partitions.len(),
                    partitions
                        .strategy
                        .clone()
                        .unwrap_or_else(|| "非分区表".into())
                )
            }
            Self::Meta(MetaEvent::PartitionsFailed { path, message }) => {
                format!("{path} 的分区获取失败：{message}")
            }
            Self::Meta(MetaEvent::TableDetailLoaded { path, detail }) => {
                format!(
                    "{path}：{} 列 / {} 索引",
                    detail.columns.len(),
                    detail.indexes.len()
                )
            }
            Self::Meta(MetaEvent::TableDetailFailed { path, message }) => {
                format!("{path} 的表结构加载失败：{message}")
            }
            Self::Meta(MetaEvent::DocumentLoaded { namespace, input }) => {
                format!("{namespace}：文档汇总 {} 张表", input.tables.len())
            }
            Self::Meta(MetaEvent::DocumentFailed { namespace, message }) => {
                format!("{namespace} 的文档汇总失败：{message}")
            }
            Self::Meta(MetaEvent::LineageLoaded { namespace, input }) => {
                format!("{namespace}：血缘快照 {} 张表", input.tables.len())
            }
            Self::Meta(MetaEvent::LineageFailed { namespace, message }) => {
                format!("{namespace} 的血缘汇总失败：{message}")
            }
            Self::Dump(DumpEvent::Started {
                namespace,
                tables,
                path,
                gzip,
            }) => format!(
                "开始导出 {namespace}：{tables} 张表 → {path}{}",
                if *gzip { "（gzip）" } else { "" }
            ),
            Self::Dump(DumpEvent::TableDone { table, rows, error }) => match error {
                Some(error) => format!("导出 {table} 失败：{error}"),
                None => format!("已导出 {table}：{rows} 行"),
            },
            Self::Dump(DumpEvent::ObjectDone { object, error }) => match error {
                Some(error) => format!("导出 {object} 失败：{error}"),
                None => format!("已导出 {object}"),
            },
            Self::Dump(DumpEvent::Finished {
                path,
                tables,
                objects,
                rows,
                failed,
                bytes,
            }) => format!(
                "导出完成：{tables} 张表 / {objects} 个对象 / {rows} 行 / {failed} 个失败（{}，{path}）",
                human_bytes(*bytes)
            ),
            Self::Dump(DumpEvent::Failed { message }) => format!("导出失败：{message}"),
            Self::Transfer(TransferEvent::Started {
                source,
                target,
                tables,
                mode,
                backup,
            }) => format!(
                "开始传输 {source} → {target}：{tables} 张表（{mode}{}）",
                if *backup { "，重建前备份" } else { "" }
            ),
            Self::Transfer(TransferEvent::Preview { lines, .. }) => {
                format!("传输预览：{} 段", lines.len())
            }
            Self::Transfer(TransferEvent::BackupDone { path, bytes }) => {
                format!("已备份目标表：{}（{path}）", human_bytes(*bytes))
            }
            Self::Transfer(TransferEvent::TableStarted { table, index }) => {
                format!("正在传输第 {index} 张表：{table}")
            }
            Self::Transfer(TransferEvent::TableProgress { table, rows }) => {
                format!("{table} 已写入 {rows} 行")
            }
            Self::Transfer(TransferEvent::TableDone { table, rows, error }) => match error {
                Some(error) => format!("传输 {table} 失败：{error}"),
                None => format!("已传输 {table}：{rows} 行"),
            },
            Self::Transfer(TransferEvent::Finished {
                tables,
                rows,
                failed,
                elapsed_ms,
            }) => format!(
                "传输完成：{tables} 张表 / {rows} 行 / {failed} 个失败（{elapsed_ms} ms）"
            ),
            Self::Transfer(TransferEvent::Failed { message }) => format!("传输失败：{message}"),
            Self::DataCompare(DataCompareEvent::Started {
                source,
                target,
                tables,
                apply,
            }) => format!(
                "开始数据对比 {source} → {target}：{tables} 张表{}",
                if *apply { "（对比后同步）" } else { "" }
            ),
            Self::DataCompare(DataCompareEvent::TableStarted { table, index }) => {
                format!("正在对比第 {index} 张表：{table}")
            }
            Self::DataCompare(DataCompareEvent::TableDone {
                table,
                insert,
                update,
                delete,
                same,
                error,
                ..
            }) => match error {
                Some(error) => format!("对比 {table} 失败：{error}"),
                None => format!(
                    "已对比 {table}：新增 {insert} / 更新 {update} / 删除 {delete} / 相同 {same}"
                ),
            },
            Self::DataCompare(DataCompareEvent::Preview {
                tables,
                insert,
                update,
                delete,
                ..
            }) => format!(
                "数据对比预览：{tables} 张表 / 新增 {insert} / 更新 {update} / 删除 {delete}"
            ),
            Self::DataCompare(DataCompareEvent::SyncStarted { statements }) => {
                format!("开始同步：{statements} 条语句")
            }
            Self::DataCompare(DataCompareEvent::SyncProgress {
                table,
                applied,
                statements,
            }) => format!("同步 {table}：{applied}/{statements} 条"),
            Self::DataCompare(DataCompareEvent::SyncTableFailed { table, message }) => {
                format!("同步 {table} 失败：{message}")
            }
            Self::DataCompare(DataCompareEvent::SyncDone { applied, failed }) => {
                format!("同步结束：{applied} 条成功 / {failed} 个失败")
            }
            Self::DataCompare(DataCompareEvent::Finished {
                tables,
                insert,
                update,
                delete,
                elapsed_ms,
                failed,
            }) => format!(
                "对比完成：{tables} 张表 / 新增 {insert} / 更新 {update} / 删除 {delete} / {failed} 个失败（{elapsed_ms} ms）"
            ),
            Self::DataCompare(DataCompareEvent::Failed { message }) => {
                format!("数据对比失败：{message}")
            }
            Self::Meta(MetaEvent::SchemaSnapshotLoaded { slot, snapshot }) => {
                format!("{slot} 结构快照：{} 张表", snapshot.tables.len())
            }
            Self::Meta(MetaEvent::SchemaSnapshotFailed { slot, message }) => {
                format!("{slot} 结构快照生成失败：{message}")
            }
            Self::Ai(AiEvent::Started { .. }) => "AI 开始处理".into(),
            Self::Ai(AiEvent::Thinking { .. }) => "AI 正在思考…".into(),
            Self::Ai(AiEvent::Delta { .. }) => "AI 正在输出…".into(),
            Self::Ai(AiEvent::ToolStarted { tool, risk, .. }) => {
                format!("AI 调用工具 {tool}（{}）", risk.label())
            }
            Self::Ai(AiEvent::ToolCompleted { ok, summary, .. }) => {
                format!("工具{}：{summary}", if *ok { "完成" } else { "被拒绝" })
            }
            Self::Ai(AiEvent::ConfirmationRequired { summary, .. }) => {
                format!("等待确认：{summary}")
            }
            Self::Ai(AiEvent::Completed { .. }) => "AI 已完成".into(),
            Self::Ai(AiEvent::Failed { message, .. }) => format!("AI 失败：{message}"),
            Self::Ai(AiEvent::Cancelled { .. }) => "AI 已取消".into(),
            Self::System(SystemEvent::AppReady { cold_start_ms }) => {
                format!("启动完成（{cold_start_ms} ms）")
            }
            Self::System(SystemEvent::DriverRegistered { driver }) => {
                format!("驱动已注册：{}", driver.display_name())
            }
            Self::System(SystemEvent::StorageWarning { message, .. }) => {
                format!("存储警告：{message}")
            }
        }
    }
}

/// 人类可读的字节数（导出完成提示用）。
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

struct BusInner {
    tx: broadcast::Sender<Arc<EventEnvelope>>,
    seq: AtomicU64,
    session_seq: Mutex<HashMap<SessionId, u64>>,
    closed: AtomicU64,
}

/// 事件总线：多订阅者、按会话有序、慢订阅者不阻塞生产者。
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<BusInner>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(BusInner {
                tx,
                seq: AtomicU64::new(1),
                session_seq: Mutex::new(HashMap::new()),
                closed: AtomicU64::new(0),
            }),
        }
    }

    pub fn publish(&self, event: AppEvent, session: Option<SessionId>) -> Arc<EventEnvelope> {
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        let session_seq = session.as_ref().map(|sid| {
            let mut map = self.inner.session_seq.lock();
            let next = map.entry(sid.clone()).or_insert(0);
            *next += 1;
            *next
        });
        let envelope = Arc::new(EventEnvelope {
            seq,
            session_seq,
            ts_ms: Utc::now().timestamp_millis(),
            session_id: session,
            event,
        });
        // 没有订阅者时 send 会失败：这不是错误（启动早期 / 窗口关闭）。
        let _ = self.inner.tx.send(envelope.clone());
        envelope
    }

    pub fn subscribe(&self) -> EventStream {
        EventStream {
            rx: self.inner.tx.subscribe(),
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner.tx.receiver_count()
    }

    pub fn closed(&self) -> u64 {
        self.inner.closed.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventRecvError {
    Empty,
    Lagged(u64),
    Closed,
}

/// UI 侧的事件流包装：不把 tokio 类型泄漏到 ui crate。
pub struct EventStream {
    rx: broadcast::Receiver<Arc<EventEnvelope>>,
}

impl EventStream {
    /// 异步等待一条事件（Agent 等查询结果用）。
    pub async fn recv_timeout(
        &mut self,
        wait: std::time::Duration,
    ) -> Result<Arc<EventEnvelope>, EventRecvError> {
        match tokio::time::timeout(wait, self.rx.recv()).await {
            Ok(Ok(envelope)) => Ok(envelope),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                Err(EventRecvError::Closed)
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                Err(EventRecvError::Lagged(n))
            }
            Err(_) => Err(EventRecvError::Empty),
        }
    }

    pub fn try_recv(&mut self) -> Result<Arc<EventEnvelope>, EventRecvError> {
        match self.rx.try_recv() {
            Ok(v) => Ok(v),
            Err(broadcast::error::TryRecvError::Empty) => Err(EventRecvError::Empty),
            Err(broadcast::error::TryRecvError::Closed) => Err(EventRecvError::Closed),
            Err(broadcast::error::TryRecvError::Lagged(n)) => Err(EventRecvError::Lagged(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_position_survives_event_conversion() {
        let mut error = rustdb_db_api::DriverError::new(DriverErrorKind::Syntax, "syntax error");
        error.position = Some(23);
        let event = EventError::from(&error);
        assert_eq!(event.position, Some(23));
        // 旧日志没有该字段时按 `None` 读取。
        let json = serde_json::to_string(&serde_json::json!({
            "kind": "syntax",
            "message": "x",
            "sqlstate": null,
            "code": null,
            "hint": null,
            "retryable": false,
        }))
        .unwrap();
        let decoded: EventError = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.position, None);
    }

    #[test]
    fn cancelled_event_reports_done_and_unexecuted_counts() {
        let event = AppEvent::Query(QueryEvent::Cancelled {
            query_id: QueryId::new(),
            elapsed_ms: 5,
            at: 2,
            total: 5,
        });
        let text = event.describe();
        assert!(text.contains("成功 1 条"), "{text}");
        assert!(text.contains("未执行 3 条"), "{text}");

        // 旧事件没有 at/total：按 0 读入，不回退成错误计数。
        let old: QueryEvent =
            serde_json::from_str(r#"{"kind":"cancelled","query_id":"q1","elapsed_ms":1}"#).unwrap();
        assert_eq!(
            old,
            QueryEvent::Cancelled {
                query_id: QueryId("q1".into()),
                elapsed_ms: 1,
                at: 0,
                total: 0,
            }
        );
        assert_eq!(
            AppEvent::Query(old).describe(),
            "查询已取消",
            "没有语句序号时不编造未执行数量"
        );
    }
}
