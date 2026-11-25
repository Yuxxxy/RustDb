//! 整库逻辑导出面板（F-76）：结构/数据/对象选项与流式导出。
use super::*;
impl MainView {
    /// F-76：打开逻辑导出面板（当前库）。
    pub(crate) fn open_dump_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ns) = self
            .current_ns
            .clone()
            .or_else(|| self.active().and_then(|tab| tab.ns.clone()))
        else {
            self.set_status("请先选择数据库", StatusKind::Error, cx);
            return;
        };
        self.open_dump_panel_for(ns, window, cx);
    }

    /// F-76：对指定库打开逻辑导出面板（库右键「转储 SQL 文件…」）。
    /// 表清单来自已加载元数据，默认全选。
    pub(crate) fn open_dump_panel_for(
        &mut self,
        ns: Namespace,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 与单表转储面板互斥，避免两个面板同时消费导出事件。
        self.table_dump = None;
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let profile = self
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let key = ns_key(&ns);
        let mut namespaces = self.namespaces.clone();
        if !namespaces
            .iter()
            .any(|item| item.display_label() == ns.display_label())
        {
            namespaces.push(ns.clone());
        }
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
                "当前库还没有可导出的表（等待元数据加载）",
                StatusKind::Error,
                cx,
            );
            return;
        }
        self.dump = Some(DumpPanel {
            session,
            profile,
            namespace: ns,
            selected: tables.iter().cloned().collect(),
            tables,
            namespaces,
            export_all: false,
            options: DumpOptions::default(),
            running: false,
            total: 0,
            done: 0,
            rows: 0,
            objects: 0,
            failed: 0,
            current: None,
            path: None,
            bytes: 0,
            error: None,
        });
        cx.notify();
    }

    /// 提交导出。文件名沿用结果导出目录：`rustdb-dump-<秒>.sql[.gz]`。
    pub(crate) fn start_dump(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.dump.clone() else {
            return;
        };
        if panel.running {
            return;
        }
        if panel.export_all {
            if let Some(panel) = self.dump.as_mut() {
                panel.running = true;
                panel.total = 0;
                panel.done = 0;
                panel.rows = 0;
                panel.objects = 0;
                panel.failed = 0;
                panel.current = None;
                panel.path = None;
                panel.bytes = 0;
                panel.error = None;
            }
            self.core.request_dump_namespaces(
                &panel.session,
                panel.namespaces.clone(),
                panel.options,
                crate::csv::export_dir(),
            );
            self.set_status(
                format!("正在按库导出 {} 个文件…", panel.namespaces.len()),
                StatusKind::Busy,
                cx,
            );
            return;
        }
        let tables = panel.selected_tables();
        if tables.is_empty() {
            if let Some(panel) = self.dump.as_mut() {
                panel.error = Some("至少选择一张表".into());
            }
            cx.notify();
            return;
        }
        let extension = if panel.options.gzip { "sql.gz" } else { "sql" };
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let path = crate::csv::export_dir().join(format!("rustdb-dump-{secs}.{extension}"));
        if let Some(panel) = self.dump.as_mut() {
            panel.running = true;
            panel.total = tables.len();
            panel.done = 0;
            panel.rows = 0;
            panel.objects = 0;
            panel.failed = 0;
            panel.current = None;
            panel.path = None;
            panel.bytes = 0;
            panel.error = None;
        }
        self.core.request_dump(
            &panel.session,
            panel.namespace.clone(),
            tables,
            panel.options,
            path,
        );
        self.set_status("正在导出…", StatusKind::Busy, cx);
    }

    /// F-76：逻辑导出面板（居中模态）。
    pub(crate) fn render_dump_panel(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(panel) = self.dump.clone() else {
            return div().into_any_element();
        };
        let header = h_flex()
            .h(px(30.))
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::Download)
                    .small()
                    .text_color(hex(t.accent)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .child(SharedString::from(format!(
                        "逻辑导出 · {} · {}",
                        panel.profile,
                        panel.namespace.display_label()
                    ))),
            )
            .child(
                div()
                    .id("dump-close")
                    .debug_selector(|| "dump-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.dump = None;
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
        let chip = |id: &'static str, text: &'static str, on: bool| {
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
                .child(text)
        };
        let options = h_flex()
            .gap_2()
            .items_center()
            .child(
                chip("dump-structure", "结构", panel.options.include_structure).on_click(
                    cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            panel.options.include_structure = !panel.options.include_structure;
                        }
                        cx.notify();
                    }),
                ),
            )
            .child(
                chip("dump-data", "数据", panel.options.include_data).on_click(cx.listener(
                    |view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            panel.options.include_data = !panel.options.include_data;
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                chip("dump-objects", "其它对象", panel.options.include_objects).on_click(
                    cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            panel.options.include_objects = !panel.options.include_objects;
                        }
                        cx.notify();
                    }),
                ),
            )
            .child(
                chip("dump-drops", "含 DROP", panel.options.include_drops).on_click(cx.listener(
                    |view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            panel.options.include_drops = !panel.options.include_drops;
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                chip("dump-gzip", "gzip", panel.options.gzip).on_click(cx.listener(
                    |view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            panel.options.gzip = !panel.options.gzip;
                        }
                        cx.notify();
                    },
                )),
            )
            .child(
                chip(
                    "dump-abort",
                    "失败即中止",
                    panel.options.error_mode == rustdb_core::DumpErrorMode::Abort,
                )
                .on_click(cx.listener(|view, _, _, cx| {
                    if let Some(panel) = view.dump.as_mut() {
                        panel.options.error_mode =
                            if panel.options.error_mode == rustdb_core::DumpErrorMode::Abort {
                                rustdb_core::DumpErrorMode::Comment
                            } else {
                                rustdb_core::DumpErrorMode::Abort
                            };
                    }
                    cx.notify();
                })),
            );
        let scope = h_flex()
            .gap_2()
            .items_center()
            .when(panel.namespaces.len() > 1, |row| {
                row.child(
                    chip("dump-scope-all", "全部库（分文件）", panel.export_all).on_click(
                        cx.listener(|view, _, _, cx| {
                            if let Some(panel) = view.dump.as_mut() {
                                panel.export_all = !panel.export_all;
                            }
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_muted))
                        .child(SharedString::from(format!(
                            "{} 个库 / Schema",
                            panel.namespaces.len()
                        ))),
                )
            });
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
                    .id("dump-toggle-all")
                    .debug_selector(|| "dump-toggle-all".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.accent))
                    .cursor_pointer()
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            if panel.all_selected() {
                                panel.selected.clear();
                            } else {
                                panel.selected = panel.tables.iter().cloned().collect();
                            }
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
            .max_h(px(150.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        for (index, table) in panel.tables.iter().enumerate() {
            let on = panel.selected.contains(table);
            let name = table.clone();
            table_list = table_list.child(
                h_flex()
                    .id(("dump-table", index))
                    .debug_selector(move || format!("dump-table-{index}"))
                    .h(px(20.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.dump.as_mut() {
                            if !panel.selected.remove(&name) {
                                panel.selected.insert(name.clone());
                            }
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
        let selection: gpui_kit::AnyElement = if panel.export_all {
            div()
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(t.fg_muted))
                .child("全部库各写一个文件；表选择不生效。")
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .child(select_all)
                .child(table_list)
                .into_any_element()
        };
        let progress = div()
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
                    "正在导出 {}/{} 表 · {} 行 · {} 个对象{}",
                    panel.done,
                    panel.total,
                    panel.rows,
                    panel.objects,
                    panel
                        .current
                        .as_ref()
                        .map(|table| format!(" · {table}"))
                        .unwrap_or_default()
                )
            } else if let Some(path) = &panel.path {
                format!(
                    "已写入 {} · {} 行 · {} 个对象{} · {}",
                    path.display(),
                    panel.rows,
                    panel.objects,
                    if panel.failed > 0 {
                        format!(" · {} 项失败", panel.failed)
                    } else {
                        String::new()
                    },
                    rustdb_core::human_bytes(panel.bytes)
                )
            } else {
                "SQL 可直接回放；导出不是一致性快照。".to_string()
            }));
        let actions = h_flex()
            .gap_2()
            .items_center()
            .child(
                div().debug_selector(|| "dump-run".to_string()).child(
                    Button::new("dump-run-btn")
                        .small()
                        .primary()
                        .icon(IconName::Download)
                        .label(if panel.running {
                            "导出中…"
                        } else {
                            "开始导出"
                        })
                        .disabled(panel.running)
                        .on_click(cx.listener(|view, _, _, cx| view.start_dump(cx))),
                ),
            )
            .child(div().flex_1())
            .child(
                Button::new("dump-close-btn")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.dump = None;
                        cx.notify();
                    })),
            );
        v_flex()
            .id("dump-panel")
            .debug_selector(|| "dump-panel".to_string())
            .occlude()
            .w(px(460.))
            .max_h(px(520.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(options)
            .child(scope)
            .child(progress)
            .child(selection)
            .child(actions)
            .into_any_element()
    }
}

impl MainView {
    /// Navicat 式「转储 SQL 文件」入口：单表面板，二选一「结构和数据 / 仅结构」。
    pub(crate) fn open_table_dump(
        &mut self,
        path: rustdb_db_api::ObjectPath,
        cx: &mut Context<Self>,
    ) {
        self.obj_menu = None;
        // 与整库导出面板互斥，避免两个面板同时消费导出事件。
        self.dump = None;
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let safe: String = path
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let dest = crate::csv::export_dir().join(format!("{safe}.sql"));
        self.table_dump = Some(TableDumpPanel {
            path,
            session,
            with_data: true,
            include_drops: true,
            dest,
            running: false,
            finished: false,
            rows: 0,
            bytes: 0,
            failed: 0,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn table_dump_set_data(&mut self, with_data: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.table_dump.as_mut() {
            panel.with_data = with_data;
            panel.finished = false;
        }
        cx.notify();
    }

    pub(crate) fn table_dump_toggle_drops(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.table_dump.as_mut() {
            panel.include_drops = !panel.include_drops;
            panel.finished = false;
        }
        cx.notify();
    }

    /// 真正落盘：复用 F-76 流式导出引擎，按 Navicat 风格写 DROP / CREATE / INSERT。
    /// 保存位置在点「开始转储」时由系统保存对话框确定，面板里不再单独提供路径行。
    pub(crate) fn start_table_dump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.table_dump.clone() else {
            return;
        };
        if panel.running {
            return;
        }
        let dir = panel
            .dest
            .parent()
            .map(|parent| parent.to_path_buf())
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let suggested = panel
            .dest
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        let prompt = cx.prompt_for_new_path(&dir, suggested.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let _ = this.update_in(cx, |view, _, cx| {
                let Some(panel) = view.table_dump.as_mut() else {
                    return;
                };
                panel.dest = path.clone();
                panel.running = true;
                panel.finished = false;
                panel.rows = 0;
                panel.bytes = 0;
                panel.failed = 0;
                panel.error = None;
                let options = rustdb_core::DumpOptions {
                    include_structure: true,
                    include_data: panel.with_data,
                    include_objects: false,
                    include_drops: panel.include_drops,
                    gzip: false,
                    batch_rows: rustdb_core::DEFAULT_INSERT_BATCH,
                    error_mode: rustdb_core::DumpErrorMode::Comment,
                    style: rustdb_core::DumpStyle::Navicat,
                };
                let session = panel.session.clone();
                let ns = panel.path.ns.clone();
                let table = panel.path.name.to_string();
                let label = panel.path.name.to_string();
                view.core
                    .request_dump(&session, ns, vec![table], options, path);
                view.set_status(format!("正在转储 {label}…"), StatusKind::Busy, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Navicat 式转储面板（居中模态）。
    pub(crate) fn render_table_dump_panel(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let Some(panel) = self.table_dump.clone() else {
            return div().into_any_element();
        };
        let chip = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .h(px(22.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .border_1()
                .border_color(hex(if on { t.accent } else { t.border_subtle }))
                .when(on, |node| node.bg(crate::theme::to_hsla(t.accent).opacity(0.12)))
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(if on { t.accent } else { t.fg_muted }))
                .child(label)
        };
        let header = h_flex()
            .h(px(30.))
            .gap_2()
            .items_center()
            .child(Icon::new(IconName::Download).small().text_color(hex(t.accent)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_ellipsis()
                    .child(SharedString::from(format!(
                        "转储 SQL 文件 · {}",
                        panel.path.display_label()
                    ))),
            )
            .child(
                div()
                    .id("table-dump-close")
                    .debug_selector(|| "table-dump-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.table_dump = None;
                        cx.notify();
                    }))
                    .child(Icon::new(IconName::Close).small().text_color(hex(t.fg_muted))),
            );
        let modes = v_flex()
            .gap_1()
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("导出内容"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        chip("table-dump-with-data", "结构和数据", panel.with_data).on_click(
                            cx.listener(|view, _, _, cx| view.table_dump_set_data(true, cx)),
                        ),
                    )
                    .child(
                        chip("table-dump-structure-only", "仅结构", !panel.with_data).on_click(
                            cx.listener(|view, _, _, cx| view.table_dump_set_data(false, cx)),
                        ),
                    )
                    .child(
                        chip("table-dump-drops", "含 DROP TABLE", panel.include_drops).on_click(
                            cx.listener(|view, _, _, cx| view.table_dump_toggle_drops(cx)),
                        ),
                    ),
            );
        let progress = div()
            .min_h(px(28.))
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
                format!("正在写入 {} · {} 行…", panel.dest.display(), panel.rows)
            } else if panel.finished {
                format!(
                    "已写入 {} · {} 行{} · {}",
                    panel.dest.display(),
                    panel.rows,
                    if panel.failed > 0 {
                        format!(" · {} 项失败", panel.failed)
                    } else {
                        String::new()
                    },
                    rustdb_core::human_bytes(panel.bytes)
                )
            } else {
                "生成可直接回放的 SQL 文件；导出不是一致性快照。".to_string()
            }));
        let actions = h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .debug_selector(|| "table-dump-run".to_string())
                    .child(
                        Button::new("table-dump-run-btn")
                            .small()
                            .primary()
                            .icon(IconName::Download)
                            .label(if panel.running {
                                "转储中…"
                            } else {
                                "开始转储"
                            })
                            .disabled(panel.running)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.start_table_dump(window, cx);
                            })),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("table-dump-close-btn")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.table_dump = None;
                        cx.notify();
                    })),
            );
        v_flex()
            .id("table-dump-panel")
            .debug_selector(|| "table-dump-panel".to_string())
            .occlude()
            .w(px(480.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(modes)
            .child(progress)
            .child(actions)
            .into_any_element()
    }
}

/// 把导出事件同步到单表转储面板；返回状态栏文案。
pub(crate) fn update_table_dump(
    panel: &mut TableDumpPanel,
    event: &DumpEvent,
) -> Option<(String, StatusKind)> {
    match event {
        DumpEvent::Started { .. } | DumpEvent::ObjectDone { .. } => None,
        DumpEvent::TableDone { table, rows, error } => {
            panel.rows += *rows;
            if let Some(error) = error {
                panel.failed += 1;
                panel.error = Some(format!("{table}：{error}"));
            }
            Some((
                format!("正在转储 {} · {} 行", panel.path.name, panel.rows),
                StatusKind::Busy,
            ))
        }
        DumpEvent::Finished {
            path,
            rows,
            failed,
            bytes,
            ..
        } => {
            panel.running = false;
            panel.finished = true;
            panel.rows = *rows;
            panel.failed = *failed;
            panel.bytes = *bytes;
            panel.dest = std::path::PathBuf::from(path);
            if *failed == 0 {
                panel.error = None;
            }
            Some((
                format!(
                    "已转储 {} · {} 行 · {}",
                    panel.dest.display(),
                    rows,
                    rustdb_core::human_bytes(*bytes)
                ),
                if *failed == 0 {
                    StatusKind::Ok
                } else {
                    StatusKind::Error
                },
            ))
        }
        DumpEvent::Failed { message } => {
            panel.running = false;
            panel.error = Some(message.clone());
            Some((format!("转储失败：{message}"), StatusKind::Error))
        }
    }
}
