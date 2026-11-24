//! AI 侧栏：对话气泡、工具调用与上下文芯片。

use super::*;
use gpui_kit::component::scroll::ScrollbarHandle as _;

/// P1-⑤：转录虚拟化的消息数阈值；以下保持整段渲染（短会话无滚动开销，测试路径不变）。
const AI_VIRTUAL_THRESHOLD: usize = 50;
/// P1-⑤：贴底判定容差，吸收高度回写带来的一帧抖动。
const AI_STICK_SLACK: f32 = 40.;
/// P1-⑤：首帧拿不到实测列宽时的估算兜底。
const AI_FALLBACK_WIDTH: f32 = 300.;

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
                gpui_kit::component::input::InputState::new(window, cx)
                    .placeholder("描述需求，如：上周各渠道 GMV")
            });
            self.ai_input = Some(input);
        }
        let input = self.ai_input.clone().expect("刚刚创建");

        // 转录：短会话整段渲染；长会话走虚拟列表，只画可见气泡（P1-⑤）。
        let transcript: gpui_kit::AnyElement =
            if self.ai_lines.len() >= AI_VIRTUAL_THRESHOLD {
                self.render_ai_transcript_virtual(window, cx)
            } else {
                let mut body = v_flex().gap(px(6.));
                if self.ai_lines.is_empty() {
                    body = body.child(crate::design::empty_state(
                        cx,
                        IconName::Bot,
                        "描述需求，让 AI 生成 SQL",
                        "示例：统计上周各渠道 GMV",
                    ));
                }
                for (line_index, (role, text)) in self.ai_lines.clone().into_iter().enumerate() {
                    body = body.child(self.render_ai_bubble(line_index, &role, &text, cx));
                }
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(body)
                    .into_any_element()
            };

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
                        .text_size(px(crate::design::TS_CAPTION))
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
                        .hover(|s| s.bg(hex(t.bg_elevated)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(crate::design::TS_CAPTION))
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
                                .child(gpui_kit::component::input::Input::new(&input).small()),
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
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(format!("需要确认 · {tool}"))),
                )
                .child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
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
                            .text_size(px(crate::design::TS_CAPTION))
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
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_ellipsis()
                                    .child(SharedString::from(label)),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
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
                            .text_size(px(crate::design::TS_CAPTION))
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
                            .text_size(px(crate::design::TS_CAPTION))
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
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_ellipsis()
                                    .child(SharedString::from(label)),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
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
                            .text_size(px(crate::design::TS_CAPTION))
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
                            .text_size(px(crate::design::TS_BODY))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
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
                            .text_size(px(crate::design::TS_CAPTION))
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
                    .child(transcript)
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
                                    .child(gpui_kit::component::input::Input::new(&input).small()),
                            )
                            .child(
                                div().debug_selector(|| "ai-send".to_string()).child(
                                    Button::new("ai-send-btn")
                                        .small()
                                        .label(if self.ai_running {
                                            "生成中…"
                                        } else {
                                            "发送"
                                        })
                                        .disabled(self.ai_running)
                                        .tooltip("发送需求给 AI 助手")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.ai_send(window, cx);
                                        })),
                                ),
                            ),
                    ),
            )
    }

    /// 单条消息气泡（整段渲染与虚拟列表共用）。
    pub(crate) fn render_ai_bubble(
        &self,
        line_index: usize,
        role: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = current(cx);
        let (fg, bg, role_label) = match role {
            "user" => (t.fg_primary, t.bg_overlay, "我"),
            "assistant" => (t.fg_primary, t.bg_base, "AI 助手"),
            "error" => (t.danger, t.bg_base, "错误"),
            _ => (t.fg_muted, t.bg_base, "工具"),
        };
        let mut bubble = v_flex()
            .id(SharedString::from(format!("ai-bubble-{line_index}")))
            .debug_selector(move || format!("ai-bubble-{line_index}"))
            .gap(px(2.))
            .p_2()
            .rounded(px(6.))
            .bg(hex(bg))
            .border_1()
            .border_color(hex(t.border_strong))
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(role_label),
            );
        // 助手回答：SQL 围栏块单独成卡（应用 / 试跑 / 执行），其余仍按文本渲染。
        let blocks = if role == "assistant" {
            rustdb_core::answer_blocks(text)
        } else {
            vec![rustdb_core::AnswerBlock::Text(text.to_string())]
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
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(fg))
                            .whitespace_normal()
                            .child(SharedString::from(part)),
                    );
                }
                rustdb_core::AnswerBlock::Sql { sql, language } => {
                    bubble = bubble
                        .child(self.render_ai_sql_block(line_index, ordinal, &sql, &language, cx));
                    ordinal += 1;
                }
            }
        }
        bubble
    }

    /// P1-⑤：长会话转录。VirtualList 只渲染可见气泡；高度先估算、可见项实测回写，
    /// 贴底时新消息 / 流式增长自动跟随滚动。
    fn render_ai_transcript_virtual(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        // 贴底判定用上一帧的视口 / 内容尺寸：用户上滑超过容差即停止跟随。
        let viewport_h = f32::from(self.ai_transcript_scroll.bounds().size.height);
        let content_h = f32::from(self.ai_transcript_scroll.content_size().height);
        let bottom = (viewport_h - content_h).min(0.);
        self.ai_transcript_stick =
            f32::from(self.ai_transcript_scroll.offset().y) - bottom <= AI_STICK_SLACK;
        // 只在内容变化（新消息 / 流式追加 / 高度回写）时跟随；静默帧不碰滚动，
        // 免得覆盖用户刚发起的延迟 scroll_to_item。
        let len = self.ai_lines.len();
        let tail = self
            .ai_lines
            .last()
            .map(|(role, text)| ai_bubble_key(role, text))
            .unwrap_or(0);
        let content_changed = len != self.ai_transcript_len
            || tail != self.ai_transcript_tail
            || (content_h - self.ai_transcript_content_h).abs() > 0.5;
        self.ai_transcript_len = len;
        self.ai_transcript_tail = tail;
        self.ai_transcript_content_h = content_h;
        if content_changed && self.ai_transcript_stick {
            // 不用 scroll_to_bottom：首帧句柄内 items_count 还是 0，会滚到首条。
            self.ai_transcript_scroll
                .scroll_to_item(len - 1, gpui_kit::ScrollStrategy::Top);
        }

        // 流式追加会不断产生新指纹，定期清掉不再命中的缓存条目。
        if self.ai_bubble_heights.len() > self.ai_lines.len() * 2 + 64 {
            let keys: HashSet<u64> = self
                .ai_lines
                .iter()
                .map(|(role, text)| ai_bubble_key(role, text))
                .collect();
            self.ai_bubble_heights.retain(|(key, _, _)| keys.contains(key));
        }

        let width = self.ai_transcript_width();
        let sizes: Vec<gpui_kit::Size<gpui_kit::Pixels>> = self
            .ai_lines
            .iter()
            .map(|(role, text)| {
                let height = self
                    .ai_bubble_heights
                    .iter()
                    .find(|(key, w, _)| {
                        *key == ai_bubble_key(role, text) && (*w - width).abs() < 0.5
                    })
                    .map(|(_, _, h)| *h)
                    .unwrap_or_else(|| estimate_ai_bubble_height(window, role, text, width));
                gpui_kit::size(px(width), px(height))
            })
            .collect();

        let entity = cx.entity();
        let list = gpui_kit::component::v_virtual_list(
            entity,
            "ai-transcript",
            std::rc::Rc::new(sizes),
            move |view, range: std::ops::Range<usize>, window, cx| {
                // prepaint 阶段句柄 bounds 已是本帧视口，实测宽度以此为准。
                let measured_width = f32::from(view.ai_transcript_scroll.bounds().size.width);
                let width = if measured_width > 0. {
                    measured_width
                } else {
                    AI_FALLBACK_WIDTH
                };
                let mut changed = false;
                let mut items = Vec::with_capacity(range.len());
                for index in range {
                    let (role, text) = view.ai_lines[index].clone();
                    let key = ai_bubble_key(&role, &text);
                    let cached = view
                        .ai_bubble_heights
                        .iter()
                        .find(|(k, w, _)| *k == key && (*w - width).abs() < 0.5)
                        .map(|(_, _, h)| *h);
                    let mut bubble = view
                        .render_ai_bubble(index, &role, &text, cx)
                        .into_any_element();
                    if cached.is_none() {
                        let estimate = estimate_ai_bubble_height(window, &role, &text, width);
                        let actual = f32::from(
                            bubble
                                .layout_as_root(
                                    gpui_kit::size(
                                        gpui_kit::AvailableSpace::Definite(px(width)),
                                        gpui_kit::AvailableSpace::MinContent,
                                    ),
                                    window,
                                    cx,
                                )
                                .height,
                        );
                        view.ai_bubble_heights.push((key, width, actual));
                        changed |= (actual - estimate).abs() > 0.5;
                    }
                    items.push(bubble);
                }
                if changed {
                    cx.notify();
                }
                items
            },
        )
        .gap(px(6.))
        .track_scroll(&self.ai_transcript_scroll);

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(list)
            .child(gpui_kit::component::scroll::Scrollbar::vertical(
                &self.ai_transcript_scroll,
            ))
            .into_any_element()
    }

    /// P1-⑤：转录视口宽（上一帧实测；首帧兜底）。
    fn ai_transcript_width(&self) -> f32 {
        let width = f32::from(self.ai_transcript_scroll.bounds().size.width);
        if width > 0. {
            width
        } else {
            AI_FALLBACK_WIDTH
        }
    }
}

/// P1-⑤：气泡内容指纹（角色 + 文本），用于高度缓存键。
fn ai_bubble_key(role: &str, text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    role.hash(&mut hasher);
    text.hash(&mut hasher);
    hasher.finish()
}

/// P1-⑤：按宽度估算气泡高度（离屏项用）。文本按真实排版行数；SQL 卡按词 wrap
/// 近似 token 换行，可见后由实测修正。
fn estimate_ai_bubble_height(window: &mut Window, role: &str, text: &str, width: f32) -> f32 {
    // TS_CAPTION 12px × phi() 行高，gpui 逐行取整 = 19px。
    const LINE: f32 = 19.;
    let inner = (width - 18.).max(20.);
    // 气泡 padding 16 + border 2 + 角色标签一行。
    let mut height = 18. + LINE;
    let mut children = 1usize;
    let blocks = if role == "assistant" {
        rustdb_core::answer_blocks(text)
    } else {
        vec![rustdb_core::AnswerBlock::Text(text.to_string())]
    };
    for block in blocks {
        match block {
            rustdb_core::AnswerBlock::Text(part) => {
                if part.trim().is_empty() {
                    continue;
                }
                height += wrapped_line_count(window, &part, inner) as f32 * LINE;
                children += 1;
            }
            rustdb_core::AnswerBlock::Sql { sql, .. } => {
                let code = (wrapped_line_count(window, &sql, (inner - 18.).max(20.)) as f32
                    * LINE)
                    .min(150.);
                // SQL 卡：padding+border 18 + 头 18 + 两道 gap 8 + 按钮行 22.5。
                height += 66.5 + code;
                children += 1;
            }
        }
    }
    height + 2. * children.saturating_sub(1) as f32
}

/// P1-⑤：文本在给定宽度内的排版行数（含换行符）。
fn wrapped_line_count(window: &mut Window, text: &str, wrap_width: f32) -> usize {
    if text.is_empty() {
        return 1;
    }
    let run = gpui_kit::TextRun {
        len: text.len(),
        font: gpui_kit::Font::default(),
        color: gpui_kit::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(
            SharedString::from(text.to_string()),
            px(crate::design::TS_CAPTION),
            &[run],
            Some(px(wrap_width)),
            None,
        )
        .map(|lines| lines.len().max(1))
        .unwrap_or(1)
}
