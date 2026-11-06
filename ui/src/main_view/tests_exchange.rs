//! 文档、血缘、结构对比、导出与跨库传输面板测试。

use super::tests_helpers::*;
use super::*;

/// F-87：文档面板渲染表清单与导出入口，Esc 关闭。
#[gpui::test]
fn docs_panel_lists_tables_and_closes_on_escape(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let input = docs_input();
            let preview =
                rustdb_core::generate_document(&input, &rustdb_core::docs::DocNotes::default());
            view.docs = Some(DocsPanel {
                namespace: ns("main"),
                loading: false,
                input: Some(input),
                notes: rustdb_core::docs::DocNotes::default(),
                project_note: designer_input(window, cx, "项目说明", None),
                table_note: designer_input(window, cx, "表笔记", None),
                column_note: designer_input(window, cx, "列笔记", None),
                selected_table: None,
                selected_column: None,
                preview: Some(preview),
                error: None,
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let panel = vcx.debug_bounds("docs-panel").expect("面板应打开");
    assert!(vcx.debug_bounds("doc-table-0").is_some());
    assert!(vcx.debug_bounds("doc-save").is_some());
    assert!(vcx.debug_bounds("doc-export-md").is_some());
    assert!(vcx.debug_bounds("doc-export-dbml").is_some());
    // 预览区必须拿到剩余高度，不能被 auto-height 塌陷成 0。
    let preview = vcx.debug_bounds("doc-preview").expect("预览区应渲染");
    assert!(
        preview.size.height > px(200.),
        "预览区高度应占满面板剩余空间：{:?}",
        preview
    );
    assert!(
        preview.size.width > px(300.),
        "预览区宽度应占右侧栏（h_flex 默认居中会让右栏塌陷）：{:?}",
        preview
    );
    let close = vcx.debug_bounds("docs-close").expect("缺少关闭按钮");
    assert!(
        close.bottom() <= panel.bottom(),
        "关闭按钮应落在面板内：close={close:?} panel={panel:?}"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("docs-panel").is_none(), "Esc 应关闭面板");
}

/// F-87：保存笔记会合并项目 / 表笔记、刷新预览并原子落盘。
#[gpui::test]
fn docs_notes_save_merges_inputs_and_persists(cx: &mut gpui::TestAppContext) {
    let state_path = std::env::temp_dir().join(format!(
        "rustdb-ui-docs-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&state_path);

    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let input = docs_input();
            view.doc_notes_path = state_path.clone();
            view.docs = Some(DocsPanel {
                namespace: ns("main"),
                loading: false,
                input: Some(input),
                notes: rustdb_core::docs::DocNotes::default(),
                project_note: designer_input(window, cx, "项目说明", None),
                table_note: designer_input(window, cx, "表笔记", None),
                column_note: designer_input(window, cx, "列笔记", None),
                selected_table: None,
                selected_column: None,
                preview: None,
                error: None,
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let table = vcx.debug_bounds("doc-table-0").expect("缺少表条目");
    vcx.simulate_click(table.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let panel = vcx
        .update(|_, app| view.read(app).docs.clone())
        .expect("面板应存在");
    vcx.update(|window, app| {
        panel
            .project_note
            .update(app, |state, cx| state.set_value("项目说明", window, cx));
        panel
            .table_note
            .update(app, |state, cx| state.set_value("本地表笔记", window, cx));
    });

    let save = vcx.debug_bounds("doc-save").expect("缺少保存按钮");
    vcx.simulate_click(save.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let path = ObjectPath::new(ns("main"), "orders", ObjectKind::Table);
    let stored = vcx.update(|_, app| view.read(app).doc_notes.notes("default"));
    assert_eq!(stored.project, "项目说明");
    assert_eq!(stored.table_note(&path), Some("本地表笔记"));
    assert!(state_path.exists(), "笔记应落盘");

    let preview = vcx
        .update(|_, app| view.read(app).docs.clone())
        .and_then(|panel| panel.preview)
        .expect("保存后应刷新预览");
    assert!(
        preview.markdown.contains("项目说明"),
        "{}",
        preview.markdown
    );
    assert!(
        preview.markdown.contains("本地表笔记"),
        "{}",
        preview.markdown
    );

    let _ = std::fs::remove_file(&state_path);
}

/// F-88：血缘面板按置信度过滤，Esc 关闭。
#[gpui::test]
fn lineage_panel_filters_by_confidence_and_closes(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.lineage = Some(lineage_panel(lineage_input(), window, cx));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("lineage-panel").is_some(), "面板应打开");
    assert!(vcx.debug_bounds("lineage-col-1").is_some());
    // 外键（确定）+ 同名字段（可能）各一条上游。
    let edge = vcx.debug_bounds("lineage-edge-up-0").expect("缺少上游血缘");
    let panel = vcx.debug_bounds("lineage-panel").expect("面板应打开");
    assert!(
        edge.size.height > px(10.),
        "血缘条目不能被 auto-height 塌陷：{edge:?}"
    );
    assert!(
        edge.left() > panel.left() + px(210.),
        "血缘条目应落在右侧栏（h_flex 默认居中会让右栏塌陷）：edge={edge:?} panel={panel:?}"
    );
    assert!(vcx.debug_bounds("lineage-edge-up-1").is_some());

    let chip = vcx
        .debug_bounds("lineage-min-certain")
        .expect("缺少置信度筛选");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("lineage-edge-up-0").is_some());
    assert!(
        vcx.debug_bounds("lineage-edge-up-1").is_none(),
        "只保留确定级时应过滤同名字段"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("lineage-panel").is_none(),
        "Esc 应关闭面板"
    );
}

/// F-86：面板列出差异，执行要二次确认；Esc 先撤确认再关面板。
#[gpui::test]
fn compare_panel_requires_second_click_and_closes(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.compare = Some(compare_panel(window, cx));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("compare-panel").is_some(), "面板应打开");
    assert!(vcx.debug_bounds("compare-table-0").is_some());
    assert!(vcx.debug_bounds("compare-execute").is_some());
    assert!(vcx.debug_bounds("compare-toggle-rollback").is_some());

    let execute = vcx.debug_bounds("compare-execute").expect("缺少执行按钮");
    vcx.simulate_click(execute.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let confirming = vcx.update(|_, app| {
        view.read(app)
            .compare
            .as_ref()
            .is_some_and(|panel| panel.confirm_execute)
    });
    assert!(confirming, "第一次点击只做确认，不直接执行脚本");

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("compare-panel").is_some(),
        "Esc 先只撤销确认，面板不关"
    );
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("compare-panel").is_none(),
        "再按 Esc 关闭面板"
    );
}

/// F-86：勾选「允许 DROP 多余表」后重算差异并把配置写回磁盘。
#[gpui::test]
fn compare_drop_chip_recomputes_and_persists_config(cx: &mut gpui::TestAppContext) {
    let state_path = std::env::temp_dir().join(format!(
        "rustdb-ui-compare-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_file(&state_path);

    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.compare_config_path = state_path.clone();
            view.compare = Some(compare_panel(window, cx));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let chip = vcx
        .debug_bounds("compare-drop-tables")
        .expect("缺少 DROP 表开关");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (drop_tables, removed) = vcx.update(|_, app| {
        let view = view.read(app);
        let panel = view.compare.as_ref().expect("面板应仍在");
        (
            panel.drop_tables,
            panel.report.as_ref().map(|report| report.counts().removed),
        )
    });
    assert!(drop_tables, "点击后应打开 DROP 表开关");
    assert_eq!(removed, Some(1), "目标多出的 legacy 表应计为删除");

    let saved = std::fs::read_to_string(&state_path).expect("配置应已落盘");
    assert!(saved.contains("\"version\""), "{saved}");
    assert!(saved.contains("\"ignored_tables\""), "{saved}");
    let _ = std::fs::remove_file(&state_path);
}

/// F-76：默认全选；可全不选、单表切换；空选时拒绝提交；Esc 关闭。
#[gpui::test]
fn dump_panel_toggles_tables_and_requires_selection(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.dump = Some(dump_panel());
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("dump-panel").is_some(), "面板应打开");
    assert!(vcx.debug_bounds("dump-table-1").is_some());

    let all = vcx.debug_bounds("dump-toggle-all").expect("缺少全选开关");
    vcx.simulate_click(all.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let count = vcx.update(|_, app| {
        view.read(app)
            .dump
            .as_ref()
            .map(|panel| panel.selected.len())
    });
    assert_eq!(count, Some(0), "全不选应清空选择");

    let first = vcx.debug_bounds("dump-table-0").expect("缺少表条目");
    vcx.simulate_click(first.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let selected =
        vcx.update(|_, app| view.read(app).dump.as_ref().map(DumpPanel::selected_tables));
    assert_eq!(selected, Some(vec!["channels".to_string()]));

    // 再取消这张表，空选提交应被拦下，不发起导出。
    let first = vcx.debug_bounds("dump-table-0").expect("缺少表条目");
    vcx.simulate_click(first.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let run = vcx.debug_bounds("dump-run").expect("缺少导出按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let error = vcx.update(|_, app| {
        view.read(app)
            .dump
            .as_ref()
            .and_then(|panel| panel.error.clone())
    });
    assert_eq!(error.as_deref(), Some("至少选择一张表"));

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("dump-panel").is_none(), "Esc 应关闭面板");
}

/// F-76：结构 / 数据 / gzip / 中止都是独立开关，默认不压不删；多库可切分文件。
#[gpui::test]
fn dump_panel_options_toggle_independently(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let mut panel = dump_panel();
            panel.namespaces = vec![ns("main"), ns("reporting")];
            view.dump = Some(panel);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let defaults = vcx.update(|_, app| {
        view.read(app)
            .dump
            .as_ref()
            .map(|panel| panel.options.clone())
    });
    let defaults = defaults.expect("面板应存在");
    assert!(defaults.include_structure && defaults.include_data && defaults.include_objects);
    assert!(!defaults.include_drops && !defaults.gzip);
    assert_eq!(defaults.error_mode, rustdb_core::DumpErrorMode::Comment);

    for selector in ["dump-structure", "dump-gzip", "dump-drops", "dump-abort"] {
        let bound = vcx.debug_bounds(selector).expect("缺少选项开关");
        vcx.simulate_click(bound.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    }
    let options = vcx.update(|_, app| {
        view.read(app)
            .dump
            .as_ref()
            .map(|panel| panel.options.clone())
    });
    let options = options.expect("面板应存在");
    assert!(!options.include_structure, "结构开关应关闭");
    assert!(options.include_data, "数据开关不受影响");
    assert!(options.gzip && options.include_drops);
    assert_eq!(options.error_mode, rustdb_core::DumpErrorMode::Abort);

    let scope = vcx
        .debug_bounds("dump-scope-all")
        .expect("多库时应出现「全部库」开关");
    vcx.simulate_click(scope.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let export_all = vcx.update(|_, app| {
        view.read(app)
            .dump
            .as_ref()
            .is_some_and(|panel| panel.export_all)
    });
    assert!(export_all, "切换后应按库分文件导出");
    assert!(
        vcx.debug_bounds("dump-table-0").is_none(),
        "按库导出时表选择不生效"
    );
}

/// F-75：未预览不能执行；预览后选项 / 表选择 / 目标变化一律失效；Esc 关闭。
#[gpui::test]
fn transfer_panel_requires_preview_before_run(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.transfer = Some(transfer_panel_for_test());
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("transfer-panel").is_some(), "面板应打开");
    assert!(vcx.debug_bounds("transfer-target-1").is_some());
    assert!(vcx.debug_bounds("transfer-preview").is_some());

    // 未预览时执行按钮禁用，点它不应产生请求或错误。
    let run = vcx.debug_bounds("transfer-run").expect("缺少执行按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let error = vcx.update(|_, app| {
        view.read(app)
            .transfer
            .as_ref()
            .and_then(|panel| panel.error.clone())
    });
    assert!(error.is_none(), "未预览时按钮应禁用（{error:?}）");

    // 预览事件到达后可以执行；请求因测试环境没有会话而失败，但通道是通的。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.apply_event(&transfer_envelope(TransferEvent::Preview {
                lines: vec!["表 orders → orders（建表）".into()],
                warnings: vec!["跨引擎提示".into()],
            }));
            cx.notify();
        });
    });
    vcx.run_until_parked();
    let (ready, lines) = vcx.update(|_, app| {
        let panel = view.read(app).transfer.clone().expect("面板应存在");
        (panel.preview_ready, panel.lines.len())
    });
    assert!(ready);
    assert_eq!(lines, 1);

    let run = vcx.debug_bounds("transfer-run").expect("缺少执行按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let error = vcx.update(|_, app| {
        view.read(app)
            .transfer
            .as_ref()
            .and_then(|panel| panel.error.clone())
    });
    assert_eq!(error.as_deref(), Some("找不到目标连接 t1"));

    // 选项 / 目标 / 表选择变化都会让旧预览失效。
    for selector in [
        "transfer-mode",
        "transfer-case",
        "transfer-batch",
        "transfer-target-1",
    ] {
        let bound = vcx.debug_bounds(selector).expect("缺少选项开关");
        vcx.simulate_click(bound.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    }
    let (ready, mode) = vcx.update(|_, app| {
        let panel = view.read(app).transfer.clone().expect("面板应存在");
        (panel.preview_ready, panel.options.mode)
    });
    assert!(!ready, "选项变化后必须重新预览");
    assert_eq!(mode, TransferMode::Recreate, "模式应在四种之间循环");

    let table = vcx.debug_bounds("transfer-table-0").expect("缺少表条目");
    vcx.simulate_click(table.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let selected = vcx.update(|_, app| {
        view.read(app)
            .transfer
            .as_ref()
            .map(TransferPanel::selected_tables)
    });
    assert_eq!(selected, Some(vec!["orders".to_string()]));

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("transfer-panel").is_none(),
        "Esc 应关闭面板"
    );
}

/// F-75：传输事件驱动进度、备份提示与失败清单。
#[gpui::test]
fn transfer_events_track_progress_and_failures(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.transfer = Some(transfer_panel_for_test());
            view.apply_event(&transfer_envelope(TransferEvent::Started {
                source: "local".into(),
                target: "目标一".into(),
                tables: 2,
                mode: "重建".into(),
                backup: true,
            }));
            view.apply_event(&transfer_envelope(TransferEvent::BackupDone {
                path: "backup.sql".into(),
                bytes: 2048,
            }));
            view.apply_event(&transfer_envelope(TransferEvent::TableStarted {
                table: "orders".into(),
                index: 1,
            }));
            view.apply_event(&transfer_envelope(TransferEvent::TableProgress {
                table: "orders".into(),
                rows: 500,
            }));
            view.apply_event(&transfer_envelope(TransferEvent::TableDone {
                table: "orders".into(),
                rows: 1000,
                error: None,
            }));
            view.apply_event(&transfer_envelope(TransferEvent::TableDone {
                table: "channels".into(),
                rows: 0,
                error: Some("目标表不存在".into()),
            }));
            view.apply_event(&transfer_envelope(TransferEvent::Finished {
                tables: 2,
                rows: 1000,
                failed: 1,
                elapsed_ms: 1500,
            }));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (running, done, rows, failed, failures, backup, elapsed, status) = vcx.update(|_, app| {
        let view = view.read(app);
        let panel = view.transfer.clone().expect("面板应存在");
        (
            panel.running,
            panel.done,
            panel.rows,
            panel.failed,
            panel.failures,
            panel.backup,
            panel.elapsed_ms,
            view.status.clone(),
        )
    });
    assert!(!running);
    assert_eq!((done, rows, failed), (2, 1000, 1));
    assert_eq!(failures, vec!["channels：目标表不存在".to_string()]);
    assert_eq!(
        backup,
        Some(("backup.sql".to_string(), 2048)),
        "备份路径与大小应保留"
    );
    assert_eq!(elapsed, 1500);
    assert!(status.contains("1 张失败"), "{status}");
}

/// F-78：未预览不能同步；预览后目标 / 选项 / 表选择变化一律失效；Esc 关闭。
#[gpui::test]
fn data_compare_panel_requires_preview_before_sync(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.data_compare = Some(data_compare_panel_for_test(window, cx));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("data-compare-panel").is_some(),
        "面板应打开"
    );
    assert!(vcx.debug_bounds("dc-target-1").is_some());
    assert!(
        vcx.debug_bounds("dc-table-1").is_some(),
        "两张表都应参与布局"
    );

    // 未预览时执行按钮禁用，点它不应产生请求或错误。
    let sync = vcx.debug_bounds("dc-sync").expect("缺少执行同步按钮");
    vcx.simulate_click(sync.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let error = vcx.update(|_, app| {
        view.read(app)
            .data_compare
            .as_ref()
            .and_then(|panel| panel.error.clone())
    });
    assert!(error.is_none(), "未预览时按钮应禁用（{error:?}）");

    // 预览事件到达后可以同步；测试环境没有目标配置，失败信息来自请求预检但通道是通的。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.apply_event(&data_compare_envelope(DataCompareEvent::Preview {
                lines: vec!["INSERT INTO orders VALUES (1)".into()],
                warnings: Vec::new(),
                tables: 2,
                insert: 1,
                update: 0,
                delete: 0,
            }));
            cx.notify();
        });
    });
    vcx.run_until_parked();
    let (ready, insert) = vcx.update(|_, app| {
        let panel = view.read(app).data_compare.clone().expect("面板应存在");
        (panel.preview_ready, panel.insert)
    });
    assert!(ready);
    assert_eq!(insert, 1);

    let sync = vcx.debug_bounds("dc-sync").expect("缺少执行同步按钮");
    vcx.simulate_click(sync.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let error = vcx.update(|_, app| {
        view.read(app)
            .data_compare
            .as_ref()
            .and_then(|panel| panel.error.clone())
    });
    assert_eq!(error.as_deref(), Some("找不到目标连接 t1"));

    // 选项 / 目标 / 表选择变化都会让旧预览失效。
    for selector in ["dc-update", "dc-case", "dc-batch", "dc-target-1"] {
        let bound = vcx.debug_bounds(selector).expect("缺少选项开关");
        vcx.simulate_click(bound.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    }
    let (ready, update, target) = vcx.update(|_, app| {
        let panel = view.read(app).data_compare.clone().expect("面板应存在");
        (panel.preview_ready, panel.options.update, panel.target)
    });
    assert!(!ready, "选项变化后必须重新生成差异");
    assert!(!update, "UPDATE 开关应关闭");
    assert_eq!(target.as_deref(), Some("t2"));

    let table = vcx.debug_bounds("dc-table-0").expect("缺少表条目");
    vcx.simulate_click(table.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let selected = vcx.update(|_, app| {
        view.read(app)
            .data_compare
            .as_ref()
            .map(DataComparePanel::selected_tables)
    });
    assert_eq!(selected, Some(vec!["orders".to_string()]));

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("data-compare-panel").is_none(),
        "Esc 应关闭面板"
    );
}

/// F-78：对比事件累计差异计数、警告与失败清单；同步结束后旧脚本失效。
#[gpui::test]
fn data_compare_events_track_progress_and_failures(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.data_compare = Some(data_compare_panel_for_test(window, cx));
            view.apply_event(&data_compare_envelope(DataCompareEvent::Started {
                source: "local".into(),
                target: "目标一".into(),
                tables: 2,
                apply: false,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::TableStarted {
                table: "orders".into(),
                index: 0,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::TableDone {
                table: "orders".into(),
                insert: 2,
                update: 1,
                delete: 0,
                same: 10,
                warnings: vec!["键值重复，已保留首行".into()],
                error: None,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::TableDone {
                table: "channels".into(),
                insert: 0,
                update: 0,
                delete: 1,
                same: 3,
                warnings: Vec::new(),
                error: Some("列类型不一致".into()),
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::Preview {
                lines: vec![
                    "INSERT INTO orders VALUES (1)".into(),
                    "DELETE FROM channels WHERE id = 4".into(),
                ],
                warnings: vec!["channels：列类型不一致".into()],
                tables: 2,
                insert: 2,
                update: 1,
                delete: 1,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::SyncStarted {
                statements: 3,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::SyncProgress {
                table: "channels".into(),
                applied: 2,
                statements: 3,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::SyncTableFailed {
                table: "channels".into(),
                message: "权限不足".into(),
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::SyncDone {
                applied: 2,
                failed: 1,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::Finished {
                tables: 2,
                insert: 2,
                update: 1,
                delete: 1,
                elapsed_ms: 900,
                failed: 1,
            }));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let panel = vcx.update(|_, app| view.read(app).data_compare.clone().expect("面板应存在"));
    assert!(!panel.running);
    assert_eq!((panel.done, panel.failed), (2, 1));
    assert_eq!((panel.insert, panel.update, panel.delete), (2, 1, 1));
    assert_eq!((panel.applied, panel.statements), (2, 3));
    assert_eq!(panel.elapsed_ms, 900);
    assert_eq!(panel.lines.len(), 2, "预览脚本应保留");
    assert_eq!(
        panel.failures,
        vec![
            "channels：列类型不一致".to_string(),
            "channels：权限不足".to_string()
        ]
    );
    assert!(panel.preview_ready, "只读对比后脚本仍可执行");

    // 同步型运行结束后数据已变，旧预览必须失效。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.apply_event(&data_compare_envelope(DataCompareEvent::Started {
                source: "local".into(),
                target: "目标一".into(),
                tables: 2,
                apply: true,
            }));
            view.apply_event(&data_compare_envelope(DataCompareEvent::Finished {
                tables: 0,
                insert: 0,
                update: 0,
                delete: 0,
                elapsed_ms: 200,
                failed: 0,
            }));
            cx.notify();
        });
    });
    vcx.run_until_parked();
    let panel = vcx.update(|_, app| view.read(app).data_compare.clone().expect("面板应存在"));
    assert!(!panel.preview_ready, "同步后旧脚本不能再用");
    assert!(panel.lines.is_empty());
}

/// F-88：切换字段会按新列重算上游 / 下游。
#[gpui::test]
fn lineage_column_select_recomputes_report(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let mut panel = lineage_panel(lineage_input(), window, cx);
            panel.selected = None;
            panel.report = None;
            view.lineage = Some(panel);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let second = vcx.debug_bounds("lineage-col-1").expect("缺少字段条目");
    vcx.simulate_click(second.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (selected, upstream) = vcx.update(|_, app| {
        let panel = view.read(app).lineage.clone().expect("面板应存在");
        let upstream = panel
            .report
            .as_ref()
            .map(|report| report.upstream.len())
            .unwrap_or(0);
        (panel.selected, upstream)
    });
    assert_eq!(selected.as_deref(), Some("user_id"));
    assert_eq!(upstream, 2, "应重算出外键与同名字段两条上游");
}
