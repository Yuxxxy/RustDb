//! Rendering for `MainView`.
//!
//! Split out of `main_view.rs` to keep state/mutation and layout code apart.
//! Methods and helpers stay on the parent module and are pulled in via `super::*`.

use super::*;

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(key) = self.demo_sidebar.take() {
            if let Some(panel) = PanelKind::ALL.into_iter().find(|kind| kind.key() == key) {
                self.panel = panel;
                self.layout.sidebar_collapsed = false;
            }
        }
        // 事件处理阶段拿不到 window，异步拿回的 DDL 在这里落进编辑器。
        if let Some((ns, sql)) = self.pending_sql.take() {
            if self.tabs.is_empty() {
                if let Some(ns) = ns.clone() {
                    self.open_tab(ns, window, cx);
                }
            }
            if !self.tabs.is_empty() {
                self.sql_input
                    .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.sql = sql;
                }
                self.diag_dirty = true;
            }
        }
        // F-73：批量更新执行完刷新当前表数据（事件阶段拿不到 window）。
        if self.bulk_refresh {
            self.bulk_refresh = false;
            self.refresh_after_bulk(window, cx);
        }
        if self.diag_dirty {
            self.refresh_diagnostics(cx);
        }
        if let Some(panel) = self.demo_panel.clone() {
            let ready = self.objects.values().any(|objects| {
                objects
                    .iter()
                    .any(|object| object.path.kind == ObjectKind::Table)
            });
            if ready {
                self.demo_panel = None;
                match panel.as_str() {
                    "transfer" => self.open_transfer_panel(window, cx),
                    "data-compare" => self.open_data_compare_panel(window, cx),
                    "docs" => self.open_docs_panel(window, cx),
                    "lineage" => {
                        if let Some(path) = self.first_table_path() {
                            self.open_lineage_panel(path, window, cx);
                        }
                    }
                    "compare" => self.open_compare_panel(window, cx),
                    "dump" => self.open_dump_panel(window, cx),
                    "optimize" => self.open_optimize_panel(cx),
                    "settings" => self.settings_open = true,
                    "exchange" => self.open_exchange_panel(window, cx),
                    "conn-editor" => self.open_conn_editor(None, window, cx),
                    "designer" => {
                        if let Some(path) = self.first_table_path() {
                            self.design_table(path, cx);
                        }
                    }
                    "ai-models" => {
                        self.layout.ai_open = true;
                        self.ai_models_open(window, cx);
                    }
                    _ => {}
                }
            }
        }
        let t = current(cx);
        let rail = self.render_rail(cx);
        // 与 RustShell 同口径：窗口窄于 1024 时侧栏自动让位，但不改用户自己的折叠开关，
        // 窗口拉宽后按原样回来。
        let viewport_w = f32::from(window.viewport_size().width);
        let narrow = viewport_w < rustdb_core::NARROW_WINDOW;
        let sidebar_visible = !self.layout.sidebar_collapsed && !narrow;
        let ai_visible = self.layout.ai_open;
        let sidebar_w = self.layout.sidebar_width(self.panel.key());
        let ai_w = self.layout.clamped_ai_width();

        // 留白由面板自己的 padding 产生而不是外层 padding：拖拽手柄是 absolute 定位，
        // 外层 padding 会把面板连同手柄一起挤窄。左右各分一半 CARD_GAP，首尾块补满。
        let lead = usize::from(sidebar_visible);
        let total = lead + 1 + usize::from(ai_visible);
        let pad = |ix: usize| -> (f32, f32) {
            let half = CARD_GAP * 0.5;
            (
                if ix == 0 { CARD_GAP } else { half },
                if ix + 1 == total { CARD_GAP } else { half },
            )
        };

        let split_id = layout_split_id(self.panel, sidebar_visible, ai_visible);
        let panel_key = self.panel.key();
        let me_resize = cx.entity();
        let mut group = gpui_component::resizable::h_resizable(split_id)
            // 只换掉画出来的那条线，命中区与拖拽逻辑仍由组件托管。
            .with_handle_appearance(std::rc::Rc::new(|_ctx, _w: &mut Window, _cx: &mut App| {
                Some(div().into_any_element())
            }));
        if sidebar_visible {
            let (pl, pr) = pad(0);
            group = group.child(
                gpui_component::resizable::resizable_panel()
                    .size(px(sidebar_w))
                    .size_range(
                        px(rustdb_core::SIDEBAR_WIDTH_MIN)..px(rustdb_core::SIDEBAR_WIDTH_MAX),
                    )
                    .flex_none()
                    .pl(px(pl))
                    .pr(px(pr))
                    .py(px(CARD_GAP))
                    .child(
                        card_shell(t.bg_base, cx)
                            .debug_selector(|| "sidebar-card".to_string())
                            .size_full()
                            .child(self.render_sidebar(window, cx)),
                    ),
            );
        }
        {
            let (pl, pr) = pad(lead);
            group = group.child(
                gpui_component::resizable::resizable_panel()
                    .pl(px(pl))
                    .pr(px(pr))
                    .py(px(CARD_GAP))
                    .child(
                        card_shell(t.bg_base, cx)
                            .debug_selector(|| "workspace-card".to_string())
                            .size_full()
                            .child(self.render_workspace(cx)),
                    ),
            );
        }
        if ai_visible {
            let (pl, pr) = pad(lead + 1);
            group = group.child(
                gpui_component::resizable::resizable_panel()
                    .size(px(ai_w))
                    .size_range(px(rustdb_core::AI_WIDTH_MIN)..px(rustdb_core::AI_WIDTH_MAX))
                    .flex_none()
                    .pl(px(pl))
                    .pr(px(pr))
                    .py(px(CARD_GAP))
                    .child(
                        card_shell(t.bg_base, cx)
                            .debug_selector(|| "ai-card".to_string())
                            .size_full()
                            .child(self.render_ai_column(window, cx)),
                    ),
            );
        }
        let group = group.on_resize(move |state, _w, cx| {
            // 只取两块定宽面板的宽度，不必区分拖的是哪一根分界。
            // 侧栏折叠时第 0 块是工作区，那条分支必须跳过。
            let sidebar_w = sidebar_visible
                .then(|| state.read(cx).sizes().first().copied())
                .flatten();
            let ai_w = ai_visible
                .then(|| state.read(cx).sizes().last().copied())
                .flatten();
            me_resize.update(cx, |view, cx| {
                if let Some(w) = sidebar_w {
                    view.layout.remember_sidebar_width(panel_key, f32::from(w));
                }
                if let Some(w) = ai_w {
                    view.layout.ai_width = f32::from(w);
                }
                view.persist_layout();
                cx.notify();
            });
        });
        let right_col = v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(h_flex().flex_1().min_h_0().items_stretch().child(group));

        v_flex()
            .id("rustdb-root")
            .size_full()
            .justify_start()
            .bg(hex(t.bg_base))
            .text_color(hex(t.fg_primary))
            .text_size(px(13.))
            .overflow_hidden()
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .id("body-row")
                    .debug_selector(|| "body-row".to_string())
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .items_stretch()
                    .overflow_hidden()
                    .child(rail)
                    .child(right_col),
            )
            .when_some(self.db_menu, |layout, anchor| {
                let menu = self.render_db_menu(cx);
                layout
                    // 点击别处关闭：整窗遮罩 + 下拉面板
                    .child(
                        div()
                            .id("db-scrim")
                            .debug_selector(|| "db-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.db_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        anchored()
                            .position(anchor)
                            .offset(gpui::point(px(0.), px(6.)))
                            .child(menu),
                    )
            })
            .when_some(self.obj_menu.as_ref().map(|menu| menu.at), |layout, at| {
                let menu = self.render_object_menu(cx);
                layout
                    .child(
                        div()
                            .id("obj-scrim")
                            .debug_selector(|| "obj-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.obj_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(anchored().position(at).child(menu))
            })
            .when_some(self.history_menu, |layout, (at, kind)| {
                let menu = self.render_history_menu(kind, cx);
                layout
                    .child(
                        div()
                            .id("history-scrim")
                            .debug_selector(|| "history-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.history_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        anchored()
                            .position(at)
                            .offset(gpui::point(px(0.), px(4.)))
                            .child(menu),
                    )
            })
            .when_some(self.ns_menu.as_ref().map(|menu| menu.at), |layout, at| {
                let menu = self.render_ns_menu(cx);
                layout
                    .child(
                        div()
                            .id("ns-scrim")
                            .debug_selector(|| "ns-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.ns_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(anchored().position(at).child(menu))
            })
            .when_some(self.conn_menu.as_ref().map(|menu| menu.2), |layout, at| {
                let menu = self.render_conn_menu(cx);
                layout
                    .child(
                        div()
                            .id("conn-scrim")
                            .debug_selector(|| "conn-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.conn_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(anchored().position(at).child(menu))
            })
            .when_some(self.export_menu, |layout, at| {
                let menu = self.render_export_menu(cx);
                layout
                    .child(
                        div()
                            .id("export-scrim")
                            .debug_selector(|| "export-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.export_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        anchored()
                            .position(at)
                            .offset(gpui::point(px(-190.), px(6.)))
                            .child(menu),
                    )
            })
            .when_some(self.filter_menu.map(|(_, at)| at), |layout, at| {
                let menu = self.render_filter_menu(window, cx);
                layout
                    .child(
                        div()
                            .id("filter-scrim")
                            .debug_selector(|| "filter-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.filter_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(anchored().position(at).child(menu))
            })
            // F-71：行操作菜单（新增 / 克隆 / 粘贴 / 生成器 / 默认值 / 撤销）。
            .when_some(self.row_menu, |layout, at| {
                let menu = self.render_row_menu(cx);
                layout
                    .child(
                        div()
                            .id("row-menu-scrim")
                            .debug_selector(|| "row-menu-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.row_menu = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        anchored()
                            .position(at)
                            .offset(gpui::point(px(0.), px(6.)))
                            .child(menu),
                    )
            })
            // F-71：数据生成器面板（居中模态）。
            .when_some(self.generator.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_generator(window, cx);
                layout
                    .child(
                        div()
                            .id("generator-scrim")
                            .debug_selector(|| "generator-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.generator = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-73：批量更新面板（居中模态）。
            .when_some(self.bulk.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_bulk_panel(window, cx);
                layout
                    .child(
                        div()
                            .id("bulk-scrim")
                            .debug_selector(|| "bulk-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.bulk = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-88：字段血缘面板（只读，居中模态）。
            .when_some(self.lineage.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_lineage_panel(window, cx);
                layout
                    .child(
                        div()
                            .id("lineage-scrim")
                            .debug_selector(|| "lineage-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.lineage = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-86：结构对比面板（居中模态）。
            .when_some(self.compare.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_compare_panel(window, cx);
                layout
                    .child(
                        div()
                            .id("compare-scrim")
                            .debug_selector(|| "compare-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.compare = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-76：逻辑导出面板（居中模态）。
            .when_some(self.dump.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_dump_panel(cx);
                layout
                    .child(
                        div()
                            .id("dump-scrim")
                            .debug_selector(|| "dump-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.dump = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-75：跨库传输面板（居中模态）。
            .when_some(self.transfer.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_transfer_panel(cx);
                layout
                    .child(
                        div()
                            .id("transfer-scrim")
                            .debug_selector(|| "transfer-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.transfer = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-78：数据对比与同步面板（居中模态）。
            .when_some(self.data_compare.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_data_compare_panel(window, cx);
                layout
                    .child(
                        div()
                            .id("data-compare-scrim")
                            .debug_selector(|| "data-compare-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.data_compare = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-87：数据库文档面板（只读，居中模态）。
            .when_some(self.docs.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_docs_panel(window, cx);
                layout
                    .child(
                        div()
                            .id("docs-scrim")
                            .debug_selector(|| "docs-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.docs = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            .when_some(self.table_panel.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_table_panel(window, cx);
                // 非模态面板：不铺遮罩（遮罩会抢走面板内按钮的点击），用右上角 ✕ 关闭。
                layout.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(panel),
                )
            })
            // F-82：例程源码面板（非模态，右上角 ✕ 关闭）。
            .when_some(self.routine.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_routine_panel(window, cx);
                layout.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(panel),
                )
            })
            // F-85：分区管理面板（非模态，右上角 ✕ 关闭）。
            .when_some(self.partition.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_partition_panel(cx);
                layout.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(panel),
                )
            })
            // F-90：配置交换面板（非模态，右上角 ✕ 关闭）。
            .when_some(self.exchange.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_exchange_panel(cx);
                layout.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(panel),
                )
            })
            // 设置面板：低频入口集中在这里（主题 / 布局 / 工具 / 连接）。
            .when(self.settings_open, |layout| {
                let panel = self.render_settings(window, cx);
                layout
                    .child(
                        div()
                            .id("settings-scrim")
                            .debug_selector(|| "settings-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.settings_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-85：分区 DDL 预览（盖在分区面板之上）。
            .when_some(
                self.partition
                    .as_ref()
                    .and_then(|panel| panel.preview.as_ref())
                    .map(|_| ()),
                |layout, _| {
                    let preview = self.render_partition_preview(cx);
                    layout
                        .child(
                            div()
                                .id("partition-preview-scrim")
                                .debug_selector(|| "partition-preview-scrim".to_string())
                                .absolute()
                                .inset_0()
                                .bg(gpui::hsla(0., 0., 0., 0.35))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    if let Some(panel) = view.partition.as_mut() {
                                        panel.preview = None;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(preview),
                        )
                },
            )
            // F-82：保存前的 SQL 预览浮层（盖在源码面板之上）。
            .when_some(
                self.routine_save_preview.as_ref().map(|_| ()),
                |layout, _| {
                    let preview = self.render_routine_save_preview(cx);
                    layout
                        .child(
                            div()
                                .id("routine-save-preview-scrim")
                                .debug_selector(|| "routine-save-preview-scrim".to_string())
                                .absolute()
                                .inset_0()
                                .bg(gpui::hsla(0., 0., 0., 0.35))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.routine_save_preview = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(preview),
                        )
                },
            )
            // F-82：放弃脏草稿的确认框。
            .when_some(self.routine_discard.then_some(()), |layout, _| {
                let discard = self.render_routine_discard(cx);
                layout
                    .child(
                        div()
                            .id("routine-discard-scrim")
                            .debug_selector(|| "routine-discard-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.routine_discard = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(discard),
                    )
            })
            // F-02：连接编辑器浮层。
            .when_some(self.conn_editor.as_ref().map(|_| ()), |layout, _| {
                let editor = self.render_conn_editor(cx);
                layout
                    .child(
                        div()
                            .id("conn-editor-scrim")
                            .debug_selector(|| "conn-editor-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.conn_editor = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(editor),
                    )
            })
            // F-94：模型配置浮层。
            .when_some(self.ai_model_panel.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_ai_model_panel(cx);
                layout
                    .child(
                        div()
                            .id("ai-model-scrim")
                            .debug_selector(|| "ai-model-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.ai_model_panel = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-91：主机密钥确认浮层。
            .when_some(self.host_key_prompt.as_ref().map(|_| ()), |layout, _| {
                let prompt = self.render_host_key_prompt(cx);
                layout
                    .child(
                        div()
                            .id("host-key-scrim")
                            .debug_selector(|| "host-key-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35)),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(prompt),
                    )
            })
            // 单元格详情：遮罩 + 居中卡片。
            .when_some(self.cell_detail.clone(), |layout, _| {
                let panel = self.render_cell_detail(window, cx);
                layout
                    .child(
                        div()
                            .id("cell-detail-scrim")
                            .debug_selector(|| "cell-detail-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.cell_detail = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // F-92：AI 优化浮层（遮罩可点关闭；卡片自身 occlude）。
            .when_some(self.optimize.as_ref().map(|_| ()), |layout, _| {
                let panel = self.render_optimize_panel(window, cx);
                layout
                    .child(
                        div()
                            .id("ai-optimize-scrim")
                            .debug_selector(|| "ai-optimize-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.optimize = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(panel),
                    )
            })
            // 变更集 SQL 预览 / 迁移 DDL 预览共用一个浮层。
            .when_some(self.edit_preview.clone(), |layout, sql| {
                // 短脚本不必占满整块浮层；长脚本撑到上限后滚动。
                let height = 96. + sql.lines().count().max(1) as f32 * 16.;
                layout
                    .child(
                        div()
                            .id("edit-preview-scrim")
                            .debug_selector(|| "edit-preview-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.edit_preview = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                v_flex()
                                    .id("edit-preview-panel")
                                    .debug_selector(|| "edit-preview-panel".to_string())
                                    .w(px(560.))
                                    .h(px(height.clamp(180., 420.)))
                                    .p_3()
                                    .gap_2()
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(hex(t.border_subtle))
                                    .bg(hex(t.bg_elevated))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(hex(t.fg_primary))
                                            .child("待提交 SQL 预览"),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h_0()
                                            .overflow_y_scrollbar()
                                            .scroll_bounce(false)
                                            .whitespace_normal()
                                            .child(crate::ddl::render_sql(&sql, &t)),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .justify_end()
                                            .child(
                                                Button::new("edit-preview-copy")
                                                    .small()
                                                    .ghost()
                                                    .label("复制")
                                                    .on_click(cx.listener(
                                                        move |view, _, _, cx| {
                                                            let text = view
                                                                .edit_preview
                                                                .clone()
                                                                .unwrap_or_default();
                                                            cx.write_to_clipboard(
                                                                ClipboardItem::new_string(text),
                                                            );
                                                            view.status = "已复制".into();
                                                            view.status_kind = StatusKind::Ok;
                                                            cx.notify();
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("edit-preview-close")
                                                    .small()
                                                    .label("关闭")
                                                    .on_click(cx.listener(|view, _, _, cx| {
                                                        view.edit_preview = None;
                                                        cx.notify();
                                                    })),
                                            ),
                                    ),
                            ),
                    )
            })
            .when_some(self.danger.clone(), |layout, _| {
                let confirm = self.render_danger_confirm(window, cx);
                layout
                    .child(
                        div()
                            .id("danger-scrim")
                            .debug_selector(|| "danger-scrim".to_string())
                            .absolute()
                            .inset_0()
                            .bg(gpui::hsla(0., 0., 0., 0.35))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.danger = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(confirm),
                    )
            })
            .child(self.render_status_bar(cx))
    }
}
