//! 结构对比与部署面板（F-86）：快照、差异、部署/回滚脚本与配置交换。
use super::*;
impl MainView {
    /// F-86：打开结构对比面板。先把当前库作为源 / 目标各取一份快照。
    pub(crate) fn open_compare_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let profile = self
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let mut config = rustdb_core::load_compare_config(&self.compare_config_path);
        config.source = Some(ns.clone());
        config.target = Some(ns.clone());
        let ignored = config.ignored_tables.join(", ");
        let renames = rename_text(&config.renames);
        let ignore_input = designer_input(window, cx, "逗号分隔", Some(&ignored));
        let rename_input = designer_input(window, cx, "旧表=>新表，逗号分隔", Some(&renames));
        self.compare = Some(ComparePanel {
            session: session.clone(),
            profile,
            namespace: ns.clone(),
            source: None,
            target: None,
            loading: [true, true],
            error: [None, None],
            config,
            report: None,
            ignore_input,
            rename_input,
            drop_tables: false,
            drop_columns: false,
            show_rollback: false,
            confirm_execute: false,
            dirty: false,
            message: None,
        });
        self.core
            .request_schema_snapshot(&session, ns.clone(), "source".into());
        self.core
            .request_schema_snapshot(&session, ns, "target".into());
        self.set_status("正在读取源 / 目标结构快照…", StatusKind::Busy, cx);
    }

    /// 重新读取某个槽位（源 / 目标）的结构快照。
    pub(crate) fn reload_compare_slot(&mut self, source: bool, cx: &mut Context<Self>) {
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
        if let Some(panel) = self.compare.as_mut() {
            let index = if source { 0 } else { 1 };
            panel.loading[index] = true;
            panel.error[index] = None;
            panel.namespace = ns.clone();
            panel.message = None;
        }
        let slot = if source { "source" } else { "target" };
        self.core.request_schema_snapshot(&session, ns, slot.into());
        self.set_status("正在读取结构快照…", StatusKind::Busy, cx);
    }

    /// 按当前输入与快照重算差异，并把配置落盘。
    pub(crate) fn recompute_compare(&mut self, cx: &App) {
        let Some(panel) = self.compare.as_mut() else {
            return;
        };
        panel.dirty = false;
        let ignored = panel.ignore_input.read(cx).value().to_string();
        panel.config.set_ignored_text(&ignored);
        let renames = panel.rename_input.read(cx).value().to_string();
        panel.config.renames = parse_rename_pairs(&renames);
        panel.config.source = panel.source.as_ref().map(|snap| snap.namespace.clone());
        panel.config.target = panel.target.as_ref().map(|snap| snap.namespace.clone());
        panel.report = match (panel.source.as_ref(), panel.target.as_ref()) {
            (Some(source), Some(target)) => {
                Some(rustdb_core::compare_schemas(source, target, &panel.config))
            }
            _ => None,
        };
        panel.confirm_execute = false;
        if let Err(error) =
            rustdb_core::save_compare_config(&self.compare_config_path, &panel.config)
        {
            log::warn!("对比配置写入失败：{error}");
        }
        if let Some(report) = panel.report.as_ref() {
            let counts = report.counts();
            self.status = format!(
                "结构对比：新增 {} / 删除 {} / 修改 {} / 一致 {}",
                counts.added, counts.removed, counts.changed, counts.same
            );
            self.status_kind = StatusKind::Idle;
        }
    }

    /// 从 JSON 文件导入源快照（目标侧由「导出快照」产生）。
    pub(crate) fn compare_import_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("选择结构快照 JSON")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let reading = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { rustdb_core::load_snapshot(&reading) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match result {
                Ok(snapshot) => {
                    if let Some(panel) = view.compare.as_mut() {
                        panel.source = Some(snapshot);
                        panel.loading[0] = false;
                        panel.error[0] = None;
                        panel.dirty = true;
                        panel.message = None;
                    }
                    view.set_status(
                        format!("已导入源快照：{}", path.display()),
                        StatusKind::Ok,
                        cx,
                    );
                }
                Err(error) => {
                    view.set_status(format!("导入快照失败：{error}"), StatusKind::Error, cx)
                }
            });
        })
        .detach();
    }

    /// 把目标快照导出为 JSON，供离线对比或别的实例导入。
    pub(crate) fn compare_export_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.compare.as_ref() else {
            return;
        };
        let Some(snapshot) = panel.target.clone() else {
            self.set_status("目标快照还没载入", StatusKind::Error, cx);
            return;
        };
        let stem = doc_file_stem(&panel.namespace.display_label());
        let suggested = format!("{stem}-snapshot.json");
        let dir = std::env::current_dir().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let writing = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { rustdb_core::save_snapshot(&writing, &snapshot) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match result {
                Ok(()) => view.set_status(
                    format!("已导出结构快照：{}", path.display()),
                    StatusKind::Ok,
                    cx,
                ),
                Err(error) => {
                    view.set_status(format!("导出快照失败：{error}"), StatusKind::Error, cx)
                }
            });
        })
        .detach();
    }

    /// 导出对比配置（命名空间、忽略表、已确认重命名）。
    pub(crate) fn compare_export_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.compare.as_ref() else {
            return;
        };
        let text = match panel.config.to_json() {
            Ok(text) => text,
            Err(error) => {
                self.set_status(error, StatusKind::Error, cx);
                return;
            }
        };
        let stem = doc_file_stem(&panel.namespace.display_label());
        let dir = std::env::current_dir().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some(&format!("{stem}-compare.json")));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let writing = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { rustdb_core::write_doc_atomic(&writing, &text) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match result {
                Ok(()) => view.set_status(
                    format!("已导出对比配置：{}", path.display()),
                    StatusKind::Ok,
                    cx,
                ),
                Err(error) => {
                    view.set_status(format!("导出配置失败：{error}"), StatusKind::Error, cx)
                }
            });
        })
        .detach();
    }

    /// 导入对比配置；输入框与差异随之一并刷新。
    pub(crate) fn compare_import_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("选择对比配置 JSON")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let reading = path.clone();
            let text = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(&reading) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| match text {
                Ok(text) => match rustdb_core::CompareConfig::from_json(&text) {
                    Ok(config) => {
                        if let Some(panel) = view.compare.as_mut() {
                            let ignored = config.ignored_tables.join(", ");
                            let renames = rename_text(&config.renames);
                            panel
                                .ignore_input
                                .update(cx, |state, cx| state.set_value(ignored, window, cx));
                            panel
                                .rename_input
                                .update(cx, |state, cx| state.set_value(renames, window, cx));
                            panel.config = config;
                            panel.dirty = true;
                            panel.message = None;
                        }
                        view.set_status("已导入对比配置", StatusKind::Ok, cx);
                    }
                    Err(error) => view.set_status(error, StatusKind::Error, cx),
                },
                Err(error) => {
                    view.set_status(format!("读取配置失败：{error}"), StatusKind::Error, cx)
                }
            });
        })
        .detach();
    }

    /// 执行部署脚本。第一次点击只做确认；写入门控仍由 `Core` 统一判断。
    pub(crate) fn compare_execute(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.compare.clone() else {
            return;
        };
        let Some(report) = panel.report.as_ref() else {
            return;
        };
        let deploy = report.deploy_plan(&panel.options());
        if deploy.plan.executable_sql().is_empty() {
            if let Some(panel) = self.compare.as_mut() {
                panel.message = Some("没有需要执行的语句".into());
            }
            cx.notify();
            return;
        }
        if !panel.confirm_execute {
            if let Some(panel) = self.compare.as_mut() {
                panel.confirm_execute = true;
                panel.message = Some("请先核对影响与回滚建议，再次点击「确认执行」".into());
            }
            cx.notify();
            return;
        }
        let script = deploy.script();
        match self.core.execute(&panel.session, &script) {
            Ok(query_id) => {
                if let Some(panel) = self.compare.as_mut() {
                    panel.confirm_execute = false;
                    panel.message = Some("部署已提交，完成后请重新载入目标快照核对".into());
                }
                self.set_status(format!("部署已提交 · {query_id}"), StatusKind::Busy, cx);
            }
            Err(message) => {
                if let Some(panel) = self.compare.as_mut() {
                    panel.confirm_execute = false;
                    panel.message = Some(message.clone());
                }
                self.set_status(message, StatusKind::Error, cx);
            }
        }
    }

    /// F-86：结构对比面板（源 / 目标 → 差异 → 影响 / 回滚 → 部署）。
    pub(crate) fn render_compare_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        if self.compare.as_ref().is_some_and(|panel| panel.dirty) {
            self.recompute_compare(cx);
        }
        let Some(panel) = self.compare.clone() else {
            return div().into_any_element();
        };
        let options = panel.options();
        let deploy = panel
            .report
            .as_ref()
            .map(|report| report.deploy_plan(&options));
        let can_execute = deploy
            .as_ref()
            .is_some_and(|deploy| !deploy.plan.executable_sql().is_empty());
        let header = h_flex()
            .h(px(30.))
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::ArrowLeftRight)
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
                    .child(SharedString::from(format!("结构对比 · {}", panel.profile))),
            )
            .child(
                div()
                    .id("compare-close")
                    .debug_selector(|| "compare-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.compare = None;
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
        let slots = h_flex()
            .gap_2()
            .items_stretch()
            .child(self.render_compare_slot(true, cx))
            .child(self.render_compare_slot(false, cx));
        let config = v_flex()
            .gap(px(2.))
            .child(label("忽略的表"))
            .child(Input::new(&panel.ignore_input).small())
            .child(label("重命名"))
            .child(Input::new(&panel.rename_input).small());
        let drop_tables = div()
            .id("compare-drop-tables")
            .debug_selector(|| "compare-drop-tables".to_string())
            .h(px(18.))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(3.))
            .cursor_pointer()
            .border_1()
            .border_color(hex(if panel.drop_tables {
                t.accent
            } else {
                t.border_subtle
            }))
            .when(panel.drop_tables, |chip| chip.bg(hex(t.bg_overlay)))
            .text_size(px(9.5))
            .text_color(hex(if panel.drop_tables {
                t.accent
            } else {
                t.fg_muted
            }))
            .on_click(cx.listener(|view, _, _, cx| {
                if let Some(panel) = view.compare.as_mut() {
                    panel.drop_tables = !panel.drop_tables;
                    panel.dirty = true;
                }
                cx.notify();
            }))
            .child("允许 DROP 多余表");
        let drop_columns = div()
            .id("compare-drop-columns")
            .debug_selector(|| "compare-drop-columns".to_string())
            .h(px(18.))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(3.))
            .cursor_pointer()
            .border_1()
            .border_color(hex(if panel.drop_columns {
                t.accent
            } else {
                t.border_subtle
            }))
            .when(panel.drop_columns, |chip| chip.bg(hex(t.bg_overlay)))
            .text_size(px(9.5))
            .text_color(hex(if panel.drop_columns {
                t.accent
            } else {
                t.fg_muted
            }))
            .on_click(cx.listener(|view, _, _, cx| {
                if let Some(panel) = view.compare.as_mut() {
                    panel.drop_columns = !panel.drop_columns;
                    panel.dirty = true;
                }
                cx.notify();
            }))
            .child("允许 DROP 多余列");
        let actions = h_flex()
            .gap_2()
            .items_center()
            .child(
                div().debug_selector(|| "compare-run".to_string()).child(
                    Button::new("compare-run-btn")
                        .small()
                        .ghost()
                        .label("应用并对比")
                        .on_click(cx.listener(|view, _, _, cx| {
                            if let Some(panel) = view.compare.as_mut() {
                                panel.dirty = true;
                            }
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "compare-import-config".to_string())
                    .child(
                        Button::new("compare-import-config-btn")
                            .small()
                            .ghost()
                            .label("导入配置")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.compare_import_config(window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "compare-export-config".to_string())
                    .child(
                        Button::new("compare-export-config-btn")
                            .small()
                            .ghost()
                            .label("导出配置")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.compare_export_config(window, cx)
                            })),
                    ),
            )
            .child(drop_tables)
            .child(drop_columns);
        let mut report_area = v_flex().flex_1().min_h_0().gap_1();
        if let Some(report) = panel.report.as_ref() {
            let counts = report.counts();
            report_area = report_area.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(label("差异"))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.success))
                            .child(SharedString::from(format!("新增 {}", counts.added))),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.danger))
                            .child(SharedString::from(format!("删除 {}", counts.removed))),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.warning))
                            .child(SharedString::from(format!("修改 {}", counts.changed))),
                    )
                    .child(div().text_size(px(10.)).text_color(hex(t.fg_muted)).child(
                        SharedString::from(format!(
                            "一致 {} · 变更列 {}",
                            counts.same, counts.columns_changed
                        )),
                    )),
            );
            let mut list = v_flex()
                .w(px(300.))
                .flex_none()
                .gap(px(1.))
                .max_h(px(260.))
                .overflow_y_scrollbar()
                .scroll_bounce(false);
            if report
                .tables
                .iter()
                .all(|table| table.status == rustdb_core::DiffStatus::Same)
            {
                list = list.child(
                    div()
                        .text_size(px(10.))
                        .text_color(hex(t.fg_muted))
                        .child("结构一致，没有差异"),
                );
            }
            for (index, table) in report.tables.iter().enumerate() {
                let color = match table.status {
                    rustdb_core::DiffStatus::Added => t.success,
                    rustdb_core::DiffStatus::Removed => t.danger,
                    rustdb_core::DiffStatus::Changed => t.warning,
                    rustdb_core::DiffStatus::Same => t.fg_muted,
                };
                let changed_columns = table
                    .columns
                    .iter()
                    .filter(|column| column.status != rustdb_core::DiffStatus::Same)
                    .count();
                let mut row = h_flex()
                    .id(("compare-table", index))
                    .debug_selector(move || format!("compare-table-{index}"))
                    .h(px(20.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.))
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .child(
                        div()
                            .w(px(14.))
                            .text_size(px(10.))
                            .text_color(hex(color))
                            .child(SharedString::from(table.status.symbol())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(10.))
                            .text_ellipsis()
                            .child(SharedString::from(table.name().to_string())),
                    );
                if table.is_renamed() {
                    row = row.child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.accent))
                            .child("重命名"),
                    );
                }
                row = row.child(div().text_size(px(9.)).text_color(hex(t.fg_muted)).child(
                    SharedString::from(if changed_columns > 0 {
                        format!("{changed_columns} 列")
                    } else {
                        table.status.label().to_string()
                    }),
                ));
                list = list.child(row);
            }
            let mut side = v_flex().flex_1().min_w_0().gap_1();
            if !report.rename_candidates.is_empty() {
                let mut candidates = v_flex().gap(px(1.));
                for candidate in report.rename_candidates.iter().take(5) {
                    candidates =
                        candidates.child(div().text_size(px(9.5)).text_color(hex(t.accent)).child(
                            SharedString::from(format!(
                                "重命名候选：{} → {}（{}/{} 列匹配；填入上方「重命名」确认）",
                                candidate.from,
                                candidate.to,
                                candidate.matched_columns,
                                candidate.total_columns
                            )),
                        ));
                }
                side = side.child(candidates);
            }
            if !report.warnings.is_empty() {
                let mut warnings = v_flex()
                    .gap(px(1.))
                    .max_h(px(60.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false);
                for warning in &report.warnings {
                    warnings = warnings.child(
                        div()
                            .text_size(px(9.5))
                            .text_color(hex(t.warning))
                            .child(SharedString::from(format!("警告：{warning}"))),
                    );
                }
                side = side.child(warnings);
            }
            if let Some(deploy) = deploy.as_ref() {
                let script = deploy.script();
                side = side.child(label("部署脚本")).child(
                    div()
                        .max_h(px(96.))
                        .p_1()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(hex(t.border_subtle))
                        .overflow_y_scrollbar()
                        .scroll_bounce(false)
                        .text_size(px(9.5))
                        .text_color(hex(t.fg_secondary))
                        .whitespace_normal()
                        .child(SharedString::from(if script.is_empty() {
                            "（无需部署）".to_string()
                        } else {
                            script
                        })),
                );
                if !deploy.plan.unsupported.is_empty() {
                    let mut unsupported = v_flex().gap(px(1.));
                    for item in &deploy.plan.unsupported {
                        unsupported = unsupported.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(hex(t.warning))
                                .child(SharedString::from(format!("不支持：{item}"))),
                        );
                    }
                    side = side.child(unsupported);
                }
                if !deploy.impact.is_empty() {
                    let mut impact = v_flex()
                        .gap(px(1.))
                        .max_h(px(64.))
                        .overflow_y_scrollbar()
                        .scroll_bounce(false);
                    for line in &deploy.impact {
                        impact = impact.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(hex(t.fg_secondary))
                                .child(SharedString::from(line.clone())),
                        );
                    }
                    side = side.child(impact);
                }
                let rollback = deploy.rollback_script();
                let rollback_toggle = h_flex()
                    .gap_1()
                    .items_center()
                    .child(label("回滚建议"))
                    .child(
                        div()
                            .id("compare-toggle-rollback")
                            .debug_selector(|| "compare-toggle-rollback".to_string())
                            .h(px(16.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_size(px(9.))
                            .text_color(hex(t.accent))
                            .hover(|style| style.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(panel) = view.compare.as_mut() {
                                    panel.show_rollback = !panel.show_rollback;
                                }
                                cx.notify();
                            }))
                            .child(if panel.show_rollback {
                                "收起"
                            } else {
                                "展开"
                            }),
                    );
                side = side.child(rollback_toggle);
                if panel.show_rollback {
                    side = side.child(
                        div()
                            .max_h(px(96.))
                            .p_1()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(hex(t.border_subtle))
                            .overflow_y_scrollbar()
                            .scroll_bounce(false)
                            .text_size(px(9.5))
                            .text_color(hex(t.fg_secondary))
                            .whitespace_normal()
                            .child(SharedString::from(if rollback.is_empty() {
                                "（没有可自动生成的回滚语句；数据不会自动恢复）".to_string()
                            } else {
                                rollback
                            })),
                    );
                }
            }
            report_area = report_area.child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .gap_2()
                    .child(list)
                    .child(side),
            );
        } else if panel.loading[0] || panel.loading[1] {
            report_area = report_area.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child("正在读取结构快照…"),
            );
        } else if let Some(error) = panel.error.iter().flatten().next() {
            report_area = report_area.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error.clone())),
            );
        }
        let footer = h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(9.5))
                    .text_color(hex(if panel.confirm_execute {
                        t.warning
                    } else {
                        t.fg_muted
                    }))
                    .child(SharedString::from(
                        panel
                            .message
                            .clone()
                            .unwrap_or_else(|| "先生成，再执行".to_string()),
                    )),
            )
            .child(
                div()
                    .debug_selector(|| "compare-execute".to_string())
                    .child(
                        Button::new("compare-execute-btn")
                            .small()
                            .ghost()
                            .label(if panel.confirm_execute {
                                "确认执行"
                            } else {
                                "执行部署"
                            })
                            .disabled(!can_execute)
                            .on_click(cx.listener(|view, _, _, cx| view.compare_execute(cx))),
                    ),
            )
            .child(
                Button::new("compare-close-btn")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.compare = None;
                        cx.notify();
                    })),
            );
        v_flex()
            .id("compare-panel")
            .debug_selector(|| "compare-panel".to_string())
            .occlude()
            .w(px(900.))
            .h(px(640.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(slots)
            .child(config)
            .child(actions)
            .child(report_area)
            .child(footer)
            .into_any_element()
    }

    /// F-86：单个槽位卡片（源 / 目标）。
    pub(crate) fn render_compare_slot(
        &mut self,
        source: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.compare.clone() else {
            return div().into_any_element();
        };
        let index = if source { 0 } else { 1 };
        let snapshot = if source { &panel.source } else { &panel.target };
        let title = if source {
            "源 · 参照（期望）"
        } else {
            "目标 · 待修改"
        };
        let mut status = v_flex().gap(px(1.));
        if panel.loading[index] {
            status = status.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child("正在读取结构快照…"),
            );
        } else if let Some(error) = panel.error[index].as_ref() {
            status = status.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error.clone())),
            );
        } else if let Some(snapshot) = snapshot.as_ref() {
            let mut line = format!(
                "{} · {} 张表",
                snapshot.namespace.display_label(),
                snapshot.tables.len()
            );
            if snapshot.skipped > 0 {
                line.push_str(&format!(" · 跳过 {}", snapshot.skipped));
            }
            if !snapshot.failed.is_empty() {
                line.push_str(&format!(" · {} 张取详情失败", snapshot.failed.len()));
            }
            status = status.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(if snapshot.failed.is_empty() {
                        t.fg_secondary
                    } else {
                        t.warning
                    }))
                    .child(SharedString::from(line)),
            );
        } else {
            status = status.child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child("（未载入）"),
            );
        }
        let reload_selector = if source {
            "compare-load-source"
        } else {
            "compare-load-target"
        };
        let mut actions = h_flex().gap_1().items_center().child(
            div()
                .debug_selector(move || reload_selector.to_string())
                .child(
                    Button::new(if source {
                        "compare-reload-source"
                    } else {
                        "compare-reload-target"
                    })
                    .small()
                    .ghost()
                    .label("载入当前库")
                    .on_click(
                        cx.listener(move |view, _, _, cx| view.reload_compare_slot(source, cx)),
                    ),
                ),
        );
        if source {
            actions = actions.child(
                div()
                    .debug_selector(|| "compare-import-snapshot".to_string())
                    .child(
                        Button::new("compare-import-snapshot-btn")
                            .small()
                            .ghost()
                            .label("导入快照")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.compare_import_snapshot(window, cx)
                            })),
                    ),
            );
        } else {
            actions = actions.child(
                div()
                    .debug_selector(|| "compare-export-snapshot".to_string())
                    .child(
                        Button::new("compare-export-snapshot-btn")
                            .small()
                            .ghost()
                            .label("导出快照")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.compare_export_snapshot(window, cx)
                            })),
                    ),
            );
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .p_2()
            .rounded(px(6.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_overlay))
            .child(
                div()
                    .text_size(px(9.5))
                    .text_color(hex(t.fg_muted))
                    .child(title),
            )
            .child(status)
            .child(actions)
            .into_any_element()
    }
}
