//! core 事件驱动的运行时封装：连接、执行、取元数据。
//!
//! CLI 不直接持有驱动，也不自己执行 SQL：一切经由 `Core`，
//! 读只读 / 生产标记 / 隧道门控仍由 core 统一执行。

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustdb_core::events::{
    AppEvent, ConnEvent, EventRecvError, MetaEvent, QueryEvent, ResultSummary,
};
use rustdb_core::{Core, EventStream, HistorySource};
use rustdb_db_api::{Namespace, ObjectDdl, ObjectPath, ObjectSummary, SessionId, TableDetail};

use crate::error::CliError;

/// 一次执行的结果摘要。
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub results: Vec<ResultSummary>,
    pub elapsed_ms: u64,
    pub warnings: u32,
    pub notices: Vec<String>,
}

/// CLI 的 core 运行时。
pub struct Runtime {
    core: Arc<Core>,
    events: EventStream,
}

impl Runtime {
    pub fn new(core: Arc<Core>) -> Self {
        // 先订阅再连接：事件总线是广播，订阅晚一步就会漏掉 Ready。
        let events = core.subscribe();
        Self { core, events }
    }

    pub fn core(&self) -> &Arc<Core> {
        &self.core
    }

    /// 连接并等到就绪。
    pub async fn connect(
        &mut self,
        profile_id: &str,
        timeout: Duration,
    ) -> Result<SessionId, CliError> {
        self.core.connect(profile_id);
        let deadline = deadline(timeout);
        loop {
            match self.wait_event(deadline).await? {
                AppEvent::Conn(ConnEvent::Ready { session_id, .. }) => return Ok(session_id),
                AppEvent::Conn(ConnEvent::Failed {
                    message,
                    error_kind,
                    ..
                }) => {
                    return Err(CliError::connect(format!("{message}（{error_kind:?}）")));
                }
                AppEvent::Conn(ConnEvent::TunnelHostKey {
                    layer,
                    fingerprint,
                    changed,
                    ..
                }) => {
                    return Err(CliError::connect(format!(
                        "第 {layer} 层主机密钥{}（{fingerprint}）待确认：CLI 无法交互确认，请在桌面端确认后重试",
                        if changed { "已变更" } else { "未知" }
                    )));
                }
                _ => {}
            }
        }
    }

    /// 等命名空间列表加载完成（连接事件之后才发出）。
    pub async fn wait_namespaces(&mut self, timeout: Duration) -> Result<Vec<Namespace>, CliError> {
        let deadline = deadline(timeout);
        loop {
            match self.wait_event(deadline).await? {
                AppEvent::Meta(MetaEvent::NamespacesLoaded { namespaces, .. }) => {
                    return Ok(namespaces)
                }
                _ => {}
            }
        }
    }

    /// 执行 SQL 并等结果。
    pub async fn execute(
        &mut self,
        session_id: &SessionId,
        sql: &str,
        timeout: Duration,
    ) -> Result<QueryOutcome, CliError> {
        let query_id = self
            .core
            .execute_with_source(session_id, sql, true, HistorySource::Cli)
            .map_err(CliError::query)?;
        let deadline = deadline(timeout);
        let mut notices = Vec::new();
        loop {
            match self.wait_event(deadline).await? {
                AppEvent::Query(QueryEvent::Completed {
                    query_id: id,
                    results,
                    elapsed_ms,
                    warnings,
                    ..
                }) if id == query_id => {
                    return Ok(QueryOutcome {
                        results,
                        elapsed_ms,
                        warnings,
                        notices,
                    });
                }
                AppEvent::Query(QueryEvent::Failed {
                    query_id: id,
                    error,
                }) if id == query_id => {
                    return Err(CliError::query(error.message));
                }
                AppEvent::Query(QueryEvent::Notice {
                    query_id: id,
                    message,
                    ..
                }) if id == query_id => notices.push(message),
                _ => {}
            }
        }
    }

    /// 取命名空间下的对象列表（已缓存则直接返回）。
    pub async fn load_objects(
        &mut self,
        session_id: &SessionId,
        namespace: Namespace,
        timeout: Duration,
    ) -> Result<Vec<ObjectSummary>, CliError> {
        let label = namespace.display_label();
        if self.core.has_objects(session_id, &namespace) {
            return Ok(self.core.objects(session_id, &namespace));
        }
        self.core.load_objects(session_id, namespace.clone()).await;
        let deadline = deadline(timeout);
        loop {
            match self.wait_event(deadline).await? {
                AppEvent::Meta(MetaEvent::Loaded {
                    namespace: key,
                    objects,
                }) if key == label => return Ok(objects),
                AppEvent::Meta(MetaEvent::LoadFailed {
                    namespace: key,
                    message,
                }) if key == label => return Err(CliError::query(message)),
                _ => {}
            }
        }
    }

    /// 取表详情（列 / 索引 / 外键）。
    pub async fn table_detail(
        &mut self,
        session_id: &SessionId,
        path: ObjectPath,
        timeout: Duration,
    ) -> Result<TableDetail, CliError> {
        let label = path.display_label();
        self.core.request_table_detail(session_id, path);
        let deadline = deadline(timeout);
        loop {
            match self.wait_event(deadline).await? {
                AppEvent::Meta(MetaEvent::TableDetailLoaded { path, detail }) if path == label => {
                    return Ok(detail)
                }
                AppEvent::Meta(MetaEvent::TableDetailFailed { path, message }) if path == label => {
                    return Err(CliError::query(message))
                }
                _ => {}
            }
        }
    }

    /// 取对象的 DDL。
    pub async fn ddl(
        &mut self,
        session_id: &SessionId,
        path: ObjectPath,
        timeout: Duration,
    ) -> Result<ObjectDdl, CliError> {
        let label = path.display_label();
        self.core.request_ddl(session_id, path);
        let deadline = deadline(timeout);
        loop {
            match self.wait_event(deadline).await? {
                AppEvent::Meta(MetaEvent::DdlLoaded { path, ddl }) if path == label => {
                    return Ok(ddl)
                }
                AppEvent::Meta(MetaEvent::DdlFailed { path, message }) if path == label => {
                    return Err(CliError::query(message))
                }
                _ => {}
            }
        }
    }

    /// 等下一条事件；`deadline = None` 表示不限时长。
    async fn wait_event(&mut self, deadline: Option<Instant>) -> Result<AppEvent, CliError> {
        loop {
            let wait = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(CliError::timeout("等待超时"));
                    }
                    remaining
                }
                // 不限时长：分段等待，任何一段超时都继续等下一段。
                None => Duration::from_secs(5),
            };
            match self.events.recv_timeout(wait).await {
                Ok(envelope) => return Ok(envelope.event.clone()),
                Err(EventRecvError::Empty) => {
                    if deadline.is_some() {
                        return Err(CliError::timeout("等待超时"));
                    }
                }
                Err(EventRecvError::Lagged(count)) => {
                    return Err(CliError::internal(format!("事件丢失 {count} 条")));
                }
                Err(EventRecvError::Closed) => {
                    return Err(CliError::internal("事件总线已关闭"));
                }
            }
        }
    }
}

/// 超时时长 → 截止时刻；零表示不限时长。
fn deadline(timeout: Duration) -> Option<Instant> {
    if timeout.is_zero() {
        None
    } else {
        Some(Instant::now() + timeout)
    }
}
