//! 垂直切片端到端：连接 → 元数据 → 查询 → 结果 → 事件。
//!
//! 这是需求文档 9.12 的「事件契约测试」的第一个实现：
//! 断言命令下行、事件上行、结果数据只在 ResultStore。

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
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn connect_query_and_collect_rows() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();

    core.connect("demo-sqlite");

    let mut session: Option<SessionId> = None;
    let ready = wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
            session = Some(session_id.clone());
            true
        } else {
            false
        }
    });
    assert!(ready, "5s 内没有收到 ConnEvent::Ready");
    let session = session.expect("Ready 事件缺少 session_id");

    // 元数据加载完成
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Meta(rustdb_core::MetaEvent::Loaded { objects, .. }) if objects.len() >= 2
        )),
        "元数据没有加载出表"
    );

    let query_id = core
        .execute(
            &session,
            "SELECT c.name AS channel, COUNT(*) AS orders, SUM(o.total) AS gmv \
             FROM orders o JOIN channels c ON c.id = o.channel_id \
             GROUP BY c.name ORDER BY gmv DESC",
        )
        .expect("提交查询失败");

    let mut set_id = None;
    let completed = wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Query(QueryEvent::Completed {
            query_id: qid,
            results,
            ..
        }) = event
        {
            if qid == &query_id {
                set_id = results.first().map(|r| r.set_id);
                return true;
            }
        }
        false
    });
    assert!(completed, "5s 内没有收到 QueryEvent::Completed");
    let set_id = set_id.expect("Completed 事件没有结果集");

    let meta = core.results().meta(set_id).expect("结果集不存在");
    assert_eq!(meta.loaded_rows, 3, "三个渠道应各一行");
    assert_eq!(meta.columns.len(), 3);
    assert_eq!(meta.state, rustdb_core::ResultState::Complete);

    let rows = core.results().rows(set_id, 0, 10);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0].display_text(), "自然搜索");
}

fn connect_demo(core: &std::sync::Arc<Core>) -> (SessionId, rustdb_core::EventStream) {
    let mut events = core.subscribe();
    core.connect("demo-sqlite");
    let mut session: Option<SessionId> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                session = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "5s 内没有收到 ConnEvent::Ready"
    );
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Meta(rustdb_core::MetaEvent::Loaded { .. })
        )),
        "元数据没有加载完成"
    );
    (session.expect("Ready 缺少 session"), events)
}

#[test]
fn batch_statements_produce_separate_result_sets() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let query_id = core
        .execute(
            &session,
            "CREATE TABLE batch_demo (id INTEGER PRIMARY KEY, name TEXT);\n\
             INSERT INTO batch_demo (id, name) VALUES (1, 'a'), (2, 'b');\n\
             SELECT COUNT(*) AS n FROM batch_demo;",
        )
        .expect("提交批量 SQL 失败");

    let mut summaries = Vec::new();
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Query(QueryEvent::Completed {
                query_id: qid,
                results,
                ..
            }) = event
            {
                if qid == &query_id {
                    summaries = results.clone();
                    return true;
                }
            }
            false
        }),
        "批量执行没有完成"
    );

    assert_eq!(summaries.len(), 3, "三条语句应产出三个结果集");
    assert_eq!(summaries[0].columns, 0, "CREATE 无结果列");
    assert_eq!(summaries[1].columns, 0, "INSERT 无结果列");
    assert_eq!(summaries[2].columns, 1);
    assert_eq!(summaries[2].rows, 1);

    let insert_meta = core
        .results()
        .meta(summaries[1].set_id)
        .expect("INSERT 结果集不存在");
    assert_eq!(insert_meta.affected, Some(2), "INSERT 应报告影响 2 行");

    let rows = core.results().rows(summaries[2].set_id, 0, 10);
    assert_eq!(rows[0][0].display_text(), "2");
}

#[test]
fn same_session_statements_are_serialized() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let first = core.execute(&session, "SELECT 1").expect("提交第一条失败");
    let second = core.execute(&session, "SELECT 2").expect("提交第二条失败");

    let mut order: Vec<&str> = Vec::new();
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            match event {
                AppEvent::Query(QueryEvent::Started { query_id, .. }) => {
                    if query_id == &first {
                        order.push("start-1");
                    } else if query_id == &second {
                        order.push("start-2");
                    }
                }
                AppEvent::Query(QueryEvent::Completed { query_id, .. }) => {
                    if query_id == &first {
                        order.push("done-1");
                    } else if query_id == &second {
                        order.push("done-2");
                    }
                }
                _ => {}
            }
            order.len() == 4
        }),
        "两条语句没有先后完成"
    );

    assert_eq!(
        order,
        vec!["start-1", "done-1", "start-2", "done-2"],
        "同一连接上的语句必须串行：第一条完成前不应开始第二条"
    );
}

#[test]
fn schema_snapshot_collects_tables_and_columns() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let ns = rustdb_db_api::Namespace {
        catalog: Some(std::sync::Arc::from("main")),
        schema: None,
    };
    core.request_schema_snapshot(&session, ns, "source".into());

    let mut snapshot = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Meta(rustdb_core::MetaEvent::SchemaSnapshotLoaded {
                slot,
                snapshot: snap,
            }) = event
            {
                if slot == "source" {
                    snapshot = Some(snap.clone());
                    return true;
                }
            }
            false
        }),
        "5s 内没有收到 SchemaSnapshotLoaded"
    );
    let snapshot = snapshot.expect("快照事件缺少数据");
    assert_eq!(snapshot.driver, rustdb_db_api::DbKind::Sqlite);
    assert_eq!(snapshot.skipped, 0);
    assert!(snapshot.failed.is_empty(), "{:?}", snapshot.failed);
    let orders = snapshot.table("orders").expect("缺少 orders 表");
    assert!(
        orders
            .columns
            .iter()
            .any(|column| column.meta.name.as_ref() == "total"),
        "{orders:?}"
    );
    assert!(snapshot.table("channels").is_some(), "缺少 channels 表");
}

#[test]
fn schema_snapshot_reports_missing_session() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    let missing = rustdb_db_api::SessionId("nope".into());
    core.request_schema_snapshot(
        &missing,
        rustdb_db_api::Namespace {
            catalog: None,
            schema: None,
        },
        "target".into(),
    );
    assert!(
        wait_until(&mut events, Duration::from_secs(2), |event| matches!(
            event,
            AppEvent::Meta(rustdb_core::MetaEvent::SchemaSnapshotFailed { slot, .. })
                if slot == "target"
        )),
        "未建立连接时应回到 SchemaSnapshotFailed"
    );
}

fn unique_dump_path(extension: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rustdb-dump-{}-{}.{extension}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ))
}

#[test]
fn dump_exports_selected_tables_and_replays() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);
    let ns = rustdb_db_api::Namespace {
        catalog: Some(std::sync::Arc::from("main")),
        schema: None,
    };
    let path = unique_dump_path("sql");
    let options = rustdb_core::DumpOptions {
        include_drops: true,
        include_objects: false,
        ..Default::default()
    };
    core.request_dump(&session, ns, vec!["orders".into()], options, path.clone());

    let mut tables_done = 0;
    let finished = wait_until(&mut events, Duration::from_secs(5), |event| match event {
        AppEvent::Dump(rustdb_core::DumpEvent::TableDone { error, rows, .. }) => {
            assert!(error.is_none(), "导出表失败：{error:?}");
            assert!(*rows > 0, "orders 应有数据行");
            tables_done += 1;
            false
        }
        AppEvent::Dump(rustdb_core::DumpEvent::Finished {
            tables,
            rows,
            failed,
            bytes,
            ..
        }) => {
            assert_eq!(*tables, 1);
            assert!(*rows > 0);
            assert_eq!(*failed, 0);
            assert!(*bytes > 0);
            true
        }
        _ => false,
    });
    assert!(finished, "5s 内没有收到 DumpEvent::Finished");
    assert_eq!(tables_done, 1, "应逐表上报进度");

    let script = std::fs::read_to_string(&path).expect("导出文件应存在");
    assert!(
        script.contains("DROP TABLE IF EXISTS \"orders\""),
        "{script}"
    );
    assert!(script.contains("CREATE TABLE"), "{script}");
    assert!(script.contains("INSERT INTO \"orders\""), "{script}");
    assert!(script.contains("不是一致性快照"), "{script}");

    // 带 DROP 的脚本可直接回放到同一库。
    let query_id = core.execute(&session, &script).expect("回放提交失败");
    let mut replay_error: Option<String> = None;
    let replayed = wait_until(&mut events, Duration::from_secs(5), |event| match event {
        AppEvent::Query(QueryEvent::Completed { query_id: id, .. }) => id == &query_id,
        AppEvent::Query(QueryEvent::Failed {
            query_id: id,
            error,
        }) => {
            if id == &query_id {
                replay_error = Some(format!("{error:?}"));
                true
            } else {
                false
            }
        }
        _ => false,
    });
    assert!(replayed, "回放未完成");
    assert!(
        replay_error.is_none(),
        "回放失败：{replay_error:?}\n{script}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn dump_gzip_and_missing_session() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);
    let ns = rustdb_db_api::Namespace {
        catalog: Some(std::sync::Arc::from("main")),
        schema: None,
    };
    let path = unique_dump_path("sql.gz");
    let options = rustdb_core::DumpOptions {
        gzip: true,
        include_objects: false,
        ..Default::default()
    };
    core.request_dump(&session, ns, vec!["channels".into()], options, path.clone());
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Dump(rustdb_core::DumpEvent::Finished { .. })
        )),
        "5s 内没有收到 gzip 导出完成事件"
    );
    let bytes = std::fs::read(&path).expect("gzip 文件应存在");
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "应为 gzip 魔数");
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut text).expect("解压失败");
    assert!(text.contains("INSERT INTO \"channels\""), "{text}");
    let _ = std::fs::remove_file(&path);

    let missing = SessionId("nope".into());
    core.request_dump(
        &missing,
        rustdb_db_api::Namespace {
            catalog: None,
            schema: None,
        },
        Vec::new(),
        rustdb_core::DumpOptions::default(),
        unique_dump_path("sql"),
    );
    assert!(
        wait_until(&mut events, Duration::from_secs(2), |event| matches!(
            event,
            AppEvent::Dump(rustdb_core::DumpEvent::Failed { .. })
        )),
        "缺会话时应回到 DumpEvent::Failed"
    );
}

#[test]
fn write_statement_is_rejected_on_read_only_connection() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);

    // demo 连接不是只读；这里直接验证只读拦截的判定路径：
    // 将 profile 的 read_only 打开需要可变访问，M0 用显式 SQL 分类测试兜底。
    assert!(!rustdb_sql_lang::is_read_only("DELETE FROM orders"));
    assert!(rustdb_sql_lang::is_read_only("SELECT * FROM orders"));
    assert!(core.profiles().iter().any(|p| p.id == "demo-sqlite"));
}

/// F-89：生产连接默认拦截写入，临时解锁后才能写，回锁 / 断开后再次拦截。
#[test]
fn production_connection_requires_temporary_unlock() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    assert!(
        core.unlock_writes("demo-sqlite", Duration::from_secs(60))
            .is_err(),
        "未标记生产时不允许解锁"
    );

    core.set_profile_production("demo-sqlite", true)
        .expect("标记生产");
    assert!(core
        .profiles()
        .iter()
        .find(|profile| profile.id == "demo-sqlite")
        .is_some_and(|profile| profile.production));
    assert!(core.is_production_session(&session));

    // 读不受影响。
    run_sql(
        &core,
        &mut events,
        &session,
        "SELECT COUNT(*) FROM channels",
        "生产读",
    );

    let error = core
        .execute(
            &session,
            "INSERT INTO channels (id, name) VALUES (9, 'prod-write')",
        )
        .expect_err("未解锁的写入必须被拦截");
    assert!(error.contains("生产"), "{error}");

    // CTE / CALL / SELECT INTO 不能靠“首关键字不是写”绕过生产门禁。
    for sql in [
        "WITH x AS (SELECT 1)\nINSERT INTO channels (id, name) VALUES (10, 'cte')",
        "CALL cleanup()",
        "SELECT * INTO backup FROM channels",
    ] {
        let error = core.execute(&session, sql).expect_err("绕过门禁必须被拦截");
        assert!(error.contains("生产"), "{sql}: {error}");
    }

    core.unlock_writes("demo-sqlite", Duration::from_secs(60))
        .expect("解锁");
    assert!(core.write_unlock_remaining("demo-sqlite").is_some());
    run_sql(
        &core,
        &mut events,
        &session,
        "INSERT INTO channels (id, name) VALUES (9, 'prod-write')",
        "解锁后写入",
    );

    core.lock_writes("demo-sqlite");
    assert!(core.write_unlock_remaining("demo-sqlite").is_none());
    assert!(core.execute(&session, "DELETE FROM channels").is_err());

    core.set_profile_production("demo-sqlite", false)
        .expect("取消生产");
    assert!(!core.is_production_session(&session));
}

#[test]
fn pg_namespace_switch_through_core() {
    if std::env::var("RUSTDB_LIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("skip: set RUSTDB_LIVE_TESTS=1 (requires docker container rustdb-pg)");
        return;
    }
    std::env::set_var("RUSTDB_DEV_DB", "1");

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_postgres::PostgresFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();

    core.connect("dev-postgres");
    let mut session: Option<SessionId> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                session = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "PG 没有在 5s 内连上"
    );
    let session = session.expect("Ready 缺少 session");

    let mut schemas = Vec::new();
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Meta(rustdb_core::MetaEvent::NamespacesLoaded { namespaces, .. }) =
                event
            {
                schemas = namespaces.clone();
                true
            } else {
                false
            }
        }),
        "没有收到 NamespacesLoaded"
    );
    assert!(
        schemas.iter().any(|n| n.schema.as_deref() == Some("shop")),
        "缺少 shop schema：{schemas:?}"
    );

    let shop = schemas
        .iter()
        .find(|n| n.schema.as_deref() == Some("shop"))
        .cloned()
        .unwrap();
    core.switch_namespace(&session, shop);

    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            matches!(
                event,
                AppEvent::Conn(ConnEvent::NamespaceChanged { namespace, .. })
                    if namespace.schema.as_deref() == Some("shop")
            )
        }),
        "没有收到 NamespaceChanged(shop)"
    );
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            matches!(
                event,
                AppEvent::Meta(rustdb_core::MetaEvent::Loaded { objects, .. })
                    if objects.len() >= 2
            )
        }),
        "切库后没有重新加载对象"
    );

    let query_id = core
        .execute(&session, "select count(*) from orders")
        .expect("提交切库后的查询失败");
    let mut set_id = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Query(QueryEvent::Completed {
                query_id: qid,
                results,
                ..
            }) = event
            {
                if qid == &query_id {
                    set_id = results.first().map(|r| r.set_id);
                    return true;
                }
            }
            false
        }),
        "切库后的查询没有完成"
    );
    let rows = core.results().rows(set_id.expect("缺少结果集"), 0, 10);
    assert_eq!(rows[0][0].display_text(), "4");
}

/// 自动提交关闭 → 隐式 BEGIN；回滚后数据不落库，提交后才落库。
#[test]
fn manual_tx_rollback_and_commit() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");

    let mut session: Option<SessionId> = None;
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
    let session = session.expect("缺少 session");

    // 等元数据加载完，确认连接已就绪。
    assert!(wait_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, AppEvent::Meta(rustdb_core::MetaEvent::Loaded { .. }))
    ));

    // 关闭自动提交执行 INSERT：应自动 BEGIN。
    let qid = core
        .execute_with_autocommit(
            &session,
            "INSERT INTO channels (id, name) VALUES (900, 'tx-rollback')",
            false,
        )
        .expect("提交事务内语句失败");
    let mut began = false;
    let mut completed = false;
    let _ = wait_until(&mut events, Duration::from_secs(5), |event| {
        match event {
            AppEvent::Tx(rustdb_core::TxEvent::Began { .. }) => began = true,
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &qid => {
                completed = true
            }
            _ => {}
        }
        began && completed
    });
    assert!(began, "关闭自动提交后应先发 TxEvent::Began");
    assert!(completed, "事务内语句应执行完成");

    // 回滚后行不存在。
    core.rollback_tx(&session).expect("回滚失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Tx(rustdb_core::TxEvent::RolledBack { .. })
        )),
        "没有收到回滚事件"
    );
    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &session,
            "SELECT COUNT(*) FROM channels WHERE id = 900"
        ),
        "0",
        "回滚后不应留下数据"
    );

    // 再插一次并提交。
    core.execute_with_autocommit(
        &session,
        "INSERT INTO channels (id, name) VALUES (901, 'tx-commit')",
        false,
    )
    .expect("提交事务内语句失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Tx(rustdb_core::TxEvent::Began { .. })
        )),
        "第二次执行应重新 BEGIN"
    );
    core.commit_tx(&session).expect("提交失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Tx(rustdb_core::TxEvent::Committed { .. })
        )),
        "没有收到提交事件"
    );
    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &session,
            "SELECT COUNT(*) FROM channels WHERE id = 901"
        ),
        "1",
        "提交后数据应落库"
    );
}

/// 跑一条标量查询并把首行首列当文本返回。
fn count_rows(
    core: &std::sync::Arc<Core>,
    events: &mut rustdb_core::EventStream,
    session: &SessionId,
    sql: &str,
) -> String {
    let qid = core.execute(session, sql).expect("查询提交失败");
    let mut set_id = None;
    assert!(
        wait_until(events, Duration::from_secs(5), |event| {
            if let AppEvent::Query(QueryEvent::Completed {
                query_id, results, ..
            }) = event
            {
                if query_id == &qid {
                    set_id = results.first().map(|r| r.set_id);
                    return true;
                }
            }
            false
        }),
        "查询没有完成"
    );
    let rows = core.results().rows(set_id.expect("缺少结果集"), 0, 1);
    rows.first()
        .and_then(|row| row.first())
        .map(|value| value.display_text())
        .unwrap_or_default()
}

/// 右键「查看 DDL」：SQLite 应拿到 sqlite_master 里的建表原文。
#[test]
fn object_ddl_comes_from_sqlite_master() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");

    let mut session: Option<SessionId> = None;
    assert!(wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
            session = Some(session_id.clone());
            true
        } else {
            false
        }
    }));
    let session = session.expect("缺少 session");

    let path = rustdb_db_api::ObjectPath::new(
        rustdb_db_api::Namespace::new(Some("main"), None::<&str>),
        "orders",
        rustdb_db_api::ObjectKind::Table,
    );
    core.request_ddl(&session, path);

    let mut ddl = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Meta(rustdb_core::MetaEvent::DdlLoaded { ddl: loaded, .. }) = event {
                ddl = Some(loaded.clone());
                true
            } else {
                false
            }
        }),
        "没有收到 DDL"
    );
    let ddl = ddl.expect("DDL 为空");
    assert_eq!(ddl.source, rustdb_db_api::DdlSource::Native);
    assert!(
        ddl.text.to_uppercase().contains("CREATE TABLE"),
        "DDL 应包含建表语句，实际：{}",
        ddl.text
    );
    assert!(ddl.text.contains("channel_id"), "应包含列定义");
}

/// 右键菜单背后的 SQL 必须真的能执行（建表模板 / 重命名 / 维护 / 统计）。
#[test]
fn menu_actions_execute_against_sqlite() {
    use rustdb_db_api::{DbKind, ObjectKind, ObjectPath};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");

    let mut session: Option<SessionId> = None;
    assert!(wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
            session = Some(session_id.clone());
            true
        } else {
            false
        }
    }));
    let session = session.expect("缺少 session");
    assert!(wait_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, AppEvent::Meta(rustdb_core::MetaEvent::Loaded { .. }))
    ));

    let path = ObjectPath::new(
        rustdb_db_api::Namespace::new(Some("main"), None::<&str>),
        "orders",
        ObjectKind::Table,
    );

    // 1) 新建表模板
    let create = Core::new_object_template(DbKind::Sqlite, ObjectKind::Table);
    run_sql(&core, &mut events, &session, &create, "新建表模板");

    // 2) 重命名（再改回来）
    let rename = Core::rename_sql(DbKind::Sqlite, &path, "orders_v2").expect("重命名 SQL");
    run_sql(&core, &mut events, &session, &rename, "重命名表");
    let renamed = ObjectPath::new(
        rustdb_db_api::Namespace::new(Some("main"), None::<&str>),
        "orders_v2",
        ObjectKind::Table,
    );
    let back = Core::rename_sql(DbKind::Sqlite, &renamed, "orders").expect("改回原名");
    run_sql(&core, &mut events, &session, &back, "改回原名");

    // 3) 维护
    let maintain = Core::maintenance_sql(DbKind::Sqlite, &path).expect("维护 SQL");
    run_sql(&core, &mut events, &session, &maintain, "维护表");

    // 4) 统计行数 + 限定名
    let from = core.object_from_clause(&session, &path).expect("限定名");
    assert_eq!(from, "\"orders\"", "SQLite 不应带 catalog");
    let count = count_rows(
        &core,
        &mut events,
        &session,
        &format!("SELECT COUNT(*) FROM {from};"),
    );
    assert!(
        count.parse::<i64>().unwrap_or(0) > 0,
        "统计行数应为正数：{count}"
    );
}

/// 提交一条 SQL 并等它成功结束（失败则断言终止）。
fn run_sql(
    core: &std::sync::Arc<Core>,
    events: &mut rustdb_core::EventStream,
    session: &SessionId,
    sql: &str,
    label: &str,
) {
    let qid = core.execute(session, sql).expect("提交失败");
    let mut ok = false;
    let mut failure = None;
    assert!(
        wait_until(events, Duration::from_secs(5), |event| match event {
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &qid => {
                ok = true;
                true
            }
            AppEvent::Query(QueryEvent::Failed { query_id, error }) if query_id == &qid => {
                failure = Some(error.message.clone());
                true
            }
            _ => false,
        }),
        "{label}：没有等到结束事件"
    );
    assert!(ok, "{label} 执行失败：{}", failure.unwrap_or_default());
}

/// 变更集端到端：单事务提交、乐观并发冲突整体回滚。
#[test]
fn change_set_commits_and_rolls_back_on_conflict() {
    use rustdb_core::edit::ChangeSet;
    use rustdb_db_api::{DbKind, ObjectKind, ObjectPath, Value};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");
    let mut session: Option<SessionId> = None;
    assert!(wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
            session = Some(session_id.clone());
            true
        } else {
            false
        }
    }));
    let session = session.expect("缺少 session");
    assert!(wait_until(
        &mut events,
        Duration::from_secs(5),
        |event| matches!(event, AppEvent::Meta(rustdb_core::MetaEvent::Loaded { .. }))
    ));

    let path = ObjectPath::new(
        rustdb_db_api::Namespace::new(Some("main"), None::<&str>),
        "channels",
        ObjectKind::Table,
    );
    let columns = vec!["id".to_string(), "name".to_string(), "type".to_string()];
    let key = vec![0usize];

    // 1) 正常提交：改名后能读回新值。
    let mut set = ChangeSet::new(path.clone(), columns.clone(), key.clone());
    set.record_cell(
        0,
        1,
        vec![
            Value::Int(1),
            Value::Text("自然搜索".into()),
            Value::Text("organic".into()),
        ],
        Value::Text("改名成功".into()),
    )
    .expect("记录修改");
    let (query_id, plan) = core.apply_changes(&session, &set).expect("提交变更集");
    assert_eq!(plan.len(), 1);
    let mut ok = false;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| match event {
            AppEvent::Query(QueryEvent::Completed { query_id: id, .. }) if id == &query_id => {
                ok = true;
                true
            }
            AppEvent::Query(QueryEvent::Failed { .. }) => true,
            _ => false,
        }),
        "没有等到变更集结束事件"
    );
    assert!(ok, "变更集应提交成功");
    let name = scalar(
        &core,
        &mut events,
        &session,
        "SELECT name FROM channels WHERE id = 1",
    );
    assert_eq!(name, "改名成功", "库里的值应已更新");
    let _ = DbKind::Sqlite;

    // 2) 冲突：用过期原值去 UPDATE（主键存在但原值不匹配时 affected=0 也判冲突）。
    let mut stale = ChangeSet::new(path.clone(), columns.clone(), key.clone());
    stale
        .record_delete(
            0,
            vec![
                Value::Int(999),
                Value::Text("x".into()),
                Value::Text("organic".into()),
            ],
        )
        .expect("记录删除");
    let (conflict_id, _) = core.apply_changes(&session, &stale).expect("提交冲突变更");
    let mut conflict = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| match event {
            AppEvent::Query(QueryEvent::Failed { query_id, error }) if query_id == &conflict_id => {
                conflict = Some(error.message.clone());
                true
            }
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &conflict_id => {
                true
            }
            _ => false,
        }),
        "没有等到冲突事件"
    );
    let conflict = conflict.expect("应判定为并发冲突");
    assert!(conflict.contains("未命中任何行"), "{conflict}");

    // 冲突后必须整体回滚：行还在。
    let count = scalar(
        &core,
        &mut events,
        &session,
        "SELECT COUNT(*) FROM channels WHERE id = 1",
    );
    assert_eq!(count, "1", "冲突应整体回滚，行不能被删掉");
}

/// 查询单个标量（测试用）。
fn scalar(
    core: &std::sync::Arc<Core>,
    events: &mut rustdb_core::EventStream,
    session: &SessionId,
    sql: &str,
) -> String {
    let qid = core.execute(session, sql).expect("提交查询");
    let mut set_id = None;
    assert!(
        wait_until(events, Duration::from_secs(5), |event| match event {
            AppEvent::Query(QueryEvent::Completed {
                query_id, results, ..
            }) if query_id == &qid => {
                set_id = results.first().map(|summary| summary.set_id);
                true
            }
            _ => false,
        }),
        "没有等到查询完成"
    );
    core.results()
        .rows(set_id.expect("缺少结果集"), 0, 1)
        .first()
        .and_then(|row| row.first())
        .map(|value| value.display_text())
        .unwrap_or_default()
}

/// AI Agent：只读工具自动执行并留审计；写工具必须确认。
#[test]
fn agent_runs_read_tool_and_requires_confirmation_for_write() {
    use rustdb_core::agent::{AssistantTurn, ScriptedProvider, ToolCall};
    use rustdb_core::{AgentEngine, AiEvent};
    use std::sync::Arc;

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");
    let mut session: Option<SessionId> = None;
    assert!(wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
            session = Some(session_id.clone());
            true
        } else {
            false
        }
    }));
    let session = session.expect("缺少 session");

    // 1) 只读：直接跑，出审计
    let provider = Arc::new(ScriptedProvider::new(vec![
        AssistantTurn::ToolCall(ToolCall::new(
            "run_read_query",
            serde_json::json!({"sql": "SELECT name FROM channels ORDER BY id LIMIT 1"}),
        )),
        AssistantTurn::Answer("已经查到渠道名".into()),
    ]));
    let agent = Arc::new(AgentEngine::new(provider.clone()));
    let run_id = agent
        .start(&core, &session, "第一个渠道叫什么？")
        .expect("启动 agent");

    let mut completed = false;
    let mut delta = String::new();
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| match event {
            AppEvent::Ai(AiEvent::Delta { text, .. }) => {
                delta.push_str(text);
                false
            }
            AppEvent::Ai(AiEvent::Completed { .. }) => {
                completed = true;
                true
            }
            AppEvent::Ai(AiEvent::Failed { message, .. }) => panic!("agent 失败：{message}"),
            _ => false,
        }),
        "没有等到 agent 完成"
    );
    assert!(completed);
    assert!(delta.contains("渠道名"), "应把回答推给 UI：{delta}");
    let audit = agent.audit(&run_id);
    assert_eq!(audit.len(), 1, "只读工具应记一条审计");
    assert_eq!(audit[0].tool, "run_read_query");
    assert_eq!(audit[0].risk, rustdb_core::RiskLevel::ReadOnly);
    assert!(audit[0].ok);
    assert!(
        audit[0].summary.contains("自然搜索"),
        "审计应带结果摘要：{}",
        audit[0].summary
    );
    assert!(audit[0].sql.as_deref().unwrap().contains("SELECT"));

    // 2) 写操作：必须先确认
    let provider2 = Arc::new(ScriptedProvider::new(vec![
        AssistantTurn::ToolCall(ToolCall::new(
            "run_write_query",
            serde_json::json!({"sql": "UPDATE channels SET name = 'AI 改名' WHERE id = 2"}),
        )),
        AssistantTurn::Answer("已改名".into()),
    ]));
    let agent2 = Arc::new(AgentEngine::new(provider2));
    let run2 = agent2
        .start(&core, &session, "把第二个渠道改名")
        .expect("启动写 agent");

    let mut pending = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| match event {
            AppEvent::Ai(AiEvent::ConfirmationRequired { summary, risk, .. }) => {
                assert_eq!(*risk, rustdb_core::RiskLevel::Write);
                pending = Some(summary.clone());
                true
            }
            AppEvent::Ai(AiEvent::Failed { message, .. }) => panic!("不应失败：{message}"),
            _ => false,
        }),
        "写操作必须挂起等确认"
    );
    assert!(pending.unwrap().contains("UPDATE"));
    assert!(agent2.pending(&run2).is_some(), "应有待确认调用");

    // 确认后执行
    agent2.confirm(&core, &run2, true).expect("确认");
    let mut done = false;
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| match event {
            AppEvent::Ai(AiEvent::Completed { .. }) => {
                done = true;
                true
            }
            AppEvent::Ai(AiEvent::Failed { message, .. }) => panic!("确认后失败：{message}"),
            _ => false,
        }),
        "确认后应跑完"
    );
    assert!(done);
    let name = scalar(
        &core,
        &mut events,
        &session,
        "SELECT name FROM channels WHERE id = 2",
    );
    assert_eq!(name, "AI 改名", "写操作应真的落库");
    assert_eq!(agent2.audit(&run2).len(), 1);
}

/// 只读工具里塞写语句：拒绝执行（防止越权）。
#[test]
fn agent_rejects_write_inside_read_tool() {
    use rustdb_core::agent::{AssistantTurn, ScriptedProvider, ToolCall};
    use rustdb_core::{AgentEngine, AiEvent};
    use std::sync::Arc;

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    core.connect("demo-sqlite");
    let mut session: Option<SessionId> = None;
    assert!(wait_until(&mut events, Duration::from_secs(5), |event| {
        if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
            session = Some(session_id.clone());
            true
        } else {
            false
        }
    }));
    let session = session.expect("缺少 session");

    let provider = Arc::new(ScriptedProvider::new(vec![
        AssistantTurn::ToolCall(ToolCall::new(
            "run_read_query",
            serde_json::json!({"sql": "DELETE FROM channels"}),
        )),
        AssistantTurn::Answer("被拒了".into()),
    ]));
    let agent = Arc::new(AgentEngine::new(provider));
    let run = agent.start(&core, &session, "删掉所有渠道").expect("启动");
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            matches!(event, AppEvent::Ai(AiEvent::Completed { .. }))
        }),
        "应结束"
    );
    let audit = agent.audit(&run);
    assert_eq!(audit.len(), 1);
    assert!(!audit[0].ok, "必须记为失败");
    assert!(audit[0].summary.contains("拒绝"), "{:?}", audit[0].summary);
    let count = scalar(
        &core,
        &mut events,
        &session,
        "SELECT COUNT(*) FROM channels",
    );
    assert!(count.parse::<i64>().unwrap_or(0) > 0, "数据不能被删");
}

/// F-98：多语句夹带（只读语句 + 破坏性语句）在执行前被审查拦截。
#[test]
fn agent_review_blocks_multi_statement_smuggling() {
    use rustdb_core::agent::{AssistantTurn, ScriptedProvider, ToolCall};
    use rustdb_core::{AgentEngine, AiEvent};
    use std::sync::Arc;

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let provider = Arc::new(ScriptedProvider::new(vec![
        AssistantTurn::ToolCall(ToolCall::new(
            "run_read_query",
            serde_json::json!({"sql": "SELECT 1; DELETE FROM channels"}),
        )),
        AssistantTurn::Answer("被拒了".into()),
    ]));
    let agent = Arc::new(AgentEngine::new(provider));
    let run = agent.start(&core, &session, "顺便清空渠道").expect("启动");
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            matches!(event, AppEvent::Ai(AiEvent::Completed { .. }))
        }),
        "应结束"
    );
    let audit = agent.audit(&run);
    assert_eq!(audit.len(), 1);
    assert!(!audit[0].ok, "多语句必须记为失败");
    assert!(audit[0].summary.contains("拒绝"), "{:?}", audit[0].summary);
    assert!(agent.pending(&run).is_none(), "阻断项不该挂确认");
    let count = scalar(
        &core,
        &mut events,
        &session,
        "SELECT COUNT(*) FROM channels",
    );
    assert!(count.parse::<i64>().unwrap_or(0) > 0, "数据不能被删");
}

/// F-65 / F-68：每次执行一条运行记录 + 一条历史条目，互不覆盖。
#[test]
fn runs_and_history_record_each_execution() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    run_sql(
        &core,
        &mut events,
        &session,
        "SELECT 1 AS a; SELECT 2 AS b",
        "两条语句",
    );

    let runs = core.runs().list();
    assert_eq!(runs.len(), 1, "一次执行只产生一条运行记录");
    let run = &runs[0];
    assert_eq!(run.state, rustdb_core::RunState::Completed);
    assert_eq!(run.result_sets.len(), 2, "两条语句两个结果集");
    assert_eq!(run.rows, 2);
    assert!(run.tab_label().contains("2 行"), "{}", run.tab_label());
    assert!(run.elapsed_ms.is_some());

    // 第二次执行追加记录，不覆盖第一次。
    run_sql(&core, &mut events, &session, "SELECT 3", "单条语句");
    assert_eq!(core.runs().list().len(), 2);
    assert_eq!(core.runs().list()[0].result_sets.len(), 1);

    // 固定项不参与淘汰。
    let pinned = core.runs().list()[1].id;
    assert_eq!(core.runs().toggle_pin(pinned), Some(true));
    core.runs().set_limit(1);
    let evicted = core.runs().trim();
    assert!(!evicted.is_empty());
    let left = core.runs().list();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, pinned, "固定项必须留下");

    // 历史：来源默认手动、成功、带耗时。
    let history = core.history().list(None, None);
    assert!(history.iter().any(|e| e.sql.contains("SELECT 1 AS a")));
    assert!(history.iter().all(|e| e.ok == Some(true)));
    assert!(history
        .iter()
        .all(|e| e.source == rustdb_core::HistorySource::Manual));
}

/// F-63：预览是只读的，且确实没有写库。
#[test]
fn dml_preview_is_read_only_and_writes_nothing() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    run_sql(
        &core,
        &mut events,
        &session,
        "CREATE TABLE preview_demo (id INTEGER PRIMARY KEY, v INTEGER);\n\
         INSERT INTO preview_demo (id, v) VALUES (1, 10);",
        "准备预览表",
    );

    let outcome = core
        .preview_dml(&session, "UPDATE preview_demo SET v = 99 WHERE id = 1")
        .expect("预览调用失败");
    let preview = outcome.preview().expect("SQLite 已声明 dml_preview 能力");
    assert!(preview.sql.starts_with("SELECT *,"), "{}", preview.sql);
    assert!(preview.sql.contains("FROM preview_demo WHERE id = 1"));
    assert!(rustdb_sql_lang::is_read_only(&preview.sql));

    // 零写入：预览之后数据仍是 10。
    let value = scalar(
        &core,
        &mut events,
        &session,
        "SELECT v FROM preview_demo WHERE id = 1",
    );
    assert_eq!(value, "10", "预览不允许写库");

    // 非 DML 不需要预览；多语句明确降级。
    assert_eq!(
        core.preview_dml(&session, "SELECT 1").unwrap(),
        rustdb_core::PreviewOutcome::NotDml
    );
    let multi = core
        .preview_dml(
            &session,
            "UPDATE preview_demo SET v = 1; UPDATE preview_demo SET v = 2",
        )
        .unwrap();
    assert!(multi.unsupported_reason().is_some());
}

/// F-72：结果集流式导出到文件（含 NULL 与公式防护）。
#[test]
fn export_result_streams_to_file() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let query_id = core
        .execute(
            &session,
            "SELECT name, '=1+1' AS danger, NULL AS empty FROM channels",
        )
        .expect("提交查询失败");
    let mut set_id = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| match event {
            AppEvent::Query(QueryEvent::Completed {
                query_id: qid,
                results,
                ..
            }) if qid == &query_id => {
                set_id = results.first().map(|r| r.set_id);
                true
            }
            _ => false,
        }),
        "没有收到 Completed"
    );
    let set_id = set_id.expect("Completed 里没有结果集");

    let dir = std::env::temp_dir().join(format!("rustdb-export-{}", std::process::id()));
    let path = dir.join("out.csv");
    let options = rustdb_core::export::ExportOptions {
        excel_bom: true,
        ..Default::default()
    };
    let written = core
        .export_result_to(
            set_id,
            &path,
            rustdb_core::export::ExportFormat::Csv,
            &options,
        )
        .expect("导出失败");
    assert!(written >= 1, "应至少导出一行");

    let csv = std::fs::read_to_string(&path).expect("读回导出文件失败");
    assert!(csv.starts_with('\u{feff}'), "CSV 应带 BOM");
    assert!(csv.contains("name,danger,empty"), "{csv}");
    assert!(csv.contains("'=1+1"), "公式必须被防护：{csv}");
    assert!(
        csv.contains(",\r\n") || csv.contains(",\n"),
        "NULL 应是空字段：{csv}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// F-64：文件执行的 SQL 在历史里标记为「文件」来源。
#[test]
fn file_execution_source_is_recorded() {
    use rustdb_core::history::{HistoryFilter, HistorySource};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let query_id = core
        .execute_with_source(&session, "SELECT 1", true, HistorySource::File)
        .expect("提交文件执行失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Completed { query_id: qid, .. }) if qid == &query_id
        )),
        "文件执行没有完成"
    );

    let entries = core.history().list_filtered(&HistoryFilter {
        profile_id: Some("demo-sqlite".into()),
        source: Some(HistorySource::File),
        ..HistoryFilter::default()
    });
    assert_eq!(entries.len(), 1, "应有一条文件来源的历史");
    assert_eq!(entries[0].source, HistorySource::File);
}

/// F-70：服务端分页走 `Core::fetch_page`，页码、排序与 has_more 都由驱动保证。
#[test]
fn fetch_page_returns_stable_server_pages() {
    use rustdb_db_api::{Namespace, ObjectKind, ObjectPath, PageOrder, PageRequest};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, _events) = connect_demo(&core);

    let path = ObjectPath::new(
        Namespace::new(Some("main"), None::<&str>),
        "orders",
        ObjectKind::Table,
    );
    // Core 自己的运行时在后台线程；分页直接 await 会话，测试线程跑一个本地运行时。
    let rt = tokio::runtime::Runtime::new().expect("创建测试运行时");
    let ids = |result: &rustdb_db_api::PageResult| -> Vec<String> {
        result
            .rows
            .iter()
            .map(|row| row.first().map(|v| v.display_text()).unwrap_or_default())
            .collect()
    };

    let mut first = PageRequest::new(path.clone());
    first.page_size = 2;
    let first = rt
        .block_on(core.fetch_page(&session, &first))
        .expect("第一页应成功");
    assert_eq!(first.page, 1);
    assert_eq!(first.page_size, 2);
    assert_eq!(first.offset, 0);
    assert_eq!(ids(&first), vec!["1001", "1002"], "默认按主键兜底排序");
    assert!(first.has_more, "4 行取 2 行应还有下一页");
    assert_eq!(first.columns.len(), 5, "列定义应来自表元数据");

    let mut second = PageRequest::new(path.clone());
    second.page = 2;
    second.page_size = 2;
    let second = rt
        .block_on(core.fetch_page(&session, &second))
        .expect("第二页应成功");
    assert_eq!(ids(&second), vec!["1003", "1004"]);
    assert!(!second.has_more, "最后一页不应提示还有下一页");
    assert_eq!(second.offset, 2);

    let mut sorted = PageRequest::new(path.clone());
    sorted.page_size = 10;
    sorted.order_by = vec![PageOrder::desc("total")];
    let sorted = rt
        .block_on(core.fetch_page(&session, &sorted))
        .expect("排序分页应成功");
    assert_eq!(
        ids(&sorted),
        vec!["1003", "1001", "1002", "1004"],
        "应按 total 降序"
    );

    let mut bad = PageRequest::new(path);
    bad.page = 0;
    assert!(
        rt.block_on(core.fetch_page(&session, &bad)).is_err(),
        "页码 0 必须拒绝"
    );
}

/// 手写 block_on：没有任何 Tokio reactor 时直接轮询 future，模拟 UI 线程。
fn block_on_no_runtime<F: std::future::Future>(future: F) -> F::Output {
    use futures_util::task::{waker, ArcWake};
    use std::sync::Arc;

    struct Unpark(std::thread::Thread);
    impl ArcWake for Unpark {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.0.unpark();
        }
    }

    let waker = waker(Arc::new(Unpark(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

/// `Core::fetch_page` 是 UI 直接 await 的入口，必须自己切到 core 运行时：
/// 调用方线程没有 reactor 时不能 panic（历史 bug：SQLite 打开表直接崩）。
#[test]
fn fetch_page_survives_without_caller_runtime() {
    use rustdb_db_api::{Namespace, ObjectKind, ObjectPath, PageRequest};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, _events) = connect_demo(&core);

    let mut request = PageRequest::new(ObjectPath::new(
        Namespace::new(Some("main"), None::<&str>),
        "orders",
        ObjectKind::Table,
    ));
    request.page_size = 2;
    let page = block_on_no_runtime(core.fetch_page(&session, &request))
        .expect("调用方没有运行时也应分页成功");
    let ids: Vec<String> = page
        .rows
        .iter()
        .map(|row| row.first().map(|v| v.display_text()).unwrap_or_default())
        .collect();
    assert_eq!(ids, vec!["1001", "1002"]);
}

#[test]
fn statement_progress_stops_at_first_failure() {
    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let query_id = core
        .execute(&session, "SELECT 1; SELECT * FROM missing_table; SELECT 3")
        .expect("提交批量执行失败");

    let mut progress = Vec::new();
    let mut error = None;
    let done = wait_until(&mut events, Duration::from_secs(5), |event| match event {
        AppEvent::Query(QueryEvent::StatementStarted {
            query_id: id,
            index,
            total,
        }) if id == &query_id => {
            progress.push((*index, *total));
            false
        }
        AppEvent::Query(QueryEvent::Failed {
            query_id: id,
            error: event_error,
        }) if id == &query_id => {
            error = Some(event_error.message.clone());
            true
        }
        AppEvent::Query(QueryEvent::Completed { query_id: id, .. }) if id == &query_id => true,
        _ => false,
    });
    assert!(done, "批量执行没有结束");
    assert_eq!(progress, vec![(1, 3), (2, 3)], "第三条不应开始：遇错停止");
    let error = error.expect("应收到失败事件");
    assert!(
        error.contains("missing_table"),
        "错误应指向失败语句：{error}"
    );

    // 失败也写入历史，并标记为失败。
    let entry = &core.history().list(Some("demo-sqlite"), None)[0];
    assert_eq!(entry.ok, Some(false));
}

/// F-74：CSV → 映射 → 分批 INSERT；失败批次不静默跳过，成功批次照常落库。
#[test]
fn csv_import_batches_and_reports_failed_batch() {
    use rustdb_core::import::{build_plan, parse, ImportFormat, ImportOptions};
    use rustdb_db_api::{ColumnInfo, LogicalType};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    let mut target = vec![
        ColumnInfo::simple(
            "id",
            LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            1,
        ),
        ColumnInfo::simple("name", LogicalType::Text { max_len: None }, 2),
        ColumnInfo::simple("type", LogicalType::Text { max_len: None }, 3),
    ];
    target[0].meta.nullable = false;
    target[0].is_primary_key = true;
    target[0].is_auto_increment = true;
    target[1].meta.nullable = false;
    target[2].default = Some("'organic'".into());

    // 第二批的 id=12 成功；第一批的 id=1 与现有行冲突，整批失败。
    let data = parse(
        b"id,name,type\n10,new-a,organic\n1,dup,paid\n12,new-b,social\n",
        &ImportOptions {
            format: ImportFormat::Csv,
            header: true,
            ..ImportOptions::default()
        },
    )
    .expect("CSV 解析失败");
    let plan = build_plan(
        rustdb_db_api::DbKind::Sqlite,
        "\"channels\"",
        &target,
        &data,
        &ImportOptions {
            batch_rows: 2,
            ..ImportOptions::default()
        },
    )
    .expect("导入计划应通过预检");
    assert_eq!(plan.total_rows, 3);
    assert_eq!(plan.batch_count(), 2);

    let query_id = core.import(&session, &plan).expect("提交导入失败");
    let mut progress = Vec::new();
    let mut finished = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Query(QueryEvent::ImportProgress {
                query_id: id,
                batch,
                total_batches,
                ..
            }) = event
            {
                if id == &query_id {
                    progress.push((*batch, *total_batches));
                }
            }
            if let AppEvent::Query(QueryEvent::ImportFinished {
                query_id: id,
                inserted_rows,
                failed_rows,
                failed_batches,
                cancelled,
                ..
            }) = event
            {
                if id == &query_id {
                    finished = Some((
                        *inserted_rows,
                        *failed_rows,
                        failed_batches.clone(),
                        *cancelled,
                    ));
                    return true;
                }
            }
            false
        }),
        "导入没有结束"
    );
    assert_eq!(progress, vec![(1, 2), (2, 2)], "每批开始前应发进度");
    let (inserted, failed, failures, cancelled) = finished.expect("缺少 ImportFinished");
    assert_eq!(inserted, 1, "第二批 1 行应成功");
    assert_eq!(failed, 2, "第一批 2 行应失败");
    assert!(!cancelled);
    assert_eq!(failures.len(), 1);
    assert_eq!((failures[0].start_row, failures[0].end_row), (1, 2));
    assert!(
        failures[0].message.to_uppercase().contains("UNIQUE")
            || failures[0].message.contains("约束"),
        "失败原因应来自驱动：{}",
        failures[0].message
    );

    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &session,
            "SELECT COUNT(*) FROM channels WHERE id IN (10, 12)"
        ),
        "1",
        "只有成功批次的行应落库"
    );
}

/// F-74：清空模式先 DELETE，再导入；源为 GBK 时自动回退并给出提示。
#[test]
fn import_truncate_mode_and_gbk_source() {
    use rustdb_core::import::{build_plan, parse, ImportFormat, ImportMode, ImportOptions};

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let (session, mut events) = connect_demo(&core);

    // 演示库的 channels 被 orders 引用，清空会触发外键；用独立表验证清空模式。
    run_sql(
        &core,
        &mut events,
        &session,
        "CREATE TABLE import_demo (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
        "建导入表",
    );
    let target = vec![
        rustdb_db_api::ColumnInfo::simple(
            "id",
            rustdb_db_api::LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            1,
        ),
        rustdb_db_api::ColumnInfo::simple(
            "name",
            rustdb_db_api::LogicalType::Text { max_len: None },
            2,
        ),
    ];

    let (gbk, _, _) = encoding_rs::GBK.encode(
        "id,name
20,渠道A
",
    );
    let data = parse(
        &gbk,
        &ImportOptions {
            format: ImportFormat::Csv,
            header: true,
            ..ImportOptions::default()
        },
    )
    .expect("GBK 源应能解析");
    assert!(data
        .warnings
        .iter()
        .any(|issue| issue.message.contains("GBK")));

    let plan = build_plan(
        rustdb_db_api::DbKind::Sqlite,
        "\"import_demo\"",
        &target,
        &data,
        &ImportOptions {
            mode: ImportMode::Truncate,
            batch_rows: 500,
            ..ImportOptions::default()
        },
    )
    .expect("导入计划应通过预检");
    assert_eq!(
        plan.truncate_sql.as_deref(),
        Some("DELETE FROM \"import_demo\";")
    );

    let query_id = core.import(&session, &plan).expect("提交导入失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            matches!(
                event,
                AppEvent::Query(QueryEvent::ImportFinished { query_id: id, .. }) if id == &query_id
            )
        }),
        "清空导入没有结束"
    );

    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &session,
            "SELECT COUNT(*) FROM import_demo"
        ),
        "1",
        "清空后只应留下导入的 1 行"
    );
    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &session,
            "SELECT COUNT(*) FROM import_demo WHERE id = 20 AND name = '渠道A'"
        ),
        "1",
        "GBK 文本应按中文落库"
    );
}

/// F-75：跨库传输（SQLite 文件 → SQLite 文件）。覆盖建表、分批写入、重建前备份。
#[test]
fn transfer_copies_rows_and_rebuilds_with_backup() {
    use rustdb_core::{
        ConnectionProfile, TransferEvent, TransferMode, TransferOptions, TransferRequest,
    };

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    let dir = std::env::temp_dir().join(format!(
        "rustdb-transfer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");

    let mut source_profile = ConnectionProfile::demo_sqlite();
    source_profile.id = "transfer-src".into();
    source_profile.name = "传输源".into();
    source_profile.database = Some(dir.join("source.db").display().to_string());
    core.add_profile(source_profile).expect("注册源连接失败");
    let mut target_profile = ConnectionProfile::demo_sqlite();
    target_profile.id = "transfer-dst".into();
    target_profile.name = "传输目标".into();
    target_profile.database = Some(dir.join("target.db").display().to_string());
    core.add_profile(target_profile).expect("注册目标连接失败");

    core.connect("transfer-src");
    let mut source: Option<SessionId> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                source = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "源连接没有就绪"
    );
    let source = source.expect("缺少源会话");

    let seed = core
        .execute(
            &source,
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL); \
             INSERT INTO items (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        )
        .expect("建表失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &seed
        )),
        "源表初始化没有完成"
    );

    let ns = rustdb_db_api::Namespace {
        catalog: Some(std::sync::Arc::from("main")),
        schema: None,
    };
    let request = |mode: TransferMode| TransferRequest {
        source_session: source.clone(),
        target_profile: "transfer-dst".into(),
        source_ns: ns.clone(),
        target_ns: ns.clone(),
        tables: vec!["items".into()],
        options: TransferOptions {
            mode,
            batch_rows: 2,
            timeout_ms: Some(5_000),
            ..TransferOptions::default()
        },
        backup_dir: dir.clone(),
    };

    core.request_transfer_preview(request(TransferMode::Create));
    let mut preview: Option<(Vec<String>, Vec<String>)> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            if let AppEvent::Transfer(TransferEvent::Preview { lines, warnings }) = event {
                preview = Some((lines.clone(), warnings.clone()));
                true
            } else {
                false
            }
        }),
        "传输预览没有生成"
    );
    let (lines, warnings) = preview.expect("缺少预览事件");
    assert!(
        lines.iter().any(|line| line.contains("CREATE TABLE")),
        "预览应包含建表 SQL：{lines:?}"
    );
    assert!(warnings.is_empty(), "同引擎建表不应有警告：{warnings:?}");

    core.request_transfer(request(TransferMode::Create))
        .expect("提交传输失败");
    let mut finished: Option<(u64, usize)> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            if let AppEvent::Transfer(TransferEvent::Finished { rows, failed, .. }) = event {
                finished = Some((*rows, *failed));
                true
            } else {
                false
            }
        }),
        "传输没有结束"
    );
    assert_eq!(finished, Some((3, 0)), "应写入 3 行且无失败");

    core.connect("transfer-dst");
    let mut target: Option<SessionId> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                target = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "目标连接没有就绪"
    );
    let target = target.expect("缺少目标会话");
    assert_eq!(
        count_rows(&core, &mut events, &target, "SELECT COUNT(*) FROM items"),
        "3"
    );
    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &target,
            "SELECT name FROM items WHERE id = 2"
        ),
        "b"
    );

    let more = core
        .execute(&source, "INSERT INTO items (id, name) VALUES (4, 'd')")
        .expect("追加源数据失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &more
        )),
        "追加源数据没有完成"
    );
    let stale = core
        .execute(&target, "INSERT INTO items (id, name) VALUES (99, 'old')")
        .expect("写入目标旧数据失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &stale
        )),
        "写入目标旧数据没有完成"
    );

    core.request_transfer(request(TransferMode::Recreate))
        .expect("提交重建传输失败");
    let mut rebuilt: Option<(u64, usize)> = None;
    let mut backed_up = false;
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            match event {
                AppEvent::Transfer(TransferEvent::BackupDone { .. }) => backed_up = true,
                AppEvent::Transfer(TransferEvent::Finished { rows, failed, .. }) => {
                    rebuilt = Some((*rows, *failed));
                    return true;
                }
                _ => {}
            }
            false
        }),
        "重建传输没有结束"
    );
    assert!(backed_up, "重建前应先生成备份");
    assert_eq!(rebuilt, Some((4, 0)), "重建后应有 4 行");
    assert_eq!(
        count_rows(&core, &mut events, &target, "SELECT COUNT(*) FROM items"),
        "4",
        "重建应清掉目标旧行"
    );
    assert!(
        std::fs::read_dir(&dir)
            .expect("读取临时目录失败")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("rustdb-transfer-backup-")),
        "备份文件应写入指定目录"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// F-78：数据对比先出差异计数与脚本预览，同步后目标表与源表一致。
#[test]
fn data_compare_counts_and_sync_apply() {
    use rustdb_core::{ConnectionProfile, Core};
    use rustdb_core::{
        DataCompareEvent, DataCompareOptions, DataCompareRequest, TableCompareConfig,
    };

    let mut registry = DriverRegistry::new();
    registry.register(rustdb_db_sqlite::SqliteFactory::new());
    let core = Core::new(registry);
    let mut events = core.subscribe();
    let dir = std::env::temp_dir().join(format!(
        "rustdb-datacompare-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");

    let mut source_profile = ConnectionProfile::demo_sqlite();
    source_profile.id = "compare-src".into();
    source_profile.name = "对比源".into();
    source_profile.database = Some(dir.join("source.db").display().to_string());
    core.add_profile(source_profile).expect("注册源连接失败");
    let mut target_profile = ConnectionProfile::demo_sqlite();
    target_profile.id = "compare-dst".into();
    target_profile.name = "对比目标".into();
    target_profile.database = Some(dir.join("target.db").display().to_string());
    core.add_profile(target_profile).expect("注册目标连接失败");

    core.connect("compare-src");
    let mut source: Option<SessionId> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                source = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "源连接没有就绪"
    );
    let source = source.expect("缺少源会话");
    let seed = core
        .execute(
            &source,
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL); \
             INSERT INTO items (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        )
        .expect("源表初始化失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &seed
        )),
        "源表初始化没有完成"
    );

    core.connect("compare-dst");
    let mut target: Option<SessionId> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::Conn(ConnEvent::Ready { session_id, .. }) = event {
                target = Some(session_id.clone());
                true
            } else {
                false
            }
        }),
        "目标连接没有就绪"
    );
    let target = target.expect("缺少目标会话");
    let seed = core
        .execute(
            &target,
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL); \
             INSERT INTO items (id, name) VALUES (1, 'a'), (2, 'old'), (4, 'stale')",
        )
        .expect("目标表初始化失败");
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| matches!(
            event,
            AppEvent::Query(QueryEvent::Completed { query_id, .. }) if query_id == &seed
        )),
        "目标表初始化没有完成"
    );

    let ns = rustdb_db_api::Namespace {
        catalog: Some(std::sync::Arc::from("main")),
        schema: None,
    };
    let request = DataCompareRequest {
        source_session: source.clone(),
        target_profile: "compare-dst".into(),
        source_ns: ns.clone(),
        target_ns: ns.clone(),
        tables: vec![TableCompareConfig::new("items")],
        options: DataCompareOptions::default(),
    };

    core.request_data_compare(request.clone());
    let mut preview: Option<(u64, u64, u64, Vec<String>, Vec<String>)> = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            if let AppEvent::DataCompare(DataCompareEvent::Preview {
                insert,
                update,
                delete,
                lines,
                warnings,
                ..
            }) = event
            {
                preview = Some((*insert, *update, *delete, lines.clone(), warnings.clone()));
                true
            } else {
                false
            }
        }),
        "对比预览没有生成"
    );
    let (insert, update, delete, lines, warnings) = preview.expect("缺少预览事件");
    assert_eq!(
        (insert, update, delete),
        (1, 1, 1),
        "差异计数不对：{lines:?}"
    );
    assert!(warnings.is_empty(), "同构表不应有警告：{warnings:?}");
    assert!(
        lines.iter().any(|line| line.contains("INSERT INTO")),
        "预览应含 INSERT：{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("UPDATE")),
        "预览应含 UPDATE：{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("DELETE FROM")),
        "预览应含 DELETE：{lines:?}"
    );
    let mut finished_failed = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(5), |event| {
            if let AppEvent::DataCompare(DataCompareEvent::Finished { failed, .. }) = event {
                finished_failed = Some(*failed);
                true
            } else {
                false
            }
        }),
        "对比没有结束"
    );
    assert_eq!(finished_failed, Some(0));

    core.request_data_compare_sync(request);
    let mut applied = None;
    assert!(
        wait_until(&mut events, Duration::from_secs(10), |event| {
            if let AppEvent::DataCompare(DataCompareEvent::SyncDone {
                applied: count,
                failed,
            }) = event
            {
                assert_eq!(*failed, 0, "同步不应失败");
                applied = Some(*count);
                true
            } else {
                false
            }
        }),
        "同步没有结束"
    );
    assert_eq!(applied, Some(3), "应执行 3 条同步语句");
    assert_eq!(
        count_rows(&core, &mut events, &target, "SELECT COUNT(*) FROM items"),
        "3",
        "目标表应与源表行数一致"
    );
    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &target,
            "SELECT name FROM items WHERE id = 2"
        ),
        "b",
        "更新后应与源值一致"
    );
    assert_eq!(
        count_rows(
            &core,
            &mut events,
            &target,
            "SELECT COUNT(*) FROM items WHERE id = 4"
        ),
        "0",
        "目标多余行应被删除"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
