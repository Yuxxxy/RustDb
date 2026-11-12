//! 数据导入面板（F-74）：文件选择、映射、选项与分批执行。
//!
//! 从 `impl MainView` 的导入相关命令簇拆出，状态仍保存在 `MainView`。

use super::*;

impl MainView {
    /// 打开导入入口：先选文件，再取表结构（事件回来后弹面板）。
    pub(crate) fn start_import(
        &mut self,
        path: ObjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(
                "选择要导入的文件（CSV / TSV / JSON / SQL）",
            )),
        });
        let target_path = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = prompt.await else {
                return;
            };
            let Some(file_path) = paths.pop() else {
                return;
            };
            let reading = file_path.clone();
            let bytes = cx
                .background_executor()
                .spawn(async move { std::fs::read(&reading) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match bytes {
                Ok(bytes) => {
                    view.status = format!("正在读取 {} 的结构…", target_path.name);
                    view.status_kind = StatusKind::Busy;
                    view.import_pending = Some(ImportPending {
                        path: target_path.clone(),
                        file_path,
                        bytes,
                    });
                    view.core.request_table_detail(&session, target_path);
                    cx.notify();
                }
                Err(error) => {
                    view.set_status(format!("读取文件失败：{error}"), StatusKind::Error, cx)
                }
            });
        })
        .detach();
    }

    /// 表结构回来后建立导入面板：默认按列名映射，首屏即可看到预检结果。
    pub(crate) fn open_import_panel(&mut self, pending: ImportPending, columns: Vec<ColumnInfo>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            return;
        };
        let Some(table) = self.core.object_from_clause(&session, &pending.path) else {
            self.status = "无法生成目标表的限定名".into();
            self.status_kind = StatusKind::Error;
            return;
        };
        let mut options = ImportOptions {
            format: import::ImportFormat::from_path(&pending.file_path)
                .unwrap_or(import::ImportFormat::Csv),
            ..ImportOptions::default()
        };
        options.columns = columns
            .iter()
            .map(|column| column.meta.name.to_string())
            .collect();
        let mut panel = ImportPanel {
            path: pending.path,
            file_path: pending.file_path,
            bytes: pending.bytes,
            target: columns,
            table,
            options,
            parsed: None,
            plan: None,
            mapping: Vec::new(),
            error: None,
            warning: None,
            confirm_truncate: false,
        };
        panel.rebuild(self.session_kind());
        let status = match (&panel.error, &panel.parsed, &panel.plan) {
            (Some(error), _, _) => error.clone(),
            (_, Some(data), Some(plan)) => format!(
                "导入预览 · {} 行 · {} 批（{}）",
                data.rows.len(),
                plan.batch_count(),
                plan.table
            ),
            _ => "导入预览".to_string(),
        };
        self.status = status;
        self.status_kind = if panel.error.is_some() {
            StatusKind::Error
        } else {
            StatusKind::Ok
        };
        self.import_panel = Some(panel);
    }

    pub(crate) fn cycle_import_mapping(&mut self, source: usize, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        let count = panel.target.len();
        let next = match panel.mapping.get(source).copied().flatten() {
            None if count > 0 => Some(0),
            Some(index) if index + 1 < count => Some(index + 1),
            _ => None,
        };
        if let Some(slot) = panel.mapping.get_mut(source) {
            *slot = next;
            panel.options.mapping = panel.mapping.clone();
            panel.rebuild(kind);
        }
        cx.notify();
    }

    pub(crate) fn cycle_import_format(&mut self, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        let index = import::ImportFormat::ALL
            .iter()
            .position(|format| *format == panel.options.format)
            .unwrap_or(0);
        panel.options.format =
            import::ImportFormat::ALL[(index + 1) % import::ImportFormat::ALL.len()];
        panel.options.delimiter = None;
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn cycle_import_encoding(&mut self, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        panel.options.encoding = panel.options.encoding.next();
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn toggle_import_header(&mut self, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        panel.options.header = !panel.options.header;
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn toggle_import_trim(&mut self, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        panel.options.trim = !panel.options.trim;
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn toggle_import_empty_null(&mut self, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        panel.options.empty_as_null = !panel.options.empty_as_null;
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn cycle_import_mode(&mut self, cx: &mut Context<Self>) {
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        panel.options.mode = panel.options.mode.next();
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn cycle_import_skip(&mut self, cx: &mut Context<Self>) {
        const CHOICES: [usize; 5] = [0, 1, 2, 3, 5];
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        let index = CHOICES
            .iter()
            .position(|value| *value == panel.options.skip_rows)
            .unwrap_or(0);
        panel.options.skip_rows = CHOICES[(index + 1) % CHOICES.len()];
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn cycle_import_batch(&mut self, cx: &mut Context<Self>) {
        const CHOICES: [usize; 4] = [100, 500, 1000, 5000];
        let kind = self.session_kind();
        let Some(panel) = self.import_panel.as_mut() else {
            return;
        };
        let index = CHOICES
            .iter()
            .position(|value| *value == panel.options.batch_rows)
            .unwrap_or(1);
        panel.options.batch_rows = CHOICES[(index + 1) % CHOICES.len()];
        panel.rebuild(kind);
        cx.notify();
    }

    pub(crate) fn close_import_panel(&mut self, cx: &mut Context<Self>) {
        if self.import_panel.take().is_some() {
            self.set_status("已关闭导入", StatusKind::Idle, cx);
        }
    }

    /// 执行导入：清空模式需要第二次点击确认；成功后关闭面板。
    pub(crate) fn run_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.import_panel.as_ref() else {
            return;
        };
        let plan = panel.plan.clone();
        let needs_confirm =
            panel.options.mode == import::ImportMode::Truncate && !panel.confirm_truncate;
        let namespace = panel.path.ns.clone();
        let file = panel.file_name();
        let table = panel.path.display_label();
        let Some(plan) = plan else {
            self.set_status("导入预检未通过，无法执行", StatusKind::Error, cx);
            return;
        };
        if needs_confirm {
            if let Some(panel) = self.import_panel.as_mut() {
                panel.confirm_truncate = true;
                panel.warning = Some("清空会删除目标表现有数据：再点一次「确认清空并导入」".into());
            }
            self.status = "清空导入需要二次确认".into();
            self.status_kind = StatusKind::Warn;
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        if self.active().is_none() {
            self.open_tab(namespace, window, cx);
        }
        let query_id = match self.core.import(&session, &plan) {
            Ok(query_id) => query_id,
            Err(error) => {
                self.set_status(error, StatusKind::Error, cx);
                return;
            }
        };
        self.import_panel = None;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.running = Some(query_id);
            tab.sql = format!("-- 导入 {file} → {table}");
            tab.file_path = None;
            tab.batch = None;
            tab.failed_at = None;
            tab.current_statement = None;
            tab.sql_error = None;
        }
        self.status = format!("正在导入 {file} → {table}…");
        self.status_kind = StatusKind::Busy;
        cx.notify();
    }

    /// 导入面板（F-74）：选项、字段映射、预检摘要与执行按钮。
    pub(crate) fn render_import_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = current(cx);
        let panel = self.import_panel.as_ref()?;
        let file = panel.file_name();
        let table = panel.path.name.to_string();
        let options = panel.options.clone();
        let mapping = panel.mapping.clone();
        let sources = panel
            .parsed
            .as_ref()
            .map(|data| data.columns.clone())
            .unwrap_or_default();
        let rows = panel
            .parsed
            .as_ref()
            .map(|data| data.rows.len())
            .unwrap_or(0);
        let warnings = panel
            .parsed
            .as_ref()
            .map(|data| data.warnings.clone())
            .unwrap_or_default();
        let error = panel.error.clone();
        let warning = panel.warning.clone();
        let confirm = panel.confirm_truncate;
        let plan_summary = panel
            .plan
            .as_ref()
            .map(|plan| format!("{} 行 · {} 批", plan.total_rows, plan.batch_count()));
        let targets: Vec<(String, String)> = panel
            .target
            .iter()
            .map(|column| {
                (
                    column.meta.name.to_string(),
                    import::type_label(&column.meta),
                )
            })
            .collect();
        let target_len = targets.len();

        let mut option_row = h_flex().items_center().gap_1().flex_wrap();
        option_row = option_row
            .child(import_option_chip(
                "import-format",
                format!("格式 · {}", options.format.label()),
                "切换文件格式",
                cx.listener(|view, _, _, cx| view.cycle_import_format(cx)),
            ))
            .child(import_option_chip(
                "import-encoding",
                format!("编码 · {}", options.encoding.label()),
                "自动：BOM → UTF-8 → GBK",
                cx.listener(|view, _, _, cx| view.cycle_import_encoding(cx)),
            ))
            .child(import_option_chip(
                "import-mode",
                format!("模式 · {}", options.mode.label()),
                "追加或清空后导入（清空需二次确认）",
                cx.listener(|view, _, _, cx| view.cycle_import_mode(cx)),
            ));
        option_row = option_row
            .child(import_option_chip(
                "import-header",
                format!("表头 · {}", if options.header { "是" } else { "否" }),
                "第一条记录是否作为列名",
                cx.listener(|view, _, _, cx| view.toggle_import_header(cx)),
            ))
            .child(import_option_chip(
                "import-trim",
                format!("去空白 · {}", if options.trim { "开" } else { "关" }),
                "去除字段首尾空白",
                cx.listener(|view, _, _, cx| view.toggle_import_trim(cx)),
            ))
            .child(import_option_chip(
                "import-empty",
                format!(
                    "空→NULL · {}",
                    if options.empty_as_null { "开" } else { "关" }
                ),
                "空串按 NULL 写入",
                cx.listener(|view, _, _, cx| view.toggle_import_empty_null(cx)),
            ))
            .child(import_option_chip(
                "import-skip",
                format!("跳过 {} 行", options.skip_rows),
                "跳过文件开头的说明行",
                cx.listener(|view, _, _, cx| view.cycle_import_skip(cx)),
            ))
            .child(import_option_chip(
                "import-batch",
                format!("{} 行/批", options.batch_rows),
                "每批 INSERT 的行数",
                cx.listener(|view, _, _, cx| view.cycle_import_batch(cx)),
            ));

        let mut map_rows = v_flex().gap(px(2.));
        for (index, source) in sources.iter().enumerate() {
            let target = mapping
                .get(index)
                .copied()
                .flatten()
                .and_then(|target| targets.get(target))
                .map(|(name, kind)| format!("{name} · {kind}"))
                .unwrap_or_else(|| "忽略".to_string());
            map_rows = map_rows.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(128.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(source.clone())),
                    )
                    .child(
                        div()
                            .debug_selector({
                                let id = format!("import-map-{index}");
                                move || id.clone()
                            })
                            .child(
                                Button::new(SharedString::from(format!("import-map-{index}")))
                                    .small()
                                    .ghost()
                                    .label(SharedString::from(target))
                                    .tooltip("点击切换目标列；循环到「忽略」表示不导入")
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.cycle_import_mapping(index, cx)
                                    })),
                            ),
                    ),
            );
        }

        let run_label = if confirm {
            "确认清空并导入"
        } else {
            "导入"
        };

        Some(
            v_flex()
                .id("import-panel")
                .debug_selector(|| "import-panel".to_string())
                .flex_none()
                .px_2()
                .py_2()
                .gap_2()
                .border_b_1()
                .border_color(hex(t.border_subtle))
                .bg(hex(t.bg_elevated))
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::FolderOpen)
                                .small()
                                .text_color(hex(t.accent)),
                        )
                        .child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_primary))
                                .child(SharedString::from(format!("导入 · {table}"))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .text_ellipsis()
                                .child(SharedString::from(file)),
                        )
                        .child(
                            Button::new("import-cancel")
                                .small()
                                .ghost()
                                .label("取消")
                                .on_click(
                                    cx.listener(|view, _, _, cx| view.close_import_panel(cx)),
                                ),
                        )
                        .child(
                            div().debug_selector(|| "import-run".to_string()).child(
                                Button::new("import-run-btn")
                                    .small()
                                    .primary()
                                    .label(run_label)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.run_import(window, cx)
                                    })),
                            ),
                        ),
                )
                .child(option_row)
                .child(
                    v_flex()
                        .max_h(px(118.))
                        .overflow_y_scrollbar()
                        .child(map_rows),
                )
                .when_some(plan_summary, |panel, summary| {
                    panel.child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(format!(
                                "{summary} · 源 {rows} 行 · 目标 {target_len}"
                            ))),
                    )
                })
                .when_some(error, |panel, error| {
                    panel.child(
                        div()
                            .debug_selector(|| "import-error".to_string())
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.danger))
                            .child(SharedString::from(error)),
                    )
                })
                .when_some(warning, |panel, warning| {
                    panel.child(
                        div()
                            .debug_selector(|| "import-warning".to_string())
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.danger))
                            .child(SharedString::from(warning)),
                    )
                })
                .when(!warnings.is_empty(), |panel| {
                    let text = warnings
                        .iter()
                        .take(2)
                        .map(import::ImportIssue::line)
                        .collect::<Vec<_>>()
                        .join("；");
                    panel.child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(text)),
                    )
                })
                .into_any_element(),
        )
    }
}
