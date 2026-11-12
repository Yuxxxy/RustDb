//! 数据对比与同步面板（F-78）：差异计数、脚本预览与同步执行。
use super::*;
impl MainView {
    /// F-78：打开数据对比面板。源 = 当前库，目标按连接配置临时建链。
    pub(crate) fn open_data_compare_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                "当前库还没有可对比的表（等待元数据加载）",
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
        let keys = designer_input(
            window,
            cx,
            "留空 = 源表主键；逗号分隔，如 tenant_id, id",
            None,
        );
        self.data_compare = Some(DataComparePanel {
            session,
            profile_id,
            profile,
            namespace: ns,
            selected: tables.iter().cloned().collect(),
            tables,
            targets,
            target,
            keys,
            options: DataCompareOptions::default(),
            previewing: false,
            preview_ready: false,
            preview_stale: false,
            lines: Vec::new(),
            warnings: Vec::new(),
            insert: 0,
            update: 0,
            delete: 0,
            running: false,
            applying: false,
            total: 0,
            done: 0,
            failed: 0,
            failures: Vec::new(),
            current: None,
            applied: 0,
            statements: 0,
            elapsed_ms: 0,
            error: None,
            config_note: None,
        });
        cx.notify();
    }

    /// 由面板状态构造对比请求；关键字段应用到所有选中表。
    pub(crate) fn data_compare_request(
        &self,
        panel: &DataComparePanel,
        cx: &gpui::App,
    ) -> Result<DataCompareRequest, String> {
        let target_id = panel
            .target
            .clone()
            .ok_or_else(|| "请选择目标连接".to_string())?;
        let profile = self
            .profiles
            .iter()
            .find(|item| item.id == target_id)
            .ok_or_else(|| format!("找不到目标连接 {target_id}"))?;
        let selected = panel.selected_tables();
        if selected.is_empty() {
            return Err("至少选择一张表".into());
        }
        let keys = panel.parse_keys(cx);
        let tables = selected
            .into_iter()
            .map(|source| TableCompareConfig {
                source,
                target: None,
                key_columns: keys.clone(),
                columns: Vec::new(),
                ignore_columns: Vec::new(),
            })
            .collect();
        Ok(DataCompareRequest {
            source_session: panel.session.clone(),
            target_profile: target_id,
            source_ns: panel.namespace.clone(),
            target_ns: profile.default_namespace(),
            tables,
            options: panel.options.clone(),
        })
    }

    pub(crate) fn data_compare_config(
        &self,
        panel: &DataComparePanel,
        cx: &gpui::App,
    ) -> Result<DataCompareConfig, String> {
        let target_id = panel
            .target
            .clone()
            .ok_or_else(|| "请选择目标连接".to_string())?;
        let profile = self
            .profiles
            .iter()
            .find(|item| item.id == target_id)
            .ok_or_else(|| format!("找不到目标连接 {target_id}"))?;
        let keys = panel.parse_keys(cx);
        let config = DataCompareConfig {
            version: rustdb_core::DATA_COMPARE_CONFIG_VERSION,
            source_profile: Some(panel.profile_id.clone()),
            target_profile: Some(target_id),
            source_ns: panel.namespace.clone(),
            target_ns: profile.default_namespace(),
            tables: panel
                .selected_tables()
                .into_iter()
                .map(|source| TableCompareConfig {
                    source,
                    target: None,
                    key_columns: keys.clone(),
                    columns: Vec::new(),
                    ignore_columns: Vec::new(),
                })
                .collect(),
            options: panel.options.clone(),
        };
        config.validate()?;
        if config.tables.is_empty() {
            return Err("至少选择一张表".into());
        }
        Ok(config)
    }

    /// 生成差异与同步脚本预览（只读，不写目标）。
    pub(crate) fn preview_data_compare(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.data_compare.clone() else {
            return;
        };
        if panel.previewing || panel.running {
            return;
        }
        let request = match self.data_compare_request(&panel, cx) {
            Ok(request) => request,
            Err(message) => {
                if let Some(panel) = self.data_compare.as_mut() {
                    panel.error = Some(message.clone());
                }
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        if let Some(panel) = self.data_compare.as_mut() {
            panel.previewing = true;
            panel.preview_ready = false;
            panel.preview_stale = false;
            panel.lines.clear();
            panel.warnings.clear();
            panel.error = None;
            panel.config_note = None;
        }
        self.core.request_data_compare(request);
        self.set_status("正在对比并生成同步脚本…", StatusKind::Busy, cx);
    }

    /// 执行同步；必须先看过差异与脚本预览。
    pub(crate) fn start_data_compare_sync(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.data_compare.clone() else {
            return;
        };
        if panel.running || panel.previewing {
            return;
        }
        if !panel.preview_ready {
            if let Some(panel) = self.data_compare.as_mut() {
                panel.error = Some("请先生成并查看差异脚本".into());
            }
            self.set_status("请先生成并查看差异脚本", StatusKind::Error, cx);
            return;
        }
        let request = match self.data_compare_request(&panel, cx) {
            Ok(request) => request,
            Err(message) => {
                if let Some(panel) = self.data_compare.as_mut() {
                    panel.error = Some(message.clone());
                }
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        if let Some(panel) = self.data_compare.as_mut() {
            panel.running = true;
            panel.applying = true;
            panel.done = 0;
            panel.failed = 0;
            panel.failures.clear();
            panel.current = None;
            panel.applied = 0;
            panel.statements = 0;
            panel.elapsed_ms = 0;
            panel.error = None;
        }
        self.core.request_data_compare_sync(request);
        self.set_status("正在同步差异…", StatusKind::Busy, cx);
    }

    /// 保存对比配置到默认位置（表清单、关键字段与动作选项）。
    pub(crate) fn save_data_compare_config(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.data_compare.clone() else {
            return;
        };
        let config = match self.data_compare_config(&panel, cx) {
            Ok(config) => config,
            Err(message) => {
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        let path = self.data_compare_config_path.clone();
        match rustdb_core::save_data_compare_config(&path, &config) {
            Ok(()) => {
                if let Some(panel) = self.data_compare.as_mut() {
                    panel.config_note = Some(format!("已保存到 {}", path.display()));
                }
                self.set_status(
                    format!("对比配置已保存：{}", path.display()),
                    StatusKind::Ok,
                    cx,
                );
            }
            Err(error) => self.set_status(format!("保存配置失败：{error}"), StatusKind::Error, cx),
        }
    }

    /// 导出对比配置 JSON。
    pub(crate) fn export_data_compare_config(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.data_compare.clone() else {
            return;
        };
        let config = match self.data_compare_config(&panel, cx) {
            Ok(config) => config,
            Err(message) => {
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        let text = match serde_json::to_string_pretty(&config) {
            Ok(text) => text,
            Err(error) => {
                self.set_status(format!("序列化配置失败：{error}"), StatusKind::Error, cx);
                return;
            }
        };
        let dir = std::env::current_dir().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some("data-compare.json"));
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

    /// 导入对比配置：选中表、关键字段与动作选项一并恢复。
    pub(crate) fn import_data_compare_config(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("选择数据对比配置 JSON")),
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
                .spawn(async move { rustdb_core::import_data_compare_config(&reading) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| match result {
                Ok(config) => {
                    let Some(panel) = view.data_compare.as_mut() else {
                        return;
                    };
                    let selected: HashSet<String> = config
                        .tables
                        .iter()
                        .map(|table| table.source.clone())
                        .filter(|source| panel.tables.contains(source))
                        .collect();
                    if selected.is_empty() {
                        panel.config_note = Some("配置里的表不在当前库里".into());
                        view.set_status("配置里的表不在当前库里", StatusKind::Error, cx);
                        return;
                    }
                    panel.selected = selected;
                    let keys = config
                        .tables
                        .first()
                        .map(|table| table.key_columns.join(", "))
                        .unwrap_or_default();
                    panel
                        .keys
                        .update(cx, |state, cx| state.set_value(keys, window, cx));
                    panel.options = config.options.clone();
                    if let Some(target) = config
                        .target_profile
                        .filter(|id| panel.targets.iter().any(|(target, _)| target == id))
                    {
                        panel.target = Some(target);
                    }
                    panel.reset_preview();
                    panel.config_note = Some(format!(
                        "已导入 {} 张表（{}）",
                        panel.selected.len(),
                        path.display()
                    ));
                    let count = panel.selected.len();
                    view.set_status(format!("已导入对比配置：{count} 张表"), StatusKind::Ok, cx);
                }
                Err(message) => view.set_status(message, StatusKind::Error, cx),
            });
        })
        .detach();
    }

    /// F-78：数据对比与同步面板（居中模态）。
    pub(crate) fn render_data_compare_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.data_compare.clone() else {
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
                Icon::new(IconName::CopyPlus)
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
                        "数据对比 · {} · {}",
                        panel.profile,
                        panel.namespace.display_label()
                    ))),
            )
            .child(
                div()
                    .id("data-compare-close")
                    .debug_selector(|| "data-compare-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.data_compare = None;
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
                .text_size(px(crate::design::TS_CAPTION))
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
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(if on { t.accent } else { t.fg_muted }))
                .child(SharedString::from(text))
        };
        let mut target_row = h_flex().gap_1().items_center().flex_wrap();
        for (index, (id, name)) in panel.targets.iter().enumerate() {
            let on = panel.target.as_deref() == Some(id.as_str());
            let target_id = id.clone();
            target_row = target_row.child(
                h_flex()
                    .id(("dc-target", index))
                    .debug_selector(move || format!("dc-target-{index}"))
                    .h(px(18.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(hex(if on { t.accent } else { t.border_subtle }))
                    .when(on, |chip| chip.bg(hex(t.bg_overlay)))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(if on { t.accent } else { t.fg_muted }))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.data_compare.as_mut() {
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
                                .text_size(px(crate::design::TS_CAPTION))
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
                chip("dc-insert", "INSERT".into(), panel.options.insert).on_click(cx.listener(
                    |view, _, _, cx| {
                        if let Some(panel) = view.data_compare.as_mut() {
                            panel.options.insert = !panel.options.insert;
                            panel.reset_preview();
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                chip("dc-update", "UPDATE".into(), panel.options.update).on_click(cx.listener(
                    |view, _, _, cx| {
                        if let Some(panel) = view.data_compare.as_mut() {
                            panel.options.update = !panel.options.update;
                            panel.reset_preview();
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                chip("dc-delete", "DELETE".into(), panel.options.delete).on_click(cx.listener(
                    |view, _, _, cx| {
                        if let Some(panel) = view.data_compare.as_mut() {
                            panel.options.delete = !panel.options.delete;
                            panel.reset_preview();
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                chip(
                    "dc-case",
                    format!("大小写 {}", panel.options.case.label()),
                    panel.options.case != rustdb_core::CaseStrategy::Preserve,
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some(panel) = view.data_compare.as_mut() {
                        panel.options.case = panel.options.case.next();
                        panel.reset_preview();
                    }
                    cx.notify();
                })),
            )
            .child(
                chip(
                    "dc-batch",
                    format!("每批 {}", panel.options.batch_rows),
                    panel.options.batch_rows != rustdb_core::DEFAULT_DIFF_BATCH_ROWS,
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(panel) = view.data_compare.as_mut() {
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
                    "dc-timeout",
                    timeout_label,
                    panel.options.timeout_ms.is_none(),
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(panel) = view.data_compare.as_mut() {
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
            );
        let keys_row = h_flex()
            .gap_2()
            .items_center()
            .child(label("关键字段"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&panel.keys).small()),
            )
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("留空 = 主键"),
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
                    .id("dc-toggle-all")
                    .debug_selector(|| "dc-toggle-all".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.data_compare.as_mut() {
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
            .h(px((panel.tables.len() as f32 * 21.).min(92.)))
            .flex_shrink_0()
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        for (index, table) in panel.tables.iter().enumerate() {
            let on = panel.selected.contains(table);
            let name = table.clone();
            table_list = table_list.child(
                h_flex()
                    .id(("dc-table", index))
                    .debug_selector(move || format!("dc-table-{index}"))
                    .h(px(20.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.data_compare.as_mut() {
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
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(if on { t.accent } else { t.border_subtle }))
                            .child(if on { "✓" } else { "·" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_ellipsis()
                            .child(SharedString::from(table.clone())),
                    ),
            );
        }
        let config_row = h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .id("dc-save-config")
                    .debug_selector(|| "dc-save-config".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, _, cx| view.save_data_compare_config(cx)))
                    .child("保存配置"),
            )
            .child(
                div()
                    .id("dc-export-config")
                    .debug_selector(|| "dc-export-config".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.export_data_compare_config(window, cx)
                    }))
                    .child("导出"),
            )
            .child(
                div()
                    .id("dc-import-config")
                    .debug_selector(|| "dc-import-config".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.import_data_compare_config(window, cx)
                    }))
                    .child("导入"),
            )
            .when_some(panel.config_note.clone(), |row, note| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_muted))
                        .text_ellipsis()
                        .child(SharedString::from(note)),
                )
            });
        let mut preview = v_flex()
            .gap(px(1.))
            .max_h(px(170.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        if panel.lines.is_empty() {
            let hint = if panel.previewing {
                "正在读取两侧数据并生成差异…"
            } else if panel.running {
                "正在同步…"
            } else {
                "先生成差异（只读），确认后再同步。"
            };
            preview = preview.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(hint)),
            );
            for (index, warning) in panel.warnings.iter().enumerate() {
                preview = preview.child(
                    div()
                        .id(("dc-warning", index))
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.warning))
                        .whitespace_normal()
                        .child(SharedString::from(format!("警告：{warning}"))),
                );
            }
        } else {
            for (index, line) in panel.lines.iter().enumerate() {
                preview = preview.child(
                    div()
                        .id(("dc-preview-line", index))
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(if line.starts_with("警告：") {
                            hex(t.warning)
                        } else {
                            hex(t.fg_secondary)
                        })
                        .whitespace_normal()
                        .child(SharedString::from(line.clone())),
                );
            }
        }
        let progress = div()
            .flex_shrink_0()
            .text_size(px(crate::design::TS_CAPTION))
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
                    "同步中 · {}/{} 表 · {}/{} 条{}",
                    panel.done,
                    panel.total,
                    panel.applied,
                    panel.statements,
                    panel
                        .current
                        .as_ref()
                        .map(|table| format!(" · {table}"))
                        .unwrap_or_default()
                )
            } else if panel.done > 0 {
                format!(
                    "已对比 {}/{} 表 · 新增 {} / 更新 {} / 删除 {}{} · {:.1}s{}",
                    panel.done,
                    panel.total,
                    panel.insert,
                    panel.update,
                    panel.delete,
                    if panel.failed > 0 {
                        format!(" · {} 个失败", panel.failed)
                    } else {
                        String::new()
                    },
                    panel.elapsed_ms as f64 / 1000.0,
                    if panel.preview_ready {
                        ""
                    } else {
                        " · 请重新生成差异"
                    }
                )
            } else if panel.preview_ready {
                format!(
                    "差异已生成 · 新增 {} / 更新 {} / 删除 {}；确认脚本后可执行同步。",
                    panel.insert, panel.update, panel.delete
                )
            } else {
                match panel.target_label() {
                    Some(target) => format!("源 → {target}；先对比再同步。"),
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
                        .id(("dc-failure", index))
                        .debug_selector(move || format!("dc-failure-{index}"))
                        .text_size(px(crate::design::TS_CAPTION))
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
                div().debug_selector(|| "dc-preview".to_string()).child(
                    Button::new("dc-preview-btn")
                        .small()
                        .ghost()
                        .icon(IconName::Eye)
                        .label(if panel.previewing {
                            "对比中…"
                        } else {
                            "生成差异"
                        })
                        .disabled(panel.previewing || panel.running)
                        .on_click(cx.listener(|view, _, _, cx| view.preview_data_compare(cx))),
                ),
            )
            .child(
                div().debug_selector(|| "dc-sync".to_string()).child(
                    Button::new("dc-sync-btn")
                        .small()
                        .primary()
                        .icon(IconName::CopyPlus)
                        .label(if panel.running {
                            "同步中…"
                        } else {
                            "执行同步"
                        })
                        .disabled(!panel.preview_ready || panel.running || panel.previewing)
                        .on_click(cx.listener(|view, _, _, cx| view.start_data_compare_sync(cx))),
                ),
            )
            .child(div().flex_1())
            .child(
                Button::new("dc-close-btn")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.data_compare = None;
                        cx.notify();
                    })),
            );
        v_flex()
            .id("data-compare-panel")
            .debug_selector(|| "data-compare-panel".to_string())
            .occlude()
            .w(px(600.))
            .h(px(440.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(target_section)
            .child(options_row)
            .child(keys_row)
            .child(select_all)
            .child(table_list)
            .child(config_row)
            .child(preview)
            .child(progress)
            .child(failure_list)
            .child(div().flex_1())
            .child(actions)
            .into_any_element()
    }
}
