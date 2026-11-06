//! 结果网格、单元格编辑、设计器与列操作测试。

use super::tests_helpers::*;
use super::*;

/// 类型感知校验：整数列填字母必须被拒。
#[test]
fn parse_cell_text_is_type_aware() {
    use rustdb_db_api::LogicalType;
    let int_type = LogicalType::Int {
        bits: 64,
        unsigned: false,
    };
    assert_eq!(parse_cell_text("42", &int_type).unwrap(), Value::Int(42));
    assert!(parse_cell_text("abc", &int_type).is_err());
    assert_eq!(
        parse_cell_text("true", &LogicalType::Bool).unwrap(),
        Value::Bool(true)
    );
    assert!(parse_cell_text("maybe", &LogicalType::Bool).is_err());
    assert!(parse_cell_text("2026-10-21", &LogicalType::Date).is_ok());
    assert!(parse_cell_text("2026-13-40", &LogicalType::Date).is_err());
    // 文本列原样保留（含空串，与 NULL 不同）。
    assert_eq!(
        parse_cell_text("", &LogicalType::Text { max_len: None }).unwrap(),
        Value::Text("".into())
    );
}

/// 单元格编辑 → 变更集 → DML（只 UPDATE 改动列，主键定位）。
#[test]
fn cell_edit_builds_update_statement() {
    let mut rv = editable_result_view();
    let count = apply_cell_edit(&mut rv, 0, 1, Some("z".into())).expect("编辑应成功");
    assert_eq!(count, 1);
    assert_eq!(rv.rows[0][1], Value::Text("z".into()), "网格应显示新值");
    let plan = rv
        .changes
        .as_ref()
        .unwrap()
        .plan(rustdb_db_api::DbKind::Sqlite)
        .unwrap();
    assert_eq!(plan.len(), 1);
    assert!(plan[0].sql.starts_with("UPDATE"), "{}", plan[0].sql);
    assert!(plan[0].sql.contains("\"name\" = 'z'"), "{}", plan[0].sql);
    assert!(plan[0].sql.contains("WHERE \"id\" = 1"), "{}", plan[0].sql);
    assert!(plan[0].expects_row, "UPDATE 必须校验影响行数");
}

/// NULL 与空串严格区分。
#[test]
fn null_edit_is_distinct_from_empty_string() {
    let mut rv = editable_result_view();
    apply_cell_edit(&mut rv, 0, 1, None).expect("设为 NULL");
    assert!(rv.rows[0][1].is_null());
    let plan = rv
        .changes
        .as_ref()
        .unwrap()
        .plan(rustdb_db_api::DbKind::Sqlite)
        .unwrap();
    assert!(plan[0].sql.contains("= NULL"), "{}", plan[0].sql);
}

/// 多格选区取子矩阵：列/行按选区裁剪，主键下标重映射；漏主键要显式标记。
#[test]
fn selection_submatrix_remaps_keys_and_reports_missing() {
    let mut tab = SqlTab::new(1, Some(ns("main")));
    tab.results.push(editable_result_view());
    tab.active_result = 0;

    let all_cells = Some(GridSelection {
        result: ResultSetId(1),
        anchor: (0, 0),
        focus: (1, 1),
    });
    let payload = tab_selection_typed(&tab, all_cells).expect("多格选区应有载荷");
    assert_eq!(payload.columns, vec!["id", "name"]);
    assert_eq!(payload.rows, editable_rows());
    assert_eq!(payload.key_columns, vec![0]);
    assert!(!payload.missing_key_columns);
    assert_eq!(payload.dimensions, (2, 2));

    // 只选 name 列：主键 id 不在选区内，SQL UPDATE 复制必须被拦下。
    let second_column = Some(GridSelection {
        result: ResultSetId(1),
        anchor: (0, 1),
        focus: (1, 1),
    });
    let payload = tab_selection_typed(&tab, second_column).expect("单列多行也是选区");
    assert_eq!(payload.columns, vec!["name"]);
    assert_eq!(
        payload.rows,
        vec![vec![Value::Text("a".into())], vec![Value::Text("b".into())]]
    );
    assert!(payload.key_columns.is_empty());
    assert!(payload.missing_key_columns, "缺少主键列时要报告");

    let single = Some(GridSelection {
        result: ResultSetId(1),
        anchor: (0, 0),
        focus: (0, 0),
    });
    assert!(
        tab_selection_typed(&tab, single).is_none(),
        "单格不进入子矩阵复制"
    );
    assert!(
        tab_selection_typed(&tab, None).is_none(),
        "无选区时回落到整结果"
    );
}

/// 选区行序跟随当前排序；主键列仍按子集列重映射。
#[test]
fn selection_follows_current_sort_order() {
    let mut tab = SqlTab::new(1, Some(ns("main")));
    tab.results.push(editable_result_view());
    tab.active_result = 0;
    tab.results[0].sort = Some(SortState {
        column: 0,
        ascending: false,
    });

    let selection = Some(GridSelection {
        result: ResultSetId(1),
        anchor: (0, 0),
        focus: (1, 1),
    });
    let payload = tab_selection_typed(&tab, selection).expect("应有载荷");
    assert_eq!(
        payload.rows,
        vec![
            vec![Value::Int(2), Value::Text("b".into())],
            vec![Value::Int(1), Value::Text("a".into())]
        ],
        "降序后选区应复制 2, 1"
    );
    assert_eq!(payload.key_columns, vec![0]);
    assert!(!payload.missing_key_columns);
}

/// F-63：预览入口在未连接时给出明确提示。
#[gpui::test]
fn preview_button_prompts_connection(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");

    let preview = vcx.debug_bounds("preview-dml").expect("缺少预览按钮");
    vcx.simulate_click(preview.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let status = vcx.update(|_, app| view.read(app).status.clone());
    assert_eq!(status, "请先在「连接」面板连接一个数据源");
}

/// 预览提示条渲染在结果区顶部，下一次执行前一直可见。
#[gpui::test]
fn preview_note_renders_above_results(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 2, 2);
            view.preview_note = Some("预览 · 只读 · orders · 预览值可能不同".into());
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("preview-note").is_some(),
        "预览提示条应渲染"
    );
}

/// Shift + 点击扩展为矩形选区；普通单击仍复制单格。
#[gpui::test]
fn shift_click_extends_grid_selection(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 4, 3);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let first = vcx.debug_bounds("cell-0-0").expect("缺少首格");
    vcx.simulate_click(first.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let second = vcx.debug_bounds("cell-2-1").expect("缺少目标格");
    vcx.simulate_click(
        second.center(),
        gpui::Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    vcx.run_until_parked();

    let (selection, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.grid_selection
                .map(|selection| (selection.anchor, selection.focus)),
            view.status.clone(),
        )
    });
    assert_eq!(selection, Some(((0, 0), (2, 1))), "Shift 点击应扩成矩形");
    assert_eq!(status, "已复制", "普通单击仍保留单格复制");
    assert!(
        vcx.debug_bounds("copy-selection").is_some(),
        "多格选区应出现「复制选区」入口"
    );

    // 排序会改变显示顺序，选区必须失效而不是错位。
    vcx.update(|window, app| {
        view.update(app, |view, cx| view.toggle_sort(0, window, cx));
    });
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).grid_selection.is_none()),
        "排序后选区应清空"
    );
}

/// F-80：详情面板的 JSON 格式化/压缩、路径定位与二进制十六进制/文本切换。
#[gpui::test]
fn cell_detail_modes_and_json_pointer(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            let mut tab = SqlTab::new(1, Some(ns("main")));
            tab.results.push(ResultView {
                id: ResultSetId(1),
                columns: vec![
                    ColumnMeta::new("payload", rustdb_db_api::LogicalType::Json),
                    ColumnMeta::new("blob", rustdb_db_api::LogicalType::Bytes),
                    ColumnMeta::new("at", rustdb_db_api::LogicalType::TimestampTz),
                ],
                rows: vec![vec![
                    Value::Json(Arc::from(r#"{"a":[1,2],"b":{"c":"x"}}"#)),
                    Value::Bytes(Arc::from(&b"hello"[..])),
                    Value::TimestampTz(
                        chrono::DateTime::parse_from_rfc3339("2026-10-23T08:00:00Z")
                            .unwrap()
                            .with_timezone(&chrono::Utc),
                    ),
                ]],
                loaded: 1,
                elapsed_ms: 3,
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
            view.selected_cell = Some((0, 0));
            view.open_selected_cell_detail(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("cell-detail").is_some(), "详情面板应打开");
    assert!(vcx.debug_bounds("cell-detail-mode-pretty").is_some());
    assert!(vcx.debug_bounds("cell-detail-mode-compact").is_some());
    assert!(vcx.debug_bounds("cell-detail-pointer").is_some());
    assert!(vcx.debug_bounds("cell-detail-body").is_some());

    let compact = vcx
        .debug_bounds("cell-detail-mode-compact")
        .expect("缺少压缩按钮");
    vcx.simulate_click(compact.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view.read(app).cell_detail.as_ref().map(|d| d.mode)),
        Some(crate::detail::DetailMode::JsonCompact),
        "点压缩应切换模式"
    );

    // 路径输入回车即生效（订阅 Change 事件；测试里直接写状态）。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            if let Some(detail) = view.cell_detail.as_mut() {
                detail.pointer = "/a/0".into();
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view
            .read(app)
            .cell_detail
            .as_ref()
            .map(|detail| detail.pointer.clone())),
        Some("/a/0".to_string())
    );

    // 二进制：十六进制 / 文本两种模式。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 1));
            view.open_selected_cell_detail(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("cell-detail-mode-hex").is_some());
    let text = vcx
        .debug_bounds("cell-detail-mode-text")
        .expect("缺少文本模式");
    vcx.simulate_click(text.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view.read(app).cell_detail.as_ref().map(|d| d.mode)),
        Some(crate::detail::DetailMode::Text)
    );

    // 时间戳没有切换按钮，但详情仍可打开（时区说明在正文上方）。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.selected_cell = Some((0, 2));
            view.open_selected_cell_detail(cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("cell-detail-mode-hex").is_none());
    assert!(vcx.debug_bounds("cell-detail-body").is_some());

    let close = vcx.debug_bounds("cell-detail-close").expect("缺少关闭按钮");
    vcx.simulate_click(close.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("cell-detail").is_none());
}

/// 按住左键拖过单元格 → 矩形选区，松手后不触发单格复制。
#[gpui::test]
fn drag_extends_grid_selection(cx: &mut gpui::TestAppContext) {
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

    let start = vcx.debug_bounds("cell-0-0").expect("缺少起点").center();
    let end = vcx.debug_bounds("cell-3-2").expect("缺少终点").center();
    vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.simulate_mouse_move(
        end,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    vcx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();

    let (selection, dragged) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.grid_selection
                .map(|selection| (selection.anchor, selection.focus)),
            view.drag_moved,
        )
    });
    assert_eq!(selection, Some(((0, 0), (3, 2))), "拖拽应扩成矩形选区");
    assert!(dragged, "拖拽过程要记录已扩选");
    assert!(
        vcx.debug_bounds("copy-selection").is_some(),
        "拖拽后应出现「复制选区」入口"
    );
}

/// 不可编辑结果集：拒绝编辑并给出原因。
#[test]
fn uneditable_result_rejects_edit() {
    let mut rv = editable_result_view();
    rv.editable = None;
    let err = apply_cell_edit(&mut rv, 0, 1, Some("z".into())).unwrap_err();
    assert!(err.contains("不可编辑"), "{err}");
}

/// 无主键表：只能插入，不能改/删。
#[test]
fn table_without_pk_can_only_insert() {
    let mut rv = editable_result_view();
    if let Some((_, key)) = rv.editable.as_mut() {
        key.clear();
    }
    let err = apply_cell_edit(&mut rv, 0, 1, Some("z".into())).unwrap_err();
    assert!(err.contains("主键"), "{err}");
    assert!(insert_empty_row(&mut rv).is_ok(), "无主键仍可新增");
    assert_eq!(rv.rows.len(), 3);
}

/// 双击可编辑单元格 → 出现编辑条；取消后收起。
#[gpui::test]
fn double_click_opens_edit_bar(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_editable_result(view);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let cell = vcx.debug_bounds("cell-0-1").expect("缺少单元格");
    vcx.simulate_event(gpui::MouseDownEvent {
        position: cell.center(),
        button: gpui::MouseButton::Left,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        position: cell.center(),
        button: gpui::MouseButton::Left,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("edit-bar").is_some(), "双击应打开编辑条");
    let cancel = vcx.debug_bounds("edit-cancel").expect("缺少取消按钮");
    vcx.simulate_click(cancel.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("edit-bar").is_none(), "取消后编辑条应消失");
}

/// 表设计器：列可删、可加；改动只进草稿，不直接执行。
#[gpui::test]
fn table_designer_edits_drafts(cx: &mut gpui::TestAppContext) {
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
            view.design_pending = Some(table_path("main", "orders"));
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

    assert!(
        vcx.debug_bounds("designer-drop-0").is_some(),
        "应有删列按钮"
    );
    let drop = vcx.debug_bounds("designer-drop-0").expect("缺少删列按钮");
    vcx.simulate_click(drop.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    vcx.update(|_, app| {
        let designer = view.read(app).designer.as_ref().expect("设计器");
        assert_eq!(designer.draft.columns.len(), 1, "删列只改草稿");
        assert_eq!(designer.draft.columns[0].name, "total");
        assert_eq!(designer.before.len(), 2, "现状列仍保留");
    });
}

/// 设计器：六个页签可切换，各页内容跟着换。
#[gpui::test]
fn table_designer_switches_tabs(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = open_designer(cx);
    let view = holder.borrow().clone().expect("view 未创建");
    // 页脚两个按钮必须完整落在卡片内（此前竖向排版把按钮挤出了面板）。
    let panel = vcx.debug_bounds("table-panel").expect("缺少面板");
    let preview_btn = vcx.debug_bounds("designer-preview").expect("缺少预览按钮");
    let apply_btn = vcx.debug_bounds("designer-apply").expect("缺少执行按钮");
    assert!(
        preview_btn.bottom() <= panel.bottom() && apply_btn.bottom() <= panel.bottom(),
        "设计器页脚按钮不应溢出面板：panel={panel:?} preview={preview_btn:?} apply={apply_btn:?}"
    );
    for (selector, marker) in [
        ("designer-tab-列", "designer-add"),
        ("designer-tab-索引", "designer-index-add"),
        ("designer-tab-外键", "designer-fk-add"),
        ("designer-tab-检查约束", "designer-check-add"),
        ("designer-tab-选项", "designer-option-apply"),
    ] {
        let bounds = vcx.debug_bounds(selector).expect("缺少页签");
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds(marker).is_some(),
            "{selector} 对应的内容（{marker}）没渲染出来"
        );
    }
    // 触发器页只有只读列表（没有触发器元数据时给一句话）。
    let bounds = vcx
        .debug_bounds("designer-tab-触发器")
        .expect("缺少触发器页签");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, app| {
        let designer = view.read(app).designer.as_ref().expect("设计器");
        assert_eq!(designer.tab, rustdb_core::DesignerTab::Triggers);
    });
}

/// 设计器：拖拽排序真的换了列顺序（现状列不动）。
#[gpui::test]
fn table_designer_reorders_columns(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = open_designer(cx);
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.designer_move_column(0, 1, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, app| {
        let designer = view.read(app).designer.as_ref().expect("设计器");
        assert_eq!(designer.draft.columns[0].name, "total");
        assert_eq!(designer.draft.columns[1].name, "id");
        assert_eq!(designer.before.len(), 2, "现状列不受拖拽影响");
    });
}

/// SQLite 改结构要重建表；其它方言不需要。
#[test]
fn designer_rebuild_only_for_sqlite() {
    let columns = vec![rustdb_db_api::ColumnInfo::simple(
        "id",
        rustdb_db_api::LogicalType::Int {
            bits: 64,
            unsigned: false,
        },
        1,
    )];
    let mut designer = DesignerState {
        path: table_path("main", "orders"),
        before: columns.clone(),
        before_indexes: Vec::new(),
        before_foreign_keys: Vec::new(),
        before_checks: Vec::new(),
        triggers: None,
        draft: rustdb_core::DesignerDraft::default(),
        tab: rustdb_core::DesignerTab::Columns,
        issues: Vec::new(),
        failure: None,
        inputs: DesignerInputs::default(),
    };
    designer.draft.columns = vec![rustdb_core::ColumnDraft::new("id", "TEXT")];
    assert!(designer_needs_rebuild(DbKind::Sqlite, &designer));
    assert!(!designer_needs_rebuild(DbKind::Postgres, &designer));
    // 只是加索引不动列，不需要重建。
    designer.draft.columns = vec![rustdb_core::ColumnDraft::new(
        "id",
        columns[0].meta.native_type.clone(),
    )];
    designer.draft.indexes = vec![rustdb_core::IndexDraft::new("idx_id", vec!["id".into()])];
    assert!(!designer_needs_rebuild(DbKind::Sqlite, &designer));
}

/// AI 面板：离线脚本 provider 跑完一轮，回答进对话记录。
#[gpui::test]
fn ai_panel_runs_offline_round(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.session = Some(SessionId::new());
            if view.ai_input.is_none() {
                view.ai_input =
                    Some(cx.new(|cx| gpui_component::input::InputState::new(window, cx)));
            }
            let input = view.ai_input.clone().expect("输入框");
            input.update(cx, |state, cx| {
                state.set_value("看看有哪些表", window, cx);
            });
            view.ai_send(window, cx);
            cx.notify();
        });
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        vcx.update(|_, app| {
            view.update(app, |view, cx| {
                view.poll(cx);
            });
        });
        vcx.run_until_parked();
        let done = vcx.update(|_, app| {
            let state = view.read(app);
            // 文本可能在 Delta 就到齐，`ai_running` 要等 Completed 才复位——
            // 两个条件都满足才算结束，否则会随机在下面「Run 结束后应复位」翻车。
            !state.ai_running
                && state
                    .ai_lines
                    .iter()
                    .any(|(role, text)| role == "assistant" && text.contains("离线演示"))
        });
        if done || std::time::Instant::now() > deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let (lines, pending, running) = vcx.update(|_, app| {
        let state = view.read(app);
        (
            state.ai_lines.clone(),
            state.ai_pending.clone(),
            state.ai_running,
        )
    });
    assert!(
        lines
            .iter()
            .any(|(role, text)| role == "tool" && text.contains("search_objects")),
        "应记录工具调用：{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|(role, text)| role == "assistant" && text.contains("离线演示")),
        "应收到回答：{lines:?}"
    );
    assert!(pending.is_none(), "只读工具不该弹确认卡");
    assert!(!running, "Run 结束后应复位");
}

/// 待确认时渲染确认卡，拒绝可点。
#[gpui::test]
fn ai_confirmation_card_is_rendered(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.layout.ai_open = true;
            view.ai_pending = Some((
                "run-1".to_string(),
                "call-1".to_string(),
                "run_write_query".to_string(),
                "UPDATE orders SET total = 0".to_string(),
            ));
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("ai-confirm").is_some(),
        "待确认时应显示确认卡"
    );
    assert!(vcx.debug_bounds("ai-approve").is_some());
    assert!(vcx.debug_bounds("ai-reject").is_some());

    let reject = vcx.debug_bounds("ai-reject").expect("缺少拒绝按钮");
    vcx.simulate_click(reject.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.update(|_, app| view.read(app).ai_pending.is_none()),
        "拒绝后应清掉待确认状态"
    );
}

/// 列筛选：大小写不敏感、多列取交集、空条件等于不过滤。
#[test]
fn filters_narrow_rows_and_combine() {
    let rows = vec![
        vec![Value::Text("paid".into()), Value::Int(3)],
        vec![Value::Text("refunded".into()), Value::Int(1)],
        vec![Value::Text("paid".into()), Value::Int(9)],
    ];

    // 单列
    let paid = ordered_indices(&rows, None, &[Some("PAID".into()), None]);
    assert_eq!(paid, vec![0, 2], "大小写不敏感");

    // 两列取交集
    let both = ordered_indices(&rows, None, &[Some("paid".into()), Some("9".into())]);
    assert_eq!(both, vec![2]);

    // 空白条件不算筛选
    let all = ordered_indices(&rows, None, &[Some("  ".into()), None]);
    assert_eq!(all.len(), 3);

    // 无匹配
    assert!(ordered_indices(&rows, None, &[Some("zzz".into())]).is_empty());
}

/// 筛选只作用于已加载行，切换结果集 / 重新查询后由调用方决定是否保留。
#[gpui::test]
fn filter_menu_filters_grid(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 4, 2);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let funnel = vcx.debug_bounds("col-filter-0").expect("缺少筛选按钮");
    vcx.simulate_click(funnel.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("filter-menu").is_some(), "应打开筛选面板");

    // 直接写条件（输入法路径在测试里不稳定）。
    vcx.update(|window, app| {
        let input = view.read(app).filter_input.clone().expect("筛选输入框");
        input.update(app, |state, cx| state.set_value("1", window, cx));
        view.update(app, |view, cx| {
            if let Some(tab) = view.tabs.get_mut(view.active_tab) {
                if let Some(result) = tab.results.get_mut(tab.active_result) {
                    result.set_filter(0, Some("1".into()));
                }
            }
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let (visible, len) = vcx.update(|_, app| {
        let result = view.read(app).active_result().expect("缺少结果");
        (
            result.visible_row_count(),
            result.ordered_row_indices().len(),
        )
    });
    assert_eq!(visible, len);
    assert!(visible < 4, "筛选后行数应减少，实际 {visible}");

    // 清除
    let clear = vcx.debug_bounds("filter-clear").expect("缺少清除按钮");
    vcx.simulate_click(clear.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let visible = vcx.update(|_, app| {
        view.read(app)
            .active_result()
            .map(|result| result.visible_row_count())
            .unwrap_or_default()
    });
    assert_eq!(visible, 4, "清除后应恢复全部行");
}

/// 列宽夹取与最小总宽：未拖过的列按 `MIN_COL_W` 计入，拖过的按定宽。
#[test]
fn column_widths_clamp_and_sum() {
    let mut view = ResultView {
        id: ResultSetId(1),
        columns: vec![
            ColumnMeta::new(
                "a",
                rustdb_db_api::LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
            ),
            ColumnMeta::new(
                "b",
                rustdb_db_api::LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
            ),
        ],
        rows: Vec::new(),
        loaded: 0,
        elapsed_ms: 0,
        truncated: false,
        affected: None,
        sort: None,
        widths: Vec::new(),
        filters: Vec::new(),
        editable: None,
        changes: None,
        page: None,
    };
    assert_eq!(view.fixed_width(0), None, "未拖过的列交给 flex");
    assert_eq!(view.min_total_width(), 144., "两列按下限 72 计入");
    assert!(view.set_width(0, 500.));
    assert_eq!(view.fixed_width(0), Some(500.));
    assert_eq!(view.min_total_width(), 572., "500 + 72");

    view.set_width(0, 1.);
    assert_eq!(view.fixed_width(0), Some(MIN_COL_W), "低于下限按 72");
    view.set_width(0, 9999.);
    assert_eq!(view.fixed_width(0), Some(MAX_COL_W), "高于上限按 640");
    assert!(!view.set_width(9, 300.), "越界列不改");
}

/// 拖表头把手改列宽：第二列表头随之后移，宽度写入结果集。
#[gpui::test]
fn column_resize_follows_drag(cx: &mut gpui::TestAppContext) {
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

    let handle = vcx.debug_bounds("col-handle-0").expect("缺少列宽把手");
    let next_before = vcx.debug_bounds("header-1").expect("缺少第二列表头");
    let start = handle.center();
    let to = start + gpui::point(px(48.), px(0.));

    // 起点宽度必须在按下之前取：拖完首列就有定宽了。
    let start_w = vcx.update(|_, app| {
        let view = view.read(app);
        let result = view.active_result().expect("缺少结果集");
        view.col_display_width(result, 0)
    });

    vcx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.simulate_mouse_move(
        to,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    vcx.simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::default());
    vcx.run_until_parked();

    let next_after = vcx.debug_bounds("header-1").expect("缺少第二列表头");
    let shift = f32::from(next_after.origin.x) - f32::from(next_before.origin.x);
    assert!((shift - 48.).abs() < 1.5, "第二列应右移 48px，实为 {shift}");

    let width = vcx.update(|_, app| {
        view.read(app)
            .active_result()
            .and_then(|result| result.fixed_width(0))
            .unwrap_or_default()
    });
    assert!(
        (width - (start_w + 48.)).abs() < 1.5,
        "列宽应在拖拽前宽度上 +48，实为 {width}（起点 {start_w}）"
    );
    assert!(
        vcx.update(|_, app| view.read(app).col_resize.is_none()),
        "松手后应结束拖拽"
    );
}

/// 结果网格只渲染视口附近的行：300 行时末行不在帧里，滚到底才出现。
#[gpui::test]
fn grid_virtualizes_rows(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            push_result(view, 300, 3);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("cell-0-0").is_some(), "首行应在视口内");
    assert!(
        vcx.debug_bounds("cell-299-0").is_none(),
        "屏幕外的行不应进入元素树"
    );

    let area = vcx.debug_bounds("result-scroll").expect("缺少结果滚动区");
    vcx.simulate_event(gpui::ScrollWheelEvent {
        position: area.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-9000.))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("cell-299-0").is_some(),
        "滚到底后末行应渲染出来"
    );
    assert!(
        vcx.debug_bounds("cell-0-0").is_none(),
        "首行滚出视口后应被回收"
    );
}

/// 右键表节点弹出对象菜单；表有「打开数据」，存储过程没有。
#[gpui::test]
fn object_context_menu_lists_actions(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.namespaces = vec![ns("main")];
            view.current_ns = Some(ns("main"));
            // 树只在「已连接」时展开库/对象层级，这里伪装一个活动会话。
            view.session = Some(SessionId::new());
            view.active_profile = Some("demo-sqlite".to_string());
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            view.objects.insert(
                "main".to_string(),
                vec![
                    ObjectSummary {
                        path: rustdb_db_api::ObjectPath::new(
                            ns("main"),
                            "orders",
                            ObjectKind::Table,
                        ),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: None,
                    },
                    ObjectSummary {
                        path: rustdb_db_api::ObjectPath::new(
                            ns("main"),
                            "p_touch",
                            ObjectKind::Procedure,
                        ),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: None,
                    },
                ],
            );
            view.expanded_ns.insert("main".to_string());
            // 分组默认只展开「表」，存储过程组要显式打开。
            view.group_state.insert(
                object_group_key("demo-sqlite", "main", ObjectKind::Procedure),
                true,
            );
            cx.notify();
        });
    });
    vcx.run_until_parked();

    // 表 → 查询 / DDL / 统计 / 复制 / 导入 / 清空 / 删除。
    let orders = vcx
        .debug_bounds("obj-main-orders")
        .expect("缺少 orders 节点");
    vcx.simulate_mouse_down(
        orders.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("obj-scrim").is_some(), "应出现关闭遮罩");
    for action in [
        "menu-obj-open",
        "menu-obj-ddl",
        "menu-obj-count",
        "menu-obj-copy",
        "menu-obj-copy-qualified",
        "menu-obj-import",
        "menu-obj-truncate",
        "menu-obj-drop",
    ] {
        assert!(vcx.debug_bounds(action).is_some(), "表菜单缺少 {action}");
    }

    // 存储过程 → 看定义 / 复制 / 删除，没有查询与清空。
    let procedure = vcx
        .debug_bounds("obj-main-p_touch")
        .expect("缺少 p_touch 节点");
    vcx.simulate_mouse_down(
        procedure.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("menu-obj-open").is_none(),
        "存储过程不应有「查询前 100 行」"
    );
    assert!(
        vcx.debug_bounds("menu-obj-truncate").is_none(),
        "存储过程不应有「清空数据」"
    );
    assert!(
        vcx.debug_bounds("menu-obj-import").is_none(),
        "存储过程不应有「导入数据」"
    );
    assert!(
        vcx.debug_bounds("menu-obj-ddl").is_some(),
        "应有「查看定义」"
    );
    assert!(vcx.debug_bounds("menu-obj-copy").is_some());
    assert!(vcx.debug_bounds("menu-obj-drop").is_some());

    // 点遮罩关闭。
    let scrim = vcx.debug_bounds("obj-scrim").expect("缺少遮罩");
    vcx.simulate_click(scrim.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("menu-obj-copy").is_none(), "点别处应关闭");
}

/// F-84：连接树中的对象搜索（名称 + 注释）、置顶、最近访问，状态按连接落盘。
#[gpui::test]
fn objects_tree_search_pin_and_recent(cx: &mut gpui::TestAppContext) {
    let state_path =
        std::env::temp_dir().join(format!("rustdb-ui-objects-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&state_path);
    std::env::set_var("RUSTDB_OBJECT_STATE_PATH", &state_path);

    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.panel = PanelKind::Connections;
            view.namespaces = vec![ns("main")];
            view.current_ns = Some(ns("main"));
            view.session = Some(SessionId::new());
            view.active_profile = Some("demo-sqlite".to_string());
            view.tabs.push(SqlTab::new(1, Some(ns("main"))));
            view.active_tab = 0;
            view.expanded_ns.insert("main".to_string());
            // 分组默认只展开「表」，存储过程组要显式打开。
            view.group_state.insert(
                object_group_key("demo-sqlite", "main", ObjectKind::Procedure),
                true,
            );
            view.objects.insert(
                "main".to_string(),
                vec![
                    ObjectSummary {
                        path: ObjectPath::new(ns("main"), "orders", ObjectKind::Table),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: None,
                    },
                    ObjectSummary {
                        path: ObjectPath::new(ns("main"), "channels", ObjectKind::Table),
                        row_estimate: None,
                        comment: None,
                        has_primary_key: None,
                    },
                    ObjectSummary {
                        path: ObjectPath::new(ns("main"), "p_touch", ObjectKind::Procedure),
                        row_estimate: None,
                        comment: Some("回写渠道".into()),
                        has_primary_key: None,
                    },
                ],
            );
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("obj-main-orders").is_some());
    assert!(vcx.debug_bounds("obj-main-channels").is_some());
    assert!(vcx.debug_bounds("obj-main-p_touch").is_some());

    // 置顶：图钉点一下，orders 从分组区移到「置顶」，状态写到活动连接下。
    let pin = vcx
        .debug_bounds("obj-pin-orders")
        .expect("缺少 orders 图钉");
    vcx.simulate_click(pin.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("obj-pinned-orders").is_some(),
        "orders 应出现在置顶区"
    );
    let key = "main|Table|orders";
    assert!(
        vcx.update(|_, app| view
            .read(app)
            .object_state
            .profile("demo-sqlite")
            .is_some_and(|state| state.is_pinned(key))),
        "置顶状态应记在活动连接下"
    );
    let saved = rustdb_platform::object_state::load_from(&state_path).expect("状态应落盘");
    assert!(
        saved
            .profile("demo-sqlite")
            .is_some_and(|state| state.is_pinned(key)),
        "置顶状态应写入按连接的状态文件"
    );

    // 最近：访问过的对象出现在「最近」区，置顶项不再重复。
    let channels = ObjectPath::new(ns("main"), "channels", ObjectKind::Table);
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.record_recent_object(&channels);
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("obj-recent-channels").is_some(),
        "channels 应出现在最近区"
    );

    // 搜索：注释命中（p_touch 的「回写渠道」），其余对象隐藏。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.object_filter = "回写".into();
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("obj-main-orders").is_none(),
        "不匹配的对象应隐藏"
    );
    assert!(vcx.debug_bounds("obj-recent-channels").is_none());
    assert!(vcx.debug_bounds("obj-pinned-orders").is_none());
    assert!(
        vcx.debug_bounds("obj-main-p_touch").is_some(),
        "注释命中应保留"
    );

    // 按名称过滤。
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.object_filter = "orders".into();
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("obj-main-p_touch").is_none());
    assert!(vcx.debug_bounds("obj-pinned-orders").is_some());

    std::env::remove_var("RUSTDB_OBJECT_STATE_PATH");
    let _ = std::fs::remove_file(&state_path);
}
