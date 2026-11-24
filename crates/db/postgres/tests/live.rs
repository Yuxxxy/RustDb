//! 真实 PostgreSQL 验证（需要 Docker 容器 `rustdb-pg`）。
//!
//! 运行：
//! ```text
//! RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-postgres --test live -- --nocapture
//! ```

use futures_util::StreamExt;
use rustdb_db_api::{
    ConnectOptions, DriverErrorKind, DriverFactory, ExecuteRequest, FetchOptions, Namespace,
    ObjectKind, ObjectKindMask, ObjectPath, PageRequest, Session, TxOptions,
};
use rustdb_db_postgres::PostgresFactory;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

fn options() -> ConnectOptions {
    ConnectOptions {
        host: Some("127.0.0.1".into()),
        port: Some(55432),
        database: Some("rustdb_test".into()),
        username: Some("postgres".into()),
        secret: Some("rustdb".into()),
        ssl_mode: Some("disable".into()),
        params: BTreeMap::new(),
        connect_timeout_ms: 5_000,
        read_only: false,
    }
}

#[tokio::test]
async fn pg_live_query_metadata_and_switch_schema() {
    if std::env::var("RUSTDB_LIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires docker container rustdb-pg)");
        return;
    }

    let session = PostgresFactory::new()
        .connect(&options())
        .await
        .expect("connect postgres");
    assert!(session
        .server()
        .version
        .to_ascii_lowercase()
        .contains("postgres"));

    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select c.name, count(*)::text as orders, sum(o.total)::text as gmv \
                  from shop.orders o join shop.channels c on c.id = o.channel_id \
                  group by c.name order by sum(o.total) desc"
                .into(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("execute aggregate");

    let mut rows = Vec::new();
    while let Some(batch) = handle.stream.next().await {
        rows.extend(batch.expect("row batch").rows);
    }
    assert_eq!(rows.len(), 3, "三个渠道应各一行");
    assert_eq!(rows[0][0].display_text(), "自然搜索");
    assert_eq!(handle.columns.len(), 3);

    let namespaces = session.metadata().namespaces().await.expect("namespaces");
    assert!(
        namespaces
            .iter()
            .any(|n| n.schema.as_deref() == Some("shop")),
        "没有发现 shop schema：{namespaces:?}"
    );

    let shop = Namespace {
        catalog: Some(Arc::from("rustdb_test")),
        schema: Some(Arc::from("shop")),
    };
    let objects = session
        .metadata()
        .objects(&shop, ObjectKindMask::ALL)
        .await
        .expect("objects");
    assert!(objects.iter().any(|o| o.path.name.as_ref() == "orders"));
    assert!(objects.iter().any(|o| o.path.name.as_ref() == "channels"));
    // 物化视图 / 序列 / 函数 / 存储过程 / 类型（容器内已建好）。
    let has = |name: &str, kind: ObjectKind| {
        objects
            .iter()
            .any(|o| o.path.name.as_ref() == name && o.path.kind == kind)
    };
    assert!(
        has("mv_channels", ObjectKind::MaterializedView),
        "缺少物化视图：{objects:?}"
    );
    assert!(has("seq_order_id", ObjectKind::Sequence), "缺少序列");
    assert!(has("f_double", ObjectKind::Function), "缺少函数");
    assert!(has("p_touch", ObjectKind::Procedure), "缺少存储过程");
    assert!(has("mood", ObjectKind::Type), "缺少自定义类型");

    // DDL：函数走 pg_get_functiondef（原生），表按列合成。
    let function_path = rustdb_db_api::ObjectPath::new(
        Namespace::new(Some("rustdb_test"), Some("shop")),
        "f_double",
        ObjectKind::Function,
    );
    if let Some(ddl) = session
        .metadata()
        .object_ddl(&function_path)
        .await
        .expect("function ddl")
    {
        assert_eq!(ddl.source, rustdb_db_api::DdlSource::Native);
        assert!(
            ddl.text.to_uppercase().contains("FUNCTION"),
            "函数 DDL 应含 FUNCTION：{}",
            ddl.text
        );
    }

    let table_path = rustdb_db_api::ObjectPath::new(
        Namespace::new(Some("rustdb_test"), Some("shop")),
        "orders",
        ObjectKind::Table,
    );
    let ddl = session
        .metadata()
        .object_ddl(&table_path)
        .await
        .expect("table ddl")
        .expect("表应能合成 DDL");
    assert_eq!(ddl.source, rustdb_db_api::DdlSource::Synthesized);
    assert!(ddl.text.contains("CREATE TABLE"), "合成 DDL：{}", ddl.text);
    assert!(ddl.text.contains("total"), "应含列定义");

    session
        .use_namespace(&shop)
        .await
        .expect("switch schema shop");

    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select count(*) from channels".into(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("query after switch");
    let mut rows = Vec::new();
    while let Some(batch) = handle.stream.next().await {
        rows.extend(batch.expect("row batch").rows);
    }
    assert_eq!(rows[0][0].display_text(), "3");
}

// ---------------------------------------------------------------------------
// P2 live contract: TLS modes, explicit transactions, fetch_page delegation.
// ---------------------------------------------------------------------------

fn live_enabled() -> bool {
    if std::env::var("RUSTDB_LIVE_TESTS").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires docker container rustdb-pg)");
    false
}

fn options_with_ssl(mode: &str) -> ConnectOptions {
    ConnectOptions {
        ssl_mode: Some(mode.into()),
        ..options()
    }
}

/// 把结果流读成字符串矩阵，便于断言标量；同时验证多批流也能被正确消费。
async fn scalar_rows(session: &dyn Session, sql: &str) -> Vec<Vec<String>> {
    let mut handle = session
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
        .unwrap_or_else(|error| panic!("执行失败 {sql}: {error}"));
    let mut out = Vec::new();
    while let Some(batch) = handle.stream.next().await {
        let batch = batch.unwrap_or_else(|error| panic!("读取批次失败 {sql}: {error}"));
        for row in batch.rows {
            out.push(row.iter().map(|v| v.display_text().to_string()).collect());
        }
    }
    out
}

#[tokio::test]
async fn pg_live_tls_mode_matrix() {
    if !live_enabled() {
        return;
    }
    // 容器用自签证书开启 ssl=on，同时仍允许明文连接。
    for mode in ["disable", "prefer", "require"] {
        let session = PostgresFactory::new()
            .connect(&options_with_ssl(mode))
            .await
            .unwrap_or_else(|error| panic!("sslmode={mode} 应能连上：{error}"));
        let rows = scalar_rows(
            session.as_ref(),
            "select ssl from pg_stat_ssl where pid = pg_backend_pid()",
        )
        .await;
        let encrypted = rows.first().and_then(|r| r.first()).map(|s| s == "t") == Some(true);
        match mode {
            "disable" => assert!(!encrypted, "sslmode=disable 不应走 TLS"),
            "require" => assert!(encrypted, "sslmode=require 必须走 TLS 握手"),
            _ => {}
        }
    }
    // 负例：自签 CA 不在系统根证书里，verify-full 必须拒绝连接。
    let rejected = PostgresFactory::new()
        .connect(&options_with_ssl("verify-full"))
        .await;
    assert!(
        rejected.is_err(),
        "verify-full 对自签证书必须失败（实际连上了）"
    );
}

#[tokio::test]
async fn pg_live_explicit_transaction() {
    if !live_enabled() {
        return;
    }
    let session = PostgresFactory::new()
        .connect(&options())
        .await
        .expect("connect postgres");
    scalar_rows(session.as_ref(), "delete from shop.p2_tx_probe").await;

    let tx = session.begin(TxOptions::default()).await.expect("begin");
    scalar_rows(
        session.as_ref(),
        "insert into shop.p2_tx_probe values (1, 'rolled')",
    )
    .await;
    let in_tx = scalar_rows(session.as_ref(), "select count(*) from shop.p2_tx_probe").await;
    assert_eq!(in_tx[0][0], "1", "事务内应能看到未提交写入");
    session.rollback(tx).await.expect("rollback");
    let after_rollback =
        scalar_rows(session.as_ref(), "select count(*) from shop.p2_tx_probe").await;
    assert_eq!(after_rollback[0][0], "0", "rollback 应撤销写入");

    let tx = session.begin(TxOptions::default()).await.expect("begin");
    scalar_rows(
        session.as_ref(),
        "insert into shop.p2_tx_probe values (2, 'committed')",
    )
    .await;
    session.commit(tx).await.expect("commit");
    let after_commit = scalar_rows(session.as_ref(), "select count(*) from shop.p2_tx_probe").await;
    assert_eq!(after_commit[0][0], "1", "commit 应保留写入");
}

#[tokio::test]
async fn pg_live_fetch_page_delegates_to_core_fallback() {
    if !live_enabled() {
        return;
    }
    let session = PostgresFactory::new()
        .connect(&options())
        .await
        .expect("connect postgres");
    let req = PageRequest::new(ObjectPath::new(
        Namespace::new(Some("rustdb_test"), Some("shop")),
        "p2_rows",
        ObjectKind::Table,
    ));
    let error = session
        .fetch_page(&req)
        .await
        .expect_err("PG 驱动未实现服务端分页");
    assert_eq!(
        error.kind,
        DriverErrorKind::Unsupported,
        "驱动必须返回 Unsupported，Core 才会用分页 SQL 兜底"
    );
    // 真正的兜底分页由 core/tests/live_p2.rs 通过 Core::fetch_page 验证。
}

#[tokio::test]
async fn pg_live_lazy_stream_multi_batch_and_early_stop() {
    if !live_enabled() {
        return;
    }
    let session = PostgresFactory::new()
        .connect(&options())
        .await
        .expect("connect postgres");
    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select id::text, val from shop.p2_rows order by id".into(),
            params: Vec::new(),
            fetch: FetchOptions {
                max_rows: 5,
                page_size: 2,
                first_screen: 2,
                ..FetchOptions::default()
            },
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("execute capped query");
    assert_eq!(handle.columns.len(), 2, "execute 返回时列描述应已就绪");

    let mut batches = 0;
    let mut rows = 0;
    while let Some(batch) = handle.stream.next().await {
        batches += 1;
        rows += batch.expect("row batch").rows.len();
    }
    assert_eq!(rows, 5, "max_rows 应只返回 5 行");
    assert!(
        batches >= 2,
        "page_size=2 应产出多个 RowBatch，实际 {batches} 批"
    );

    // 流结束会触发 ActiveQueryGuard::drop，cancel 不应再命中旧查询。
    let error = session
        .cancel(&handle.query_id)
        .await
        .expect_err("流结束后 active_query 应已清除");
    assert!(
        error.user_message().contains("无需取消"),
        "取消已结束查询应给出明确提示：{error:?}"
    );

    // take(max_rows) 提前收流之后，同一连接仍可继续执行。
    let after = scalar_rows(session.as_ref(), "select 1").await;
    assert_eq!(after[0][0], "1", "截断后连接应仍可执行语句");
}

#[tokio::test]
async fn pg_live_dropping_partial_stream_keeps_connection_usable() {
    if !live_enabled() {
        return;
    }
    let session = PostgresFactory::new()
        .connect(&options())
        .await
        .expect("connect postgres");
    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select id::text, val from shop.p2_rows order by id".into(),
            params: Vec::new(),
            fetch: FetchOptions {
                max_rows: 10_000,
                page_size: 10,
                first_screen: 10,
                ..FetchOptions::default()
            },
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("execute full query");
    let first = handle
        .stream
        .next()
        .await
        .expect("首批应存在")
        .expect("首批无错误");
    assert_eq!(first.rows.len(), 10, "首批应恰好是 page_size 行");
    // 只消费一批就丢弃：底层 Responses 需要自己 drain 剩余协议帧。
    drop(handle);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after = scalar_rows(session.as_ref(), "select 1").await;
    assert_eq!(after[0][0], "1", "提前丢弃流后连接应仍可用");
}
