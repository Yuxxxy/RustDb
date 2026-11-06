//! 真实 SQL Server 验证（需要 Docker 容器 `rustdb-mssql`）。
//!
//! 运行：
//! ```text
//! RUSTDB_LIVE_TESTS=1 cargo test -p rustdb-db-mssql --test live -- --nocapture
//! ```
//!
//! 容器口令是**本地测试容器**的固定口令（与 PG/MySQL live 用例同风格），
//! 生产连接一律走 keyring，不落盘。

use futures_util::StreamExt;
use rustdb_db_api::{
    ConnectOptions, DriverFactory, ExecuteRequest, FetchOptions, Namespace, ObjectKind,
    ObjectKindMask, ObjectPath,
};
use rustdb_db_mssql::MssqlFactory;
use std::collections::BTreeMap;

fn options() -> ConnectOptions {
    ConnectOptions {
        host: Some("127.0.0.1".into()),
        port: Some(51433),
        database: Some("rustdb_test".into()),
        username: Some("sa".into()),
        secret: Some("RustdbLocal123".into()),
        ssl_mode: Some("disable".into()),
        params: BTreeMap::new(),
        connect_timeout_ms: 8_000,
        read_only: false,
    }
}

fn live_enabled() -> bool {
    std::env::var("RUSTDB_LIVE_TESTS").as_deref() == Ok("1")
}

#[tokio::test]
async fn mssql_live_query_metadata_and_transaction() {
    if !live_enabled() {
        eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires docker container rustdb-mssql)");
        return;
    }

    let session = MssqlFactory
        .connect(&options())
        .await
        .expect("connect sql server");
    assert!(
        session.server().version.contains("SQL Server")
            || session
                .server()
                .version
                .to_ascii_lowercase()
                .contains("microsoft"),
        "版本串：{}",
        session.server().version
    );

    // 1) 查询：聚合 + 中文列值
    let mut handle = session
        .execute(ExecuteRequest {
            sql: "select c.name as channel, count(*) as orders, sum(o.total) as gmv \
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
        .expect("执行聚合查询");
    let mut rows = Vec::new();
    while let Some(batch) = handle.stream.next().await {
        let batch = batch.expect("取行");
        rows.extend(batch.rows);
    }
    assert_eq!(rows.len(), 2, "两个渠道各一行：{rows:?}");
    // 顺序由 gmv 倒序决定，这里按渠道名找对应行断言单量。
    let summary: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            (
                row.first()
                    .map(|value| value.display_text())
                    .unwrap_or_default(),
                row.get(1)
                    .map(|value| value.display_text())
                    .unwrap_or_default(),
            )
        })
        .collect();
    let natural = summary
        .iter()
        .find(|(name, _)| name.contains("自然搜索"))
        .expect("应有自然搜索");
    assert_eq!(natural.1, "2", "自然搜索 2 单：{summary:?}");
    let ad = summary
        .iter()
        .find(|(name, _)| name.contains("广告"))
        .expect("应有广告");
    assert_eq!(ad.1, "1", "广告 1 单：{summary:?}");

    // 2) 元数据：schema / 表 / 列
    let metadata = session.metadata();
    let namespaces = metadata.namespaces().await.expect("列命名空间");
    assert!(
        namespaces
            .iter()
            .any(|ns| ns.schema.as_deref() == Some("shop")),
        "应有 shop schema：{namespaces:?}"
    );
    let shop = namespaces
        .iter()
        .find(|ns| ns.schema.as_deref() == Some("shop"))
        .expect("shop schema");
    let objects = metadata
        .objects(shop, ObjectKindMask::only(ObjectKind::Table))
        .await
        .expect("列对象");
    assert!(
        objects
            .iter()
            .any(|object| object.path.name.as_ref() == "channels"),
        "应有 channels 表：{objects:?}"
    );

    let path = ObjectPath::new(
        Namespace::new(Some("rustdb_test"), Some("shop")),
        "channels",
        ObjectKind::Table,
    );
    let detail = metadata.table(&path).await.expect("读表结构");
    assert_eq!(detail.columns.len(), 3, "channels 有 3 列");
    let id_column = detail
        .columns
        .iter()
        .find(|column| column.meta.name.as_ref() == "id")
        .expect("id 列");
    assert!(id_column.is_primary_key, "id 应是主键");
    let name_column = detail
        .columns
        .iter()
        .find(|column| column.meta.name.as_ref() == "name")
        .expect("name 列");
    assert!(!name_column.meta.nullable, "name 是 NOT NULL");
    assert!(!detail.indexes.is_empty(), "应有主键索引");

    // 3) 事务：插入 → 回滚 → 查不到
    let tx = session.begin(Default::default()).await.expect("开事务");
    let mut insert = session
        .execute(ExecuteRequest {
            sql: "insert into shop.channels (id, name, type) values (99, N'临时', 'temp')".into(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: Some(tx.clone()),
            read_only: false,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("插入");
    while let Some(item) = insert.stream.next().await {
        item.expect("插入结果");
    }
    session.rollback(tx).await.expect("回滚");

    let mut check = session
        .execute(ExecuteRequest {
            sql: "select count(*) from shop.channels where id = 99".into(),
            params: Vec::new(),
            fetch: FetchOptions::default(),
            tx: None,
            read_only: true,
            request_id: None,
            query_id: None,
        })
        .await
        .expect("回滚后查询");
    let mut count = String::new();
    while let Some(batch) = check.stream.next().await {
        let batch = batch.expect("取行");
        if let Some(row) = batch.rows.first() {
            if let Some(value) = row.first() {
                count = value.display_text();
            }
        }
    }
    assert_eq!(count, "0", "回滚后不应留下数据");
}
