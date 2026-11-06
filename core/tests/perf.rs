//! M2 出口指标实测：M-04 首屏、M-07 取消。
//!
//! 只测本机 SQLite（内存库），避免网络抖动；阈值取需求文档第 2 章的值。

use rustdb_core::{AppEvent, ConnEvent, Core, EventRecvError, QueryEvent};
use rustdb_db_api::{DriverRegistry, SessionId};
use std::time::{Duration, Instant};

fn wait_until<F>(stream: &mut rustdb_core::EventStream, timeout: Duration, mut f: F) -> bool
where
    F: FnMut(&AppEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        loop {
            match stream.try_recv() {
                Ok(envelope) => {
                    if f(&envelope.event) {
                        return true;
                    }
                }
                Err(EventRecvError::Empty) => break,
                Err(EventRecvError::Lagged(_)) => continue,
                Err(EventRecvError::Closed) => return false,
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn connect(
    registry: DriverRegistry,
) -> (std::sync::Arc<Core>, rustdb_core::EventStream, SessionId) {
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");
    let mut session = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                session = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "没有连上"
    );
    (core, events, session.expect("缺少 session"))
}

/// M-04：SQLite 首屏（本机内存库）应远低于 300ms。
#[test]
fn m04_first_screen_under_300ms() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let (core, mut events, session) = connect(registry);

    let started = Instant::now();
    let qid = core
        .execute(
            &session,
            "SELECT c.name AS channel, COUNT(*) AS orders, SUM(o.total) AS gmv \
             FROM orders o JOIN channels c ON c.id = o.channel_id \
             GROUP BY c.name ORDER BY gmv DESC",
        )
        .expect("提交失败");
    let mut elapsed_ms = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Query(QueryEvent::Completed {
                query_id,
                elapsed_ms: ms,
                ..
            }) = event
            {
                if query_id == &qid {
                    elapsed_ms = Some(*ms);
                    return true;
                }
            }
            false
        }),
        "查询没有完成"
    );
    let wall = started.elapsed().as_millis();
    eprintln!("M-04 first screen: {wall} ms（actor 计时 {elapsed_ms:?} ms）");
    assert!(
        wall < 300,
        "M-04 首屏应 ≤ 300ms，实测 {wall}ms（actor 计时 {elapsed_ms:?}ms）"
    );
}

/// M-07：取消要真的中断驱动，且 2s 内收敛到 Cancelled。
#[test]
fn m07_cancel_interrupts_long_query() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let (core, mut events, session) = connect(registry);

    let qid = core
        .execute(
            &session,
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 20000000) \
             SELECT COUNT(*) FROM c",
        )
        .expect("提交失败");

    // 等它真的跑起来，再取消。
    assert!(
        wait_until(&mut events, Duration::from_secs(2), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Started { .. })
        )),
        "查询没有开始"
    );
    std::thread::sleep(Duration::from_millis(30));

    let cancel_at = Instant::now();
    core.cancel(&session, &qid).expect("取消失败");
    let mut seen = Vec::new();
    let cancelled = wait_until(&mut events, Duration::from_secs(3), |event| {
        match event {
            AppEvent::Query(QueryEvent::Cancelled { .. }) => return true,
            AppEvent::Query(QueryEvent::Failed { error, .. }) => {
                seen.push(format!("Failed:{:?}", error.kind));
                return true;
            }
            AppEvent::Query(QueryEvent::Completed { .. }) => {
                seen.push("Completed".into());
                return true;
            }
            _ => {}
        }
        false
    });
    let feedback = cancel_at.elapsed();
    eprintln!("M-07 cancel interrupt: {} ms", feedback.as_millis());
    assert!(cancelled, "取消后没有任何终止事件，等待 3s 超时");
    assert!(
        seen.is_empty(),
        "取消后应收到 Cancelled，实际先收到 {seen:?}"
    );
    assert!(
        feedback < Duration::from_secs(2),
        "M-07 驱动中断应 ≤ 2s，实测 {}ms",
        feedback.as_millis()
    );
}
