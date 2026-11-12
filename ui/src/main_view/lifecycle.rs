//! 生命周期与全局状态：初始化、轮询、标签页、文件、连接与布局切换。

use super::*;

impl MainView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, core: Arc<rustdb_core::Core>) -> Self {
        let layout_path = rustdb_core::layout_path(&rustdb_platform::paths::data_dir());
        Self::with_layout_path(window, cx, core, layout_path)
    }

    /// 测试用：指定布局文件，避免读到本机真实拖拽结果。
    #[cfg(test)]
    pub(crate) fn new_for_test(
        window: &mut Window,
        cx: &mut Context<Self>,
        core: Arc<rustdb_core::Core>,
        layout_path: std::path::PathBuf,
    ) -> Self {
        Self::with_layout_path(window, cx, core, layout_path)
    }

    pub(crate) fn with_layout_path(
        window: &mut Window,
        cx: &mut Context<Self>,
        core: Arc<rustdb_core::Core>,
        layout_path: std::path::PathBuf,
    ) -> Self {
        let completion_catalog: std::rc::Rc<
            std::cell::RefCell<crate::completion::CompletionCatalog>,
        > = Default::default();
        let completion_provider = std::rc::Rc::new(crate::completion::SqlCompletionProvider::new(
            completion_catalog.clone(),
        ));
        let hover_provider = std::rc::Rc::new(crate::diagnostics::SqlHoverProvider::new(
            completion_catalog.clone(),
        ));
        let sql_input = cx.new(|cx| {
            let mut state = EditorState::new(window, cx).language("sql");
            state.lsp_mut().completion_provider = Some(completion_provider.clone());
            state.lsp_mut().hover_provider = Some(hover_provider.clone());
            // 编辑器默认不带内置查找面板，Ctrl+F / Ctrl+H 需要显式打开。
            state.set_searchable(true, cx);
            state
        });

        cx.subscribe_in(
            &sql_input,
            window,
            |this: &mut Self, _state, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.run(window, cx),
                    InputEvent::Change => {
                        // 服务端错误属于上一次执行，编辑后不再指向原文。
                        if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                            tab.sql_error = None;
                        }
                        this.diag_dirty = true;
                        cx.notify();
                    }
                    _ => {}
                }
            },
        )
        .detach();

        let events = core.subscribe();
        let profiles = core.profiles();
        let demo_target = std::env::var("RUSTDB_DEMO").unwrap_or_default();
        let demo_autorun = !demo_target.is_empty();
        if demo_autorun {
            let profile_id = match demo_target.as_str() {
                "pg" | "postgres" => "dev-postgres",
                "mysql" | "mariadb" => "dev-mysql",
                _ => "demo-sqlite",
            };
            core.connect(profile_id);
        }
        let demo_schema = std::env::var("RUSTDB_DEMO_SCHEMA")
            .ok()
            .filter(|s| !s.is_empty());

        // 崩溃恢复：非演示模式下回填上次的标签页 SQL（结果集不保存，重新执行即可）。
        let restored = if demo_autorun {
            None
        } else {
            rustdb_platform::recovery::load().filter(|snapshot| !snapshot.is_empty())
        };

        // 截图/演示模式：预置标签页与展开的库节点；正常模式必须先点连接树的库节点。
        let tabs = if demo_autorun {
            let mut tab = SqlTab::new(1, None);
            tab.sql = DEFAULT_SQL.to_string();
            sql_input.update(cx, |state, cx| {
                state.set_value(DEFAULT_SQL, window, cx);
            });
            vec![tab]
        } else if let Some(snapshot) = restored.clone() {
            let mut out = Vec::new();
            for (index, saved) in snapshot.tabs.iter().enumerate() {
                let mut tab = SqlTab::new(
                    index as u64 + 1,
                    saved
                        .namespace
                        .as_deref()
                        .map(|name| Namespace::new(Some(name), None::<&str>)),
                );
                tab.sql = saved.sql.clone();
                out.push(tab);
            }
            if let Some(tab) = out.get(snapshot.active_tab.min(out.len().saturating_sub(1))) {
                sql_input.update(cx, |state, cx| {
                    state.set_value(tab.sql.clone(), window, cx);
                });
            }
            out
        } else {
            Vec::new()
        };
        let next_tab_id = tabs.len() as u64 + 1;
        // 截图钩子：`RUSTDB_DEMO_MENU=1` 直接展开库下拉框（正常由点击触发）。
        let demo_menu = demo_autorun && std::env::var("RUSTDB_DEMO_MENU").as_deref() == Ok("1");
        let db_menu = demo_menu.then(|| gpui::point(px(505.), px(48.)));
        // 截图钩子：`RUSTDB_DEMO_OBJ_MENU=1` 在对象树加载完后直接展开表菜单。
        let demo_obj_menu =
            demo_autorun && std::env::var("RUSTDB_DEMO_OBJ_MENU").as_deref() == Ok("1");
        // 截图钩子：`RUSTDB_DEMO_PANEL=transfer|data-compare` 在元数据到位后打开浮层。
        let demo_panel = demo_autorun
            .then(|| std::env::var("RUSTDB_DEMO_PANEL").ok())
            .flatten()
            .filter(|value| !value.is_empty());
        let demo_sidebar = demo_autorun
            .then(|| std::env::var("RUSTDB_DEMO_SIDEBAR").ok())
            .flatten()
            .filter(|value| !value.is_empty());

        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            if this
                .update(cx, |view, cx| {
                    if view.poll(cx) {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();

        // F-84：恢复按连接保存的置顶 / 最近 / 分组展开状态。
        let object_state = rustdb_platform::object_state::load().unwrap_or_default();
        let group_state: HashMap<String, bool> = object_state
            .by_profile
            .iter()
            .flat_map(|(profile, state)| {
                state
                    .groups
                    .iter()
                    .map(move |(key, open)| (format!("{profile}|{key}"), *open))
            })
            .collect();

        // F-94：模型配置放在数据目录，密钥只留在内存 / 环境变量。
        let ai_config_path = rustdb_core::settings_path(&rustdb_platform::paths::data_dir());
        let ai_settings = rustdb_core::load_settings(&ai_config_path);
        let agent = build_agent(&ai_settings, &HashMap::new());
        // F-87：文档笔记与 AI 配置同目录，只存本地笔记，不写数据库。
        let doc_notes_path = rustdb_core::doc_notes_path(&rustdb_platform::paths::data_dir());
        let doc_notes = rustdb_core::load_doc_notes(&doc_notes_path);
        // 三列布局与连接配置同目录，只存宽度与折叠开关。
        let layout = rustdb_core::load_layout(&layout_path);
        // F-86：对比配置与快照同目录。
        let compare_config_path =
            rustdb_core::compare_config_path(&rustdb_platform::paths::data_dir());
        let data_compare_config_path =
            rustdb_core::data_compare_config_path(&rustdb_platform::paths::data_dir());

        let mut view = Self {
            core,
            events,
            focus: cx.focus_handle(),
            panel: PanelKind::Connections,
            profiles,
            collapsed_folders: HashSet::new(),
            active_profile: None,
            connecting: None,
            session: None,
            server_label: None,
            capabilities: None,
            namespaces: Vec::new(),
            current_ns: None,
            auto_commit: true,
            tx: None,
            history_filter: String::new(),
            history_source: None,
            history_range: HistoryRange::All,
            history_menu: None,
            history_input: None,
            completion: completion_catalog,
            diag: Default::default(),
            diag_dirty: true,
            page_seq: 0,
            params_panel: None,
            file_root: None,
            file_expanded: HashSet::new(),
            import_pending: None,
            import_panel: None,
            objects: HashMap::new(),
            loading: HashSet::new(),
            object_loading_phase: HashMap::new(),
            query_loading_phase: HashMap::new(),
            expanded_ns: HashSet::new(),
            group_state,
            object_filter: String::new(),
            object_input: None,
            object_state,
            detail_input: None,
            db_menu,
            obj_menu: None,
            ns_menu: None,
            conn_menu: None,
            unlock_tick: None,
            row_menu: None,
            escape_key: None,
            generator: None,
            table_fks: HashMap::new(),
            bulk: None,
            bulk_refresh: false,
            docs: None,
            doc_notes,
            doc_notes_path,
            lineage: None,
            compare: None,
            compare_config_path,
            dump: None,
            transfer: None,
            data_compare: None,
            data_compare_config_path,
            layout,
            layout_path,
            export_menu: None,
            table_panel: None,
            filter_menu: None,
            filter_input: None,
            cell_detail: None,
            edit_cell: None,
            edit_input: None,
            table_edit: None,
            edit_preview: None,
            optimize: None,
            designer: None,
            design_pending: None,
            designer_apply_query: None,
            routine: None,
            routine_save_preview: None,
            routine_discard: false,
            pending_routine_source: None,
            partition: None,
            exchange: None,
            settings_open: false,
            conn_editor: None,
            host_key_prompt: None,
            agent,
            ai_input: None,
            ai_lines: Vec::new(),
            ai_stream: None,
            ai_pending: None,
            ai_running: false,
            ai_context: Vec::new(),
            ai_ctx_pending: None,
            ai_ctx_menu: None,
            ai_block_execs: HashMap::new(),
            ai_settings,
            ai_config_path,
            ai_keys: HashMap::new(),
            ai_model_panel: None,
            ai_session_id: String::new(),
            ai_sessions: rustdb_platform::ai_sessions::load_all(),
            ai_history_open: false,
            ai_rename: None,
            pending_edit: None,
            selected_cell: None,
            grid_selection: None,
            drag_select: None,
            drag_moved: false,
            preview_note: None,
            pending_sql: None,
            pending_ddl_ns: None,
            danger: None,
            danger_input: None,
            col_resize: None,
            editor_resize: None,
            grid_h: 0.,
            grid_viewport: gpui::ScrollHandle::new(),
            tabs,
            active_tab: 0,
            next_tab_id,
            sql_input,
            status: "未连接 · 点击左侧「本地示例」开始".into(),
            status_kind: StatusKind::Idle,
            recovered: restored.is_some(),
            query_count: 0,
            dark: true,
            demo_autorun,
            demo_schema,
            demo_switched: false,
            demo_obj_menu,
            demo_panel,
            demo_sidebar,
        };
        // 主题随布局落盘：启动时按上次选择恢复，避免每次先闪一下深色。
        crate::theme::apply_theme_mode(cx, view.layout.theme);
        view.dark = crate::theme::is_dark(cx);
        if view.recovered {
            view.status = format!(
                "已恢复上次会话 · {} 个标签页（结果需重新执行）",
                view.tabs.len()
            );
            view.status_kind = StatusKind::Idle;
        }
        // 恢复最近一次 AI 会话（F-48a）。
        if let Some(latest) = view.ai_sessions.first().cloned() {
            view.ai_session_id = latest.id.clone();
            view.ai_lines = latest
                .messages
                .iter()
                .map(|message| (message.role.clone(), message.content.clone()))
                .collect();
        }
        // F-71：Esc 先关浮层、再回退当前单元格、最后撤销未填写的新增行。
        // 用全局按键观察者：焦点落在菜单按钮等非输入控件时也能收到 Esc。
        let escape_key = cx.observe_keystrokes(|view, event, window, cx| {
            if event.keystroke.key == "escape" {
                view.handle_escape(cx);
                return;
            }
            // P1-①：分隔条键盘可达。全局观察者与 Esc 同口径，不要求焦点落在分隔条上，
            // 在 SQL 编辑器里也能用；Ctrl+Alt+0 复位到默认高度。
            if event.keystroke.modifiers.control && event.keystroke.modifiers.alt {
                let available = view.available_workspace_height(window);
                match event.keystroke.key.as_str() {
                    "up" => {
                        view.nudge_editor_height(-rustdb_core::EDITOR_RESIZE_STEP, available, cx)
                    }
                    "down" => {
                        view.nudge_editor_height(rustdb_core::EDITOR_RESIZE_STEP, available, cx)
                    }
                    "0" => view.reset_editor_height(available, cx),
                    _ => {}
                }
            }
        });
        view.escape_key = Some(escape_key);
        view
    }

    /// 事件广播滞后后，按会话实时状态校正本地视图。
    ///
    /// 只做「降级到可验证的状态」：Actor 说没有执行中的查询，就把对应标签页
    /// 的 `running` 清掉并在消息面板留痕；事务状态同样以 Actor 为准。
    pub(crate) fn resync_after_lag(&mut self, skipped: u64) -> bool {
        let Some(session) = self.session.clone() else {
            return false;
        };
        let Some(state) = self.core.session_state(&session) else {
            return false;
        };
        let mut changed = false;
        let mut affected = 0usize;
        for tab in &mut self.tabs {
            let Some(query) = tab.running.clone() else {
                continue;
            };
            if state.active_query.as_ref() == Some(&query) {
                continue;
            }
            tab.running = None;
            tab.current_statement = None;
            tab.messages.push(TabMessage {
                level: rustdb_db_api::WarningLevel::Warning,
                text: "事件流落后，已按会话状态重新同步：该查询可能已结束，结果可能不完整，请重新执行确认"
                    .into(),
            });
            affected += 1;
            changed = true;
        }
        if state.tx.is_none() && self.tx.is_some() {
            self.tx = None;
            changed = true;
        } else if state.tx.is_some() && self.tx.is_none() {
            self.tx = Some(TxView {
                started: Instant::now(),
                statements: 0,
                read_only: false,
            });
            changed = true;
        }
        if changed {
            self.status = if affected > 0 {
                format!("事件丢失 {skipped} 条：已重新同步 {affected} 个执行中的标签页")
            } else {
                format!("事件丢失 {skipped} 条：已按会话状态重新同步")
            };
            self.status_kind = StatusKind::Warn;
        }
        changed
    }

    pub(crate) fn poll(&mut self, _cx: &mut Context<Self>) -> bool {
        let mut drained = 0;
        let mut changed = false;
        while drained < 64 {
            match self.events.try_recv() {
                Ok(envelope) => {
                    changed |= self.apply_event(&envelope);
                    drained += 1;
                }
                Err(EventRecvError::Empty) => break,
                Err(EventRecvError::Closed) => break,
                Err(EventRecvError::Lagged(skipped)) => {
                    // 事件流丢包：不能只记日志，否则标签页会永久停在“执行中”。
                    // 以 Actor 的实时状态为准做一次重同步。
                    log::warn!("UI 事件落后，跳过 {skipped} 条");
                    changed |= self.resync_after_lag(skipped);
                }
            }
        }
        // F-89：解锁倒计时每秒刷新一次；到期自动回锁也要让界面知道。
        let tick = self
            .active_profile
            .as_deref()
            .and_then(|id| self.core.write_unlock_remaining(id))
            .map(|left| left.as_secs());
        if tick != self.unlock_tick {
            self.unlock_tick = tick;
            changed = true;
        }
        // P1-②：跨过 400ms 时主动补一帧，否则骨架 / 旧内容降透明度不会自己出现。
        changed |= self.sync_loading_phases();
        changed
    }

    /// 把「正在加载」同步成带 400ms 延迟的加载相位。
    /// 复用既有 80ms 轮询，不新建定时器；只在跨阈值时返回 true 触发重绘。
    pub(crate) fn sync_loading_phases(&mut self) -> bool {
        use crate::design::LoadingPhase;
        let now = Instant::now();

        let object_keys: Vec<String> = self.loading.iter().cloned().collect();
        self.object_loading_phase
            .retain(|key, _| object_keys.contains(key));
        for key in &object_keys {
            let stale = self.objects.contains_key(key);
            self.object_loading_phase
                .entry(key.clone())
                .or_insert_with(|| LoadingPhase::begin_at(now, stale));
        }

        let running: Vec<(u64, bool)> = self
            .tabs
            .iter()
            .filter(|tab| tab.running.is_some())
            .map(|tab| (tab.id, !tab.results.is_empty()))
            .collect();
        self.query_loading_phase
            .retain(|id, _| running.iter().any(|(running_id, _)| running_id == id));
        for (id, stale) in running {
            self.query_loading_phase
                .entry(id)
                .or_insert_with(|| LoadingPhase::begin_at(now, stale));
        }

        let mut changed = false;
        for phase in self
            .object_loading_phase
            .values_mut()
            .chain(self.query_loading_phase.values_mut())
        {
            changed |= phase.reveal_if_due(now);
        }
        if let Some(panel) = self.routine.as_mut() {
            if panel.deps.loading {
                if !panel.deps.phase.is_loading() {
                    panel.deps.phase = LoadingPhase::begin_at(now, panel.deps.list.is_some());
                }
                changed |= panel.deps.phase.reveal_if_due(now);
            } else if panel.deps.phase.is_loading() {
                panel.deps.phase.finish();
            }
        }
        changed
    }

    pub(crate) fn maybe_demo_run(&mut self) {
        if !self.demo_autorun {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        if self
            .active()
            .map(|tab| !tab.results.is_empty())
            .unwrap_or(true)
        {
            return;
        }
        if let Ok(query_id) = self.core.execute(&session, DEFAULT_SQL) {
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.running = Some(query_id);
            }
        }
    }

    pub(crate) fn active(&self) -> Option<&SqlTab> {
        self.tabs.get(self.active_tab)
    }

    /// 事件 → 标签页：`query_id` 是执行时返回的，足够把结果路由回发起它的标签。
    pub(crate) fn tab_for_query(&self, query_id: &QueryId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.running.as_ref() == Some(query_id))
    }

    /// 活动标签页绑定的库节点在树里保持展开，并顺带加载对象。
    pub(crate) fn expand_active_ns(&mut self) {
        if let Some(ns) = self.active().and_then(|tab| tab.ns.clone()) {
            self.expanded_ns.insert(ns_key(&ns));
            self.request_objects(ns);
        }
    }

    /// 把编辑器当前内容写回活动标签页（切页 / 关页 / 执行前调用）。
    pub(crate) fn save_text(&mut self, cx: &App) {
        let text = self.sql_input.read(cx).value().to_string();
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = text;
        }
    }

    /// 重算编辑器诊断：元数据问题 + 最近一次服务端错误。
    ///
    /// 诊断只画波浪线、不阻止执行；每次文本变化后编辑器会自行清空诊断，
    /// 所以这里重建整个 `DiagnosticSet`。
    pub(crate) fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.diag_dirty = false;
        let rope = self.sql_input.read(cx).text().clone();
        let text = rope.to_string();
        let report = if self.tabs.get(self.active_tab).is_none() || text.trim().is_empty() {
            crate::diagnostics::Report::default()
        } else {
            let catalog = self.completion.borrow();
            crate::diagnostics::validate(&text, &catalog)
        };
        let mut diagnostics = Vec::new();
        let to_position = |offset: usize| rope.offset_to_position(offset.min(rope.len()));
        for issue in &report.issues {
            let (start, end) = clamp_span(&text, issue.range.start, issue.range.end);
            if start >= end {
                continue;
            }
            diagnostics.push(lsp_types::Diagnostic {
                range: lsp_types::Range {
                    start: to_position(start),
                    end: to_position(end),
                },
                severity: Some(match issue.severity {
                    crate::diagnostics::Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                    crate::diagnostics::Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                }),
                source: Some("RustDb".into()),
                code: Some(lsp_types::NumberOrString::String(issue.code.into())),
                message: issue.message.clone(),
                ..Default::default()
            });
        }
        if let Some(mark) = self.active().and_then(|tab| tab.sql_error.clone()) {
            let (start, end) = clamp_span(&text, mark.start, mark.end);
            if start < end {
                diagnostics.push(lsp_types::Diagnostic {
                    range: lsp_types::Range {
                        start: to_position(start),
                        end: to_position(end),
                    },
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    source: Some("RustDb".into()),
                    code: Some(lsp_types::NumberOrString::String("server_error".into())),
                    message: mark.message,
                    ..Default::default()
                });
            }
        }
        self.diag = report;
        self.sql_input.update(cx, |state, cx| {
            if let Some(set) = state.diagnostics_mut() {
                set.reset(&rope);
                for diagnostic in diagnostics {
                    set.push(diagnostic);
                }
            }
            cx.notify();
        });
    }

    /// 工具栏诊断徽标：有语义问题或无法全量校验时显示。
    pub(crate) fn diagnostic_badge(&self) -> Option<(String, String)> {
        // 「无法完全校验」只是信息性状态，常驻工具栏太吵；有真实问题时才出现。
        if self.diag.issues.is_empty() {
            return None;
        }
        let label = match (self.diag.issues.len(), self.diag.incomplete) {
            (count, true) => format!("{count} 个问题 · 部分未校验"),
            (count, false) => format!("{count} 个问题"),
        };
        let mut tip: Vec<String> = self
            .diag
            .issues
            .iter()
            .take(3)
            .map(|issue| issue.message.clone())
            .collect();
        if let Some(note) = &self.diag.note {
            tip.push(note.clone());
        }
        Some((label, tip.join("\n")))
    }

    /// F-93：把出错语句连同报错交给「改写」面板，只出建议、不执行。
    pub(crate) fn ai_fix_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mark) = self.active().and_then(|tab| tab.sql_error.clone()) else {
            return;
        };
        let text = self.sql_input.read(cx).value().to_string();
        let start = mark.start.min(text.len());
        let end = mark.end.min(text.len());
        let failing = text.get(start..end).unwrap_or_default().trim().to_string();
        let sql = if failing.is_empty() {
            self.editor_sql(cx).0
        } else {
            failing
        };
        if sql.trim().is_empty() {
            self.set_status("没有可修复的 SQL", StatusKind::Idle, cx);
            return;
        }
        let source = self.session_kind();
        let target = DbKind::ALL
            .into_iter()
            .find(|kind| *kind != source)
            .unwrap_or(DbKind::Postgres);
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder("例如：改成分页查询并返回总数")
        });
        input.update(cx, |state, cx| {
            state.set_value(rustdb_core::fix_instruction(&mark.message), window, cx)
        });
        self.optimize = Some(OptimizePanel {
            kind: OptimizeKind::Rewrite,
            source,
            target,
            instruction: Some(input),
            original: sql,
            running: false,
            error: None,
            result: None,
            accepted: Vec::new(),
        });
        self.set_status("已打开「改写」：审阅修复建议后再应用", StatusKind::Idle, cx);
        cx.notify();
    }

    /// 把光标移到最近一次服务端错误的位置。
    pub(crate) fn jump_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mark) = self.active().and_then(|tab| tab.sql_error.clone()) else {
            return;
        };
        self.jump_editor_to(mark.start, window, cx);
    }

    /// 跳到第一条语义诊断。
    pub(crate) fn jump_to_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(issue) = self.diag.issues.first() else {
            return;
        };
        self.jump_editor_to(issue.range.start, window, cx);
    }

    pub(crate) fn jump_editor_to(
        &mut self,
        offset: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rope = self.sql_input.read(cx).text().clone();
        let position = rope.offset_to_position(offset.min(rope.len()));
        self.sql_input.update(cx, |state, cx| {
            state.set_cursor_position(position, window, cx)
        });
        cx.notify();
    }

    /// 打开 `.sql` 文件：读盘后放进新标签（同一文件已打开则激活原标签）。
    pub(crate) fn open_sql_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from("打开 SQL 文件")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };
            let _ = this.update_in(cx, |view, window, cx| view.open_file_path(path, window, cx));
        })
        .detach();
    }

    pub(crate) fn load_sql_file(
        &mut self,
        path: PathBuf,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let statements = rustdb_sql_lang::split_statements_for(
            &text,
            rustdb_core::params::flavor_for(self.session_kind()),
        )
        .len();
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.file_path.as_ref() == Some(&path))
        {
            self.activate_tab(index, window, cx);
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.sql = text.clone();
            }
        } else {
            self.save_text(cx);
            let id = self.next_tab_id;
            self.next_tab_id += 1;
            let mut tab = SqlTab::new(id, self.current_ns.clone());
            tab.file_path = Some(path.clone());
            tab.sql = text.clone();
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
            self.clear_grid_selection();
            self.params_panel = None;
        }
        self.sql_input
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.diag_dirty = true;
        self.set_status(
            format!("已打开 {} · {statements} 条语句", file_label(&path)),
            StatusKind::Ok,
            cx,
        );
    }

    /// 保存当前编辑器内容：有文件路径直接写回，否则弹「另存为」。
    pub(crate) fn save_sql_file(
        &mut self,
        save_as: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active() else {
            return;
        };
        let tab_id = tab.id;
        let existing = tab.file_path.clone();
        let text = self.sql_input.read(cx).value().to_string();
        self.save_text(cx);

        if save_as || existing.is_none() {
            let dir = existing
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            let suggested = existing
                .as_deref()
                .map(file_label)
                .unwrap_or_else(|| "query.sql".into());
            let prompt = cx.prompt_for_new_path(&dir, Some(&suggested));
            cx.spawn_in(window, async move |this, cx| {
                let Ok(Ok(Some(path))) = prompt.await else {
                    return;
                };
                let writing = path.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { std::fs::write(&writing, text) })
                    .await;
                let _ =
                    this.update_in(cx, |view, _, cx| view.finish_save(tab_id, path, result, cx));
            })
            .detach();
            return;
        }

        let path = existing.expect("checked above");
        let writing = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&writing, text) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| view.finish_save(tab_id, path, result, cx));
        })
        .detach();
    }

    pub(crate) fn finish_save(
        &mut self,
        tab_id: u64,
        path: PathBuf,
        result: std::io::Result<()>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    tab.file_path = Some(path.clone());
                }
                self.set_status(format!("已保存 {}", file_label(&path)), StatusKind::Ok, cx);
            }
            Err(error) => self.set_status(format!("保存失败：{error}"), StatusKind::Error, cx),
        }
    }

    /// 单击库节点：展开 / 收起对象列表（首次展开时懒加载元数据）。
    pub(crate) fn toggle_namespace(&mut self, ns: Namespace, cx: &mut Context<Self>) {
        let key = ns_key(&ns);
        if self.expanded_ns.remove(&key) {
            cx.notify();
            return;
        }
        self.expanded_ns.insert(key);
        self.request_objects(ns);
        cx.notify();
    }

    /// 打开（或聚焦）某个库的 SQL 标签页。**只有库节点双击会调用它。**
    pub(crate) fn open_tab(&mut self, ns: Namespace, window: &mut Window, cx: &mut Context<Self>) {
        let key = ns_key(&ns);
        match self
            .tabs
            .iter()
            .position(|tab| tab.ns_key() == Some(key.clone()))
        {
            Some(index) => self.activate_tab(index, window, cx),
            None => {
                self.save_text(cx);
                let id = self.next_tab_id;
                self.next_tab_id += 1;
                let mut tab = SqlTab::new(id, Some(ns.clone()));
                if self.demo_autorun {
                    tab.sql = DEFAULT_SQL.to_string();
                }
                let sql = tab.sql.clone();
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
                self.params_panel = None;
                if self.demo_autorun {
                    self.sql_input
                        .update(cx, |state, cx| state.set_value(sql, window, cx));
                } else {
                    self.sql_input
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                self.diag_dirty = true;
            }
        }

        self.expanded_ns.insert(key);
        self.request_objects(ns.clone());
        if self
            .current_ns
            .as_ref()
            .map(|current| current.display_label())
            != Some(ns.display_label())
        {
            self.switch_to(ns, cx);
        }
        cx.notify();
    }

    pub(crate) fn activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.save_text(cx);
        self.active_tab = index;
        self.clear_grid_selection();
        self.params_panel = None;
        let sql = self.tabs[index].sql.clone();
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql, window, cx));
        self.diag_dirty = true;
        self.expand_active_ns();
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let was_active = index == self.active_tab;
        if was_active {
            self.save_text(cx);
        }
        self.tabs.remove(index);
        self.clear_grid_selection();
        self.params_panel = None;
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.sql_input
                .update(cx, |state, cx| state.set_value("", window, cx));
        } else {
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
            if was_active {
                let sql = self.tabs[self.active_tab].sql.clone();
                self.sql_input
                    .update(cx, |state, cx| state.set_value(sql, window, cx));
            }
        }
        self.diag_dirty = true;
        cx.notify();
    }

    /// 标签页切库：更新标签绑定 + 会话当前库。
    pub(crate) fn switch_to(&mut self, ns: Namespace, cx: &mut Context<Self>) {
        // 绑定先落地：即使连接已断，标签页也应显示目标库，而不是回滚。
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.ns = Some(ns.clone());
        }
        self.expanded_ns.insert(ns_key(&ns));
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        self.status = format!("正在切换到 {}", ns_label(&ns));
        self.status_kind = StatusKind::Busy;
        self.core.switch_namespace(&session, ns);
        cx.notify();
    }

    pub(crate) fn request_objects(&mut self, ns: Namespace) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.objects.contains_key(&ns_key(&ns)) || self.loading.contains(&ns_key(&ns)) {
            return;
        }
        self.loading.insert(ns_key(&ns));
        self.core.request_objects(&session, ns);
    }

    /// 建一个同库的新标签页（不按命名空间复用）。
    pub(crate) fn open_untitled_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ns) = self.current_ns.clone() else {
            self.set_status("请先选择数据库", StatusKind::Idle, cx);
            return false;
        };
        self.save_text(cx);
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(SqlTab::new(id, Some(ns)));
        self.active_tab = self.tabs.len() - 1;
        self.params_panel = None;
        self.sql_input
            .update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.diag_dirty = true;
        true
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(query_id) = self.active().and_then(|tab| tab.running.clone()) else {
            return;
        };
        if let Err(message) = self.core.cancel(&session, &query_id) {
            self.status = message;
            self.status_kind = StatusKind::Error;
        }
        cx.notify();
    }

    pub(crate) fn connect(&mut self, profile_id: String, cx: &mut Context<Self>) {
        // 一次只允许一个连接尝试：并发点击会让迟到的 Ready 抢占当前会话。
        if self.connecting.is_some() {
            return;
        }
        if self.session.is_some() && self.active_profile.as_deref() == Some(profile_id.as_str()) {
            return;
        }
        // UI 只维护一个活动连接：切换连接前先关掉旧会话，避免后台残留。
        if let Some(old) = self.session.take() {
            self.core.disconnect(&old);
            self.tx = None;
        }
        self.active_profile = None;
        self.connecting = Some(profile_id.clone());
        self.core.connect(&profile_id);
        self.status = "正在连接…".into();
        self.status_kind = StatusKind::Busy;
        cx.notify();
    }

    /// 布局变化只写 `layout.json`；失败留日志，不打断交互。
    pub(crate) fn persist_layout(&self) {
        if let Err(error) = rustdb_core::save_layout(&self.layout_path, &self.layout) {
            log::warn!("布局配置保存失败：{error}");
        }
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.layout.sidebar_collapsed = !self.layout.sidebar_collapsed;
        self.persist_layout();
        cx.notify();
    }

    pub(crate) fn toggle_ai(&mut self, cx: &mut Context<Self>) {
        self.layout.ai_open = !self.layout.ai_open;
        self.persist_layout();
        cx.notify();
    }

    pub(crate) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        crate::theme::apply_theme_mode(
            cx,
            if self.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
        );
        cx.notify();
    }
}
