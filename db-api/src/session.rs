use crate::capabilities::Capabilities;
use crate::dialect::Dialect;
use crate::error::DriverError;
use crate::metadata::MetadataProvider;
use crate::value::{LogicalType, Row};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static ID_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str) -> String {
    let seq = ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{prefix}_{ms:x}{seq:x}")
}

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(next_id($prefix))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(SessionId, "s");
id_type!(QueryId, "q");
id_type!(TxId, "tx");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    pub flavor: Option<String>,
    pub charset: Option<String>,
    pub timezone: Option<String>,
    pub current_db: Option<String>,
    pub read_only: bool,
    pub max_ident_len: usize,
}

impl ServerInfo {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            flavor: None,
            charset: None,
            timezone: None,
            current_db: None,
            read_only: false,
            max_ident_len: 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub name: Arc<str>,
    pub logical_type: LogicalType,
    pub native_type: String,
    pub nullable: bool,
}

impl ColumnMeta {
    pub fn new(name: impl Into<Arc<str>>, logical_type: LogicalType) -> Self {
        let native_type = logical_type.label();
        Self {
            name: name.into(),
            logical_type,
            native_type,
            nullable: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOptions {
    pub isolation: Option<IsolationLevel>,
    pub read_only: bool,
    pub deferrable: bool,
}

impl Default for TxOptions {
    fn default() -> Self {
        Self {
            isolation: None,
            read_only: false,
            deferrable: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// 单次执行最多拉取的行数（保护内存）。
    pub max_rows: u64,
    /// 每批行数。驱动按它切 `RowBatch`，core 逐批做预算判定与增量事件。
    pub page_size: u32,
    pub timeout_ms: Option<u64>,
    /// 首批大小上限（不超过 `page_size`）：让首屏尽快拿到第一批，
    /// 其余行继续按 `page_size` 追加；`chunked_row_stream` 会落实这个语义。
    pub first_screen: u32,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            max_rows: 10_000,
            page_size: 200,
            timeout_ms: Some(30_000),
            first_screen: 200,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecuteRequest {
    pub sql: String,
    pub params: Vec<crate::value::Value>,
    pub fetch: FetchOptions,
    pub tx: Option<TxId>,
    pub read_only: bool,
    pub request_id: Option<String>,
    /// 上层（core）分配的查询 id。
    ///
    /// 驱动**必须**用它作为 `QueryHandle::query_id`，否则 `Session::cancel`
    /// 传入的 id 与驱动记录的 active id 对不上，取消会静默失效。
    pub query_id: Option<QueryId>,
}

#[derive(Debug, Clone)]
pub struct RowBatch {
    pub rows: Vec<Row>,
    pub approx_bytes: usize,
}

impl RowBatch {
    pub fn new(rows: Vec<Row>) -> Self {
        let approx_bytes = rows.iter().flatten().map(|v| v.approx_bytes()).sum();
        Self { rows, approx_bytes }
    }
}

pub type RowStream = Pin<Box<dyn Stream<Item = Result<RowBatch, DriverError>> + Send + 'static>>;

/// 把已物化的行按 [`FetchOptions`] 切成多个 [`RowBatch`]。
///
/// 尚未做真惰性流改造的驱动仍会先读完结果集，但至少让 `RowStream` 产出多批：
/// core 的 `would_exceed` 预算判定与 `RowsAppended` 增量事件才有实际粒度，
/// 而不是「整条查询一个巨批」。真流式驱动（PG `simple_query_raw`）不需要它。
pub fn chunked_row_stream(rows: Vec<Row>, fetch: &FetchOptions) -> RowStream {
    use futures_util::stream;
    let page_size = fetch.page_size.max(1) as usize;
    let first_screen = (fetch.first_screen.max(1) as usize).min(page_size);
    let mut batches: Vec<Result<RowBatch, DriverError>> = Vec::new();
    let mut iter = rows.into_iter();
    let mut first = true;
    loop {
        let size = if first { first_screen } else { page_size };
        let chunk: Vec<Row> = iter.by_ref().take(size).collect();
        if chunk.is_empty() {
            break;
        }
        batches.push(Ok(RowBatch::new(chunk)));
        first = false;
    }
    Box::pin(stream::iter(batches))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningLevel {
    Info,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbWarning {
    pub level: WarningLevel,
    pub message: String,
}

pub struct QueryHandle {
    pub query_id: QueryId,
    pub columns: Arc<[ColumnMeta]>,
    pub stream: RowStream,
    /// DML 的影响行数（SELECT 为 None）。
    pub affected: Option<u64>,
    pub warnings: Vec<DbWarning>,
}

#[async_trait::async_trait]
pub trait Session: Send + Sync {
    fn id(&self) -> SessionId;
    fn server(&self) -> &ServerInfo;
    fn capabilities(&self) -> &Capabilities;
    fn dialect(&self) -> Arc<dyn Dialect>;

    async fn ping(&self) -> Result<Duration, DriverError>;

    /// 提交一条语句并**尽快**返回结果流；不得等全部结果物化。
    async fn execute(&self, req: ExecuteRequest) -> Result<QueryHandle, DriverError>;

    /// 旁路取消：必须真的发给服务端（或调用驱动的中断接口）。
    async fn cancel(&self, query_id: &QueryId) -> Result<(), DriverError>;

    async fn begin(&self, opts: TxOptions) -> Result<TxId, DriverError>;
    async fn commit(&self, tx: TxId) -> Result<(), DriverError>;
    async fn rollback(&self, tx: TxId) -> Result<(), DriverError>;

    /// 切换当前 catalog / schema。
    ///
    /// 能原地切换的方言（MySQL `USE`、PG `SET search_path`、MSSQL `USE`）实现它；
    /// 需要新建连接的场景返回 `Unsupported`（UI 会提示，而不是静默失败）。
    async fn use_namespace(&self, ns: &crate::metadata::Namespace) -> Result<(), DriverError> {
        let _ = ns;
        Err(DriverError::unsupported("切换数据库"))
    }

    fn metadata(&self) -> &dyn MetadataProvider;

    /// 服务端分页（X-02）：按 [`crate::page::PageRequest`] 取一页。
    ///
    /// 驱动应使用主键兜底排序、多取一行探测 `has_more`；不支持的方言
    /// 保持默认实现，上层会退回到客户端分页并提示。
    async fn fetch_page(
        &self,
        req: &crate::page::PageRequest,
    ) -> Result<crate::page::PageResult, DriverError> {
        let _ = req;
        Err(DriverError::unsupported("服务端分页"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use futures_util::{FutureExt, StreamExt};

    fn rows(n: usize) -> Vec<Row> {
        (0..n).map(|_| vec![Value::Int(1)]).collect()
    }

    /// `stream::iter` 是立即 ready 的，用 `now_or_never` 同步 poll 即可，
    /// 不需要引入 executor / tokio。
    fn batch_sizes(n: usize, fetch: FetchOptions) -> Vec<usize> {
        let mut stream = chunked_row_stream(rows(n), &fetch);
        let mut sizes = Vec::new();
        while let Some(Some(batch)) = stream.next().now_or_never() {
            sizes.push(batch.expect("batch").rows.len());
        }
        sizes
    }

    #[test]
    fn chunked_stream_splits_first_screen_then_pages() {
        let fetch = FetchOptions {
            page_size: 3,
            first_screen: 1,
            ..FetchOptions::default()
        };
        assert_eq!(batch_sizes(8, fetch), vec![1, 3, 3, 1]);
    }

    #[test]
    fn chunked_stream_clamps_first_screen_to_page_size() {
        let fetch = FetchOptions {
            page_size: 2,
            first_screen: 99,
            ..FetchOptions::default()
        };
        assert_eq!(batch_sizes(5, fetch), vec![2, 2, 1]);
    }

    #[test]
    fn chunked_stream_handles_empty_result() {
        assert!(batch_sizes(0, FetchOptions::default()).is_empty());
    }
}
