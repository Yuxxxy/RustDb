//! 例程面板（F-82）：查看、搜索、编辑、保存与依赖。

use super::*;

impl MainView {
    /// F-82：打开例程源码面板（视图 / 过程 / 函数 / 触发器 / 程序包）。
    pub(crate) fn open_routine(
        &mut self,
        path: rustdb_db_api::ObjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.obj_menu = None;
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let Some(kind) = routine_kind_of(path.kind) else {
            self.status = "该对象不是例程，没有源码可看".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let Some(db_kind) = self.core.session_kind(&session) else {
            self.status = "无法识别连接类型，请重新连接".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let support = rustdb_core::routine_support(db_kind, kind);
        let editor = cx.new(|cx| EditorState::new(window, cx).language("sql"));
        let search =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("搜索源码"));
        // 搜索框变化 → 同步到编辑器的查找会话。
        cx.subscribe(&search, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.routine_apply_search(cx);
            }
        })
        .detach();
        // 源码变化 → 同步草稿、清掉上一次的失败提示，让「保存」重新可用。
        cx.subscribe(&editor, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if let Some(panel) = view.routine.as_mut() {
                    panel.failure = None;
                    panel.draft = panel.editor.read(cx).value().to_string();
                }
                cx.notify();
            }
        })
        .detach();
        // F-83：驱动支持依赖查询时才发请求（R5：不假装支持）。
        let deps_supported = self
            .capabilities
            .as_ref()
            .is_some_and(|caps| caps.routine_dependencies);
        self.routine = Some(RoutinePanel {
            label: path.display_label(),
            path: path.clone(),
            kind,
            original: String::new(),
            draft: String::new(),
            editor: editor.clone(),
            search,
            failure: None,
            saving: None,
            readonly_reason: support.reason,
            loading: true,
            deps: RoutineDeps {
                loading: deps_supported,
                ..RoutineDeps::default()
            },
        });
        self.routine_discard = false;
        self.routine_save_preview = None;
        self.status = format!("正在读取 {} 的源码…", path.name);
        self.status_kind = StatusKind::Busy;
        self.core.request_routine_source(&session, path.clone());
        if deps_supported {
            self.core.request_dependencies(&session, path);
        }
        cx.notify();
    }

    /// F-82：把例程草稿包成保存计划（失败时给出原因，不静默）。
    pub(crate) fn routine_plan(&self, cx: &App) -> Result<rustdb_core::RoutineSavePlan, String> {
        let panel = self.routine.as_ref().ok_or("例程面板未打开")?;
        let Some(session) = self.session.as_ref() else {
            return Err("请先连接数据源".into());
        };
        let db_kind = self
            .core
            .session_kind(session)
            .ok_or("无法识别连接类型，请重新连接")?;
        let qualified = rustdb_core::core::object_from_clause(db_kind, &panel.path);
        let draft = panel.editor.read(cx).value().to_string();
        Ok(rustdb_core::plan_save(
            db_kind, panel.kind, &qualified, &draft,
        ))
    }

    /// F-82：保存前先给用户看要执行的语句（DROP+CREATE 必须显式确认）。
    pub(crate) fn routine_save(&mut self, cx: &mut Context<Self>) {
        let plan = match self.routine_plan(cx) {
            Ok(plan) => plan,
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
                cx.notify();
                return;
            }
        };
        if let Some(error) = &plan.error {
            self.status = error.clone();
            self.status_kind = StatusKind::Error;
            if let Some(panel) = self.routine.as_mut() {
                panel.failure = Some(DesignerFailure {
                    hint: rustdb_core::recovery_hint(error),
                    message: error.clone(),
                });
            }
            cx.notify();
            return;
        }
        self.routine_save_preview = Some(RoutineSavePreview { plan });
        cx.notify();
    }

    /// F-82：真正下发保存语句。
    pub(crate) fn routine_save_execute(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.routine_save_preview.take() else {
            return;
        };
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        match self.core.apply_routine_save(&session, &preview.plan) {
            Ok(query_id) => {
                if let Some(panel) = self.routine.as_mut() {
                    panel.saving = Some(query_id);
                    panel.failure = None;
                }
                self.status = "正在保存例程源码…".into();
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                if let Some(panel) = self.routine.as_mut() {
                    panel.failure = Some(DesignerFailure {
                        hint: rustdb_core::recovery_hint(&message),
                        message: message.clone(),
                    });
                }
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// F-82：关闭源码面板；草稿没保存时要先确认。
    pub(crate) fn routine_close(&mut self, cx: &mut Context<Self>) {
        let dirty = self
            .routine
            .as_ref()
            .is_some_and(|panel| self.routine_dirty(panel, cx));
        if dirty {
            self.routine_discard = true;
            cx.notify();
            return;
        }
        self.routine = None;
        self.routine_discard = false;
        cx.notify();
    }

    /// 草稿是否与库里的原文不同。
    pub(crate) fn routine_dirty(&self, panel: &RoutinePanel, cx: &App) -> bool {
        let draft = panel.editor.read(cx).value().to_string();
        !panel.original.is_empty() && draft != panel.original
    }

    /// F-83：切换「依赖 / 被依赖」。
    pub(crate) fn routine_deps_tab(&mut self, tab: RoutineDepsTab, cx: &mut Context<Self>) {
        if let Some(panel) = self.routine.as_mut() {
            panel.deps.tab = tab;
            cx.notify();
        }
    }

    /// F-83：点依赖条目 → 表看数据、例程看源码；跳转前先守住未保存草稿。
    pub(crate) fn routine_open_dependency(
        &mut self,
        dep: rustdb_db_api::DependencyInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if routine_kind_of(dep.path.kind).is_some() {
            let dirty = self
                .routine
                .as_ref()
                .is_some_and(|panel| self.routine_dirty(panel, cx));
            if dirty {
                self.status = "当前源码有未保存修改，先保存或还原再打开其他例程".into();
                self.status_kind = StatusKind::Warn;
                cx.notify();
                return;
            }
            self.open_routine(dep.path, window, cx);
            return;
        }
        match dep.path.kind {
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                self.preview_object(dep.path, 100, window, cx);
            }
            other => {
                self.status = format!("暂不支持打开 {} 对象", other.label());
                self.status_kind = StatusKind::Warn;
                cx.notify();
            }
        }
    }
    /// F-82：把搜索框内容同步到编辑器的查找会话（空查询时清掉高亮）。
    pub(crate) fn routine_apply_search(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.routine.as_ref() else {
            return;
        };
        let query = panel.search.read(cx).value().to_string();
        let editor = panel.editor.clone();
        editor.update(cx, |state, cx| {
            if query.is_empty() {
                state.close_search(cx);
            } else {
                state.set_search_query(query.clone(), true, cx);
            }
        });
    }

    /// F-82：跳到上一处 / 下一处匹配。
    pub(crate) fn routine_search_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(panel) = self.routine.as_ref() else {
            return;
        };
        let editor = panel.editor.clone();
        editor.update(cx, |state, cx| {
            if forward {
                state.next_search_match(cx);
            } else {
                state.previous_search_match(cx);
            }
        });
    }

    /// F-82：把源码还原成库里的原文。
    pub(crate) fn routine_revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.routine.as_ref() else {
            return;
        };
        let original = panel.original.clone();
        let editor = panel.editor.clone();
        editor.update(cx, |state, cx| {
            state.replace_all(original.clone(), window, cx);
        });
        if let Some(panel) = self.routine.as_mut() {
            panel.failure = None;
            panel.draft = original;
        }
        cx.notify();
    }
    /// F-82：例程源码面板（查看 / 搜索 / 编辑 / 保存）。
    pub(crate) fn render_routine_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.routine.clone() else {
            return div().into_any_element();
        };
        // 事件里回来的源码在这里写进编辑器（事件处理拿不到 window）。
        if let Some(source) = self.pending_routine_source.take() {
            panel.editor.update(cx, |state, cx| {
                state.set_value(source.clone(), window, cx);
            });
            if let Some(panel) = self.routine.as_mut() {
                panel.draft = source;
            }
        }
        let draft = panel.editor.read(cx).value().to_string();
        let dirty = !panel.original.is_empty() && draft != panel.original;
        let query = panel.search.read(cx).value().to_string();
        let matches = if query.is_empty() {
            String::new()
        } else {
            panel.editor.read(cx).search_session().matcher.label()
        };
        let read_only = panel.readonly_reason.is_some()
            || self
                .active_profile
                .as_deref()
                .and_then(|id| self.profiles.iter().find(|p| p.id == id))
                .is_some_and(|profile| profile.read_only);
        let can_save =
            panel.readonly_reason.is_none() && !read_only && dirty && panel.saving.is_none();

        let header = h_flex()
            .h(px(34.))
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .child(
                Icon::new(IconName::Braces)
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
                        "{} · {}",
                        panel.label,
                        panel.kind.label()
                    ))),
            )
            .child(
                div()
                    .id("routine-panel-close")
                    .debug_selector(|| "routine-panel-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| view.routine_close(cx)))
                    .child(
                        Icon::new(IconName::Close)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );

        let search_row = h_flex()
            .h(px(30.))
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .child(div().w(px(220.)).child(Input::new(&panel.search).small()))
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(if query.is_empty() {
                        "支持在源码内搜索".to_string()
                    } else {
                        format!("匹配 {matches}")
                    })),
            )
            .child(
                div()
                    .id("routine-search-prev")
                    .debug_selector(|| "routine-search-prev".to_string())
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.routine_search_step(false, cx);
                    }))
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .child("上一处"),
                    ),
            )
            .child(
                div()
                    .id("routine-search-next")
                    .debug_selector(|| "routine-search-next".to_string())
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.routine_search_step(true, cx);
                    }))
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .child("下一处"),
                    ),
            );

        // F-83：依赖区。驱动不支持时整块不出现（R5：不假装支持）。
        let deps_visible =
            panel.deps.loading || panel.deps.error.is_some() || panel.deps.list.is_some();
        let deps_strip = deps_visible.then(|| {
            let tab = panel.deps.tab;
            let list = panel.deps.list.as_ref();
            let dep_count = list.map_or(0, |list| list.depends_on.len());
            let rev_count = list.map_or(0, |list| list.depended_by.len());
            let (entries, truncated) = match (list, tab) {
                (Some(list), RoutineDepsTab::DependsOn) => {
                    (list.depends_on.as_slice(), list.truncated)
                }
                (Some(list), RoutineDepsTab::DependedBy) => {
                    (list.depended_by.as_slice(), list.truncated)
                }
                (None, _) => (&[][..], false),
            };
            let tab_chip = |id: &'static str,
                            label: String,
                            active: bool,
                            target: RoutineDepsTab| {
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || id.to_string())
                    .px_2()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .text_size(px(crate::design::TS_CAPTION))
                    .when(active, |this| {
                        this.bg(hex(t.bg_overlay)).text_color(hex(t.accent))
                    })
                    .when(!active, |this| {
                        this.text_color(hex(t.fg_muted))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| view.routine_deps_tab(target, cx)))
                    .child(SharedString::from(label))
            };
            let mut row = h_flex()
                .h(px(28.))
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(hex(t.border_subtle))
                .child(tab_chip(
                    "routine-deps-tab-dep",
                    format!("依赖 {dep_count}"),
                    tab == RoutineDepsTab::DependsOn,
                    RoutineDepsTab::DependsOn,
                ))
                .child(tab_chip(
                    "routine-deps-tab-rev",
                    format!("被依赖 {rev_count}"),
                    tab == RoutineDepsTab::DependedBy,
                    RoutineDepsTab::DependedBy,
                ));
            if panel.deps.loading {
                row = row.child(
                    div()
                        .w(px(96.))
                        .h(px(12.))
                        .rounded(px(6.))
                        .bg(hex(t.fg_muted).opacity(0.18)),
                );
            } else if let Some(error) = &panel.deps.error {
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.danger))
                        .text_ellipsis()
                        .child(SharedString::from(error.clone())),
                );
            } else if entries.is_empty() {
                row = row.child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_muted))
                        .child(if tab == RoutineDepsTab::DependsOn {
                            "没有记录到它依赖的对象"
                        } else {
                            "没有记录到依赖它的对象"
                        }),
                );
            } else {
                for (index, dep) in entries.iter().take(4).enumerate() {
                    let label = format!("{} · {}", dep.path.kind.label(), dep.path.name);
                    let tip = match &dep.remote {
                        Some(ns) => format!(
                            "{}（跨库 {ns}）· {}",
                            dep.path.display_label(),
                            dep.dependency_type
                        ),
                        None => format!("{} · {}", dep.path.display_label(), dep.dependency_type),
                    };
                    let dep = dep.clone();
                    row = row.child(
                        div()
                            .debug_selector(move || format!("routine-dep-{index}"))
                            .child(
                                Button::new(SharedString::from(format!("routine-dep-btn-{index}")))
                                    .small()
                                    .ghost()
                                    .label(SharedString::from(label))
                                    .tooltip(SharedString::from(tip))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.routine_open_dependency(dep.clone(), window, cx);
                                    })),
                            ),
                    );
                }
                let hidden = entries.len().saturating_sub(4);
                if hidden > 0 || truncated {
                    let text = if truncated {
                        format!("仅显示前 4 项（已截断，共 {}）", entries.len())
                    } else {
                        format!("还有 {hidden} 项")
                    };
                    row = row.child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(text)),
                    );
                }
            }
            row
        });

        let editor = div()
            .flex_1()
            .min_h_0()
            .p_2()
            .child(Editor::new(&panel.editor).h_full().readonly(read_only));

        let mut left = v_flex().flex_1().min_w_0().gap(px(1.));
        if let Some(failure) = &panel.failure {
            left = left
                .child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.danger))
                        .text_ellipsis()
                        .child(SharedString::from(failure.message.clone())),
                )
                .child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_muted))
                        .text_ellipsis()
                        .child(SharedString::from(failure.hint.clone())),
                );
        } else if let Some(reason) = &panel.readonly_reason {
            left = left.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.warning))
                    .text_ellipsis()
                    .child(SharedString::from(reason.clone())),
            );
        } else if read_only {
            left = left.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.warning))
                    .child("该连接为只读模式，不能保存源码"),
            );
        } else if panel.loading {
            left = left.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("正在读取源码…"),
            );
        } else if dirty {
            left = left.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.warning))
                    .child("源码已修改，保存后生效"),
            );
        }

        let footer = h_flex()
            .h(px(36.))
            .px_3()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(hex(t.border_subtle))
            .child(left)
            .child(
                div()
                    .id("routine-revert")
                    .debug_selector(|| "routine-revert".to_string())
                    .child(
                        Button::new("routine-revert-btn")
                            .small()
                            .ghost()
                            .label("还原")
                            .disabled(!dirty)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.routine_revert(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("routine-save")
                    .debug_selector(|| "routine-save".to_string())
                    .child(
                        Button::new("routine-save-btn")
                            .small()
                            .primary()
                            .label(if panel.saving.is_some() {
                                "保存中…"
                            } else {
                                "保存"
                            })
                            .disabled(!can_save)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.routine_save(cx);
                            })),
                    ),
            );

        v_flex()
            .id("routine-panel")
            .debug_selector(|| "routine-panel".to_string())
            .w(px(720.))
            .h(px(520.))
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .overflow_hidden()
            // 吃掉面板内的点击，避免穿透到遮罩把面板关掉。
            .on_click(cx.listener(|_, _, _, _| {}))
            .child(header)
            .child(search_row)
            .when_some(deps_strip, |this, strip| this.child(strip))
            .child(editor)
            .child(footer)
            .into_any_element()
    }

    /// F-82：保存前的确认浮层（预览 + 警告）。
    pub(crate) fn render_routine_save_preview(
        &mut self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(preview) = self.routine_save_preview.as_ref() else {
            return div().into_any_element();
        };
        let text = preview.plan.preview_text();
        let warning = preview.plan.warning.clone();
        v_flex()
            .id("routine-save-preview")
            .debug_selector(|| "routine-save-preview".to_string())
            .w(px(600.))
            .h(px(420.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child("保存例程源码"),
            )
            .when(warning.is_some(), |card| {
                card.child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(warning.unwrap_or_default())),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .whitespace_normal()
                    .child(crate::ddl::render_sql(&text, &t)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .debug_selector(|| "routine-save-cancel".to_string())
                            .child(
                                Button::new("routine-save-cancel-btn")
                                    .small()
                                    .ghost()
                                    .label("取消")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.routine_save_preview = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "routine-save-confirm".to_string())
                            .child(
                                Button::new("routine-save-confirm-btn")
                                    .small()
                                    .primary()
                                    .label("执行保存")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.routine_save_execute(cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// F-82：离开脏草稿时的确认框。
    pub(crate) fn render_routine_discard(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        if !self.routine_discard {
            return div().into_any_element();
        }
        v_flex()
            .id("routine-discard")
            .debug_selector(|| "routine-discard".to_string())
            .w(px(300.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(
                div()
                    .text_size(px(crate::design::TS_BODY))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child("放弃未保存的修改？"),
            )
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_secondary))
                    .child("源码已被修改，关闭面板会丢掉这些改动。"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .debug_selector(|| "routine-discard-cancel".to_string())
                            .child(
                                Button::new("routine-discard-cancel-btn")
                                    .small()
                                    .ghost()
                                    .label("继续编辑")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.routine_discard = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "routine-discard-confirm".to_string())
                            .child(
                                Button::new("routine-discard-confirm-btn")
                                    .small()
                                    .danger()
                                    .label("放弃修改")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.routine = None;
                                        view.routine_discard = false;
                                        view.routine_save_preview = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
