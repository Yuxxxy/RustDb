//! 每会话一个 Actor：串行化该连接上的命令，取消走旁路。
//!
//! 为什么不是 `Mutex<Connection>`：连接是天然串行资源，Actor 用消息队列保证
//! 「同一连接上的语句不并发」，同时让取消不必排在长查询后面。

use crate::events::{
    AppEvent, EventBus, EventError, ImportFailure, QueryEvent, ResultSummary, TxEvent,
};
use crate::results::ResultStore;
use crate::runs::{RunLog, RunState};
use futures_util::StreamExt;
use rustdb_db_api::{
    DriverError, DriverErrorKind, ExecuteRequest, FetchOptions, Namespace, QueryId, Session,
    SessionId, TxId, TxOptions,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub enum SessionCmd {
    Execute {
        query_id: QueryId,
        request_id: String,
        /// 一批语句：按顺序串行执行，每条产出独立结果集。
        statements: Vec<String>,
        max_rows: u64,
        /// 自动提交关闭时为 true：还没有活动事务就先 `BEGIN`，本次执行落在事务内。
        auto_begin: bool,
        /// 历史来源：手动 / AI / MCP / CLI / 文件。
        source: crate::history::HistorySource,
    },
    /// 变更集提交：若当前无事务则用单事务包裹（全成功才提交、失败整体回滚），
    /// 若已有手工事务则加入其中（不自动提交）。
    EditBatch {
        query_id: QueryId,
        request_id: String,
        statements: Vec<String>,
        /// 与 statements 等长：`true` 表示该语句必须影响 ≥1 行，否则视为并发冲突并整体回滚。
        expect_rows: Vec<bool>,
        max_rows: u64,
    },
    /// 手动开启事务（工具栏「开始事务」）。
    BeginTx {
        read_only: bool,
        reply: tokio::sync::oneshot::Sender<Result<TxId, DriverError>>,
    },
    CommitTx {
        reply: tokio::sync::oneshot::Sender<Result<(), DriverError>>,
    },
    RollbackTx {
        reply: tokio::sync::oneshot::Sender<Result<(), DriverError>>,
    },
    /// 数据导入（F-74）：清空（可选）后逐批执行 INSERT。
    /// 单批失败只记账并继续，最终由 `ImportFinished` 汇总；不承诺跨批回滚。
    Import {
        query_id: QueryId,
        request_id: String,
        truncate_sql: Option<String>,
        batches: Vec<crate::import::ImportBatch>,
        total_rows: u64,
    },
    UseNamespace {
        ns: Namespace,
        reply: tokio::sync::oneshot::Sender<Result<(), DriverError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct SessionHandle {
    pub id: SessionId,
    pub profile_id: String,
    pub session: Arc<dyn Session>,
    pub tx: mpsc::UnboundedSender<SessionCmd>,
    /// Actor 维护的实时状态；事件丢失时 UI 用它做重同步。
    pub state: Arc<SessionRuntimeState>,
}

/// 会话的可观察运行态：只由 Actor 写，外部只读。
#[derive(Default)]
pub struct SessionRuntimeState {
    active_query: parking_lot::Mutex<Option<QueryId>>,
    tx: parking_lot::Mutex<Option<TxId>>,
}

impl SessionRuntimeState {
    pub fn active_query(&self) -> Option<QueryId> {
        self.active_query.lock().clone()
    }

    pub fn tx(&self) -> Option<TxId> {
        self.tx.lock().clone()
    }
}

/// 重同步快照：`None` 表示该会话当前没有执行中的查询 / 事务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLiveState {
    pub active_query: Option<QueryId>,
    pub tx: Option<TxId>,
}

impl SessionHandle {
    pub fn send(&self, cmd: SessionCmd) -> bool {
        self.tx.send(cmd).is_ok()
    }
}

pub fn spawn(
    session: Arc<dyn Session>,
    profile_id: String,
    events: EventBus,
    results: Arc<ResultStore>,
    history: Arc<crate::history::HistoryStore>,
    runs: Arc<crate::runs::RunLog>,
    runtime: &tokio::runtime::Handle,
) -> SessionHandle {
    let id = session.id();
    let (tx, rx) = mpsc::unbounded_channel();
    let actor_session = session.clone();
    let actor_profile = profile_id.clone();
    let state = Arc::new(SessionRuntimeState::default());
    let actor_state = state.clone();
    runtime.spawn(async move {
        run_actor(
            actor_session,
            actor_profile,
            rx,
            events,
            results,
            history,
            runs,
            actor_state,
        )
        .await;
    });
    SessionHandle {
        id,
        profile_id,
        session,
        tx,
        state,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_actor(
    session: Arc<dyn Session>,
    profile_id: String,
    mut rx: mpsc::UnboundedReceiver<SessionCmd>,
    events: EventBus,
    results: Arc<ResultStore>,
    history: Arc<crate::history::HistoryStore>,
    runs: Arc<crate::runs::RunLog>,
    live: Arc<SessionRuntimeState>,
) {
    // 事务是连接级状态，只能由持有连接的这个 Actor 记住。
    let mut tx: Option<TxId> = None;
    // true = 由 `auto_commit = false` 隐式开启；失败时必须自动回滚。
    let mut implicit_tx = false;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            SessionCmd::Execute {
                query_id,
                request_id,
                statements,
                max_rows,
                auto_begin,
                source,
            } => {
                // 自动提交关闭且未开事务：先隐式 BEGIN，后续语句复用它。
                if auto_begin && tx.is_none() {
                    let sid = session.id();
                    match session.begin(TxOptions::default()).await {
                        Ok(id) => {
                            events.publish(
                                AppEvent::Tx(TxEvent::Began {
                                    tx_id: id.to_string(),
                                    read_only: false,
                                }),
                                Some(sid),
                            );
                            tx = Some(id);
                            implicit_tx = true;
                        }
                        Err(e) => {
                            events.publish(
                                AppEvent::Query(QueryEvent::Failed {
                                    query_id,
                                    error: EventError::from(&e),
                                }),
                                Some(sid),
                            );
                            continue;
                        }
                    }
                }
                // 就地 await：同一连接上的语句必须串行。
                // 取消不走这条队列（见 `Core::cancel` 的旁路实现），否则会被长查询堵住。
                *live.active_query.lock() = Some(query_id.clone());
                let ok = execute_batch(
                    session.clone(),
                    &profile_id,
                    query_id,
                    request_id,
                    statements,
                    max_rows,
                    tx.clone(),
                    events.clone(),
                    results.clone(),
                    history.clone(),
                    runs.clone(),
                    None,
                    source,
                )
                .await;
                *live.active_query.lock() = None;
                // 隐式事务（自动提交关闭）遇到失败必须回滚：否则 PostgreSQL 会
                // 停留在 aborted transaction，SQLite 则可能把失败前的语句提交掉。
                if !ok && implicit_tx {
                    if let Some(id) = tx.take() {
                        let sid = session.id();
                        let tx_id = id.to_string();
                        let reason = match session.rollback(id).await {
                            Ok(()) => "执行失败，隐式事务已自动回滚".to_string(),
                            Err(error) => {
                                format!("执行失败，隐式事务回滚失败：{}", error.user_message())
                            }
                        };
                        events.publish(
                            AppEvent::Tx(TxEvent::RolledBack { tx_id, reason }),
                            Some(sid),
                        );
                    }
                    implicit_tx = false;
                }
            }
            SessionCmd::EditBatch {
                query_id,
                request_id,
                statements,
                expect_rows,
                max_rows,
            } => {
                let owned_tx = tx.is_none();
                if owned_tx {
                    let sid = session.id();
                    match session.begin(TxOptions::default()).await {
                        Ok(id) => {
                            events.publish(
                                AppEvent::Tx(TxEvent::Began {
                                    tx_id: id.to_string(),
                                    read_only: false,
                                }),
                                Some(sid),
                            );
                            tx = Some(id);
                            implicit_tx = true;
                        }
                        Err(e) => {
                            events.publish(
                                AppEvent::Query(QueryEvent::Failed {
                                    query_id,
                                    error: EventError::from(&e),
                                }),
                                Some(sid),
                            );
                            continue;
                        }
                    }
                }
                *live.active_query.lock() = Some(query_id.clone());
                let ok = execute_batch(
                    session.clone(),
                    &profile_id,
                    query_id,
                    request_id,
                    statements,
                    max_rows,
                    tx.clone(),
                    events.clone(),
                    results.clone(),
                    history.clone(),
                    runs.clone(),
                    Some(expect_rows),
                    crate::history::HistorySource::Manual,
                )
                .await;
                *live.active_query.lock() = None;
                // 只提交「本次变更集自己开启」的事务；已有手工事务交给用户决定。
                if owned_tx {
                    let sid = session.id();
                    match tx.take() {
                        Some(id) if ok => {
                            let tx_id = id.to_string();
                            match session.commit(id.clone()).await {
                                Ok(()) => {
                                    implicit_tx = false;
                                    events.publish(
                                        AppEvent::Tx(TxEvent::Committed { tx_id }),
                                        Some(sid),
                                    );
                                }
                                Err(error) => {
                                    log::error!("变更集提交失败：{error}");
                                    let rollback_error = session.rollback(id).await.err();
                                    implicit_tx = false;
                                    let reason = match rollback_error {
                                        Some(error) => format!(
                                            "变更集提交失败，事务已结束（回滚也失败：{}）",
                                            error.user_message()
                                        ),
                                        None => "变更集提交失败，事务已结束".into(),
                                    };
                                    events.publish(
                                        AppEvent::Tx(TxEvent::RolledBack { tx_id, reason }),
                                        Some(sid),
                                    );
                                }
                            }
                        }
                        Some(id) => {
                            let _ = session.rollback(id.clone()).await;
                            implicit_tx = false;
                            events.publish(
                                AppEvent::Tx(TxEvent::RolledBack {
                                    tx_id: id.to_string(),
                                    reason: "变更集未全部成功".into(),
                                }),
                                Some(sid),
                            );
                        }
                        None => {
                            implicit_tx = false;
                        }
                    }
                }
            }
            SessionCmd::BeginTx { read_only, reply } => {
                let sid = session.id();
                if tx.is_some() {
                    let _ = reply.send(Err(DriverError::unsupported(
                        "已有进行中的事务，请先提交或回滚",
                    )));
                    continue;
                }
                let result = match session
                    .begin(TxOptions {
                        read_only,
                        ..TxOptions::default()
                    })
                    .await
                {
                    Ok(id) => {
                        events.publish(
                            AppEvent::Tx(TxEvent::Began {
                                tx_id: id.to_string(),
                                read_only,
                            }),
                            Some(sid),
                        );
                        tx = Some(id.clone());
                        implicit_tx = false;
                        Ok(id)
                    }
                    Err(e) => Err(e),
                };
                let _ = reply.send(result);
            }
            SessionCmd::CommitTx { reply } => {
                let sid = session.id();
                let result = match tx.clone() {
                    Some(id) => match session.commit(id.clone()).await {
                        Ok(()) => {
                            tx = None;
                            implicit_tx = false;
                            events.publish(
                                AppEvent::Tx(TxEvent::Committed {
                                    tx_id: id.to_string(),
                                }),
                                Some(sid),
                            );
                            Ok(())
                        }
                        // 提交失败时保留 tx，让用户仍可回滚 / 重试；不能先把状态丢掉。
                        Err(e) => Err(e),
                    },
                    None => Err(DriverError::unsupported("没有进行中的事务")),
                };
                let _ = reply.send(result);
            }
            SessionCmd::RollbackTx { reply } => {
                let sid = session.id();
                let result = match tx.clone() {
                    Some(id) => match session.rollback(id.clone()).await {
                        Ok(()) => {
                            tx = None;
                            implicit_tx = false;
                            events.publish(
                                AppEvent::Tx(TxEvent::RolledBack {
                                    tx_id: id.to_string(),
                                    reason: "用户回滚".into(),
                                }),
                                Some(sid),
                            );
                            Ok(())
                        }
                        Err(e) => Err(e),
                    },
                    None => Err(DriverError::unsupported("没有进行中的事务")),
                };
                let _ = reply.send(result);
            }
            SessionCmd::Import {
                query_id,
                request_id,
                truncate_sql,
                batches,
                total_rows,
            } => {
                let sid = session.id();
                let started = Instant::now();
                *live.active_query.lock() = Some(query_id.clone());
                events.publish(
                    AppEvent::Query(QueryEvent::Started {
                        query_id: query_id.clone(),
                        request_id: request_id.clone(),
                    }),
                    Some(sid.clone()),
                );

                let mut inserted_rows = 0u64;
                let mut failed_rows = 0u64;
                let mut failed_batches = Vec::new();
                let mut cancelled = false;
                let mut truncated = false;
                let total_batches = batches.len();

                if let Some(sql) = truncate_sql {
                    match execute_import_one(&session, &query_id, &request_id, sql, &events, &sid)
                        .await
                    {
                        Ok(_) => truncated = true,
                        Err(ExecFailure::Cancelled) => cancelled = true,
                        Err(ExecFailure::Failed(e)) => {
                            events.publish(
                                AppEvent::Query(QueryEvent::Failed {
                                    query_id,
                                    error: EventError::from(&e),
                                }),
                                Some(sid),
                            );
                            *live.active_query.lock() = None;
                            continue;
                        }
                    }
                }

                for batch in batches {
                    if cancelled {
                        break;
                    }
                    events.publish(
                        AppEvent::Query(QueryEvent::ImportProgress {
                            query_id: query_id.clone(),
                            batch: batch.index,
                            total_batches,
                            done_rows: inserted_rows,
                            total_rows,
                        }),
                        Some(sid.clone()),
                    );
                    let rows = (batch.end_row.saturating_sub(batch.start_row) + 1) as u64;
                    match execute_import_one(
                        &session,
                        &query_id,
                        &request_id,
                        batch.sql,
                        &events,
                        &sid,
                    )
                    .await
                    {
                        Ok(_) => inserted_rows += rows,
                        Err(ExecFailure::Cancelled) => cancelled = true,
                        Err(ExecFailure::Failed(e)) => {
                            failed_rows += rows;
                            failed_batches.push(ImportFailure {
                                index: batch.index,
                                start_row: batch.start_row,
                                end_row: batch.end_row,
                                message: e.user_message(),
                            });
                        }
                    }
                }

                *live.active_query.lock() = None;
                events.publish(
                    AppEvent::Query(QueryEvent::ImportFinished {
                        query_id,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        total_rows,
                        inserted_rows,
                        failed_rows,
                        failed_batches,
                        truncated,
                        cancelled,
                    }),
                    Some(sid),
                );
            }
            SessionCmd::UseNamespace { ns, reply } => {
                let result = session.use_namespace(&ns).await;
                let _ = reply.send(result);
            }
            SessionCmd::Shutdown => {
                // 断开连接前显式回滚未提交事务；连接直接 drop 时部分驱动不会
                // 立刻把回滚结果反馈给服务端。
                if let Some(id) = tx.take() {
                    let _ = session.rollback(id).await;
                }
                break;
            }
        }
        // 每处理完一条命令就把事务状态同步给外部观察者；
        // 事件丢失时 UI 靠这份快照做重同步。
        *live.tx.lock() = tx.clone();
    }
}

/// 单条语句的失败：取消是正常终止，不该按错误上报。
enum ExecFailure {
    Cancelled,
    Failed(DriverError),
}

fn rows_of(summaries: &[ResultSummary]) -> u64 {
    summaries.iter().map(|s| s.rows).sum()
}

/// 收尾一条运行记录，并按条数上限 / 内存预算淘汰。
///
/// 保护对象：固定记录引用的结果集 + **本次运行**自己的结果集（新结果不能被
/// 刚产生的淘汰逻辑吃掉）。
fn finish_run(
    runs: &RunLog,
    results: &ResultStore,
    id: u64,
    state: RunState,
    elapsed_ms: u64,
    rows: u64,
    affected: Option<u64>,
) {
    runs.finish(id, state, elapsed_ms, rows, affected);
    let mut keep = runs.pinned_sets();
    if let Some(current) = runs.get(id) {
        keep.extend(current.result_sets);
    }
    for record in runs.trim() {
        for set in record.result_sets {
            results.remove_if_unreferenced(set, &keep);
        }
    }
    let evicted = results.evict_unpinned(&keep);
    if !evicted.is_empty() {
        log::debug!("结果内存超预算，已淘汰 {} 个结果集", evicted.len());
    }
}

/// 执行一批语句：按顺序串行，每条产出独立结果集；
/// 遇到错误或取消即停止，已产出的结果集保留。
#[allow(clippy::too_many_arguments)]
async fn execute_batch(
    session: Arc<dyn Session>,
    profile_id: &str,
    query_id: QueryId,
    request_id: String,
    statements: Vec<String>,
    max_rows: u64,
    tx: Option<TxId>,
    events: EventBus,
    results: Arc<ResultStore>,
    history: Arc<crate::history::HistoryStore>,
    runs: Arc<crate::runs::RunLog>,
    // 逐语句的「必须影响行数」期望；None 表示不做校验。
    expect_rows: Option<Vec<bool>>,
    source: crate::history::HistorySource,
) -> bool {
    let sid = session.id();
    let started = Instant::now();
    // 历史条目先落库（此时还不知道耗时与成败），结束时回填。
    let text = statements.join(";\n");
    let history_id = history.begin_with(profile_id, &text, source);
    let run_kind = if statements.len() == 1 {
        format!("{:?}", rustdb_sql_lang::classify(&statements[0]))
    } else {
        format!("Batch({})", statements.len())
    };
    // F-65：每次执行登记一条独立运行记录（不覆盖旧结果）。
    let run_id = runs.begin(profile_id, &text, &run_kind);
    events.publish(
        AppEvent::Query(QueryEvent::Started {
            query_id: query_id.clone(),
            request_id: request_id.clone(),
        }),
        Some(sid.clone()),
    );

    let mut summaries: Vec<ResultSummary> = Vec::with_capacity(statements.len());
    let total = statements.len();
    let mut warning_count = 0u32;
    let mut affected_total: Option<u64> = None;

    for (index, sql) in statements.into_iter().enumerate() {
        events.publish(
            AppEvent::Query(QueryEvent::StatementStarted {
                query_id: query_id.clone(),
                index: index + 1,
                total,
            }),
            Some(sid.clone()),
        );
        // 保护本次运行已产出的结果集：流式追加中途触发预算淘汰时不能把它们吃掉。
        let keep: Vec<crate::results::ResultSetId> = runs
            .get(run_id)
            .map(|record| record.result_sets)
            .unwrap_or_default();
        match execute_one(
            &session,
            &query_id,
            &request_id,
            sql,
            max_rows,
            tx.clone(),
            &events,
            &results,
            &sid,
            &keep,
        )
        .await
        {
            Ok((summary, warnings, affected)) => {
                // 运行记录先挂上结果集，UI 才能在运行标签里按需取页。
                runs.attach_result(run_id, summary.set_id);
                summaries.push(summary);
                // 告警原文逐条发 Notice（UI 的消息面板按顺序累积）。
                for warning in warnings {
                    warning_count += 1;
                    events.publish(
                        AppEvent::Query(QueryEvent::Notice {
                            query_id: query_id.clone(),
                            level: warning.level,
                            message: warning.message,
                        }),
                        Some(sid.clone()),
                    );
                }
                if let Some(n) = affected {
                    affected_total = Some(affected_total.unwrap_or(0) + n);
                }
                // 乐观并发：该语句本应命中 1 行却命中 0 行 → 数据已被他人改动。
                let expects = expect_rows
                    .as_ref()
                    .and_then(|e| e.get(index))
                    .copied()
                    .unwrap_or(false);
                if expects && affected.unwrap_or(0) == 0 {
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    history.finish_with(history_id, elapsed_ms, false, affected_total);
                    finish_run(
                        &runs,
                        &results,
                        run_id,
                        RunState::Failed,
                        elapsed_ms,
                        rows_of(&summaries),
                        affected_total,
                    );
                    events.publish(
                        AppEvent::Query(QueryEvent::Failed {
                            query_id,
                            error: EventError {
                                kind: DriverErrorKind::Conflict,
                                message: format!(
                                    "第 {} 条语句未命中任何行：该行已被他人修改或删除，已回滚整批变更",
                                    index + 1
                                ),
                                sqlstate: None,
                                code: None,
                                position: None,
                                hint: Some("请刷新数据后重试".into()),
                                retryable: false,
                            },
                        }),
                        Some(sid),
                    );
                    return false;
                }
            }
            Err(ExecFailure::Cancelled) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                history.finish_with(history_id, elapsed_ms, false, affected_total);
                finish_run(
                    &runs,
                    &results,
                    run_id,
                    RunState::Cancelled,
                    elapsed_ms,
                    rows_of(&summaries),
                    affected_total,
                );
                events.publish(
                    AppEvent::Query(QueryEvent::Cancelled {
                        query_id,
                        elapsed_ms,
                        at: index + 1,
                        total,
                    }),
                    Some(sid),
                );
                return false;
            }
            Err(ExecFailure::Failed(e)) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                history.finish_with(history_id, elapsed_ms, false, affected_total);
                finish_run(
                    &runs,
                    &results,
                    run_id,
                    RunState::Failed,
                    elapsed_ms,
                    rows_of(&summaries),
                    affected_total,
                );
                events.publish(
                    AppEvent::Query(QueryEvent::Failed {
                        query_id,
                        error: EventError::from(&e),
                    }),
                    Some(sid),
                );
                return false;
            }
        }
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    history.finish_with(history_id, elapsed_ms, true, affected_total);
    finish_run(
        &runs,
        &results,
        run_id,
        RunState::Completed,
        elapsed_ms,
        rows_of(&summaries),
        affected_total,
    );
    events.publish(
        AppEvent::Query(QueryEvent::Completed {
            query_id,
            elapsed_ms,
            affected: affected_total,
            results: summaries,
            warnings: warning_count,
        }),
        Some(sid),
    );
    true
}

/// 导入语句：只关心成败与告警，不写入结果存储（DML 没有行流）。
async fn execute_import_one(
    session: &Arc<dyn Session>,
    query_id: &QueryId,
    request_id: &str,
    sql: String,
    events: &EventBus,
    sid: &SessionId,
) -> Result<Option<u64>, ExecFailure> {
    let req = ExecuteRequest {
        sql,
        params: Vec::new(),
        fetch: FetchOptions {
            max_rows: 0,
            ..FetchOptions::default()
        },
        tx: None,
        read_only: false,
        request_id: Some(request_id.to_string()),
        query_id: Some(query_id.clone()),
    };
    let handle = match session.execute(req).await {
        Ok(handle) => handle,
        Err(error) => {
            return Err(if error.kind == DriverErrorKind::Cancelled {
                ExecFailure::Cancelled
            } else {
                ExecFailure::Failed(error)
            });
        }
    };
    for warning in handle.warnings {
        events.publish(
            AppEvent::Query(QueryEvent::Notice {
                query_id: query_id.clone(),
                level: warning.level,
                message: warning.message,
            }),
            Some(sid.clone()),
        );
    }
    let affected = handle.affected;
    let mut stream = handle.stream;
    while let Some(item) = stream.next().await {
        match item {
            Ok(_) => {}
            Err(error) => {
                return Err(if error.kind == DriverErrorKind::Cancelled {
                    ExecFailure::Cancelled
                } else {
                    ExecFailure::Failed(error)
                });
            }
        }
    }
    Ok(affected)
}

#[allow(clippy::too_many_arguments)]
async fn execute_one(
    session: &Arc<dyn Session>,
    query_id: &QueryId,
    request_id: &str,
    sql: String,
    max_rows: u64,
    tx: Option<TxId>,
    events: &EventBus,
    results: &Arc<ResultStore>,
    sid: &SessionId,
    // 预算淘汰时必须保留的结果集（本次运行已产出的）。
    keep: &[crate::results::ResultSetId],
) -> Result<(ResultSummary, Vec<rustdb_db_api::DbWarning>, Option<u64>), ExecFailure> {
    let stmt_started = Instant::now();
    let req = ExecuteRequest {
        sql,
        params: Vec::new(),
        fetch: FetchOptions {
            max_rows,
            ..FetchOptions::default()
        },
        tx,
        read_only: false,
        request_id: Some(request_id.to_string()),
        // 用 core 的 query_id，取消才能命中驱动记录的 active id。
        query_id: Some(query_id.clone()),
    };

    let handle = match session.execute(req).await {
        Ok(h) => h,
        Err(e) => {
            return Err(if e.kind == DriverErrorKind::Cancelled {
                ExecFailure::Cancelled
            } else {
                ExecFailure::Failed(e)
            });
        }
    };

    // 告警 / NOTICE 原样上抛到消息面板，不再当作「结果被截断」。
    let mut warnings = handle.warnings;
    let affected = handle.affected;
    let (set_id, _set) = results.create(handle.columns.clone(), query_id.clone());
    let mut stream = handle.stream;
    // 单条结果集也有字节上限：超限就停止拉流，而不是先物化再淘汰。
    let mut budget_hit = false;

    while let Some(item) = stream.next().await {
        match item {
            Ok(batch) => {
                if results.would_exceed(set_id, batch.approx_bytes) {
                    budget_hit = true;
                    break;
                }
                if let Some((loaded, bytes)) = results.append(set_id, batch) {
                    if results.over_budget() {
                        // 只淘汰未固定、已完成的旧结果；本次运行的结果由 keep 保护。
                        results.evict_unpinned(keep);
                    }
                    events.publish(
                        AppEvent::Query(QueryEvent::RowsAppended {
                            query_id: query_id.clone(),
                            set_id,
                            loaded,
                            total_hint: None,
                            bytes,
                        }),
                        Some(sid.clone()),
                    );
                }
            }
            Err(e) => {
                if e.kind == DriverErrorKind::Cancelled {
                    results.cancel(set_id);
                    return Err(ExecFailure::Cancelled);
                }
                results.fail(set_id);
                return Err(ExecFailure::Failed(e));
            }
        }
    }

    if budget_hit {
        // 主动断开流后行数不再增长；标记截断，让 UI 明确提示而不是静默少行。
        warnings.push(rustdb_db_api::DbWarning {
            level: rustdb_db_api::WarningLevel::Warning,
            message: format!(
                "结果超出内存预算（{}），已截断；请加 LIMIT 或分批导出",
                crate::events::human_bytes(results.budget_bytes() as u64)
            ),
        });
    }

    let elapsed_ms = stmt_started.elapsed().as_millis() as u64;
    results.complete(set_id, elapsed_ms, affected);
    let meta = results.meta(set_id);
    // 拉满上限即视为截断：驱动在 max_rows 处停流，行数正好等于上限。
    let loaded = meta.as_ref().map(|m| m.loaded_rows).unwrap_or(0);
    let truncated = budget_hit || (max_rows > 0 && loaded >= max_rows);
    if truncated {
        results.mark_truncated(set_id, None);
    }
    Ok((
        ResultSummary {
            set_id,
            columns: meta.as_ref().map(|m| m.columns.len()).unwrap_or(0),
            rows: loaded,
            truncated,
            affected,
        },
        warnings,
        affected,
    ))
}
