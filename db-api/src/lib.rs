//! 数据库驱动契约层。
//!
//! 这里是 RustDb 唯一的「方言边界」：
//! * 驱动 crate 只依赖本 crate，不知道 UI / core 的存在；
//! * core 只依赖本 crate 的 trait 与 IR，不知道任何具体驱动；
//! * UI 只消费本 crate 的纯数据类型，禁止出现 `match DbKind`。
//!
//! 详细设计见 `docs/RustDb-需求开发文档.md` 第 7 章。

pub mod capabilities;
pub mod dialect;
pub mod driver;
pub mod error;
pub mod metadata;
pub mod page;
pub mod proxy;
pub mod session;
pub mod tunnel;
pub mod value;

pub use capabilities::{CancelStyle, Capabilities, ExplainStyle, JsonStyle, UpsertStyle};
pub use dialect::{quote_string_literal, Dialect};
pub use driver::{ConnectOptions, DbKind, DriverFactory, DriverRegistry};
pub use error::{DriverError, DriverErrorKind, RawDbError};
pub use metadata::{
    CheckConstraint, ColumnInfo, DdlSource, DependencyInfo, DependencyList, ForeignKeyInfo,
    IndexInfo, MetadataProvider, Namespace, ObjectDdl, ObjectKind, ObjectKindMask, ObjectPath,
    ObjectSummary, PartitionInfo, PartitionList, TableDetail, TriggerInfo, MAX_DEPENDENCIES,
    MAX_PARTITIONS,
};
pub use page::{
    build_page_sql, build_page_sql_literal, PageFilter, PageOp, PageOrder, PageRequest, PageResult,
    PageSql, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE,
};
pub use proxy::{ProxyKind, ProxyOrder, ProxySpec};
pub use session::{
    chunked_row_stream, ColumnMeta, DbWarning, ExecuteRequest, FetchOptions, IsolationLevel,
    QueryHandle, QueryId, RowBatch, RowStream, ServerInfo, Session, SessionId, TxId, TxOptions,
    WarningLevel,
};
pub use tunnel::{HostKeySpec, TunnelAuthMethod, TunnelAuthSpec, TunnelHopSpec, TunnelSpec};
pub use value::{compare_values, hex_dump, LogicalType, Row, Value, ValueDetailKind};
