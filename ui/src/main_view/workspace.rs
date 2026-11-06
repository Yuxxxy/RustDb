//! 工作区布局、标签栏、数据库/对象菜单、危险确认与 Esc 分发。

use super::*;

impl MainView {
    pub(crate) fn render_workspace(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let has_tab = self.active().is_some();
        let tab_bar = self.render_tab_bar(cx);

        let toolbar =
            h_flex()
                .h(px(34.))
                .px_2()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(hex(t.border_subtle))
                .child(
                    Button::new("run-query")
                        .small()
                        .primary()
                        .icon(IconName::Play)
                        .label("执行")
                        .on_click(cx.listener(|view, _, window, cx| view.run(window, cx))),
                )
                .child(
                    Button::new("cancel-query")
                        .small()
                        .ghost()
                        .icon(IconName::Square)
                        .label("取消")
                        .on_click(cx.listener(|view, _, _, cx| view.cancel(cx))),
                )
                .child(
                    div().debug_selector(|| "preview-dml".to_string()).child(
                        Button::new("preview-dml-btn")
                            .small()
                            .ghost()
                            .icon(IconName::Eye)
                            .label("只读预览")
                            .tooltip("改写为只读 SELECT")
                            .on_click(cx.listener(|view, _, _, cx| view.preview_dml(cx))),
                    ),
                )
                .child(div().w(px(1.)).h(px(16.)).mx_1().bg(hex(t.border_subtle)))
                .child(
                    div().debug_selector(|| "open-sql".to_string()).child(
                        Button::new("open-sql-btn")
                            .small()
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .tooltip("打开文件")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.open_sql_file(window, cx)),
                            ),
                    ),
                )
                .child(
                    div().debug_selector(|| "save-sql".to_string()).child(
                        Button::new("save-sql-btn")
                            .small()
                            .ghost()
                            .icon(IconName::Save)
                            .tooltip("保存 SQL")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.save_sql_file(false, window, cx)
                            })),
                    ),
                )
                .child(div().w(px(1.)).h(px(16.)).mx_1().bg(hex(t.border_subtle)))
                .child(self.render_db_picker(cx))
                .child(div().w(px(1.)).h(px(16.)).mx_1().bg(hex(t.border_subtle)))
                .child(self.render_tx_controls(cx))
                .when_some(self.diagnostic_badge(), |row, (label, tip)| {
                    row.child(
                        div().debug_selector(|| "diag-badge".to_string()).child(
                            Button::new("diag-badge-btn")
                                .small()
                                .ghost()
                                .label(SharedString::from(label))
                                .tooltip(tip)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.jump_to_issue(window, cx)
                                })),
                        ),
                    )
                })
                .child(div().flex_1())
                .child(
                    div()
                        .debug_selector(|| "export-slot".to_string())
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .child(
                            Button::new("export-csv")
                                .small()
                                .ghost()
                                .label("导出")
                                .tooltip("导出 CSV")
                                .on_click(cx.listener(|view, _, _, cx| view.export_csv(cx))),
                        )
                        .child(
                            div()
                                .id("export-format")
                                .debug_selector(|| "export-format".to_string())
                                .h(px(24.))
                                .w(px(20.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|view, event: &gpui::MouseDownEvent, _, cx| {
                                        view.export_menu = Some(event.position);
                                        cx.notify();
                                    }),
                                )
                                .child(
                                    Icon::new(IconName::ChevronDown)
                                        .small()
                                        .text_color(hex(t.fg_secondary)),
                                ),
                        ),
                );

        let import_panel = self.render_import_panel(cx);
        let body: gpui::AnyElement = if has_tab {
            let params_panel = self.render_params_panel(cx);
            let editor = div()
                .h(px(176.))
                .p_2()
                .border_b_1()
                .border_color(hex(t.border_subtle))
                .child(Editor::new(&self.sql_input).h(px(156.)));
            let result_area = self.render_result(cx);
            v_flex()
                .flex_1()
                .min_h_0()
                .when_some(params_panel, |layout, panel| layout.child(panel))
                .child(editor)
                .child(result_area)
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::Database).text_color(hex(t.fg_muted)))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(hex(t.fg_muted))
                        .child("从左侧连接打开查询"),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(tab_bar)
            .child(toolbar)
            .when_some(import_panel, |layout, panel| layout.child(panel))
            .child(body)
    }

    /// 顶部标签栏：每个标签页绑定一个库。
    pub(crate) fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let tabs: Vec<(usize, u64, String, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| (index, tab.id, tab.title(), index == self.active_tab))
            .collect();
        let mut row = h_flex()
            .h(px(31.))
            .w_full()
            .flex_none()
            .items_center()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(hex(t.border_subtle));
        if tabs.is_empty() {
            row = row.child(
                div()
                    .px_1()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_muted))
                    .child("未打开查询"),
            );
        }
        for (index, id, title, active) in tabs {
            row =
                row.child(
                    h_flex()
                        .id(SharedString::from(format!("tab-{id}")))
                        .debug_selector({
                            let selector = format!("tab-{id}");
                            move || selector.clone()
                        })
                        .h(px(23.))
                        .pl_2()
                        .pr_1()
                        .gap_1()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(active, |d| d.bg(hex(t.bg_overlay)))
                        .hover(|s| s.bg(hex(t.bg_overlay)))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.activate_tab(index, window, cx)
                        }))
                        .child(
                            Icon::new(IconName::SquareTerminal)
                                .small()
                                .text_color(hex(if active { t.accent } else { t.fg_muted })),
                        )
                        .child(
                            div()
                                .max_w(px(120.))
                                .text_size(px(11.))
                                .text_color(hex(if active { t.fg_primary } else { t.fg_secondary }))
                                .text_ellipsis()
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("tab-close-{id}")))
                                .debug_selector({
                                    let selector = format!("tab-close-{id}");
                                    move || selector.clone()
                                })
                                .w(px(16.))
                                .h(px(16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.))
                                .hover(|s| s.bg(hex(t.bg_base)))
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    cx.stop_propagation();
                                    view.close_tab(index, window, cx);
                                }))
                                .child(
                                    Icon::new(IconName::Close)
                                        .small()
                                        .text_color(hex(t.fg_muted)),
                                ),
                        ),
                );
        }
        row.child(div().flex_1())
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(
                        self.server_label.clone().unwrap_or_else(|| "未连接".into()),
                    )),
            )
    }

    /// 查询页的库下拉框（切库）。
    pub(crate) fn render_db_picker(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let current = self
            .active()
            .and_then(|tab| tab.ns.clone())
            .or_else(|| self.current_ns.clone());
        let label = current
            .as_ref()
            .map(ns_label)
            .unwrap_or_else(|| "选择库".into());
        let enabled = !self.namespaces.is_empty();
        div().relative().child(
            h_flex()
                .id("db-picker")
                .debug_selector(|| "db-picker".to_string())
                .h(px(22.))
                .px_2()
                .gap_1()
                .rounded(px(5.))
                .border_1()
                .border_color(hex(t.border_subtle))
                .bg(hex(t.bg_elevated))
                .when(enabled, |el| el.cursor_pointer())
                .hover(|s| s.bg(hex(t.bg_overlay)))
                .on_click(cx.listener(move |view, event: &gpui::ClickEvent, _, cx| {
                    if view.namespaces.is_empty() {
                        return;
                    }
                    let anchor = event.position();
                    view.db_menu = if view.db_menu.is_some() {
                        None
                    } else {
                        Some(anchor)
                    };
                    cx.notify();
                }))
                .child(
                    Icon::new(IconName::Database)
                        .small()
                        .text_color(hex(if enabled { t.accent } else { t.fg_muted })),
                )
                .child(
                    div()
                        .max_w(px(150.))
                        .text_size(px(10.5))
                        .text_color(hex(if enabled { t.fg_primary } else { t.fg_muted }))
                        .text_ellipsis()
                        .child(SharedString::from(label)),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .small()
                        .text_color(hex(t.fg_muted)),
                ),
        )
    }

    /// 库下拉列表（覆盖层内容）。
    pub(crate) fn render_db_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let current_key = self.active().and_then(|tab| tab.ns_key());
        let mut menu = v_flex()
            .w(px(196.))
            .max_h(px(280.))
            .overflow_y_scrollbar()
            .scroll_bounce(false)
            .p_1()
            .gap(px(1.))
            .rounded(px(6.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated));
        for ns in self.namespaces.clone() {
            let key = ns_key(&ns);
            let active = current_key.as_deref() == Some(key.as_str());
            let pick = ns.clone();
            menu = menu.child(
                h_flex()
                    .id(SharedString::from(format!("db-option-{key}")))
                    .debug_selector({
                        let selector = format!("db-option-{key}");
                        move || selector.clone()
                    })
                    .h(px(24.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(active, |row| row.bg(hex(t.bg_overlay)))
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.db_menu = None;
                        view.expanded_ns.insert(ns_key(&pick));
                        view.request_objects(pick.clone());
                        view.switch_to(pick.clone(), cx);
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconName::Database)
                            .small()
                            .text_color(hex(if active { t.accent } else { t.fg_muted })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(hex(if active { t.accent } else { t.fg_primary }))
                            .text_ellipsis()
                            .child(SharedString::from(ns_label(&ns))),
                    )
                    .when(active, |row| {
                        row.child(Icon::new(IconName::Check).small().text_color(hex(t.accent)))
                    }),
            );
        }
        menu
    }

    /// 通用的右键菜单渲染（仿 Navicat：分组、图标、快捷键提示、危险项红色）。
    /// 菜单本身不含标题行。
    pub(crate) fn render_menu(
        &mut self,
        groups: Vec<Vec<MenuEntry>>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let mut col = v_flex()
            .w(px(204.))
            .p_1()
            .gap(px(1.))
            .rounded(px(6.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated));

        for (group_index, group) in groups.into_iter().enumerate() {
            if group_index > 0 {
                col = col.child(div().h(px(1.)).my_1().mx_1().bg(hex(t.border_subtle)));
            }
            for entry in group {
                let label = entry.label.clone();
                let hint = entry.hint.clone();
                let danger = entry.danger;
                let icon = entry.icon;
                let action = entry.action.clone();
                let selector = if entry.key.is_empty() {
                    format!("menu-{label}")
                } else {
                    format!("menu-{}", entry.key)
                };
                col = col.child(
                    h_flex()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector({
                            let selector = selector.clone();
                            move || selector.clone()
                        })
                        .h(px(25.))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(hex(t.bg_overlay)))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.dispatch_menu(action.clone(), window, cx);
                        }))
                        .child(Icon::new(icon).small().text_color(hex(if danger {
                            t.danger
                        } else {
                            t.fg_muted
                        })))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .text_color(hex(if danger { t.danger } else { t.fg_primary }))
                                .text_ellipsis()
                                .child(SharedString::from(label)),
                        )
                        .when_some(hint, |row, hint| {
                            row.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(hex(t.fg_muted))
                                    .child(SharedString::from(hint)),
                            )
                        }),
                );
            }
        }
        col.into_any_element()
    }

    /// 对象右键菜单的动作清单：按种类给不同组合，顺序贴近 Navicat。
    pub(crate) fn object_menu_groups(&self) -> Vec<Vec<MenuEntry>> {
        let Some(menu) = self.obj_menu.clone() else {
            return Vec::new();
        };
        let path = menu.path;
        let kind = path.kind;
        let name = path.name.to_string();
        let qualified = path.display_label();
        let selectable = matches!(
            kind,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        );
        let is_table = kind == ObjectKind::Table;
        // F-82：驱动没开 `routine_source_edit` 时不显示源码入口（R5：不假装支持）。
        let is_routine = routine_kind_of(kind).is_some()
            && self
                .capabilities
                .as_ref()
                .is_some_and(|caps| caps.routine_source_edit);
        let mut groups: Vec<Vec<MenuEntry>> = Vec::new();

        if selectable {
            let mut open = vec![MenuEntry::new(
                MenuAction::OpenData(path.clone()),
                if is_table {
                    "打开表"
                } else {
                    "打开视图"
                },
                IconName::Play,
            )
            .key("obj-open")
            .hint("100 行")];
            if is_table {
                open.push(
                    MenuEntry::new(
                        MenuAction::DesignTable(path.clone()),
                        "设计表",
                        IconName::Columns3,
                    )
                    .key("obj-design"),
                );
                // F-85：驱动没开 `partition_admin` 时不显示入口（R5：不假装支持）。
                if self
                    .capabilities
                    .as_ref()
                    .is_some_and(|caps| caps.partition_admin)
                {
                    open.push(
                        MenuEntry::new(
                            MenuAction::ManagePartitions(path.clone()),
                            "分区管理…",
                            IconName::Layers,
                        )
                        .key("obj-partitions"),
                    );
                }
                open.push(
                    MenuEntry::new(
                        MenuAction::ShowLineage(path.clone()),
                        "字段血缘…",
                        IconName::ArrowLeftRight,
                    )
                    .key("obj-lineage"),
                );
            }
            if is_routine {
                open.push(
                    MenuEntry::new(
                        MenuAction::EditRoutine(path.clone()),
                        "查看 / 编辑源码",
                        IconName::Braces,
                    )
                    .key("obj-source"),
                );
            }
            groups.push(open);

            groups.push(vec![
                MenuEntry::new(
                    MenuAction::ShowDdl(path.clone()),
                    "查看 DDL",
                    IconName::SquareCode,
                )
                .key("obj-ddl"),
                MenuEntry::new(
                    MenuAction::DumpSql(path.clone()),
                    "转储 SQL",
                    IconName::Download,
                )
                .key("obj-dump"),
                MenuEntry::new(
                    MenuAction::ImportTable(path.clone()),
                    "导入数据…",
                    IconName::FolderOpen,
                )
                .key("obj-import"),
                MenuEntry::new(
                    MenuAction::CountRows(path.clone()),
                    "统计行数",
                    IconName::Sigma,
                )
                .key("obj-count"),
            ]);
            if is_table {
                groups.push(vec![MenuEntry::new(
                    MenuAction::MaintainTable(path.clone()),
                    "维护表",
                    IconName::Wrench,
                )
                .key("obj-maintain")]);
            }
            groups.push(vec![MenuEntry::new(
                MenuAction::RenameObject(path.clone()),
                "重命名…",
                IconName::Pencil,
            )
            .key("obj-rename")]);
            let pinned = self
                .object_state
                .profile(&self.object_profile_key())
                .is_some_and(|state| state.is_pinned(&object_key(&path)));
            groups.push(vec![MenuEntry::new(
                MenuAction::TogglePin(path.clone()),
                if pinned { "取消置顶" } else { "置顶" },
                if pinned {
                    IconName::PinOff
                } else {
                    IconName::Pin
                },
            )
            .key("obj-pin")]);
        } else {
            if is_routine {
                groups.push(vec![MenuEntry::new(
                    MenuAction::EditRoutine(path.clone()),
                    "查看 / 编辑源码",
                    IconName::Braces,
                )
                .key("obj-source")]);
            }
            groups.push(vec![MenuEntry::new(
                MenuAction::ShowDdl(path.clone()),
                "查看定义",
                IconName::SquareCode,
            )
            .key("obj-ddl")]);
        }

        if is_table {
            groups.push(vec![MenuEntry::new(
                MenuAction::Truncate(path.clone()),
                "清空表",
                IconName::Eraser,
            )
            .key("obj-truncate")]);
        }

        let can_drop = matches!(
            kind,
            ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::Procedure
                | ObjectKind::Function
                | ObjectKind::Trigger
                | ObjectKind::Sequence
        );
        if can_drop {
            groups.push(vec![MenuEntry::new(
                MenuAction::Drop(path.clone()),
                "删除…",
                IconName::Trash,
            )
            .key("obj-drop")
            .danger()]);
        }

        groups.push(vec![
            MenuEntry::new(MenuAction::Copy(name), "复制名称", IconName::Copy).key("obj-copy"),
            MenuEntry::new(MenuAction::Copy(qualified), "复制限定名", IconName::Copy)
                .key("obj-copy-qualified"),
        ]);
        groups
    }

    /// 库节点右键菜单：打开 / 新建对象 / 刷新 / 复制。
    pub(crate) fn ns_menu_groups(&self) -> Vec<Vec<MenuEntry>> {
        let Some(menu) = self.ns_menu.clone() else {
            return Vec::new();
        };
        let ns = menu.ns;
        let name = ns_label(&ns);
        vec![
            vec![
                MenuEntry::new(
                    MenuAction::OpenDatabase(ns.clone()),
                    "打开数据库",
                    IconName::SquareTerminal,
                )
                .key("ns-open"),
                MenuEntry::new(
                    MenuAction::RefreshNamespace(ns.clone()),
                    "刷新",
                    IconName::RefreshCw,
                )
                .key("ns-refresh"),
            ],
            vec![
                MenuEntry::new(
                    MenuAction::NewObject(ns.clone(), ObjectKind::Table),
                    "新建表",
                    IconName::Table,
                )
                .key("ns-new-table"),
                MenuEntry::new(
                    MenuAction::NewObject(ns.clone(), ObjectKind::View),
                    "新建视图",
                    IconName::Eye,
                )
                .key("ns-new-view"),
                MenuEntry::new(
                    MenuAction::NewObject(ns.clone(), ObjectKind::Procedure),
                    "新建存储过程",
                    IconName::SquareFunction,
                )
                .key("ns-new-proc"),
            ],
            vec![MenuEntry::new(MenuAction::Copy(name), "复制库名", IconName::Copy).key("ns-copy")],
        ]
    }

    /// 连接节点右键菜单：新建查询 / 刷新 / 断开。
    pub(crate) fn conn_menu_groups(&self) -> Vec<Vec<MenuEntry>> {
        let Some(menu) = self.conn_menu.clone() else {
            return Vec::new();
        };
        let connected =
            self.session.is_some() && self.active_profile.as_deref() == Some(menu.0.as_str());
        let mut groups = Vec::new();
        let mut first = Vec::new();
        if connected {
            let ns = self
                .current_ns
                .clone()
                .or_else(|| self.namespaces.first().cloned())
                .unwrap_or_else(|| Namespace::new(None::<&str>, None::<&str>));
            first.push(
                MenuEntry::new(
                    MenuAction::NewQuery(ns.clone()),
                    "新建查询",
                    IconName::SquareTerminal,
                )
                .key("conn-new-query"),
            );
            first.push(
                MenuEntry::new(
                    MenuAction::RefreshNamespace(ns),
                    "刷新对象",
                    IconName::RefreshCw,
                )
                .key("conn-refresh"),
            );
        } else {
            first.push(
                MenuEntry::new(
                    MenuAction::Connect(menu.0.clone()),
                    "打开连接",
                    IconName::PlugZap,
                )
                .key("conn-connect"),
            );
        }
        groups.push(first);
        if let Some(profile) = self.profiles.iter().find(|p| p.id == menu.0) {
            // F-89：生产标记与临时写解锁；只读连接不提供解锁。
            let mark = if profile.production {
                MenuEntry::new(
                    MenuAction::SetProduction(profile.id.clone(), false),
                    "取消生产标记",
                    IconName::TriangleAlert,
                )
                .key("conn-prod-clear")
            } else {
                MenuEntry::new(
                    MenuAction::SetProduction(profile.id.clone(), true),
                    "标记为生产连接",
                    IconName::TriangleAlert,
                )
                .key("conn-prod-set")
            };
            groups.push(vec![mark]);
            if profile.production && !profile.read_only {
                let unlocked = connected && self.core.write_unlock_remaining(&profile.id).is_some();
                if unlocked {
                    groups.push(vec![MenuEntry::new(
                        MenuAction::LockWrites(profile.id.clone()),
                        "立即回锁",
                        IconName::KeyRound,
                    )
                    .key("conn-lock")]);
                } else if connected {
                    groups.push(vec![
                        MenuEntry::new(
                            MenuAction::UnlockWrites(profile.id.clone(), 60),
                            "临时解锁写 1 分钟",
                            IconName::Clock,
                        )
                        .key("conn-unlock-1")
                        .hint("到期自动回锁；AI 仍拒绝生产写入"),
                        MenuEntry::new(
                            MenuAction::UnlockWrites(profile.id.clone(), 300),
                            "临时解锁写 5 分钟",
                            IconName::Clock,
                        )
                        .key("conn-unlock-5")
                        .hint("到期自动回锁；AI 仍拒绝生产写入"),
                    ]);
                }
            }
        }
        groups.push(vec![MenuEntry::new(
            MenuAction::Copy(menu.1.clone()),
            "复制连接名",
            IconName::Copy,
        )
        .key("conn-copy")]);
        // F-02：连接配置的增删改。
        groups.push(vec![
            MenuEntry::new(
                MenuAction::EditConnection(menu.0.clone()),
                "编辑连接",
                IconName::Settings,
            )
            .key("conn-edit"),
            MenuEntry::new(
                MenuAction::DeleteConnection(menu.0.clone()),
                "删除连接",
                IconName::Trash,
            )
            .key("conn-delete")
            .danger(),
        ]);
        if connected {
            groups.push(vec![MenuEntry::new(
                MenuAction::Disconnect,
                "断开连接",
                IconName::PlugZap,
            )
            .key("conn-disconnect")
            .danger()]);
        }
        groups
    }

    pub(crate) fn render_object_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = self.object_menu_groups();
        self.render_menu(groups, cx)
    }

    pub(crate) fn render_ns_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = self.ns_menu_groups();
        self.render_menu(groups, cx)
    }

    pub(crate) fn render_conn_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = self.conn_menu_groups();
        self.render_menu(groups, cx)
    }

    /// F-91：主机密钥确认浮层（首次连接 / 密钥变更）。
    pub(crate) fn render_host_key_prompt(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some(prompt) = self.host_key_prompt.as_ref() else {
            return div().into_any_element();
        };
        let endpoint = match (&prompt.host, prompt.port) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.clone(),
            _ => "未知主机".to_string(),
        };
        let title = if prompt.changed {
            "主机密钥已变更"
        } else {
            "确认主机密钥"
        };
        let detail = if prompt.changed {
            "该主机的密钥与已有记录不同，可能是中间人替换。确认前不会发送任何凭据。"
        } else {
            "首次连接该主机，需要你确认它的密钥。确认前不会发送任何凭据。"
        };

        v_flex()
            .id("host-key-prompt")
            .debug_selector(|| "host-key-prompt".to_string())
            .w(px(520.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(if prompt.changed {
                t.danger
            } else {
                t.border_subtle
            }))
            .bg(hex(t.bg_elevated))
            .occlude()
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(if prompt.changed {
                        t.danger
                    } else {
                        t.fg_primary
                    }))
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_secondary))
                    .child(SharedString::from(detail)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_size(px(10.)).text_color(hex(t.fg_muted)).child(
                        SharedString::from(format!("第 {} 层 · {}", prompt.layer, endpoint)),
                    ))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_primary))
                            .child(SharedString::from(format!("指纹：{}", prompt.fingerprint))),
                    )
                    .when(prompt.recorded.is_some(), |column| {
                        column.child(div().text_size(px(10.)).text_color(hex(t.warning)).child(
                            SharedString::from(format!(
                                "原记录：{}",
                                prompt.recorded.clone().unwrap_or_default()
                            )),
                        ))
                    })
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(hex(t.fg_muted))
                            .whitespace_normal()
                            .child(SharedString::from(prompt.public_key.clone())),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .debug_selector(|| "host-key-reject".to_string())
                            .child(
                                Button::new("host-key-reject-btn")
                                    .small()
                                    .ghost()
                                    .label("拒绝并断开")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.resolve_host_key(false, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "host-key-accept".to_string())
                            .child(
                                Button::new("host-key-accept-btn")
                                    .small()
                                    .primary()
                                    .label("信任并继续")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.resolve_host_key(true, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// F-91：处理主机密钥确认结果（确认后重试连接）。
    pub(crate) fn resolve_host_key(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.host_key_prompt.take() else {
            return;
        };
        match self
            .core
            .confirm_tunnel_host_key(&prompt.profile_id, accept)
        {
            Ok(message) => {
                self.status = message;
                self.status_kind = if accept {
                    StatusKind::Ok
                } else {
                    StatusKind::Idle
                };
                if accept {
                    // 重新发起连接：这次主机密钥已被信任。
                    self.connecting = Some(prompt.profile_id.clone());
                    self.core.connect(&prompt.profile_id);
                } else {
                    self.connecting = None;
                }
            }
            Err(error) => {
                self.status = error;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 菜单动作的唯一出口。
    pub(crate) fn dispatch_menu(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 任何动作都会先关掉菜单。
        self.obj_menu = None;
        self.ns_menu = None;
        self.conn_menu = None;
        self.export_menu = None;
        self.row_menu = None;
        match action {
            MenuAction::Connect(profile_id) => {
                self.connect(profile_id, cx);
            }
            MenuAction::Disconnect => {
                if let Some(session) = self.session.clone() {
                    self.core.disconnect(&session);
                }
            }
            MenuAction::EditConnection(profile_id) => {
                self.open_conn_editor(Some(&profile_id), window, cx);
            }
            MenuAction::DeleteConnection(profile_id) => {
                match self.core.remove_profile(&profile_id) {
                    Ok(()) => {
                        self.profiles = self.core.profiles();
                        self.status = format!("已删除连接 {profile_id}");
                        self.status_kind = StatusKind::Ok;
                    }
                    Err(message) => {
                        self.status = message;
                        self.status_kind = StatusKind::Error;
                    }
                }
                cx.notify();
            }
            MenuAction::SetProduction(profile_id, production) => {
                match self.core.set_profile_production(&profile_id, production) {
                    Ok(()) => {
                        self.profiles = self.core.profiles();
                        self.unlock_tick = None;
                        self.status = if production {
                            format!("{profile_id} 已标记为生产连接 · 写操作需临时解锁")
                        } else {
                            format!("{profile_id} 已取消生产标记")
                        };
                        self.status_kind = if production {
                            StatusKind::Warn
                        } else {
                            StatusKind::Ok
                        };
                    }
                    Err(message) => {
                        self.status = message;
                        self.status_kind = StatusKind::Error;
                    }
                }
                cx.notify();
            }
            MenuAction::UnlockWrites(profile_id, secs) => {
                match self
                    .core
                    .unlock_writes(&profile_id, Duration::from_secs(secs))
                {
                    Ok(()) => {
                        self.unlock_tick = self
                            .core
                            .write_unlock_remaining(&profile_id)
                            .map(|left| left.as_secs());
                        self.status = format!("生产写操作已解锁 {} 分钟 · 到期自动回锁", secs / 60);
                        self.status_kind = StatusKind::Warn;
                    }
                    Err(message) => {
                        self.status = message;
                        self.status_kind = StatusKind::Error;
                    }
                }
                cx.notify();
            }
            MenuAction::LockWrites(profile_id) => {
                self.core.lock_writes(&profile_id);
                self.unlock_tick = None;
                self.status = "生产写操作已回锁".into();
                self.status_kind = StatusKind::Idle;
                cx.notify();
            }
            MenuAction::NewQuery(ns) => {
                self.open_tab(ns, window, cx);
            }
            MenuAction::OpenDatabase(ns) => {
                self.open_tab(ns, window, cx);
            }
            MenuAction::RefreshNamespace(ns) => {
                self.refresh_objects(ns, cx);
            }
            MenuAction::NewObject(ns, object) => {
                let Some(session) = self.session.clone() else {
                    self.status = "请先连接数据源".into();
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                };
                let Some(kind) = self.core.session_kind(&session) else {
                    self.status = "无法识别连接类型，请重新连接".into();
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                };
                let sql = rustdb_core::Core::new_object_template(kind, object);
                self.open_tab(ns, window, cx);
                self.insert_sql(sql, window, cx);
            }
            MenuAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.status = "已复制".into();
                self.status_kind = StatusKind::Ok;
                cx.notify();
            }
            MenuAction::Export(format) => {
                self.export_result(format, cx);
            }
            MenuAction::CopyAs(format) => {
                self.copy_result_as(format, cx);
            }
            MenuAction::OpenData(path) => {
                self.preview_object(path, 100, window, cx);
            }
            MenuAction::DesignTable(path) => {
                self.design_table(path, cx);
            }
            MenuAction::ManagePartitions(path) => {
                self.open_partitions(path, window, cx);
            }
            MenuAction::ShowLineage(path) => {
                self.open_lineage_panel(path, window, cx);
            }
            MenuAction::ShowDdl(path) => {
                self.show_ddl(path, cx);
            }
            MenuAction::EditRoutine(path) => {
                self.open_routine(path, window, cx);
            }
            MenuAction::CountRows(path) => {
                self.count_object(path, window, cx);
            }
            MenuAction::DumpSql(path) => {
                let Some(session) = self.session.clone() else {
                    self.status = "请先连接数据源".into();
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                };
                if let Some(sql) = self.core.dump_sql(&session, &path) {
                    self.open_tab(path.ns.clone(), window, cx);
                    self.insert_sql(sql, window, cx);
                }
            }
            MenuAction::ImportTable(path) => {
                self.start_import(path, window, cx);
            }
            MenuAction::MaintainTable(path) => {
                let Some(session) = self.session.clone() else {
                    self.status = "请先连接数据源".into();
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                };
                let Some(kind) = self.core.session_kind(&session) else {
                    self.status = "无法识别连接类型，请重新连接".into();
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                };
                match rustdb_core::Core::maintenance_sql(kind, &path) {
                    Some(sql) => {
                        self.open_tab(path.ns.clone(), window, cx);
                        self.insert_sql(sql, window, cx);
                    }
                    None => {
                        self.status = "该驱动暂不支持维护命令".into();
                        self.status_kind = StatusKind::Idle;
                        cx.notify();
                    }
                }
            }
            MenuAction::RenameObject(path) => {
                self.ask_rename(path, cx);
            }
            MenuAction::TogglePin(path) => {
                self.toggle_object_pin(&path, cx);
            }
            MenuAction::AppendRows(count) => {
                self.append_rows(count, cx);
            }
            MenuAction::CloneRow => {
                self.clone_selected_row(cx);
            }
            MenuAction::PasteTsv => {
                self.paste_tsv(cx);
            }
            MenuAction::OpenGenerator => {
                self.open_generator(window, cx);
            }
            MenuAction::SetCellDefault => {
                self.set_selected_cell_default(cx);
            }
            MenuAction::RevertRow => {
                self.revert_selected_row(cx);
            }
            MenuAction::Truncate(path) => {
                self.ask_danger(DangerAction::Truncate { path }, cx);
            }
            MenuAction::Drop(path) => {
                self.ask_danger(DangerAction::Drop { path }, cx);
            }
        }
    }

    /// 把 SQL 写进当前标签页（没有标签就先开一个）。
    pub(crate) fn insert_sql(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            if let Some(ns) = self.current_ns.clone() {
                self.open_tab(ns, window, cx);
            } else {
                return;
            }
        }
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        cx.notify();
    }
    pub(crate) fn render_danger_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = current(cx);
        let Some(action) = self.danger.clone() else {
            return div().into_any_element();
        };
        let (title, detail, target) = match &action {
            DangerAction::Truncate { path } => (
                "清空数据",
                "将删除该表的全部行，无法撤销。",
                path.name.to_string(),
            ),
            DangerAction::Drop { path } => (
                "删除对象",
                "将永久删除该对象，无法撤销。",
                path.name.to_string(),
            ),
            DangerAction::Rename { path } => ("重命名对象", "输入新名称", path.name.to_string()),
            DangerAction::BulkUpdate { path } => (
                "批量更新",
                "未加载的行无法生成回滚 SQL。",
                path.name.to_string(),
            ),
        };
        let needs_name = !matches!(action, DangerAction::Truncate { .. });
        let placeholder = match action {
            DangerAction::Rename { .. } => "输入新的对象名",
            _ => "输入对象名以确认",
        };

        if needs_name && self.danger_input.is_none() {
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx).placeholder(placeholder)
            });
            self.danger_input = Some(input);
        }

        let mut card = v_flex()
            .w(px(300.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(hex(t.fg_secondary))
                    .child(SharedString::from(format!("{detail}（{target}）"))),
            );

        if needs_name {
            card = card.child(div().debug_selector(|| "danger-input".to_string()).child(
                gpui_component::input::Input::new(
                    self.danger_input.as_ref().expect("确认输入框已创建"),
                ),
            ));
        }

        let action_for_confirm = action.clone();
        card.child(
            h_flex()
                .gap_2()
                .justify_end()
                .child(
                    div().debug_selector(|| "danger-cancel".to_string()).child(
                        Button::new("danger-cancel-btn")
                            .small()
                            .ghost()
                            .label("取消")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.danger = None;
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    div().debug_selector(|| "danger-ok".to_string()).child(
                        Button::new("danger-ok-btn")
                            .small()
                            .label("确认")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                let typed = view
                                    .danger_input
                                    .as_ref()
                                    .map(|input| input.read(cx).value().to_string())
                                    .unwrap_or_default();
                                if !MainView::danger_accepts(&action_for_confirm, &typed) {
                                    view.status = match &action_for_confirm {
                                        DangerAction::Rename { .. } => {
                                            "新名称不能为空且不能与原名相同".into()
                                        }
                                        _ => "输入的对象名不匹配".into(),
                                    };
                                    view.status_kind = StatusKind::Error;
                                    cx.notify();
                                    return;
                                }
                                view.run_danger(action_for_confirm.clone(), &typed, cx);
                            })),
                    ),
                ),
        )
        .into_any_element()
    }

    /// Esc：关浮层 → 回退当前单元格 → 撤销未填写的新增行。
    pub(crate) fn handle_escape(&mut self, cx: &mut Context<Self>) {
        // F-91：主机密钥确认只能显式接受或拒绝；Esc 等同于拒绝（绝不等于接受）。
        if self.host_key_prompt.is_some() {
            self.resolve_host_key(false, cx);
            return;
        }
        if self.conn_editor.is_some() {
            self.conn_editor = None;
            self.status = "已取消编辑".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        // F-94：模型配置面板（未保存的改动随关闭丢弃）。
        if self.ai_model_panel.is_some() {
            self.ai_model_panel = None;
            cx.notify();
            return;
        }
        if self.optimize.is_some() {
            self.optimize = None;
            cx.notify();
            return;
        }
        if self.generator.is_some() {
            self.generator = None;
            cx.notify();
            return;
        }
        // F-88：血缘面板（只读，直接关闭）。
        if self.lineage.is_some() {
            self.lineage = None;
            cx.notify();
            return;
        }
        // F-86：结构对比面板。确认状态先撤销，再关面板（脚本从未自动执行）。
        if let Some(panel) = self.compare.as_mut() {
            if panel.confirm_execute {
                panel.confirm_execute = false;
            } else {
                self.compare = None;
            }
            cx.notify();
            return;
        }
        // F-78：数据对比面板（同步本身不可取消，关闭面板不打断后台执行）。
        if self.data_compare.is_some() {
            self.data_compare = None;
            cx.notify();
            return;
        }
        // F-75：传输面板（执行本身不可取消，关闭面板不打断后台传输）。
        if self.transfer.is_some() {
            self.transfer = None;
            cx.notify();
            return;
        }
        // F-76：导出面板（导出本身不可取消，关闭面板不打断后台写盘）。
        if self.dump.is_some() {
            self.dump = None;
            cx.notify();
            return;
        }
        // F-87：文档面板（未保存的笔记随关闭丢弃）。
        if self.docs.is_some() {
            self.docs = None;
            cx.notify();
            return;
        }
        // F-73：批量更新面板（预览 / 输入随关闭丢弃）。
        if self.bulk.is_some() {
            self.bulk = None;
            cx.notify();
            return;
        }
        if self.row_menu.is_some() {
            self.row_menu = None;
            cx.notify();
            return;
        }
        // F-82：先关源码相关的浮层，再关源码面板（脏草稿会先弹确认）。
        if self.routine_save_preview.is_some() {
            self.routine_save_preview = None;
            cx.notify();
            return;
        }
        if self.routine_discard {
            self.routine_discard = false;
            cx.notify();
            return;
        }
        if self.routine.is_some() {
            self.routine_close(cx);
            return;
        }
        // F-85：先关预览，再收表单，最后关分区面板。
        if self
            .partition
            .as_ref()
            .is_some_and(|panel| panel.preview.is_some())
        {
            if let Some(panel) = self.partition.as_mut() {
                panel.preview = None;
            }
            cx.notify();
            return;
        }
        if self
            .partition
            .as_ref()
            .is_some_and(|panel| panel.form != PartitionForm::None)
        {
            if let Some(panel) = self.partition.as_mut() {
                panel.form = PartitionForm::None;
            }
            cx.notify();
            return;
        }
        if self.partition.is_some() {
            self.partition = None;
            cx.notify();
            return;
        }
        // F-90：配置交换面板（未落盘的导出 / 导入预览随关闭丢弃）。
        if self.exchange.is_some() {
            self.exchange = None;
            cx.notify();
            return;
        }
        if self.edit_cell.is_some() {
            self.cancel_cell_edit(cx);
            self.status = "已放弃本次编辑".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        self.undo_last_new_row(cx);
    }
}
