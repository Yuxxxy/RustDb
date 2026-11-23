//! 只读连接的**服务端**加固（可选实库护栏）。
//!
//! 与 `live.rs` 的 Docker 夹具不同，这里连真实 MySQL，验证：
//! 1. `read_only = true` 的连接，`@@session.transaction_read_only` 必须为 1；
//! 2. **绕过 core / CLI 的语句拦截**，直接通过驱动发一条 UPDATE，服务端必须拒绝；
//! 3. 读写连接的会话变量仍为 0（防止把「只读」误设到所有连接上）。
//!
//! 运行（默认跳过，避免 CI 依赖公网库）：
//! ```text
//! RUSTDB_LIVE_MYSQL_HOST=... RUSTDB_LIVE_MYSQL_PORT=3307 \
//! RUSTDB_LIVE_MYSQL_DATABASE=... RUSTDB_LIVE_MYSQL_USER=... \
//! RUSTDB_LIVE_MYSQL_PASSWORD=... \
//! cargo test -p rustdb-db-mysql --test read_only_live -- --nocapture
//! ```
//! 写语句是 `UPDATE <table> SET <col> = <col> WHERE 1 = 0`，即使服务端没拦住也不会改数据。

use futures_util::StreamExt;
use rustdb_db_api::{ConnectOptions, DriverFactory, ExecuteRequest, FetchOptions, Session};
use rustdb_db_mysql::MySqlFactory;
use std::collections::BTreeMap;

fn options(read_only: bool) -> Option<ConnectOptions> {
    let host = std::env::var("RUSTDB_LIVE_MYSQL_HOST").ok()?;
    let secret = std::env::var("RUSTDB_LIVE_MYSQL_PASSWORD").ok()?;
    let port = std::env::var("RUSTDB_LIVE_MYSQL_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3306);
    let database = std::env::var("RUSTDB_LIVE_MYSQL_DATABASE").ok();
    let username = std::env::var("RUSTDB_LIVE_MYSQL_USER").ok();
    Some(ConnectOptions {
        host: Some(host),
        port: Some(port),
        database,
        username,
        secret: Some(secret),
        ssl_mode: Some("prefer".into()),
        params: BTreeMap::new(),
        connect_timeout_ms: 15_000,
        read_only,
    })
}

async fn scalar(session: &dyn Session, sql: &str) -> String {
    let mut handle = session
        .execute(ExecuteRequest {
            sql: sql.into(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .unwrap_or_else(|error| panic!("查询失败 {sql}: {error}"));
    let mut rows = Vec::new();
    while let Some(batch) = handle.stream.next().await {
        rows.extend(batch.expect("读取行批次").rows);
    }
    assert_eq!(rows.len(), 1, "标量查询应只有一行：{sql}");
    rows[0][0].display_text()
}

#[tokio::test]
async fn readonly_session_is_enforced_by_mysql_server() {
    let Some(opts) = options(true) else {
        eprintln!("skip: 未设置 RUSTDB_LIVE_MYSQL_HOST / RUSTDB_LIVE_MYSQL_PASSWORD");
        return;
    };
    let session = MySqlFactory::new().connect(&opts).await.expect("连接只读会话");
    assert!(session.server().read_only, "连接元数据应标记只读");

    assert_eq!(
        scalar(session.as_ref(), "SELECT @@session.transaction_read_only")
            .await
            .as_str(),
        "1",
        "只读连接的服务端会话变量应为 1"
    );

    let table = std::env::var("RUSTDB_LIVE_MYSQL_TABLE").unwrap_or_else(|_| "tc_emp".into());
    let column = std::env::var("RUSTDB_LIVE_MYSQL_COLUMN").unwrap_or_else(|_| "name".into());
    let sql = format!("UPDATE {table} SET {column} = {column} WHERE 1 = 0");
    let error = session
        .execute(ExecuteRequest {
            sql: sql.clone(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .err()
        .unwrap_or_else(|| panic!("只读会话的写入必须被服务端拒绝：{sql}"));
    let message = format!("{} (code={:?})", error.user_message(), error.code);
    assert!(
        error.code == Some(1792)
            || message.to_uppercase().contains("READ ONLY")
            || message.contains("只读"),
        "应以只读错误拒绝写入，实际：{message}"
    );

    // 对照：读写连接不能被误设成只读。
    let Some(opts_rw) = options(false) else {
        return;
    };
    let rw = MySqlFactory::new()
        .connect(&opts_rw)
        .await
        .expect("连接读写会话");
    assert_eq!(
        scalar(rw.as_ref(), "SELECT @@session.transaction_read_only")
            .await
            .as_str(),
        "0",
        "读写连接不应带只读标记"
    );
}
