//! P2 live：Core::fetch_page 对「未实现服务端分页」的驱动走方言分页 SQL 兜底。
//!
//! 运行前先起容器：`scripts/setup-live-p2.sh all`
//! ```text
//! RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-core --test live_p2 -- --nocapture
//! ```
//! PG 与 MySQL 的 `Session::fetch_page` 都返回 `Unsupported`，这里验证 Core
//! 确实用 `page_size + 1` 的分页 SQL 只取一页，而不是物化整表。

use rustdb_core::{AppEvent, ConnEvent, ConnectionProfile, Core, EventRecvError, EventStream};
use rustdb_db_api::{
    DbKind, DriverRegistry, Namespace, ObjectKind, ObjectPath, PageOrder, PageRequest, SessionId,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn live_enabled() -> bool {
    if std::env::var("RUSTDB_LIVE_TESTS").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires scripts/setup-live-p2.sh all)");
    false
}

fn profile(driver: DbKind, id: &str, port: u16, user: &str) -> ConnectionProfile {
    let mut p = ConnectionProfile::demo_sqlite();
    p.id = id.into();
    p.name = format!("P2 live {id}");
    p.driver = driver;
    p.host = Some("127.0.0.1".into());
    p.port = Some(port);
    p.database = Some("rustdb_test".into());
    p.username = Some(user.into());
    p.dev_secret = Some("rustdb".into());
    p.ssl_mode = Some("disable".into());
    p.connect_timeout_ms = 5_000;
    p
}

fn registry() -> DriverRegistry {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_postgres::PostgresFactory::new());
    registry.register(rustdb_db_mysql::MySqlFactory::new());
    registry
}

fn wait_ready(events: &mut EventStream, timeout: Duration) -> Option<SessionId> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        loop {
            match events.try_recv() {
                Ok(envelope) => {
                    if let AppEvent::Conn(ConnEvent::Ready { ref session_id, .. }) = envelope.event
                    {
                        return Some(session_id.clone());
                    }
                }
                Err(EventRecvError::Empty) => break,
                Err(EventRecvError::Lagged(_)) => continue,
                Err(EventRecvError::Closed) => return None,
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// 必须同步创建 Core：它自带一个 tokio Runtime，在异步上下文里 drop 会触发
/// "Cannot drop a runtime in a context where blocking is not allowed"。
fn live_session(p: ConnectionProfile) -> (Arc<Core>, SessionId) {
    let core = Core::new(registry());
    core.add_profile(p.clone()).expect("add profile");
    let mut events = core.subscribe();
    core.connect(&p.id);
    let session = wait_ready(&mut events, Duration::from_secs(20)).expect("ConnEvent::Ready");
    (core, session)
}

async fn assert_paging(core: &Core, session: &SessionId, path: ObjectPath) {
    let mut req = PageRequest::new(path);
    req.page_size = 10;
    req.order_by = vec![PageOrder::asc("id")];

    let first = core.fetch_page(session, &req).await.expect("第 1 页");
    assert_eq!(first.rows.len(), 10, "应只取 page_size 行");
    assert_eq!(first.offset, 0);
    assert!(first.has_more, "500 行数据第 1 页必须还有下一页");
    assert_eq!(first.rows[0][0].display_text(), "1");

    let mut second = req.clone();
    second.page = 2;
    let second = core.fetch_page(session, &second).await.expect("第 2 页");
    assert_eq!(second.offset, 10);
    assert_eq!(second.rows[0][0].display_text(), "11", "分页不能重行/漏行");

    let mut last = req.clone();
    last.page = 50;
    let last = core.fetch_page(session, &last).await.expect("最后一页");
    assert_eq!(last.rows.len(), 10);
    assert!(!last.has_more, "最后一页不应误报 has_more");

    let mut beyond = req.clone();
    beyond.page = 51;
    let beyond = core.fetch_page(session, &beyond).await.expect("越界页");
    assert!(beyond.rows.is_empty(), "越界页应为空");
    assert!(!beyond.has_more);
}

fn run_paging(p: ConnectionProfile, path: ObjectPath) {
    let (core, session) = live_session(p);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试运行时");
    runtime.block_on(assert_paging(&core, &session, path));
    // core / runtime 都在同步栈上 drop，避免嵌套 runtime 的 shutdown panic。
}

#[test]
fn pg_fetch_page_fallback_uses_dialect_sql() {
    if !live_enabled() {
        return;
    }
    let path = ObjectPath::new(
        Namespace::new(Some("rustdb_test"), Some("shop")),
        "p2_rows",
        ObjectKind::Table,
    );
    run_paging(
        profile(DbKind::Postgres, "live-pg", 55432, "postgres"),
        path,
    );
}

#[test]
fn mysql_fetch_page_fallback_uses_dialect_sql() {
    if !live_enabled() {
        return;
    }
    let path = ObjectPath::new(
        Namespace {
            catalog: Some(Arc::from("rustdb_test")),
            schema: None,
        },
        "p2_rows",
        ObjectKind::Table,
    );
    run_paging(profile(DbKind::Mysql, "live-mysql", 53306, "root"), path);
}
