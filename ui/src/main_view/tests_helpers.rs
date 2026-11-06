//! UI 测试共用夹具：视图构造、结果集填充与固定数据。
#![allow(private_interfaces)]

use super::*;

/// 折叠/展开相关的用例都把布局写到临时文件，别碰真实用户配置。
pub(crate) fn temp_layout(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rustdb-ui-layout-{name}-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}
pub(crate) fn reset_layout(holder: &Holder, vcx: &mut gpui::VisualTestContext, path: PathBuf) {
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |main, cx| {
            main.layout = rustdb_core::LayoutSettings::default();
            main.layout_path = path;
            cx.notify();
        });
    });
    vcx.run_until_parked();
}
pub(crate) fn click(vcx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("找不到 {selector}"));
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
}
pub(crate) type Holder = std::rc::Rc<std::cell::RefCell<Option<gpui::Entity<MainView>>>>;
pub(crate) fn test_view(cx: &mut gpui::TestAppContext) -> (&mut gpui::VisualTestContext, Holder) {
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::theme::apply_theme_mode(cx, rustdb_core::theme::ThemeMode::Dark);
    });
    let core = rustdb_core::Core::new(rustdb_db_api::DriverRegistry::new());
    let holder: Holder = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = holder.clone();
    // 每个测试自己的布局文件：既不读本机拖拽结果，也避免并行用例互相写。
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let layout_path = temp_layout(&format!("boot-{n}"));
    let (_, vcx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| MainView::new_for_test(window, cx, core, layout_path));
        *slot.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    (vcx, holder)
}
pub(crate) fn ns(catalog: &str) -> Namespace {
    Namespace::new(Some(catalog), None::<&str>)
}
/// 动态 selector 需要 `'static`（测试进程内泄漏一次即可）。
pub(crate) fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}
/// 造一个结果集塞进活动标签页（真实路径由驱动事件填充）。
pub(crate) fn push_result(view: &mut MainView, rows: usize, columns: usize) {
    let columns_meta: Vec<ColumnMeta> = (0..columns)
        .map(|index| {
            ColumnMeta::new(
                format!("c{index}"),
                rustdb_db_api::LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
            )
        })
        .collect();
    let data: Vec<Vec<Value>> = (0..rows)
        .map(|row| {
            (0..columns)
                .map(|col| Value::Int((row * 10 + col) as i64))
                .collect()
        })
        .collect();
    let mut tab = SqlTab::new(1, Some(ns("main")));
    tab.results.push(ResultView {
        id: ResultSetId(1),
        columns: columns_meta,
        rows: data,
        loaded: rows as u64,
        elapsed_ms: 12,
        truncated: false,
        affected: None,
        sort: None,
        widths: Vec::new(),
        filters: Vec::new(),
        editable: None,
        changes: None,
        page: None,
    });
    view.tabs = vec![tab];
    view.active_tab = 0;
}
/// 造一个可编辑结果集：主键是第 0 列（id）。
pub(crate) fn push_editable_result(view: &mut MainView) {
    let columns_meta = editable_columns();
    let mut tab = SqlTab::new(1, Some(ns("main")));
    tab.results.push(ResultView {
        id: ResultSetId(1),
        columns: columns_meta,
        rows: editable_rows(),
        loaded: 2,
        elapsed_ms: 5,
        truncated: false,
        affected: None,
        sort: None,
        widths: Vec::new(),
        filters: Vec::new(),
        editable: Some((table_path("main", "orders"), vec![0])),
        changes: None,
        page: None,
    });
    view.tabs = vec![tab];
    view.active_tab = 0;
}
pub(crate) fn table_path(ns_name: &str, table: &str) -> rustdb_db_api::ObjectPath {
    rustdb_db_api::ObjectPath::new(ns(ns_name), table, ObjectKind::Table)
}
pub(crate) fn editable_columns() -> Vec<ColumnMeta> {
    vec![
        ColumnMeta::new(
            "id",
            rustdb_db_api::LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
        ),
        ColumnMeta::new("name", rustdb_db_api::LogicalType::Text { max_len: None }),
    ]
}
pub(crate) fn editable_rows() -> Vec<Vec<Value>> {
    vec![
        vec![Value::Int(1), Value::Text("a".into())],
        vec![Value::Int(2), Value::Text("b".into())],
    ]
}
pub(crate) fn editable_result_view() -> ResultView {
    ResultView {
        id: ResultSetId(1),
        columns: editable_columns(),
        rows: editable_rows(),
        loaded: 2,
        elapsed_ms: 1,
        truncated: false,
        affected: None,
        sort: None,
        widths: Vec::new(),
        filters: Vec::new(),
        editable: Some((table_path("main", "orders"), vec![0])),
        changes: None,
        page: None,
    }
}
/// 造一个带设计器的视图（列草稿 = id / total）。
pub(crate) fn open_designer(
    cx: &mut gpui::TestAppContext,
) -> (&mut gpui::VisualTestContext, Holder) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            use rustdb_db_api::{ColumnInfo, LogicalType, TableDetail};
            let columns = vec![
                ColumnInfo::simple(
                    "id",
                    LogicalType::Int {
                        bits: 64,
                        unsigned: false,
                    },
                    1,
                ),
                ColumnInfo::simple("total", LogicalType::Float { bits: 64 }, 2),
            ];
            view.table_panel = Some((
                "main.orders".to_string(),
                TableDetail {
                    summary: ObjectSummary {
                        path: table_path("main", "orders"),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: Some(true),
                    },
                    columns: columns.clone(),
                    indexes: Vec::new(),
                    foreign_keys: Vec::new(),
                    checks: Vec::new(),
                    engine: None,
                    collation: None,
                    comment: None,
                    ddl_source: rustdb_db_api::DdlSource::Native,
                    triggers: None,
                },
            ));
            let mut draft = rustdb_core::DesignerDraft::default();
            draft.columns = columns
                .iter()
                .map(|column| {
                    rustdb_core::ColumnDraft::new(
                        column.meta.name.to_string(),
                        column.meta.native_type.clone(),
                    )
                })
                .collect();
            view.designer = Some(DesignerState {
                path: table_path("main", "orders"),
                before: columns.clone(),
                before_indexes: Vec::new(),
                before_foreign_keys: Vec::new(),
                before_checks: Vec::new(),
                triggers: None,
                draft,
                tab: rustdb_core::DesignerTab::Columns,
                issues: Vec::new(),
                failure: None,
                inputs: DesignerInputs::default(),
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();
    (vcx, holder)
}
/// 导入面板的测试夹具：目标表 channels(id, name)，源文件两行 CSV。
pub(crate) fn import_panel_fixture() -> ImportPanel {
    let path = ObjectPath::new(ns("main"), "channels", ObjectKind::Table);
    let id = ColumnInfo::simple(
        "id",
        rustdb_db_api::LogicalType::Int {
            bits: 64,
            unsigned: false,
        },
        1,
    );
    let name = ColumnInfo::simple(
        "name",
        rustdb_db_api::LogicalType::Text { max_len: None },
        2,
    );
    let mut options = ImportOptions::default();
    options.format = import::ImportFormat::Csv;
    options.columns = vec!["id".into(), "name".into()];
    let mut panel = ImportPanel {
        path,
        file_path: PathBuf::from("channels.csv"),
        bytes: b"id,name\r\n1,\xe6\xb8\xa0\xe9\x81\x93A\r\n2,\xe6\xb8\xa0\xe9\x81\x93B\r\n"
            .to_vec(),
        target: vec![id, name],
        table: "\"channels\"".into(),
        options,
        parsed: None,
        plan: None,
        mapping: Vec::new(),
        error: None,
        warning: None,
        confirm_truncate: false,
    };
    panel.rebuild(DbKind::Sqlite);
    panel
}
/// 造一个带可编辑结果集的视图，并返回视图实体。
pub(crate) fn editable_view(
    vcx: &mut &mut gpui::VisualTestContext,
    holder: &Holder,
) -> gpui::Entity<MainView> {
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_editable_result(view);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    view
}
/// 打开行操作菜单（返回菜单是否出现）。
pub(crate) fn open_row_menu(vcx: &mut &mut gpui::VisualTestContext) -> bool {
    let trigger = vcx.debug_bounds("edit-row-menu").expect("缺少行操作入口");
    vcx.simulate_click(trigger.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.debug_bounds("menu-row-append-1").is_some()
}
pub(crate) fn docs_input() -> DocInput {
    use rustdb_db_api::{DdlSource, TableDetail};
    let path = ObjectPath::new(ns("main"), "orders", ObjectKind::Table);
    let summary = ObjectSummary {
        path: path.clone(),
        row_estimate: Some(3),
        comment: Some("订单".into()),
        has_primary_key: Some(true),
    };
    let mut id = ColumnInfo::simple(
        "id",
        rustdb_db_api::LogicalType::Int {
            bits: 64,
            unsigned: false,
        },
        0,
    );
    id.meta.nullable = false;
    DocInput {
        database: "main".into(),
        tables: vec![rustdb_core::docs::DocTable {
            summary: summary.clone(),
            detail: Some(TableDetail {
                summary,
                columns: vec![id],
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
                checks: Vec::new(),
                triggers: None,
                engine: None,
                collation: None,
                comment: Some("订单".into()),
                ddl_source: DdlSource::Synthesized,
            }),
            error: None,
        }],
        skipped: 0,
    }
}
pub(crate) fn lineage_input() -> rustdb_core::LineageInput {
    let users = ObjectPath::new(ns("main"), "users", ObjectKind::Table);
    let orders = ObjectPath::new(ns("main"), "orders", ObjectKind::Table);
    let archive = ObjectPath::new(ns("main"), "archive", ObjectKind::Table);
    rustdb_core::LineageInput {
        database: "main".into(),
        tables: vec![
            rustdb_core::LineageTable {
                path: users,
                columns: vec!["id".into(), "email".into()],
                foreign_keys: Vec::new(),
            },
            rustdb_core::LineageTable {
                path: orders,
                columns: vec!["id".into(), "user_id".into()],
                foreign_keys: vec![ForeignKeyInfo {
                    name: "fk_orders_user".into(),
                    columns: vec!["user_id".into()],
                    referenced_table: "users".into(),
                    referenced_columns: vec!["id".into()],
                    on_delete: None,
                    on_update: None,
                }],
            },
            rustdb_core::LineageTable {
                path: archive,
                columns: vec!["id".into(), "user_id".into()],
                foreign_keys: Vec::new(),
            },
        ],
        ..Default::default()
    }
}
pub(crate) fn lineage_panel(
    input: rustdb_core::LineageInput,
    window: &mut Window,
    cx: &mut Context<MainView>,
) -> LineagePanel {
    let table = ObjectPath::new(ns("main"), "orders", ObjectKind::Table);
    LineagePanel {
        namespace: ns("main"),
        loading: false,
        columns: input
            .tables
            .iter()
            .find(|item| item.path.display_label() == table.display_label())
            .map(|item| item.columns.clone())
            .unwrap_or_default(),
        selected: Some("user_id".into()),
        filter: designer_input(window, cx, "筛选字段", None),
        min: rustdb_core::Confidence::Possible,
        report: Some(rustdb_core::lineage_report(&input, &table, "user_id")),
        input: Some(input),
        table,
        error: None,
    }
}
pub(crate) fn compare_snapshots() -> (rustdb_core::SchemaSnapshot, rustdb_core::SchemaSnapshot) {
    let column = |name: &str| {
        ColumnInfo::simple(name, rustdb_db_api::LogicalType::Text { max_len: None }, 0)
    };
    let source = rustdb_core::SchemaSnapshot {
        driver: DbKind::Sqlite,
        namespace: ns("main"),
        tables: vec![
            rustdb_core::TableSnapshot {
                name: "users".into(),
                columns: vec![column("id"), column("email")],
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
            },
            rustdb_core::TableSnapshot {
                name: "orders".into(),
                columns: vec![column("id")],
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
            },
        ],
        skipped: 0,
        failed: Vec::new(),
    };
    let target = rustdb_core::SchemaSnapshot {
        driver: DbKind::Sqlite,
        namespace: ns("main"),
        tables: vec![
            rustdb_core::TableSnapshot {
                name: "users".into(),
                columns: vec![column("id")],
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
            },
            rustdb_core::TableSnapshot {
                name: "legacy".into(),
                columns: vec![column("id")],
                indexes: Vec::new(),
                foreign_keys: Vec::new(),
            },
        ],
        skipped: 0,
        failed: Vec::new(),
    };
    (source, target)
}
pub(crate) fn compare_panel(window: &mut Window, cx: &mut Context<MainView>) -> ComparePanel {
    let (source, target) = compare_snapshots();
    let mut config = rustdb_core::CompareConfig::default();
    config.source = Some(source.namespace.clone());
    config.target = Some(target.namespace.clone());
    let report = rustdb_core::compare_schemas(&source, &target, &config);
    ComparePanel {
        session: SessionId("test".into()),
        profile: "local".into(),
        namespace: ns("main"),
        source: Some(source),
        target: Some(target),
        loading: [false, false],
        error: [None, None],
        config,
        report: Some(report),
        ignore_input: designer_input(window, cx, "忽略的表", None),
        rename_input: designer_input(window, cx, "重命名", None),
        drop_tables: false,
        drop_columns: false,
        show_rollback: false,
        confirm_execute: false,
        dirty: false,
        message: None,
    }
}
pub(crate) fn dump_panel() -> DumpPanel {
    let tables = vec!["channels".to_string(), "orders".to_string()];
    DumpPanel {
        session: SessionId("test".into()),
        profile: "local".into(),
        namespace: ns("main"),
        selected: tables.iter().cloned().collect(),
        tables,
        namespaces: vec![ns("main")],
        export_all: false,
        options: DumpOptions::default(),
        running: false,
        total: 0,
        done: 0,
        rows: 0,
        objects: 0,
        failed: 0,
        current: None,
        path: None,
        bytes: 0,
        error: None,
    }
}
pub(crate) fn transfer_panel_for_test() -> TransferPanel {
    let tables = vec!["channels".to_string(), "orders".to_string()];
    TransferPanel {
        session: SessionId("transfer-test".into()),
        profile: "local".into(),
        namespace: ns("main"),
        selected: tables.iter().cloned().collect(),
        tables,
        targets: vec![
            ("t1".to_string(), "目标一".to_string()),
            ("t2".to_string(), "目标二".to_string()),
        ],
        target: Some("t1".to_string()),
        options: TransferOptions::default(),
        previewing: false,
        preview_ready: false,
        preview_stale: false,
        lines: Vec::new(),
        warnings: Vec::new(),
        running: false,
        total: 0,
        done: 0,
        rows: 0,
        table_rows: 0,
        failed: 0,
        failures: Vec::new(),
        current: None,
        backup: None,
        elapsed_ms: 0,
        error: None,
    }
}
pub(crate) fn transfer_envelope(event: TransferEvent) -> rustdb_core::EventEnvelope {
    rustdb_core::EventEnvelope {
        seq: 1,
        session_seq: Some(1),
        ts_ms: 0,
        session_id: None,
        event: AppEvent::Transfer(event),
    }
}
pub(crate) fn data_compare_panel_for_test(
    window: &mut Window,
    cx: &mut Context<MainView>,
) -> DataComparePanel {
    let tables = vec!["channels".to_string(), "orders".to_string()];
    DataComparePanel {
        session: SessionId("data-compare-test".into()),
        profile_id: "local".into(),
        profile: "local".into(),
        namespace: ns("main"),
        selected: tables.iter().cloned().collect(),
        tables,
        targets: vec![
            ("t1".to_string(), "目标一".to_string()),
            ("t2".to_string(), "目标二".to_string()),
        ],
        target: Some("t1".to_string()),
        keys: designer_input(window, cx, "关键字段", None),
        options: DataCompareOptions::default(),
        previewing: false,
        preview_ready: false,
        preview_stale: false,
        lines: Vec::new(),
        warnings: Vec::new(),
        insert: 0,
        update: 0,
        delete: 0,
        running: false,
        applying: false,
        total: 0,
        done: 0,
        failed: 0,
        failures: Vec::new(),
        current: None,
        applied: 0,
        statements: 0,
        elapsed_ms: 0,
        error: None,
        config_note: None,
    }
}
pub(crate) fn data_compare_envelope(event: DataCompareEvent) -> rustdb_core::EventEnvelope {
    rustdb_core::EventEnvelope {
        seq: 1,
        session_seq: Some(1),
        ts_ms: 0,
        session_id: None,
        event: AppEvent::DataCompare(event),
    }
}
pub(crate) fn ai_test_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rustdb-ai-{tag}-{}-{}.json",
        std::process::id(),
        QueryId::new()
    ))
}
