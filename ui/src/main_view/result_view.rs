//! 结果网格渲染：表头、数据行、空态与分页条。

use super::*;

impl MainView {
    pub(crate) fn render_result_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let preview_note = self.preview_note.clone();
        let has_many = self
            .active()
            .map(|tab| tab.results.len() > 1 || !tab.messages.is_empty())
            .unwrap_or(false);
        let tabs = has_many.then(|| self.render_result_tabs(cx));

        // 消息页优先：用户显式切过去后再渲染结果网格会让位给消息列表。
        if self.active().map(|tab| tab.show_messages).unwrap_or(false) {
            let messages = self.render_messages(cx);
            return v_flex()
                .flex_1()
                .min_h_0()
                .when_some(tabs, |layout, tabs| layout.child(tabs))
                .child(messages)
                .into_any_element();
        }
        // 表格变窄（换标签页 / 缩列宽）后偏移可能越界，渲染前先夹一次。
        let max_h = self.max_grid_h();
        if self.grid_h > max_h {
            self.grid_h = max_h;
        }
        let grid_h = self.grid_h;
        let active_result = self.active().map(|tab| tab.active_result).unwrap_or(0);
        // 编辑条要在借用 result 之前构建（它读的是 MainView 自身状态）。
        let edit_bar = self.render_edit_bar(cx);
        let Some(result) = self.active().and_then(|tab| tab.results.get(active_result)) else {
            return v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::Database).text_color(hex(t.fg_muted)))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(hex(t.fg_muted))
                        .child("执行 SQL 后显示结果"),
                )
                .into_any_element();
        };
        let selected_range = selection_range(self.grid_selection, result);
        let selection_bg = hex(t.accent).opacity(0.24);
        if result.columns.is_empty() {
            let message = match result.affected {
                Some(n) => format!("语句已执行 · 影响 {n} 行"),
                None => "语句已执行".to_string(),
            };
            return v_flex()
                .flex_1()
                .min_h_0()
                .when_some(tabs, |layout, tabs| layout.child(tabs))
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(Icon::new(IconName::Check).text_color(hex(t.success)))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(hex(t.fg_secondary))
                                .child(SharedString::from(message)),
                        ),
                )
                .child(
                    h_flex()
                        .h(px(23.))
                        .w_full()
                        .px_2()
                        .items_center()
                        .border_t_1()
                        .border_color(hex(t.border_subtle))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(hex(t.fg_muted))
                                .child(SharedString::from(format!("{} ms", result.elapsed_ms))),
                        ),
                )
                .into_any_element();
        }

        // 表头：可点击排序的内容区 + 右侧 6px 拖拽把手，两者合计即列宽。
        // 未拖过的列用 flex_1 均分剩余空间（min_w 保证列多时撑出横向滚动）。
        let mut header = h_flex()
            .h(px(27.))
            .w_full()
            .flex_none()
            .ml(px(-grid_h))
            .bg(hex(t.grid_header_bg));
        for (index, column) in result.columns.iter().enumerate() {
            let marker = match result.sort {
                Some(sort) if sort.column == index => {
                    if sort.ascending {
                        " ↑"
                    } else {
                        " ↓"
                    }
                }
                _ => "",
            };
            let is_new_value = column.name.ends_with("(new)");
            let fixed = result.fixed_width(index);
            let cell_w = fixed
                .map(|width| width - COL_HANDLE_W - COL_FILTER_W)
                .unwrap_or(0.);
            let title = h_flex()
                .id(SharedString::from(format!("header-{index}")))
                .debug_selector({
                    let selector = format!("header-{index}");
                    move || selector.clone()
                })
                .h_full()
                .px_2()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .border_b_1()
                .border_color(hex(t.border_subtle))
                .hover(|s| s.bg(hex(t.bg_overlay)))
                .on_click(
                    cx.listener(move |view, _, window, cx| view.toggle_sort(index, window, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(hex(if is_new_value { t.accent } else { t.fg_primary }))
                        .text_ellipsis()
                        .child(SharedString::from(format!("{}{marker}", column.name))),
                );
            let title = match fixed {
                Some(_) => title.w(px(cell_w)).flex_shrink_0(),
                None => title
                    .flex_1()
                    .min_w(px(MIN_COL_W - COL_FILTER_W))
                    .max_w(px(MAX_COL_W)),
            };
            // 筛选按钮：有筛选条件时高亮。
            let filtering = result.filter_of(index).is_some();
            header = header
                .child(title)
                .child(
                    div()
                        .id(SharedString::from(format!("col-filter-{index}")))
                        .debug_selector({
                            let selector = format!("col-filter-{index}");
                            move || selector.clone()
                        })
                        .w(px(COL_FILTER_W))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .border_b_1()
                        .border_color(hex(t.border_subtle))
                        .hover(|s| s.bg(hex(t.bg_overlay)))
                        .on_click(
                            cx.listener(move |view, event: &gpui::ClickEvent, window, cx| {
                                cx.stop_propagation();
                                view.open_filter(index, event.position(), window, cx);
                            }),
                        )
                        .child(
                            Icon::new(IconName::Funnel)
                                .small()
                                .text_color(hex(if filtering { t.accent } else { t.fg_muted })),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("col-handle-{index}")))
                        .debug_selector({
                            let selector = format!("col-handle-{index}");
                            move || selector.clone()
                        })
                        .w(px(COL_HANDLE_W))
                        .h_full()
                        .flex()
                        .justify_end()
                        .cursor(gpui::CursorStyle::ResizeLeftRight)
                        .border_b_1()
                        .border_color(hex(t.border_subtle))
                        .hover(|s| s.bg(hex(t.bg_overlay)))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                                // 双击分隔线自适应列宽，单击进入拖拽。
                                if event.click_count >= 2 {
                                    view.fit_column(index, window, cx);
                                } else {
                                    view.begin_col_resize(index, event.position.x, cx);
                                }
                            }),
                        )
                        // 拖出网格再松手时，靠这个收尾。
                        .on_mouse_up_out(
                            gpui::MouseButton::Left,
                            cx.listener(|view, _, _, cx| view.end_col_resize(cx)),
                        )
                        .child(div().w(px(1.)).h_full().bg(hex(t.border_subtle))),
                );
        }

        // 行体：等行高虚拟列表，只渲染可视区附近的行；横向滚动交给外层，表头一起动。
        let body = uniform_list(
            "result-rows",
            result.rows.len(),
            cx.processor(move |view, range: std::ops::Range<usize>, _window, cx| {
                let Some(tab) = view.tabs.get(view.active_tab) else {
                    return Vec::new();
                };
                let Some(result) = tab.results.get(tab.active_result) else {
                    return Vec::new();
                };
                let order = result.ordered_row_indices();
                let offset = view.grid_h;
                let widths: Vec<Option<f32>> = (0..result.columns.len())
                    .map(|index| result.fixed_width(index))
                    .collect();

                let mut lines = Vec::with_capacity(range.len());
                for position in range {
                    let Some(&row_index) = order.get(position) else {
                        break;
                    };
                    let change = result.change_of(row_index);
                    let bg = match change {
                        Some(rustdb_core::ChangeKind::Insert) => hex(t.diff_add),
                        Some(rustdb_core::ChangeKind::Delete) => hex(t.diff_del),
                        Some(rustdb_core::ChangeKind::Update) => hex(t.bg_overlay),
                        None if position % 2 == 1 => hex(t.grid_row_alt),
                        None => hex(t.bg_base),
                    };
                    let mut line = h_flex()
                        .h(px(ROW_H))
                        .w_full()
                        .min_w_0()
                        .ml(px(-offset))
                        .bg(bg);
                    for (col, cell_w) in widths.iter().enumerate() {
                        let value = result.rows.get(row_index).and_then(|row| row.get(col));
                        // 交回数据库默认值的单元格显示「(默认)」而不是 NULL。
                        let default_cell = result.cell_default(row_index, col);
                        let null = !default_cell && value.map(Value::is_null).unwrap_or(true);
                        let text = if default_cell {
                            "(默认)".to_string()
                        } else {
                            value.map(Value::display_text).unwrap_or_default()
                        };
                        let copy_text = value.map(Value::export_text).unwrap_or_default();
                        // 双击看详情：把整行整列的值带出去，避免闭包只拿到渲染文本。
                        let detail_value = value.cloned();
                        let detail_column = result
                            .columns
                            .get(col)
                            .map(|column| column.name.to_string())
                            .unwrap_or_default();
                        let detail_meta = result.columns.get(col).cloned();
                        let editable_cell = result.editable.is_some();
                        // 改过的单元格用警告色标出来（与行底色一起表达「未提交」）。
                        let dirty_cell = result.cell_dirty(row_index, col);
                        let selected = selected_range
                            .map(|range| {
                                position >= range.top
                                    && position <= range.bottom
                                    && col >= range.left
                                    && col <= range.right
                            })
                            .unwrap_or(false);
                        let cell = div()
                            .id(SharedString::from(format!("cell-{row_index}-{col}")))
                            .debug_selector({
                                let selector = format!("cell-{row_index}-{col}");
                                move || selector.clone()
                            })
                            .h_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .border_r_1()
                            .border_b_1()
                            .border_color(hex(t.border_subtle))
                            .text_size(px(11.))
                            .text_color(hex(if null { t.fg_muted } else { t.fg_primary }))
                            .when(null, |d| d.italic())
                            .text_ellipsis()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .when(selected, |d| d.bg(selection_bg))
                            // 按下开始拖拽选区；Shift 点击在 click 里扩选，避免覆盖锚点。
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                                    view.begin_grid_drag(row_index, col, event, cx);
                                }),
                            )
                            .on_mouse_move(cx.listener(move |view, _, _, cx| {
                                view.extend_grid_drag(row_index, col, cx);
                            }))
                            // 单击复制；Shift 点击扩选；双击进入编辑或打开详情查看器。
                            .on_click(cx.listener(
                                move |view, event: &gpui::ClickEvent, window, cx| {
                                    if view.finish_grid_click() {
                                        cx.notify();
                                        return;
                                    }
                                    let (double, shift) = match event {
                                        gpui::ClickEvent::Mouse(mouse) => (
                                            mouse.down.click_count >= 2,
                                            mouse.down.modifiers.shift,
                                        ),
                                        _ => (false, false),
                                    };
                                    if double && editable_cell {
                                        view.begin_cell_edit(row_index, col, window, cx);
                                        return;
                                    }
                                    if double {
                                        if let Some(value) = detail_value.clone() {
                                            view.cell_detail = Some(CellDetail::new(
                                                detail_column.clone(),
                                                detail_meta.as_ref(),
                                                value,
                                            ));
                                            cx.notify();
                                        }
                                        return;
                                    }
                                    view.select_grid_cell(row_index, col, shift, cx);
                                    if shift {
                                        return;
                                    }
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy_text.clone(),
                                    ));
                                    view.status = "已复制".into();
                                    view.status_kind = StatusKind::Ok;
                                    cx.notify();
                                },
                            ))
                            .when(dirty_cell, |d| d.text_color(hex(t.warning)))
                            .child(SharedString::from(text));
                        let cell = match cell_w {
                            Some(width) => cell.w(px(*width)).flex_shrink_0(),
                            None => cell.flex_1().min_w(px(MIN_COL_W)).max_w(px(MAX_COL_W)),
                        };
                        line = line.child(cell);
                    }
                    lines.push(line);
                }
                lines
            }),
        )
        .flex_1()
        .min_h_0()
        .h(px(400.));

        let selection = selected_range.filter(GridRange::is_multi_cell);
        let page_tab_id = self.active().map(|tab| tab.id).unwrap_or_default();
        let footer = h_flex()
            .h(px(23.))
            .w_full()
            .px_2()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(hex(t.border_subtle))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(format!(
                        "{} · {} ms{}{}",
                        if result.filter_of_all().is_empty() {
                            format!("已加载 {} 行", result.loaded)
                        } else {
                            format!("显示 {} / {} 行", result.visible_row_count(), result.loaded)
                        },
                        result.elapsed_ms,
                        if result.truncated {
                            " · 仅显示前 300 行"
                        } else {
                            ""
                        },
                        if result.filter_of_all().is_empty() {
                            ""
                        } else {
                            " · 已筛选"
                        }
                    ))),
            )
            // 多格选区：显示行列数与复制入口（单格仍走点击复制）。
            .when_some(selection, |row, range| {
                row.child(div().text_size(px(10.)).text_color(hex(t.accent)).child(
                    SharedString::from(format!("已选 {} 行 × {} 列", range.rows(), range.cols())),
                ))
                .child(
                    div().debug_selector(|| "copy-selection".to_string()).child(
                        Button::new("copy-selection-btn")
                            .small()
                            .ghost()
                            .label("复制选区")
                            .tooltip("复制为 TSV")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.copy_result_as(ExportFormat::Tsv, cx);
                            })),
                    ),
                )
            })
            // 选中单元格后可从这里打开详情（双击之外的第二个入口）。
            // 放在行数信息右侧而非最右端，避免贴住 AI 栏导致难点中。
            .child(
                div()
                    .debug_selector(|| "cell-detail-button".to_string())
                    .child(
                        Button::new("cell-detail-btn")
                            .small()
                            .ghost()
                            .label("详情")
                            .tooltip("查看完整内容")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.open_selected_cell_detail(cx);
                            })),
                    ),
            )
            // 变更集：行操作 / 删行（仅可编辑结果集）
            .when(result.editable.is_some(), |row| {
                row.child(
                    div().debug_selector(|| "edit-row-menu".to_string()).child(
                        Button::new("edit-row-menu-btn")
                            .small()
                            .ghost()
                            .label("行操作")
                            .icon(IconName::ChevronDown)
                            .tooltip("行编辑")
                            .on_click(cx.listener(|view, event, window, cx| {
                                view.open_row_menu(event, window, cx);
                            })),
                    ),
                )
                .child(
                    div().debug_selector(|| "edit-delete".to_string()).child(
                        Button::new("edit-delete-btn")
                            .small()
                            .ghost()
                            .label("删行")
                            .tooltip("标记删除")
                            .on_click(cx.listener(|view, _, _, cx| view.delete_selected_row(cx))),
                    ),
                )
            })
            // F-73：外键跳转（选中单元格所在列属于外键时跳到被引用记录）。
            .when(
                result.editable.is_some()
                    && self
                        .capabilities
                        .as_ref()
                        .is_some_and(|caps| caps.foreign_key_pivot),
                |row| {
                    row.child(
                        div().debug_selector(|| "fk-jump".to_string()).child(
                            Button::new("fk-jump-btn")
                                .small()
                                .ghost()
                                .label("外键")
                                .tooltip("跳转外键")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.foreign_key_jump(window, cx);
                                })),
                        ),
                    )
                },
            )
            // F-73：按条件批量更新（未加载行也能按 WHERE 命中）。
            .when(result.editable.is_some(), |row| {
                row.child(
                    div().debug_selector(|| "bulk-open".to_string()).child(
                        Button::new("bulk-open-btn")
                            .small()
                            .ghost()
                            .label("批量更新")
                            .tooltip("按条件更新")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_bulk_panel(window, cx);
                            })),
                    ),
                )
            })
            // 待提交变更：预览 / 提交 / 回滚
            .when(result.change_count() > 0, |row| {
                let count = result.change_count();
                row.child(
                    div()
                        .debug_selector(|| "edit-pending".to_string())
                        .text_size(px(10.))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(format!("待提交 {count} 条"))),
                )
                .child(
                    div().debug_selector(|| "edit-preview".to_string()).child(
                        Button::new("edit-preview-btn")
                            .small()
                            .ghost()
                            .label("变更 SQL")
                            .on_click(cx.listener(|view, _, _, cx| view.preview_changes(cx))),
                    ),
                )
                .child(
                    div().debug_selector(|| "edit-apply".to_string()).child(
                        Button::new("edit-apply-btn")
                            .small()
                            .label(format!("写入 ({count})"))
                            .on_click(cx.listener(|view, _, _, cx| view.apply_changes(cx))),
                    ),
                )
                .child(
                    div().debug_selector(|| "edit-revert".to_string()).child(
                        Button::new("edit-revert-btn")
                            .small()
                            .ghost()
                            .label("撤销")
                            .on_click(cx.listener(|view, _, _, cx| view.revert_changes(cx))),
                    ),
                )
            })
            // F-70：服务端分页导航（仅分页取数的结果集出现）。
            .when_some(result.page.clone(), |row, page| {
                let next_size = PAGE_SIZE_CHOICES
                    .iter()
                    .copied()
                    .find(|size| *size > page.page_size)
                    .unwrap_or(PAGE_SIZE_CHOICES[0]);
                let path = page.path.clone();
                let order = page.order.clone();
                row.when(page.page > 1, |row| {
                    let path = path.clone();
                    let order = order.clone();
                    row.child(
                        div().debug_selector(|| "page-prev".to_string()).child(
                            Button::new("page-prev-btn")
                                .small()
                                .ghost()
                                .label("‹")
                                .tooltip("上一页")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.reload_page(
                                        page_tab_id,
                                        path.clone(),
                                        page.page - 1,
                                        page.page_size,
                                        order.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                    )
                })
                .child(
                    div()
                        .debug_selector(|| "page-label".to_string())
                        .text_size(px(10.))
                        .text_color(hex(t.fg_muted))
                        .child(SharedString::from(format!(
                            "第 {} 页 · {} 行/页",
                            page.page, page.page_size
                        ))),
                )
                .when(page.has_more, |row| {
                    let path = path.clone();
                    let order = order.clone();
                    row.child(
                        div().debug_selector(|| "page-next".to_string()).child(
                            Button::new("page-next-btn")
                                .small()
                                .ghost()
                                .label("›")
                                .tooltip("下一页")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.reload_page(
                                        page_tab_id,
                                        path.clone(),
                                        page.page + 1,
                                        page.page_size,
                                        order.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                    )
                })
                .child(
                    div().debug_selector(|| "page-size".to_string()).child(
                        Button::new("page-size-btn")
                            .small()
                            .ghost()
                            .label(SharedString::from(format!("{} 行/页", next_size)))
                            .tooltip("切换每页行数")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.reload_page(
                                    page_tab_id,
                                    path.clone(),
                                    1,
                                    next_size,
                                    order.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    ),
                )
            })
            .child(div().flex_1());

        v_flex()
            .flex_1()
            .min_h_0()
            .when_some(preview_note, |layout, note| {
                layout.child(
                    h_flex()
                        .debug_selector(|| "preview-note".to_string())
                        .h(px(22.))
                        .w_full()
                        .flex_none()
                        .px_2()
                        .gap_1()
                        .items_center()
                        .border_b_1()
                        .border_color(hex(t.border_subtle))
                        .bg(hex(t.bg_elevated))
                        .child(Icon::new(IconName::Eye).small().text_color(hex(t.accent)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(10.))
                                .text_color(hex(t.fg_secondary))
                                .text_ellipsis()
                                .child(SharedString::from(note)),
                        ),
                )
            })
            .when_some(tabs, |layout, tabs| layout.child(tabs))
            .child(
                v_flex()
                    .id("result-scroll")
                    .debug_selector(|| "result-scroll".to_string())
                    .flex_1()
                    .min_h_0()
                    .on_scroll_wheel(cx.listener(|view, event: &gpui::ScrollWheelEvent, _, cx| {
                        view.scroll_grid_h(event, cx);
                    }))
                    .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                        view.drag_col_resize(event, cx);
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|view, _, _, cx| view.end_col_resize(cx)),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|view, _, _, cx| view.end_col_resize(cx)),
                    )
                    // 表头：裁剪 + 横向位移，竖向始终贴在顶部。
                    .child(
                        div()
                            .id("grid-head")
                            .flex_none()
                            .h(px(27.))
                            .overflow_hidden()
                            .child(header),
                    )
                    // 行体：虚拟列表自带竖向滚动，只把它量一量宽度用于横向夹取。
                    .child(
                        div()
                            .id("grid-body")
                            .flex_1()
                            .min_h_0()
                            .track_scroll(&self.grid_viewport)
                            .child(body),
                    ),
            )
            .child(edit_bar)
            .child(footer)
            .into_any_element()
    }
}
