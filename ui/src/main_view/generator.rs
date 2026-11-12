//! 行生成器与临时行撤销。

use super::*;

impl MainView {
    /// 撤销末尾未填写的新增行（填过内容的行不自动丢弃）。
    pub(crate) fn undo_last_new_row(&mut self, cx: &mut Context<Self>) {
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => match result.rows.len().checked_sub(1) {
                Some(row) if result.change_of(row) == Some(rustdb_core::ChangeKind::Insert) => {
                    if result.rows[row].iter().any(|value| !value.is_null()) {
                        Err("新增行已填写，可用「行操作 → 撤销本行变更」丢弃".to_string())
                    } else {
                        revert_row(result, row)
                    }
                }
                _ => Err("没有可撤销的新增行".to_string()),
            },
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(total) => {
                self.status = format!("已撤销未填写的新增行（待提交 {total} 条）");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Idle;
            }
        }
        cx.notify();
    }

    /// 打开数据生成器面板。
    pub(crate) fn open_generator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_result().map(|result| result.editable.is_none()) != Some(false) {
            self.status = "只有「打开表」的结果能写入（生成器会新增行）".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        let column = self.selected_column().unwrap_or(0);
        let expr = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder("seq:1:1 · int:1:100 · date · uuid · regex:[a-z]{6} · fk:列名")
        });
        let count =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("生成行数"));
        count.update(cx, |state, cx| {
            state.set_value("10".to_string(), window, cx)
        });
        self.generator = Some(GeneratorPanel {
            column,
            expr,
            count,
            error: None,
        });
        cx.notify();
    }

    /// 生成表达式 → 规则（`fk:` 在这里从当前结果取候选值）。
    pub(crate) fn generator_spec(&self, expr: &str) -> Result<rustdb_core::GeneratorSpec, String> {
        let Some(result) = self.active_result() else {
            return Err("没有可编辑的结果集".into());
        };
        resolve_generator_spec(expr, result)
    }

    /// 生成器面板内报错（面板保持打开）。
    pub(crate) fn generator_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(panel) = self.generator.as_mut() {
            panel.error = Some(message.clone());
        }
        self.status = message;
        self.status_kind = StatusKind::Error;
        cx.notify();
    }

    /// 按生成表达式新增一批行。
    pub(crate) fn apply_generator(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.generator.clone() else {
            return;
        };
        let expr = panel.expr.read(cx).value().to_string();
        let count_text = panel.count.read(cx).value().to_string();
        let count: usize = match count_text.trim().parse() {
            Ok(count) if count > 0 => count,
            _ => {
                self.generator_error("生成行数需要正整数".into(), cx);
                return;
            }
        };
        if count > 10_000 {
            self.generator_error("单次最多生成 10000 行".into(), cx);
            return;
        }
        let spec = match self.generator_spec(&expr) {
            Ok(spec) => spec,
            Err(message) => {
                self.generator_error(message, cx);
                return;
            }
        };
        let values = match rustdb_core::generate(&spec, count) {
            Ok(values) => values,
            Err(message) => {
                self.generator_error(message, cx);
                return;
            }
        };
        let column = panel.column;
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => apply_generated(result, column, &values),
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(total) => {
                self.generator = None;
                self.status = format!("已生成 {count} 行（待提交 {total} 条）");
                self.status_kind = StatusKind::Ok;
                cx.notify();
            }
            Err(message) => {
                self.generator_error(message, cx);
            }
        }
    }

    /// 数据生成器面板。
    pub(crate) fn render_generator(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.generator.clone() else {
            return div().into_any_element();
        };
        let column_name = self
            .active_result()
            .and_then(|result| result.columns.get(panel.column))
            .map(|column| column.name.to_string())
            .unwrap_or_else(|| format!("第 {} 列", panel.column + 1));
        v_flex()
            .id("generator-panel")
            .debug_selector(|| "generator-panel".to_string())
            .w(px(380.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(
                div()
                    .text_size(px(crate::design::TS_BODY))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child("数据生成器"),
            )
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_secondary))
                    .child(SharedString::from(format!(
                        "列 {column_name} · 其余用默认值"
                    ))),
            )
            .child(
                div()
                    .debug_selector(|| "generator-expr".to_string())
                    .child(gpui_component::input::Input::new(&panel.expr)),
            )
            .child(
                div()
                    .debug_selector(|| "generator-count".to_string())
                    .child(gpui_component::input::Input::new(&panel.count)),
            )
            .when_some(panel.error.clone(), |card, error| {
                card.child(
                    div()
                        .debug_selector(|| "generator-error".to_string())
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.danger))
                        .child(SharedString::from(error)),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .debug_selector(|| "generator-cancel".to_string())
                            .child(
                                Button::new("generator-cancel")
                                    .small()
                                    .ghost()
                                    .label("取消")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.generator = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "generator-apply".to_string())
                            .child(
                                Button::new("generator-apply")
                                    .small()
                                    .label("生成")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.apply_generator(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
