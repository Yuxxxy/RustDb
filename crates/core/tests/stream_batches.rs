//! P2 回归：驱动按 `FetchOptions.page_size` 交付多个 RowBatch 后，
//! core 必须逐批发布 `RowsAppended`（而不是整条查询一个巨批）。
//!
//! 用 SQLite 递归 CTE 生成 500 行，无需真实实例；默认 page_size=200
//! 应切成 3 批，事件数 >= 2 即证明增量链路打通。

use rustdb_core::{AppEvent, ConnEvent, Core, EventRecvError, EventStream, QueryEvent};
use rustdb_db_api::{DriverRegistry, SessionId};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn connect_sqlite() -> (Arc<Core>, EventStream, SessionId) {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    // Core 自带 tokio Runtime，必须在同步上下文创建/销毁。
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        loop {
            match events.try_recv() {
                Ok(envelope) => {
                    if let AppEvent::Conn(ConnEvent::Ready { ref session_id, .. }) = envelope.event
                    {
                        return (core, events, session_id.clone());
                    }
                }
                Err(EventRecvError::Empty) => break,
                Err(EventRecvError::Lagged(_)) => continue,
                Err(EventRecvError::Closed) => panic!("事件通道提前关闭"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("10s 内没有收到 ConnEvent::Ready");
}

#[test]
fn multi_batch_stream_publishes_incremental_rows_appended() {
    let (core, mut events, session) = connect_sqlite();
    let sql = "WITH RECURSIVE seq(n) AS \
               (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 500) \
               SELECT n FROM seq";
    let query_id = core.execute(&session, sql).expect("提交查询失败");

    let mut appended = 0usize;
    let mut last_loaded = 0u64;
    let mut completed = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !completed {
        loop {
            match events.try_recv() {
                Ok(envelope) => {
                    if let AppEvent::Query(event) = &envelope.event {
                        match event {
                            QueryEvent::RowsAppended { loaded, .. } => {
                                appended += 1;
                                assert!(
                                    *loaded >= last_loaded,
                                    "loaded 必须单调不减：{last_loaded} -> {loaded}"
                                );
                                last_loaded = *loaded;
                            }
                            QueryEvent::Completed { query_id: id, .. } => {
                                if *id == query_id {
                                    completed = true;
                                }
                            }
                            QueryEvent::Failed { error, .. } => {
                                panic!("查询失败：{error:?}");
                            }
                            _ => {}
                        }
                    }
                }
                Err(EventRecvError::Empty) => break,
                Err(EventRecvError::Lagged(_)) => continue,
                Err(EventRecvError::Closed) => panic!("事件通道提前关闭"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(completed, "15s 内没有收到 QueryEvent::Completed");
    assert!(
        appended >= 2,
        "500 行应按 page_size=200 产生多个 RowsAppended，实际只有 {appended} 个"
    );
    assert_eq!(last_loaded, 500, "最终应加载 500 行，实际 {last_loaded}");
}
