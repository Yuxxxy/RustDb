//! AI 优化面板（F-92）：解释 / 优化 / 改写 / 转换与 diff 应用。

use super::*;

impl MainView {
    /// F-92：打开优化面板；默认「优化」，转换默认选一个不同方言。
    pub(crate) fn open_optimize_panel(&mut self, cx: &mut Context<Self>) {
        if self.active().is_none() {
            self.set_status("先打开查询页并输入 SQL", StatusKind::Idle, cx);
            return;
        }
        let (sql, _) = self.editor_sql(cx);
        if sql.trim().is_empty() {
            self.set_status("请先选中或输入 SQL", StatusKind::Idle, cx);
            return;
        }
        let source = self.session_kind();
        let target = DbKind::ALL
            .into_iter()
            .find(|kind| *kind != source)
            .unwrap_or(DbKind::Postgres);
        self.optimize = Some(OptimizePanel {
            kind: OptimizeKind::Optimize,
            source,
            target,
            instruction: None,
            original: sql,
            running: false,
            error: None,
            result: None,
            accepted: Vec::new(),
        });
        cx.notify();
    }

    /// 切换任务种类：旧结果作废，避免把「解释」的文本套进转换结果。
    pub(crate) fn set_optimize_kind(&mut self, kind: OptimizeKind, cx: &mut Context<Self>) {
        if let Some(panel) = self.optimize.as_mut() {
            if panel.kind == kind {
                return;
            }
            panel.kind = kind;
            panel.result = None;
            panel.accepted.clear();
            panel.error = None;
        }
        cx.notify();
    }

    pub(crate) fn set_optimize_target(&mut self, target: DbKind, cx: &mut Context<Self>) {
        if let Some(panel) = self.optimize.as_mut() {
            panel.target = target;
            panel.result = None;
            panel.accepted.clear();
            panel.error = None;
        }
        cx.notify();
    }

    /// 生成：一次模型调用，不挂工具；结果只进面板，不执行。
    pub(crate) fn optimize_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.optimize.as_ref() else {
            return;
        };
        if panel.running {
            return;
        }
        let kind = panel.kind;
        let instruction = if kind.needs_instruction() {
            panel
                .instruction
                .as_ref()
                .map(|input| input.read(cx).value().to_string())
        } else {
            None
        };
        if kind.needs_instruction()
            && instruction
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            if let Some(panel) = self.optimize.as_mut() {
                panel.error = Some("请填写改写要求".into());
            }
            cx.notify();
            return;
        }
        // 离线演示模型只会走对话脚本（首个回合是工具调用），单轮生成必然失败；
        // 先给出可操作的提示，而不是把内部的工具调用错误抛给用户。
        if self.agent.provider_name() == "offline-demo" {
            if let Some(panel) = self.optimize.as_mut() {
                panel.error = Some("离线演示不生成内容，请先在「模型」里接入".into());
            }
            self.status = "离线演示不生成内容，请先接入模型".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let req = OptimizeRequest {
            kind,
            sql: panel.original.clone(),
            source: panel.source,
            target: kind.needs_target().then_some(panel.target),
            instruction,
            schema_digest: rustdb_core::AgentEngine::schema_digest(
                &self.core,
                self.session.as_ref(),
            ),
        };
        if let Some(panel) = self.optimize.as_mut() {
            panel.running = true;
            panel.error = None;
            panel.result = None;
            panel.accepted.clear();
        }
        cx.notify();
        let optimizer = rustdb_core::Optimizer::new(self.agent.provider());
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { optimizer.run(&req) })
                .await;
            let _ = this.update_in(cx, |view, _window, cx| view.finish_optimize(outcome, cx));
        })
        .detach();
    }

    pub(crate) fn finish_optimize(
        &mut self,
        outcome: Result<OptimizeResult, String>,
        cx: &mut Context<Self>,
    ) {
        let mut status: Option<(String, StatusKind)> = None;
        if let Some(panel) = self.optimize.as_mut() {
            panel.running = false;
            match outcome {
                Ok(result) => {
                    panel.accepted = vec![true; result.hunks.len()];
                    panel.result = Some(result);
                    status = Some(("已生成，检查后插入编辑器".into(), StatusKind::Ok));
                }
                Err(message) => {
                    panel.error = Some(message.clone());
                    status = Some((message, StatusKind::Error));
                }
            }
        }
        if let Some((message, kind)) = status {
            self.status = message;
            self.status_kind = kind;
        }
        cx.notify();
    }

    pub(crate) fn toggle_optimize_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(panel) = self.optimize.as_mut() {
            if let Some(accepted) = panel.accepted.get_mut(index) {
                *accepted = !*accepted;
            }
        }
        cx.notify();
    }

    /// 采纳的 hunk 拼出的 SQL；解释模式没有 SQL 时返回 `None`。
    pub(crate) fn optimize_output_sql(&self) -> Option<String> {
        let panel = self.optimize.as_ref()?;
        let result = panel.result.as_ref()?;
        let sql = result.sql.as_ref()?;
        if result.hunks.is_empty() {
            Some(sql.clone())
        } else {
            Some(rustdb_core::apply_hunks(
                &panel.original,
                &result.hunks,
                &panel.accepted,
            ))
        }
    }

    pub(crate) fn optimize_apply(
        &mut self,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sql) = self.optimize_output_sql() else {
            return;
        };
        if new_tab && !self.open_untitled_tab(window, cx) {
            return;
        }
        self.insert_sql(sql, window, cx);
        self.optimize = None;
        self.set_status("已插入编辑器（未执行）", StatusKind::Ok, cx);
    }
    /// F-92：优化面板（种类 / 目标方言 / 要求 / 说明 / 逐段 diff / 插入）。
    pub(crate) fn render_optimize_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.optimize.as_ref() else {
            return div().into_any_element();
        };
        let kind = panel.kind;
        let source = panel.source;
        let target = panel.target;
        let original = panel.original.clone();
        let running = panel.running;
        let error = panel.error.clone();
        let result = panel.result.clone();
        let accepted = panel.accepted.clone();
        let can_insert = result
            .as_ref()
            .and_then(|result| result.sql.as_ref())
            .is_some();

        if kind.needs_instruction()
            && self
                .optimize
                .as_ref()
                .and_then(|panel| panel.instruction.clone())
                .is_none()
        {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder("例如：改成分页查询并返回总数")
            });
            if let Some(panel) = self.optimize.as_mut() {
                panel.instruction = Some(input);
            }
        }
        let instruction = self
            .optimize
            .as_ref()
            .and_then(|panel| panel.instruction.clone());

        let mut kind_row = h_flex().gap_1().items_center();
        for option in OptimizeKind::ALL {
            let active = option == kind;
            let selector = format!("ai-optimize-kind-{}", option.key());
            kind_row = kind_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(20.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                    .when(!active, |d| {
                        d.text_color(hex(t.fg_secondary))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.set_optimize_kind(option, cx);
                    }))
                    .child(SharedString::from(option.label())),
            );
        }
        kind_row = kind_row.child(div().flex_1()).child(
            div()
                .debug_selector(|| "ai-optimize-run".to_string())
                .child(
                    Button::new("ai-optimize-run-btn")
                        .small()
                        .primary()
                        .label(if running {
                            "…"
                        } else if result.is_some() {
                            "重新生成"
                        } else {
                            "生成"
                        })
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.optimize_run(window, cx);
                        })),
                ),
        );

        let mut target_row = h_flex().gap_1().items_center();
        if kind.needs_target() {
            target_row = target_row.child(
                div()
                    .text_size(px(9.5))
                    .text_color(hex(t.fg_muted))
                    .child("目标方言"),
            );
            for option in DbKind::ALL.into_iter().filter(|option| *option != source) {
                let active = option == target;
                let selector = format!("ai-optimize-target-{}", option.key());
                target_row = target_row.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .h(px(18.))
                        .px_2()
                        .rounded(px(4.))
                        .text_size(px(10.))
                        .cursor_pointer()
                        .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                        .when(!active, |d| {
                            d.text_color(hex(t.fg_secondary))
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.set_optimize_target(option, cx);
                        }))
                        .child(SharedString::from(option.display_name())),
                );
            }
        }

        let mut instruction_row = h_flex().h(px(30.)).items_center().gap_2();
        if let Some(input) = instruction {
            instruction_row = instruction_row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "ai-optimize-instruction".to_string())
                    .child(gpui_component::input::Input::new(&input).small()),
            );
        }

        let mut body = v_flex().gap_2();
        if let Some(error) = error {
            body = body.child(
                div()
                    .debug_selector(|| "ai-optimize-error".to_string())
                    .whitespace_normal()
                    .text_size(px(10.5))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error)),
            );
        }
        if running {
            body = body.child(
                div()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_muted))
                    .child("正在生成…"),
            );
        }
        if let Some(result) = result.as_ref() {
            if !result.explanation.is_empty() {
                body = body.child(
                    div()
                        .whitespace_normal()
                        .text_size(px(10.5))
                        .text_color(hex(t.fg_primary))
                        .child(SharedString::from(result.explanation.clone())),
                );
            }
            for note in &result.dialect_notes {
                let color = if note.level == rustdb_core::NoteLevel::Warning {
                    t.warning
                } else {
                    t.fg_muted
                };
                body = body.child(
                    h_flex()
                        .gap_1()
                        .items_start()
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(hex(color))
                                .child(SharedString::from(note.level.label())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(px(10.))
                                .text_color(hex(color))
                                .child(SharedString::from(note.message.clone())),
                        ),
                );
            }
            for note in &result.notes {
                body = body.child(
                    div()
                        .whitespace_normal()
                        .text_size(px(10.))
                        .text_color(hex(t.fg_muted))
                        .child(SharedString::from(format!("· {note}"))),
                );
            }
            if result.hunks.is_empty() {
                if let Some(sql) = result.sql.as_deref() {
                    body = body.child(
                        div()
                            .debug_selector(|| "ai-optimize-sql".to_string())
                            .child(crate::ddl::render_sql(sql, &t)),
                    );
                }
            } else {
                for (index, hunk) in result.hunks.iter().enumerate() {
                    let is_accepted = accepted.get(index).copied().unwrap_or(false);
                    let selector = format!("ai-optimize-hunk-{index}");
                    let mut card = v_flex()
                        .gap_1()
                        .p_2()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hex(t.border_subtle))
                        .bg(hex(t.bg_base))
                        .child(
                            h_flex()
                                .h(px(18.))
                                .items_center()
                                .gap_2()
                                .child(div().text_size(px(9.)).text_color(hex(t.fg_muted)).child(
                                    SharedString::from(format!(
                                        "第 {} 段 · -{} +{}",
                                        index + 1,
                                        hunk.removed(),
                                        hunk.added()
                                    )),
                                ))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .id(SharedString::from(selector.clone()))
                                        .debug_selector(move || selector.clone())
                                        .h(px(17.))
                                        .px_2()
                                        .rounded(px(4.))
                                        .text_size(px(9.))
                                        .cursor_pointer()
                                        .when(is_accepted, |d| {
                                            d.bg(hex(t.accent)).text_color(hex(t.on_accent))
                                        })
                                        .when(!is_accepted, |d| {
                                            d.text_color(hex(t.fg_muted))
                                                .hover(|s| s.bg(hex(t.bg_overlay)))
                                        })
                                        .on_click(cx.listener(move |view, _, _, cx| {
                                            view.toggle_optimize_hunk(index, cx);
                                        }))
                                        .child(SharedString::from(if is_accepted {
                                            "采纳"
                                        } else {
                                            "跳过"
                                        })),
                                ),
                        );
                    for line in &hunk.context_before {
                        card = card.child(diff_line(line, None, t.fg_muted));
                    }
                    for line in original.lines().skip(hunk.old_start).take(hunk.old_len) {
                        card = card.child(diff_line(line, Some("-"), t.danger));
                    }
                    for line in &hunk.new_lines {
                        card = card.child(
                            h_flex()
                                .gap_1()
                                .items_start()
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(hex(t.accent))
                                        .child("+"),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(crate::ddl::render_sql(line, &t)),
                                ),
                        );
                    }
                    for line in &hunk.context_after {
                        card = card.child(diff_line(line, None, t.fg_muted));
                    }
                    body = body.child(card);
                }
            }
        }

        v_flex()
            .id("ai-optimize-panel")
            .debug_selector(|| "ai-optimize-panel".to_string())
            .w(px(600.))
            .max_h(px(540.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .occlude()
            .child(
                h_flex()
                    .h(px(24.))
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::Bot).small().text_color(hex(t.purple)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("AI 优化"),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(self.agent.provider_name())),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(source.display_name())),
                    ),
            )
            .child(kind_row)
            .when(kind.needs_target(), |layout| layout.child(target_row))
            .when(kind.needs_instruction(), |layout| {
                layout.child(instruction_row)
            })
            .child(
                div()
                    .id("ai-optimize-body")
                    .max_h(px(360.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(body),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child("只生成，不执行"),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("ai-optimize-close-btn")
                            .small()
                            .ghost()
                            .label("关闭")
                            .debug_selector(|| "ai-optimize-close".to_string())
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.optimize = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("ai-optimize-new-tab-btn")
                            .small()
                            .ghost()
                            .label("新标签")
                            .debug_selector(|| "ai-optimize-new-tab".to_string())
                            .disabled(!can_insert)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.optimize_apply(true, window, cx);
                            })),
                    )
                    .child(
                        Button::new("ai-optimize-insert-btn")
                            .small()
                            .primary()
                            .label("插入编辑器")
                            .debug_selector(|| "ai-optimize-insert".to_string())
                            .disabled(!can_insert)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.optimize_apply(false, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}
