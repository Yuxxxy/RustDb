// 结构对比等深层嵌套的 serde 派生在测试构建下会超过默认 trait 求解深度。

pub mod agent;
pub mod ai_config;
pub mod ai_context;
pub mod bulk_update;
pub mod compare;
pub mod core;
pub mod data_compare;
pub mod docs;
pub mod dump;
pub mod edit;
pub mod events;
pub mod exchange;
pub mod export;
pub mod generate;
pub mod history;
pub mod import;
pub mod layout;
pub mod lineage;
pub mod migration;
pub mod optimize;
pub mod params;
pub mod partition;
pub mod preview;
pub mod profile;
pub mod profiles;
pub mod results;
pub mod review;
pub mod routine;
pub mod runs;
pub mod secrets;
pub mod session_actor;
pub mod theme;
pub mod transfer;
pub mod tunnel;

pub use agent::{
    AgentEngine, AuditEntry, CliProvider, EngineLimits, LlmProvider, OpenAiProvider, RiskLevel,
    ScriptedProvider, ToolCall,
};
pub use ai_config::{
    classify_status, classify_transport, conclusion_markdown, discover_models, filter_models,
    is_chat_model, load_settings, parse_models, presets, probe, save_settings, settings_path,
    AiErrorKind, AiKind, AiProfile, AiSettings, AuthKind, ConnectivityReport, AI_CONFIG_VERSION,
    DEFAULT_MAX_TURNS, DEFAULT_RETRIES,
};
pub use ai_context::{
    answer_blocks, compose_prompt, fix_instruction, prepare, sql_block_at, table_item,
    truncate_body, AnswerBlock, ContextItem, ContextKind, PreparedContext, PreparedItem,
    MAX_CONTEXT_CHARS,
};
pub use bulk_update::{
    foreign_key_for_column, foreign_key_lookup_sql, BulkCondition, BulkFilter, BulkJoin, BulkOp,
    BulkUpdatePlan, BulkUpdateRequest,
};
pub use compare::{
    compare as compare_schemas, compare_config_path, load_compare_config, load_snapshot,
    native_type_for, save_compare_config, save_snapshot, ColumnDiff, ColumnShape, CompareConfig,
    CompareReport, DeployOptions, DiffCounts, DiffStatus, ForeignKeyDiff, IndexDiff,
    RenameCandidate, RenamePair, SchemaDeploy, SchemaSnapshot, TableDiff, TableSnapshot,
    COMPARE_CONFIG_VERSION, MAX_COMPARE_TABLES, MAX_RENAME_CANDIDATES,
};
pub use core::{Core, PendingHostKey};
pub use data_compare::{
    data_compare_config_path, export_data_compare_config, import_data_compare_config,
    load_data_compare_config, plan_compare_table, preview_lines as data_compare_preview_lines,
    save_data_compare_config, sync_statements as data_compare_sync_statements, DataCompareConfig,
    DataCompareOptions, DataCompareRequest, DataCompareTablePlan, TableCompareConfig,
    TableCompareResult, TableDiff as DataCompareTableDiff, DATA_COMPARE_CONFIG_VERSION,
    DEFAULT_DIFF_BATCH_ROWS, DEFAULT_SYNC_TIMEOUT_MS,
    MAX_COMPARE_TABLES as MAX_DATA_COMPARE_TABLES,
};
pub use docs::{
    generate as generate_document, load_notes as load_doc_notes, notes_path as doc_notes_path,
    save_notes as save_doc_notes, write_atomic as write_doc_atomic, DocInput, DocNoteStore,
    DocNotes, DocTable, Documentation, TableNote, MAX_DOC_TABLES,
};
pub use dump::{
    render_script as render_dump_script, DumpErrorMode, DumpInput, DumpObjectInput, DumpOptions,
    DumpSummary, DumpTableInput, DumpWriter, TableDumpOutcome, DEFAULT_INSERT_BATCH,
    MAX_DUMP_OBJECTS, MAX_DUMP_TABLES,
};
pub use edit::{parse_tsv, ChangeKind, ChangeSet, PlannedStatement, RowChange};
pub use events::{
    human_bytes, AiEvent, AppEvent, ConnEvent, DataCompareEvent, DumpEvent, EventBus,
    EventEnvelope, EventError, EventRecvError, EventStream, MetaEvent, QueryEvent, ResultSummary,
    SystemEvent, TransferEvent, TxEvent,
};
pub use exchange::{
    drafts_from_client_config, export as export_exchange, preview_bundle, profiles_from_preview,
    ExchangeError, ExchangePreview, ExchangeProfile, PreviewProfile, EXCHANGE_FORMAT,
    EXCHANGE_VERSION,
};
pub use export::{ExportError, ExportFormat, ExportOptions};
pub use generate::{generate, generate_seeded, parse_spec, GeneratorSpec};
pub use history::{
    HistoryEntry, HistoryFilter, HistorySource, HistoryStore, DEFAULT_HISTORY_LIMIT,
    HISTORY_LIMIT_CHOICES,
};
pub use layout::{
    clamp_ai_width, clamp_sidebar_width, layout_path, load_layout, save_layout, LayoutSettings,
    AI_WIDTH_DEFAULT, AI_WIDTH_MAX, AI_WIDTH_MIN, NARROW_WINDOW, SIDEBAR_WIDTH_DEFAULT,
    SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN,
};
pub use lineage::{
    filter_edges as filter_lineage_edges, report as lineage_report, Confidence, EvidenceKind,
    LineageEdge, LineageInput, LineageReport, LineageTable, LineageView, MAX_LINEAGE_EDGES,
    MAX_LINEAGE_QUERIES, MAX_LINEAGE_TABLES, MAX_LINEAGE_VIEWS,
};
pub use migration::{
    columns_changed, has_blocking_issue, plan_add_check, plan_add_foreign_key, plan_add_index,
    plan_changes, plan_create_table, plan_drop_check, plan_drop_foreign_key, plan_drop_index,
    plan_sqlite_rebuild, plan_table_options, recovery_hint, reorder_columns, validate_draft,
    CheckDraft, ColumnDraft, DesignerDraft, DesignerTab, ForeignKeyDraft, IndexDraft, IssueLevel,
    MigrationPlan, MigrationStatement, TableOptionsDraft, ValidationIssue,
};
pub use optimize::{
    apply_hunks, check_incompatible, diff_hunks, parse_answer, DialectNote, Hunk, NoteLevel,
    OptimizeKind, OptimizeRequest, OptimizeResult, Optimizer, ParsedAnswer,
};
pub use partition::{
    plan as plan_partition, supports as partition_supports, PartitionAction, PartitionBound,
    PartitionPlan, PartitionStrategy,
};
pub use preview::{DmlPreview, PreviewOutcome};
pub use profile::{group_by_folder, ConnectionFolder, ConnectionProfile};
pub use profiles::{ProfileStore, PROFILE_FILE_VERSION};
pub use results::{ResultMeta, ResultSet, ResultSetId, ResultState, ResultStore};
pub use review::{Finding, Review, ReviewContext, Severity};
pub use routine::{
    plan_save, routine_support, RoutineKind, RoutineSavePlan, RoutineSupport, SaveStrategy,
    SourceShape,
};
pub use runs::{RunLog, RunRecord, RunState};
pub use session_actor::{SessionCmd, SessionHandle, SessionLiveState, SessionRuntimeState};
pub use transfer::{
    find_target_table, insert_sql as transfer_insert_sql, plan_table as plan_transfer_table,
    target_table_name, CaseStrategy, ColumnPlan, TransferMode, TransferOptions, TransferRequest,
    TransferTablePlan, DEFAULT_BATCH_ROWS as TRANSFER_DEFAULT_BATCH_ROWS,
    DEFAULT_TIMEOUT_MS as TRANSFER_DEFAULT_TIMEOUT_MS, MAX_TRANSFER_TABLES,
};

pub const APP_NAME: &str = "RustDb";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
