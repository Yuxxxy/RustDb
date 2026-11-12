//! 侧栏与导航：连接树（含对象搜索 / 置顶 / 最近）、历史、文件、帮助与参数面板。

use super::*;

impl MainView {
    pub(crate) fn render_rail(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let dark = self.dark;
        v_flex()
            .w(px(RAIL_W))
            .h_full()
            .flex_shrink_0()
            .items_center()
            .py(px(6.))
            .gap(px(2.))
            // 窗口外框（标题栏 + 竖栏）比内容卡片亮一档，层次与 RustShell 对齐。
            .bg(hex(t.bg_elevated))
            .children(PanelKind::ALL.map(|panel| {
                let active = self.panel == panel;
                let collapsed = self.layout.sidebar_collapsed;
                let id = SharedString::from(format!("rail-{}", panel.key()));
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .absolute()
                            .left(px(2.))
                            .top(px(8.))
                            .bottom(px(8.))
                            .w(px(2.))
                            .rounded_full()
                            .when(active, |d| d.bg(hex(t.accent))),
                    )
                    .child(
                        div()
                            .debug_selector({
                                let selector = format!("rail-{}", panel.key());
                                move || selector.clone()
                            })
                            .child(
                                Button::new(id)
                                    .ghost()
                                    .with_size(Size::Medium)
                                    .icon(panel.icon())
                                    .tooltip(SharedString::from(if active && !collapsed {
                                        format!("收起{}", panel.label())
                                    } else {
                                        panel.label().to_string()
                                    }))
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        // 再点当前面板 = 收起侧栏；点别的面板同时展开。
                                        if view.panel == panel && !view.layout.sidebar_collapsed {
                                            view.toggle_sidebar(cx);
                                        } else {
                                            view.panel = panel;
                                            view.layout.sidebar_collapsed = false;
                                            view.persist_layout();
                                        }
                                        view.history_menu = None;
                                        cx.notify();
                                    })),
                            ),
                    )
            }))
            .child(div().flex_1())
            .child(
                div().debug_selector(|| "rail-ai".to_string()).child(
                    Button::new("rail-ai-btn")
                        .ghost()
                        .with_size(Size::Medium)
                        .icon(IconName::Bot)
                        .tooltip(if self.layout.ai_open {
                            "收起 AI 助手"
                        } else {
                            "打开 AI 助手"
                        })
                        .on_click(cx.listener(|view, _, _, cx| view.toggle_ai(cx))),
                ),
            )
            .child(
                Button::new("rail-theme")
                    .ghost()
                    .with_size(Size::Medium)
                    .icon(if dark { IconName::Moon } else { IconName::Sun })
                    .tooltip("切换主题")
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_theme(cx))),
            )
            .child(
                Button::new("rail-settings")
                    .ghost()
                    .with_size(Size::Medium)
                    .icon(IconName::Settings)
                    .tooltip("设置")
                    .on_click(cx.listener(|view, _, _, cx| view.open_settings(cx))),
            )
    }

    pub(crate) fn render_sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = current(cx);
        let is_connections = self.panel == PanelKind::Connections;
        let header = h_flex()
            .h(px(34.))
            .px_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .child(
                div()
                    .text_size(px(crate::design::TS_BODY))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child(self.panel.label()),
            )
            // F-02：连接面板可以直接新建连接。
            .when(is_connections, |head| {
                head.child(
                    h_flex().gap_2().items_center().child(
                        div()
                            .id("conn-new")
                            .debug_selector(|| "conn-new".to_string())
                            .text_size(px(crate::design::TS_BODY))
                            .text_color(hex(t.fg_muted))
                            .cursor_pointer()
                            .hover(|s| s.text_color(hex(t.accent)))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_conn_editor(None, window, cx);
                            }))
                            .child("+"),
                    ),
                )
            })
            .child(
                div()
                    .debug_selector(|| "sidebar-collapse".to_string())
                    .child(
                        Button::new("sidebar-collapse-btn")
                            .small()
                            .ghost()
                            .icon(IconName::PanelLeftClose)
                            .tooltip("收起侧栏")
                            .on_click(cx.listener(|view, _, _, cx| view.toggle_sidebar(cx))),
                    ),
            );

        let body: gpui::AnyElement = match self.panel {
            PanelKind::Dashboard => self.render_dashboard(cx).into_any_element(),
            PanelKind::Connections => self.render_connections(window, cx).into_any_element(),
            PanelKind::Files => self.render_files(window, cx).into_any_element(),
            PanelKind::History => self.render_history(window, cx).into_any_element(),
        };

        v_flex().size_full().child(header).child(
            div()
                .id("sidebar-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                // gpui-kit 0.8.0 起 macOS 默认带橡皮筋回弹；桌面树不需要，显式关掉。
                .scroll_bounce(false)
                .child(body),
        )
    }

    pub(crate) fn render_dashboard(&self, cx: &App) -> impl IntoElement {
        let t = current(cx);
        let connected = self.session.is_some();
        v_flex()
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(6.))
                            .h(px(6.))
                            .rounded_full()
                            .bg(hex(if connected { t.success } else { t.fg_muted })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_BODY))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(
                                self.server_label.clone().unwrap_or_else(|| "未连接".into()),
                            )),
                    ),
            )
            .child(div().h(px(1.)).w_full().bg(hex(t.border_subtle)))
            .child(metric_row("查询", self.query_count.to_string(), &t))
            .child(metric_row("已加载库", self.objects.len().to_string(), &t))
            .child(metric_row(
                "结果内存",
                format!("{} KB", self.core.results().total_bytes() / 1024),
                &t,
            ))
    }

    pub(crate) fn render_connections(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = current(cx);
        let connected = self.session.is_some();
        let active_profile = self.active_profile.clone();
        // 对象面板并入连接树后，搜索框随树一起出现。
        if self.object_input.is_none() {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("搜索对象")
            });
            cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    view.object_filter = view
                        .object_input
                        .as_ref()
                        .map(|input| input.read(cx).value().to_string())
                        .unwrap_or_default();
                    cx.notify();
                }
            })
            .detach();
            self.object_input = Some(input);
        }
        let filter = self.object_filter.trim().to_lowercase();
        let matches_filter = |object: &ObjectSummary| {
            filter.is_empty()
                || object.path.name.to_lowercase().contains(&filter)
                || object
                    .comment
                    .as_deref()
                    .is_some_and(|comment| comment.to_lowercase().contains(&filter))
        };
        let mut tree = v_flex().p_2().gap(px(1.));
        if connected {
            if let Some(input) = &self.object_input {
                tree = tree.child(
                    div()
                        .px_1()
                        .pb_1()
                        .child(gpui_component::input::Input::new(input).small()),
                );
            }
        }

        for folder in group_by_folder(&self.profiles) {
            let key = folder.key().to_string();
            let collapsed = self.collapsed_folders.contains(&key);
            let toggle_key = key.clone();
            tree = tree.child(
                h_flex()
                    .id(SharedString::from(format!("folder-{key}")))
                    .debug_selector({
                        let selector_key = key.clone();
                        move || format!("folder-{selector_key}")
                    })
                    .h(px(26.))
                    .px_1()
                    .gap_1()
                    .items_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if !view.collapsed_folders.remove(&toggle_key) {
                            view.collapsed_folders.insert(toggle_key.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .small()
                        .text_color(hex(t.fg_muted)),
                    )
                    .child(
                        Icon::new(if collapsed {
                            IconName::Folder
                        } else {
                            IconName::FolderOpen
                        })
                        .small()
                        .text_color(hex(t.fg_muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(folder.label().to_string())),
                    ),
            );

            if collapsed {
                continue;
            }

            for profile in folder.profiles {
                let is_active = connected && active_profile.as_deref() == Some(profile.id.as_str());
                let name = profile.name.clone();
                let badge = profile.driver_badge();
                let click_id = profile.id.clone();
                let menu_id = profile.id.clone();
                let menu_name = profile.name.clone();
                let production = profile.production;
                let unlock_secs = if production && is_active {
                    self.core
                        .write_unlock_remaining(&profile.id)
                        .map(|left| left.as_secs())
                } else {
                    None
                };
                let unlock_id = profile.id.clone();
                tree = tree.child(
                    h_flex()
                        .id(SharedString::from(format!("conn-{}", profile.id)))
                        .debug_selector({
                            let selector_id = profile.id.clone();
                            move || format!("conn-{selector_id}")
                        })
                        .h(px(28.))
                        .pl(px(20.))
                        .pr_1()
                        .gap_2()
                        .items_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .when(is_active, |row| row.bg(hex(t.bg_overlay)))
                        .hover(|s| s.bg(hex(t.bg_overlay)))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            // 已连接 / 正在连接的节点只做高亮，不再重复发起连接。
                            if view.connecting.as_deref() == Some(click_id.as_str())
                                || (view.session.is_some()
                                    && view.active_profile.as_deref() == Some(click_id.as_str()))
                            {
                                return;
                            }
                            view.connect(click_id.clone(), cx);
                        }))
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                                view.conn_menu =
                                    Some((menu_id.clone(), menu_name.clone(), event.position));
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .w(px(6.))
                                .h(px(6.))
                                .rounded_full()
                                .bg(hex(if is_active { t.success } else { t.fg_muted })),
                        )
                        .child(
                            Icon::new(IconName::Database)
                                .small()
                                .text_color(hex(if is_active { t.accent } else { t.fg_muted })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(if is_active {
                                    t.fg_primary
                                } else {
                                    t.fg_secondary
                                }))
                                .text_ellipsis()
                                .child(SharedString::from(name)),
                        )
                        .when(production, |row| {
                            let selector_id = unlock_id.clone();
                            row.child(
                                div()
                                    .debug_selector(move || format!("prod-badge-{selector_id}"))
                                    .px_1()
                                    .rounded(px(3.))
                                    .bg(hex(t.danger))
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(hex(t.on_danger_resolved))
                                    .child("生产"),
                            )
                        })
                        .when_some(unlock_secs, |row, left| {
                            row.child(
                                div()
                                    .debug_selector({
                                        let selector_id = unlock_id.clone();
                                        move || format!("unlock-badge-{selector_id}")
                                    })
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.success))
                                    .child(SharedString::from(format!(
                                        "解锁 {:02}:{:02}",
                                        left / 60,
                                        left % 60
                                    ))),
                            )
                        })
                        .child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .child(SharedString::from(badge)),
                        ),
                );

                // 连接建立后展开：库 → 对象分组（表 / 视图 / 存储过程 …）→ 对象
                if !is_active {
                    continue;
                }
                let namespaces = self.namespaces.clone();
                for ns in namespaces {
                    let key = ns_key(&ns);
                    // 搜索时所有库展开，命中结果直接可见。
                    let expanded = self.expanded_ns.contains(&key) || !filter.is_empty();
                    let is_current =
                        self.current_ns.as_ref().map(ns_key).as_deref() == Some(key.as_str());
                    let in_tab = self
                        .tabs
                        .iter()
                        .any(|tab| tab.ns_key().as_deref() == Some(key.as_str()));
                    let loading = self.loading.contains(&key);
                    // P1-②：400ms 内只留透明占位，超时后才真正显示骨架。
                    let skeleton_visible =
                        self.object_loading_phase.get(&key).is_some_and(|phase| {
                            phase.visual() == crate::design::LoadingVisual::Skeleton
                        });

                    let open_ns = ns.clone();
                    let row_key = key.clone();
                    let row_key_ns = ns.clone();
                    tree = tree.child(
                        h_flex()
                            .id(SharedString::from(format!("db-{key}")))
                            .debug_selector({
                                let selector = format!("db-{key}");
                                move || selector.clone()
                            })
                            .h(px(26.))
                            .pl(px(22.))
                            .pr_1()
                            .gap_1()
                            .items_center()
                            .rounded(px(5.))
                            .cursor_pointer()
                            .when(is_current || in_tab, |row| row.bg(hex(t.bg_overlay)))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            // 单击展开 / 收起；双击打开 SQL 标签页。
                            .on_click(cx.listener(
                                move |view, event: &gpui::ClickEvent, window, cx| {
                                    let double = matches!(
                                        event,
                                        gpui::ClickEvent::Mouse(mouse) if mouse.down.click_count >= 2
                                    );
                                    if double {
                                        view.open_tab(open_ns.clone(), window, cx);
                                    } else {
                                        view.toggle_namespace(open_ns.clone(), cx);
                                    }
                                },
                            ))
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(
                                    move |view, event: &gpui::MouseDownEvent, _, cx| {
                                        view.ns_menu = Some(NamespaceMenu {
                                            ns: row_key_ns.clone(),
                                            at: event.position,
                                        });
                                        cx.notify();
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("db-toggle-{row_key}")))
                                    .debug_selector({
                                        let selector = format!("db-toggle-{row_key}");
                                        move || selector.clone()
                                    })
                                    .w(px(14.))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        cx.stop_propagation();
                                        if !view.expanded_ns.remove(&row_key) {
                                            view.expanded_ns.insert(row_key.clone());
                                            if let Some(ns) = view
                                                .namespaces
                                                .iter()
                                                .find(|ns| ns_key(ns) == row_key)
                                                .cloned()
                                            {
                                                view.request_objects(ns);
                                            }
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        Icon::new(if expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .small()
                                        .text_color(hex(t.fg_muted)),
                                    ),
                            )
                            .child(
                                Icon::new(IconName::Database)
                                    .small()
                                    .text_color(hex(if is_current {
                                        t.accent
                                    } else {
                                        t.fg_muted
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(if is_current {
                                        t.accent
                                    } else {
                                        t.fg_primary
                                    }))
                                    .text_ellipsis()
                                    .child(SharedString::from(ns_label(&ns))),
                            ),
                    );

                    if !expanded {
                        continue;
                    }
                    let objects = self.objects.get(&key).cloned().unwrap_or_default();
                    if objects.is_empty() {
                        if loading {
                            let skeleton = div()
                                .pl(px(50.))
                                .py_1()
                                .opacity(if skeleton_visible { 1. } else { 0. })
                                .child(crate::design::skeleton_rows(cx, 3, 18.));
                            if skeleton_visible {
                                tree = tree.child(
                                    skeleton
                                        .id("object-loading-skeleton")
                                        .debug_selector(|| "object-loading-skeleton".to_string()),
                                );
                            } else {
                                tree = tree.child(skeleton);
                            }
                        } else {
                            tree = tree.child(
                                div()
                                    .pl(px(50.))
                                    .py_1()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_muted))
                                    .child("无对象"),
                            );
                        }
                        continue;
                    }
                    // 原对象面板的置顶 / 最近并入树中，状态仍按连接持久化。
                    let profile = self.object_profile_key();
                    let profile_state = self
                        .object_state
                        .profile(&profile)
                        .cloned()
                        .unwrap_or_default();
                    let ns_prefix = format!("{key}|");
                    let pinned: Vec<ObjectSummary> = objects
                        .iter()
                        .filter(|object| {
                            profile_state.is_pinned(&object_key(&object.path))
                                && matches_filter(object)
                        })
                        .cloned()
                        .collect();
                    let pinned_keys: HashSet<String> = pinned
                        .iter()
                        .map(|object| object_key(&object.path))
                        .collect();
                    let recent: Vec<ObjectSummary> = profile_state
                        .recent
                        .iter()
                        .filter(|recent_key| recent_key.starts_with(&ns_prefix))
                        .filter_map(|recent_key| {
                            objects
                                .iter()
                                .find(|object| &object_key(&object.path) == recent_key)
                        })
                        .filter(|object| {
                            matches_filter(object)
                                && !pinned_keys.contains(&object_key(&object.path))
                        })
                        .cloned()
                        .collect();
                    let section = |label: &'static str| {
                        div()
                            .pl(px(38.))
                            .pt_2()
                            .pb_1()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(SharedString::from(label))
                    };
                    if !pinned.is_empty() {
                        tree = tree.child(section("置顶"));
                        for object in &pinned {
                            tree =
                                tree.child(self.render_object_row(object, true, "obj-pinned", cx));
                        }
                    }
                    if !recent.is_empty() {
                        tree = tree.child(section("最近"));
                        for object in &recent {
                            tree =
                                tree.child(self.render_object_row(object, false, "obj-recent", cx));
                        }
                    }
                    let visible: Vec<ObjectSummary> = objects
                        .iter()
                        .filter(|object| {
                            matches_filter(object)
                                && !pinned_keys.contains(&object_key(&object.path))
                        })
                        .cloned()
                        .collect();
                    // 搜索时强制展开分组，命中的对象不用再逐层点开。
                    let forced_open = !filter.is_empty();
                    for (kind, items) in group_objects(&visible) {
                        let group_key = object_group_key(&profile, &key, kind);
                        let persisted_group = format!("{key}|{:?}", kind);
                        let group_open = forced_open
                            || self
                                .group_state
                                .get(&group_key)
                                .copied()
                                .unwrap_or(kind == ObjectKind::Table);
                        let toggle_group = group_key.clone();
                        let toggle_profile = profile.clone();
                        tree = tree.child(
                            h_flex()
                                .id(SharedString::from(format!("grp-{group_key}")))
                                .debug_selector({
                                    let selector = format!("grp-{group_key}");
                                    move || selector.clone()
                                })
                                .h(px(24.))
                                .pl(px(38.))
                                .pr_1()
                                .gap_1()
                                .items_center()
                                .rounded(px(5.))
                                .cursor_pointer()
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    let open = view
                                        .group_state
                                        .get(&toggle_group)
                                        .copied()
                                        .unwrap_or(kind == ObjectKind::Table);
                                    view.group_state.insert(toggle_group.clone(), !open);
                                    view.object_state
                                        .profile_mut(&toggle_profile)
                                        .set_group(&persisted_group, !open);
                                    view.persist_object_state();
                                    cx.notify();
                                }))
                                .child(
                                    Icon::new(if group_open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .small()
                                    .text_color(hex(t.fg_muted)),
                                )
                                .child(
                                    div()
                                        .text_size(px(crate::design::TS_CAPTION))
                                        .text_color(hex(t.fg_secondary))
                                        .child(kind_label(kind)),
                                ),
                        );
                        if !group_open {
                            continue;
                        }
                        let row_prefix = format!("obj-{key}");
                        for object in items {
                            tree =
                                tree.child(self.render_object_row(object, false, &row_prefix, cx));
                        }
                    }
                }
            }
        }

        tree
    }

    /// 历史面板当前筛选结果：数据源 + 关键字 + 来源 + 时间范围。
    pub(crate) fn history_entries(&self) -> Vec<rustdb_core::history::HistoryEntry> {
        let needle = self.history_filter.trim();
        self.core.history().list_filtered(&HistoryFilter {
            profile_id: self.active_profile.clone(),
            needle: (!needle.is_empty()).then(|| needle.to_string()),
            from_ms: self.history_range.from_ms(history_now_ms()),
            to_ms: None,
            source: self.history_source,
            pinned_only: false,
            limit: None,
        })
    }

    /// 查询历史面板：当前数据源的历史，可搜索、固定、插入编辑器。
    pub(crate) fn render_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = current(cx);
        // 搜索框按需创建一次。
        if self.history_input.is_none() {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder("搜索 SQL")
            });
            cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    view.history_filter = view
                        .history_input
                        .as_ref()
                        .map(|input| input.read(cx).value().to_string())
                        .unwrap_or_default();
                    cx.notify();
                }
            })
            .detach();
            self.history_input = Some(input);
        }

        let filter = self.history_filter.trim().to_lowercase();
        let entries = self.history_entries();
        let filtered = self.history_source.is_some() || self.history_range != HistoryRange::All;

        let mut list = v_flex().gap(px(1.)).p_1();
        if entries.is_empty() {
            list = list.child(
                div()
                    .p_3()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(if filter.is_empty() && !filtered {
                        "执行过的查询会出现在这里"
                    } else {
                        "没有匹配的历史"
                    }),
            );
        }
        for entry in &entries {
            let id = entry.id;
            let sql = entry.sql.clone();
            let pinned = entry.pinned;
            // 单行预览：折叠空白，最多 80 个字符。
            let preview: String = sql
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(80)
                .collect();
            let status = match entry.ok {
                Some(true) => (t.success, format!("{} ms", entry.elapsed_ms.unwrap_or(0))),
                Some(false) => (t.danger, "失败".to_string()),
                None => (t.fg_muted, "…".to_string()),
            };
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("hist-{id}")))
                    .debug_selector({
                        let selector = format!("hist-{id}");
                        move || selector.clone()
                    })
                    .h(px(26.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.apply_history(sql.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(preview)),
                    )
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(status.0))
                            .child(SharedString::from(status.1)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("hist-pin-{id}")))
                            .debug_selector({
                                let selector = format!("hist-pin-{id}");
                                move || selector.clone()
                            })
                            .px_1()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.core.history().toggle_pin(id);
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
                    ),
            );
        }

        let source_active = self.history_source.is_some();
        let range_active = self.history_range != HistoryRange::All;
        let source_label = self.history_source.map_or("全部来源", HistorySource::label);
        let range_label = self.history_range.label();
        let mut filters = h_flex().px_2().pb_2().gap_1().items_center();
        for (selector, menu, label, active) in [
            (
                "history-source-filter",
                HistoryMenu::Source,
                source_label,
                source_active,
            ),
            (
                "history-range-filter",
                HistoryMenu::Range,
                range_label,
                range_active,
            ),
        ] {
            filters = filters.child(
                h_flex()
                    .id(SharedString::from(selector))
                    .debug_selector({
                        let selector = selector.to_string();
                        move || selector.clone()
                    })
                    .h(px(20.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(px(4.))
                    .bg(hex(if active { t.bg_overlay } else { t.bg_elevated }))
                    .border_1()
                    .border_color(hex(if active { t.accent } else { t.border_subtle }))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, event: &gpui::ClickEvent, _, cx| {
                        view.history_menu = Some((event.position(), menu));
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(if active { t.accent } else { t.fg_secondary }))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );
        }

        v_flex()
            .size_full()
            .child(div().p_2().child(gpui_component::input::Input::new(
                self.history_input.as_ref().expect("搜索框已创建"),
            )))
            .child(filters)
            .child(
                div()
                    .id("history-list")
                    .debug_selector(|| "history-list".to_string())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(list),
            )
    }

    /// 历史筛选下拉：来源 / 时间范围。
    pub(crate) fn render_history_menu(
        &mut self,
        kind: HistoryMenu,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let mut col = v_flex()
            .w(px(148.))
            .p_1()
            .gap(px(1.))
            .rounded(px(6.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated));

        let item = |selector: String, label: &'static str, active: bool, pick: HistoryPick| {
            h_flex()
                .id(SharedString::from(selector.clone()))
                .debug_selector({
                    let selector = selector.clone();
                    move || selector.clone()
                })
                .h(px(24.))
                .px_2()
                .gap_2()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(hex(t.bg_overlay)))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.apply_history_filter(pick, cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(if active { t.accent } else { t.fg_primary }))
                        .child(SharedString::from(label)),
                )
                .when(active, |row| {
                    row.child(Icon::new(IconName::Check).small().text_color(hex(t.accent)))
                })
        };

        match kind {
            HistoryMenu::Source => {
                for source in std::iter::once(None).chain(HistorySource::ALL.map(Some)) {
                    let active = self.history_source == source;
                    let label = source.map_or("全部来源", HistorySource::label);
                    col = col.child(item(
                        format!("history-source-{}", history_source_key(source)),
                        label,
                        active,
                        HistoryPick::Source(source),
                    ));
                }
            }
            HistoryMenu::Range => {
                for range in HistoryRange::ALL {
                    let active = self.history_range == range;
                    col = col.child(item(
                        format!("history-range-{}", range.key()),
                        range.label(),
                        active,
                        HistoryPick::Range(range),
                    ));
                }
            }
        }
        col.into_any_element()
    }

    /// 历史筛选下拉的选中动作。
    pub(crate) fn apply_history_filter(&mut self, pick: HistoryPick, cx: &mut Context<Self>) {
        match pick {
            HistoryPick::Source(source) => self.history_source = source,
            HistoryPick::Range(range) => self.history_range = range,
        }
        self.history_menu = None;
        cx.notify();
    }

    /// 把历史里的 SQL 放进当前标签页（没有标签就新建一个）。
    pub(crate) fn apply_history(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.is_empty() {
            let ns = self.current_ns.clone();
            self.open_tab(
                ns.unwrap_or_else(|| Namespace::new(None::<&str>, None::<&str>)),
                window,
                cx,
            );
        }
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        self.status = "已插入历史 SQL".into();
        self.status_kind = StatusKind::Ok;
        cx.notify();
    }

    /// F-84：连接树中的对象行（名称可点开预览，右侧图钉切换置顶）。
    pub(crate) fn render_object_row(
        &self,
        object: &ObjectSummary,
        pinned: bool,
        id_prefix: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let path = object.path.clone();
        let name = path.name.to_string();
        let comment = object.comment.clone();
        let kind = path.kind;
        let queryable = matches!(
            kind,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        );
        let selector = format!("{id_prefix}-{name}");
        let pin_selector = format!("obj-pin-{name}");
        let pin_group = format!("{id_prefix}-row-{name}");
        let row_path = path.clone();
        let pin_path = path.clone();
        let menu_path = path.clone();
        h_flex()
            .group(SharedString::from(pin_group.clone()))
            .items_center()
            .h(px(24.))
            .gap_2()
            .pl(px(46.))
            .pr_2()
            .rounded(px(5.))
            .hover(|s| s.bg(hex(t.bg_overlay)))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                    view.obj_menu = Some(ObjectMenu {
                        path: menu_path.clone(),
                        at: event.position,
                    });
                    cx.notify();
                }),
            )
            .child(
                Icon::new(kind_icon(kind))
                    .small()
                    .text_color(hex(t.fg_muted)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("obj-open-{selector}")))
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .min_w_0()
                    .when(queryable, |el| el.cursor_pointer())
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if queryable {
                            view.preview_object(row_path.clone(), 200, window, cx);
                        }
                    }))
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_primary))
                            .text_ellipsis()
                            .child(SharedString::from(name.clone())),
                    )
                    .when_some(comment, |el, comment| {
                        el.child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .text_ellipsis()
                                .child(SharedString::from(comment)),
                        )
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("obj-pin-btn-{name}")))
                    .debug_selector(move || pin_selector.clone())
                    .cursor_pointer()
                    // 未置顶时只在悬停该行时露出图钉，避免整列图标堆在一起。
                    .when(!pinned, |el| {
                        el.opacity(0.)
                            .group_hover(SharedString::from(pin_group.clone()), |style| {
                                style.opacity(1.)
                            })
                    })
                    .child(
                        Icon::new(if pinned {
                            IconName::Pin
                        } else {
                            IconName::PinOff
                        })
                        .small()
                        .text_color(hex(if pinned {
                            t.accent
                        } else {
                            t.fg_muted
                        })),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.toggle_object_pin(&pin_path, cx);
                    })),
            )
            .into_any_element()
    }

    /// F-64：SQL 文件树（目录 + `.sql`，点文件名打开到标签）。
    pub(crate) fn render_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = current(cx);
        let root = self.file_root.clone();
        let header = h_flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .text_ellipsis()
                    .child(SharedString::from(
                        root.as_deref()
                            .map(file_label)
                            .unwrap_or_else(|| "未选择文件夹".into()),
                    )),
            )
            .child(
                Button::new("files-pick")
                    .small()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .tooltip("选择文件夹")
                    .on_click(cx.listener(|view, _, window, cx| view.pick_file_root(window, cx))),
            )
            .child(
                Button::new("files-refresh")
                    .small()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("刷新文件树")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.file_expanded.clear();
                        cx.notify();
                    })),
            );
        let layout = v_flex().p_2().gap(px(2.)).child(header);
        let Some(root) = root else {
            return layout.child(hint_text("选择文件夹后浏览 .sql 文件", &t));
        };
        if !root.is_dir() {
            return layout.child(hint_text("文件夹不存在或不可访问", &t));
        }
        let rows = self.file_tree_nodes(&root, 0, window, cx);
        layout.child(rows)
    }

    pub(crate) fn file_tree_nodes(
        &mut self,
        dir: &Path,
        depth: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let t = current(cx);
        let mut rows = v_flex().gap(px(1.)).w_full();
        for (path, is_dir) in sql_dir_entries(dir) {
            let name = file_label(&path);
            let expanded = is_dir && self.file_expanded.contains(&path);
            let selector = if is_dir {
                format!("file-dir-{name}")
            } else {
                format!("file-{name}")
            };
            let mut row = h_flex()
                .items_center()
                .gap(px(4.))
                .pl(px(4. + depth as f32 * 10.))
                .py_1()
                .rounded(px(5.))
                .hover(|s| s.bg(hex(t.bg_overlay)))
                .debug_selector({
                    let selector = selector.clone();
                    move || selector.clone()
                })
                .child(
                    Icon::new(if is_dir {
                        if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        }
                    } else {
                        IconName::SquareCode
                    })
                    .small()
                    .text_color(hex(if is_dir {
                        t.fg_muted
                    } else {
                        t.accent
                    })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_primary))
                        .text_ellipsis()
                        .child(SharedString::from(name)),
                );
            if is_dir {
                let toggle = path.clone();
                row = row.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        if !view.file_expanded.remove(&toggle) {
                            view.file_expanded.insert(toggle.clone());
                        }
                        cx.notify();
                    }),
                );
            } else {
                let open = path.clone();
                row = row.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |view, _, window, cx| {
                        view.open_file_path(open.clone(), window, cx);
                    }),
                );
            }
            rows = rows.child(row);
            if expanded {
                rows = rows.child(self.file_tree_nodes(&path, depth + 1, window, cx));
            }
        }
        rows
    }

    /// 选择文件树根目录；切换后清空展开状态。
    pub(crate) fn pick_file_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from("选择 SQL 文件目录")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let _ = this.update_in(cx, |view, _, cx| {
                view.file_root = Some(path);
                view.file_expanded.clear();
                view.set_status("已选择文件目录", StatusKind::Ok, cx);
            });
        })
        .detach();
    }

    /// 读盘并打开文件（文件树与「打开」按钮共用）。
    pub(crate) fn open_file_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reading = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let text = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(&reading) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| match text {
                Ok(text) => view.load_sql_file(path, text, window, cx),
                Err(error) => view.set_status(format!("打开失败：{error}"), StatusKind::Error, cx),
            });
        })
        .detach();
    }

    /// 参数面板（F-62）：逐项输入 + 类型切换 + 执行/取消。
    pub(crate) fn render_params_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = current(cx);
        let panel = self.params_panel.as_ref()?;
        let slots = panel.slots.clone();
        let warning = panel.warning.clone();
        let confirm_raw = panel.confirm_raw;

        let mut rows = v_flex().gap(px(3.));
        for (index, slot) in slots.into_iter().enumerate() {
            let raw = slot.input.read(cx).value().to_string();
            let label = param_kind_label(&slot, &raw);
            let name = format!("{} {}", slot.style.label(), slot.name);
            rows = rows.child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(126.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .text_ellipsis()
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&slot.input).small()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("param-kind-{index}")))
                            .small()
                            .ghost()
                            .label(SharedString::from(label))
                            .tooltip("切换参数类型（自动 → 文本 → 数字 → 布尔 → NULL → 原始 SQL → 列表）")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.cycle_param_kind(index, cx)
                            })),
                    ),
            );
        }

        let run_label = if confirm_raw {
            "确认并执行"
        } else {
            "执行"
        };
        Some(
            v_flex()
                .id("params-panel")
                .debug_selector(|| "params-panel".to_string())
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
                        .child(Icon::new(IconName::Play).small().text_color(hex(t.accent)))
                        .child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_primary))
                                .child("SQL 参数"),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("params-cancel")
                                .small()
                                .ghost()
                                .label("取消")
                                .on_click(
                                    cx.listener(|view, _, _, cx| view.close_params_panel(cx)),
                                ),
                        )
                        .child(
                            div().debug_selector(|| "params-run".to_string()).child(
                                Button::new("params-run-btn")
                                    .small()
                                    .primary()
                                    .label(run_label)
                                    .on_click(cx.listener(|view, _, _, cx| view.apply_params(cx))),
                            ),
                        ),
                )
                .child(rows)
                .when_some(warning, |panel, warning| {
                    panel.child(
                        div()
                            .debug_selector(|| "params-warning".to_string())
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.danger))
                            .child(SharedString::from(warning)),
                    )
                })
                .into_any_element(),
        )
    }
}
