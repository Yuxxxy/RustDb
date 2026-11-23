//! 只读连接的**服务端**加固（F-10 双保险的另一半）。
//!
//! 客户端语句拦截在 `core`，这里验证的是驱动层：连上只读连接后，
//! `PRAGMA query_only` 必须在 SQLite 端真正生效——否则「双保险」只有一半。
//! 测试**直接走驱动**、绕开 core 的拦截，才能证明服务端自己也会拒绝写入。

use rustdb_db_api::{
    ConnectOptions, DriverFactory, ExecuteRequest, FetchOptions, Session, Value,
};
use rustdb_db_sqlite::SqliteFactory;
use std::collections::BTreeMap;

fn opts(database: &str, read_only: bool) -> ConnectOptions {
    ConnectOptions {
        host: None,
        port: None,
        database: Some(database.to_string()),
        username: None,
        secret: None,
        ssl_mode: None,
        params: BTreeMap::new(),
        connect_timeout_ms: 5_000,
        read_only,
    }
}

fn request(sql: &str) -> ExecuteRequest {
    ExecuteRequest {
        sql: sql.to_string(),
        params: Vec::new(),
        fetch: FetchOptions {
            max_rows: 100,
            page_size: 50,
            timeout_ms: None,
            first_screen: 10,
        },
        tx: None,
        read_only: true,
        request_id: None,
        query_id: None,
    }
}

/// 收集一个结果流的全部行；返回（行, 失败消息）。
async fn run(session: &dyn Session, sql: &str) -> (Vec<Vec<Value>>, Option<String>) {
    match session.execute(request(sql)).await {
        Ok(handle) => {
            let mut rows = Vec::new();
            let mut stream = handle.stream;
            while let Some(batch) = futures_util::StreamExt::next(&mut stream).await {
                match batch {
                    Ok(batch) => rows.extend(batch.rows),
                    Err(error) => return (rows, Some(error.user_message())),
                }
            }
            (rows, None)
        }
        Err(error) => (Vec::new(), Some(error.user_message())),
    }
}

#[tokio::test]
async fn read_only_session_rejects_writes_at_the_server() {
    let dir = std::env::temp_dir().join(format!("rustdb-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ro.db");
    let path_text = path.to_string_lossy().to_string();

    let factory = SqliteFactory::new();
    {
        // 先建表并落一行，再重开只读连接。
        let writer = factory.connect(&opts(&path_text, false)).await.unwrap();
        let (_, error) = run(writer.as_ref(), "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").await;
        assert_eq!(error, None, "建表应成功");
        let (_, error) = run(writer.as_ref(), "INSERT INTO t (name) VALUES ('x')").await;
        assert_eq!(error, None, "写入应成功");
    }

    let reader = factory.connect(&opts(&path_text, true)).await.unwrap();
    assert!(reader.server().read_only, "连接元数据应标记只读");

    // 只读会话仍能读。
    let (rows, error) = run(reader.as_ref(), "SELECT count(*) FROM t").await;
    assert_eq!(error, None, "只读连接必须能查询");
    assert_eq!(rows.len(), 1);

    // 但写入必须在**服务端**被拒：这里绕过了 core 的语句拦截，直达驱动。
    let (_, error) = run(reader.as_ref(), "INSERT INTO t (name) VALUES ('y')").await;
    let message = error.expect("只读会话的写入应被服务端拒绝");
    assert!(
        message.to_lowercase().contains("readonly") || message.contains("只读"),
        "错误信息应说明只读，实际：{message}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
