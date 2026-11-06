//! 导入 / 消息 / 事务 / 历史 / 文件 / 行操作与批量更新测试。

use super::tests_helpers::*;
use super::*;

/// 导入面板渲染选项、字段映射与执行按钮；映射可循环切换。
#[gpui::test]
fn import_panel_renders_options_and_mapping(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            view.import_panel = Some(import_panel_fixture());
            cx.notify();
        });
    });
    vcx.run_until_parked();

    for id in [
        "import-panel",
        "import-format",
        "import-encoding",
        "import-mode",
        "import-header",
        "import-trim",
        "import-empty",
        "import-skip",
        "import-batch",
        "import-map-0",
        "import-map-1",
        "import-run",
    ] {
        assert!(vcx.debug_bounds(id).is_some(), "导入面板缺少 {id}");
    }

    // 映射循环：点一次从 id 换到下一列（或忽略）。
    let before = vcx.update(|_, app| {
        view.read(app)
            .import_panel
            .as_ref()
            .expect("面板已关闭")
            .mapping[0]
    });
    let map = vcx.debug_bounds("import-map-0").expect("缺少首个映射按钮");
    vcx.simulate_click(map.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let after = vcx.update(|_, app| {
        view.read(app)
            .import_panel
            .as_ref()
            .expect("面板已关闭")
            .mapping[0]
    });
    assert_ne!(before, after, "点击映射按钮应切换目标列");
}

/// 清空模式必须点两次「导入」：第一次只提示，不执行。
#[gpui::test]
fn import_truncate_needs_confirmation(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            let mut panel = import_panel_fixture();
            panel.options.mode = import::ImportMode::Truncate;
            panel.rebuild(DbKind::Sqlite);
            view.import_panel = Some(panel);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let run = vcx.debug_bounds("import-run").expect("缺少导入按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (confirm, still_open) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.import_panel
                .as_ref()
                .map(|panel| panel.confirm_truncate)
                .unwrap_or(false),
            view.import_panel.is_some(),
        )
    });
    assert!(still_open, "第一次点击不应执行或关闭面板");
    assert!(confirm, "第一次点击应进入二次确认");
    assert!(
        vcx.debug_bounds("import-warning").is_some(),
        "应显示清空告警"
    );
}

/// 有告警时结果区多出「消息」页签，点开显示服务端原文。
#[gpui::test]
fn messages_tab_shows_warnings(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 3, 2);
            let tab = &mut view.tabs[0];
            tab.messages = vec![
                TabMessage {
                    level: rustdb_db_api::WarningLevel::Info,
                    text: "NOTICE: 表 orders 已存在".into(),
                },
                TabMessage {
                    level: rustdb_db_api::WarningLevel::Warning,
                    text: "WARNING: 截断到 1000 行".into(),
                },
            ];
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("result-tab-messages").is_some(),
        "有消息时应出现「消息」页签"
    );
    assert!(
        vcx.debug_bounds("message-panel").is_none(),
        "默认仍停在结果集"
    );

    let tab = vcx
        .debug_bounds("result-tab-messages")
        .expect("缺少消息页签");
    vcx.simulate_click(tab.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("message-panel").is_some(),
        "应切到消息面板"
    );
    assert!(vcx.debug_bounds("cell-0-0").is_none(), "消息页不渲染网格");

    // 切回结果集。
    let first = vcx.debug_bounds("result-tab-0").expect("缺少结果页签");
    vcx.simulate_click(first.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("cell-0-0").is_some(), "应回到结果网格");
    assert!(vcx.debug_bounds("message-panel").is_none());
}

/// 自动提交开关可切换；事务活跃时工具栏出现「提交 / 回滚」与语句计数。
#[gpui::test]
fn tx_toolbar_toggles_autocommit(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main")];
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.update(|_, app| view.read(app).auto_commit),
        "默认应为自动提交"
    );
    let toggle = vcx.debug_bounds("tx-autocommit").expect("缺少自动提交开关");
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        !vcx.update(|_, app| view.read(app).auto_commit),
        "点击后应关闭自动提交"
    );

    // 事务开始后出现状态与两个按钮。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.tx = Some(TxView {
                started: Instant::now(),
                statements: 3,
                read_only: false,
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tx-status").is_some(), "应显示事务状态");
    let commit = vcx.debug_bounds("tx-commit");
    assert!(commit.is_some(), "应出现提交按钮");
    assert!(vcx.debug_bounds("tx-rollback").is_some(), "应出现回滚按钮");
}

/// 历史面板：按当前数据源列历史，点条目把 SQL 放回编辑器。
#[gpui::test]
fn history_panel_inserts_sql(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main")];
            view.current_ns = Some(ns("main"));
            view.active_profile = Some("demo-sqlite".into());
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            view.sql_input
                .update(cx, |state, cx| state.set_value("", window, cx));
            // 两条历史：一条属于当前数据源，一条属于别的。
            let id = view
                .core
                .history()
                .begin("demo-sqlite", "SELECT 42 AS answer");
            view.core.history().finish(id, 7, true);
            view.core.history().begin("other-db", "SELECT 99");
            cx.notify();
        });
    });
    vcx.run_until_parked();

    // 切到历史面板。
    let rail = vcx.debug_bounds("rail-history").expect("缺少历史入口");
    vcx.simulate_click(rail.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("history-list").is_some(), "应渲染历史列表");
    let first = vcx
        .debug_bounds("hist-1")
        .expect("缺少当前数据源的历史条目");
    assert!(
        vcx.debug_bounds("hist-2").is_none(),
        "其他数据源的历史不应混进来"
    );

    vcx.simulate_click(first.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let text = vcx.update(|_, app| view.read(app).sql_input.read(app).value().to_string());
    assert_eq!(text, "SELECT 42 AS answer", "点历史应把 SQL 放回编辑器");
}

/// 历史面板：来源筛选只留下所选来源的条目。
#[gpui::test]
fn history_source_filter_narrows_list(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let (manual, ai) = vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.active_profile = Some("demo-sqlite".into());
            let manual = view.core.history().begin("demo-sqlite", "SELECT 1");
            let ai = view
                .core
                .history()
                .begin_with("demo-sqlite", "SELECT 2", HistorySource::Ai);
            cx.notify();
            (manual, ai)
        })
    });
    vcx.run_until_parked();

    let rail = vcx.debug_bounds("rail-history").expect("缺少历史入口");
    vcx.simulate_click(rail.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx
        .debug_bounds(selector(format!("hist-{manual}")))
        .is_some());
    assert!(vcx.debug_bounds(selector(format!("hist-{ai}"))).is_some());

    let source = vcx
        .debug_bounds("history-source-filter")
        .expect("缺少来源筛选");
    vcx.simulate_click(source.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let pick = vcx.debug_bounds("history-source-ai").expect("缺少 AI 选项");
    vcx.simulate_click(pick.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds(selector(format!("hist-{ai}"))).is_some(),
        "AI 来源应保留"
    );
    assert!(
        vcx.debug_bounds(selector(format!("hist-{manual}")))
            .is_none(),
        "手动来源应被筛掉"
    );
    assert!(
        vcx.debug_bounds("history-scrim").is_none(),
        "选完应关闭下拉"
    );
}

/// 时间范围换算：今天从本地零点起算。
#[test]
fn history_range_starts_at_local_midnight() {
    use chrono::TimeZone as _;
    let noon = chrono::Local
        .with_ymd_and_hms(2026, 10, 23, 12, 30, 0)
        .unwrap()
        .timestamp_millis();
    let expected = chrono::Local
        .with_ymd_and_hms(2026, 10, 23, 0, 0, 0)
        .unwrap()
        .timestamp_millis();
    assert_eq!(HistoryRange::Today.from_ms(noon), Some(expected));
    assert_eq!(
        HistoryRange::Days7.from_ms(noon),
        Some(noon - 7 * 86_400_000)
    );
    assert_eq!(HistoryRange::All.from_ms(noon), None);
}

/// 运行记录条：点条目恢复结果、可固定、可关闭。
#[gpui::test]
fn run_strip_switches_pins_and_closes(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let (run_id, set_id) = vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.active_profile = Some("demo-sqlite".into());
            view.tabs = vec![SqlTab::new(1, Some(ns("main")))];
            view.active_tab = 0;
            let columns: Arc<[ColumnMeta]> = vec![ColumnMeta::new(
                "v",
                rustdb_db_api::LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
            )]
            .into();
            let store = view.core.results();
            let (set_id, _) = store.create(columns, QueryId::new());
            store.append(
                set_id,
                rustdb_db_api::RowBatch::new(vec![vec![Value::Int(7)]]),
            );
            store.complete(set_id, 9, None);
            let run = view.core.runs().begin("demo-sqlite", "SELECT 7", "Select");
            view.core.runs().attach_result(run, set_id);
            view.core
                .runs()
                .finish(run, rustdb_core::runs::RunState::Completed, 9, 1, None);
            cx.notify();
            (run, set_id)
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("run-strip").is_some(), "应渲染运行记录条");

    let chip = vcx
        .debug_bounds(selector(format!("run-chip-{run_id}")))
        .expect("缺少运行记录");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let (result_id, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.active()
                .and_then(|tab| tab.results.first().map(|r| r.id)),
            view.status.clone(),
        )
    });
    assert_eq!(result_id, Some(set_id), "点运行记录应恢复对应结果集");
    assert!(status.contains("查看运行"), "状态栏应说明当前查看的运行");

    let pin = vcx
        .debug_bounds(selector(format!("run-pin-{run_id}")))
        .expect("缺少固定按钮");
    vcx.simulate_click(pin.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).core.runs().get(run_id).unwrap().pinned),
        "固定状态应写回 RunLog"
    );

    let close = vcx
        .debug_bounds(selector(format!("run-close-{run_id}")))
        .expect("缺少关闭按钮");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).core.runs().get(run_id).is_none()),
        "关闭应移除运行记录"
    );
    assert!(
        vcx.debug_bounds("run-strip").is_none(),
        "没有记录时整条隐藏"
    );
}

/// F-64：打开文件进新标签，同一文件不重复开，保存写回原路径。
#[gpui::test]
fn sql_file_opens_in_tab_and_saves_back(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let dir = std::env::temp_dir().join(format!(
        "rustdb-ui-file-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    let path = dir.join("report.sql");
    std::fs::write(&path, "SELECT 1;\nSELECT 2;").expect("写临时文件失败");

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.load_sql_file(path.clone(), "SELECT 1;\nSELECT 2;".into(), window, cx);
        });
    });
    vcx.run_until_parked();

    let (tabs, title, editor) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.tabs.len(),
            view.tabs[0].title(),
            view.sql_input.read(app).value().to_string(),
        )
    });
    assert_eq!(tabs, 1, "打开文件应新建标签");
    assert_eq!(title, "report.sql", "文件标签显示文件名");
    assert_eq!(editor, "SELECT 1;\nSELECT 2;");
    assert!(
        vcx.update(|_, app| view.read(app).status.contains("2 条语句")),
        "状态栏应报告文件内语句数"
    );

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.load_sql_file(path.clone(), "SELECT 1;".into(), window, cx);
        });
    });
    assert_eq!(
        vcx.update(|_, app| view.read(app).tabs.len()),
        1,
        "同一文件应复用标签"
    );

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.sql_input
                .update(cx, |state, cx| state.set_value("SELECT 42;", window, cx));
            view.save_sql_file(false, window, cx);
        });
    });
    vcx.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&path).expect("读回文件失败"),
        "SELECT 42;",
        "保存应写回已绑定的文件"
    );
    assert!(vcx.update(|_, app| view.read(app).status.contains("已保存")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// F-64：文件树只列 `.sql` 与子目录，展开子目录后才出现深层文件。
#[gpui::test]
fn file_tree_lists_sql_and_expands_folders(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let dir = std::env::temp_dir().join(format!(
        "rustdb-ui-tree-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).expect("创建临时目录失败");
    std::fs::write(dir.join("report.sql"), "SELECT 1;").expect("写 SQL 失败");
    std::fs::write(dir.join("notes.txt"), "ignore").expect("写文本失败");
    std::fs::write(sub.join("nested.sql"), "SELECT 2;").expect("写嵌套 SQL 失败");

    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.file_root = Some(dir.clone());
            view.panel = PanelKind::Files;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("file-report.sql").is_some(),
        "应列出 .sql 文件"
    );
    assert!(
        vcx.debug_bounds("file-notes.txt").is_none(),
        "非 .sql 文件不显示"
    );
    assert!(vcx.debug_bounds("file-dir-sub").is_some(), "应列出子目录");
    assert!(
        vcx.debug_bounds("file-nested.sql").is_none(),
        "子目录未展开时不应显示其中的文件"
    );

    let folder = vcx.debug_bounds("file-dir-sub").expect("缺少子目录节点");
    vcx.simulate_click(folder.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("file-nested.sql").is_some(),
        "展开后应显示子目录里的 SQL"
    );

    vcx.simulate_click(folder.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("file-nested.sql").is_none(),
        "再次点击应收起子目录"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 批量失败后状态栏出现「继续」：只在失败语句后面还有语句时显示。
#[gpui::test]
fn failed_batch_offers_resume(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let mut tab = SqlTab::new(1, Some(ns("main")));
            tab.batch = Some((
                QueryId::new(),
                vec![
                    BatchStatement {
                        text: "SELECT 1".into(),
                        start: 0,
                    },
                    BatchStatement {
                        text: "SELECT * FROM nope".into(),
                        start: 10,
                    },
                    BatchStatement {
                        text: "SELECT 3".into(),
                        start: 31,
                    },
                ],
            ));
            tab.failed_at = Some(1);
            tab.current_statement = Some((2, 3));
            view.tabs.push(tab);
            view.active_tab = 0;
            view.status = "第 2/3 条失败".into();
            view.status_kind = StatusKind::Error;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert_eq!(
        vcx.update(|_, app| view.read(app).resumable()),
        Some((1, 3)),
        "失败后还剩一条可继续"
    );
    let resume = vcx.debug_bounds("resume-batch").expect("缺少继续按钮");
    vcx.simulate_click(resume.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).status.contains("请先在")),
        "没有连接时继续应给出提示"
    );

    // 失败在最后一条：没有可继续的语句，按钮消失。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.tabs[0].failed_at = Some(2);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("resume-batch").is_none());
}

/// F-61：诊断徽标出现，服务端错误入口把光标送到驱动给出的字符位置。
#[gpui::test]
fn diagnostics_badge_and_error_jump(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let sql = "SELECT 1;\nSELECT 中 FROM nope;";
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.completion.borrow_mut().objects = vec!["orders".into()];
            let mut tab = SqlTab::new(1, Some(ns("main")));
            tab.sql = sql.into();
            tab.sql_error = Some(SqlErrorMark {
                message: "syntax error at or near \"中\"".into(),
                start: 17,
                end: 20,
            });
            view.tabs.push(tab);
            view.active_tab = 0;
            view.sql_input
                .update(cx, |state, cx| state.set_value(sql, window, cx));
            view.diag_dirty = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("diag-badge").is_some(),
        "没有元数据时应提示无法完全校验"
    );
    let goto = vcx.debug_bounds("goto-error").expect("缺少跳到错误入口");
    vcx.simulate_click(goto.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let cursor = vcx.update(|_, app| view.read(app).sql_input.read(app).cursor());
    assert_eq!(cursor, 17, "光标应落在错误字符上（多字节按字符换算）");

    let badge = vcx.debug_bounds("diag-badge").expect("缺少诊断徽标");
    vcx.simulate_click(badge.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let cursor = vcx.update(|_, app| view.read(app).sql_input.read(app).cursor());
    assert_eq!(cursor, 26, "点徽标应跳到第一条语义问题的表名");
}

/// AC-28：取消后保留已执行结果，并明确标出中断与未执行的语句。
#[gpui::test]
fn cancel_marks_unexecuted_statements(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let sql = "SELECT 1;\nSELECT 2;\nSELECT 3;\nSELECT 4;";
    let qid = QueryId("q-cancel".into());
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let mut tab = SqlTab::new(1, Some(ns("main")));
            tab.sql = sql.into();
            tab.running = Some(qid.clone());
            tab.current_statement = Some((2, 4));
            tab.batch = Some((
                qid.clone(),
                vec![
                    BatchStatement {
                        text: "SELECT 1".into(),
                        start: 0,
                    },
                    BatchStatement {
                        text: "SELECT 2".into(),
                        start: 10,
                    },
                    BatchStatement {
                        text: "SELECT 3".into(),
                        start: 20,
                    },
                    BatchStatement {
                        text: "SELECT 4".into(),
                        start: 30,
                    },
                ],
            ));
            view.tabs.push(tab);
            view.active_tab = 0;
            view.sql_input
                .update(cx, |state, cx| state.set_value(sql, window, cx));
            view.apply_event(&rustdb_core::EventEnvelope {
                seq: 1,
                session_seq: Some(1),
                ts_ms: 0,
                session_id: None,
                event: AppEvent::Query(QueryEvent::Cancelled {
                    query_id: qid.clone(),
                    elapsed_ms: 9,
                    at: 2,
                    total: 4,
                }),
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (status, messages, running, batch) = vcx.update(|_, app| {
        let view = view.read(app);
        let tab = &view.tabs[0];
        (
            view.status.clone(),
            tab.messages
                .iter()
                .map(|message| message.text.clone())
                .collect::<Vec<_>>(),
            tab.running.is_some(),
            tab.batch.is_some(),
        )
    });
    assert_eq!(
        status, "已取消 · 成功 1 条 · 中断 1 条 · 未执行 2 条",
        "状态栏要给出成功 / 中断 / 未执行计数"
    );
    assert!(!running && !batch, "取消后应结束本次运行");
    assert_eq!(messages.len(), 1, "未执行语句应留一条消息");
    assert!(
        messages[0].contains("第 3–4 条未执行") && messages[0].contains("第 3、4 行"),
        "消息应标出未执行语句及其行号：{}",
        messages[0]
    );
}

/// F-70：分页结果在页脚出现翻页控件，未连接时给出明确提示。
#[gpui::test]
fn page_controls_follow_has_more(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 3, 2);
            view.tabs[0].results[0].page = Some(TablePage {
                path: table_path("main", "orders"),
                page: 1,
                page_size: 100,
                has_more: true,
                order: None,
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("page-prev").is_none(),
        "第一页不应出现上一页"
    );
    assert!(vcx.debug_bounds("page-next").is_some(), "还有下一页");
    assert!(vcx.debug_bounds("page-size").is_some(), "应能切换页大小");
    let next = vcx.debug_bounds("page-next").expect("缺少下一页按钮");
    vcx.simulate_click(next.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).status.contains("请先")),
        "未连接时翻页应提示先连接"
    );

    // 最后一页：没有下一页，但可以回上一页。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let page = view.tabs[0].results[0].page.as_mut().expect("应有分页状态");
            page.page = 3;
            page.has_more = false;
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("page-next").is_none(),
        "最后一页没有下一页"
    );
    assert!(vcx.debug_bounds("page-prev").is_some(), "应能回到上一页");
}

/// F-62：有占位符先出参数面板；缺值拦截；原始 SQL 需二次确认。
#[gpui::test]
fn params_panel_collects_values_and_confirms_raw(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let sql = "SELECT * FROM orders WHERE id = :id";
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let mut tab = SqlTab::new(1, Some(ns("main")));
            tab.sql = sql.into();
            view.tabs.push(tab);
            view.active_tab = 0;
            // 只验证参数面板：会话是假的，真正提交会在后面报「连接尚未建立」。
            view.session = Some(SessionId("s-test".into()));
            view.sql_input
                .update(cx, |state, cx| state.set_value(sql, window, cx));
            view.run(window, cx);
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("params-panel").is_some(),
        "有占位符时应先弹参数面板"
    );
    let slots = vcx.update(|_, app| {
        view.read(app)
            .params_panel
            .as_ref()
            .expect("面板应存在")
            .slots
            .iter()
            .map(|slot| (slot.name.clone(), slot.style))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        slots,
        vec![("id".to_string(), PlaceholderStyle::ColonNamed)],
        "应按名字去重并保留占位符样式"
    );

    // 空值点执行：面板保留并提示。
    let run = vcx.debug_bounds("params-run").expect("缺少执行按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("params-panel").is_some(),
        "缺值不应关闭面板"
    );
    assert!(
        vcx.debug_bounds("params-warning").is_some(),
        "缺值应有行内提示"
    );
    assert!(
        vcx.update(|_, app| view.read(app).status.contains("还有 1 个参数未填写")),
        "状态栏应说明缺几个参数"
    );

    // 填入值并把类型切成「原始 SQL」。
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let input = view.params_panel.as_ref().expect("面板应存在").slots[0]
                .input
                .clone();
            input.update(cx, |state, cx| state.set_value("1=1", window, cx));
            for _ in 0..5 {
                view.cycle_param_kind(0, cx);
            }
            assert_eq!(
                view.params_panel.as_ref().unwrap().slots[0].kind,
                Some(ParamKind::Raw)
            );
        });
    });
    vcx.run_until_parked();

    // 第一次执行只确认，不提交。
    let run = vcx.debug_bounds("params-run").expect("缺少执行按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("params-panel").is_some(),
        "原始 SQL 第一次执行应停在确认"
    );
    assert!(
        vcx.update(|_, app| view
            .read(app)
            .params_panel
            .as_ref()
            .and_then(|panel| panel.warning.clone())
            .is_some_and(|warning| warning.contains("原始 SQL"))),
        "确认提示应说明会原样拼接"
    );

    // 第二次执行才真正提交；会话是假的，因此报连接问题而面板关闭。
    let run = vcx.debug_bounds("params-run").expect("缺少执行按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).params_panel.is_none()),
        "确认后应关闭面板并提交"
    );
    assert!(
        vcx.update(|_, app| view.read(app).status.contains("连接尚未建立")),
        "渲染后的 SQL 应进入提交流程"
    );
}

/// F-71：行操作菜单能追加多行，并生成对应数量的 INSERT。
#[gpui::test]
fn row_menu_appends_multiple_rows(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);

    assert!(open_row_menu(&mut vcx), "行操作菜单应能打开");
    assert!(vcx.debug_bounds("menu-row-append-10").is_some());
    assert!(vcx.debug_bounds("menu-row-clone").is_some());
    assert!(vcx.debug_bounds("menu-row-paste").is_some());
    assert!(vcx.debug_bounds("menu-row-generator").is_some());
    assert!(vcx.debug_bounds("menu-row-default").is_some());
    assert!(vcx.debug_bounds("menu-row-revert").is_some());

    let append = vcx
        .debug_bounds("menu-row-append-10")
        .expect("缺少追加 10 行");
    vcx.simulate_click(append.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (rows, pending, plan) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        (
            result.rows.len(),
            result.change_count(),
            result
                .changes
                .as_ref()
                .map(|set| set.plan(rustdb_db_api::DbKind::Sqlite).unwrap().len())
                .unwrap_or(0),
        )
    });
    assert_eq!(rows, 12, "应追加 10 行");
    assert_eq!(pending, 10, "应有 10 条待提交变更");
    assert_eq!(plan, 10, "应生成 10 条 INSERT");
    assert!(
        vcx.debug_bounds("row-menu-scrim").is_none(),
        "执行后菜单应关闭"
    );
}

/// F-71：克隆行时主键交给数据库默认值（INSERT 不带主键列）。
#[gpui::test]
fn clone_row_leaves_primary_key_to_database(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 1));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(open_row_menu(&mut vcx));
    let clone = vcx.debug_bounds("menu-row-clone").expect("缺少克隆行");
    vcx.simulate_click(clone.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (rows, sql) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        let plan = result
            .changes
            .as_ref()
            .map(|set| set.plan(rustdb_db_api::DbKind::Sqlite).unwrap())
            .unwrap_or_default();
        (
            result.rows.len(),
            plan.first().map(|s| s.sql.clone()).unwrap_or_default(),
        )
    });
    assert_eq!(rows, 3, "应多出一行");
    assert!(sql.starts_with("INSERT"), "{sql}");
    assert!(!sql.contains("\"id\""), "主键应留给数据库：{sql}");
    assert!(sql.contains("\"name\""), "{sql}");
}

/// F-71：从剪贴板粘贴 TSV，首行表头被跳过，空字段按 NULL 处理。
#[gpui::test]
fn paste_tsv_fills_selected_cells(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.write_to_clipboard(ClipboardItem::new_string(
        "id\tname\n7\tseven\n".to_string(),
    ));
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 0));
            view.paste_tsv(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (row, pending) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        (result.rows[0].clone(), result.change_count())
    });
    assert_eq!(row[0], Value::Int(7), "整数列应按类型解析");
    assert_eq!(row[1], Value::Text("seven".into()));
    assert_eq!(pending, 1, "整行改动合并为一条变更");
}

/// F-71：设为数据库默认值 → 单元格标「(默认)」，SQL 用 `SET 列 = DEFAULT`。
#[gpui::test]
fn set_cell_default_marks_default_column(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 1));
            view.set_selected_cell_default(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (marked, sql) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        let plan = result
            .changes
            .as_ref()
            .map(|set| set.plan(rustdb_db_api::DbKind::Sqlite).unwrap())
            .unwrap_or_default();
        (
            result.cell_default(0, 1),
            plan.first().map(|s| s.sql.clone()).unwrap_or_default(),
        )
    });
    assert!(marked, "该单元格应标记为数据库默认值");
    assert!(sql.contains("\"name\" = DEFAULT"), "{sql}");
    assert!(sql.contains("WHERE \"id\" = 1"), "{sql}");
}

/// F-71：撤销本行变更 → 恢复原值；新增行整行作废。
#[gpui::test]
fn revert_row_restores_original_value(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 1));
            view.append_rows(1, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (rows, pending) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        (result.rows.len(), result.change_count())
    });
    assert_eq!(rows, 3);
    assert_eq!(pending, 1);

    // 撤销新增行：整行消失，变更集清空。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((2, 0));
            view.revert_selected_row(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (rows, pending) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        (result.rows.len(), result.change_count())
    });
    assert_eq!(rows, 2, "新增行应整行撤销");
    assert_eq!(pending, 0);
}

/// F-71：数据生成器按表达式新增行，其余列交给数据库默认值。
#[gpui::test]
fn generator_appends_generated_rows(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 1));
            view.open_generator(window, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("generator-panel").is_some(),
        "生成器面板应打开"
    );

    let panel = vcx
        .update(|_, app| view.read(app).generator.clone())
        .expect("生成器面板应存在");
    vcx.update(|window, app| {
        panel.expr.update(app, |state, cx| {
            state.set_value("seq:1:1".to_string(), window, cx)
        });
        panel.count.update(app, |state, cx| {
            state.set_value("3".to_string(), window, cx)
        });
    });
    vcx.run_until_parked();

    let apply = vcx.debug_bounds("generator-apply").expect("缺少生成按钮");
    vcx.simulate_click(apply.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (rows, generated, plan) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        let plan = result
            .changes
            .as_ref()
            .map(|set| set.plan(rustdb_db_api::DbKind::Sqlite).unwrap())
            .unwrap_or_default();
        (
            result.rows.len(),
            result.rows[2].clone(),
            plan.iter().map(|s| s.sql.clone()).collect::<Vec<_>>(),
        )
    });
    assert_eq!(rows, 5, "应新增 3 行");
    assert_eq!(generated[1], Value::Text("1".into()), "序列值写入目标列");
    assert_eq!(plan.len(), 3);
    assert!(plan[0].contains("\"name\""), "{}", plan[0]);
    assert!(!plan[0].contains("\"id\""), "其余列交给默认值：{}", plan[0]);
    assert!(
        vcx.debug_bounds("generator-panel").is_none(),
        "生成后面板应关闭"
    );
}

/// F-73：批量更新面板可开关、切模式、增删条件；Esc 关闭。
#[gpui::test]
fn bulk_panel_toggles_conditions_and_closes_on_escape(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    editable_view(&mut vcx, &holder);

    let open = vcx.debug_bounds("bulk-open").expect("缺少批量更新入口");
    vcx.simulate_click(open.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bulk-panel").is_some(), "面板应打开");
    assert!(vcx.debug_bounds("bulk-cond-0-op-eq").is_some());
    assert!(
        vcx.debug_bounds("bulk-custom-where").is_none(),
        "默认是结构化条件"
    );

    let custom = vcx
        .debug_bounds("bulk-mode-custom")
        .expect("缺少自定义模式");
    vcx.simulate_click(custom.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bulk-custom-where").is_some(),
        "应切到自定义 WHERE"
    );
    assert!(vcx.debug_bounds("bulk-cond-0-op-eq").is_none());

    let structured = vcx
        .debug_bounds("bulk-mode-struct")
        .expect("缺少结构化模式");
    vcx.simulate_click(structured.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let add = vcx
        .debug_bounds("bulk-add-condition")
        .expect("缺少添加条件");
    vcx.simulate_click(add.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bulk-cond-1-op-eq").is_some(),
        "应新增第二条条件"
    );

    let remove = vcx.debug_bounds("bulk-remove-1").expect("缺少移除按钮");
    vcx.simulate_click(remove.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("bulk-cond-1-op-eq").is_none(),
        "应移除第二条条件"
    );

    // 没有预览 / 没有连接时点执行，只回填面板错误，不关面板。
    let execute = vcx.debug_bounds("bulk-execute").expect("缺少执行按钮");
    vcx.simulate_click(execute.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bulk-error").is_some(), "应显示面板错误");

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("bulk-panel").is_none(), "Esc 应关闭面板");
}

/// F-73：面板输入完整落到请求（SET 列 / 新值 / 条件列与值）。
#[gpui::test]
fn bulk_request_reads_panel_inputs(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.open_bulk_panel(window, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let panel = vcx
        .update(|_, app| view.read(app).bulk.clone())
        .expect("面板应存在");
    let row = panel.conditions[0].clone();
    vcx.update(|window, app| {
        panel
            .set_column
            .update(app, |state, cx| state.set_value("name", window, cx));
        panel
            .set_value
            .update(app, |state, cx| state.set_value("paid", window, cx));
        row.column
            .update(app, |state, cx| state.set_value("id", window, cx));
        row.value
            .update(app, |state, cx| state.set_value("2", window, cx));
    });

    let request = vcx
        .update(|_, app| view.update(app, |view, cx| view.bulk_request(cx)))
        .expect("请求应可组装");
    assert_eq!(
        request.target.display_label(),
        table_path("main", "orders").display_label()
    );
    assert_eq!(request.set_column, "name");
    assert_eq!(request.set_value, "paid");
    match request.filter {
        rustdb_core::BulkFilter::Structured { join, conditions } => {
            assert_eq!(join, rustdb_core::BulkJoin::And);
            assert_eq!(conditions.len(), 1);
            assert_eq!(conditions[0].column, "id");
            assert_eq!(conditions[0].op, rustdb_core::BulkOp::Eq);
            assert_eq!(conditions[0].value, "2");
        }
        other => panic!("默认应是结构化条件：{other:?}"),
    }
}

/// F-73：外键入口只在驱动声明 `foreign_key_pivot` 时出现。
#[gpui::test]
fn foreign_key_jump_requires_capability(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.capabilities = Some(Capabilities::default());
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("fk-jump").is_none(),
        "能力关闭时不显示外键入口"
    );

    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let mut caps = Capabilities::default();
            caps.foreign_key_pivot = true;
            view.capabilities = Some(caps);
            view.table_fks.insert(
                table_path("main", "orders").display_label(),
                vec![ForeignKeyInfo {
                    name: "fk_orders_channel".into(),
                    columns: vec!["name".into()],
                    referenced_table: "channels".into(),
                    referenced_columns: vec!["name".into()],
                    on_delete: None,
                    on_update: None,
                }],
            );
            view.selected_cell = Some((0, 1));
            cx.notify();
        });
    });
    vcx.run_until_parked();
    let jump = vcx
        .debug_bounds("fk-jump")
        .expect("能力开启后应显示外键入口");
    vcx.simulate_click(jump.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    // 测试环境没有会话：应给出明确提示而不是静默。
    let status = vcx.update(|_, app| view.read(app).status.clone());
    assert!(status.contains("请先连接数据源"), "{status}");
}
