//! 事务控制、结果标签、消息、编辑条与运行条。

use super::*;

impl MainView {
    /// 事务区：自动提交开关 + 事务内语句数/耗时 + 提交/回滚。
    pub(crate) fn render_tx_controls(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let auto = self.auto_commit;
        let tx = self.tx.as_ref().map(|tx| {
            (
                tx.statements,
                tx.read_only,
                tx.started.elapsed().as_secs_f64(),
            )
        });

        let toggle = h_flex()
            .id("tx-autocommit")
            .debug_selector(|| "tx-autocommit".to_string())
            .h(px(20.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded(px(4.))
            .cursor_pointer()
            .border_1()
            .border_color(hex(if auto { t.border_subtle } else { t.warning }))
            .hover(|s| s.bg(hex(t.bg_overlay)))
            .on_click(cx.listener(|view, _, _, cx| {
                view.auto_commit = !view.auto_commit;
                view.status = if view.auto_commit {
                    "已开启自动提交".into()
                } else {
                    "已关闭自动提交，下次执行前自动开始事务".into()
                };
                view.status_kind = StatusKind::Idle;
                cx.notify();
            }))
            .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(hex(if auto {
                t.success
            } else {
                t.warning
            })))
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(hex(if auto { t.fg_secondary } else { t.warning }))
                    .child(if auto { "自动提交" } else { "手动提交" }),
            );

        let mut row = h_flex().gap_1().items_center().child(toggle);
        if let Some((statements, read_only, elapsed_s)) = tx {
            row = row
                .child(
                    div()
                        .debug_selector(|| "tx-status".to_string())
                        .px_1()
                        .text_size(px(10.))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(format!(
                            "事务 · {statements} 条 · {elapsed_s:.1}s{}",
                            if read_only { " · 只读" } else { "" }
                        ))),
                )
                .child(
                    div().debug_selector(|| "tx-commit".to_string()).child(
                        Button::new("tx-commit-btn")
                            .small()
                            .ghost()
                            .label("提交")
                            .on_click(cx.listener(|view, _, _, cx| view.finish_tx(false, cx))),
                    ),
                )
                .child(
                    div().debug_selector(|| "tx-rollback".to_string()).child(
                        Button::new("tx-rollback-btn")
                            .small()
                            .ghost()
                            .label("回滚")
                            .on_click(cx.listener(|view, _, _, cx| view.finish_tx(true, cx))),
                    ),
                );
        }
        row
    }

    /// 提交 / 回滚当前事务。
    pub(crate) fn finish_tx(&mut self, rollback: bool, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let result = if rollback {
            self.core.rollback_tx(&session)
        } else {
            self.core.commit_tx(&session)
        };
        if let Err(message) = result {
            self.status = message;
            self.status_kind = StatusKind::Error;
        } else {
            self.status = if rollback {
                "正在回滚…".into()
            } else {
                "正在提交…".into()
            };
            self.status_kind = StatusKind::Busy;
        }
        cx.notify();
    }

    pub(crate) fn render_result_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let active_result = self.active().map(|tab| tab.active_result).unwrap_or(0);
        let show_messages = self.active().map(|tab| tab.show_messages).unwrap_or(false);
        let tabs: Vec<(usize, String, bool)> = self
            .active()
            .map(|tab| {
                tab.results
                    .iter()
                    .enumerate()
                    .map(|(index, view)| {
                        (
                            index,
                            view.tab_label(index),
                            !show_messages && index == active_result,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let message_count = self.active().map(|tab| tab.messages.len()).unwrap_or(0);
        let mut row = h_flex()
            .h(px(26.))
            .w_full()
            .flex_none()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated));
        for (index, label, active) in tabs {
            row = row.child(
                div()
                    .id(SharedString::from(format!("result-tab-{index}")))
                    .debug_selector({
                        let selector = format!("result-tab-{index}");
                        move || selector.clone()
                    })
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .border_b_2()
                    .border_color(hex(if active { t.accent } else { t.bg_elevated }))
                    .text_size(px(10.5))
                    .when(active, |d| {
                        d.bg(hex(t.bg_overlay)).text_color(hex(t.accent))
                    })
                    .when(!active, |d| d.text_color(hex(t.fg_muted)))
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(tab) = view.tabs.get_mut(view.active_tab) {
                            tab.active_result = index;
                            tab.show_messages = false;
                        }
                        view.clear_grid_selection();
                        cx.notify();
                    }))
                    .child(SharedString::from(label)),
            );
        }
        // 「消息」页签始终在末尾；没有消息时也可点开看「无消息」。
        let message_label = if message_count > 0 {
            format!("消息 {message_count}")
        } else {
            "消息".to_string()
        };
        row = row.child(
            div()
                .id("result-tab-messages")
                .debug_selector(|| "result-tab-messages".to_string())
                .h(px(20.))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_b_2()
                .border_color(hex(if show_messages {
                    t.accent
                } else {
                    t.bg_elevated
                }))
                .text_size(px(10.5))
                .when(show_messages, |d| {
                    d.bg(hex(t.bg_overlay)).text_color(hex(t.accent))
                })
                .when(!show_messages, |d| d.text_color(hex(t.fg_muted)))
                .hover(|s| s.bg(hex(t.bg_overlay)))
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(tab) = view.tabs.get_mut(view.active_tab) {
                        tab.show_messages = true;
                    }
                    cx.notify();
                }))
                .child(SharedString::from(message_label)),
        );
        row
    }

    /// 消息面板：影响行数 / 耗时 / 服务端告警原文（`NOTICE`、`RAISE NOTICE`）。
    pub(crate) fn render_messages(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let (messages, elapsed_ms, affected, tab_labels): (
            Vec<(bool, String)>,
            u64,
            Option<u64>,
            Vec<String>,
        ) = {
            let Some(tab) = self.active() else {
                return div().into_any_element();
            };
            let labels = tab
                .results
                .iter()
                .enumerate()
                .map(|(index, view)| {
                    let kind = if view.columns.is_empty() {
                        match view.affected {
                            Some(n) => format!("语句 {} · {n} 行", index + 1),
                            None => format!("语句 {}", index + 1),
                        }
                    } else {
                        format!("结果 {}", index + 1)
                    };
                    kind
                })
                .collect();
            let elapsed = tab.results.iter().map(|r| r.elapsed_ms).max().unwrap_or(0);
            let affected = tab.results.iter().rev().find_map(|r| r.affected);
            (
                tab.messages
                    .iter()
                    .map(|m| {
                        (
                            m.level == rustdb_db_api::WarningLevel::Warning,
                            m.text.clone(),
                        )
                    })
                    .collect(),
                elapsed,
                affected,
                labels,
            )
        };

        let mut list = v_flex().w_full().p_2().gap_1();
        list = list.child(
            h_flex()
                .gap_3()
                .text_size(px(10.5))
                .text_color(hex(t.fg_secondary))
                .child(SharedString::from(format!("{} ms", elapsed_ms)))
                .when_some(affected, |row, n| {
                    row.child(SharedString::from(format!("影响 {n} 行")))
                })
                .when(!tab_labels.is_empty(), |row| {
                    row.child(SharedString::from(tab_labels.join(" · ")))
                }),
        );
        if messages.is_empty() {
            list = list.child(
                div()
                    .mt_1()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_muted))
                    .child("无消息"),
            );
        }
        for (is_warning, text) in messages {
            list =
                list.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .text_size(px(11.))
                        .text_color(hex(if is_warning {
                            t.warning
                        } else {
                            t.fg_secondary
                        }))
                        .child(
                            Icon::new(if is_warning {
                                IconName::TriangleAlert
                            } else {
                                IconName::Info
                            })
                            .small()
                            .text_color(hex(if is_warning { t.warning } else { t.fg_muted })),
                        )
                        .child(div().flex_1().min_w_0().child(SharedString::from(text))),
                );
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .id("message-panel")
            .debug_selector(|| "message-panel".to_string())
            .overflow_y_scrollbar()
            .scroll_bounce(false)
            .child(list)
            .into_any_element()
    }

    /// 单元格编辑条：列名 + 输入框 + 设为 NULL / 确定 / 取消。
    pub(crate) fn render_edit_bar(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some((_, _, column)) = self.edit_cell.clone() else {
            return div().into_any_element();
        };
        if self.edit_input.is_none() {
            return div().into_any_element();
        }
        let input = self.edit_input.clone().expect("已检查");
        h_flex()
            .id("edit-bar")
            .debug_selector(|| "edit-bar".to_string())
            .h(px(30.))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(hex(t.accent))
            .bg(hex(t.bg_elevated))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(format!("编辑 {column}"))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(|| "edit-input".to_string())
                    .child(gpui_component::input::Input::new(&input).small()),
            )
            .child(
                Button::new("edit-null-btn")
                    .small()
                    .ghost()
                    .label("设为 NULL")
                    .debug_selector(|| "edit-null".to_string())
                    .on_click(cx.listener(|view, _, _, cx| view.set_cell_null(cx))),
            )
            .child(
                Button::new("edit-commit-btn")
                    .small()
                    .label("确定")
                    .debug_selector(|| "edit-commit".to_string())
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.commit_cell_edit(window, cx);
                    })),
            )
            .child(
                Button::new("edit-cancel-btn")
                    .small()
                    .ghost()
                    .label("取消")
                    .debug_selector(|| "edit-cancel".to_string())
                    .on_click(cx.listener(|view, _, _, cx| view.cancel_cell_edit(cx))),
            )
            .into_any_element()
    }

    /// 结果区：顶部运行记录条 + 结果网格 / 消息。
    pub(crate) fn render_result(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let strip = self.render_run_strip(cx);
        let body = self.render_result_body(cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .when_some(strip, |layout, strip| layout.child(strip))
            .child(body)
            .into_any_element()
    }

    /// 运行记录条（F-65）：最近几次执行，可切换查看 / 固定 / 关闭。
    pub(crate) fn render_run_strip(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let t = current(cx);
        let profile = self.active_profile.clone();
        let all = self.core.runs().list();
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|run| profile.as_deref().map_or(true, |p| run.profile_id == p))
            .collect();
        let total = filtered.len();
        if total == 0 {
            return None;
        }
        // 固定项保证可见；其余按新→旧补到 8 条。
        let mut runs = Vec::with_capacity(8);
        let mut pinned = 0;
        for run in &filtered {
            if run.pinned {
                if pinned < 8 {
                    runs.push(run.clone());
                    pinned += 1;
                }
            } else if runs.len() < 8 {
                runs.push(run.clone());
            }
        }
        let active_set = self
            .active()
            .and_then(|tab| tab.results.get(tab.active_result))
            .map(|view| view.id);

        let mut strip = h_flex()
            .id("run-strip")
            .debug_selector(|| "run-strip".to_string())
            .h(px(23.))
            .w_full()
            .flex_none()
            .px_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_base))
            .child(
                Icon::new(IconName::Clock)
                    .small()
                    .text_color(hex(t.fg_muted)),
            );

        for run in &runs {
            let id = run.id;
            let active = active_set.map_or(false, |set| run.result_sets.contains(&set));
            let pinned = run.pinned;
            let label = run.tab_label();
            strip = strip.child(
                h_flex()
                    .id(SharedString::from(format!("run-{id}")))
                    .debug_selector({
                        let selector = format!("run-chip-{id}");
                        move || selector.clone()
                    })
                    .h(px(18.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(4.))
                    .when(active, |chip| {
                        chip.bg(hex(t.bg_overlay)).text_color(hex(t.accent))
                    })
                    .when(!active, |chip| chip.text_color(hex(t.fg_muted)))
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _, _, cx| view.show_run(id, cx)))
                    .child(div().text_size(px(9.5)).child(SharedString::from(label)))
                    .child(
                        div()
                            .id(SharedString::from(format!("run-pin-{id}")))
                            .debug_selector({
                                let selector = format!("run-pin-{id}");
                                move || selector.clone()
                            })
                            .w(px(14.))
                            .h(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_elevated)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                cx.stop_propagation();
                                view.core.runs().toggle_pin(id);
                                cx.notify();
                            }))
                            .child(
                                Icon::new(if pinned {
                                    IconName::Pin
                                } else {
                                    IconName::PinOff
                                })
                                .small()
                                .text_color(hex(if pinned { t.accent } else { t.fg_muted })),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("run-close-{id}")))
                            .debug_selector({
                                let selector = format!("run-close-{id}");
                                move || selector.clone()
                            })
                            .w(px(14.))
                            .h(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_elevated)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                cx.stop_propagation();
                                view.close_run(id, cx);
                            }))
                            .child(
                                Icon::new(IconName::Close)
                                    .small()
                                    .text_color(hex(t.fg_muted)),
                            ),
                    ),
            );
        }
        if total > runs.len() {
            strip = strip.child(
                div()
                    .text_size(px(9.))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(format!("+{}", total - runs.len()))),
            );
        }
        Some(strip.into_any_element())
    }

    /// 切到某条运行记录的结果集（从 ResultStore 重建视图）。
    pub(crate) fn show_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        let Some(record) = self.core.runs().get(run_id) else {
            return;
        };
        if self.tabs.is_empty() {
            return;
        }
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if tab.running.is_some() {
            self.status = "当前查询仍在执行，完成后可切换运行记录".into();
            self.status_kind = StatusKind::Busy;
            cx.notify();
            return;
        }
        if record.is_running() {
            self.status = format!("运行 #{} 仍在执行", record.id);
            self.status_kind = StatusKind::Busy;
            cx.notify();
            return;
        }
        let store = self.core.results();
        let sets: Vec<ResultSetId> = record
            .result_sets
            .iter()
            .copied()
            .filter(|set| store.meta(*set).is_some())
            .collect();
        if sets.is_empty() {
            self.status = format!("运行 #{} 的结果已释放", record.id);
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        if tab
            .results
            .iter()
            .map(|view| view.id)
            .eq(sets.iter().copied())
        {
            return;
        }
        if tab.results.iter().any(|view| view.change_count() > 0) {
            self.status = "请先提交或回滚待提交的修改".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        self.clear_grid_selection();
        let tab = &mut self.tabs[self.active_tab];
        tab.results.clear();
        tab.messages.clear();
        tab.show_messages = false;
        for set in &sets {
            ensure_result(tab, &store, *set);
        }
        tab.active_result = tab.results.len().saturating_sub(1);
        self.status = format!("查看运行 #{}", record.id);
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 关闭一条运行记录：只丢弃记录引用，已显示/仍被引用的结果不删除。
    pub(crate) fn close_run(&mut self, run_id: u64, cx: &mut Context<Self>) {
        if self.core.runs().remove(run_id).is_some() {
            self.status = "已关闭运行记录".into();
            self.status_kind = StatusKind::Idle;
        }
        cx.notify();
    }
}
