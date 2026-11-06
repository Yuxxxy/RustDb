//! 真实 Oracle 的 spike 契约测试；默认 `#[ignore]`，需要环境变量：
//!
//! ```text
//! RUSTDB_ORACLE_USER=scott
//! RUSTDB_ORACLE_PASSWORD=tiger
//! RUSTDB_ORACLE_CONNECT=127.0.0.1:1521/FREEPDB1
//! ```
//!
//! 运行：`cargo test -p rustdb-db-oracle-rust --test live -- --ignored`

use futures_util::StreamExt;
use rustdb_db_api::{
    ConnectOptions, DriverErrorKind, DriverFactory, ExecuteRequest, FetchOptions, Row, Session,
    Value,
};
use rustdb_db_oracle_rust::OracleRustFactory;
use std::collections::BTreeMap;

fn options() -> Option<ConnectOptions> {
    let user = std::env::var("RUSTDB_ORACLE_USER").ok()?;
    let password = std::env::var("RUSTDB_ORACLE_PASSWORD").ok()?;
    let connect = std::env::var("RUSTDB_ORACLE_CONNECT").ok()?;
    let mut params = BTreeMap::new();
    params.insert("connect_string".into(), connect);
    Some(ConnectOptions {
        host: None,
        port: None,
        database: None,
        username: Some(user),
        secret: Some(password),
        ssl_mode: None,
        params,
        connect_timeout_ms: 10_000,
        read_only: false,
    })
}

async fn connect() -> Option<Box<dyn Session>> {
    let opts = match options() {
        Some(opts) => opts,
        None => {
            eprintln!(
                "跳过：未设置 RUSTDB_ORACLE_USER / RUSTDB_ORACLE_PASSWORD / RUSTDB_ORACLE_CONNECT"
            );
            return None;
        }
    };
    Some(
        OracleRustFactory::new()
            .connect(&opts)
            .await
            .expect("connect"),
    )
}

async fn query(session: &dyn Session, sql: &str) -> Vec<Row> {
    let handle = session
        .execute(ExecuteRequest {
            sql: sql.into(),
            params: Vec::new(),
            fetch: FetchOptions {
                max_rows: 100,
                timeout_ms: Some(5_000),
                ..FetchOptions::default()
            },
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("execute");
    let mut stream = handle.stream;
    let mut rows = Vec::new();
    while let Some(batch) = stream.next().await {
        rows.extend(batch.expect("row batch").rows);
    }
    rows
}

#[tokio::test]
#[ignore = "需要真实 Oracle 实例"]
async fn number_clob_and_timestamp_roundtrip() {
    let Some(session) = connect().await else {
        return;
    };
    let rows = query(
        &*session,
        "SELECT 123.45 AS n, TO_CLOB('渠道A') AS c, \
         TIMESTAMP '2026-01-02 03:04:05.123456789' AS ts FROM dual",
    )
    .await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    match &row[0] {
        Value::Decimal(text) => assert_eq!(&**text, "123.45"),
        other => panic!("NUMBER 期望 Decimal，实际 {other:?}"),
    }
    match &row[1] {
        Value::Text(text) => assert_eq!(&**text, "渠道A"),
        other => panic!("CLOB 期望 Text，实际 {other:?}"),
    }
    match &row[2] {
        Value::Timestamp(value) => {
            assert_eq!(value.to_string(), "2026-01-02 03:04:05.123456789");
        }
        other => panic!("TIMESTAMP 期望 Timestamp，实际 {other:?}"),
    }
}

#[tokio::test]
#[ignore = "需要真实 Oracle 实例"]
async fn integer_number_stays_integer() {
    let Some(session) = connect().await else {
        return;
    };
    let rows = query(&*session, "SELECT CAST(42 AS NUMBER(10)) FROM dual").await;
    assert!(matches!(rows[0][0], Value::Int(42)));
    let rows = query(
        &*session,
        "SELECT CAST(1234567890123456789 AS NUMBER(20)) FROM dual",
    )
    .await;
    assert!(matches!(rows[0][0], Value::Int(1234567890123456789)));
    let rows = query(
        &*session,
        "SELECT CAST(12345678901234567890 AS NUMBER(20)) FROM dual",
    )
    .await;
    match &rows[0][0] {
        Value::Decimal(text) => assert_eq!(&**text, "12345678901234567890"),
        other => panic!("超出 i64 的 NUMBER 期望 Decimal，实际 {other:?}"),
    }
}

#[tokio::test]
#[ignore = "需要真实 Oracle 实例"]
async fn begin_read_only_and_commit() {
    let Some(session) = connect().await else {
        return;
    };
    let tx = session
        .begin(rustdb_db_api::TxOptions {
            isolation: None,
            read_only: true,
            deferrable: false,
        })
        .await
        .expect("begin read only");
    session.commit(tx).await.expect("commit");
}

#[tokio::test]
#[ignore = "需要真实 Oracle 实例"]
async fn call_timeout_maps_to_timeout() {
    let Some(session) = connect().await else {
        return;
    };
    let result = session
        .execute(ExecuteRequest {
            sql: "BEGIN DBMS_SESSION.SLEEP(5); END;".into(),
            params: Vec::new(),
            fetch: FetchOptions {
                max_rows: 1,
                timeout_ms: Some(300),
                ..FetchOptions::default()
            },
            tx: None,
            read_only: false,
            request_id: None,
            query_id: None,
        })
        .await;
    match result {
        Err(err) => assert_eq!(err.kind, DriverErrorKind::Timeout, "{err}"),
        Ok(_) => panic!("期望调用超时"),
    }
}
