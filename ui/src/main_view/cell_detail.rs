//! 单元格详情面板与列筛选菜单。

use super::*;

impl MainView {
    /// 打开当前选中单元格的详情（底栏「详情」按钮）。
    pub(crate) fn open_selected_cell_detail(&mut self, cx: &mut Context<Self>) {
        let Some((row, col)) = self.selected_cell else {
            self.status = "请先点击一个单元格".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let Some(result) = self.active_result() else {
            return;
        };
        let Some(value) = result.rows.get(row).and_then(|line| line.get(col)) else {
            return;
        };
        let column = result
            .columns
            .get(col)
            .map(|column| column.name.to_string())
            .unwrap_or_default();
        let meta = result.columns.get(col).cloned();
        self.cell_detail = Some(CellDetail::new(column, meta.as_ref(), value.clone()));
        cx.notify();
    }

    /// 打开某列的筛选面板（面板里输入即时过滤）。
    pub(crate) fn open_filter(
        &mut self,
        index: usize,
        at: Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_menu = Some((index, at));
        // 复用同一个输入实体，打开时回填该列已有条件。
        if let Some(input) = self.filter_input.clone() {
            let existing = self
                .active_result()
                .and_then(|result| result.filter_of(index).map(str::to_string))
                .unwrap_or_default();
            input.update(cx, |state, cx| state.set_value(existing, window, cx));
        }
        cx.notify();
    }

    /// 筛选面板：输入框 + 清除按钮。
    pub(crate) fn render_filter_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some((index, _)) = self.filter_menu else {
            return div().into_any_element();
        };
        let column = self
            .active_result()
            .and_then(|result| result.columns.get(index).map(|c| c.name.to_string()))
            .unwrap_or_default();

        if self.filter_input.is_none() {
            let input = cx
                .new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("包含…"));
            let entity = input.clone();
            cx.subscribe(&input, move |view, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    let text = entity.read(cx).value().to_string();
                    if let Some((index, _)) = view.filter_menu {
                        if let Some(tab) = view.tabs.get_mut(view.active_tab) {
                            if let Some(result) = tab.results.get_mut(tab.active_result) {
                                result.set_filter(index, Some(text));
                            }
                        }
                        view.clear_grid_selection();
                        cx.notify();
                    }
                }
            })
            .detach();
            self.filter_input = Some(input);
        }

        v_flex()
            .id("filter-menu")
            .debug_selector(|| "filter-menu".to_string())
            .w(px(220.))
            .p_2()
            .gap_2()
            .rounded(px(6.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(format!("筛选「{column}」"))),
            )
            .child(div().debug_selector(|| "filter-input".to_string()).child(
                gpui_component::input::Input::new(
                    self.filter_input.as_ref().expect("筛选输入框已创建"),
                ),
            ))
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .id("filter-clear")
                            .debug_selector(|| "filter-clear".to_string())
                            .px_2()
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if let Some(tab) = view.tabs.get_mut(view.active_tab) {
                                    if let Some(result) = tab.results.get_mut(tab.active_result) {
                                        result.set_filter(index, None);
                                    }
                                }
                                view.clear_grid_selection();
                                if let Some(input) = view.filter_input.clone() {
                                    input.update(cx, |state, cx| state.set_value("", window, cx));
                                }
                                cx.notify();
                            }))
                            .child("清除"),
                    )
                    .child(
                        div()
                            .id("filter-done")
                            .debug_selector(|| "filter-done".to_string())
                            .px_2()
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.accent))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.filter_menu = None;
                                cx.notify();
                            }))
                            .child("完成"),
                    ),
            )
            .into_any_element()
    }

    /// 单元格详情查看器：长文本全文、JSON 格式化、二进制十六进制。
    pub(crate) fn render_cell_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(detail) = self.cell_detail.clone() else {
            return div().into_any_element();
        };
        let is_json = matches!(&detail.value, Value::Json(_));
        // JSON 路径输入按需创建一次。
        if is_json && self.detail_input.is_none() {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("路径，如 /a/b/0")
            });
            cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    let text = view
                        .detail_input
                        .as_ref()
                        .map(|input| input.read(cx).value().to_string())
                        .unwrap_or_default();
                    if let Some(detail) = view.cell_detail.as_mut() {
                        detail.pointer = text;
                    }
                    cx.notify();
                }
            })
            .detach();
            self.detail_input = Some(input);
        }

        let body = crate::detail::render_body(&detail.value, detail.mode, &detail.pointer);
        let notes = crate::detail::notes(&detail.value);
        let modes = crate::detail::modes(&detail.value);
        let size_label = if detail.size >= 1024 {
            format!("{:.1} KB", detail.size as f64 / 1024.)
        } else {
            format!("{} B", detail.size)
        };
        let null_label = if detail.nullable { "可空" } else { "非空" };
        let copy_body = body.clone();

        let mode_buttons = modes.iter().map(|mode| {
            let mode = *mode;
            let key = mode.key();
            let active = detail.mode == mode;
            div()
                .id(SharedString::from(format!("cell-detail-mode-{key}")))
                .debug_selector(move || format!("cell-detail-mode-{key}"))
                .px_2()
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |el| el.bg(hex(t.bg_overlay)))
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(if active { t.accent } else { t.fg_secondary }))
                .hover(|s| s.bg(hex(t.bg_overlay)))
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(detail) = view.cell_detail.as_mut() {
                        detail.mode = mode;
                    }
                    cx.notify();
                }))
                .child(mode.label())
        });

        // 短值不需要整块大视口：一行文本配一块空面板既浪费也更难读。
        let panel_height = if is_json {
            460.
        } else if detail.size <= 64 {
            140.
        } else if detail.size <= 160 {
            200.
        } else {
            460.
        };

        v_flex()
            .id("cell-detail")
            .debug_selector(|| "cell-detail".to_string())
            .occlude()
            .w(px(560.))
            .h(px(panel_height))
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(34.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        Icon::new(IconName::ScanEye)
                            .small()
                            .text_color(hex(t.accent)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_ellipsis()
                            .child(SharedString::from(format!(
                                "{} · {} · {size_label}",
                                detail.column, detail.native_type
                            ))),
                    )
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(format!("{null_label} · 只读"))),
                    )
                    .child(
                        div()
                            .id("cell-detail-copy")
                            .debug_selector(|| "cell-detail-copy".to_string())
                            .px_2()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_body.clone()));
                                view.status = "已复制".into();
                                view.status_kind = StatusKind::Ok;
                                cx.notify();
                            }))
                            .child("复制"),
                    )
                    .child(
                        div()
                            .id("cell-detail-close")
                            .debug_selector(|| "cell-detail-close".to_string())
                            .w(px(18.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.cell_detail = None;
                                cx.notify();
                            }))
                            .child(
                                Icon::new(IconName::Close)
                                    .small()
                                    .text_color(hex(t.fg_muted)),
                            ),
                    ),
            )
            .when(!modes.is_empty() || is_json, |el| {
                el.child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(hex(t.border_subtle))
                        .children(mode_buttons)
                        .when(is_json, |row| {
                            row.child(
                                div()
                                    .id("cell-detail-pointer")
                                    .debug_selector(|| "cell-detail-pointer".to_string())
                                    .flex_1()
                                    .min_w_0()
                                    .child(gpui_component::input::Input::new(
                                        self.detail_input.as_ref().expect("路径输入已创建"),
                                    )),
                            )
                        }),
                )
            })
            .when(!notes.is_empty(), |el| {
                el.child(
                    v_flex()
                        .px_3()
                        .pt_2()
                        .gap(px(2.))
                        .children(notes.into_iter().map(|note| {
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .child(SharedString::from(note))
                        })),
                )
            })
            .child(
                div()
                    .id("cell-detail-body")
                    .debug_selector(|| "cell-detail-body".to_string())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .p_3()
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .whitespace_normal()
                            .child(SharedString::from(body)),
                    ),
            )
            .into_any_element()
    }
}
