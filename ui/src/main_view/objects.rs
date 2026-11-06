//! 对象操作：变更集、分页、DDL、危险操作与对象状态。

use super::*;

impl MainView {
    /// 提交变更集：单事务，任何一条失败或乐观并发冲突都整体回滚。
    pub(crate) fn apply_changes(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let Some(set) = self
            .active_result()
            .and_then(|result| result.changes.clone())
        else {
            return;
        };
        match self.core.apply_changes(&session, &set) {
            Ok((query_id, plan)) => {
                self.pending_edit = Some(query_id);
                self.edit_preview = None;
                self.edit_cell = None;
                self.edit_input = None;
                self.status = format!("提交中 · {} 条语句（单事务）", plan.len());
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 放弃全部待提交变更（行级/整表回滚）。
    pub(crate) fn revert_changes(&mut self, cx: &mut Context<Self>) {
        let store = self.core.results();
        if let Some(result) = self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            result.changes = None;
            // 行里可能已经被本地改过，回滚要从 store 重新读一份。
            result.rows = store.rows(result.id, 0, MAX_VIEW_ROWS);
            result.loaded = result.rows.len() as u64;
        }
        self.pending_edit = None;
        self.status = "已放弃全部变更".into();
        self.status_kind = StatusKind::Idle;
        cx.notify();
    }

    /// 预览将要执行的 DML（只读，不执行）。
    pub(crate) fn preview_changes(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let Some(kind) = self.core.session_kind(&session) else {
            self.status = "无法识别连接类型，请重新连接".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let text = match self
            .active_result()
            .and_then(|result| result.changes.as_ref())
            .map(|set| set.preview_text(kind))
        {
            Some(Ok(text)) => text,
            Some(Err(message)) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
                cx.notify();
                return;
            }
            None => return,
        };
        self.edit_preview = Some(text);
        cx.notify();
    }

    /// 「打开表」后把结果集标记为可编辑：按表主键列名映射到结果集列下标。
    pub(crate) fn attach_editable(&mut self) {
        let Some(target) = self.table_edit.clone() else {
            return;
        };
        let Some(pk) = target.pk.clone() else {
            return;
        };
        let session = self.session.clone();
        let kind = session.as_ref().and_then(|s| self.core.session_kind(s));
        let Some(result) = self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        else {
            return;
        };
        if result.editable.is_some() {
            return;
        }
        // 主键列必须都在结果集里，否则宁可不给编辑（R5：显式告知）。
        let key: Vec<usize> = pk
            .iter()
            .filter_map(|name| {
                result
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
            })
            .collect();
        if key.len() != pk.len() || key.is_empty() {
            self.status = format!(
                "{} 没有可用的主键列，结果集只读（无主键的表不能安全定位行）",
                target.path.display_label()
            );
            self.status_kind = StatusKind::Idle;
            return;
        }
        result.editable = Some((target.path.clone(), key.clone()));
        let columns: Vec<String> = result
            .columns
            .iter()
            .map(|column| column.name.to_string())
            .collect();
        result.changes = Some(rustdb_core::ChangeSet::new(target.path, columns, key));
        let _ = kind;
    }

    /// F-84：当前连接在对象状态里的键；未连接时归到 `default`（演示/截图也会保存）。
    pub(crate) fn object_profile_key(&self) -> String {
        self.active_profile
            .clone()
            .unwrap_or_else(|| "default".into())
    }

    pub(crate) fn persist_object_state(&self) {
        if let Err(err) = rustdb_platform::object_state::save(&self.object_state) {
            log::warn!("对象状态写入失败：{err}");
        }
    }

    pub(crate) fn record_recent_object(&mut self, path: &ObjectPath) {
        let key = object_key(path);
        let profile = self.object_profile_key();
        self.object_state.profile_mut(&profile).record_recent(&key);
        self.persist_object_state();
    }

    pub(crate) fn toggle_object_pin(&mut self, path: &ObjectPath, cx: &mut Context<Self>) {
        let key = object_key(path);
        let profile = self.object_profile_key();
        self.object_state.profile_mut(&profile).toggle_pin(&key);
        self.persist_object_state();
        cx.notify();
    }

    pub(crate) fn preview_object(
        &mut self,
        path: rustdb_db_api::ObjectPath,
        limit: u64,
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
        self.record_recent_object(&path);
        // F-70：驱动支持服务端分页时，「打开表」按页取数，而不是只拉前 N 行。
        if matches!(path.kind, ObjectKind::Table)
            && self
                .capabilities
                .as_ref()
                .is_some_and(|caps| caps.server_side_filter)
        {
            self.open_object_page(path, 1, DEFAULT_PAGE_SIZE, None, window, cx);
            return;
        }
        let Some(sql) = self.core.object_select_sql(&session, &path, limit) else {
            self.status = "无法生成查询语句".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        self.open_tab(path.ns.clone(), window, cx);
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        // 打开表是唯一可编辑入口：先记住目标，主键列由表结构异步补全。
        self.table_edit = Some(TableEditTarget {
            path: path.clone(),
            pk: None,
        });
        if matches!(path.kind, ObjectKind::Table) {
            self.core.request_table_detail(&session, path);
        }
        self.query_count += 1;
        self.run(window, cx);
    }

    /// 「打开表」的分页入口（F-70）：编辑器里放一份带 LIMIT 的等价 SQL，
    /// 结果区走驱动 `fetch_page`，可翻页且不写入查询历史。
    pub(crate) fn open_object_page(
        &mut self,
        path: rustdb_db_api::ObjectPath,
        page: u64,
        page_size: u32,
        order: Option<(String, bool)>,
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
        self.record_recent_object(&path);
        self.open_tab(path.ns.clone(), window, cx);
        let sql = self
            .core
            .object_select_sql(&session, &path, page_size as u64)
            .unwrap_or_default();
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        self.table_edit = Some(TableEditTarget {
            path: path.clone(),
            pk: None,
        });
        if matches!(path.kind, ObjectKind::Table) {
            self.core.request_table_detail(&session, path.clone());
        }
        let tab_id = self.tabs[self.active_tab].id;
        self.request_page(tab_id, path, page, page_size, order, window, cx);
    }

    /// 重新拉取当前标签的某一页（翻页 / 换页大小 / 服务端排序）。
    pub(crate) fn reload_page(
        &mut self,
        tab_id: u64,
        path: rustdb_db_api::ObjectPath,
        page: u64,
        page_size: u32,
        order: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_page(tab_id, path, page, page_size, order, window, cx);
    }

    pub(crate) fn request_page(
        &mut self,
        tab_id: u64,
        path: rustdb_db_api::ObjectPath,
        page: u64,
        page_size: u32,
        order: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        self.page_seq += 1;
        let seq = self.page_seq;
        let mut req = PageRequest::new(path.clone());
        req.page = page;
        req.page_size = page_size;
        if let Some((column, descending)) = &order {
            req.order_by = vec![if *descending {
                PageOrder::desc(column.clone())
            } else {
                PageOrder::asc(column.clone())
            }];
        }
        let label = path.display_label();
        self.status = format!("正在加载 {label} 第 {page} 页…");
        self.status_kind = StatusKind::Busy;
        cx.notify();
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = core.fetch_page(&session, &req).await;
            let _ = this.update_in(cx, |view, _, cx| {
                view.finish_page(seq, tab_id, path, order, page, page_size, result, cx)
            });
        })
        .detach();
    }

    pub(crate) fn finish_page(
        &mut self,
        seq: u64,
        tab_id: u64,
        path: rustdb_db_api::ObjectPath,
        order: Option<(String, bool)>,
        requested_page: u64,
        requested_size: u32,
        result: Result<PageResult, rustdb_db_api::DriverError>,
        cx: &mut Context<Self>,
    ) {
        // 快速翻页时旧请求可能后到：只认最后一次。
        if seq != self.page_seq {
            return;
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        match result {
            Ok(page) => {
                let row_count = page.rows.len();
                let (set_id, _) = self
                    .core
                    .results()
                    .create(page.columns.clone(), QueryId::new());
                self.core.results().append(set_id, RowBatch::new(page.rows));
                self.core.results().complete(set_id, 0, None);
                // 同一标签只保留当前页，避免翻页时结果标签越积越多。
                if let Some(position) = self.tabs[index]
                    .results
                    .iter()
                    .position(|result| result.page.is_some())
                {
                    let old = self.tabs[index].results.remove(position);
                    self.core.results().remove(old.id);
                }
                refresh_result(
                    &mut self.tabs[index],
                    &self.core.results(),
                    set_id,
                    row_count as u64,
                );
                if let Some(view) = self.tabs[index]
                    .results
                    .iter_mut()
                    .find(|result| result.id == set_id)
                {
                    view.page = Some(TablePage {
                        path: path.clone(),
                        page: page.page,
                        page_size: page.page_size,
                        has_more: page.has_more,
                        order,
                    });
                }
                self.clear_grid_selection();
                self.attach_editable();
                self.status = format!(
                    "{} · 第 {} 页 · {} 行{}",
                    path.display_label(),
                    page.page,
                    row_count,
                    if page.has_more {
                        " · 还有下一页"
                    } else {
                        ""
                    }
                );
                self.status_kind = StatusKind::Ok;
                cx.notify();
            }
            Err(error) => {
                self.status = format!(
                    "{} 第 {requested_page} 页（每页 {requested_size}）加载失败：{}",
                    path.display_label(),
                    rustdb_core::Core::describe_error(&error)
                );
                self.status_kind = StatusKind::Error;
                cx.notify();
            }
        }
    }

    /// 「查看 DDL」：异步取回后写进编辑器（原生 / 合成在状态栏标注）。
    pub(crate) fn show_ddl(&mut self, path: rustdb_db_api::ObjectPath, cx: &mut Context<Self>) {
        self.obj_menu = None;
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        self.status = format!("正在读取 {} 的 DDL…", path.name);
        self.status_kind = StatusKind::Busy;
        self.pending_ddl_ns = Some(path.ns.clone());
        self.core.request_ddl(&session, path);
        cx.notify();
    }

    /// 「统计行数」：生成 `SELECT COUNT(*)`，直接走正常查询链路（结果进网格）。
    pub(crate) fn count_object(
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
        let Some(from) = self.core.object_from_clause(&session, &path) else {
            self.status = "无法解析对象名".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let sql = format!("SELECT COUNT(*) FROM {from};");

        self.open_tab(path.ns.clone(), window, cx);
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        self.query_count += 1;
        self.run(window, cx);
    }

    /// 刷新某命名空间的对象（丢弃缓存后重新拉取）。
    pub(crate) fn refresh_objects(&mut self, ns: Namespace, cx: &mut Context<Self>) {
        self.ns_menu = None;
        let key = ns_key(&ns);
        self.objects.remove(&key);
        self.loading.remove(&key);
        self.expanded_ns.insert(key);
        self.request_objects(ns);
        self.status = "正在刷新对象…".into();
        self.status_kind = StatusKind::Busy;
        cx.notify();
    }

    /// 破坏性动作是否满足确认条件：删除要求输入对象名，重命名要求非空且不同于原名。
    pub(crate) fn danger_accepts(action: &DangerAction, typed: &str) -> bool {
        let typed = typed.trim();
        match action {
            DangerAction::Truncate { .. } => true,
            DangerAction::Drop { path } => typed == path.name.as_ref(),
            DangerAction::Rename { path } => !typed.is_empty() && typed != path.name.as_ref(),
            DangerAction::BulkUpdate { path } => typed == path.name.as_ref(),
        }
    }

    /// 打开破坏性动作的确认框。
    pub(crate) fn ask_danger(&mut self, action: DangerAction, cx: &mut Context<Self>) {
        self.danger_input = None;
        self.danger = Some(action);
        cx.notify();
    }

    /// 「设计表」：异步取列 / 索引 / 外键，回来后弹结构面板。
    pub(crate) fn design_table(&mut self, path: rustdb_db_api::ObjectPath, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        self.status = format!("正在读取 {} 的结构…", path.name);
        self.status_kind = StatusKind::Busy;
        self.design_pending = Some(path.clone());
        self.core.request_table_detail(&session, path);
        cx.notify();
    }

    /// 「重命名」：复用确认框，要求输入新名字。
    pub(crate) fn ask_rename(&mut self, path: rustdb_db_api::ObjectPath, cx: &mut Context<Self>) {
        self.danger = Some(DangerAction::Rename { path });
        self.danger_input = None;
        cx.notify();
    }

    /// 确认后真正下发破坏性 SQL（清空 / 删除 / 重命名）。
    pub(crate) fn run_danger(&mut self, action: DangerAction, typed: &str, cx: &mut Context<Self>) {
        // F-73：批量更新的 SQL 已经在面板里预览过，确认后直接执行那份计划。
        if matches!(action, DangerAction::BulkUpdate { .. }) {
            self.danger = None;
            self.danger_input = None;
            self.bulk_execute(cx);
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let path = match &action {
            DangerAction::Truncate { path }
            | DangerAction::Drop { path }
            | DangerAction::Rename { path }
            | DangerAction::BulkUpdate { path } => path.clone(),
        };
        let Some(from) = self.core.object_from_clause(&session, &path) else {
            return;
        };
        let sql = match &action {
            DangerAction::Truncate { .. } => format!("DELETE FROM {from};"),
            DangerAction::Drop { path } => {
                let keyword = match path.kind {
                    ObjectKind::View => "VIEW",
                    ObjectKind::MaterializedView => "MATERIALIZED VIEW",
                    ObjectKind::Procedure => "PROCEDURE",
                    ObjectKind::Function => "FUNCTION",
                    ObjectKind::Trigger => "TRIGGER",
                    ObjectKind::Sequence => "SEQUENCE",
                    _ => "TABLE",
                };
                format!("DROP {keyword} {from};")
            }
            DangerAction::BulkUpdate { .. } => return,
            DangerAction::Rename { .. } => {
                let Some(kind) = self.core.session_kind(&session) else {
                    self.status = "无法识别连接类型，请重新连接".into();
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                };
                match rustdb_core::Core::rename_sql(kind, &path, typed.trim()) {
                    Some(sql) => sql,
                    None => {
                        self.status = "该驱动暂不支持重命名".into();
                        self.status_kind = StatusKind::Idle;
                        self.danger = None;
                        cx.notify();
                        return;
                    }
                }
            }
        };
        self.danger = None;
        self.danger_input = None;
        match self.core.execute(&session, &sql) {
            Ok(_) => {
                self.status = format!("已提交：{}", sql.trim_end_matches(';'));
                self.status_kind = StatusKind::Busy;
                // 结构类变更后让对象树重新拉一次。
                if !matches!(action, DangerAction::Truncate { .. }) {
                    self.refresh_objects(path.ns.clone(), cx);
                    return;
                }
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }
}
