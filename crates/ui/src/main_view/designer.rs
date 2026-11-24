//! 表设计器：列 / 索引 / 外键 / 检查 / 触发器 / 选项与 DDL 计划。

use super::*;

/// 工具条右侧的一句话说明，避免用户不知道当前页签该点哪里。
fn designer_status_hint(designer: &DesignerState) -> String {
    match designer.tab {
        rustdb_core::DesignerTab::Columns => {
            format!("选中字段后可在下方修改属性 · 共 {} 个字段", designer.draft.columns.len())
        }
        rustdb_core::DesignerTab::Indexes => "在下方填写索引名与列，点「添加索引」".into(),
        rustdb_core::DesignerTab::ForeignKeys => "新增外键需逐条「保存」后才下发".into(),
        rustdb_core::DesignerTab::Checks => "在下方填写约束名与表达式".into(),
        rustdb_core::DesignerTab::Triggers => "触发器只读，编辑请使用例程源码面板".into(),
        rustdb_core::DesignerTab::Options => "存储引擎 / 排序规则 / 表注释（按方言支持）".into(),
    }
}

/// 从 `varchar(50)` / `numeric(12,2)` 拆出 Navicat 网格里的「类型 / 长度 / 小数位」。
fn type_parts(type_text: &str) -> (String, String, String) {
    let text = type_text.trim();
    let Some(open) = text.find('(') else {
        return (text.to_string(), String::new(), String::new());
    };
    let Some(close) = text.rfind(')') else {
        return (text.to_string(), String::new(), String::new());
    };
    let base = text[..open].trim().to_string();
    let args = &text[open + 1..close];
    let mut parts = args.split(',');
    let length = parts.next().unwrap_or("").trim().to_string();
    let decimals = parts.next().unwrap_or("").trim().to_string();
    (base, length, decimals)
}

impl MainView {
    /// 设计器分区标题：统一的 13px 半粗标题 + 12px 灰色说明。
    fn designer_section(
        title: &'static str,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = current(cx);
        h_flex()
            .mb(px(2.))
            .gap_2()
            .items_center()
            .child(
                div()
                    .text_size(px(crate::design::TS_BODY))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .text_ellipsis()
                    .child(SharedString::from(hint)),
            )
            .into_any_element()
    }

    /// Navicat 风格的表设计器：占满工作区（连接树保留），页签 + 工具栏 +
    /// 字段网格 + 属性面板 + SQL 预览 + 状态栏。
    pub(crate) fn render_table_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some((label, detail)) = self.table_panel.clone() else {
            return div().into_any_element();
        };
        let tab = self.designer.as_ref().map(|designer| designer.tab);
        let sql_preview = self
            .designer
            .as_ref()
            .is_some_and(|designer| designer.sql_preview);
        // 输入框按当前页签懒创建（需要 window）。
        if let Some(designer) = self.designer.as_mut() {
            let options = designer.draft.options.clone();
            let field = designer.draft.columns.get(designer.selected_column).cloned();
            designer.inputs.ensure(
                tab.unwrap_or(rustdb_core::DesignerTab::Columns),
                &options,
                window,
                cx,
            );
            designer.inputs.ensure_field(field.as_ref(), window, cx);
        }

        let tabs_bar = self.render_designer_tabs(cx);
        let toolbar = self.render_designer_toolbar(cx);
        let body = if sql_preview {
            self.render_designer_sql_preview(cx)
        } else {
            match tab {
                Some(rustdb_core::DesignerTab::Columns) => self.render_designer_columns(cx),
                Some(rustdb_core::DesignerTab::Indexes) => self.render_designer_indexes(cx),
                Some(rustdb_core::DesignerTab::ForeignKeys) => self.render_designer_foreign_keys(cx),
                Some(rustdb_core::DesignerTab::Checks) => self.render_designer_checks(cx),
                Some(rustdb_core::DesignerTab::Triggers) => self.render_designer_triggers(cx),
                Some(rustdb_core::DesignerTab::Options) => self.render_designer_options(cx),
                None => self.render_structure_body(&detail, cx),
            }
        };
        let issues = self
            .designer
            .as_ref()
            .map(|designer| designer.issues.clone())
            .unwrap_or_default();
        let failure = self
            .designer
            .as_ref()
            .and_then(|designer| designer.failure.clone());
        let rebuild = self
            .designer
            .as_ref()
            .and_then(|designer| {
                let session = self.session.as_ref()?;
                let kind = self.core.session_kind(session)?;
                Some(designer_needs_rebuild(kind, designer))
            })
            .unwrap_or(false);
        let column_count = detail.columns.len();

        v_flex()
            .id("table-panel")
            .debug_selector(|| "table-panel".to_string())
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(hex(t.bg_base))
            .occlude()
            .overflow_hidden()
            .on_click(cx.listener(|_, _, _, _| {}))
            .child(
                h_flex()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(Icon::new(IconName::Table).small().text_color(hex(t.accent)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_BASE))
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(hex(t.fg_primary))
                                    .text_ellipsis()
                                    .child(SharedString::from(label)),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_muted))
                                    .text_ellipsis()
                                    .child(SharedString::from(format!(
                                        "表设计器 · {column_count} 个字段"
                                    ))),
                            ),
                    )
                    .when(self.designer.is_some(), |row| {
                        row.child(
                            div().debug_selector(|| "designer-save".to_string()).child(
                                Button::new("designer-save-btn")
                                    .small()
                                    .icon(IconName::Save)
                                    .label("保存")
                                    .on_click(cx.listener(|view, _, _, cx| view.designer_apply(cx))),
                            ),
                        )
                    })
                    .child(
                        div()
                            .id("table-panel-close")
                            .debug_selector(|| "table-panel-close".to_string())
                            .w(px(26.))
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.table_panel = None;
                                view.designer = None;
                                view.design_pending = None;
                                cx.notify();
                            }))
                            .child(Icon::new(IconName::Close).small().text_color(hex(t.fg_muted))),
                    ),
            )
            .when(tab.is_some(), |panel| panel.child(tabs_bar))
            .when(tab.is_some() && !sql_preview, |panel| panel.child(toolbar))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(body),
            )
            .child(self.render_designer_footer(issues, failure, rebuild, cx))
            .into_any_element()
    }

    /// Navicat 式下划线页签：字段 / 索引 / 外键 / 检查 / 触发器 / 选项 / SQL 预览。
    pub(crate) fn render_designer_tabs(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let active = self
            .designer
            .as_ref()
            .map(|designer| designer.tab)
            .unwrap_or(rustdb_core::DesignerTab::Columns);
        let preview = self
            .designer
            .as_ref()
            .is_some_and(|designer| designer.sql_preview);
        let mut row = h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_2()
            .gap(px(2.))
            .items_stretch()
            .border_b_1()
            .border_color(hex(t.border_subtle));
        for tab in rustdb_core::DesignerTab::ALL {
            let label = tab.label();
            let on = !preview && tab == active;
            row = row.child(
                div()
                    .id(SharedString::from(format!("designer-tab-{label}")))
                    .debug_selector({
                        let selector = format!("designer-tab-{label}");
                        move || selector.clone()
                    })
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(crate::design::TS_BODY))
                    .text_color(hex(if on { t.accent } else { t.fg_secondary }))
                    .border_b_2()
                    .border_color(hex(if on { t.accent } else { t.bg_base }))
                    .hover(|s| s.bg(hex(t.bg_elevated)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(designer) = view.designer.as_mut() {
                            designer.tab = tab;
                            designer.sql_preview = false;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(label)),
            );
        }
        row.child(
            div()
                .id("designer-tab-sql")
                .debug_selector(|| "designer-tab-SQL 预览".to_string())
                .h_full()
                .px_3()
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(px(crate::design::TS_BODY))
                .text_color(hex(if preview { t.accent } else { t.fg_secondary }))
                .border_b_2()
                .border_color(hex(if preview { t.accent } else { t.bg_base }))
                .hover(|s| s.bg(hex(t.bg_elevated)))
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some(designer) = view.designer.as_mut() {
                        designer.sql_preview = true;
                    }
                    cx.notify();
                }))
                .child("SQL 预览"),
        )
        .into_any_element()
    }

    /// 工具栏按钮：Navicat 的页签工具条（图标 + 文字，统一 hover/focus 态）。
    fn designer_tool(
        id: &'static str,
        label: &'static str,
        icon: IconName,
        tip: &'static str,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui_kit::AnyElement {
        div()
            .debug_selector(move || id.to_string())
            .child(
                Button::new(id)
                    .small()
                    .ghost()
                    .icon(icon)
                    .label(label)
                    .tooltip(tip)
                    .on_click(cx.listener(move |view, _event, window, cx| {
                        action(view, window, cx)
                    })),
            )
            .into_any_element()
    }

    /// 页签工具条：按当前页签换动作；字段页提供 Navicat 的增 / 插 / 删 / 键 / 上下移。
    pub(crate) fn render_designer_toolbar(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let (tab, hint) = self
            .designer
            .as_ref()
            .map(|designer| (designer.tab, designer_status_hint(designer)))
            .unwrap_or((rustdb_core::DesignerTab::Columns, String::new()));
        let divider = || div().w(px(1.)).h(px(16.)).mx_1().bg(hex(t.border_subtle));
        let mut row = h_flex()
            .h(px(34.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle));
        match tab {
            rustdb_core::DesignerTab::Columns => {
                row = row
                    .child(Self::designer_tool(
                        "designer-field-add",
                        "添加字段",
                        IconName::ListPlus,
                        "在末尾追加字段",
                        cx,
                        |view, window, cx| view.designer_insert_column(false, window, cx),
                    ))
                    .child(Self::designer_tool(
                        "designer-field-insert",
                        "插入字段",
                        IconName::CopyPlus,
                        "在选中字段后插入",
                        cx,
                        |view, window, cx| view.designer_insert_column(true, window, cx),
                    ))
                    .child(Self::designer_tool(
                        "designer-field-delete",
                        "删除字段",
                        IconName::Trash,
                        "从草稿删除选中字段（保存后删列）",
                        cx,
                        |view, window, cx| view.designer_delete_selected_column(window, cx),
                    ))
                    .child(divider())
                    .child(Self::designer_tool(
                        "designer-field-pk",
                        "主键",
                        IconName::KeyRound,
                        "切换选中字段的主键标记",
                        cx,
                        |view, _, cx| view.designer_toggle_column_pk(cx),
                    ))
                    .child(Self::designer_tool(
                        "designer-field-up",
                        "上移",
                        IconName::ArrowUp,
                        "上移选中字段",
                        cx,
                        |view, _, cx| view.designer_move_selected_column(-1, cx),
                    ))
                    .child(Self::designer_tool(
                        "designer-field-down",
                        "下移",
                        IconName::ArrowDown,
                        "下移选中字段",
                        cx,
                        |view, _, cx| view.designer_move_selected_column(1, cx),
                    ));
            }
            rustdb_core::DesignerTab::Indexes
            | rustdb_core::DesignerTab::ForeignKeys
            | rustdb_core::DesignerTab::Checks => {
                row = row.child(Self::designer_tool(
                    "designer-row-delete",
                    "删除选中行",
                    IconName::Trash,
                    "删除当前页签选中的草稿行",
                    cx,
                    |view, _, cx| view.designer_delete_selected_row(cx),
                ));
            }
            _ => {}
        }
        row.child(div().flex_1())
            .child(
                div()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .text_ellipsis()
                    .child(SharedString::from(hint)),
            )
            .into_any_element()
    }

    /// SQL 预览页：直接展示即将执行的 DDL，不另起预览弹窗。
    pub(crate) fn render_designer_sql_preview(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let text = match self.designer_plan() {
            Ok(plan) if plan.is_empty() => "-- 没有待执行的改动".to_string(),
            Ok(plan) => plan.executable_sql().join("\n\n"),
            Err(message) => format!("-- {message}"),
        };
        div()
            .id("designer-sql-preview")
            .debug_selector(|| "designer-sql-preview".to_string())
            .size_full()
            .min_h_0()
            .p_3()
            .overflow_scroll()
            .child(
                v_flex()
                    .gap_2()
                    .children(text.split("\n\n").enumerate().map(|(index, sql)| {
                        h_flex()
                            .gap_3()
                            .items_start()
                            .child(
                                div()
                                    .w(px(28.))
                                    .text_align(gpui_kit::TextAlign::Right)
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_muted))
                                    .child(SharedString::from(format!("{}", index + 1))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(crate::design::TS_BODY))
                                    .text_color(hex(t.fg_primary))
                                    .child(SharedString::from(sql.to_string())),
                            )
                    })),
            )
            .into_any_element()
    }

    /// 设计器状态栏：左侧校验/失败说明，右侧预览与保存。
    pub(crate) fn render_designer_footer(
        &mut self,
        issues: Vec<rustdb_core::ValidationIssue>,
        failure: Option<DesignerFailure>,
        rebuild: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = current(cx);
        let has_designer = self.designer.is_some();
        let error_count = issues
            .iter()
            .filter(|issue| issue.level == rustdb_core::IssueLevel::Error)
            .count();
        let warning_count = issues.len() - error_count;

        let (icon, color, mut lines): (IconName, u32, Vec<String>) = if let Some(failure) = failure {
            (
                IconName::TriangleAlert,
                t.danger,
                vec![
                    format!("失败：{}", failure.message),
                    failure.hint.to_string(),
                ],
            )
        } else if error_count > 0 {
            (
                IconName::TriangleAlert,
                t.danger,
                vec![format!("{error_count} 处问题待修正，保存前需先处理")],
            )
        } else if warning_count > 0 {
            (
                IconName::Info,
                t.warning,
                vec![format!("{warning_count} 条提示")],
            )
        } else {
            (
                IconName::Check,
                t.fg_muted,
                vec!["就绪 · 保存后写入数据库".to_string()],
            )
        };
        if rebuild {
            lines.push("SQLite 改结构将重建表（先备份）".to_string());
        }
        let status = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .items_start()
            .child(Icon::new(icon).small().text_color(hex(color)).mt(px(1.)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .children(lines.into_iter().enumerate().map(|(index, line)| {
                        div()
                            .text_size(px(if index == 0 {
                                crate::design::TS_BODY
                            } else {
                                crate::design::TS_CAPTION
                            }))
                            .text_color(hex(if index == 0 { color } else { t.fg_muted }))
                            .text_ellipsis()
                            .child(SharedString::from(line))
                    })),
            );

        h_flex()
            .h(px(42.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(hex(t.border_subtle))
            .child(status)
            .when(has_designer, |footer| {
                footer
                    .child(
                        div()
                            .debug_selector(|| "designer-preview".to_string())
                            .child(
                                Button::new("designer-preview-btn")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Eye)
                                    .label("预览 SQL")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.designer_preview(cx);
                                    })),
                            ),
                    )
                    .child(
                        div().debug_selector(|| "designer-apply".to_string()).child(
                            Button::new("designer-apply-btn")
                                .small()
                                .icon(IconName::Save)
                                .label("保存")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.designer_apply(cx);
                                })),
                        ),
                    )
            })
            .into_any_element()
    }

    /// 只读结构视图（没有进入设计器时）。
    pub(crate) fn render_structure_body(
        &mut self,
        detail: &rustdb_db_api::TableDetail,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = current(cx);
        let mut columns = v_flex().gap(px(2.));
        for column in &detail.columns {
            let mut flags: Vec<gpui_kit::AnyElement> = Vec::new();
            if column.is_primary_key {
                flags.push(
                    gpui_kit::component::tag::Tag::primary().small()
                        .child("PK")
                        .into_any_element(),
                );
            }
            if !column.meta.nullable {
                flags.push(
                    gpui_kit::component::tag::Tag::warning().small()
                        .child("NOT NULL")
                        .into_any_element(),
                );
            }
            if let Some(default) = column.default.as_deref() {
                flags.push(
                    gpui_kit::component::tag::Tag::secondary().small()
                        .child(SharedString::from(default.to_string()))
                        .into_any_element(),
                );
            }
            columns = columns.child(
                h_flex()
                    .h(px(28.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        div()
                            .w(px(150.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(column.meta.name.to_string())),
                    )
                    .child(
                        div()
                            .w(px(130.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(column.meta.native_type.clone())),
                    )
                    .child(h_flex().flex_1().min_w_0().gap_1().children(flags)),
            );
        }
        let mut indexes = v_flex().gap(px(2.));
        for index in &detail.indexes {
            let tag = if index.primary {
                "PK"
            } else if index.unique {
                "UNIQUE"
            } else {
                "INDEX"
            };
            indexes = indexes.child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.accent))
                            .child(SharedString::from(tag)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(format!(
                                "{}（{}）",
                                index.name,
                                index.columns.join(", ")
                            ))),
                    ),
            );
        }
        v_flex()
            .child(Self::designer_section("列", "只读结构（点对象树「设计表」进入编辑）", cx))
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .bg(hex(t.bg_overlay))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(div().w(px(150.)).child("列名"))
                    .child(div().w(px(130.)).child("类型"))
                    .child(div().flex_1().child("约束")),
            )
            .child(columns)
            .when(!detail.indexes.is_empty(), |panel| {
                panel
                    .child(
                        div()
                            .px_3()
                            .pt_2()
                            .pb_1()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child("索引"),
                    )
                    .child(indexes)
            })
            .into_any_element()
    }

    /// Navicat 字段页：上方字段网格 + 下方「字段属性」面板。
    pub(crate) fn render_designer_columns(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        let columns = designer.draft.columns.clone();
        let before = designer.before.clone();
        let selected = designer
            .selected_column
            .min(columns.len().saturating_sub(1));
        let field_inputs = (
            designer.inputs.field_name.clone(),
            designer.inputs.field_type.clone(),
            designer.inputs.field_default.clone(),
        );

        let head_cell = |width: f32, label: &'static str| {
            div()
                .w(px(width))
                .flex_shrink_0()
                .text_size(px(crate::design::TS_CAPTION))
                .child(label)
        };
        let header = h_flex()
            .h(px(26.))
            .w_full()
            .min_w(px(760.))
            .px_2()
            .items_center()
            .bg(hex(t.bg_elevated))
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .text_color(hex(t.fg_muted))
            .child(head_cell(30., "#"))
            .child(head_cell(150., "名"))
            .child(head_cell(110., "类型"))
            .child(head_cell(56., "长度"))
            .child(head_cell(56., "小数位"))
            .child(head_cell(66., "不是 null"))
            .child(head_cell(72., "自动递增"))
            .child(head_cell(46., "键"))
            .child(head_cell(130., "默认值"))
            .child(div().flex_1().min_w(px(120.)).child("注释"));

        let cell = |width: f32, text: String, color: u32| {
            div()
                .w(px(width))
                .flex_shrink_0()
                .px_1()
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(color))
                .text_ellipsis()
                .child(SharedString::from(text))
        };
        let mut rows = v_flex().w_full().min_w(px(760.));
        for (index, draft) in columns.iter().enumerate() {
            let (base, length, decimals) = type_parts(&draft.type_text);
            let old = before
                .iter()
                .find(|column| column.meta.name.eq_ignore_ascii_case(&draft.name));
            let comment = old
                .and_then(|column| column.comment.clone())
                .unwrap_or_default();
            let auto_increment = old.is_some_and(|column| column.is_auto_increment);
            let on = index == selected;
            let not_null_id = SharedString::from(format!("designer-not-null-{index}"));
            rows = rows.child(
                h_flex()
                    .id(SharedString::from(format!("designer-column-{index}")))
                    .debug_selector({
                        let selector = format!("designer-column-{index}");
                        move || selector.clone()
                    })
                    .h(px(28.))
                    .w_full()
                    .min_w(px(760.))
                    .px_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .when(on, |row| row.bg(crate::theme::to_hsla(t.accent).opacity(0.14)))
                    .when(!on, |row| row.hover(|s| s.bg(hex(t.bg_elevated))))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.designer_select_column(index, window, cx);
                    }))
                    .child(cell(
                        30.,
                        format!("{}", index + 1),
                        t.fg_muted,
                    ))
                    .child(cell(
                        150.,
                        draft.name.clone(),
                        if draft.primary_key { t.accent } else { t.fg_primary },
                    ))
                    .child(cell(110., base, t.fg_secondary))
                    .child(cell(56., length, t.fg_secondary))
                    .child(cell(56., decimals, t.fg_secondary))
                    .child(
                        div().w(px(66.)).flex_shrink_0().flex().justify_center().child(
                            gpui_kit::component::checkbox::Checkbox::new(not_null_id)
                                .checked(!draft.nullable)
                                .tooltip("不是 null")
                                .on_click(cx.listener(move |view, checked, _, cx| {
                                    view.designer_set_column_null(index, *checked, cx);
                                })),
                        ),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(if auto_increment {
                                Icon::new(IconName::Check)
                                    .small()
                                    .text_color(hex(t.success))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .w(px(46.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(if draft.primary_key {
                                Icon::new(IconName::KeyRound)
                                    .small()
                                    .text_color(hex(t.warning))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .child(cell(
                        130.,
                        draft.default.clone().unwrap_or_default(),
                        t.fg_secondary,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(120.))
                            .px_1()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .text_ellipsis()
                            .child(SharedString::from(comment)),
                    ),
            );
        }

        let grid = v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_x_scrollbar()
            .child(header)
            .when(columns.is_empty(), |view| {
                view.child(crate::design::empty_state(
                    cx,
                    IconName::Table,
                    "还没有字段",
                    "点上方「添加字段」新增一行",
                ))
            })
            .when(!columns.is_empty(), |view| {
                view.child(div().flex_1().min_h_0().overflow_y_scrollbar().child(rows))
            });

        let mut panel = v_flex()
            .h(px(160.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(crate::design::TS_BODY))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(hex(t.fg_primary))
                            .child("字段属性"),
                    )
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(if columns.is_empty() {
                                String::new()
                            } else {
                                format!("第 {} 行", selected + 1)
                            })),
                    ),
            );
        if !columns.is_empty() {
            let (name_input, type_input, default_input) = field_inputs;
            let current = &columns[selected];
            let (base, length, decimals) = type_parts(&current.type_text);
            let old = before
                .iter()
                .find(|column| column.meta.name.eq_ignore_ascii_case(&current.name));
            let comment = old
                .and_then(|column| column.comment.clone())
                .unwrap_or_default();
            let auto_increment = old.is_some_and(|column| column.is_auto_increment);
            let field_label = |label: &'static str| {
                div()
                    .mb(px(2.))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(label)
            };
            panel = panel
                .child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            v_flex().w(px(210.)).child(field_label("名")).child(match name_input {
                                Some(input) => Input::new(&input).small().into_any_element(),
                                None => div().into_any_element(),
                            }),
                        )
                        .child(
                            v_flex().w(px(210.)).child(field_label("类型")).child(match type_input {
                                Some(input) => Input::new(&input).small().into_any_element(),
                                None => div().into_any_element(),
                            }),
                        )
                        .child(
                            v_flex().w(px(210.)).child(field_label("默认值")).child(match default_input {
                                Some(input) => Input::new(&input).small().into_any_element(),
                                None => div().into_any_element(),
                            }),
                        ),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            v_flex()
                                .w(px(210.))
                                .child(field_label("长度"))
                                .child(
                                    div()
                                        .text_size(px(crate::design::TS_CAPTION))
                                        .text_color(hex(t.fg_secondary))
                                        .child(SharedString::from(if length.is_empty() {
                                            "—".to_string()
                                        } else {
                                            length
                                        })),
                                ),
                        )
                        .child(
                            v_flex()
                                .w(px(210.))
                                .child(field_label("小数位"))
                                .child(
                                    div()
                                        .text_size(px(crate::design::TS_CAPTION))
                                        .text_color(hex(t.fg_secondary))
                                        .child(SharedString::from(if decimals.is_empty() {
                                            "—".to_string()
                                        } else {
                                            decimals
                                        })),
                                ),
                        )
                        .child(
                            v_flex()
                                .w(px(210.))
                                .child(field_label("自动递增"))
                                .child(
                                    div()
                                        .text_size(px(crate::design::TS_CAPTION))
                                        .text_color(hex(if auto_increment { t.success } else { t.fg_muted }))
                                        .child(if auto_increment { "是" } else { "否" }),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(field_label("注释"))
                                .child(
                                    div()
                                        .text_size(px(crate::design::TS_CAPTION))
                                        .text_color(hex(t.fg_secondary))
                                        .text_ellipsis()
                                        .child(SharedString::from(if comment.is_empty() {
                                            "—".to_string()
                                        } else {
                                            comment
                                        })),
                                ),
                        ),
                )
                .child(
                    h_flex()
                        .gap_4()
                        .items_center()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    gpui_kit::component::checkbox::Checkbox::new("designer-field-not-null")
                                        .label("不是 null")
                                        .checked(!current.nullable)
                                        .on_click(cx.listener(|view, checked, _, cx| {
                                            let index = view
                                                .designer
                                                .as_ref()
                                                .map(|d| d.selected_column)
                                                .unwrap_or(0);
                                            view.designer_set_column_null(index, *checked, cx);
                                        })),
                                )
                                .child(
                                    gpui_kit::component::checkbox::Checkbox::new("designer-field-pk")
                                        .label("主键")
                                        .checked(current.primary_key)
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.designer_toggle_column_pk(cx);
                                        })),
                                ),
                        )
                        .child(
                            div().debug_selector(|| "designer-field-apply".to_string()).child(
                                Button::new("designer-field-apply-btn")
                                    .small()
                                    .label("应用属性")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.designer_apply_field(cx);
                                    })),
                            ),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .text_ellipsis()
                                .child(SharedString::from(format!("类型基名：{base}"))),
                        ),
                );
        }

        v_flex()
            .id("designer-field-grid")
            .debug_selector(|| "designer-field-grid".to_string())
            .size_full()
            .min_h_0()
            .child(grid)
            .child(panel)
            .into_any_element()
    }

    /// 设计器「索引」页签。
    pub(crate) fn render_designer_indexes(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        let baseline: Vec<String> = designer
            .before_indexes
            .iter()
            .filter(|index| !index.primary)
            .map(|index| index.name.clone())
            .collect();
        let indexes = designer.draft.indexes.clone();

        let mut list = v_flex().gap(px(2.));
        for (index, draft) in indexes.iter().enumerate() {
            let exists = baseline.iter().any(|name| name == &draft.name);
            let tag = if draft.unique { "UNIQUE" } else { "INDEX" };
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("designer-index-{index}")))
                    .debug_selector({
                        let selector = format!("designer-index-{index}");
                        move || selector.clone()
                    })
                    .h(px(28.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        div().w(px(56.)).child(
                            if exists {
                                gpui_kit::component::tag::Tag::primary().small()
                            } else {
                                gpui_kit::component::tag::Tag::success().small()
                            }
                            .child(SharedString::from(tag)),
                        ),
                    )
                    .child(
                        div()
                            .w(px(150.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(draft.name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(draft.columns.join(", "))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("designer-index-drop-{index}")))
                            .debug_selector({
                                let selector = format!("designer-index-drop-{index}");
                                move || selector.clone()
                            })
                            .w(px(24.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::theme::to_hsla(t.accent).opacity(0.10)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.designer_drop_index(index, cx);
                            }))
                            .child(Icon::new(IconName::Close).small().text_color(hex(t.danger))),
                    ),
            );
        }

        let mut add_row = h_flex().h(px(32.)).px_3().gap_2().items_center();
        if let Some(designer) = self.designer.as_ref() {
            if let (Some(name_input), Some(columns_input)) = (
                designer.inputs.index_name.clone(),
                designer.inputs.index_columns.clone(),
            ) {
                add_row = add_row
                    .child(
                        div()
                            .w(px(150.))
                            .child(gpui_kit::component::input::Input::new(&name_input).small()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(gpui_kit::component::input::Input::new(&columns_input).small()),
                    );
            }
        }
        add_row = add_row
            .child(
                div()
                    .debug_selector(|| "designer-index-add".to_string())
                    .child(
                        Button::new("designer-index-add-btn")
                            .small()
                            .label("添加索引")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.designer_add_index(window, cx);
                            })),
                    ),
            )
            .child(div().flex_1());

        v_flex()
            .child(Self::designer_section("索引", "UNIQUE / INDEX 草稿，执行前可预览", cx))
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .bg(hex(t.bg_overlay))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(div().w(px(56.)).child("类型"))
                    .child(div().w(px(150.)).child("名称"))
                    .child(div().flex_1().child("列"))
                    .child(div().w(px(18.))),
            )
            .child(list)
            .child(add_row)
            .into_any_element()
    }

    /// 设计器「外键」页签：每条外键可单独保存。
    pub(crate) fn render_designer_foreign_keys(
        &mut self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        let baseline: Vec<String> = designer
            .before_foreign_keys
            .iter()
            .map(|fk| fk.name.clone())
            .collect();
        let foreign_keys = designer.draft.foreign_keys.clone();

        let mut list = v_flex().gap(px(2.));
        for (index, fk) in foreign_keys.iter().enumerate() {
            let exists = baseline.iter().any(|name| name == &fk.name);
            let target = format!(
                "{} → {}({})",
                fk.columns.join(","),
                fk.referenced_table,
                fk.referenced_columns.join(",")
            );
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("designer-fk-{index}")))
                    .debug_selector({
                        let selector = format!("designer-fk-{index}");
                        move || selector.clone()
                    })
                    .h(px(28.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        div()
                            .w(px(130.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(fk.name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(target)),
                    )
                    .when(!exists, |row| {
                        row.child(
                            div()
                                .id(SharedString::from(format!("designer-fk-save-{index}")))
                                .debug_selector({
                                    let selector = format!("designer-fk-save-{index}");
                                    move || selector.clone()
                                })
                                .px_3()
                                .h(px(22.))
                                .flex()
                                .items_center()
                                .rounded(px(6.))
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.accent))
                                .cursor_pointer()
                                .hover(|s| s.bg(crate::theme::to_hsla(t.accent).opacity(0.10)))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.designer_save_foreign_key(index, cx);
                                }))
                                .child("保存"),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("designer-fk-drop-{index}")))
                            .debug_selector({
                                let selector = format!("designer-fk-drop-{index}");
                                move || selector.clone()
                            })
                            .w(px(24.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::theme::to_hsla(t.accent).opacity(0.10)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.designer_remove_foreign_key(index, cx);
                            }))
                            .child(Icon::new(IconName::Close).small().text_color(hex(t.danger))),
                    ),
            );
        }

        let mut add_row = h_flex().h(px(32.)).px_3().gap_2().items_center();
        if let Some(designer) = self.designer.as_ref() {
            if let (Some(name_input), Some(columns_input), Some(table_input), Some(ref_input)) = (
                designer.inputs.fk_name.clone(),
                designer.inputs.fk_columns.clone(),
                designer.inputs.fk_ref_table.clone(),
                designer.inputs.fk_ref_columns.clone(),
            ) {
                add_row = add_row
                    .child(
                        div()
                            .w(px(110.))
                            .child(gpui_kit::component::input::Input::new(&name_input).small()),
                    )
                    .child(
                        div()
                            .w(px(96.))
                            .child(gpui_kit::component::input::Input::new(&columns_input).small()),
                    )
                    .child(
                        div()
                            .w(px(96.))
                            .child(gpui_kit::component::input::Input::new(&table_input).small()),
                    )
                    .child(
                        div()
                            .w(px(96.))
                            .child(gpui_kit::component::input::Input::new(&ref_input).small()),
                    );
            }
        }
        add_row = add_row
            .child(
                div()
                    .debug_selector(|| "designer-fk-add".to_string())
                    .child(
                        Button::new("designer-fk-add-btn")
                            .small()
                            .label("添加外键")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.designer_add_foreign_key(window, cx);
                            })),
                    ),
            )
            .child(div().flex_1());

        v_flex()
            .child(Self::designer_section("外键", "新增的外键需逐条「保存」后才进入迁移计划", cx))
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .bg(hex(t.bg_overlay))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(div().w(px(130.)).child("名称"))
                    .child(div().flex_1().child("本地列 → 引用表(列)"))
                    .child(div().w(px(38.)))
                    .child(div().w(px(18.))),
            )
            .child(list)
            .child(add_row)
            .into_any_element()
    }

    /// 设计器「检查约束」页签。
    pub(crate) fn render_designer_checks(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        let baseline: Vec<String> = designer
            .before_checks
            .iter()
            .map(|check| check.name.clone())
            .collect();
        let checks = designer.draft.checks.clone();

        let mut list = v_flex().gap(px(2.));
        for (index, check) in checks.iter().enumerate() {
            let exists = baseline.iter().any(|name| name == &check.name);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("designer-check-{index}")))
                    .debug_selector({
                        let selector = format!("designer-check-{index}");
                        move || selector.clone()
                    })
                    .h(px(28.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        div()
                            .w(px(130.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(check.name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(check.expression.clone())),
                    )
                    .child(
                        div().w(px(52.)).child(
                            if exists {
                                gpui_kit::component::tag::Tag::primary().small()
                            } else {
                                gpui_kit::component::tag::Tag::success().small()
                            }
                            .child(if exists { "现状" } else { "新增" }),
                        ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("designer-check-drop-{index}")))
                            .debug_selector({
                                let selector = format!("designer-check-drop-{index}");
                                move || selector.clone()
                            })
                            .w(px(24.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::theme::to_hsla(t.accent).opacity(0.10)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.designer_drop_check(index, cx);
                            }))
                            .child(Icon::new(IconName::Close).small().text_color(hex(t.danger))),
                    ),
            );
        }

        let mut add_row = h_flex().h(px(32.)).px_3().gap_2().items_center();
        if let Some(designer) = self.designer.as_ref() {
            if let (Some(name_input), Some(expr_input)) = (
                designer.inputs.check_name.clone(),
                designer.inputs.check_expr.clone(),
            ) {
                add_row = add_row
                    .child(
                        div()
                            .w(px(130.))
                            .child(gpui_kit::component::input::Input::new(&name_input).small()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(gpui_kit::component::input::Input::new(&expr_input).small()),
                    );
            }
        }
        add_row = add_row
            .child(
                div()
                    .debug_selector(|| "designer-check-add".to_string())
                    .child(
                        Button::new("designer-check-add-btn")
                            .small()
                            .label("添加约束")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.designer_add_check(window, cx);
                            })),
                    ),
            )
            .child(div().flex_1());

        v_flex()
            .child(Self::designer_section("检查约束", "表达式草稿，执行前可预览", cx))
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .bg(hex(t.bg_overlay))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(div().w(px(130.)).child("名称"))
                    .child(div().flex_1().child("表达式"))
                    .child(div().w(px(52.)))
                    .child(div().w(px(18.))),
            )
            .child(list)
            .child(add_row)
            .into_any_element()
    }

    /// 设计器「触发器」页签：只读（触发体编辑归 F-82 例程源码）。
    pub(crate) fn render_designer_triggers(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        match &designer.triggers {
            None => crate::design::empty_state(
                cx,
                IconName::Zap,
                "该连接的驱动未提供触发器元数据",
                "触发器编辑请到「例程源码」面板",
            ),
            Some(triggers) if triggers.is_empty() => crate::design::empty_state(
                cx,
                IconName::Zap,
                "该表没有触发器",
                "新建触发器请使用数据库的 DDL 语句",
            ),
            Some(triggers) => {
                let mut list = v_flex().gap(px(2.));
                for trigger in triggers {
                    let mut tags: Vec<&str> = Vec::new();
                    if let Some(timing) = &trigger.timing {
                        tags.push(timing);
                    }
                    if let Some(event) = &trigger.event {
                        tags.push(event);
                    }
                    list = list.child(
                        h_flex()
                            .h(px(28.))
                            .px_3()
                            .gap_2()
                            .items_center()
                            .border_b_1()
                            .border_color(hex(t.border_subtle))
                            .child(
                                div()
                                    .w(px(170.))
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_primary))
                                    .text_ellipsis()
                                    .child(SharedString::from(trigger.name.clone())),
                            )
                            .child(
                                div()
                                    .w(px(140.))
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.accent))
                                    .text_ellipsis()
                                    .child(SharedString::from(tags.join(" · "))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_muted))
                                    .text_ellipsis()
                                    .child(SharedString::from(
                                        trigger.statement.clone().unwrap_or_default(),
                                    )),
                            ),
                    );
                }
                list.into_any_element()
            }
        }
    }

    /// 设计器「选项」页签：存储引擎 / 排序规则 / 注释。
    /// 设计器「选项」页签：存储引擎 / 排序规则 / 注释（各方言支持度不同）。
    pub(crate) fn render_designer_options(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        let engine = designer.inputs.engine.clone();
        let collation = designer.inputs.collation.clone();
        let comment = designer.inputs.comment.clone();

        let mut list = v_flex().gap(px(2.));
        for (label, input) in [
            ("存储引擎", &engine),
            ("排序规则", &collation),
            ("表注释", &comment),
        ] {
            list = list.child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        div()
                            .w(px(80.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(label.to_string())),
                    )
                    .child(
                        div().flex_1().min_w_0().child(match input {
                            Some(input) => gpui_kit::component::input::Input::new(input)
                                .small()
                                .into_any_element(),
                            None => div().into_any_element(),
                        }),
                    ),
            );
        }
        v_flex()
            .child(Self::designer_section("表选项", "各方言支持度不同，未支持的项不会下发", cx))
            .child(list)
            .child(
                h_flex()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .debug_selector(|| "designer-option-apply".to_string())
                            .child(
                                Button::new("designer-option-apply-btn")
                                    .small()
                                    .label("应用到草稿")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.designer_apply_options(cx);
                                    })),
                            ),
                    )
                    .child(div().flex_1()),
            )
            .into_any_element()
    }

    /// 设计器：加入一条索引草稿。
    pub(crate) fn designer_add_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let name = designer
            .inputs
            .index_name
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let columns = designer
            .inputs
            .index_columns
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if name.is_empty() || columns.is_empty() {
            self.status = "索引名与列都不能为空".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let columns: Vec<String> = columns
            .split(',')
            .map(|column| column.trim().to_string())
            .filter(|column| !column.is_empty())
            .collect();
        if designer
            .draft
            .indexes
            .iter()
            .any(|index| index.name.eq_ignore_ascii_case(&name))
        {
            self.status = format!("索引 {name} 已存在");
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        designer
            .draft
            .indexes
            .push(rustdb_core::IndexDraft::new(name, columns));
        self.designer_clear_inputs(&["index_name", "index_columns"], window, cx);
        self.status = "已加入索引草稿（尚未执行）".into();
        self.status_kind = StatusKind::Ok;
        cx.notify();
    }

    /// 设计器：移除一条索引草稿（现状索引会在迁移计划里变成 DROP INDEX）。
    pub(crate) fn designer_drop_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        if index >= designer.draft.indexes.len() {
            return;
        }
        let removed = designer.draft.indexes.remove(index);
        self.status = format!("已移除索引 {}", removed.name);
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 设计器：加入一条外键草稿。
    pub(crate) fn designer_add_foreign_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let name = designer
            .inputs
            .fk_name
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let columns = designer
            .inputs
            .fk_columns
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let referenced_table = designer
            .inputs
            .fk_ref_table
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let referenced_columns = designer
            .inputs
            .fk_ref_columns
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if name.is_empty()
            || columns.is_empty()
            || referenced_table.is_empty()
            || referenced_columns.is_empty()
        {
            self.status = "外键名、本地列、引用表、引用列都要填".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let split = |text: &str| -> Vec<String> {
            text.split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        };
        let draft = rustdb_core::ForeignKeyDraft::new(
            name,
            split(&columns),
            referenced_table,
            split(&referenced_columns),
        );
        if designer
            .draft
            .foreign_keys
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&draft.name))
        {
            self.status = format!("外键 {} 已存在", draft.name);
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        designer.draft.foreign_keys.push(draft);
        self.designer_clear_inputs(
            &["fk_name", "fk_columns", "fk_ref_table", "fk_ref_columns"],
            window,
            cx,
        );
        self.status = "已加入外键草稿，点「保存」逐条下发".into();
        self.status_kind = StatusKind::Ok;
        cx.notify();
    }

    /// 设计器：外键逐条保存（只下发这一条外键的 DDL）。
    pub(crate) fn designer_save_foreign_key(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let Some(designer) = self.designer.as_ref() else {
            return;
        };
        let Some(fk) = designer.draft.foreign_keys.get(index).cloned() else {
            return;
        };
        let Some(kind) = self.core.session_kind(&session) else {
            self.status = "无法识别连接类型，请重新连接".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let plan = rustdb_core::plan_add_foreign_key(kind, &designer.path, &fk);
        self.designer_run_plan(
            plan,
            &session,
            format!("保存外键 {}", fk.name),
            Some(DesignerFkOp {
                add: true,
                name: fk.name.clone(),
                definition: Some(rustdb_db_api::ForeignKeyInfo {
                    name: fk.name.clone(),
                    columns: fk.columns.clone(),
                    referenced_table: fk.referenced_table.clone(),
                    referenced_columns: fk.referenced_columns.clone(),
                    on_delete: fk.on_delete.clone(),
                    on_update: fk.on_update.clone(),
                }),
            }),
            cx,
        );
    }

    /// 设计器：移除一条外键（现状外键直接下发 DROP，草稿外键只出草稿）。
    pub(crate) fn designer_remove_foreign_key(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        if index >= designer.draft.foreign_keys.len() {
            return;
        }
        let removed = designer.draft.foreign_keys.remove(index);
        let exists = designer
            .before_foreign_keys
            .iter()
            .any(|fk| fk.name == removed.name);
        if !exists {
            self.status = format!("已移除外键草稿 {}", removed.name);
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let Some(kind) = self.core.session_kind(&session) else {
            self.status = "无法识别连接类型，请重新连接".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let path = designer.path.clone();
        let plan = rustdb_core::plan_drop_foreign_key(kind, &path, &removed.name);
        self.designer_run_plan(
            plan,
            &session,
            format!("删除外键 {}", removed.name),
            Some(DesignerFkOp {
                add: false,
                name: removed.name.clone(),
                definition: None,
            }),
            cx,
        );
    }

    /// 设计器：加入一条检查约束草稿。
    pub(crate) fn designer_add_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let name = designer
            .inputs
            .check_name
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let expression = designer
            .inputs
            .check_expr
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if name.is_empty() || expression.is_empty() {
            self.status = "约束名与表达式都不能为空".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        designer
            .draft
            .checks
            .push(rustdb_core::CheckDraft::new(name, expression));
        self.designer_clear_inputs(&["check_name", "check_expr"], window, cx);
        self.status = "已加入检查约束草稿（尚未执行）".into();
        self.status_kind = StatusKind::Ok;
        cx.notify();
    }

    /// 设计器：移除一条检查约束草稿。
    pub(crate) fn designer_drop_check(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        if index >= designer.draft.checks.len() {
            return;
        }
        let removed = designer.draft.checks.remove(index);
        self.status = format!("已移除检查约束 {}", removed.name);
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 设计器：选项页签的「编辑」按钮（在输入框里改，未保存）。
    /// 设计器：把选项页输入框的内容写进草稿。
    pub(crate) fn designer_apply_options(&mut self, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let read =
            |slot: &Option<gpui_kit::Entity<gpui_kit::component::input::InputState>>| -> Option<String> {
                let text = slot
                    .as_ref()
                    .map(|input| input.read(cx).value().trim().to_string())
                    .unwrap_or_default();
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            };
        designer.draft.options.engine = read(&designer.inputs.engine);
        designer.draft.options.collation = read(&designer.inputs.collation);
        designer.draft.options.comment = read(&designer.inputs.comment);
        self.status = "选项已写入草稿".into();
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 设计器：把输入框内容清空（`keys` 是 `DesignerInputs` 的字段名）。
    pub(crate) fn designer_clear_inputs(
        &mut self,
        keys: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        for key in keys {
            let slot = match *key {
                "column_name" => &mut designer.inputs.column_name,
                "column_type" => &mut designer.inputs.column_type,
                "index_name" => &mut designer.inputs.index_name,
                "index_columns" => &mut designer.inputs.index_columns,
                "fk_name" => &mut designer.inputs.fk_name,
                "fk_columns" => &mut designer.inputs.fk_columns,
                "fk_ref_table" => &mut designer.inputs.fk_ref_table,
                "fk_ref_columns" => &mut designer.inputs.fk_ref_columns,
                "check_name" => &mut designer.inputs.check_name,
                "check_expr" => &mut designer.inputs.check_expr,
                _ => continue,
            };
            if let Some(input) = slot.as_ref() {
                let input = input.clone();
                input.update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
    }

    /// 设计器：生成迁移计划（列 / 索引 / 外键 / 检查约束 / 选项）。
    pub(crate) fn designer_plan(&mut self) -> Result<rustdb_core::MigrationPlan, String> {
        let Some(designer) = self.designer.as_ref() else {
            return Err("设计器未打开".into());
        };
        let session = self
            .session
            .clone()
            .ok_or_else(|| "请先连接数据源".to_string())?;
        let kind = self
            .core
            .session_kind(&session)
            .ok_or_else(|| "无法识别连接类型，请重新连接".to_string())?;

        // SQLite 改列 / 改外键 / 改检查约束都只能重建表。
        if designer_needs_rebuild(kind, designer) {
            return Ok(rustdb_core::plan_sqlite_rebuild(
                &designer.path,
                &designer.before,
                &designer.draft.columns,
                &designer.draft.indexes,
                &designer.draft.foreign_keys,
                &designer.draft.checks,
            ));
        }

        let mut plan = rustdb_core::plan_changes(
            kind,
            &designer.path,
            &designer.before,
            &designer.draft.columns,
        );
        for index in &designer.draft.indexes {
            if !designer
                .before_indexes
                .iter()
                .any(|old| !old.primary && old.name == index.name)
            {
                merge_plan(
                    &mut plan,
                    rustdb_core::plan_add_index(kind, &designer.path, index),
                );
            }
        }
        for old in &designer.before_indexes {
            if old.primary {
                continue;
            }
            if !designer
                .draft
                .indexes
                .iter()
                .any(|index| index.name == old.name)
            {
                merge_plan(
                    &mut plan,
                    rustdb_core::plan_drop_index(kind, &designer.path, &old.name),
                );
            }
        }
        for fk in &designer.draft.foreign_keys {
            if !designer
                .before_foreign_keys
                .iter()
                .any(|old| old.name == fk.name)
            {
                merge_plan(
                    &mut plan,
                    rustdb_core::plan_add_foreign_key(kind, &designer.path, fk),
                );
            }
        }
        for old in &designer.before_foreign_keys {
            if !designer
                .draft
                .foreign_keys
                .iter()
                .any(|fk| fk.name == old.name)
            {
                merge_plan(
                    &mut plan,
                    rustdb_core::plan_drop_foreign_key(kind, &designer.path, &old.name),
                );
            }
        }
        for check in &designer.draft.checks {
            if !designer
                .before_checks
                .iter()
                .any(|old| old.name == check.name)
            {
                merge_plan(
                    &mut plan,
                    rustdb_core::plan_add_check(kind, &designer.path, check),
                );
            }
        }
        for old in &designer.before_checks {
            if !designer
                .draft
                .checks
                .iter()
                .any(|check| check.name == old.name)
            {
                merge_plan(
                    &mut plan,
                    rustdb_core::plan_drop_check(kind, &designer.path, &old.name),
                );
            }
        }
        merge_plan(
            &mut plan,
            rustdb_core::plan_table_options(kind, &designer.path, &designer.draft.options),
        );
        Ok(plan)
    }

    /// 设计器：执行一份计划（统一的失败处理与状态提示）。
    ///
    /// `fk_op` 非空表示这是一次「外键逐条保存 / 删除」，完成后要把基线同步过去。
    pub(crate) fn designer_run_plan(
        &mut self,
        plan: rustdb_core::MigrationPlan,
        session: &SessionId,
        title: String,
        fk_op: Option<DesignerFkOp>,
        cx: &mut Context<Self>,
    ) {
        if plan.statements.is_empty() {
            self.status = if plan.unsupported.is_empty() {
                "没有需要执行的改动".into()
            } else {
                plan.unsupported.join("；")
            };
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        match self.core.apply_migration(session, &plan) {
            Ok(query_id) => {
                self.designer_apply_query = Some((query_id, fk_op));
                if let Some(designer) = self.designer.as_mut() {
                    designer.failure = None;
                }
                self.status = format!(
                    "{title} · 执行中 · {} 条 DDL{}",
                    plan.statements.len(),
                    if plan.transactional {
                        "（单事务）"
                    } else {
                        "（该方言 DDL 不可回滚）"
                    }
                );
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                if let Some(designer) = self.designer.as_mut() {
                    designer.failure = Some(DesignerFailure {
                        hint: rustdb_core::recovery_hint(&message),
                        message: message.clone(),
                    });
                }
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    pub(crate) fn designer_preview(&mut self, cx: &mut Context<Self>) {
        // 保存前先跑一遍校验，把问题摆到台面上。
        self.designer_validate(cx);
        match self.designer_plan() {
            Ok(plan) => {
                if plan.is_empty() {
                    self.status = "没有改动".into();
                    self.status_kind = StatusKind::Idle;
                } else {
                    self.edit_preview = Some(plan.preview_text());
                }
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    pub(crate) fn designer_apply(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        // 有阻断性错误时先拦下来，不给数据库发半成品 DDL。
        self.designer_validate(cx);
        let blocking = self
            .designer
            .as_ref()
            .map(|designer| rustdb_core::has_blocking_issue(&designer.issues))
            .unwrap_or(false);
        if blocking {
            self.status = "存在未修正的问题，已阻止执行".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let plan = match self.designer_plan() {
            Ok(plan) => plan,
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
                cx.notify();
                return;
            }
        };
        self.designer_run_plan(plan, &session, "执行设计器改动".into(), None, cx);
    }

    /// 设计器：跑一遍校验，把结论存进状态里。
    pub(crate) fn designer_validate(&mut self, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(kind) = self.core.session_kind(&session) else {
            return;
        };
        let issues = rustdb_core::validate_draft(kind, &designer.draft);
        let errors = issues
            .iter()
            .filter(|issue| issue.level == rustdb_core::IssueLevel::Error)
            .count();
        designer.issues = issues;
        if errors > 0 {
            self.status = format!("{errors} 处问题待修正");
            self.status_kind = StatusKind::Error;
        }
        cx.notify();
    }

    /// 把选中字段同步到下方属性面板（点行 / 增删 / 换列后调用）。
    fn sync_field_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (column, slots) = {
            let Some(designer) = self.designer.as_ref() else {
                return;
            };
            let index = designer.selected_column.min(designer.draft.columns.len().saturating_sub(1));
            (
                designer.draft.columns.get(index).cloned(),
                (
                    designer.inputs.field_name.clone(),
                    designer.inputs.field_type.clone(),
                    designer.inputs.field_default.clone(),
                ),
            )
        };
        let Some(column) = column else {
            return;
        };
        for (slot, value) in [
            (slots.0, column.name.as_str()),
            (slots.1, column.type_text.as_str()),
            (slots.2, column.default.as_deref().unwrap_or("")),
        ] {
            if let Some(input) = slot {
                input.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    /// 点字段网格的行：选中并把属性填进下方面板。
    pub(crate) fn designer_select_column(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(designer) = self.designer.as_mut() {
            if index >= designer.draft.columns.len() {
                return;
            }
            designer.selected_column = index;
        }
        self.sync_field_inputs(window, cx);
        cx.notify();
    }

    /// Navicat「添加字段 / 插入字段」：插入一个可直接改属性的新行。
    pub(crate) fn designer_insert_column(
        &mut self,
        after_selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let mut name = "new_column".to_string();
        let mut suffix = 1;
        while designer
            .draft
            .columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(&name))
        {
            name = format!("new_column_{suffix}");
            suffix += 1;
        }
        let at = if after_selected {
            (designer.selected_column + 1).min(designer.draft.columns.len())
        } else {
            designer.draft.columns.len()
        };
        designer
            .draft
            .columns
            .insert(at, rustdb_core::ColumnDraft::new(name.clone(), "TEXT"));
        designer.selected_column = at;
        self.status = format!("已插入字段 {name}（保存后写入数据库）");
        self.status_kind = StatusKind::Idle;
        self.sync_field_inputs(window, cx);
        cx.notify();
    }

    /// Navicat「删除字段」：从草稿移除选中行（保存后才会下发 DROP）。
    pub(crate) fn designer_delete_selected_column(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let removed = {
            let Some(designer) = self.designer.as_mut() else {
                return;
            };
            if designer.draft.columns.is_empty() {
                return;
            }
            let index = designer.selected_column.min(designer.draft.columns.len() - 1);
            let removed = designer.draft.columns.remove(index);
            designer.selected_column = index.min(designer.draft.columns.len().saturating_sub(1));
            removed
        };
        self.sync_field_inputs(window, cx);
        self.status = format!("已从草稿移除 {}（保存后删列）", removed.name);
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// Navicat「主键」：切换选中字段的键标记（进入迁移计划）。
    pub(crate) fn designer_toggle_column_pk(&mut self, cx: &mut Context<Self>) {
        let message = {
            let Some(designer) = self.designer.as_mut() else {
                return;
            };
            let Some(column) = designer.draft.columns.get_mut(designer.selected_column) else {
                return;
            };
            column.primary_key = !column.primary_key;
            if column.primary_key {
                column.nullable = false;
            }
            format!(
                "{}：主键已{}",
                column.name,
                if column.primary_key { "勾选" } else { "取消" }
            )
        };
        self.status = message;
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 字段网格的「不是 null」勾选框（true = 勾选，不是 null）。
    pub(crate) fn designer_set_column_null(
        &mut self,
        index: usize,
        not_null: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        if let Some(column) = designer.draft.columns.get_mut(index) {
            column.nullable = !not_null;
        }
        self.status = "可空属性已更新（保存后写入数据库）".into();
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// Navicat「上移 / 下移」。
    pub(crate) fn designer_move_selected_column(
        &mut self,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let len = designer.draft.columns.len();
        let from = designer.selected_column;
        let to = (from as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
        if from == to || len < 2 {
            return;
        }
        rustdb_core::reorder_columns(&mut designer.draft.columns, from, to);
        designer.selected_column = to;
        self.status = "字段顺序已调整".into();
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    ///「应用属性」：把下方面板的输入写回选中字段草稿。
    pub(crate) fn designer_apply_field(&mut self, cx: &mut Context<Self>) {
        let (name, type_text, default_text) = {
            let Some(designer) = self.designer.as_ref() else {
                return;
            };
            let read = |slot: &Option<gpui_kit::Entity<gpui_kit::component::input::InputState>>| {
                slot.as_ref()
                    .map(|input| input.read(cx).value().trim().to_string())
                    .unwrap_or_default()
            };
            (
                read(&designer.inputs.field_name),
                read(&designer.inputs.field_type),
                read(&designer.inputs.field_default),
            )
        };
        if name.is_empty() || type_text.is_empty() {
            self.status = "字段名与类型都不能为空".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let index = designer.selected_column;
        if designer
            .draft
            .columns
            .iter()
            .enumerate()
            .any(|(i, column)| i != index && column.name.eq_ignore_ascii_case(&name))
        {
            self.status = format!("字段 {name} 已存在");
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        if let Some(column) = designer.draft.columns.get_mut(index) {
            column.name = name.clone();
            column.type_text = type_text;
            column.default = if default_text.is_empty() {
                None
            } else {
                Some(default_text)
            };
        }
        self.status = format!("字段 {name} 属性已写入草稿");
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 索引 / 外键 / 检查页的工具栏删除：删当前选中行。
    pub(crate) fn designer_delete_selected_row(&mut self, cx: &mut Context<Self>) {
        let tab = self.designer.as_ref().map(|designer| designer.tab);
        let index = match tab {
            Some(rustdb_core::DesignerTab::Indexes) => self
                .designer
                .as_ref()
                .map(|designer| designer.selected_index)
                .unwrap_or(0),
            Some(rustdb_core::DesignerTab::ForeignKeys) => self
                .designer
                .as_ref()
                .map(|designer| designer.selected_fk)
                .unwrap_or(0),
            Some(rustdb_core::DesignerTab::Checks) => self
                .designer
                .as_ref()
                .map(|designer| designer.selected_check)
                .unwrap_or(0),
            _ => return,
        };
        match tab {
            Some(rustdb_core::DesignerTab::Indexes) => self.designer_drop_index(index, cx),
            Some(rustdb_core::DesignerTab::ForeignKeys) => {
                self.designer_remove_foreign_key(index, cx)
            }
            Some(rustdb_core::DesignerTab::Checks) => self.designer_drop_check(index, cx),
            _ => {}
        }
        if let Some(designer) = self.designer.as_mut() {
            match tab {
                Some(rustdb_core::DesignerTab::Indexes) => {
                    designer.selected_index = designer
                        .selected_index
                        .min(designer.draft.indexes.len().saturating_sub(1))
                }
                Some(rustdb_core::DesignerTab::ForeignKeys) => {
                    designer.selected_fk = designer
                        .selected_fk
                        .min(designer.draft.foreign_keys.len().saturating_sub(1))
                }
                Some(rustdb_core::DesignerTab::Checks) => {
                    designer.selected_check = designer
                        .selected_check
                        .min(designer.draft.checks.len().saturating_sub(1))
                }
                _ => {}
            }
        }
        cx.notify();
    }
}
