//! 表设计器：列 / 索引 / 外键 / 检查 / 触发器 / 选项与 DDL 计划。

use super::*;

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

    /// 表设计器面板：六个页签（列 / 索引 / 外键 / 检查约束 / 触发器 / 选项）+ DDL 预览与执行。
    pub(crate) fn render_table_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some((label, detail)) = self.table_panel.clone() else {
            return div().into_any_element();
        };
        // 输入框按当前页签懒创建（需要 window）。
        let tab = self.designer.as_ref().map(|designer| designer.tab);
        if let Some(designer) = self.designer.as_mut() {
            let options = designer.draft.options.clone();
            designer.inputs.ensure(
                tab.unwrap_or(rustdb_core::DesignerTab::Columns),
                &options,
                window,
                cx,
            );
        }

        let tabs_bar = self.render_designer_tabs(cx);
        let body = match tab {
            Some(rustdb_core::DesignerTab::Columns) => self.render_designer_columns(cx),
            Some(rustdb_core::DesignerTab::Indexes) => self.render_designer_indexes(cx),
            Some(rustdb_core::DesignerTab::ForeignKeys) => self.render_designer_foreign_keys(cx),
            Some(rustdb_core::DesignerTab::Checks) => self.render_designer_checks(cx),
            Some(rustdb_core::DesignerTab::Triggers) => self.render_designer_triggers(cx),
            Some(rustdb_core::DesignerTab::Options) => self.render_designer_options(cx),
            None => self.render_structure_body(&detail, cx),
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
        let tab_count = rustdb_core::DesignerTab::ALL.len();

        v_flex()
            .id("table-panel")
            .debug_selector(|| "table-panel".to_string())
            .w(px(680.))
            .h(px(480.))
            .rounded(px(12.))
            .border_1()
            .border_color(hex(t.border_strong))
            .bg(hex(t.bg_elevated))
            .occlude()
            .overflow_hidden()
            // 吃掉面板内的点击，避免穿透到遮罩把面板关掉。
            .on_click(cx.listener(|_, _, _, _| {}))
            .child(
                h_flex()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        div()
                            .w(px(28.))
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.))
                            .bg(crate::theme::to_hsla(t.accent).opacity(0.14))
                            .child(Icon::new(IconName::Table).small().text_color(hex(t.accent))),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
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
                                        "表结构 · {column_count} 列 · {tab_count} 个页签"
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .id("table-panel-close")
                            .debug_selector(|| "table-panel-close".to_string())
                            .w(px(28.))
                            .h(px(28.))
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .py_2()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(body),
            )
            .child(self.render_designer_footer(issues, failure, rebuild, cx))
            .into_any_element()
    }

    /// 设计器页签栏：复用全局 chip 组件做分段控件（选中 primary、其余 ghost）。
    pub(crate) fn render_designer_tabs(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let active = self
            .designer
            .as_ref()
            .map(|designer| designer.tab)
            .unwrap_or(rustdb_core::DesignerTab::Columns);
        let mut row = h_flex()
            .h(px(38.))
            .flex_shrink_0()
            .px_3()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(hex(current(cx).border_subtle));
        for tab in rustdb_core::DesignerTab::ALL {
            let selector = format!("designer-tab-{}", tab.label());
            let label = tab.label();
            row = row.child(crate::design::chip(
                selector,
                label,
                tab == active,
                cx.listener(move |view, _, _, cx| {
                    if let Some(designer) = view.designer.as_mut() {
                        designer.tab = tab;
                    }
                    cx.notify();
                }),
            ));
        }
        row.into_any_element()
    }

    /// 设计器底栏：校验结论 / 失败明细 + 预览、执行（右对齐主次按钮）。
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
                vec!["先预览，再执行".to_string()],
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
            .h(px(46.))
            .flex_shrink_0()
            .px_4()
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
                                    .label("预览 DDL")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.designer_preview(cx);
                                    })),
                            ),
                    )
                    .child(
                        div().debug_selector(|| "designer-apply".to_string()).child(
                            Button::new("designer-apply-btn")
                                .small()
                                .label("执行")
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

    /// 设计器「列」页签：草稿可增删、可拖拽排序。
    pub(crate) fn render_designer_columns(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(designer) = self.designer.as_ref() else {
            return div().into_any_element();
        };
        let before_names: Vec<String> = designer
            .before
            .iter()
            .map(|column| column.meta.name.to_string())
            .collect();
        let columns = designer.draft.columns.clone();

        let mut list = v_flex().gap(px(2.));
        for (index, draft) in columns.iter().enumerate() {
            let name = draft.name.clone();
            let type_name = draft.type_text.clone();
            let is_new = !before_names.iter().any(|n| n.eq_ignore_ascii_case(&name));
            let mut flags: Vec<gpui_kit::AnyElement> = Vec::new();
            if !draft.nullable {
                flags.push(
                    gpui_kit::component::tag::Tag::warning().small()
                        .child("NOT NULL")
                        .into_any_element(),
                );
            }
            if let Some(default) = draft.default.as_deref() {
                flags.push(
                    gpui_kit::component::tag::Tag::secondary().small()
                        .child(SharedString::from(default.to_string()))
                        .into_any_element(),
                );
            }
            if is_new {
                flags.push(
                    gpui_kit::component::tag::Tag::success().small()
                        .child("新增")
                        .into_any_element(),
                );
            }
            let drag_label = name.clone();
            let weak = cx.weak_entity();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("designer-column-{index}")))
                    .debug_selector({
                        let selector = format!("designer-column-{index}");
                        move || selector.clone()
                    })
                    .h(px(28.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    // 拖放回调只给 `&mut App`（没有 `cx.listener`），靠弱引用回写视图。
                    .on_drop::<ColumnDrag>(move |drag, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            view.designer_move_column(drag.index, index, cx);
                        });
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("designer-grip-{index}")))
                            .debug_selector({
                                let selector = format!("designer-grip-{index}");
                                move || selector.clone()
                            })
                            .w(px(14.))
                            .h(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_move()
                            .on_drag(
                                ColumnDrag {
                                    index,
                                    label: drag_label.clone(),
                                },
                                |drag, _, _, cx| {
                                    cx.new(|_| ColumnDrag {
                                        index: drag.index,
                                        label: drag.label.clone(),
                                    })
                                },
                            )
                            .child(
                                Icon::new(IconName::GripVertical)
                                    .small()
                                    .text_color(hex(t.fg_muted)),
                            ),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(type_name)),
                    )
                    .child(h_flex().flex_1().min_w_0().gap_1().children(flags))
                    .child(
                        div()
                            .id(SharedString::from(format!("designer-drop-{index}")))
                            .debug_selector({
                                let selector = format!("designer-drop-{index}");
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
                                view.designer_drop_column(index, cx);
                            }))
                            .child(Icon::new(IconName::Close).small().text_color(hex(t.danger))),
                    ),
            );
        }

        let mut add_row = h_flex().h(px(32.)).px_3().gap_2().items_center();
        if let Some(designer) = self.designer.as_ref() {
            if let (Some(name_input), Some(type_input)) = (
                designer.inputs.column_name.clone(),
                designer.inputs.column_type.clone(),
            ) {
                add_row = add_row
                    .child(
                        div()
                            .w(px(140.))
                            .child(gpui_kit::component::input::Input::new(&name_input).small()),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .child(gpui_kit::component::input::Input::new(&type_input).small()),
                    );
            }
        }
        add_row = add_row
            .child(
                div().debug_selector(|| "designer-add".to_string()).child(
                    Button::new("designer-add-btn")
                        .small()
                        .label("添加列")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.designer_add_column(window, cx);
                        })),
                ),
            )
            .child(div().flex_1());

        v_flex()
            .child(Self::designer_section("列", "拖动左侧手柄调整顺序", cx))
            .child(
                h_flex()
                    .h(px(26.))
                    .px_3()
                    .bg(hex(t.bg_overlay))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(div().w(px(14.)))
                    .child(div().w(px(140.)).child("列名"))
                    .child(div().w(px(120.)).child("类型"))
                    .child(div().flex_1().child("约束"))
                    .child(div().w(px(18.))),
            )
            .child(list)
            .child(add_row)
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

    /// 设计器：把输入框里的「列名 + 类型」加入草稿。
    pub(crate) fn designer_add_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        let name = designer
            .inputs
            .column_name
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let type_text = designer
            .inputs
            .column_type
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if name.is_empty() || type_text.is_empty() {
            self.status = "列名与类型都不能为空".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        if designer
            .draft
            .columns
            .iter()
            .any(|draft| draft.name.eq_ignore_ascii_case(&name))
        {
            self.status = format!("列 {name} 已存在");
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        designer
            .draft
            .columns
            .push(rustdb_core::ColumnDraft::new(name, type_text));
        self.designer_clear_inputs(&["column_name", "column_type"], window, cx);
        self.status = "已加入草稿（尚未执行）".into();
        self.status_kind = StatusKind::Ok;
        cx.notify();
    }

    /// 设计器：从草稿里移除一列（现状列会在迁移计划里变成 DROP）。
    pub(crate) fn designer_drop_column(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        if index >= designer.draft.columns.len() {
            return;
        }
        let removed = designer.draft.columns.remove(index);
        self.status = format!("已在草稿中移除 {}（执行后会删列）", removed.name);
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 设计器：拖拽排序（把 from 位置的列挪到 to）。
    pub(crate) fn designer_move_column(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(designer) = self.designer.as_mut() else {
            return;
        };
        if from == to {
            return;
        }
        rustdb_core::reorder_columns(&mut designer.draft.columns, from, to);
        self.status = "已调整列顺序".into();
        self.status_kind = StatusKind::Idle;
        cx.notify();
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
}
