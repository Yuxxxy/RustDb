//! 真实 MySQL 验证（需要 Docker 容器 `rustdb-mysql`）。
//!
//! 运行：
//! ```text
//! RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-mysql --test live -- --nocapture
//! ```

use futures_util::StreamExt;
use rustdb_db_api::{
    ConnectOptions, DriverErrorKind, DriverFactory, ExecuteRequest, FetchOptions, Namespace,
    ObjectKind, ObjectKindMask, ObjectPath, PageRequest, Session, TxOptions,
};
use rustdb_db_mysql::MySqlFactory;
use std::collections::BTreeMap;
use std::sync::Arc;

fn options() -> ConnectOptions {
    ConnectOptions {
        host: Some("127.0.0.1".into()),
        port: Some(53306),
        database: Some("rustdb_test".into()),
        username: Some("root".into()),
        secret: Some("rustdb".into()),
        ssl_mode: None,
        params: BTreeMap::new(),
        connect_timeout_ms: 5_000,
        read_only: false,
    }
}

#[tokio::test]
async fn mysql_live_query_metadata_and_switch_database() {
    if std::env::var("RUSTDB_LIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires docker container rustdb-mysql)");
        return;
    }

    let session = MySqlFactory::new()
        .connect(&options())
        .await
        .expect("connect mysql");
    assert!(session.server().version.contains('.'));

    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select c.name, count(*) as orders, sum(o.total) as gmv \
                  from orders o join channels c on c.id = o.channel_id \
                  group by c.name order by gmv desc"
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
            .any(|n| n.catalog.as_deref() == Some("rustdb_test")),
        "没有发现 rustdb_test 库：{namespaces:?}"
    );

    let db = Namespace {
        catalog: Some(Arc::from("rustdb_test")),
        schema: None,
    };
    let objects = session
        .metadata()
        .objects(&db, ObjectKindMask::ALL)
        .await
        .expect("objects");
    assert!(objects.iter().any(|o| o.path.name.as_ref() == "orders"));
    assert!(objects.iter().any(|o| o.path.name.as_ref() == "channels"));
    // 存储过程 / 函数 / 触发器都要出现在对象树里（由容器内的 p_demo / f_double / trg_demo 提供）。
    let has = |name: &str, kind: ObjectKind| {
        objects
            .iter()
            .any(|o| o.path.name.as_ref() == name && o.path.kind == kind)
    };
    assert!(
        has("p_demo", ObjectKind::Procedure),
        "缺少存储过程：{objects:?}"
    );
    assert!(has("f_double", ObjectKind::Function), "缺少函数");
    assert!(has("trg_demo", ObjectKind::Trigger), "缺少触发器");

    // DDL：SHOW CREATE 的原生输出。
    let table_path = rustdb_db_api::ObjectPath::new(db.clone(), "orders", ObjectKind::Table);
    let ddl = session
        .metadata()
        .object_ddl(&table_path)
        .await
        .expect("table ddl")
        .expect("SHOW CREATE TABLE 应有输出");
    assert_eq!(ddl.source, rustdb_db_api::DdlSource::Native);
    assert!(
        ddl.text.to_uppercase().contains("CREATE TABLE"),
        "MySQL DDL：{}",
        ddl.text
    );

    let procedure_path =
        rustdb_db_api::ObjectPath::new(db.clone(), "p_demo", ObjectKind::Procedure);
    let procedure_ddl = session
        .metadata()
        .object_ddl(&procedure_path)
        .await
        .expect("procedure ddl");
    assert!(procedure_ddl.is_some(), "存储过程应有 DDL");

    session.use_namespace(&db).await.expect("switch database");

    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select count(*) from orders".into(),
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
    assert_eq!(rows[0][0].display_text(), "4");
}

// ---------------------------------------------------------------------------
// P2 live contract: TLS modes, exclusive-Conn transactions, fetch_page delegate.
// ---------------------------------------------------------------------------

fn live_enabled() -> bool {
    if std::env::var("RUSTDB_LIVE_TESTS").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires docker container rustdb-mysql)");
    false
}

fn options_with_ssl(mode: &str) -> ConnectOptions {
    ConnectOptions {
        ssl_mode: Some(mode.into()),
        ..options()
    }
}

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
        for row in batch.expect("row batch").rows {
            out.push(row.iter().map(|v| v.display_text().to_string()).collect());
        }
    }
    out
}

/// 当前连接是否真的在 TLS 上（SHOW SESSION STATUS 的第二列）。
async fn tls_cipher(session: &dyn Session) -> String {
    let rows = scalar_rows(session, "show session status like 'Ssl_cipher'").await;
    rows.first()
        .and_then(|row| row.get(1))
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn mysql_live_tls_mode_matrix() {
    if !live_enabled() {
        return;
    }
    for mode in ["disable", "prefer", "require"] {
        let session = MySqlFactory::new()
            .connect(&options_with_ssl(mode))
            .await
            .unwrap_or_else(|error| panic!("sslmode={mode} 应能连上：{error}"));
        let cipher = tls_cipher(session.as_ref()).await;
        match mode {
            "disable" => assert!(cipher.is_empty(), "sslmode=disable 不应走 TLS：{cipher}"),
            "require" => assert!(!cipher.is_empty(), "sslmode=require 必须完成 TLS 握手"),
            // prefer 允许回退明文：只要求能连上，不强制加密。
            _ => {}
        }
    }
}

#[tokio::test]
async fn mysql_live_explicit_transaction_on_exclusive_conn() {
    if !live_enabled() {
        return;
    }
    let session = MySqlFactory::new()
        .connect(&options())
        .await
        .expect("connect mysql");
    // 第二个独立连接用于验证「事务绑定在会话独占 Conn 上」的隔离性。
    let observer = MySqlFactory::new()
        .connect(&options())
        .await
        .expect("connect observer");
    scalar_rows(session.as_ref(), "delete from p2_tx_probe").await;

    let tx = session.begin(TxOptions::default()).await.expect("begin");
    scalar_rows(
        session.as_ref(),
        "insert into p2_tx_probe values (1, 'rolled')",
    )
    .await;
    let in_tx = scalar_rows(session.as_ref(), "select count(*) from p2_tx_probe").await;
    assert_eq!(in_tx[0][0], "1", "事务内应看到未提交写入");
    let seen_by_other = scalar_rows(observer.as_ref(), "select count(*) from p2_tx_probe").await;
    assert_eq!(
        seen_by_other[0][0], "0",
        "其它连接不应看到未提交写入（说明事务确实在独占 Conn 上）"
    );
    session.rollback(tx).await.expect("rollback");
    let after_rollback = scalar_rows(session.as_ref(), "select count(*) from p2_tx_probe").await;
    assert_eq!(after_rollback[0][0], "0", "rollback 应撤销写入");

    let tx = session.begin(TxOptions::default()).await.expect("begin");
    scalar_rows(
        session.as_ref(),
        "insert into p2_tx_probe values (2, 'committed')",
    )
    .await;
    session.commit(tx).await.expect("commit");
    let after_commit = scalar_rows(session.as_ref(), "select count(*) from p2_tx_probe").await;
    assert_eq!(after_commit[0][0], "1", "commit 应保留写入");
    // COMMIT 后新连接能看到，证明不是只写进了会话本地状态。
    let seen_by_other = scalar_rows(observer.as_ref(), "select count(*) from p2_tx_probe").await;
    assert_eq!(seen_by_other[0][0], "1", "commit 后其它连接应可见");
}

#[tokio::test]
async fn mysql_live_fetch_page_delegates_to_core_fallback() {
    if !live_enabled() {
        return;
    }
    let session = MySqlFactory::new()
        .connect(&options())
        .await
        .expect("connect mysql");
    let req = PageRequest::new(ObjectPath::new(
        Namespace {
            catalog: Some(Arc::from("rustdb_test")),
            schema: None,
        },
        "p2_rows",
        ObjectKind::Table,
    ));
    let error = session
        .fetch_page(&req)
        .await
        .expect_err("MySQL 驱动未实现服务端分页");
    assert_eq!(
        error.kind,
        DriverErrorKind::Unsupported,
        "驱动必须返回 Unsupported，Core 才会用分页 SQL 兜底"
    );
}

#[tokio::test]
async fn mysql_live_max_rows_truncation_and_multi_batch() {
    if !live_enabled() {
        return;
    }
    let session = MySqlFactory::new()
        .connect(&options())
        .await
        .expect("connect mysql");
    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select id, val from p2_rows order by id".into(),
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
    // 真早停之后独占 Conn 必须仍可用（mysql_async 会 drain 剩余协议帧）。
    let after = scalar_rows(session.as_ref(), "select 1").await;
    assert_eq!(after[0][0], "1", "截断后连接应仍可执行语句");
}
