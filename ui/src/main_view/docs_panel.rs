//! 数据库文档面板（F-87）：结构汇总、本地笔记与 Markdown / DBML 预览。

use super::*;

impl MainView {
    /// 截图钩子用：当前已加载对象里的第一张表。
    pub(crate) fn first_table_path(&self) -> Option<ObjectPath> {
        self.objects
            .values()
            .flat_map(|objects| objects.iter())
            .find(|object| object.path.kind == ObjectKind::Table)
            .map(|object| object.path.clone())
    }

    /// F-87：打开数据库文档面板（只读汇总当前库的表结构）。
    pub(crate) fn open_docs_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let notes = self.doc_notes.notes(&profile);
        let project_note =
            designer_input(window, cx, "项目说明（优先于库注释）", Some(&notes.project));
        let table_note = designer_input(window, cx, "选中表的本地笔记", None);
        let column_note = designer_input(window, cx, "选中列的本地笔记", None);
        self.docs = Some(DocsPanel {
            namespace: ns.clone(),
            loading: true,
            input: None,
            notes,
            project_note,
            table_note,
            column_note,
            selected_table: None,
            selected_column: None,
            preview: None,
            error: None,
        });
        self.core.request_document(&session, ns);
        self.set_status("正在汇总表结构…", StatusKind::Busy, cx);
    }

    /// 把面板输入写回 `DocNotes`（不落盘）。
    pub(crate) fn flush_doc_notes(&mut self, cx: &App) {
        let Some(panel) = self.docs.as_mut() else {
            return;
        };
        let project = panel.project_note.read(cx).value().to_string();
        panel.notes.set_project(&project);
        let Some(index) = panel.selected_table else {
            return;
        };
        let Some(table) = panel
            .input
            .as_ref()
            .and_then(|input| input.tables.get(index))
        else {
            return;
        };
        let path = table.summary.path.clone();
        let has_column = panel.selected_column.as_ref().is_some_and(|column| {
            table.detail.as_ref().is_some_and(|detail| {
                detail
                    .columns
                    .iter()
                    .any(|item| item.meta.name.as_ref() == column.as_str())
            })
        });
        let column = panel.selected_column.clone();
        let table_note = panel.table_note.read(cx).value().to_string();
        let column_note = panel.column_note.read(cx).value().to_string();
        panel.notes.set_table_note(&path, &table_note);
        if let (Some(column), true) = (column, has_column) {
            panel.notes.set_column_note(&path, &column, &column_note);
        }
    }

    pub(crate) fn regenerate_doc_preview(&mut self) {
        if let Some(panel) = self.docs.as_mut() {
            panel.preview = panel
                .input
                .as_ref()
                .map(|input| rustdb_core::generate_document(input, &panel.notes));
        }
    }

    pub(crate) fn persist_doc_notes(&mut self, cx: &mut Context<Self>) -> bool {
        let profile = self
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let Some(panel) = self.docs.as_ref() else {
            return false;
        };
        let notes = panel.notes.clone();
        self.doc_notes.set(&profile, notes);
        match rustdb_core::save_doc_notes(&self.doc_notes_path, &self.doc_notes) {
            Ok(()) => true,
            Err(error) => {
                self.set_status(format!("文档笔记保存失败：{error}"), StatusKind::Error, cx);
                false
            }
        }
    }

    /// 保存笔记并刷新预览。
    pub(crate) fn save_doc_notes(&mut self, cx: &mut Context<Self>) {
        self.flush_doc_notes(cx);
        self.regenerate_doc_preview();
        if self.persist_doc_notes(cx) {
            self.set_status("文档笔记已保存", StatusKind::Ok, cx);
        }
        cx.notify();
    }

    pub(crate) fn select_doc_table(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_doc_notes(cx);
        let note = {
            let Some(panel) = self.docs.as_ref() else {
                return;
            };
            let Some(table) = panel
                .input
                .as_ref()
                .and_then(|input| input.tables.get(index))
            else {
                return;
            };
            panel
                .notes
                .table_note(&table.summary.path)
                .unwrap_or("")
                .to_string()
        };
        if let Some(panel) = self.docs.as_mut() {
            panel.selected_table = Some(index);
            panel.selected_column = None;
            let table_note = panel.table_note.clone();
            table_note.update(cx, |state, cx| state.set_value(note, window, cx));
            let column_note = panel.column_note.clone();
            column_note.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        }
        self.regenerate_doc_preview();
        cx.notify();
    }

    pub(crate) fn select_doc_column(
        &mut self,
        column: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_doc_notes(cx);
        let note = {
            let Some(panel) = self.docs.as_ref() else {
                return;
            };
            let Some(table) = panel
                .selected_table
                .and_then(|index| panel.input.as_ref()?.tables.get(index))
            else {
                return;
            };
            panel
                .notes
                .column_note(&table.summary.path, &column)
                .unwrap_or("")
                .to_string()
        };
        if let Some(panel) = self.docs.as_mut() {
            panel.selected_column = Some(column);
            let column_note = panel.column_note.clone();
            column_note.update(cx, |state, cx| state.set_value(note, window, cx));
        }
        self.regenerate_doc_preview();
        cx.notify();
    }

    /// 重新向驱动取一次元数据（笔记保留）。
    pub(crate) fn regenerate_doc(&mut self, cx: &mut Context<Self>) {
        self.flush_doc_notes(cx);
        self.persist_doc_notes(cx);
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let Some(ns) = self.docs.as_mut().map(|panel| {
            panel.loading = true;
            panel.error = None;
            panel.namespace.clone()
        }) else {
            return;
        };
        self.core.request_document(&session, ns);
        self.set_status("正在汇总表结构…", StatusKind::Busy, cx);
    }

    /// 导出 Markdown / DBML：先存笔记与预览，再原子写盘。
    pub(crate) fn export_doc(
        &mut self,
        format: DocFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.flush_doc_notes(cx);
        self.regenerate_doc_preview();
        self.persist_doc_notes(cx);
        let Some(panel) = self.docs.as_ref() else {
            return;
        };
        let Some(docs) = panel.preview.clone() else {
            self.set_status("文档还没生成，请先重新生成", StatusKind::Error, cx);
            return;
        };
        let text = format.text(&docs).to_string();
        let stem = doc_file_stem(&panel.namespace.display_label());
        let suggested = format!("{stem}-doc.{}", format.extension());
        let dir = std::env::current_dir().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some(&suggested));
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
                    format!("已导出 {}（{}）", path.display(), format.label()),
                    StatusKind::Ok,
                    cx,
                ),
                Err(error) => view.set_status(format!("导出失败：{error}"), StatusKind::Error, cx),
            });
        })
        .detach();
        cx.notify();
    }

    /// F-87：文档面板（左笔记、右预览）。
    pub(crate) fn render_docs_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.docs.clone() else {
            return div().into_any_element();
        };
        let header = h_flex()
            .h(px(30.))
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::Braces)
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
                        "数据库文档 · {}",
                        panel.namespace.display_label()
                    ))),
            )
            .child(
                div()
                    .id("docs-close")
                    .debug_selector(|| "docs-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.docs = None;
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
        let mut left = v_flex().w(px(250.)).flex_none().gap_1();
        left = left
            .child(label("项目笔记"))
            .child(Input::new(&panel.project_note))
            .child(label("表"));
        let mut list = v_flex()
            .gap(px(1.))
            .max_h(px(140.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        match panel.input.as_ref() {
            Some(input) if !input.tables.is_empty() => {
                for (index, table) in input.tables.iter().enumerate() {
                    let active = panel.selected_table == Some(index);
                    let has_note = panel.notes.table_note(&table.summary.path).is_some();
                    let name = table.summary.path.name.to_string();
                    list = list.child(
                        div()
                            .id(("doc-table", index))
                            .debug_selector(move || format!("doc-table-{index}"))
                            .h(px(22.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .when(active, |row| row.bg(hex(t.bg_overlay)))
                            .hover(|style| style.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.select_doc_table(index, window, cx)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(10.5))
                                    .text_ellipsis()
                                    .child(SharedString::from(name)),
                            )
                            .when(has_note, |row| {
                                row.child(
                                    div()
                                        .text_size(px(9.))
                                        .text_color(hex(t.accent))
                                        .child("笔记"),
                                )
                            }),
                    );
                }
            }
            _ => {
                list = list.child(
                    div()
                        .text_size(px(10.))
                        .text_color(hex(t.fg_muted))
                        .child("（暂无表）"),
                );
            }
        }
        left = left.child(list);
        if let Some(table) = panel
            .selected_table
            .and_then(|index| panel.input.as_ref()?.tables.get(index))
        {
            left = left
                .child(label("表笔记"))
                .child(Input::new(&panel.table_note))
                .child(label("列（点击编辑列笔记）"));
            let mut columns = v_flex()
                .gap(px(1.))
                .max_h(px(110.))
                .overflow_y_scrollbar()
                .scroll_bounce(false);
            let items: Vec<String> = table
                .detail
                .as_ref()
                .map(|detail| {
                    detail
                        .columns
                        .iter()
                        .map(|column| column.meta.name.to_string())
                        .collect()
                })
                .unwrap_or_default();
            if items.is_empty() {
                columns = columns.child(
                    div()
                        .text_size(px(9.5))
                        .text_color(hex(t.fg_muted))
                        .child("（没有列信息）"),
                );
            }
            for (index, name) in items.iter().enumerate() {
                let active = panel.selected_column.as_deref() == Some(name.as_str());
                let has_note = panel.notes.column_note(&table.summary.path, name).is_some();
                let click = name.clone();
                columns = columns.child(
                    div()
                        .id(("doc-col", index))
                        .debug_selector(move || format!("doc-col-{index}"))
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .when(active, |row| row.bg(hex(t.bg_overlay)))
                        .hover(|style| style.bg(hex(t.bg_overlay)))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.select_doc_column(click.clone(), window, cx)
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(10.))
                                .text_ellipsis()
                                .child(SharedString::from(name.clone())),
                        )
                        .when(has_note, |row| {
                            row.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(hex(t.accent))
                                    .child("笔记"),
                            )
                        }),
                );
            }
            left = left.child(columns);
            if panel.selected_column.is_some() {
                left = left
                    .child(label("列笔记"))
                    .child(Input::new(&panel.column_note));
            }
        }

        let mut right = v_flex().flex_1().min_w_0().min_h_0().gap_1();
        if panel.loading {
            right = right.child(
                div()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_muted))
                    .child("正在汇总表结构…"),
            );
        }
        if let Some(error) = &panel.error {
            right = right.child(
                div()
                    .text_size(px(10.5))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error.clone())),
            );
        }
        if let Some(docs) = &panel.preview {
            if !docs.warnings.is_empty() {
                let mut warnings = v_flex()
                    .gap(px(1.))
                    .max_h(px(84.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false);
                for warning in &docs.warnings {
                    warnings = warnings.child(
                        div()
                            .text_size(px(9.5))
                            .text_color(hex(t.warning))
                            .child(SharedString::from(format!("警告：{warning}"))),
                    );
                }
                right = right.child(warnings);
            }
            right = right.child(
                div()
                    .id("doc-preview")
                    .debug_selector(|| "doc-preview".to_string())
                    .flex_1()
                    .min_h_0()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(hex(t.border_subtle))
                    .p_2()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .text_size(px(10.))
                    .text_color(hex(t.fg_secondary))
                    .whitespace_normal()
                    .child(render_doc_markdown(&docs.markdown, &t)),
            );
        }

        let footer = h_flex()
            .gap_2()
            .justify_end()
            .child(
                div().debug_selector(|| "doc-save".to_string()).child(
                    Button::new("doc-save-btn")
                        .small()
                        .ghost()
                        .label("保存笔记")
                        .on_click(cx.listener(|view, _, _, cx| view.save_doc_notes(cx))),
                ),
            )
            .child(
                div().debug_selector(|| "doc-regenerate".to_string()).child(
                    Button::new("doc-regenerate-btn")
                        .small()
                        .ghost()
                        .label("重新生成")
                        .disabled(panel.loading)
                        .on_click(cx.listener(|view, _, _, cx| view.regenerate_doc(cx))),
                ),
            )
            .child(
                div().debug_selector(|| "doc-export-md".to_string()).child(
                    Button::new("doc-export-md-btn")
                        .small()
                        .label("导出 Markdown")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.export_doc(DocFormat::Markdown, window, cx)
                        })),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "doc-export-dbml".to_string())
                    .child(
                        Button::new("doc-export-dbml-btn")
                            .small()
                            .ghost()
                            .label("导出 DBML")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.export_doc(DocFormat::Dbml, window, cx)
                            })),
                    ),
            );

        v_flex()
            .id("docs-panel")
            .debug_selector(|| "docs-panel".to_string())
            .occlude()
            .w(px(880.))
            .h(px(520.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .gap_3()
                    .child(left)
                    .child(right),
            )
            .child(footer)
            .into_any_element()
    }
}
