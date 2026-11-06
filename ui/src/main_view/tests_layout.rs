//! 布局、导航、菜单与连接相关回归测试。

use super::tests_helpers::*;
use super::*;

#[test]
fn sorting_keeps_nulls_last_in_both_directions() {
    let build = |sort: Option<SortState>| ResultView {
        id: ResultSetId(1),
        columns: vec![ColumnMeta::new(
            "v",
            rustdb_db_api::LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
        )],
        rows: vec![vec![Value::Int(3)], vec![Value::Null], vec![Value::Int(1)]],
        loaded: 3,
        elapsed_ms: 0,
        truncated: false,
        affected: None,
        sort,
        widths: Vec::new(),
        filters: Vec::new(),
        editable: None,
        changes: None,
        page: None,
    };

    assert_eq!(
        build(Some(SortState {
            column: 0,
            ascending: true
        }))
        .ordered_row_indices(),
        vec![2, 0, 1],
        "升序：1, 3, NULL"
    );
    assert_eq!(
        build(Some(SortState {
            column: 0,
            ascending: false
        }))
        .ordered_row_indices(),
        vec![0, 2, 1],
        "降序：3, 1, NULL —— NULL 始终在最后"
    );
    assert_eq!(
        build(None).ordered_row_indices(),
        vec![0, 1, 2],
        "未排序时保持原始顺序"
    );
}

#[test]
fn export_rows_follow_active_sort() {
    let view = ResultView {
        id: ResultSetId(1),
        columns: vec![
            ColumnMeta::new("name", rustdb_db_api::LogicalType::Text { max_len: None }),
            ColumnMeta::new(
                "n",
                rustdb_db_api::LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
            ),
        ],
        rows: vec![
            vec![Value::Text("a".into()), Value::Int(10)],
            vec![Value::Text("b".into()), Value::Int(2)],
            vec![Value::Text("c".into()), Value::Null],
        ],
        loaded: 3,
        elapsed_ms: 0,
        truncated: false,
        affected: None,
        sort: Some(SortState {
            column: 1,
            ascending: true,
        }),
        widths: Vec::new(),
        filters: Vec::new(),
        editable: None,
        changes: None,
        page: None,
    };

    let (headers, rows) = view.export_rows();
    assert_eq!(headers, vec!["name", "n"]);
    assert_eq!(
        rows,
        vec![
            vec!["b".to_string(), "2".to_string()],
            vec!["a".to_string(), "10".to_string()],
            vec!["c".to_string(), String::new()],
        ],
        "导出顺序跟随当前排序，且 NULL 为空字段"
    );
}

#[gpui::test]
fn export_button_is_clickable(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::apply_theme_mode(cx, rustdb_core::theme::ThemeMode::Dark);
    });
    let core = rustdb_core::Core::new(rustdb_db_api::DriverRegistry::new());
    let holder: std::rc::Rc<std::cell::RefCell<Option<gpui::Entity<MainView>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = holder.clone();
    let (_, vcx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| MainView::new(window, cx, core));
        *slot.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    vcx.run_until_parked();

    let bounds = vcx.debug_bounds("export-slot").expect("导出按钮缺失");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let status = vcx.update(|_, app| {
        holder
            .borrow()
            .as_ref()
            .expect("view 未创建")
            .read(app)
            .status
            .clone()
    });
    assert_eq!(status, "没有可导出的结果", "点击导出按钮应命中 on_click");
}

/// F-72：格式菜单列出全部格式；没有结果时给出提示而不是静默失败。
#[gpui::test]
fn export_format_menu_lists_all_formats(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::apply_theme_mode(cx, rustdb_core::theme::ThemeMode::Dark);
    });
    let core = rustdb_core::Core::new(rustdb_db_api::DriverRegistry::new());
    let holder: std::rc::Rc<std::cell::RefCell<Option<gpui::Entity<MainView>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = holder.clone();
    let (_, vcx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| MainView::new(window, cx, core));
        *slot.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    vcx.run_until_parked();

    let chevron = vcx.debug_bounds("export-format").expect("导出格式按钮缺失");
    vcx.simulate_mouse_down(
        chevron.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    for key in [
        "menu-fmt-csv",
        "menu-fmt-tsv",
        "menu-fmt-json",
        "menu-fmt-markdown",
        "menu-fmt-xlsx",
        "menu-fmt-sql-insert",
        "menu-fmt-sql-update",
        "menu-copy-tsv",
        "menu-copy-json",
        "menu-copy-markdown",
        "menu-copy-sql-insert",
    ] {
        let found = vcx.debug_bounds(key);
        assert!(found.is_some(), "格式菜单缺少 {key}");
    }

    let json = vcx.debug_bounds("menu-fmt-json").expect("缺少 JSON 项");
    vcx.simulate_click(json.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (status, menu_open) = vcx.update(|_, app| {
        let view = holder.borrow();
        let view = view.as_ref().expect("view 未创建").read(app);
        (view.status.clone(), view.export_menu.is_some())
    });
    assert_eq!(
        status, "请先在「连接」面板连接一个数据源",
        "未连接时应提示连接"
    );
    assert!(!menu_open, "选中格式后菜单必须关闭");

    // 「复制为」与导出共用同一编码器：未连接时同样给连接提示。
    let chevron = vcx.debug_bounds("export-format").expect("导出格式按钮缺失");
    vcx.simulate_mouse_down(
        chevron.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    let copy = vcx
        .debug_bounds("menu-copy-tsv")
        .expect("缺少「复制为 TSV」");
    vcx.simulate_click(copy.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let (status, menu_open) = vcx.update(|_, app| {
        let view = holder.borrow();
        let view = view.as_ref().expect("view 未创建").read(app);
        (view.status.clone(), view.export_menu.is_some())
    });
    assert_eq!(status, "请先在「连接」面板连接一个数据源");
    assert!(!menu_open, "复制动作后菜单必须关闭");
}

#[test]
fn server_label_drops_build_noise() {
    assert_eq!(
        short_server_label(
            "PostgreSQL 13.21 (Debian 13.21-1.pgdg120+1) on x86_64-pc-linux-gnu",
            Some("rustsql_test")
        ),
        "PostgreSQL 13.21 · rustsql_test"
    );
    assert_eq!(short_server_label("SQLite 3.52.2", None), "SQLite 3.52.2");
    assert_eq!(short_server_label("MySQL 8.0.43", Some("")), "MySQL 8.0.43");
}

#[gpui::test]
fn connection_tree_groups_and_collapses(cx: &mut gpui::TestAppContext) {
    std::env::set_var("RUSTDB_DEV_DB", "1");
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::apply_theme_mode(cx, rustdb_core::theme::ThemeMode::Dark);
    });
    let core = rustdb_core::Core::new(rustdb_db_api::DriverRegistry::new());
    let (_, vcx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| MainView::new(window, cx, core));
        gpui_component::Root::new(view, window, cx)
    });
    vcx.run_until_parked();

    let folder = vcx
        .debug_bounds("folder-本地开发")
        .expect("缺少「本地开发」文件夹节点");
    assert!(vcx.debug_bounds("conn-dev-postgres").is_some());
    assert!(vcx.debug_bounds("conn-dev-mysql").is_some());
    assert!(
        vcx.debug_bounds("conn-demo-sqlite").is_some(),
        "未分组之外的文件夹应同时渲染"
    );

    vcx.simulate_click(folder.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("conn-dev-postgres").is_none(),
        "折叠后子节点应消失"
    );
    assert!(
        vcx.debug_bounds("conn-demo-sqlite").is_some(),
        "折叠一个文件夹不应影响其他文件夹"
    );
}

#[gpui::test]
fn cards_fill_the_window(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    // 布局宽度会落盘；测试钉默认值，先清掉本机可能残留的拖拽结果。
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |main, cx| {
            main.layout = rustdb_core::LayoutSettings::default();
            main.layout.ai_open = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let body = vcx.debug_bounds("body-row").expect("body row missing");
    let sidebar = vcx.debug_bounds("sidebar-card").expect("sidebar missing");
    let workspace = vcx
        .debug_bounds("workspace-card")
        .expect("workspace missing");
    let ai = vcx.debug_bounds("ai-card").expect("ai card missing");
    let status = vcx.debug_bounds("status-bar").expect("status bar missing");

    // 卡片宽度 = 面板宽度 − 面板左右留白（首块 pl=CARD_GAP、pr=CARD_GAP*0.5）。
    let card_inset = CARD_GAP * 1.5;
    assert!(
        (f32::from(sidebar.size.width) - (260. - card_inset)).abs() < 0.5,
        "侧栏卡片宽度应为面板 260 扣掉留白（实际 {}）",
        f32::from(sidebar.size.width)
    );
    assert!(
        (f32::from(ai.size.width) - (360. - card_inset)).abs() < 0.5,
        "AI 栏卡片宽度应为面板 360 扣掉留白（实际 {}）",
        f32::from(ai.size.width)
    );
    assert!(
        sidebar.origin.x < workspace.origin.x && workspace.origin.x < ai.origin.x,
        "三列顺序必须是 侧栏 < 工作区 < AI"
    );
    assert!(
        (f32::from(body.origin.x + body.size.width)
            - f32::from(ai.origin.x + ai.size.width)
            - CARD_GAP)
            .abs()
            < 1.0,
        "AI 栏右缘必须与内容区右缘相隔一个卡片间距"
    );
    assert!(
        (f32::from(status.origin.y) - f32::from(body.origin.y + body.size.height)).abs() < 1.0,
        "状态栏必须紧贴内容区底部"
    );
    assert!(f32::from(workspace.size.height) > 100.);
}

#[gpui::test]
fn rail_toggles_sidebar_without_losing_width(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    reset_layout(&holder, vcx, temp_layout("sidebar"));

    click(vcx, "rail-connections");
    assert!(
        vcx.debug_bounds("sidebar-card").is_none(),
        "再点当前面板图标应收起侧栏"
    );

    click(vcx, "rail-connections");
    let sidebar = vcx.debug_bounds("sidebar-card").expect("侧栏应恢复");
    assert!(
        (f32::from(sidebar.size.width) - (260. - CARD_GAP * 1.5)).abs() < 0.5,
        "展开后应回到记忆宽度 260"
    );
}

#[gpui::test]
fn ai_close_and_rail_toggle_the_ai_column(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    reset_layout(&holder, vcx, temp_layout("ai"));

    // AI 栏默认收起，先用侧栏图标展开再验证关闭。
    click(vcx, "rail-ai");
    click(vcx, "ai-close");
    assert!(
        vcx.debug_bounds("ai-card").is_none(),
        "点关闭后 AI 栏应隐藏"
    );

    click(vcx, "rail-ai");
    let ai = vcx.debug_bounds("ai-card").expect("AI 栏应恢复");
    assert!(
        (f32::from(ai.size.width) - (360. - CARD_GAP * 1.5)).abs() < 0.5,
        "恢复后应回到记忆宽度 360"
    );
}

#[gpui::test]
fn switching_panels_uses_that_panels_own_width(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    reset_layout(&holder, vcx, temp_layout("per-panel"));
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |main, cx| {
            main.layout.remember_sidebar_width("files", 200.);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let card_w = |vcx: &mut gpui::VisualTestContext| -> f32 {
        f32::from(
            vcx.debug_bounds("sidebar-card")
                .expect("侧栏卡片缺失")
                .size
                .width,
        )
    };
    let connections_w = card_w(vcx);
    click(vcx, "rail-files");
    let files_w = card_w(vcx);
    assert!(
        (connections_w - files_w - 60.).abs() < 3.,
        "切到文件面板应换成它自己记的 200（连接 {connections_w}，文件 {files_w}）"
    );

    click(vcx, "rail-connections");
    assert!(
        (card_w(vcx) - connections_w).abs() < 1.,
        "切回连接面板应回到原宽度"
    );
}

/// 树里只按种类分组、组内保持驱动返回顺序。
#[test]
fn objects_group_in_kind_order() {
    let objects = vec![
        ObjectSummary {
            path: rustdb_db_api::ObjectPath::new(ns("main"), "fn_a", ObjectKind::Function),
            row_estimate: None,
            comment: None,
            has_primary_key: None,
        },
        ObjectSummary {
            path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
            row_estimate: None,
            comment: None,
            has_primary_key: None,
        },
        ObjectSummary {
            path: rustdb_db_api::ObjectPath::new(ns("main"), "channels", ObjectKind::Table),
            row_estimate: None,
            comment: None,
            has_primary_key: None,
        },
        ObjectSummary {
            path: rustdb_db_api::ObjectPath::new(ns("main"), "v_sales", ObjectKind::View),
            row_estimate: None,
            comment: None,
            has_primary_key: None,
        },
    ];
    let groups = group_objects(&objects);
    let kinds: Vec<_> = groups.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        vec![ObjectKind::Table, ObjectKind::View, ObjectKind::Function]
    );
    let tables: Vec<_> = groups[0].1.iter().map(|o| o.path.name.as_ref()).collect();
    assert_eq!(tables, vec!["orders", "channels"], "组内不应重排");
}

/// 库下拉框能打开、列出全部分组，并在选中后关闭。
#[gpui::test]
fn db_menu_lists_namespaces(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main"), ns("shop")];
            view.current_ns = Some(ns("main"));
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let picker = vcx.debug_bounds("db-picker").expect("缺少库下拉框");
    vcx.simulate_click(picker.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("db-scrim").is_some(), "应出现关闭遮罩");
    assert!(vcx.debug_bounds("db-option-main").is_some());
    let shop = vcx.debug_bounds("db-option-shop").expect("缺少 shop 选项");

    vcx.simulate_click(shop.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("db-option-shop").is_none(), "选中后应关闭");
    assert!(vcx.debug_bounds("db-scrim").is_none());
    let tab_ns = vcx.update(|_, app| {
        view.read(app)
            .tabs
            .first()
            .and_then(|tab| tab.ns_key())
            .unwrap_or_default()
    });
    assert_eq!(tab_ns, "shop", "标签页应绑定到新库");
}

/// 标签页切换会恢复各自的 SQL；关闭按钮不会顺手切页。
#[gpui::test]
fn tabs_switch_and_close(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main"), ns("shop")];
            let mut first = SqlTab::new(1, Some(ns("main")));
            first.sql = "select 1".into();
            let mut second = SqlTab::new(2, Some(ns("shop")));
            second.sql = "select 2".into();
            view.tabs = vec![first, second];
            view.active_tab = 0;
            // 编辑器与活动标签页同步（真实点击开页时由 open_tab 完成）。
            view.sql_input.update(cx, |state, cx| {
                state.set_value("select 1", window, cx);
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let second = vcx.debug_bounds("tab-2").expect("缺少第二个标签页");
    vcx.simulate_click(second.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let text = vcx.update(|_, app| view.read(app).sql_input.read(app).value().to_string());
    assert_eq!(text, "select 2", "切页后编辑器应显示该页内容");

    let close = vcx.debug_bounds("tab-close-2").expect("缺少关闭按钮");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("tab-2").is_none(), "标签页应关闭");
    let text = vcx.update(|_, app| view.read(app).sql_input.read(app).value().to_string());
    assert_eq!(text, "select 1", "关闭后回到上一个标签页的内容");
}

/// 双击列分隔线：列宽收缩到内容宽度（不再均分剩余空间）。
#[gpui::test]
fn double_click_fits_column(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 5, 3);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    // push_result 的列名是 c0/c1/c2、值是 0..49，内容远窄于均分宽度。
    let handle = vcx.debug_bounds("col-handle-1").expect("缺少列宽把手");
    vcx.simulate_mouse_down(
        handle.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    vcx.simulate_mouse_up(
        handle.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view
            .read(app)
            .active_result()
            .is_some_and(|r| r.fixed_width(1).is_none())),
        "单击不应写死列宽"
    );

    // 双击：click_count=2。
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.fit_column(1, window, cx);
        });
    });
    vcx.run_until_parked();

    let width = vcx.update(|_, app| {
        view.read(app)
            .active_result()
            .and_then(|result| result.fixed_width(1))
            .expect("双击后应写入定宽")
    });
    assert!(
        width >= MIN_COL_W && width <= 120.,
        "内容很窄，自适应后应贴近下限，实为 {width}"
    );
}

/// 库节点：单击展开、双击开页；右键出库菜单。
#[gpui::test]
fn namespace_click_and_double_click(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main")];
            view.current_ns = Some(ns("main"));
            view.session = Some(SessionId::new());
            view.active_profile = Some("demo-sqlite".to_string());
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let db = vcx.debug_bounds("db-main").expect("缺少库节点");
    // 单击：展开，不开标签页。
    vcx.simulate_click(db.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).expanded_ns.contains("main")),
        "单击应展开库节点"
    );
    assert!(
        vcx.update(|_, app| view.read(app).tabs.is_empty()),
        "单击不应打开标签页"
    );

    // 双击：开标签页。
    let db = vcx.debug_bounds("db-main").expect("缺少库节点");
    vcx.simulate_event(gpui::MouseDownEvent {
        position: db.center(),
        button: gpui::MouseButton::Left,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: db.center(),
        button: gpui::MouseButton::Left,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
    });
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view.read(app).tabs.len()),
        1,
        "双击应打开一个标签页"
    );

    // 右键：库菜单。
    vcx.simulate_mouse_down(
        db.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("ns-scrim").is_some(), "应出现库菜单遮罩");
    assert!(vcx.debug_bounds("menu-ns-open").is_some());
    assert!(vcx.debug_bounds("menu-ns-refresh").is_some());
    assert!(vcx.debug_bounds("menu-ns-copy").is_some());

    let scrim = vcx.debug_bounds("ns-scrim").expect("缺少遮罩");
    vcx.simulate_click(scrim.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("menu-ns-open").is_none());
}

/// 删除前的确认框：名字不匹配不执行，取消则关闭。
#[gpui::test]
fn drop_confirmation_requires_name(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.session = Some(SessionId::new());
            view.ask_danger(
                DangerAction::Drop {
                    path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("danger-scrim").is_some());
    assert!(
        vcx.debug_bounds("danger-input").is_some(),
        "删除需输入对象名"
    );

    let cancel = vcx.debug_bounds("danger-cancel").expect("缺少取消按钮");
    vcx.simulate_click(cancel.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).danger.is_none()),
        "取消应关闭确认框"
    );
}

/// 确认规则是纯函数：删除必须输入完全一致的对象名。
#[test]
fn danger_accepts_only_exact_name() {
    let drop_action = DangerAction::Drop {
        path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
    };
    assert!(!MainView::danger_accepts(&drop_action, ""), "空输入不通过");
    assert!(
        !MainView::danger_accepts(&drop_action, "order"),
        "近似名不通过"
    );
    assert!(
        MainView::danger_accepts(&drop_action, "orders"),
        "完全一致才通过"
    );
    assert!(
        MainView::danger_accepts(&drop_action, " orders "),
        "首尾空格可忽略"
    );

    let truncate = DangerAction::Truncate {
        path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
    };
    assert!(MainView::danger_accepts(&truncate, ""), "清空只需确认");

    let rename = DangerAction::Rename {
        path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
    };
    assert!(!MainView::danger_accepts(&rename, ""), "新名字不能为空");
    assert!(
        !MainView::danger_accepts(&rename, "orders"),
        "新名字不能与原名相同"
    );
    assert!(
        MainView::danger_accepts(&rename, "orders_v2"),
        "合法新名字通过"
    );
}

/// 「设计表」面板：列与索引都渲染出来，关闭按钮生效。
#[gpui::test]
fn table_panel_shows_columns(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            use rustdb_db_api::{ColumnInfo, ColumnMeta, IndexInfo, LogicalType, TableDetail};
            view.table_panel = Some((
                "main.orders".to_string(),
                TableDetail {
                    summary: ObjectSummary {
                        path: rustdb_db_api::ObjectPath::new(
                            ns("main"),
                            "orders",
                            ObjectKind::Table,
                        ),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: Some(true),
                    },
                    columns: vec![
                        ColumnInfo::simple(
                            "id",
                            LogicalType::Int {
                                bits: 64,
                                unsigned: false,
                            },
                            1,
                        ),
                        ColumnInfo::simple("total", LogicalType::Float { bits: 64 }, 2),
                    ],
                    indexes: vec![IndexInfo {
                        name: "pk_orders".into(),
                        columns: vec!["id".into()],
                        unique: true,
                        primary: true,
                        method: Some("btree".into()),
                    }],
                    foreign_keys: Vec::new(),
                    checks: Vec::new(),
                    engine: Some("SQLite".into()),
                    collation: None,
                    comment: None,
                    ddl_source: rustdb_db_api::DdlSource::Native,
                    triggers: None,
                },
            ));
            let _ = ColumnMeta::new("x", LogicalType::Bool);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("table-panel").is_some(), "应渲染结构面板");
    assert!(
        vcx.debug_bounds("table-scrim").is_none(),
        "设计表面板不应有遮罩"
    );
    let close = vcx.debug_bounds("table-panel-close").expect("缺少关闭按钮");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).table_panel.is_none()),
        "关闭后应清掉面板"
    );
}

/// F-82：源码入口只对例程出现，且要求驱动开了 `routine_source_edit`。
#[gpui::test]
fn routine_source_entry_follows_capability(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let view_path = rustdb_db_api::ObjectPath::new(ns("main"), "v_orders", ObjectKind::View);
    let table_path = rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table);

    let mut entry_for = |caps_edit: bool, path: rustdb_db_api::ObjectPath| {
        vcx.update(|_, app| {
            view.update(app, |view, _| {
                let mut caps = rustdb_db_api::Capabilities::minimal();
                caps.routine_source_edit = caps_edit;
                view.capabilities = Some(caps);
                view.obj_menu = Some(ObjectMenu {
                    path,
                    at: gpui::point(px(10.), px(10.)),
                });
                view.object_menu_groups()
                    .iter()
                    .flatten()
                    .any(|entry| entry.key == "obj-source")
            })
        })
    };

    assert!(entry_for(true, view_path.clone()), "例程应给出源码入口");
    assert!(!entry_for(true, table_path), "普通表不该有源码入口");
    assert!(
        !entry_for(false, view_path),
        "驱动没开能力位时不该出现源码入口"
    );
}

/// F-82：对象种类 → 例程种类的映射（表 / 索引等不算例程）。
#[test]
fn routine_kind_maps_only_routine_objects() {
    assert_eq!(
        routine_kind_of(ObjectKind::View),
        Some(rustdb_core::RoutineKind::View)
    );
    assert_eq!(
        routine_kind_of(ObjectKind::MaterializedView),
        Some(rustdb_core::RoutineKind::MaterializedView)
    );
    assert_eq!(
        routine_kind_of(ObjectKind::Procedure),
        Some(rustdb_core::RoutineKind::Procedure)
    );
    assert_eq!(
        routine_kind_of(ObjectKind::Function),
        Some(rustdb_core::RoutineKind::Function)
    );
    assert_eq!(
        routine_kind_of(ObjectKind::Trigger),
        Some(rustdb_core::RoutineKind::Trigger)
    );
    assert_eq!(
        routine_kind_of(ObjectKind::Package),
        Some(rustdb_core::RoutineKind::Package)
    );
    assert_eq!(routine_kind_of(ObjectKind::Table), None);
    assert_eq!(routine_kind_of(ObjectKind::Index), None);
    assert_eq!(routine_kind_of(ObjectKind::Sequence), None);
    assert_eq!(routine_kind_of(ObjectKind::Type), None);
    assert_eq!(routine_kind_of(ObjectKind::Synonym), None);
}

/// F-82：源码面板能渲染；脏草稿关闭前要先确认。
#[gpui::test]
fn routine_panel_renders_and_guards_dirty_draft(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let path = rustdb_db_api::ObjectPath::new(ns("main"), "v_orders", ObjectKind::View);
    const ORIGINAL: &str = "CREATE VIEW v_orders AS SELECT 1;";
    const DRAFT: &str = "CREATE VIEW v_orders AS SELECT 2;";

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let editor = cx.new(|cx| EditorState::new(window, cx).language("sql"));
            let search = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("搜索源码")
            });
            view.routine = Some(RoutinePanel {
                path: path.clone(),
                kind: rustdb_core::RoutineKind::View,
                label: "main.v_orders".into(),
                original: ORIGINAL.into(),
                draft: ORIGINAL.into(),
                editor,
                search,
                failure: None,
                saving: None,
                readonly_reason: None,
                loading: false,
                deps: RoutineDeps::default(),
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("routine-panel").is_some(),
        "应渲染源码面板"
    );
    assert!(
        vcx.debug_bounds("routine-search-next").is_some(),
        "应有搜索入口"
    );
    assert!(vcx.debug_bounds("routine-save").is_some(), "应有保存按钮");

    // 改成脏草稿。
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let editor = view.routine.as_ref().expect("面板已打开").editor.clone();
            editor.update(cx, |state, cx| {
                state.set_value(DRAFT.to_string(), window, cx);
            });
            if let Some(panel) = view.routine.as_mut() {
                panel.draft = DRAFT.into();
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let close = vcx
        .debug_bounds("routine-panel-close")
        .expect("缺少关闭按钮");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("routine-discard").is_some(),
        "脏草稿关闭应先弹确认"
    );
    assert!(
        vcx.update(|_, app| view.read(app).routine.is_some()),
        "确认前不应关掉面板"
    );

    let discard = vcx
        .debug_bounds("routine-discard-confirm")
        .expect("缺少放弃按钮");
    vcx.simulate_click(discard.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).routine.is_none()),
        "确认后应关掉面板"
    );
}

/// F-83：依赖区展示加载 / 结果，并在「依赖 / 被依赖」间切换。
#[gpui::test]
fn routine_dependency_strip_and_tabs(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let path = rustdb_db_api::ObjectPath::new(ns("main"), "v_orders", ObjectKind::View);
    let table = rustdb_db_api::DependencyInfo {
        path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
        dependency_type: "普通".into(),
        remote: None,
    };
    let sibling = rustdb_db_api::DependencyInfo {
        path: rustdb_db_api::ObjectPath::new(ns("main"), "v_summary", ObjectKind::View),
        dependency_type: "自动".into(),
        remote: None,
    };

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let editor = cx.new(|cx| EditorState::new(window, cx).language("sql"));
            let search = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("搜索源码")
            });
            view.routine = Some(RoutinePanel {
                path: path.clone(),
                kind: rustdb_core::RoutineKind::View,
                label: "main.v_orders".into(),
                original: "CREATE VIEW v_orders AS SELECT 1;".into(),
                draft: String::new(),
                editor,
                search,
                failure: None,
                saving: None,
                readonly_reason: None,
                loading: false,
                deps: RoutineDeps {
                    tab: RoutineDepsTab::DependsOn,
                    loading: false,
                    error: None,
                    list: Some(rustdb_db_api::DependencyList {
                        depends_on: vec![table.clone()],
                        depended_by: vec![sibling.clone()],
                        truncated: false,
                        note: None,
                    }),
                },
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("routine-deps-tab-dep").is_some(),
        "应有依赖页签"
    );
    assert!(vcx.debug_bounds("routine-dep-0").is_some(), "应有依赖条目");

    let rev = vcx
        .debug_bounds("routine-deps-tab-rev")
        .expect("缺少被依赖页签");
    vcx.simulate_click(rev.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view.read(app).routine.as_ref().map(|p| p.deps.tab)),
        Some(RoutineDepsTab::DependedBy),
        "点击后应切到被依赖"
    );
    assert!(
        vcx.debug_bounds("routine-dep-0").is_some(),
        "被依赖列表也应渲染条目"
    );

    // 加载中只显示提示，不渲染旧条目。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            if let Some(panel) = view.routine.as_mut() {
                panel.deps.loading = true;
                panel.deps.list = None;
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("routine-deps-tab-dep").is_some());
    assert!(
        vcx.debug_bounds("routine-dep-0").is_none(),
        "加载中不应渲染条目"
    );
}

/// F-83：草稿未保存时，点依赖里的例程不应直接切走。
#[gpui::test]
fn routine_dependency_jump_guards_dirty_draft(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let path = rustdb_db_api::ObjectPath::new(ns("main"), "v_orders", ObjectKind::View);
    let target = rustdb_db_api::DependencyInfo {
        path: rustdb_db_api::ObjectPath::new(ns("main"), "fn_total", ObjectKind::Function),
        dependency_type: "普通".into(),
        remote: None,
    };

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let editor = cx.new(|cx| EditorState::new(window, cx).language("sql"));
            let search = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("搜索源码")
            });
            editor.update(cx, |state, cx| {
                state.set_value("CREATE VIEW v_orders AS SELECT 2;".to_string(), window, cx);
            });
            view.routine = Some(RoutinePanel {
                path: path.clone(),
                kind: rustdb_core::RoutineKind::View,
                label: "main.v_orders".into(),
                original: "CREATE VIEW v_orders AS SELECT 1;".into(),
                draft: "CREATE VIEW v_orders AS SELECT 2;".into(),
                editor,
                search,
                failure: None,
                saving: None,
                readonly_reason: None,
                loading: false,
                deps: RoutineDeps::default(),
            });
            view.routine_open_dependency(target.clone(), window, cx);
        });
    });
    vcx.run_until_parked();

    let (status, still_open) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.status.clone(),
            view.routine
                .as_ref()
                .is_some_and(|panel| panel.path.name.as_ref() == "v_orders"),
        )
    });
    assert!(status.contains("先保存"), "{status}");
    assert!(still_open, "守卫命中时不应换掉当前面板");
}

/// F-85：分区入口只对表出现，且要求驱动开了 `partition_admin`。
#[gpui::test]
fn partition_entry_follows_capability(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let table = rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table);
    let view_path = rustdb_db_api::ObjectPath::new(ns("main"), "v_orders", ObjectKind::View);

    let mut entry_for = |admin: bool, path: rustdb_db_api::ObjectPath| {
        vcx.update(|_, app| {
            view.update(app, |view, _| {
                let mut caps = rustdb_db_api::Capabilities::minimal();
                caps.partition_admin = admin;
                view.capabilities = Some(caps);
                view.obj_menu = Some(ObjectMenu {
                    path,
                    at: gpui::point(px(10.), px(10.)),
                });
                view.object_menu_groups()
                    .iter()
                    .flatten()
                    .any(|entry| entry.key == "obj-partitions")
            })
        })
    };

    assert!(entry_for(true, table.clone()), "表应给分区入口");
    assert!(!entry_for(true, view_path), "视图不该有分区入口");
    assert!(!entry_for(false, table), "驱动没开能力位时不该出现分区入口");
}

/// F-85：分区面板渲染列表与表单入口；预览浮层先于面板被 Esc 关闭。
#[gpui::test]
fn partition_panel_lists_partitions_and_previews_ddl(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let path = rustdb_db_api::ObjectPath::new(ns("shop"), "orders", ObjectKind::Table);

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let mut list = rustdb_db_api::PartitionList {
                strategy: Some("range".into()),
                key: Some("RANGE (created_at)".into()),
                ..rustdb_db_api::PartitionList::default()
            };
            list.push(rustdb_db_api::PartitionInfo {
                name: "orders_2026_01".into(),
                bounds: "FOR VALUES FROM ('2026-01-01') TO ('2026-02-01')".into(),
                row_estimate: Some(1200),
                is_default: false,
            });
            list.push(rustdb_db_api::PartitionInfo {
                name: "orders_default".into(),
                bounds: "DEFAULT".into(),
                row_estimate: None,
                is_default: true,
            });
            view.partition = Some(PartitionPanel {
                path: path.clone(),
                label: "shop.orders".into(),
                loading: false,
                error: None,
                list: Some(list),
                form: PartitionForm::None,
                name: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("分区名")
                }),
                range_from: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("起点")
                }),
                range_to: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("终点")
                }),
                list_values: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("列表值")
                }),
                modulus: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("MODULUS")
                }),
                remainder: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).placeholder("REMAINDER")
                }),
                is_default: false,
                detach_concurrently: false,
                preview: None,
                running: None,
                failure: None,
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("partition-panel").is_some(),
        "应渲染分区面板"
    );
    assert!(vcx.debug_bounds("partition-row-0").is_some(), "应列出分区");
    assert!(
        vcx.debug_bounds("partition-detach-0").is_some(),
        "每行应有分离入口"
    );
    assert!(vcx.debug_bounds("partition-create").is_some());
    assert!(vcx.debug_bounds("partition-attach").is_some());

    // 非分区表：不给新建 / 附加入口（R5：不假装支持）。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            if let Some(panel) = view.partition.as_mut() {
                panel.list = Some(rustdb_db_api::PartitionList::default());
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("partition-create").is_none(),
        "非分区表不该有新建入口"
    );
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            if let Some(panel) = view.partition.as_mut() {
                panel.list = Some(rustdb_db_api::PartitionList {
                    strategy: Some("range".into()),
                    key: Some("RANGE (created_at)".into()),
                    ..rustdb_db_api::PartitionList::default()
                });
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();

    // 打开新建表单：范围策略给起点 / 终点，并允许 DEFAULT。
    let create = vcx.debug_bounds("partition-create").expect("缺少新建按钮");
    vcx.simulate_click(create.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("partition-form-preview").is_some());
    assert!(vcx.debug_bounds("partition-default-toggle").is_some());

    // 注入一条预览 DDL（真实链路需要连接，规划本身由 core 单测覆盖）。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let plan = rustdb_core::plan_partition(
                DbKind::Postgres,
                &view.partition.as_ref().expect("面板已打开").path,
                &rustdb_core::PartitionAction::Create {
                    name: "orders_2026_02".into(),
                    bound: rustdb_core::PartitionBound::Range {
                        from: "'2026-02-01'".into(),
                        to: "'2026-03-01'".into(),
                    },
                },
            );
            if let Some(panel) = view.partition.as_mut() {
                panel.preview = Some(PartitionPreview { plan });
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("partition-preview").is_some(),
        "应出现预览"
    );
    assert!(vcx.debug_bounds("partition-preview-scrim").is_some());

    // Esc：先关预览，面板仍在；再按一次关面板。
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("partition-preview").is_none(),
        "Esc 应关掉预览"
    );
    assert!(
        vcx.debug_bounds("partition-panel").is_some(),
        "预览关闭后分区面板仍在"
    );
    assert!(
        vcx.debug_bounds("partition-form-preview").is_some(),
        "预览关闭后表单仍在"
    );

    // 第二次 Esc 收起表单，第三次才关面板。
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("partition-form-preview").is_none(),
        "Esc 应收起表单"
    );
    assert!(vcx.debug_bounds("partition-panel").is_some());

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).partition.is_none()),
        "再按 Esc 应关掉分区面板"
    );
}

/// F-90：导出页渲染勾选列表；导入预览按名称+主机+端口标冲突并跳过。
#[gpui::test]
fn exchange_panel_exports_and_imports_with_dedupe(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.panel = PanelKind::Connections;
            view.settings_open = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let entry = vcx
        .debug_bounds("settings-exchange")
        .expect("缺少导入/导出入口");
    vcx.simulate_click(entry.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("exchange-panel").is_some(),
        "应打开交换面板"
    );
    assert!(
        vcx.debug_bounds("exchange-export-0").is_some(),
        "导出页应列出连接"
    );
    assert!(vcx.debug_bounds("exchange-tunnels").is_some());
    assert!(vcx.debug_bounds("exchange-export").is_some());

    // 切到导入页。
    let import_tab = vcx
        .debug_bounds("exchange-tab-import")
        .expect("缺少导入页签");
    vcx.simulate_click(import_tab.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("exchange-pick").is_some());
    assert!(
        vcx.debug_bounds("exchange-import").is_some(),
        "导入页应有导入按钮（未选文件时禁用）"
    );

    // 注入一份预览：第一条与现有连接同名同主机同端口（冲突），第二条是新连接。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let demo = view.profiles[0].clone();
            let mut incoming = ConnectionProfile::demo_sqlite();
            incoming.id = "incoming".into();
            incoming.name = "交换库".into();
            incoming.folder = Some("导入".into());
            incoming.driver = DbKind::Postgres;
            incoming.host = Some("10.0.0.8".into());
            incoming.port = Some(5432);
            incoming.database = Some("shop".into());
            incoming.username = Some("reader".into());
            incoming.secret_ref = Some("never-copy".into());
            let preview = rustdb_core::ExchangePreview {
                profiles: vec![
                    rustdb_core::PreviewProfile {
                        profile: rustdb_core::ExchangeProfile::from_profile(&demo, false),
                        conflict: true,
                    },
                    rustdb_core::PreviewProfile {
                        profile: rustdb_core::ExchangeProfile::from_profile(&incoming, false),
                        conflict: false,
                    },
                ],
                encrypted: false,
                drafts: false,
                exported_at: None,
            };
            view.exchange_apply_preview(preview, cx);
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("exchange-import-0").is_some());
    assert!(vcx.debug_bounds("exchange-import-1").is_some());
    let selected = vcx.update(|_, app| {
        view.read(app)
            .exchange
            .as_ref()
            .map(|panel| panel.selected.clone())
            .unwrap_or_default()
    });
    assert_eq!(selected, vec![false, true], "冲突项默认不勾选");

    let before = vcx.update(|_, app| view.read(app).profiles.len());
    let import = vcx.debug_bounds("exchange-import").expect("缺少导入按钮");
    vcx.simulate_click(import.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).exchange.is_none()),
        "导入成功后应关闭面板"
    );
    let profiles = vcx.update(|_, app| view.read(app).profiles.clone());
    assert_eq!(profiles.len(), before + 1, "冲突项应跳过");
    let imported = profiles
        .iter()
        .find(|profile| profile.name == "交换库")
        .expect("应导入新连接");
    assert!(imported.secret_ref.is_none(), "密钥引用不得导入");
    assert!(imported.dev_secret.is_none(), "口令不得导入");
}

/// F-90：空白口令导出先要求二次确认；Esc 关闭面板。
#[gpui::test]
fn exchange_panel_guards_plaintext_export_and_escape(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.panel = PanelKind::Connections;
            view.settings_open = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let entry = vcx.debug_bounds("settings-exchange").expect("缺少入口");
    vcx.simulate_click(entry.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let export = vcx.debug_bounds("exchange-export").expect("缺少导出按钮");
    vcx.simulate_click(export.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let guarded = vcx.update(|_, app| {
        view.read(app)
            .exchange
            .as_ref()
            .map(|panel| panel.confirm_plain)
            .unwrap_or(false)
    });
    assert!(guarded, "空白口令的导出应先要求确认");

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).exchange.is_none()),
        "Esc 应关闭交换面板"
    );
}

/// 连接节点右键：出连接菜单（未连接时是「打开连接」）。
#[gpui::test]
fn connection_context_menu(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main")];
            view.session = Some(SessionId::new());
            view.active_profile = Some("demo-sqlite".to_string());
            view.current_ns = Some(ns("main"));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let conn = vcx.debug_bounds("conn-demo-sqlite").expect("缺少连接节点");
    vcx.simulate_mouse_down(
        conn.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("conn-scrim").is_some(), "应出现连接菜单");
    assert!(vcx.debug_bounds("menu-conn-new-query").is_some());
    assert!(vcx.debug_bounds("menu-conn-refresh").is_some());
    assert!(vcx.debug_bounds("menu-conn-copy").is_some());
    assert!(vcx.debug_bounds("menu-conn-disconnect").is_some());
}

/// F-89：生产标记常驻徽标；右键可临时解锁 / 回锁。
#[gpui::test]
fn production_badge_and_unlock_flow(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.session = Some(SessionId::new());
            view.active_profile = Some("demo-sqlite".to_string());
            view.namespaces = vec![ns("main")];
            view.current_ns = Some(ns("main"));
            view.core
                .set_profile_production("demo-sqlite", true)
                .expect("应能标记生产");
            view.profiles = view.core.profiles();
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("prod-badge-demo-sqlite").is_some(),
        "连接行应有生产徽标"
    );
    assert!(
        vcx.debug_bounds("status-production").is_some(),
        "状态栏应常驻生产提示"
    );

    let conn = vcx.debug_bounds("conn-demo-sqlite").expect("缺少连接节点");
    vcx.simulate_mouse_down(
        conn.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("menu-conn-prod-clear").is_some());
    let unlock = vcx
        .debug_bounds("menu-conn-unlock-1")
        .expect("缺少 1 分钟解锁");
    assert!(vcx.debug_bounds("menu-conn-unlock-5").is_some());
    assert!(
        vcx.debug_bounds("menu-conn-lock").is_none(),
        "未解锁时不应出现回锁"
    );

    vcx.simulate_click(unlock.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let unlocked = vcx.update(|_, app| {
        view.read(app)
            .core
            .write_unlock_remaining("demo-sqlite")
            .is_some()
    });
    assert!(unlocked, "点解锁后应进入解锁窗口");
    assert!(
        vcx.debug_bounds("unlock-badge-demo-sqlite").is_some(),
        "连接行应显示倒计时"
    );
    assert!(
        vcx.debug_bounds("status-unlock").is_some(),
        "状态栏应显示解锁剩余"
    );

    let conn = vcx.debug_bounds("conn-demo-sqlite").expect("缺少连接节点");
    vcx.simulate_mouse_down(
        conn.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("menu-conn-lock").is_some(),
        "解锁中应提供回锁"
    );
    assert!(vcx.debug_bounds("menu-conn-unlock-1").is_none());
    let lock = vcx.debug_bounds("menu-conn-lock").unwrap();
    vcx.simulate_click(lock.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let (locked, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.core.write_unlock_remaining("demo-sqlite").is_none(),
            view.status.clone(),
        )
    });
    assert!(locked, "回锁后剩余时间应为空");
    assert_eq!(status, "生产写操作已回锁");
    assert!(vcx.debug_bounds("unlock-badge-demo-sqlite").is_none());
    assert!(vcx.debug_bounds("status-unlock").is_none());

    let conn = vcx.debug_bounds("conn-demo-sqlite").expect("缺少连接节点");
    vcx.simulate_mouse_down(
        conn.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    let clear = vcx
        .debug_bounds("menu-conn-prod-clear")
        .expect("缺少取消生产标记");
    vcx.simulate_click(clear.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let production = vcx.update(|_, app| {
        view.read(app)
            .profiles
            .iter()
            .find(|p| p.id == "demo-sqlite")
            .map(|p| p.production)
            .unwrap_or(true)
    });
    assert!(!production, "应取消生产标记");
    assert!(vcx.debug_bounds("prod-badge-demo-sqlite").is_none());
    assert!(vcx.debug_bounds("status-production").is_none());
}

/// 点菜单项必须真的派发动作：清空 → 确认框；复制 → 剪贴板。
#[gpui::test]
fn menu_click_dispatches_actions(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main")];
            view.current_ns = Some(ns("main"));
            view.session = Some(SessionId::new());
            view.active_profile = Some("demo-sqlite".to_string());
            view.objects.insert(
                "main".to_string(),
                vec![ObjectSummary {
                    path: rustdb_db_api::ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
                    row_estimate: None,
                    comment: None,
                    has_primary_key: None,
                }],
            );
            view.expanded_ns.insert("main".to_string());
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let orders = vcx
        .debug_bounds("obj-main-orders")
        .expect("缺少 orders 节点");
    vcx.simulate_mouse_down(
        orders.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    // 复制名称 → 菜单关闭 + 状态栏提示（剪贴板在无头测试里不保证可用）。
    let copy = vcx.debug_bounds("menu-obj-copy").expect("缺少复制名称");
    vcx.simulate_click(copy.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let (closed, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (view.obj_menu.is_none(), view.status.clone())
    });
    assert!(closed, "点完应关闭菜单");
    assert_eq!(status, "已复制", "应走到复制动作");

    // 清空表 → 打开确认框。
    let orders = vcx
        .debug_bounds("obj-main-orders")
        .expect("缺少 orders 节点");
    vcx.simulate_mouse_down(
        orders.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    let truncate = vcx.debug_bounds("menu-obj-truncate").expect("缺少清空表");
    vcx.simulate_click(truncate.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| matches!(view.read(app).danger, Some(DangerAction::Truncate { .. }))),
        "清空表应打开确认框"
    );

    // 取消确认框，继续验证只读动作。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.danger = None;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    // 查看 DDL → 进入「正在读取」状态（说明 dispatch 走到了 show_ddl）。
    let orders = vcx
        .debug_bounds("obj-main-orders")
        .expect("缺少 orders 节点");
    vcx.simulate_mouse_down(
        orders.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    let ddl = vcx.debug_bounds("menu-obj-ddl").expect("缺少查看 DDL");
    vcx.simulate_click(ddl.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let status = vcx.update(|_, app| view.read(app).status.clone());
    assert!(
        status.contains("DDL"),
        "查看 DDL 应进入读取状态，实际：{status}"
    );
}
