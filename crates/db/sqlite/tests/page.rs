use rustdb_db_api::{
    ConnectOptions, DriverFactory, ExecuteRequest, FetchOptions, Namespace, ObjectKind, ObjectPath,
    PageFilter, PageOp, PageOrder, PageRequest, Session, Value,
};
use rustdb_db_sqlite::SqliteFactory;
use std::collections::BTreeMap;
use std::sync::Arc;

fn opts() -> ConnectOptions {
    ConnectOptions {
        host: None,
        port: None,
        database: Some(":memory:".into()),
        username: None,
        secret: None,
        ssl_mode: None,
        params: BTreeMap::new(),
        connect_timeout_ms: 5_000,
        read_only: false,
    }
}

async fn memory_session() -> Box<dyn Session> {
    SqliteFactory::new()
        .connect(&opts())
        .await
        .expect("打开 :memory: 失败")
}

async fn exec(session: &dyn Session, sql: &str) {
    session
        .execute(ExecuteRequest {
            sql: sql.into(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: None,
            read_only: false,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("执行 SQL 失败");
}

fn path(name: &str) -> ObjectPath {
    ObjectPath::new(
        Namespace::new(Some("main"), None::<&str>),
        name,
        ObjectKind::Table,
    )
}

fn ids(result: &rustdb_db_api::PageResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match &row[0] {
            Value::Int(id) => *id,
            other => panic!("id 不是整数：{other:?}"),
        })
        .collect()
}

#[test]
fn sqlite_declares_server_side_filter() {
    assert!(
        SqliteFactory::new()
            .baseline_capabilities()
            .server_side_filter
    );
}

#[tokio::test]
async fn pages_cover_seeded_orders_exactly_once() {
    let session = memory_session().await;
    let mut req = PageRequest::new(path("orders"));
    req.page_size = 2;
    req.order_by = vec![PageOrder::asc("id")];

    let first = session.fetch_page(&req).await.expect("第一页失败");
    assert_eq!(ids(&first), vec![1001, 1002]);
    assert!(first.has_more);
    assert_eq!(first.offset, 0);
    assert_eq!(
        first
            .columns
            .iter()
            .map(|c| c.name.to_string())
            .collect::<Vec<_>>(),
        vec!["id", "channel_id", "total", "status", "created_at"]
    );

    req.page = 2;
    let second = session.fetch_page(&req).await.expect("第二页失败");
    assert_eq!(ids(&second), vec![1003, 1004]);
    assert!(!second.has_more);
    assert_eq!(second.offset, 2);

    req.page = 3;
    let third = session.fetch_page(&req).await.expect("空页失败");
    assert!(third.rows.is_empty());
    assert!(!third.has_more);
}

#[tokio::test]
async fn like_filter_and_explicit_nulls_last() {
    let session = memory_session().await;
    exec(
        &*session,
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, score NUMERIC)",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO items VALUES (1, 'alpha', 5), (2, 'beta', 9), (3, 'atom', NULL), (4, 'arc', 7)",
    )
    .await;

    let mut req = PageRequest::new(path("items"));
    req.page_size = 10;
    req.order_by = vec![PageOrder::desc("score")];
    req.filters = vec![PageFilter::like("name", "a%")];

    let page = session.fetch_page(&req).await.expect("过滤分页失败");
    assert_eq!(ids(&page), vec![4, 1, 3], "降序 + NULL 排最后");
    assert!(!page.has_more);
}

#[tokio::test]
async fn null_filter_uses_is_null_and_bad_column_fails() {
    let session = memory_session().await;
    exec(
        &*session,
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
    )
    .await;
    exec(
        &*session,
        "INSERT INTO notes VALUES (1, 'a'), (2, NULL), (3, NULL)",
    )
    .await;

    let mut req = PageRequest::new(path("notes"));
    req.page_size = 10;
    req.order_by = vec![PageOrder::asc("id")];
    req.filters = vec![PageFilter {
        column: "body".into(),
        op: PageOp::Eq,
        value: Value::Null,
    }];
    let page = session.fetch_page(&req).await.expect("NULL 过滤失败");
    assert_eq!(ids(&page), vec![2, 3]);

    let missing = PageRequest::new(path("missing_table"));
    assert!(session.fetch_page(&missing).await.is_err());
}

#[tokio::test]
async fn invalid_page_request_is_rejected() {
    let session = memory_session().await;
    let mut req = PageRequest::new(path("orders"));
    req.page = 0;
    assert!(session.fetch_page(&req).await.is_err());

    req.page = 1;
    req.page_size = rustdb_db_api::MAX_PAGE_SIZE + 1;
    assert!(session.fetch_page(&req).await.is_err());

    req.page_size = 10;
    req.filters = vec![PageFilter {
        column: "status".into(),
        op: PageOp::Like,
        value: Value::Null,
    }];
    assert!(session.fetch_page(&req).await.is_err());
}

#[tokio::test]
async fn request_survives_arc_clone() {
    // PageRequest 会被 core 包在 Arc 里传给驱动，确保它可跨线程克隆。
    let req = Arc::new(PageRequest::new(path("orders")));
    let cloned = Arc::clone(&req);
    let session = memory_session().await;
    let page = session.fetch_page(&cloned).await.expect("克隆请求失败");
    assert_eq!(page.rows.len(), 4);
}
