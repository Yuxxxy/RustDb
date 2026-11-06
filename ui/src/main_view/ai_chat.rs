//! AI 会话：上下文芯片、对话、模型面板与 SQL 代码块。

use super::*;

impl MainView {
    /// F-93：把一段上下文加进芯片区（同种类同标签去重）。
    pub(crate) fn ai_ctx_push(&mut self, item: rustdb_core::ContextItem, cx: &mut Context<Self>) {
        if self
            .ai_context
            .iter()
            .any(|existing| existing.kind == item.kind && existing.label == item.label)
        {
            self.set_status("该上下文已在列表里", StatusKind::Idle, cx);
            return;
        }
        self.ai_context.push(item);
        self.ai_ctx_menu = None;
        cx.notify();
    }

    pub(crate) fn ai_ctx_remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.ai_context.len() {
            self.ai_context.remove(index);
        }
        cx.notify();
    }

    pub(crate) fn ai_ctx_toggle_menu(&mut self, menu: AiCtxMenu, cx: &mut Context<Self>) {
        self.ai_ctx_menu = if self.ai_ctx_menu == Some(menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }

    /// 选区芯片：取编辑器当前选中文本；没有选区不猜整篇。
    pub(crate) fn ai_ctx_add_selection(&mut self, cx: &mut Context<Self>) {
        let selected = self.sql_input.read(cx).selected_text().to_string();
        if selected.trim().is_empty() {
            self.set_status("编辑器里没有选区：先选中一段 SQL", StatusKind::Idle, cx);
            return;
        }
        let label: String = selected
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("选区")
            .trim()
            .chars()
            .take(24)
            .collect();
        let item =
            rustdb_core::ContextItem::new(rustdb_core::ContextKind::Selection, label, selected);
        self.ai_ctx_push(item, cx);
    }

    /// 选表：已有结构面板时直接复用，否则异步取结构后回填。
    pub(crate) fn ai_ctx_pick_table(&mut self, path: ObjectPath, cx: &mut Context<Self>) {
        self.ai_ctx_menu = None;
        let label = path.display_label();
        if let Some((cached, detail)) = self.table_panel.as_ref() {
            if cached == &label {
                let item = rustdb_core::table_item(detail);
                self.ai_ctx_push(item, cx);
                return;
            }
        }
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        self.ai_ctx_pending = Some((label.clone(), path.clone()));
        self.core.request_table_detail(&session, path);
        self.set_status(format!("正在读取表 {label} 的结构…"), StatusKind::Busy, cx);
    }

    pub(crate) fn ai_ctx_pick_saved(
        &mut self,
        entry: rustdb_core::HistoryEntry,
        cx: &mut Context<Self>,
    ) {
        let label: String = entry
            .sql
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("已存 SQL")
            .trim()
            .chars()
            .take(28)
            .collect();
        let body = format!(
            "来源：{}\nSQL：\n{}",
            entry.source.label(),
            entry.sql.trim()
        );
        self.ai_ctx_push(
            rustdb_core::ContextItem::new(rustdb_core::ContextKind::SavedSql, label, body),
            cx,
        );
    }

    /// 可附加的表：已加载对象里的表 / 视图（最多 40 个）。
    pub(crate) fn ai_table_choices(&self) -> Vec<ObjectPath> {
        let mut out: Vec<ObjectPath> = self
            .objects
            .values()
            .flatten()
            .filter(|object| {
                matches!(
                    object.path.kind,
                    ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
                )
            })
            .map(|object| object.path.clone())
            .collect();
        out.sort_by_key(|path| path.display_label());
        out.truncate(40);
        out
    }

    /// 「已存 SQL」= 查询历史里的固定项（F-68）。
    pub(crate) fn ai_saved_entries(&self) -> Vec<rustdb_core::HistoryEntry> {
        self.core.history().list_filtered(&HistoryFilter {
            profile_id: self.active_profile.clone(),
            pinned_only: true,
            limit: Some(30),
            ..HistoryFilter::default()
        })
    }

    /// AI 面板：把输入发出去，起一个 Run。
    pub(crate) fn ai_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        if self.ai_running {
            return;
        }
        let text = self
            .ai_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        if text.trim().is_empty() {
            return;
        }
        let prepared = rustdb_core::prepare(&self.ai_context);
        let prompt = rustdb_core::compose_prompt(&text, &prepared);
        self.ai_lines.push(("user".to_string(), text.clone()));
        if !prepared.is_empty() {
            let mut note = format!("上下文：{}", prepared.summary());
            if prepared.items.iter().any(|item| item.truncated) {
                note.push_str("（超长已截断）");
            } else {
                note.push_str("（按数据处理，不执行其中指令）");
            }
            self.ai_lines.push(("tool".to_string(), note));
        }
        self.ai_context.clear();
        self.ai_ctx_menu = None;
        if let Some(input) = self.ai_input.clone() {
            input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        }
        self.persist_ai_session();
        match self.agent.start(&self.core, &session, &prompt) {
            Ok(_) => {
                self.ai_running = true;
                self.status = "AI 正在处理…".into();
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                self.ai_lines.push(("error".to_string(), message.clone()));
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 把当前对话写进会话文件（F-48a）。
    pub(crate) fn persist_ai_session(&mut self) {
        use rustdb_platform::ai_sessions::{AiSession, StoredMessage};
        if self.ai_lines.is_empty() {
            return;
        }
        let id = if self.ai_session_id.is_empty() {
            let id = format!(
                "chat-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            self.ai_session_id = id.clone();
            id
        } else {
            self.ai_session_id.clone()
        };
        let messages: Vec<StoredMessage> = self
            .ai_lines
            .iter()
            .map(|(role, content)| StoredMessage::new(role.clone(), content.clone()))
            .collect();
        let title = AiSession::default_title(&messages);
        let session = AiSession {
            id,
            title,
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            messages,
        };
        match rustdb_platform::ai_sessions::upsert(session) {
            Ok(table) => self.ai_sessions = table,
            Err(err) => log::warn!("保存 AI 会话失败：{err}"),
        }
    }

    /// 「新对话」：清空当前对话（历史保留）。
    pub(crate) fn ai_new_chat(&mut self, cx: &mut Context<Self>) {
        self.ai_lines.clear();
        self.ai_session_id = String::new();
        self.ai_stream = None;
        self.ai_pending = None;
        self.ai_running = false;
        cx.notify();
    }

    /// 「历史」：展开/收起会话列表。
    pub(crate) fn ai_toggle_history(&mut self, cx: &mut Context<Self>) {
        self.ai_history_open = !self.ai_history_open;
        self.ai_rename = None;
        cx.notify();
    }

    /// F-94：打开 / 收起「模型」面板（默认载入激活配置，其次第一个已存配置或预设）。
    pub(crate) fn ai_models_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ai_model_panel.is_some() {
            self.ai_model_panel = None;
            cx.notify();
            return;
        }
        let profile = self
            .ai_settings
            .active_profile()
            .cloned()
            .or_else(|| self.ai_settings.profiles.first().cloned())
            .or_else(|| rustdb_core::presets().into_iter().next());
        if let Some(profile) = profile {
            self.ai_panel_apply(window, cx, &profile);
        }
    }

    /// 把某个配置（已存 / 预设）载入面板草稿；密钥只从内存取。
    pub(crate) fn ai_panel_apply(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        profile: &rustdb_core::AiProfile,
    ) {
        let editing_id = self
            .ai_settings
            .find(&profile.id)
            .map(|saved| saved.id.clone());
        let key = self.ai_keys.get(&profile.id).cloned().unwrap_or_default();
        self.ai_model_panel = Some(Self::ai_panel_draft(window, cx, profile, editing_id, &key));
        cx.notify();
    }

    pub(crate) fn ai_panel_draft(
        window: &mut Window,
        cx: &mut Context<Self>,
        profile: &rustdb_core::AiProfile,
        editing_id: Option<String>,
        key: &str,
    ) -> AiModelPanel {
        let mut field =
            |value: &str, placeholder: &str| conn_input(window, cx, placeholder, Some(value));
        AiModelPanel {
            editing_id,
            kind: profile.kind,
            auth: profile.auth,
            name: field(&profile.name, "名称"),
            base_url: field(&profile.base_url, "https://api.example.com/v1"),
            model: field(&profile.model, "模型名"),
            key: field(key, "留空则读环境变量；只留在内存"),
            key_ref: field(
                profile.api_key_ref.as_deref().unwrap_or(""),
                "RUSTDB_AI_KEY",
            ),
            auth_header: field(profile.auth_header.as_deref().unwrap_or(""), "api-key"),
            temperature: field(&profile.temperature.to_string(), "0.2"),
            context_window: field(
                &profile
                    .context_window
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                "例如 128000",
            ),
            max_turns: field(&profile.max_turns.to_string(), "8"),
            retries: field(&profile.retries.to_string(), "0"),
            proxy: field(
                profile.proxy.as_deref().unwrap_or(""),
                "http://127.0.0.1:7890",
            ),
            headers: field(
                &profile
                    .headers
                    .iter()
                    .map(|(n, v)| format!("{n}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                "Name=Value，用 ; 或换行分隔",
            ),
            instruction: field(
                profile.global_instruction.as_deref().unwrap_or(""),
                "例如：回答尽量简短，先给结论",
            ),
            cli_command: field(
                profile.cli_command.as_deref().unwrap_or(""),
                "例如 ollama run llama3.1",
            ),
            errors: Vec::new(),
            report: None,
            models: Vec::new(),
            busy: None,
        }
    }

    /// 读取面板输入 → 配置草稿、本次会话密钥与解析错误。
    pub(crate) fn ai_panel_inputs(
        &self,
        cx: &App,
    ) -> Option<(rustdb_core::AiProfile, String, Vec<String>)> {
        let panel = self.ai_model_panel.as_ref()?;
        let text = |input: &gpui::Entity<gpui_component::input::InputState>| {
            input.read(cx).value().trim().to_string()
        };
        let mut errors = Vec::new();
        let context_window = match text(&panel.context_window) {
            value if value.is_empty() => None,
            value => match value.parse::<u32>() {
                Ok(window) => Some(window),
                Err(_) => {
                    errors.push("上下文窗口需要整数".to_string());
                    None
                }
            },
        };
        let max_turns = match text(&panel.max_turns) {
            value if value.is_empty() => rustdb_core::DEFAULT_MAX_TURNS,
            value => value.parse().unwrap_or_else(|_| {
                errors.push("最大轮次需要整数".to_string());
                rustdb_core::DEFAULT_MAX_TURNS
            }),
        };
        let retries = match text(&panel.retries) {
            value if value.is_empty() => 0,
            value => value.parse().unwrap_or_else(|_| {
                errors.push("重试次数需要整数".to_string());
                0
            }),
        };
        let temperature = match text(&panel.temperature) {
            value if value.is_empty() => 0.2,
            value => value.parse().unwrap_or_else(|_| {
                errors.push("temperature 需要数字".to_string());
                0.2
            }),
        };
        let mut headers = Vec::new();
        for line in panel.headers.read(cx).value().split(['\n', ';']) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match line.split_once('=') {
                Some((name, value)) => {
                    headers.push((name.trim().to_string(), value.trim().to_string()))
                }
                None => errors.push(format!("请求头缺少等号：{line}")),
            }
        }
        let name = text(&panel.name);
        let id = panel
            .editing_id
            .clone()
            .unwrap_or_else(|| ai_profile_id(&name));
        let profile = rustdb_core::AiProfile {
            id,
            name,
            kind: panel.kind,
            base_url: text(&panel.base_url),
            model: text(&panel.model),
            auth: panel.auth,
            auth_header: Some(text(&panel.auth_header)).filter(|value| !value.is_empty()),
            api_key_ref: Some(text(&panel.key_ref)).filter(|value| !value.is_empty()),
            headers,
            proxy: Some(text(&panel.proxy)).filter(|value| !value.is_empty()),
            context_window,
            max_turns,
            retries,
            temperature,
            global_instruction: Some(text(&panel.instruction)).filter(|value| !value.is_empty()),
            template_snapshot: None,
            cli_command: Some(text(&panel.cli_command)).filter(|value| !value.is_empty()),
        };
        Some((profile, text(&panel.key), errors))
    }

    pub(crate) fn ai_panel_fail(&mut self, errors: Vec<String>, cx: &mut Context<Self>) {
        if let Some(panel) = self.ai_model_panel.as_mut() {
            panel.busy = None;
            panel.errors = errors;
        }
        cx.notify();
    }

    /// 保存并设为当前配置：写 `ai_models.json`，重建引擎（密钥不落盘）。
    pub(crate) fn ai_panel_save(&mut self, cx: &mut Context<Self>) {
        let Some((mut profile, key, mut errors)) = self.ai_panel_inputs(cx) else {
            return;
        };
        errors.extend(profile.validate());
        if !errors.is_empty() {
            self.ai_panel_fail(errors, cx);
            return;
        }
        profile.capture_snapshot("RustDb SQL 助手");
        let mut settings = self.ai_settings.clone();
        if let Err(more) = settings.upsert(profile.clone()) {
            self.ai_panel_fail(more, cx);
            return;
        }
        settings.active = Some(profile.id.clone());
        if let Err(err) = rustdb_core::save_settings(&self.ai_config_path, &settings) {
            self.ai_panel_fail(vec![format!("保存失败：{err}")], cx);
            return;
        }
        if key.is_empty() {
            self.ai_keys.remove(&profile.id);
        } else {
            self.ai_keys.insert(profile.id.clone(), key);
        }
        self.ai_settings = settings;
        self.agent = build_agent(&self.ai_settings, &self.ai_keys);
        self.ai_model_panel = None;
        self.set_status(
            format!("已启用模型「{}」", profile.name),
            StatusKind::Ok,
            cx,
        );
    }

    /// 连通性测试：后台发一次最小请求（`GET /models` 或一次 CLI 调用）。
    pub(crate) fn ai_panel_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((profile, key, mut errors)) = self.ai_panel_inputs(cx) else {
            return;
        };
        errors.extend(profile.validate());
        if !errors.is_empty() {
            self.ai_panel_fail(errors, cx);
            return;
        }
        if let Some(panel) = self.ai_model_panel.as_mut() {
            panel.busy = Some(AiPanelTask::Test);
            panel.errors.clear();
            panel.models.clear();
            panel.report = None;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    let key = if key.is_empty() {
                        std::env::var(profile.key_env()).unwrap_or_default()
                    } else {
                        key
                    };
                    rustdb_core::probe(&profile, &key)
                })
                .await;
            let _ = this.update_in(cx, |view, _, cx| view.ai_panel_report(report, cx));
        })
        .detach();
    }

    pub(crate) fn ai_panel_report(
        &mut self,
        report: rustdb_core::ConnectivityReport,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.ai_model_panel.as_mut() {
            panel.busy = None;
            panel.report = Some(report);
        }
        cx.notify();
    }

    /// 发现模型：只列聊天模型，点击即填入。
    pub(crate) fn ai_panel_discover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((profile, key, mut errors)) = self.ai_panel_inputs(cx) else {
            return;
        };
        errors.extend(profile.validate());
        if !errors.is_empty() {
            self.ai_panel_fail(errors, cx);
            return;
        }
        if profile.kind == rustdb_core::AiKind::Cli {
            self.ai_panel_fail(vec!["本地 CLI 不支持发现模型".into()], cx);
            return;
        }
        if let Some(panel) = self.ai_model_panel.as_mut() {
            panel.busy = Some(AiPanelTask::Discover);
            panel.errors.clear();
            panel.report = None;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let key = if key.is_empty() {
                        std::env::var(profile.key_env()).unwrap_or_default()
                    } else {
                        key
                    };
                    rustdb_core::discover_models(&profile, &key)
                })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match outcome {
                Ok(models) => {
                    let models = rustdb_core::filter_models(&models, "");
                    let count = models.len();
                    if let Some(panel) = view.ai_model_panel.as_mut() {
                        panel.busy = None;
                        panel.models = models;
                    }
                    view.set_status(format!("发现 {count} 个可用模型"), StatusKind::Ok, cx);
                    cx.notify();
                }
                Err(report) => view.ai_panel_report(report, cx),
            });
        })
        .detach();
    }

    pub(crate) fn ai_panel_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .ai_model_panel
            .as_ref()
            .and_then(|panel| panel.editing_id.clone())
        else {
            self.ai_panel_fail(vec!["当前配置还没保存，直接关闭即可".into()], cx);
            return;
        };
        let mut settings = self.ai_settings.clone();
        settings.remove(&id);
        if let Err(err) = rustdb_core::save_settings(&self.ai_config_path, &settings) {
            self.ai_panel_fail(vec![format!("删除失败：{err}")], cx);
            return;
        }
        self.ai_settings = settings;
        self.ai_keys.remove(&id);
        self.agent = build_agent(&self.ai_settings, &self.ai_keys);
        let next = self
            .ai_settings
            .active_profile()
            .cloned()
            .or_else(|| self.ai_settings.profiles.first().cloned())
            .or_else(|| rustdb_core::presets().into_iter().next());
        match next {
            Some(profile) => self.ai_panel_apply(window, cx, &profile),
            None => self.ai_model_panel = None,
        }
        self.set_status("已删除模型配置", StatusKind::Idle, cx);
    }

    /// 导出 Markdown（配置摘要 + 最近一次连通性结论）；密钥与请求头值不落盘。
    pub(crate) fn ai_panel_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reports = self
            .ai_model_panel
            .as_ref()
            .and_then(|panel| {
                panel
                    .report
                    .clone()
                    .map(|report| vec![(panel.editing_id.clone().unwrap_or_default(), report)])
            })
            .unwrap_or_default();
        let markdown = self.ai_settings.export_markdown(&reports);
        let dir = std::env::current_dir().unwrap_or_default();
        let prompt = cx.prompt_for_new_path(&dir, Some("ai-models.md"));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = prompt.await else {
                return;
            };
            let writing = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&writing, markdown) })
                .await;
            let _ = this.update_in(cx, |view, _, cx| match result {
                Ok(()) => {
                    view.set_status(format!("已导出 {}", file_label(&path)), StatusKind::Ok, cx)
                }
                Err(err) => view.set_status(format!("导出失败：{err}"), StatusKind::Error, cx),
            });
        })
        .detach();
    }

    /// 载入某个历史会话。
    pub(crate) fn ai_load_session(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(session) = self.ai_sessions.iter().find(|item| item.id == id) else {
            return;
        };
        self.ai_session_id = session.id.clone();
        self.ai_lines = session
            .messages
            .iter()
            .map(|message| (message.role.clone(), message.content.clone()))
            .collect();
        self.ai_stream = None;
        self.ai_history_open = false;
        self.ai_rename = None;
        cx.notify();
    }

    /// 删除某个历史会话。
    pub(crate) fn ai_delete_session(&mut self, id: &str, cx: &mut Context<Self>) {
        match rustdb_platform::ai_sessions::remove(id) {
            Ok(table) => {
                self.ai_sessions = table;
                if self.ai_session_id == id {
                    self.ai_session_id = String::new();
                }
            }
            Err(err) => log::warn!("删除 AI 会话失败：{err}"),
        }
        self.ai_rename = None;
        cx.notify();
    }

    /// 开始重命名：弹出输入框。
    pub(crate) fn ai_start_rename(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self
            .ai_sessions
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.title.clone())
            .unwrap_or_default();
        let input =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("会话标题"));
        input.update(cx, |state, cx| state.set_value(title, window, cx));
        self.ai_rename = Some((id.to_string(), input));
        cx.notify();
    }

    /// 确认重命名。
    pub(crate) fn ai_commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, input)) = self.ai_rename.take() else {
            return;
        };
        let title = input.read(cx).value().trim().to_string();
        if title.is_empty() {
            cx.notify();
            return;
        }
        match rustdb_platform::ai_sessions::rename(&id, &title) {
            Ok(table) => self.ai_sessions = table,
            Err(err) => log::warn!("重命名 AI 会话失败：{err}"),
        }
        let _ = window;
        cx.notify();
    }

    /// 确认卡：允许 / 拒绝。
    pub(crate) fn ai_resolve_confirmation(&mut self, approved: bool, cx: &mut Context<Self>) {
        let Some((run_id, _, _, _)) = self.ai_pending.take() else {
            return;
        };
        match self.agent.confirm(&self.core, &run_id, approved) {
            Ok(()) => {
                self.ai_lines.push((
                    "tool".to_string(),
                    if approved {
                        "已允许执行".into()
                    } else {
                        "已拒绝".to_string()
                    },
                ));
                self.status = if approved {
                    "AI 执行中…".into()
                } else {
                    "已拒绝".into()
                };
                self.status_kind = if approved {
                    StatusKind::Busy
                } else {
                    StatusKind::Idle
                };
            }
            Err(message) => {
                self.ai_lines.push(("error".to_string(), message.clone()));
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// F-93：取某条消息里的第 `ordinal` 个 SQL 块。
    pub(crate) fn ai_block_sql(&self, line: usize, ordinal: usize) -> Option<String> {
        let text = self.ai_lines.get(line)?.1.as_str();
        rustdb_core::sql_block_at(text, ordinal)
    }

    /// 「应用到编辑器」：只写文本，不执行。
    pub(crate) fn ai_block_apply(
        &mut self,
        line: usize,
        ordinal: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sql) = self.ai_block_sql(line, ordinal) else {
            return;
        };
        if self.tabs.is_empty() && !self.open_untitled_tab(window, cx) {
            return;
        }
        self.insert_sql(sql, window, cx);
        self.set_status("已应用到编辑器（未执行）", StatusKind::Ok, cx);
    }

    /// 切到该块执行所在的标签（后台标签不抢焦点，需要时再显式查看）。
    pub(crate) fn ai_block_open(
        &mut self,
        line: usize,
        ordinal: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self
            .ai_block_execs
            .get(&(line, ordinal))
            .and_then(|exec| exec.tab_id)
        else {
            return;
        };
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        self.activate_tab(index, window, cx);
    }

    /// F-93：在后台标签执行 SQL 块。
    ///
    /// * 临时运行只放只读语句（写入要走「正式执行」，由连接权限与生产解锁把关）；
    /// * 新标签不切活动页，结果回填后块内显示状态。
    pub(crate) fn ai_block_run(
        &mut self,
        line: usize,
        ordinal: usize,
        temporary: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let Some(sql) = self.ai_block_sql(line, ordinal) else {
            return;
        };
        if sql.trim().is_empty() {
            self.set_status("SQL 块为空", StatusKind::Idle, cx);
            return;
        }
        if temporary {
            let flavor = rustdb_core::params::flavor_for(self.session_kind());
            if rustdb_sql_lang::split_statements_for(&sql, flavor)
                .iter()
                .any(|statement| !rustdb_sql_lang::is_read_only(&statement.text))
            {
                self.ai_block_execs.insert(
                    (line, ordinal),
                    AiBlockExec {
                        tab_id: None,
                        query_id: None,
                        note: Some("临时运行只执行只读语句；写入请用「正式执行」".into()),
                        temporary,
                    },
                );
                self.set_status("临时运行只执行只读语句", StatusKind::Warn, cx);
                return;
            }
        }
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut tab = SqlTab::new(id, self.current_ns.clone());
        tab.sql = sql.clone();
        tab.temporary = temporary;
        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        let mut exec = AiBlockExec {
            tab_id: Some(id),
            query_id: None,
            note: None,
            temporary,
        };
        match self.submit_tab_query(index, &sql, &sql, 0, HistorySource::Ai, &session) {
            Ok(query_id) => {
                exec.query_id = Some(query_id);
                self.set_status(
                    if temporary {
                        "已在后台试跑（只读）；结果见 AI 块状态"
                    } else {
                        "已在后台执行；结果见 AI 块状态"
                    },
                    StatusKind::Busy,
                    cx,
                );
            }
            Err(message) => {
                // 提交就被拒（只读连接 / 生产未解锁 / 连接已断）：不留空标签。
                self.tabs.remove(index);
                exec.tab_id = None;
                exec.note = Some(message.clone());
                self.set_status(message, StatusKind::Error, cx);
            }
        }
        self.ai_block_execs.insert((line, ordinal), exec);
        cx.notify();
    }

    /// 块的执行状态文案：(文本, 颜色)。没有执行记录返回 `None`。
    pub(crate) fn ai_block_status(
        &self,
        line: usize,
        ordinal: usize,
        t: &rustdb_core::theme::ThemeTokens,
    ) -> Option<(String, u32)> {
        let exec = self.ai_block_execs.get(&(line, ordinal))?;
        if let Some(note) = &exec.note {
            return Some((note.clone(), t.danger));
        }
        let tab_id = exec.tab_id?;
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else {
            return Some(("标签已关闭".into(), t.fg_muted));
        };
        if exec
            .query_id
            .as_ref()
            .is_some_and(|query_id| tab.running.as_ref() == Some(query_id))
        {
            return Some((
                if exec.temporary {
                    "试跑中…"
                } else {
                    "执行中…"
                }
                .into(),
                t.warning,
            ));
        }
        if let Some(error) = &tab.sql_error {
            let text = error
                .message
                .lines()
                .next()
                .unwrap_or("执行失败")
                .to_string();
            return Some((format!("失败：{text}"), t.danger));
        }
        let rows: u64 = tab
            .results
            .iter()
            .map(|view| view.affected.unwrap_or(view.loaded))
            .sum();
        let elapsed = tab.results.last().map(|view| view.elapsed_ms).unwrap_or(0);
        Some((
            if rows > 0 || elapsed > 0 {
                format!("完成 · {rows} 行 · {elapsed} ms")
            } else {
                "完成".to_string()
            },
            t.success,
        ))
    }

    /// F-93：把回答里的一个 SQL 块渲染成卡片（内容 + 应用 / 试跑 / 执行 + 状态）。
    pub(crate) fn render_ai_sql_block(
        &self,
        line: usize,
        ordinal: usize,
        sql: &str,
        language: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let status = self.ai_block_status(line, ordinal, &t);
        let can_open = self
            .ai_block_execs
            .get(&(line, ordinal))
            .and_then(|exec| exec.tab_id)
            .and_then(|tab_id| self.tabs.iter().find(|tab| tab.id == tab_id))
            .is_some();
        let line_count = sql.lines().count().max(1);

        let mut header =
            h_flex()
                .h(px(18.))
                .items_center()
                .gap_1()
                .child(div().text_size(px(9.)).text_color(hex(t.fg_muted)).child(
                    SharedString::from(format!("SQL · {language} · {line_count} 行")),
                ))
                .child(div().flex_1());
        if let Some((text, color)) = status.clone() {
            header = header.child(
                div()
                    .debug_selector(|| format!("ai-block-status-{line}-{ordinal}"))
                    .text_size(px(9.))
                    .text_color(hex(color))
                    .text_ellipsis()
                    .max_w(px(120.))
                    .child(SharedString::from(text)),
            );
        }
        if can_open {
            let id = format!("ai-block-open-{line}-{ordinal}");
            header = header.child(
                div().debug_selector(move || id.clone()).child(
                    Button::new(SharedString::from(format!(
                        "ai-block-open-btn-{line}-{ordinal}"
                    )))
                    .small()
                    .ghost()
                    .label("查看")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.ai_block_open(line, ordinal, window, cx);
                    })),
                ),
            );
        }

        let mut actions = h_flex().gap_1().items_center().justify_end();
        let apply_id = format!("ai-block-apply-{line}-{ordinal}");
        actions = actions.child(
            div().debug_selector(move || apply_id.clone()).child(
                Button::new(SharedString::from(format!(
                    "ai-block-apply-btn-{line}-{ordinal}"
                )))
                .small()
                .ghost()
                .label("应用")
                .tooltip("写进当前编辑器，不执行")
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.ai_block_apply(line, ordinal, window, cx);
                })),
            ),
        );
        let temp_id = format!("ai-block-temp-{line}-{ordinal}");
        actions = actions.child(
            div().debug_selector(move || temp_id.clone()).child(
                Button::new(SharedString::from(format!(
                    "ai-block-temp-btn-{line}-{ordinal}"
                )))
                .small()
                .ghost()
                .label("临时运行")
                .tooltip("只读，在后台标签试跑；不抢焦点")
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.ai_block_run(line, ordinal, true, cx);
                })),
            ),
        );
        let run_id = format!("ai-block-run-{line}-{ordinal}");
        actions = actions.child(
            div().debug_selector(move || run_id.clone()).child(
                Button::new(SharedString::from(format!(
                    "ai-block-run-btn-{line}-{ordinal}"
                )))
                .small()
                .label("正式执行")
                .tooltip("在后台标签执行；只读连接 / 生产未解锁会被拒绝")
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.ai_block_run(line, ordinal, false, cx);
                })),
            ),
        );

        v_flex()
            .id(SharedString::from(format!("ai-block-{line}-{ordinal}")))
            .debug_selector(|| format!("ai-block-{line}-{ordinal}"))
            .gap_1()
            .p_2()
            .rounded(px(6.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_overlay))
            .child(header)
            .child(
                v_flex()
                    .max_h(px(150.))
                    .overflow_y_scrollbar()
                    .child(crate::ddl::render_sql(sql, &t)),
            )
            .child(actions)
            .into_any_element()
    }
}
