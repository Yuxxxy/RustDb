//! 执行链路：取编辑器 SQL、参数收集、提交、执行与 DML 预览。

use super::*;

impl MainView {
    /// 当前编辑器要执行的 SQL：有选区取选区，否则取全文。
    pub(crate) fn editor_sql(&self, cx: &App) -> (String, bool) {
        let state = self.sql_input.read(cx);
        let selected = state.selected_text().to_string();
        if selected.trim().is_empty() {
            (state.value().to_string(), false)
        } else {
            (selected, true)
        }
    }

    pub(crate) fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先在「连接」面板连接一个数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        if self.active().is_none() {
            self.status = "点击左侧数据库打开查询页".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        let selection_start = self.sql_input.read(cx).selected_range().start;
        let (sql, has_selection) = self.editor_sql(cx);
        self.preview_note = None;

        // 面板已开且语句没变：这次点击是「执行」，先把值取出来。
        let reuse_panel = self
            .params_panel
            .as_ref()
            .is_some_and(|panel| panel.sql == sql);
        if reuse_panel {
            self.apply_params(cx);
            return;
        }

        let refs = rustdb_core::params::scan(&sql, self.session_kind());
        if !refs.is_empty() {
            self.open_params_panel(sql, refs, has_selection, selection_start, window, cx);
            return;
        }
        self.execute_sql(
            sql.clone(),
            &sql,
            has_selection,
            selection_start,
            &session,
            cx,
        );
    }

    /// 当前连接的驱动类型；未连接时按 SQLite（只影响 `@name` 的默认开关）。
    pub(crate) fn session_kind(&self) -> rustdb_db_api::DbKind {
        self.session
            .as_ref()
            .and_then(|session| self.core.session_kind(session))
            .unwrap_or(rustdb_db_api::DbKind::Sqlite)
    }

    /// 扫描到占位符：打开参数面板，填完再执行。
    pub(crate) fn open_params_panel(
        &mut self,
        sql: String,
        refs: Vec<rustdb_sql_lang::ParamRef>,
        has_selection: bool,
        selection_start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = self.session_kind();
        let mut seen = HashSet::new();
        let mut slots = Vec::new();
        for reference in refs {
            if !seen.insert(reference.name.clone()) {
                continue;
            }
            let input = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder(format!("{} 的值", reference.name))
            });
            cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    // 改了值就重新确认：原始 SQL 的警示不能被旧确认带过。
                    if let Some(panel) = view.params_panel.as_mut() {
                        panel.confirm_raw = false;
                        panel.warning = None;
                    }
                    cx.notify();
                }
            })
            .detach();
            slots.push(ParamSlot {
                name: reference.name,
                style: reference.style,
                input,
                kind: None,
            });
        }
        let count = slots.len();
        self.params_panel = Some(ParamsPanel {
            sql,
            kind,
            selection_start: has_selection.then_some(selection_start),
            slots,
            warning: None,
            confirm_raw: false,
        });
        self.status = format!("填写 {count} 个参数后执行");
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 读出面板里的值；缺失的（空输入且不是 NULL）逐个报告。
    pub(crate) fn collect_param_values(
        &self,
        cx: &App,
    ) -> (BTreeMap<String, ParamValue>, Vec<String>) {
        let mut values = BTreeMap::new();
        let mut missing = Vec::new();
        let Some(panel) = self.params_panel.as_ref() else {
            return (values, missing);
        };
        for slot in &panel.slots {
            let raw = slot.input.read(cx).value().to_string();
            if raw.trim().is_empty() && slot.kind != Some(ParamKind::Null) {
                missing.push(slot.name.clone());
                continue;
            }
            values.insert(slot.name.clone(), param_slot_value(slot, &raw));
        }
        (values, missing)
    }

    /// 参数面板的「执行」：渲染 SQL → 必要时确认原始拼接 → 执行。
    pub(crate) fn apply_params(&mut self, cx: &mut Context<Self>) {
        if self.params_panel.is_none() {
            return;
        }
        let (values, missing) = self.collect_param_values(cx);
        let panel = self.params_panel.take().expect("checked above");
        if !missing.is_empty() {
            self.status = format!(
                "还有 {} 个参数未填写：{}",
                missing.len(),
                missing.join("、")
            );
            self.status_kind = StatusKind::Error;
            let mut panel = panel;
            panel.warning = Some(self.status.clone());
            self.params_panel = Some(panel);
            cx.notify();
            return;
        }
        let rendered = rustdb_core::params::render(&panel.sql, &values, panel.kind);
        if !rendered.raw_params.is_empty() && !panel.confirm_raw {
            let names = rendered.raw_params.join("、");
            let mut panel = panel;
            panel.confirm_raw = true;
            panel.warning = Some(format!(
                "参数 {names} 是原始 SQL，会不加引号直接拼接；确认源可信后再点一次执行"
            ));
            self.params_panel = Some(panel);
            self.status = "原始 SQL 参数需要再点一次执行确认".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status = "请先在「连接」面板连接一个数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        if let Some(warning) = rendered.warnings.first() {
            self.status = warning.clone();
            self.status_kind = StatusKind::Error;
        }
        self.execute_sql(
            rendered.sql,
            &panel.sql,
            panel.selection_start.is_some(),
            panel.selection_start.unwrap_or(0),
            &session,
            cx,
        );
    }

    pub(crate) fn cycle_param_kind(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(panel) = self.params_panel.as_mut() {
            if let Some(slot) = panel.slots.get_mut(index) {
                slot.kind = next_param_kind(slot.kind);
                panel.warning = None;
                panel.confirm_raw = false;
            }
        }
        cx.notify();
    }

    pub(crate) fn close_params_panel(&mut self, cx: &mut Context<Self>) {
        self.params_panel = None;
        self.status = "已取消参数执行".into();
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 在指定标签上提交查询：登记 running / batch，结果事件按 query_id 回填。
    ///
    /// `base` 是 `offset_source` 在编辑器全文里的起点（选区执行时非 0）。
    pub(crate) fn submit_tab_query(
        &mut self,
        index: usize,
        sql: &str,
        offset_source: &str,
        base: usize,
        source: HistorySource,
        session: &SessionId,
    ) -> Result<QueryId, String> {
        let query_id = self
            .core
            .execute_with_source(session, sql, self.auto_commit, source)?;
        let flavor = rustdb_core::params::flavor_for(self.session_kind());
        let originals = rustdb_sql_lang::split_statements_for(offset_source, flavor);
        let statements: Vec<BatchStatement> = rustdb_sql_lang::split_statements_for(sql, flavor)
            .into_iter()
            .enumerate()
            .map(|(position, statement)| BatchStatement {
                // 参数渲染会改变长度：只复用原始语句的起点，避免多语句错位。
                start: originals
                    .get(position)
                    .map(|original| base + original.start)
                    .unwrap_or(base),
                text: statement.text,
            })
            .collect();
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.running = Some(query_id.clone());
            tab.batch = Some((query_id.clone(), statements));
            tab.failed_at = None;
            tab.current_statement = None;
            tab.sql_error = None;
        }
        self.diag_dirty = true;
        Ok(query_id)
    }

    /// 提交执行。`sql` 是实际发给驱动的文本，`offset_source` 用来把语句起点映射回编辑器。
    pub(crate) fn execute_sql(
        &mut self,
        sql: String,
        offset_source: &str,
        has_selection: bool,
        selection_start: usize,
        session: &SessionId,
        cx: &mut Context<Self>,
    ) {
        if has_selection {
            self.status = "正在执行选区…".into();
        }
        let source = if self
            .active()
            .and_then(|tab| tab.file_path.as_ref())
            .is_some()
        {
            HistorySource::File
        } else {
            HistorySource::Manual
        };
        // 语句起点换算到编辑器全文：选区执行时加上选区起点。
        let base = if has_selection { selection_start } else { 0 };
        let index = self.active_tab;
        match self.submit_tab_query(index, &sql, offset_source, base, source, session) {
            Ok(_) => {
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 失败批次里尚未执行的语句数（总数一并返回）；没有可继续的语句时为 `None`。
    pub(crate) fn resumable(&self) -> Option<(usize, usize)> {
        let tab = self.tabs.get(self.active_tab)?;
        let failed_at = tab.failed_at?;
        let (_, statements) = tab.batch.as_ref()?;
        if failed_at + 1 >= statements.len() {
            return None;
        }
        Some((statements.len() - failed_at - 1, statements.len()))
    }

    /// 从失败语句的下一条继续执行；已提交语句不回滚。
    pub(crate) fn resume_remaining(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先在「连接」面板连接一个数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let Some(failed_at) = tab.failed_at else {
            return;
        };
        let Some((_, statements)) = tab.batch.as_ref() else {
            return;
        };
        if failed_at + 1 >= statements.len() {
            return;
        }
        let remaining = statements[failed_at + 1..].to_vec();
        let source = if tab.file_path.is_some() {
            HistorySource::File
        } else {
            HistorySource::Manual
        };
        let sql = remaining
            .iter()
            .map(|statement| statement.text.as_str())
            .collect::<Vec<_>>()
            .join(";\n");
        match self
            .core
            .execute_with_source(&session, &sql, self.auto_commit, source)
        {
            Ok(query_id) => {
                let count = remaining.len();
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.running = Some(query_id.clone());
                    tab.batch = Some((query_id, remaining));
                    tab.failed_at = None;
                    tab.current_statement = None;
                    tab.sql_error = None;
                }
                self.diag_dirty = true;
                self.status = format!("继续执行剩余 {count} 条语句…");
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// DML 执行前预览（F-63）：把 UPDATE / DELETE / INSERT 改写成只读 SELECT 再执行。
    pub(crate) fn preview_dml(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.set_status("请先在「连接」面板连接一个数据源", StatusKind::Error, cx);
            return;
        };
        if self.active().is_none() {
            self.set_status("点击左侧数据库打开查询页", StatusKind::Idle, cx);
            return;
        }
        let (sql, _) = self.editor_sql(cx);
        let outcome = match self.core.preview_dml(&session, &sql) {
            Ok(outcome) => outcome,
            Err(message) => {
                self.set_status(message, StatusKind::Error, cx);
                return;
            }
        };
        let preview = match outcome {
            rustdb_core::preview::PreviewOutcome::Ready(preview) => preview,
            rustdb_core::preview::PreviewOutcome::Unsupported { reason, .. } => {
                self.set_status(
                    format!("无法预览：{reason}（不影响直接执行）"),
                    StatusKind::Idle,
                    cx,
                );
                return;
            }
            rustdb_core::preview::PreviewOutcome::NotDml => {
                self.set_status("当前语句不是 DML，无需预览", StatusKind::Idle, cx);
                return;
            }
        };
        let mut note = format!("预览 · 只读 · {}", preview.target);
        for extra in &preview.notes {
            note.push_str(" · ");
            note.push_str(extra);
        }
        self.preview_note = Some(note);
        match self.core.execute(&session, &preview.sql) {
            Ok(query_id) => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.running = Some(query_id);
                }
                self.set_status("正在生成预览…", StatusKind::Busy, cx);
            }
            Err(message) => {
                self.preview_note = None;
                self.set_status(format!("预览失败：{message}"), StatusKind::Error, cx);
            }
        }
    }
}
