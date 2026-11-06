//! AI 模型面板（F-94）：预设、自定义端点、测试与模型发现。
use super::*;
impl MainView {
    /// F-94：模型配置浮层（预设 / 列表 / 表单 / 测试 / 发现 / 导出）。
    pub(crate) fn render_ai_model_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.ai_model_panel.as_ref() else {
            return div().into_any_element();
        };
        let busy = panel.busy;
        let kind = panel.kind;
        let auth = panel.auth;
        let editing = panel.editing_id.clone();
        let errors = panel.errors.clone();
        let report = panel.report.clone();
        let models = panel.models.clone();
        let profiles = self.ai_settings.profiles.clone();
        let active = self.ai_settings.active.clone();
        // 左：已保存配置。
        let mut list = v_flex().gap_1();
        if profiles.is_empty() {
            list = list.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child("还没有保存的配置，从下面的预设开始。"),
            );
        }
        for saved in profiles {
            let is_active = active.as_deref() == Some(saved.id.as_str());
            let is_editing = editing.as_deref() == Some(saved.id.as_str());
            let selector = format!("ai-profile-{}", saved.id);
            let label = if is_active {
                format!("● {}", saved.name)
            } else {
                saved.name.clone()
            };
            let kind_label = saved.kind.label();
            let profile = saved.clone();
            list = list.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .px_2()
                    .py_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_editing, |row| row.bg(hex(t.bg_overlay)))
                    .hover(|row| row.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.ai_panel_apply(window, cx, &profile)
                    }))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(hex(if is_active { t.accent } else { t.fg_primary }))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(kind_label)),
                    ),
            );
        }
        // 预设。
        let mut preset_row = h_flex().items_center().gap_1().flex_wrap().child(
            div()
                .text_size(px(10.))
                .text_color(hex(t.fg_muted))
                .child("预设"),
        );
        for preset in rustdb_core::presets() {
            let selector = format!("ai-preset-{}", preset.id);
            let name = preset.name.clone();
            preset_row = preset_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(20.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .text_color(hex(t.fg_secondary))
                    .cursor_pointer()
                    .hover(|row| row.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.ai_panel_apply(window, cx, &preset)
                    }))
                    .child(SharedString::from(name)),
            );
        }
        // 类型 / 认证。
        let mut kind_chips = h_flex().items_center().gap_1().flex_wrap().child(
            div()
                .w(px(64.))
                .text_size(px(10.))
                .text_color(hex(t.fg_muted))
                .child("类型"),
        );
        for option in rustdb_core::AiKind::ALL {
            let label = option.label();
            let selector = format!("ai-kind-{option:?}");
            kind_chips = kind_chips.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(20.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .cursor_pointer()
                    .when(option == kind, |chip| {
                        chip.bg(hex(t.accent)).text_color(hex(t.on_accent))
                    })
                    .when(option != kind, |chip| {
                        chip.text_color(hex(t.fg_secondary))
                            .hover(|row| row.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.ai_model_panel.as_mut() {
                            panel.kind = option;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(label)),
            );
        }
        let mut auth_chips = h_flex().items_center().gap_1().child(
            div()
                .w(px(64.))
                .text_size(px(10.))
                .text_color(hex(t.fg_muted))
                .child("认证"),
        );
        for option in rustdb_core::AuthKind::ALL {
            let label = option.label();
            let selector = format!("ai-auth-{option:?}");
            auth_chips = auth_chips.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(20.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .cursor_pointer()
                    .when(option == auth, |chip| {
                        chip.bg(hex(t.accent)).text_color(hex(t.on_accent))
                    })
                    .when(option != auth, |chip| {
                        chip.text_color(hex(t.fg_secondary))
                            .hover(|row| row.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.ai_model_panel.as_mut() {
                            panel.auth = option;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(label)),
            );
        }
        // 发现结果：点一下即填入模型名。
        let mut model_chips = h_flex().items_center().gap_1().flex_wrap().child(
            div()
                .w(px(64.))
                .text_size(px(10.))
                .text_color(hex(t.fg_muted))
                .child("发现"),
        );
        if models.is_empty() {
            model_chips = model_chips.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child("点「发现模型」列出可用模型"),
            );
        }
        for model in models {
            let selector = format!("ai-model-chip-{model}");
            let label = model.clone();
            model_chips = model_chips.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(18.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(9.5))
                    .text_color(hex(t.accent))
                    .bg(hex(t.bg_overlay))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if let Some(panel) = view.ai_model_panel.as_ref() {
                            panel
                                .model
                                .update(cx, |state, cx| state.set_value(label.clone(), window, cx));
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(model)),
            );
        }
        let report_box = report.map(|report| {
            let tone = if report.ok { t.fg_secondary } else { t.danger };
            let border = if report.ok { t.border_subtle } else { t.danger };
            div()
                .px_2()
                .py_1()
                .rounded(px(4.))
                .border_1()
                .border_color(hex(border))
                .text_size(px(10.))
                .text_color(hex(tone))
                .child(SharedString::from(report.line()))
                .into_any_element()
        });
        let mut error_box = v_flex().gap_1();
        for error in &errors {
            error_box = error_box.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error.clone())),
            );
        }
        let form = v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(ai_field(&t, "名称", &panel.name, px(160.)))
                    .child(kind_chips),
            )
            .when(kind.needs_url(), |form| {
                form.child(ai_field(&t, "端点", &panel.base_url, px(360.)))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(ai_field(&t, "模型", &panel.model, px(170.)))
                            .child(ai_field(&t, "窗口", &panel.context_window, px(80.)))
                            .child(ai_field(&t, "温度", &panel.temperature, px(56.))),
                    )
            })
            .when(!kind.needs_url(), |form| {
                form.child(ai_field(&t, "命令", &panel.cli_command, px(360.)))
            })
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(auth_chips)
                    .when(auth == rustdb_core::AuthKind::Header, |row| {
                        row.child(ai_field(&t, "头名", &panel.auth_header, px(110.)))
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(ai_field(&t, "密钥", &panel.key, px(170.)))
                    .child(ai_field(&t, "变量", &panel.key_ref, px(120.))),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(ai_field(&t, "轮次", &panel.max_turns, px(48.)))
                    .child(ai_field(&t, "重试", &panel.retries, px(48.)))
                    .child(ai_field(&t, "代理", &panel.proxy, px(170.))),
            )
            .child(ai_field(&t, "请求头", &panel.headers, px(360.)))
            .child(ai_field(&t, "全局指令", &panel.instruction, px(360.)))
            .child(model_chips)
            .when_some(report_box, |form, report| form.child(report))
            .when(!errors.is_empty(), |form| form.child(error_box));
        let body = v_flex().gap_2().child(preset_row).child(
            h_flex()
                .gap_3()
                .items_start()
                .child(v_flex().w(px(150.)).gap_1().child(list))
                .child(form),
        );
        v_flex()
            .id("ai-model-panel")
            .debug_selector(|| "ai-model-panel".to_string())
            .w(px(660.))
            .max_h(px(620.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .occlude()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("AI 模型"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(hex(t.fg_muted))
                            .child("配置只存本机；密钥不落盘"),
                    )
                    .child(
                        div().debug_selector(|| "ai-model-close".to_string()).child(
                            Button::new("ai-model-close-btn")
                                .small()
                                .ghost()
                                .label("关闭")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.ai_model_panel = None;
                                    cx.notify();
                                })),
                        ),
                    ),
            )
            .child(
                div()
                    .max_h(px(480.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(body),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(editing.is_some(), |row| {
                        row.child(
                            div()
                                .debug_selector(|| "ai-model-delete".to_string())
                                .child(
                                    Button::new("ai-model-delete-btn")
                                        .small()
                                        .ghost()
                                        .label("删除")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.ai_panel_delete(window, cx)
                                        })),
                                ),
                        )
                    })
                    .child(
                        div()
                            .debug_selector(|| "ai-model-export".to_string())
                            .child(
                                Button::new("ai-model-export-btn")
                                    .small()
                                    .ghost()
                                    .label("导出")
                                    .tooltip("导出配置摘要与最近一次测试结论（不含密钥）")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.ai_panel_export(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "ai-model-discover".to_string())
                            .child(
                                Button::new("ai-model-discover-btn")
                                    .small()
                                    .ghost()
                                    .label(if busy == Some(AiPanelTask::Discover) {
                                        "发现中…"
                                    } else {
                                        "发现模型"
                                    })
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.ai_panel_discover(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div().debug_selector(|| "ai-model-test".to_string()).child(
                            Button::new("ai-model-test-btn")
                                .small()
                                .ghost()
                                .label(if busy == Some(AiPanelTask::Test) {
                                    "测试中…"
                                } else {
                                    "测试"
                                })
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.ai_panel_test(window, cx)
                                })),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "ai-model-save".to_string()).child(
                            Button::new("ai-model-save-btn")
                                .small()
                                .primary()
                                .label("保存并启用")
                                .on_click(cx.listener(|view, _, _, cx| view.ai_panel_save(cx))),
                        ),
                    ),
            )
            .into_any_element()
    }
}
