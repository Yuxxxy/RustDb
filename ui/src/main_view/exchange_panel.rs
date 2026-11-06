//! 连接配置加密交换面板（F-90）：导出、预览与导入。

use super::*;

impl MainView {
    /// F-90：打开连接配置交换面板（默认停在导出页）。
    pub(crate) fn open_exchange_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let passphrase = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder("口令（留空则明文导出，需二次确认）")
                .masked(true)
        });
        let export_rows = self
            .profiles
            .iter()
            .map(|profile| ExchangeExportRow {
                id: profile.id.clone(),
                profile: rustdb_core::ExchangeProfile::from_profile(profile, true),
                selected: true,
            })
            .collect();
        self.exchange = Some(ExchangePanel {
            tab: ExchangeTab::Export,
            export_rows,
            include_tunnels: true,
            passphrase,
            confirm_plain: false,
            preview: None,
            selected: Vec::new(),
            pending_text: None,
            needs_passphrase: false,
            drafts: false,
            message: None,
        });
        cx.notify();
    }

    pub(crate) fn exchange_set_tab(&mut self, tab: ExchangeTab, cx: &mut Context<Self>) {
        let profiles = self.profiles.clone();
        if let Some(panel) = self.exchange.as_mut() {
            panel.tab = tab;
            panel.confirm_plain = false;
            panel.message = None;
            if tab == ExchangeTab::Export {
                panel.export_rows = profiles
                    .iter()
                    .map(|profile| ExchangeExportRow {
                        id: profile.id.clone(),
                        profile: rustdb_core::ExchangeProfile::from_profile(
                            profile,
                            panel.include_tunnels,
                        ),
                        selected: true,
                    })
                    .collect();
            }
        }
        cx.notify();
    }

    pub(crate) fn exchange_toggle_export_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(panel) = self.exchange.as_mut() {
            if let Some(row) = panel.export_rows.get_mut(index) {
                row.selected = !row.selected;
                panel.confirm_plain = false;
            }
        }
        cx.notify();
    }

    pub(crate) fn exchange_select_all(&mut self, selected: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.exchange.as_mut() {
            for row in panel.export_rows.iter_mut() {
                row.selected = selected;
            }
            panel.confirm_plain = false;
        }
        cx.notify();
    }

    pub(crate) fn exchange_toggle_import_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(panel) = self.exchange.as_mut() {
            if let Some(slot) = panel.selected.get_mut(index) {
                if panel
                    .preview
                    .as_ref()
                    .and_then(|preview| preview.profiles.get(index))
                    .is_some_and(|row| !row.conflict)
                {
                    *slot = !*slot;
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn exchange_toggle_tunnels(&mut self, cx: &mut Context<Self>) {
        let profiles = self.profiles.clone();
        if let Some(panel) = self.exchange.as_mut() {
            panel.include_tunnels = !panel.include_tunnels;
            panel.confirm_plain = false;
            for row in panel.export_rows.iter_mut() {
                let Some(profile) = profiles.iter().find(|profile| profile.id == row.id) else {
                    continue;
                };
                row.profile =
                    rustdb_core::ExchangeProfile::from_profile(profile, panel.include_tunnels);
            }
        }
        cx.notify();
    }

    pub(crate) fn exchange_message(
        &mut self,
        message: impl Into<String>,
        error: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.exchange.as_mut() {
            panel.message = Some((message.into(), error));
        }
        cx.notify();
    }

    /// F-90：导出当前勾选的连接。留空口令时先要求二次确认（明文风险）。
    pub(crate) fn exchange_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.exchange.as_ref() else {
            return;
        };
        let selected: Vec<_> = panel
            .export_rows
            .iter()
            .filter(|row| row.selected)
            .map(|row| row.id.clone())
            .collect();
        if selected.is_empty() {
            self.exchange_message("至少选择一个连接", true, cx);
            return;
        }
        let passphrase = panel.passphrase.read(cx).value().trim().to_string();
        if passphrase.is_empty() && !panel.confirm_plain {
            if let Some(panel) = self.exchange.as_mut() {
                panel.confirm_plain = true;
                panel.message = Some((
                    "明文导出会暴露主机、用户名与分组；再点一次「导出」确认".into(),
                    false,
                ));
            }
            cx.notify();
            return;
        }
        let include_tunnels = panel.include_tunnels;
        let profiles: Vec<ConnectionProfile> = selected
            .iter()
            .filter_map(|id| {
                self.profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .cloned()
            })
            .collect();
        let text = match rustdb_core::export_exchange(
            &profiles,
            (!passphrase.is_empty()).then_some(passphrase.as_str()),
            include_tunnels,
        ) {
            Ok(text) => text,
            Err(error) => {
                self.exchange_message(error.to_string(), true, cx);
                return;
            }
        };
        let encrypted = !passphrase.is_empty();
        let dir = std::env::current_dir().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some("rustdb-connections.json"));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let writing = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&writing, text) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match result {
                Ok(()) => {
                    if let Some(panel) = view.exchange.as_mut() {
                        panel.confirm_plain = false;
                        panel.message = Some((
                            format!(
                                "已导出 {} 个连接（{}）",
                                profiles.len(),
                                if encrypted { "已加密" } else { "明文" }
                            ),
                            false,
                        ));
                    }
                    view.set_status(
                        format!("已导出连接配置：{}", file_label(&path)),
                        StatusKind::Ok,
                        cx,
                    );
                }
                Err(error) => view.set_status(format!("导出失败：{error}"), StatusKind::Error, cx),
            });
        })
        .detach();
    }

    /// F-90：选择要导入的文件（RustDB 交换文件或常见客户端配置）。
    pub(crate) fn exchange_pick_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(
                "选择连接配置（RustDB 导出文件 / DBeaver data-sources.json）",
            )),
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
            let _ = this.update_in(cx, |view, _, cx| match text {
                Ok(text) => view.exchange_file_loaded(path, text, cx),
                Err(error) => view.exchange_message(format!("读取文件失败：{error}"), true, cx),
            });
        })
        .detach();
    }

    /// 文件读入后先尝试解密预览；失败再按「其他客户端配置」生成草稿。
    pub(crate) fn exchange_file_loaded(
        &mut self,
        path: PathBuf,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let existing = self.profiles.clone();
        if let Some(panel) = self.exchange.as_mut() {
            panel.pending_text = Some(text.clone());
            panel.needs_passphrase = false;
            panel.message = None;
        }
        match rustdb_core::preview_bundle(&text, None, &existing) {
            Ok(preview) => self.exchange_apply_preview(preview, cx),
            Err(rustdb_core::ExchangeError::PassphraseRequired) => {
                if let Some(panel) = self.exchange.as_mut() {
                    panel.needs_passphrase = true;
                    panel.preview = None;
                    panel.selected.clear();
                    panel.message = Some((
                        format!("{} 已加密，请输入导出时的口令", file_label(&path)),
                        false,
                    ));
                }
                cx.notify();
            }
            Err(error) => match rustdb_core::drafts_from_client_config(&text) {
                Ok(preview) => self.exchange_apply_preview(preview, cx),
                Err(_) => {
                    self.exchange_message(format!("{}：{error}", file_label(&path)), true, cx)
                }
            },
        }
    }

    /// 把预览装进面板：冲突项默认不勾选。
    pub(crate) fn exchange_apply_preview(
        &mut self,
        preview: rustdb_core::ExchangePreview,
        cx: &mut Context<Self>,
    ) {
        let drafts = preview.drafts;
        let encrypted = preview.encrypted;
        let total = preview.profiles.len();
        let importable = preview.importable();
        let selected: Vec<bool> = preview.profiles.iter().map(|row| !row.conflict).collect();
        if let Some(panel) = self.exchange.as_mut() {
            panel.preview = Some(preview);
            panel.selected = selected;
            panel.needs_passphrase = false;
            panel.drafts = drafts;
            panel.message = Some((
                if drafts {
                    format!("识别到 {total} 个候选连接（草稿，不含口令）")
                } else if encrypted {
                    format!("已解密 {total} 个连接，其中 {importable} 个可导入")
                } else {
                    format!("明文配置 {total} 个连接，其中 {importable} 个可导入")
                },
                false,
            ));
        }
        cx.notify();
    }

    /// F-90：用口令解开加密文件并生成预览。
    pub(crate) fn exchange_unlock(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.exchange.as_ref() else {
            return;
        };
        let Some(text) = panel.pending_text.clone() else {
            return;
        };
        let passphrase = panel.passphrase.read(cx).value().trim().to_string();
        if passphrase.is_empty() {
            self.exchange_message("请输入口令", true, cx);
            return;
        }
        let existing = self.profiles.clone();
        match rustdb_core::preview_bundle(&text, Some(&passphrase), &existing) {
            Ok(preview) => self.exchange_apply_preview(preview, cx),
            Err(error) => self.exchange_message(error.to_string(), true, cx),
        }
    }

    /// F-90：把勾选的连接写入配置（跳过冲突；口令需重新输入）。
    pub(crate) fn exchange_import(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.exchange.as_ref() else {
            return;
        };
        let Some(preview) = panel.preview.clone() else {
            self.exchange_message("请先选择文件并生成预览", true, cx);
            return;
        };
        let selected = panel.selected.clone();
        let drafts = rustdb_core::profiles_from_preview(&preview, &selected, &self.profiles);
        if drafts.is_empty() {
            self.exchange_message("没有可导入的新连接（未勾选或已存在）", true, cx);
            return;
        }
        let total = drafts.len();
        let mut imported = 0;
        let mut failure = None;
        for profile in drafts {
            match self.core.add_profile(profile) {
                Ok(()) => imported += 1,
                Err(error) => failure = Some(error),
            }
        }
        self.profiles = self.core.profiles();
        if imported > 0 {
            self.exchange = None;
            self.set_status(
                format!("已导入 {imported} 个连接（口令需重新输入）"),
                StatusKind::Ok,
                cx,
            );
            return;
        }
        let message = failure.unwrap_or_else(|| format!("没有导入任何连接（共 {total} 项）"));
        self.exchange_message(message, true, cx);
    }

    /// F-85：分区管理面板（非模态，右上角 ✕ 关闭）。
    /// F-90：连接配置交换面板（导出 / 导入两个页签）。
    pub(crate) fn render_exchange_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.exchange.clone() else {
            return div().into_any_element();
        };
        let header = h_flex()
            .h(px(34.))
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .child(
                Icon::new(IconName::ArrowLeftRight)
                    .small()
                    .text_color(hex(t.accent)),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child("导入 / 导出连接"),
            )
            .child(
                div()
                    .id("exchange-panel-close")
                    .debug_selector(|| "exchange-panel-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.exchange = None;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconName::Close)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );

        let tabs = h_flex().px_3().pt_2().gap_1().items_center().children(
            [
                (ExchangeTab::Export, "导出", "exchange-tab-export"),
                (ExchangeTab::Import, "导入", "exchange-tab-import"),
            ]
            .map(|(kind, label, selector)| {
                let active = panel.tab == kind;
                div()
                    .id(SharedString::from(selector.to_string()))
                    .debug_selector(move || selector.to_string())
                    .h(px(20.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(active, |chip| {
                        chip.bg(hex(t.accent)).text_color(hex(t.on_accent))
                    })
                    .when(!active, |chip| {
                        chip.text_color(hex(t.fg_secondary))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.exchange_set_tab(kind, cx);
                    }))
                    .child(label)
            }),
        );

        let body: gpui::AnyElement = match panel.tab {
            ExchangeTab::Export => self.render_exchange_export(&panel, cx),
            ExchangeTab::Import => self.render_exchange_import(&panel, cx),
        };

        let message = panel.message.clone();
        let footer = h_flex()
            .p_3()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.))
                    .text_color(hex(if message.as_ref().is_some_and(|(_, err)| *err) {
                        t.danger
                    } else {
                        t.fg_muted
                    }))
                    .whitespace_normal()
                    .child(SharedString::from(
                        message
                            .map(|(text, _)| text)
                            .unwrap_or_else(|| match panel.tab {
                                ExchangeTab::Export => "导出不含口令、密钥库引用与隧道口令".into(),
                                ExchangeTab::Import => "导入的配置不含口令，需要重新输入".into(),
                            }),
                    )),
            )
            .when(panel.tab == ExchangeTab::Export, |footer| {
                footer.child(
                    div()
                        .debug_selector(|| "exchange-export".to_string())
                        .child(
                            Button::new("exchange-export-btn")
                                .small()
                                .primary()
                                .label("导出…")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.exchange_export(window, cx);
                                })),
                        ),
                )
            })
            .when(panel.tab == ExchangeTab::Import, |footer| {
                let selected = panel.selected.iter().filter(|value| **value).count();
                let ready = panel.preview.is_some() && selected > 0;
                footer.child(
                    div()
                        .debug_selector(|| "exchange-import".to_string())
                        .child(
                            Button::new("exchange-import-btn")
                                .small()
                                .primary()
                                .label(if ready {
                                    SharedString::from(format!("导入 {selected} 项"))
                                } else {
                                    SharedString::from("导入")
                                })
                                .disabled(!ready)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.exchange_import(cx);
                                })),
                        ),
                )
            });

        v_flex()
            .id("exchange-panel")
            .debug_selector(|| "exchange-panel".to_string())
            .w(px(560.))
            .max_h(px(500.))
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .overflow_hidden()
            .on_click(cx.listener(|_, _, _, _| {}))
            .child(header)
            .child(tabs)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    pub(crate) fn render_exchange_export(
        &mut self,
        panel: &ExchangePanel,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let selected = panel.export_rows.iter().filter(|row| row.selected).count();
        let all = !panel.export_rows.is_empty() && selected == panel.export_rows.len();

        let mut list = v_flex()
            .id("exchange-export-list")
            .max_h(px(210.))
            .min_h(px(60.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        if panel.export_rows.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_3()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_muted))
                    .child("还没有连接可导出"),
            );
        }
        for (index, row) in panel.export_rows.iter().enumerate() {
            let checked = row.selected;
            let name = row.profile.name.clone();
            let endpoint = row.profile.endpoint_label();
            let folder = row
                .profile
                .folder
                .clone()
                .filter(|folder| !folder.trim().is_empty());
            let badge = match row.profile.driver {
                DbKind::Sqlite => "SQLite",
                DbKind::Postgres => "PG",
                DbKind::Mysql => "MySQL",
                DbKind::Mariadb => "MariaDB",
                DbKind::Mssql => "MSSQL",
                DbKind::Oracle => "Oracle",
            };
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("exchange-export-{index}")))
                    .debug_selector(move || format!("exchange-export-{index}"))
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.exchange_toggle_export_row(index, cx);
                    }))
                    .child(exchange_check(checked, &t))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(name)),
                    )
                    .when_some(folder, |row, folder| {
                        row.child(
                            div()
                                .text_size(px(9.))
                                .text_color(hex(t.fg_muted))
                                .child(SharedString::from(folder)),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_secondary))
                            .child(SharedString::from(endpoint)),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child(badge),
                    ),
            );
        }

        v_flex()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .id("exchange-select-all")
                            .debug_selector(|| "exchange-select-all".to_string())
                            .text_size(px(10.))
                            .text_color(hex(t.fg_muted))
                            .cursor_pointer()
                            .hover(|s| s.text_color(hex(t.accent)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.exchange_select_all(!all, cx);
                            }))
                            .child(if all { "全不选" } else { "全选" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(format!(
                                "已选 {selected} / {}",
                                panel.export_rows.len()
                            ))),
                    )
                    .child(
                        div()
                            .id("exchange-tunnels")
                            .debug_selector(|| "exchange-tunnels".to_string())
                            .h(px(18.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(hex(if panel.include_tunnels {
                                t.accent
                            } else {
                                t.border_subtle
                            }))
                            .text_size(px(9.5))
                            .text_color(hex(if panel.include_tunnels {
                                t.accent
                            } else {
                                t.fg_muted
                            }))
                            .cursor_pointer()
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.exchange_toggle_tunnels(cx);
                            }))
                            .child("含 SSH 隧道"),
                    ),
            )
            .child(list)
            .child(
                h_flex().px_3().py_2().gap_2().items_center().child(
                    div()
                        .debug_selector(|| "exchange-passphrase".to_string())
                        .flex_1()
                        .child(Input::new(&panel.passphrase).small()),
                ),
            )
            .when(panel.confirm_plain, |body| {
                body.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_size(px(10.))
                        .text_color(hex(t.warning))
                        .child("明文导出：文件可被他人读取，主机与用户名会暴露"),
                )
            })
            .into_any_element()
    }

    pub(crate) fn render_exchange_import(
        &mut self,
        panel: &ExchangePanel,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let mut actions = h_flex().px_3().py_2().gap_2().items_center().child(
            div().debug_selector(|| "exchange-pick".to_string()).child(
                Button::new("exchange-pick-btn")
                    .small()
                    .ghost()
                    .label(if panel.preview.is_some() {
                        "换一个文件…"
                    } else {
                        "选择文件…"
                    })
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.exchange_pick_import(window, cx);
                    })),
            ),
        );
        if panel.needs_passphrase {
            actions = actions
                .child(
                    div()
                        .debug_selector(|| "exchange-unlock-passphrase".to_string())
                        .flex_1()
                        .child(Input::new(&panel.passphrase).small()),
                )
                .child(
                    div()
                        .debug_selector(|| "exchange-unlock".to_string())
                        .child(
                            Button::new("exchange-unlock-btn")
                                .small()
                                .primary()
                                .label("解锁")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.exchange_unlock(cx);
                                })),
                        ),
                );
        } else {
            actions = actions.child(
                div()
                    .flex_1()
                    .text_size(px(9.5))
                    .text_color(hex(t.fg_muted))
                    .whitespace_normal()
                    .child("支持 RustDb 导出文件、DBeaver data-sources.json、JDBC / 连接 URI"),
            );
        }

        let mut list = v_flex()
            .id("exchange-import-list")
            .max_h(px(220.))
            .min_h(px(60.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        let preview_rows = panel
            .preview
            .as_ref()
            .map(|preview| preview.profiles.clone())
            .unwrap_or_default();
        if preview_rows.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_3()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_muted))
                    .child(if panel.needs_passphrase {
                        "输入口令后显示可导入的连接"
                    } else {
                        "还没有选择文件"
                    }),
            );
        }
        for (index, row) in preview_rows.iter().enumerate() {
            let checked = panel.selected.get(index).copied().unwrap_or(false);
            let conflict = row.conflict;
            let name = row.profile.name.clone();
            let endpoint = row.profile.endpoint_label();
            let badge = match row.profile.driver {
                DbKind::Sqlite => "SQLite",
                DbKind::Postgres => "PG",
                DbKind::Mysql => "MySQL",
                DbKind::Mariadb => "MariaDB",
                DbKind::Mssql => "MSSQL",
                DbKind::Oracle => "Oracle",
            };
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("exchange-import-{index}")))
                    .debug_selector(move || format!("exchange-import-{index}"))
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .when(!conflict, |row| {
                        row.cursor_pointer().hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.exchange_toggle_import_row(index, cx);
                    }))
                    .child(exchange_check(checked, &t))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(hex(if conflict { t.fg_muted } else { t.fg_primary }))
                            .text_ellipsis()
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_secondary))
                            .child(SharedString::from(endpoint)),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(hex(t.fg_muted))
                            .child(badge),
                    )
                    .when(conflict, |row| {
                        row.child(
                            div()
                                .px_1()
                                .rounded(px(3.))
                                .bg(hex(t.bg_overlay))
                                .text_size(px(9.))
                                .text_color(hex(t.fg_muted))
                                .child("已存在"),
                        )
                    }),
            );
        }

        v_flex().child(actions).child(list).into_any_element()
    }
}
