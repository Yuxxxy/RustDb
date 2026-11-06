//! AI 侧栏：对话气泡、工具调用与上下文芯片。

use super::*;

impl MainView {
    pub(crate) fn render_ai_column(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = current(cx);
        // 输入框懒创建。
        if self.ai_input.is_none() {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder("描述需求，如：上周各渠道 GMV")
            });
            self.ai_input = Some(input);
        }
        let input = self.ai_input.clone().expect("刚刚创建");

        let mut transcript = v_flex().gap(px(6.));
        for (line_index, (role, text)) in self.ai_lines.clone().into_iter().enumerate() {
            let (fg, bg) = match role.as_str() {
                "user" => (t.fg_primary, t.bg_overlay),
                "assistant" => (t.fg_primary, t.bg_base),
                "error" => (t.danger, t.bg_base),
                _ => (t.fg_muted, t.bg_base),
            };
            let mut bubble = v_flex().gap(px(2.)).p_2().rounded(px(6.)).bg(hex(bg));
            // 助手回答：SQL 围栏块单独成卡（应用 / 试跑 / 执行），其余仍按文本渲染。
            let blocks = if role == "assistant" {
                rustdb_core::answer_blocks(&text)
            } else {
                vec![rustdb_core::AnswerBlock::Text(text)]
            };
            let mut ordinal = 0usize;
            for block in blocks {
                match block {
                    rustdb_core::AnswerBlock::Text(part) => {
                        if part.trim().is_empty() {
                            continue;
                        }
                        bubble = bubble.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(hex(fg))
                                .whitespace_normal()
                                .child(SharedString::from(part)),
                        );
                    }
                    rustdb_core::AnswerBlock::Sql { sql, language } => {
                        bubble = bubble.child(
                            self.render_ai_sql_block(line_index, ordinal, &sql, &language, cx),
                        );
                        ordinal += 1;
                    }
                }
            }
            transcript = transcript.child(bubble);
        }

        // 历史面板：会话列表 + 重命名输入行。
        let mut history_panel = div().into_any_element();
        if self.ai_history_open {
            let mut list = v_flex()
                .id("ai-history")
                .debug_selector(|| "ai-history".to_string())
                .gap(px(2.))
                .max_h(px(160.))
                .overflow_y_scrollbar();
            if self.ai_sessions.is_empty() {
                list = list.child(
                    div()
                        .text_size(px(10.))
                        .text_color(hex(t.fg_muted))
                        .child("还没有历史会话"),
                );
            }
            for session in self.ai_sessions.clone() {
                let id = session.id.clone();
                let title = session.title.clone();
                let id_load = id.clone();
                let id_rename = id.clone();
                let id_delete = id.clone();
                list = list.child(
                    h_flex()
                        .h(px(26.))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded(px(4.))
                        .hover(|s| s.bg(hex(t.bg_overlay)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(10.5))
                                .text_color(hex(t.fg_primary))
                                .text_ellipsis()
                                .cursor_pointer()
                                .id(SharedString::from(format!("ai-session-{id}")))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.ai_load_session(&id_load, cx);
                                }))
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div().debug_selector(|| format!("ai-rename-{id}")).child(
                                Button::new("ai-rename-btn")
                                    .small()
                                    .ghost()
                                    .label("改名")
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.ai_start_rename(&id_rename, window, cx);
                                    })),
                            ),
                        )
                        .child(
                            div().debug_selector(|| format!("ai-delete-{id}")).child(
                                Button::new("ai-delete-btn")
                                    .small()
                                    .ghost()
                                    .label("删除")
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.ai_delete_session(&id_delete, cx);
                                    })),
                            ),
                        ),
                );
            }
            let mut panel = v_flex()
                .id("ai-history-panel")
                .debug_selector(|| "ai-history-panel".to_string())
                .gap_2()
                .p_2()
                .rounded(px(6.))
                .border_1()
                .border_color(hex(t.border_subtle))
                .bg(hex(t.bg_base))
                .child(list);
            if let Some((_, input)) = self.ai_rename.clone() {
                panel = panel.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .debug_selector(|| "ai-rename-input".to_string())
                                .child(gpui_component::input::Input::new(&input).small()),
                        )
                        .child(
                            Button::new("ai-rename-commit")
                                .small()
                                .label("确定")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.ai_commit_rename(window, cx);
                                })),
                        )
                        .child(
                            Button::new("ai-rename-cancel")
                                .small()
                                .ghost()
                                .label("取消")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.ai_rename = None;
                                    cx.notify();
                                })),
                        ),
                );
            }
            history_panel = panel.into_any_element();
        }

        let mut confirm_card = div().into_any_element();
        if let Some((_, _, tool, summary)) = self.ai_pending.clone() {
            confirm_card = v_flex()
                .id("ai-confirm")
                .debug_selector(|| "ai-confirm".to_string())
                .gap_2()
                .p_2()
                .rounded(px(6.))
                .border_1()
                .border_color(hex(t.warning))
                .bg(hex(t.bg_base))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(format!("需要确认 · {tool}"))),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(hex(t.fg_primary))
                        .whitespace_normal()
                        .child(SharedString::from(summary)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("ai-reject-btn")
                                .small()
                                .ghost()
                                .label("拒绝")
                                .debug_selector(|| "ai-reject".to_string())
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.ai_resolve_confirmation(false, cx);
                                })),
                        )
                        .child(
                            Button::new("ai-approve-btn")
                                .small()
                                .label("允许执行")
                                .debug_selector(|| "ai-approve".to_string())
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.ai_resolve_confirmation(true, cx);
                                })),
                        ),
                )
                .into_any_element();
        }

        // F-93：上下文入口 + 芯片；选择面板就地展开，不遮整窗。
        let mut ctx_row = h_flex().gap_1().items_center().flex_wrap();
        ctx_row = ctx_row
            .child(
                div()
                    .debug_selector(|| "ai-ctx-table-open".to_string())
                    .child(
                        Button::new("ai-ctx-table-btn")
                            .small()
                            .ghost()
                            .label("@表")
                            .tooltip("把表结构作为数据附加给 AI")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.ai_ctx_toggle_menu(AiCtxMenu::Table, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "ai-ctx-saved-open".to_string())
                    .child(
                        Button::new("ai-ctx-saved-btn")
                            .small()
                            .ghost()
                            .label("@已存 SQL")
                            .tooltip("固定过的历史 SQL 作为数据附加")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.ai_ctx_toggle_menu(AiCtxMenu::Saved, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "ai-ctx-selection".to_string())
                    .child(
                        Button::new("ai-ctx-selection-btn")
                            .small()
                            .ghost()
                            .label("@选区")
                            .tooltip("把编辑器当前选中的 SQL 附加为上下文")
                            .on_click(cx.listener(|view, _, _, cx| view.ai_ctx_add_selection(cx))),
                    ),
            )
            .child(div().flex_1());
        for (index, chip) in self.ai_context.iter().cloned().enumerate() {
            let selector = format!("ai-ctx-chip-{index}");
            let mut label = format!("{} · {}", chip.kind.label(), chip.label);
            if chip.will_truncate() {
                label.push_str(" · 超长将截断");
            }
            ctx_row = ctx_row.child(
                div().debug_selector(move || selector.clone()).child(
                    Button::new(SharedString::from(format!("ai-ctx-chip-btn-{index}")))
                        .small()
                        .ghost()
                        .label(SharedString::from(format!("{label} ×")))
                        .tooltip("点击移除")
                        .on_click(cx.listener(move |view, _, _, cx| view.ai_ctx_remove(index, cx))),
                ),
            );
        }

        let mut ctx_menu = div().into_any_element();
        match self.ai_ctx_menu {
            Some(AiCtxMenu::Table) => {
                let choices = self.ai_table_choices();
                let mut list = v_flex()
                    .id("ai-ctx-tables")
                    .debug_selector(|| "ai-ctx-tables".to_string())
                    .gap(px(1.))
                    .max_h(px(140.))
                    .overflow_y_scrollbar();
                if choices.is_empty() {
                    list = list.child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_muted))
                            .child("先展开左侧数据库，加载表对象"),
                    );
                }
                for (index, path) in choices.into_iter().enumerate() {
                    let selector = format!("ai-ctx-table-{index}");
                    let label = path.display_label();
                    let kind = path.kind.label();
                    list = list.child(
                        h_flex()
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector.clone())
                            .h(px(24.))
                            .px_2()
                            .gap_2()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|style| style.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.ai_ctx_pick_table(path.clone(), cx)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(10.5))
                                    .text_ellipsis()
                                    .child(SharedString::from(label)),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(hex(t.fg_muted))
                                    .child(SharedString::from(kind)),
                            ),
                    );
                }
                ctx_menu = v_flex()
                    .gap_1()
                    .p_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hex(t.border_subtle))
                    .bg(hex(t.bg_base))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child("表结构（列 / 主键 / 索引）"),
                    )
                    .child(list)
                    .into_any_element();
            }
            Some(AiCtxMenu::Saved) => {
                let entries = self.ai_saved_entries();
                let mut list = v_flex()
                    .id("ai-ctx-saved")
                    .debug_selector(|| "ai-ctx-saved".to_string())
                    .gap(px(1.))
                    .max_h(px(140.))
                    .overflow_y_scrollbar();
                if entries.is_empty() {
                    list = list.child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_muted))
                            .child("还没有固定的历史 SQL（在历史面板点「固定」）"),
                    );
                }
                for (index, entry) in entries.into_iter().enumerate() {
                    let selector = format!("ai-ctx-saved-{index}");
                    let label: String = entry
                        .sql
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("SQL")
                        .trim()
                        .chars()
                        .take(40)
                        .collect();
                    let source = entry.source.label();
                    list = list.child(
                        h_flex()
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector.clone())
                            .h(px(24.))
                            .px_2()
                            .gap_2()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|style| style.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.ai_ctx_pick_saved(entry.clone(), cx)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(10.5))
                                    .text_ellipsis()
                                    .child(SharedString::from(label)),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(hex(t.fg_muted))
                                    .child(SharedString::from(source)),
                            ),
                    );
                }
                ctx_menu = v_flex()
                    .gap_1()
                    .p_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hex(t.border_subtle))
                    .bg(hex(t.bg_base))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child("固定的历史 SQL"),
                    )
                    .child(list)
                    .into_any_element();
            }
            None => {}
        }

        // 卡片壳由布局层统一提供，这里只画内容，避免双描边。
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .h(px(34.))
                    .px_3()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(Icon::new(IconName::Bot).small().text_color(hex(t.purple)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("AI 助手"),
                    )
                    .child(div().flex_1())
                    .child(
                        div().debug_selector(|| "ai-new-chat".to_string()).child(
                            Button::new("ai-new-chat-btn")
                                .small()
                                .ghost()
                                .label("新对话")
                                .on_click(cx.listener(|view, _, _, cx| view.ai_new_chat(cx))),
                        ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "ai-history-toggle".to_string())
                            .child(
                                Button::new("ai-history-btn")
                                    .small()
                                    .ghost()
                                    .label("历史")
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.ai_toggle_history(cx)),
                                    ),
                            ),
                    )
                    .child(
                        div().debug_selector(|| "ai-models-open".to_string()).child(
                            Button::new("ai-models-open-btn")
                                .small()
                                .ghost()
                                .label("模型")
                                .tooltip("多模型配置：预设 / 连通性测试 / 发现模型")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.ai_models_open(window, cx)
                                })),
                        ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "ai-provider".to_string())
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(self.agent.provider_name())),
                    )
                    .child(
                        div().debug_selector(|| "ai-close".to_string()).child(
                            Button::new("ai-close-btn")
                                .small()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("收起 AI 助手")
                                .on_click(cx.listener(|view, _, _, cx| view.toggle_ai(cx))),
                        ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .gap_2()
                    .child(history_panel)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .scroll_bounce(false)
                            .child(transcript),
                    )
                    .child(confirm_card)
                    .child(ctx_row)
                    .child(ctx_menu)
                    .child(
                        h_flex()
                            .h(px(32.))
                            .px_2()
                            .items_center()
                            .gap_2()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(hex(t.border_subtle))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .debug_selector(|| "ai-input".to_string())
                                    .child(gpui_component::input::Input::new(&input).small()),
                            )
                            .child(
                                div().debug_selector(|| "ai-send".to_string()).child(
                                    Button::new("ai-send-btn")
                                        .small()
                                        .label(if self.ai_running { "…" } else { "发送" })
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.ai_send(window, cx);
                                        })),
                                ),
                            ),
                    ),
            )
    }
}
