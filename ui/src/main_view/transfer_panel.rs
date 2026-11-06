//! 跨库传输面板（F-75）：建表/重建/追加/Upsert、预览与逐表进度。
use super::*;
impl MainView {
    /// F-75：打开跨库传输面板。源 = 当前会话；目标默认选另一条连接。
    pub(crate) fn open_transfer_panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let Some(ns) = self
            .current_ns
            .clone()
            .or_else(|| self.active().and_then(|tab| tab.ns.clone()))
        else {
            self.set_status("请先选择数据库", StatusKind::Error, cx);
            return;
        };
        let profile_id = self
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let profile = self
            .profiles
            .iter()
            .find(|item| item.id == profile_id)
            .map(|item| item.name.clone())
            .unwrap_or_else(|| profile_id.clone());
        let key = ns_key(&ns);
        let mut tables: Vec<String> = self
            .objects
            .get(&key)
            .map(|objects| {
                objects
                    .iter()
                    .filter(|object| object.path.kind == ObjectKind::Table)
                    .map(|object| object.path.name.to_string())
                    .collect()
            })
            .unwrap_or_default();
        tables.sort();
        tables.dedup();
        if tables.is_empty() {
            self.set_status(
                "当前库还没有可传输的表（等待元数据加载）",
                StatusKind::Error,
                cx,
            );
            return;
        }
        if self.profiles.is_empty() {
            self.set_status("还没有目标连接；请先在连接面板添加", StatusKind::Error, cx);
            return;
        }
        let targets: Vec<(String, String)> = self
            .profiles
            .iter()
            .map(|item| (item.id.clone(), item.name.clone()))
            .collect();
        let target = targets
            .iter()
            .find(|(id, _)| id != &profile_id)
            .map(|(id, _)| id.clone())
            .or_else(|| targets.first().map(|(id, _)| id.clone()));
        self.transfer = Some(TransferPanel {
            session,
            profile,
            namespace: ns,
            selected: tables.iter().cloned().collect(),
            tables,
            targets,
            target,
            options: TransferOptions::default(),
            previewing: false,
            preview_ready: false,
            preview_stale: false,
            lines: Vec::new(),
            warnings: Vec::new(),
            running: false,
            total: 0,
            done: 0,
            rows: 0,
            table_rows: 0,
            failed: 0,
            failures: Vec::new(),
            current: None,
            backup: None,
            elapsed_ms: 0,
            error: None,
        });
        cx.notify();
    }

    /// 由面板状态构造传输请求；目标命名空间按目标连接的类型推导。
    pub(crate) fn transfer_request(
        &self,
        panel: &TransferPanel,
    ) -> Result<rustdb_core::TransferRequest, String> {
        let target_id = panel
            .target
            .clone()
            .ok_or_else(|| "请选择目标连接".to_string())?;
        let profile = self
            .profiles
            .iter()
            .find(|item| item.id == target_id)
            .ok_or_else(|| format!("找不到目标连接 {target_id}"))?;
        Ok(rustdb_core::TransferRequest {
            source_session: panel.session.clone(),
            target_profile: target_id,
            source_ns: panel.namespace.clone(),
            target_ns: profile.default_namespace(),
            tables: panel.selected_tables(),
            options: panel.options.clone(),
            backup_dir: crate::csv::export_dir(),
        })
    }

    /// 生成执行前预览（只读：临时连目标读结构，不写数据）。
    pub(crate) fn preview_transfer(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.transfer.clone() else {
            return;
        };
        if panel.previewing || panel.running {
            return;
        }
        let request = match self.transfer_request(&panel) {
            Ok(request) => request,
            Err(message) => {
                if let Some(panel) = self.transfer.as_mut() {
                    panel.error = Some(message.clone());
                }
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        if let Some(panel) = self.transfer.as_mut() {
            panel.previewing = true;
            panel.preview_ready = false;
            panel.preview_stale = false;
            panel.lines.clear();
            panel.warnings.clear();
            panel.error = None;
        }
        self.core.request_transfer_preview(request);
        self.set_status("正在生成传输计划…", StatusKind::Busy, cx);
    }

    /// 执行传输；必须先看过预览。
    pub(crate) fn start_transfer(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.transfer.clone() else {
            return;
        };
        if panel.running || panel.previewing {
            return;
        }
        if !panel.preview_ready {
            if let Some(panel) = self.transfer.as_mut() {
                panel.error = Some("请先生成并查看传输计划".into());
            }
            self.set_status("请先生成并查看传输计划", StatusKind::Error, cx);
            return;
        }
        let request = match self.transfer_request(&panel) {
            Ok(request) => request,
            Err(message) => {
                if let Some(panel) = self.transfer.as_mut() {
                    panel.error = Some(message.clone());
                }
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        if let Some(panel) = self.transfer.as_mut() {
            panel.running = true;
            panel.done = 0;
            panel.rows = 0;
            panel.table_rows = 0;
            panel.failed = 0;
            panel.failures.clear();
            panel.current = None;
            panel.backup = None;
            panel.elapsed_ms = 0;
            panel.error = None;
        }
        match self.core.request_transfer(request) {
            Ok(()) => self.set_status("正在传输…", StatusKind::Busy, cx),
            Err(message) => {
                if let Some(panel) = self.transfer.as_mut() {
                    panel.running = false;
                    panel.error = Some(message.clone());
                }
                self.set_status(message, StatusKind::Error, cx);
            }
        }
    }

    /// F-75：跨库传输面板（居中模态）。
    pub(crate) fn render_transfer_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.transfer.clone() else {
            return div().into_any_element();
        };
        let target_ns = panel
            .target
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|item| &item.id == id))
            .map(|profile| profile.default_namespace().display_label());
        let header = h_flex()
            .h(px(30.))
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::ArrowRightLeft)
                    .small()
                    .text_color(hex(t.accent)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .child(SharedString::from(format!(
                        "跨库传输 · {} · {}",
                        panel.profile,
                        panel.namespace.display_label()
                    ))),
            )
            .child(
                div()
                    .id("transfer-close")
                    .debug_selector(|| "transfer-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.transfer = None;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconName::Close)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );
        let label = |text: &str| {
            div()
                .text_size(px(9.5))
                .text_color(hex(t.fg_muted))
                .child(SharedString::from(text.to_string()))
        };
        let chip = |id: &'static str, text: String, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .h(px(18.))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(3.))
                .cursor_pointer()
                .border_1()
                .border_color(hex(if on { t.accent } else { t.border_subtle }))
                .when(on, |chip| chip.bg(hex(t.bg_overlay)))
                .text_size(px(9.5))
                .text_color(hex(if on { t.accent } else { t.fg_muted }))
                .child(SharedString::from(text))
        };
        let mut target_row = h_flex().gap_1().items_center().flex_wrap();
        for (index, (id, name)) in panel.targets.iter().enumerate() {
            let on = panel.target.as_deref() == Some(id.as_str());
            let target_id = id.clone();
            target_row = target_row.child(
                h_flex()
                    .id(("transfer-target", index))
                    .debug_selector(move || format!("transfer-target-{index}"))
                    .h(px(18.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(hex(if on { t.accent } else { t.border_subtle }))
                    .when(on, |chip| chip.bg(hex(t.bg_overlay)))
                    .text_size(px(9.5))
                    .text_color(hex(if on { t.accent } else { t.fg_muted }))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.transfer.as_mut() {
                            panel.target = Some(target_id.clone());
                            panel.reset_preview();
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(name.clone())),
            );
        }
        let target_section = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(label("目标连接"))
                    .when_some(target_ns, |row, ns| {
                        row.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(hex(t.fg_secondary))
                                .child(SharedString::from(format!("· {ns}"))),
                        )
                    }),
            )
            .child(
                div()
                    .max_h(px(64.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(target_row),
            );
        let batch_choices: [usize; 4] = [100, 500, 1000, 5000];
        let timeout_choices: [Option<u64>; 4] = [Some(10_000), Some(30_000), Some(60_000), None];
        let timeout_label = match panel.options.timeout_ms {
            Some(ms) => format!("超时 {}s", ms / 1000),
            None => "超时 不限".to_string(),
        };
        let options_row = h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(
                chip(
                    "transfer-mode",
                    format!("模式 {}", panel.options.mode.label()),
                    panel.options.mode != TransferMode::Create,
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some(panel) = view.transfer.as_mut() {
                        panel.options.mode = panel.options.mode.next();
                        panel.reset_preview();
                    }
                    cx.notify();
                })),
            )
            .child(
                chip(
                    "transfer-case",
                    format!("大小写 {}", panel.options.case.label()),
                    panel.options.case != rustdb_core::CaseStrategy::Preserve,
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some(panel) = view.transfer.as_mut() {
                        panel.options.case = panel.options.case.next();
                        panel.reset_preview();
                    }
                    cx.notify();
                })),
            )
            .child(
                chip(
                    "transfer-batch",
                    format!("每批 {}", panel.options.batch_rows),
                    panel.options.batch_rows != rustdb_core::TRANSFER_DEFAULT_BATCH_ROWS,
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(panel) = view.transfer.as_mut() {
                        let index = batch_choices
                            .iter()
                            .position(|value| *value == panel.options.batch_rows)
                            .unwrap_or(2);
                        panel.options.batch_rows = batch_choices[(index + 1) % batch_choices.len()];
                        panel.reset_preview();
                    }
                    cx.notify();
                })),
            )
            .child(
                chip(
                    "transfer-timeout",
                    timeout_label,
                    panel.options.timeout_ms.is_none(),
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(panel) = view.transfer.as_mut() {
                        let index = timeout_choices
                            .iter()
                            .position(|value| *value == panel.options.timeout_ms)
                            .unwrap_or(1);
                        panel.options.timeout_ms =
                            timeout_choices[(index + 1) % timeout_choices.len()];
                        panel.reset_preview();
                    }
                    cx.notify();
                })),
            )
            .child(
                chip("transfer-backup", "重建前备份".into(), panel.options.backup).on_click(
                    cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.transfer.as_mut() {
                            panel.options.backup = !panel.options.backup;
                            panel.reset_preview();
                        }
                        cx.notify();
                    }),
                ),
            );
        let select_all = h_flex()
            .gap_2()
            .items_center()
            .child(label(&format!(
                "表（{} / {}）",
                panel.selected.len(),
                panel.tables.len()
            )))
            .child(
                div()
                    .id("transfer-toggle-all")
                    .debug_selector(|| "transfer-toggle-all".to_string())
                    .text_size(px(9.5))
                    .text_color(hex(t.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.transfer.as_mut() {
                            if panel.all_selected() {
                                panel.selected.clear();
                            } else {
                                panel.selected = panel.tables.iter().cloned().collect();
                            }
                            panel.reset_preview();
                        }
                        cx.notify();
                    }))
                    .child(if panel.all_selected() {
                        "全不选"
                    } else {
                        "全选"
                    }),
            );
        let mut table_list = v_flex()
            .gap(px(1.))
            .h(px((panel.tables.len() as f32 * 21.).min(110.)))
            .flex_shrink_0()
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        for (index, table) in panel.tables.iter().enumerate() {
            let on = panel.selected.contains(table);
            let name = table.clone();
            table_list = table_list.child(
                h_flex()
                    .id(("transfer-table", index))
                    .debug_selector(move || format!("transfer-table-{index}"))
                    .h(px(20.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.transfer.as_mut() {
                            if !panel.selected.remove(&name) {
                                panel.selected.insert(name.clone());
                            }
                            panel.reset_preview();
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(12.))
                            .text_size(px(10.))
                            .text_color(hex(if on { t.accent } else { t.border_subtle }))
                            .child(if on { "✓" } else { "·" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(10.))
                            .text_ellipsis()
                            .child(SharedString::from(table.clone())),
                    ),
            );
        }
        let mut preview = v_flex()
            .gap(px(1.))
            .max_h(px(150.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        if panel.lines.is_empty() {
            preview = preview.child(div().text_size(px(9.5)).text_color(hex(t.fg_muted)).child(
                if panel.previewing {
                    "正在读取目标结构并生成计划…"
                } else {
                    "先生成计划（只读），确认后再执行。"
                },
            ));
        } else {
            for (index, line) in panel.lines.iter().enumerate() {
                preview = preview.child(
                    div()
                        .id(("transfer-preview-line", index))
                        .text_size(px(9.5))
                        .text_color(hex(t.fg_secondary))
                        .whitespace_normal()
                        .child(SharedString::from(line.clone())),
                );
            }
        }
        for (index, warning) in panel.warnings.iter().enumerate() {
            preview = preview.child(
                div()
                    .id(("transfer-warning", index))
                    .text_size(px(9.5))
                    .text_color(hex(t.warning))
                    .whitespace_normal()
                    .child(SharedString::from(format!("警告：{warning}"))),
            );
        }
        let progress = div()
            .flex_shrink_0()
            .text_size(px(9.5))
            .text_color(hex(if panel.error.is_some() {
                t.warning
            } else {
                t.fg_muted
            }))
            .whitespace_normal()
            .child(SharedString::from(if let Some(error) = &panel.error {
                format!("失败：{error}")
            } else if panel.running {
                format!(
                    "传输中 · {}/{} 表 · {} 行{}{}",
                    panel.done,
                    panel.total,
                    panel.rows + panel.table_rows,
                    panel
                        .current
                        .as_ref()
                        .map(|table| format!(" · {table}"))
                        .unwrap_or_default(),
                    panel
                        .backup
                        .as_ref()
                        .map(|(path, _)| format!(" · 已备份 {path}"))
                        .unwrap_or_default()
                )
            } else if panel.done > 0 {
                format!(
                    "已完成 {}/{} 表 · {} 行{} · {:.1}s{}{}",
                    panel.done,
                    panel.total,
                    panel.rows,
                    if panel.failed > 0 {
                        format!(" · {} 张失败", panel.failed)
                    } else {
                        String::new()
                    },
                    panel.elapsed_ms as f64 / 1000.0,
                    panel
                        .backup
                        .as_ref()
                        .map(|(path, bytes)| {
                            format!(" · 备份 {}（{}）", path, rustdb_core::human_bytes(*bytes))
                        })
                        .unwrap_or_default(),
                    if panel.preview_ready {
                        ""
                    } else {
                        " · 选项已变，请重新预览"
                    }
                )
            } else if panel.preview_ready {
                format!(
                    "预览 · {} 张表 · {} 条警告",
                    panel.selected.len(),
                    panel.warnings.len()
                )
            } else {
                match panel.target_label() {
                    Some(target) => format!("源 → {target}"),
                    None => "请选择目标连接。".to_string(),
                }
            }));
        let mut failure_list = v_flex()
            .gap(px(1.))
            .max_h(px(56.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        if !panel.failures.is_empty() {
            failure_list =
                failure_list.child(label(&format!("失败清单（{}）", panel.failures.len())));
            for (index, failure) in panel.failures.iter().enumerate() {
                failure_list = failure_list.child(
                    div()
                        .id(("transfer-failure", index))
                        .debug_selector(move || format!("transfer-failure-{index}"))
                        .text_size(px(9.5))
                        .text_color(hex(t.warning))
                        .whitespace_normal()
                        .child(SharedString::from(failure.clone())),
                );
            }
        }
        let actions = h_flex()
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .child(
                div()
                    .debug_selector(|| "transfer-preview".to_string())
                    .child(
                        Button::new("transfer-preview-btn")
                            .small()
                            .ghost()
                            .icon(IconName::Eye)
                            .label(if panel.previewing {
                                "生成中…"
                            } else {
                                "生成预览"
                            })
                            .disabled(panel.previewing || panel.running)
                            .on_click(cx.listener(|view, _, _, cx| view.preview_transfer(cx))),
                    ),
            )
            .child(
                div().debug_selector(|| "transfer-run".to_string()).child(
                    Button::new("transfer-run-btn")
                        .small()
                        .primary()
                        .icon(IconName::ArrowRightLeft)
                        .label(if panel.running {
                            "传输中…"
                        } else {
                            "开始传输"
                        })
                        .disabled(!panel.preview_ready || panel.running || panel.previewing)
                        .on_click(cx.listener(|view, _, _, cx| view.start_transfer(cx))),
                ),
            )
            .child(div().flex_1())
            .child(
                Button::new("transfer-close-btn")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.transfer = None;
                        cx.notify();
                    })),
            );
        v_flex()
            .id("transfer-panel")
            .debug_selector(|| "transfer-panel".to_string())
            .occlude()
            .w(px(520.))
            .h(px(420.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(target_section)
            .child(options_row)
            .child(select_all)
            .child(table_list)
            .child(preview)
            .child(progress)
            .child(failure_list)
            .child(div().flex_1())
            .child(actions)
            .into_any_element()
    }
}
