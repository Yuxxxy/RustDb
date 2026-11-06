//! 事件分派：把 `AppEvent` 落到连接、标签页、结果集与面板状态。
//!
//! 这是 UI 的状态机核心；拆出后便于对事件重放做单元测试。

use super::*;

impl MainView {
    pub(crate) fn apply_event(&mut self, envelope: &rustdb_core::EventEnvelope) -> bool {
        match &envelope.event {
            AppEvent::Conn(ConnEvent::Connecting { profile_id, .. }) => {
                if self.connecting.is_none() {
                    self.connecting = Some(profile_id.clone());
                }
                if self.connecting.as_deref() != Some(profile_id.as_str()) {
                    return false;
                }
                self.active_profile = Some(profile_id.clone());
                self.status = format!("正在连接 {profile_id}…");
                self.status_kind = StatusKind::Busy;
                true
            }
            AppEvent::Conn(ConnEvent::Ready {
                session_id,
                server,
                capabilities,
            }) => {
                let ready_profile = self.core.session_profile_id(session_id);
                if let (Some(expected), Some(actual)) = (&self.connecting, &ready_profile) {
                    if expected != actual {
                        return false;
                    }
                }
                if self.connecting.is_some() && ready_profile.is_none() {
                    return false;
                }
                if let (Some(active), Some(actual)) = (&self.active_profile, &ready_profile) {
                    if self.connecting.is_none() && self.session.is_some() && active != actual {
                        return false;
                    }
                }
                if let Some(profile_id) = ready_profile {
                    self.active_profile = Some(profile_id);
                }
                self.connecting = None;
                self.session = Some(session_id.clone());
                self.server_label = Some(short_server_label(
                    &server.version,
                    server.current_db.as_deref(),
                ));
                self.capabilities = Some(capabilities.clone());
                // 新连接不能复用旧连接的库 / 对象 / 事务状态。
                self.namespaces.clear();
                self.current_ns = None;
                self.objects.clear();
                self.loading.clear();
                self.expanded_ns.clear();
                self.tx = None;
                // 让旧连接上尚未返回的分页请求失效。
                self.page_seq = self.page_seq.wrapping_add(1);
                // 新连接的表结构缓存作废；列信息在打开表后重新缓存。
                self.completion.borrow_mut().columns.clear();
                self.table_fks.clear();
                self.bulk = None;
                self.docs = None;
                self.lineage = None;
                self.compare = None;
                self.dump = None;
                self.transfer = None;
                self.data_compare = None;
                self.diag_dirty = true;
                self.status = format!("已连接 {}", server.version);
                self.status_kind = StatusKind::Ok;
                if self.demo_autorun && self.demo_schema.is_none() {
                    self.maybe_demo_run();
                }
                true
            }
            AppEvent::Conn(ConnEvent::Failed {
                profile_id,
                message,
                ..
            }) => {
                let is_current = self.connecting.as_deref() == Some(profile_id.as_str())
                    || self.active_profile.as_deref() == Some(profile_id.as_str())
                    || self.active_profile.is_none();
                if !is_current {
                    return false;
                }
                self.connecting = None;
                self.active_profile = None;
                self.session = None;
                self.namespaces.clear();
                self.current_ns = None;
                self.objects.clear();
                self.loading.clear();
                self.tx = None;
                self.status = format!("连接失败：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Conn(ConnEvent::Closed { session_id, .. }) => {
                if self.session.as_ref() != Some(session_id) {
                    return false;
                }
                self.connecting = None;
                self.active_profile = None;
                self.session = None;
                self.server_label = None;
                self.namespaces.clear();
                self.current_ns = None;
                self.tx = None;
                self.objects.clear();
                self.loading.clear();
                self.expanded_ns.clear();
                self.table_fks.clear();
                self.bulk = None;
                self.docs = None;
                self.lineage = None;
                self.compare = None;
                self.dump = None;
                self.transfer = None;
                self.data_compare = None;
                {
                    let mut catalog = self.completion.borrow_mut();
                    catalog.objects.clear();
                    catalog.tables.clear();
                    catalog.columns.clear();
                }
                self.diag_dirty = true;
                self.status = "连接已关闭".into();
                self.status_kind = StatusKind::Idle;
                true
            }
            AppEvent::Conn(ConnEvent::Lost { session_id, reason }) => {
                if self.session.as_ref() != Some(session_id) {
                    return false;
                }
                self.session = None;
                self.active_profile = None;
                self.tx = None;
                self.namespaces.clear();
                self.current_ns = None;
                self.objects.clear();
                self.loading.clear();
                self.status = format!("连接断开：{reason}");
                self.status_kind = StatusKind::Error;
                true
            }
            // F-91：隧道建链成功 / 主机密钥待确认。
            AppEvent::Conn(ConnEvent::TunnelReady { profile_id, chain }) => {
                self.status = format!("隧道已建立：{chain}");
                self.status_kind = StatusKind::Ok;
                let _ = profile_id;
                true
            }
            AppEvent::Conn(ConnEvent::TunnelHostKey {
                profile_id,
                layer,
                host,
                port,
                fingerprint,
                public_key,
                changed,
                recorded,
            }) => {
                self.host_key_prompt = Some(HostKeyPrompt {
                    profile_id: profile_id.clone(),
                    layer: *layer,
                    host: host.clone(),
                    port: *port,
                    fingerprint: fingerprint.clone(),
                    public_key: public_key.clone(),
                    changed: *changed,
                    recorded: recorded.clone(),
                });
                self.status = if *changed {
                    format!("第 {layer} 层主机密钥已变更（{fingerprint}），请确认后继续")
                } else {
                    format!("第 {layer} 层主机密钥未知（{fingerprint}），请确认后继续")
                };
                self.status_kind = StatusKind::Warn;
                true
            }
            AppEvent::Query(QueryEvent::Submitted { .. }) => {
                self.query_count += 1;
                self.status = "查询已提交…".into();
                self.status_kind = StatusKind::Busy;
                true
            }
            AppEvent::Query(QueryEvent::Started { query_id, .. }) => {
                // 新一次执行：该标签页的结果集从头开始，旧标签不残留。
                if let Some(index) = self.tab_for_query(query_id) {
                    let tab = &mut self.tabs[index];
                    tab.results.clear();
                    tab.messages.clear();
                    tab.active_result = 0;
                    tab.show_messages = false;
                    tab.sql_error = None;
                    self.clear_grid_selection();
                }
                self.diag_dirty = true;
                self.status = "正在执行…".into();
                self.status_kind = StatusKind::Busy;
                true
            }
            AppEvent::Query(QueryEvent::StatementStarted {
                query_id,
                index,
                total,
            }) => {
                let file = self
                    .tab_for_query(query_id)
                    .and_then(|position| self.tabs[position].file_path.as_deref())
                    .map(file_label);
                if let Some(position) = self.tab_for_query(query_id) {
                    self.tabs[position].current_statement = Some((*index, *total));
                    if *total > 1 {
                        self.status = match file {
                            Some(file) => format!("{file} · 第 {index}/{total} 条"),
                            None => format!("正在执行第 {index}/{total} 条…"),
                        };
                        self.status_kind = StatusKind::Busy;
                    }
                }
                true
            }
            AppEvent::Query(QueryEvent::RowsAppended {
                query_id,
                set_id,
                loaded,
                ..
            }) => {
                if let Some(index) = self.tab_for_query(query_id) {
                    refresh_result(
                        &mut self.tabs[index],
                        &self.core.results(),
                        *set_id,
                        *loaded,
                    );
                }
                true
            }
            AppEvent::Query(QueryEvent::Notice {
                query_id,
                level,
                message,
            }) => {
                if let Some(index) = self.tab_for_query(query_id) {
                    self.tabs[index].messages.push(TabMessage {
                        level: *level,
                        text: message.clone(),
                    });
                }
                true
            }
            AppEvent::Query(QueryEvent::ImportProgress {
                query_id,
                batch,
                total_batches,
                done_rows,
                total_rows,
            }) => {
                if self.tab_for_query(query_id).is_some() {
                    self.status = format!(
                        "导入中 · 第 {batch}/{total_batches} 批 · {done_rows}/{total_rows} 行"
                    );
                    self.status_kind = StatusKind::Busy;
                }
                true
            }
            AppEvent::Query(QueryEvent::ImportFinished {
                query_id,
                elapsed_ms,
                inserted_rows,
                failed_rows,
                failed_batches,
                truncated,
                cancelled,
                ..
            }) => {
                if let Some(index) = self.tab_for_query(query_id) {
                    let tab = &mut self.tabs[index];
                    tab.running = None;
                    tab.batch = None;
                    tab.failed_at = None;
                    tab.current_statement = None;
                    tab.sql_error = None;
                    for failure in failed_batches {
                        tab.messages.push(TabMessage {
                            level: rustdb_db_api::WarningLevel::Warning,
                            text: format!(
                                "第 {} 批（第 {}–{} 行）失败：{}",
                                failure.index, failure.start_row, failure.end_row, failure.message
                            ),
                        });
                    }
                    if !failed_batches.is_empty() {
                        tab.show_messages = true;
                    }
                }
                self.status = if *failed_rows > 0 {
                    format!(
                        "导入完成 · 成功 {inserted_rows} 行 · 失败 {failed_rows} 行（{elapsed_ms} ms）"
                    )
                } else if *cancelled {
                    format!("导入已取消 · 已写入 {inserted_rows} 行")
                } else if *truncated {
                    format!("已清空并导入 {inserted_rows} 行（{elapsed_ms} ms）")
                } else {
                    format!("导入完成 · {inserted_rows} 行（{elapsed_ms} ms）")
                };
                self.status_kind = if *failed_rows > 0 {
                    StatusKind::Error
                } else if *cancelled {
                    StatusKind::Warn
                } else {
                    StatusKind::Ok
                };
                self.diag_dirty = true;
                true
            }
            AppEvent::Query(QueryEvent::Completed {
                query_id,
                elapsed_ms,
                results,
                ..
            }) => {
                // F-73：批量更新的行数预览 / 执行结果在面板里消化，不建结果标签。
                if self.finish_bulk_query(query_id, results) {
                    return true;
                }
                let file = self
                    .tab_for_query(query_id)
                    .and_then(|index| self.tabs[index].file_path.as_deref())
                    .map(file_label);
                let result_count = results.len();
                let total_rows: u64 = results.iter().map(|summary| summary.rows).sum();
                if let Some(index) = self.tab_for_query(query_id) {
                    let tab = &mut self.tabs[index];
                    tab.running = None;
                    tab.batch = None;
                    tab.failed_at = None;
                    tab.current_statement = None;
                    tab.sql_error = None;
                    // DDL/DML 没有行流，只能在这里补出结果标签。
                    for summary in results {
                        ensure_result(tab, &self.core.results(), summary.set_id);
                    }
                    for view in &mut tab.results {
                        if let Some(meta) = self.core.results().meta(view.id) {
                            view.elapsed_ms = meta.elapsed_ms;
                        }
                    }
                    if let Some(last) = results.last() {
                        if let Some(position) = tab.results.iter().position(|r| r.id == last.set_id)
                        {
                            tab.active_result = position;
                        }
                    }
                    // 只有消息、没有结果集（如纯 `RAISE NOTICE`）时直接翻到消息页。
                    tab.show_messages = tab.results.is_empty() && !tab.messages.is_empty();
                }
                self.status = match (file, result_count) {
                    (Some(file), n) if n > 1 => {
                        format!("{file} · {n} 条语句 · {total_rows} 行 · {elapsed_ms} ms")
                    }
                    (Some(file), _) => format!("{file} · 完成 · {elapsed_ms} ms"),
                    (None, n) if n > 1 => format!("{n} 条语句完成 · {elapsed_ms} ms"),
                    (None, _) => format!("执行完成 · {elapsed_ms} ms"),
                };
                self.status_kind = StatusKind::Ok;
                self.diag_dirty = true;
                // 事务内每完成一次执行记一条（工具栏显示「事务 · N 条」）。
                if let Some(tx) = self.tx.as_mut() {
                    tx.statements += results.len().max(1) as u64;
                }
                // 「打开表」的结果集在主键取回后补上编辑能力（另一条顺序是先到主键）。
                self.attach_editable();
                // 变更集提交成功：数据已在库里，清掉待提交状态。
                if self.pending_edit.as_ref() == Some(query_id) {
                    self.pending_edit = None;
                    let mut submitted = 0usize;
                    for tab in &mut self.tabs {
                        for view in &mut tab.results {
                            if let Some(set) = view.changes.take() {
                                submitted += set.len();
                            }
                        }
                    }
                    self.status = format!("已提交 {submitted} 条变更（单事务）");
                    self.status_kind = StatusKind::Ok;
                }
                // 设计器迁移成功：重新拉一次表结构，让基线跟上现状。
                if let Some((pending, fk_op)) = self.designer_apply_query.clone() {
                    if pending == *query_id {
                        self.designer_apply_query = None;
                        if let Some(designer) = self.designer.as_mut() {
                            designer.failure = None;
                        }
                        match fk_op {
                            // 外键逐条保存 / 删除：只改基线里那一条，不动其它草稿。
                            Some(op) => {
                                if let Some(designer) = self.designer.as_mut() {
                                    if op.add {
                                        if let Some(definition) = op.definition {
                                            designer.before_foreign_keys.push(definition);
                                        }
                                    } else {
                                        designer
                                            .before_foreign_keys
                                            .retain(|fk| fk.name != op.name);
                                    }
                                }
                            }
                            // 整份计划执行完：重新拉表结构，让基线跟上现状。
                            None => {
                                if let Some(session) = self.session.clone() {
                                    if let Some(path) =
                                        self.designer.as_ref().map(|d| d.path.clone())
                                    {
                                        self.core.request_table_detail(&session, path);
                                    }
                                }
                            }
                        }
                    }
                }
                // F-82：例程源码保存成功 → 把当前草稿当成新的基线。
                if self
                    .routine
                    .as_ref()
                    .and_then(|panel| panel.saving.clone())
                    .is_some_and(|pending| pending == *query_id)
                {
                    if let Some(panel) = self.routine.as_mut() {
                        panel.saving = None;
                        panel.failure = None;
                        panel.original = panel.draft.clone();
                    }
                    self.status = "例程源码已保存".into();
                    self.status_kind = StatusKind::Ok;
                }
                // F-85：分区 DDL 执行完 → 重新拉一次分区列表。
                if self
                    .partition
                    .as_ref()
                    .and_then(|panel| panel.running.clone())
                    .is_some_and(|pending| pending == *query_id)
                {
                    if let Some(panel) = self.partition.as_mut() {
                        panel.running = None;
                        panel.failure = None;
                        panel.preview = None;
                        panel.form = PartitionForm::None;
                        panel.loading = true;
                    }
                    if let Some(session) = self.session.clone() {
                        if let Some(path) = self.partition.as_ref().map(|panel| panel.path.clone())
                        {
                            self.core.request_partitions(&session, path);
                        }
                    }
                    self.status = "分区操作已完成".into();
                    self.status_kind = StatusKind::Ok;
                }
                true
            }
            AppEvent::Query(QueryEvent::Failed { query_id, error }) => {
                // F-73：批量更新的预览 / 执行失败只更新面板，不落标签页。
                if self.fail_bulk_query(query_id, error) {
                    return true;
                }
                let file = self
                    .tab_for_query(query_id)
                    .and_then(|index| self.tabs[index].file_path.as_deref())
                    .map(file_label);
                let prefix = file.map(|file| format!("{file} · ")).unwrap_or_default();
                let mut progress = None;
                if let Some(index) = self.tab_for_query(query_id) {
                    let tab = &mut self.tabs[index];
                    tab.running = None;
                    tab.sql_error = None;
                    let current_statement = tab.current_statement;
                    let failure = tab
                        .batch
                        .as_ref()
                        .filter(|(batch_query, _)| batch_query == query_id)
                        .and_then(|(_, statements)| {
                            // 没收到逐语句事件时按第一条定位（单条执行）。
                            let failing = current_statement
                                .map(|(current, _)| current.saturating_sub(1))
                                .unwrap_or(0);
                            let statement = statements.get(failing)?;
                            let (start, end) = statement_error_span(statement, error.position);
                            Some((failing, start, end, statements.len()))
                        });
                    if let Some((failing, start, end, count)) = failure {
                        if let Some((current, total)) = current_statement {
                            let current = current.min(count);
                            tab.failed_at = Some(failing);
                            progress = Some((current, total, count.saturating_sub(current)));
                        }
                        tab.sql_error = Some(SqlErrorMark {
                            message: error.message.clone(),
                            start,
                            end,
                        });
                    }
                }
                self.diag_dirty = true;
                self.status = match progress {
                    Some((current, total, remaining)) if remaining > 0 => format!(
                        "{prefix}第 {current}/{total} 条失败：{}（剩余 {remaining} 条）",
                        error.message
                    ),
                    Some((current, total, _)) => {
                        format!("{prefix}第 {current}/{total} 条失败：{}", error.message)
                    }
                    None => format!("{prefix}执行失败：{}", error.message),
                };
                self.status_kind = StatusKind::Error;
                // 设计器下发的迁移失败：把明细与恢复建议摆到面板上。
                if self
                    .designer_apply_query
                    .as_ref()
                    .is_some_and(|(pending, _)| *pending == *query_id)
                {
                    self.designer_apply_query = None;
                    if let Some(designer) = self.designer.as_mut() {
                        designer.failure = Some(DesignerFailure {
                            hint: rustdb_core::recovery_hint(&error.message),
                            message: error.message.clone(),
                        });
                    }
                }
                // F-82：例程源码保存失败 → 草稿留着，只把错误与恢复建议摆出来。
                if self
                    .routine
                    .as_ref()
                    .and_then(|panel| panel.saving.clone())
                    .is_some_and(|pending| pending == *query_id)
                {
                    if let Some(panel) = self.routine.as_mut() {
                        panel.saving = None;
                        panel.failure = Some(DesignerFailure {
                            hint: rustdb_core::recovery_hint(&error.message),
                            message: error.message.clone(),
                        });
                    }
                    self.status = format!("例程源码保存失败：{}", error.message);
                    self.status_kind = StatusKind::Error;
                }
                // F-85：分区 DDL 失败 → 保留面板，把错误与恢复建议摆出来。
                if self
                    .partition
                    .as_ref()
                    .and_then(|panel| panel.running.clone())
                    .is_some_and(|pending| pending == *query_id)
                {
                    if let Some(panel) = self.partition.as_mut() {
                        panel.running = None;
                        panel.failure = Some(DesignerFailure {
                            hint: rustdb_core::recovery_hint(&error.message),
                            message: error.message.clone(),
                        });
                    }
                    self.status = format!("分区操作失败：{}", error.message);
                    self.status_kind = StatusKind::Error;
                }
                true
            }
            AppEvent::Query(QueryEvent::Cancelling { .. }) => {
                self.status = "正在取消…".into();
                self.status_kind = StatusKind::Busy;
                true
            }
            AppEvent::Query(QueryEvent::Cancelled {
                query_id,
                at,
                total,
                ..
            }) => {
                let file = self
                    .tab_for_query(query_id)
                    .and_then(|index| self.tabs[index].file_path.as_deref())
                    .map(file_label);
                let prefix = file.map(|file| format!("{file} · ")).unwrap_or_default();
                let batched = *at > 0 && *total > 1;
                let done = at.saturating_sub(1);
                let remaining = total.saturating_sub(*at);
                let text = self
                    .tab_for_query(query_id)
                    .map(|index| self.tabs[index].sql.clone())
                    .unwrap_or_default();
                let mut unexecuted_lines = Vec::new();
                if let Some(index) = self.tab_for_query(query_id) {
                    let tab = &mut self.tabs[index];
                    if batched {
                        if let Some((_, statements)) = tab
                            .batch
                            .as_ref()
                            .filter(|(batch_query, _)| batch_query == query_id)
                        {
                            let lines = rustdb_sql_lang::position::LineIndex::new(&text);
                            for statement in statements.iter().skip(*at) {
                                unexecuted_lines.push(lines.line_col(&text, statement.start).0);
                            }
                        }
                    }
                    tab.running = None;
                    tab.batch = None;
                    tab.failed_at = None;
                    tab.current_statement = None;
                    tab.sql_error = None;
                    if remaining > 0 {
                        let line_hint = if unexecuted_lines.is_empty() {
                            String::new()
                        } else {
                            let shown = unexecuted_lines
                                .iter()
                                .take(8)
                                .map(usize::to_string)
                                .collect::<Vec<_>>()
                                .join("、");
                            format!("（第 {shown} 行）")
                        };
                        tab.messages.push(TabMessage {
                            level: rustdb_db_api::WarningLevel::Warning,
                            text: format!(
                                "已取消：第 {at}/{total} 条中断，第 {}–{total} 条未执行{line_hint}",
                                at + 1
                            ),
                        });
                        tab.show_messages = tab.results.is_empty() && !tab.messages.is_empty();
                    }
                }
                self.diag_dirty = true;
                self.status = if batched && remaining > 0 {
                    format!("{prefix}已取消 · 成功 {done} 条 · 中断 1 条 · 未执行 {remaining} 条")
                } else if batched {
                    format!("{prefix}已取消 · 成功 {done}/{total} 条")
                } else {
                    format!("{prefix}查询已取消")
                };
                self.status_kind = StatusKind::Idle;
                true
            }
            AppEvent::Tx(TxEvent::Began { read_only, .. }) => {
                self.tx = Some(TxView {
                    started: Instant::now(),
                    statements: 0,
                    read_only: *read_only,
                });
                self.status = "事务已开始".into();
                self.status_kind = StatusKind::Ok;
                true
            }
            AppEvent::Tx(TxEvent::Committed { .. }) => {
                self.tx = None;
                self.status = "事务已提交".into();
                self.status_kind = StatusKind::Ok;
                true
            }
            AppEvent::Tx(TxEvent::RolledBack { .. }) => {
                self.tx = None;
                self.status = "事务已回滚".into();
                self.status_kind = StatusKind::Idle;
                true
            }
            AppEvent::Tx(TxEvent::Failed { action, message }) => {
                self.status = format!("{action}失败：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Meta(MetaEvent::NamespacesLoaded {
                namespaces,
                current,
            }) => {
                self.namespaces = namespaces.clone();
                self.current_ns = Some(current.clone());
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    if tab.ns.is_none() {
                        tab.ns = Some(current.clone());
                    }
                }
                self.expand_active_ns();
                if self.demo_autorun && !self.demo_switched {
                    if let (Some(target), Some(session)) =
                        (self.demo_schema.clone(), self.session.clone())
                    {
                        if let Some(ns) = self
                            .namespaces
                            .iter()
                            .find(|n| n.schema.as_deref() == Some(target.as_str()))
                            .cloned()
                        {
                            self.demo_switched = true;
                            // 演示切库：标签页同步绑定，旧库节点收起，避免标题停在旧库。
                            self.expanded_ns.remove(&current.display_label());
                            self.expanded_ns.insert(ns_key(&ns));
                            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                tab.ns = Some(ns.clone());
                            }
                            self.core.switch_namespace(&session, ns);
                        }
                    }
                }
                true
            }
            AppEvent::Conn(ConnEvent::NamespaceChanged { namespace, .. }) => {
                self.current_ns = Some(namespace.clone());
                self.status = format!("已切换到 {}", namespace.display_label());
                self.status_kind = StatusKind::Ok;
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    if tab.ns.is_none() {
                        tab.ns = Some(namespace.clone());
                    }
                }
                self.expand_active_ns();
                if self.demo_autorun {
                    self.maybe_demo_run();
                }
                true
            }
            AppEvent::Conn(ConnEvent::NamespaceSwitchFailed { message, .. }) => {
                self.status = format!("切换数据库失败：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Meta(MetaEvent::LoadStarted { namespace }) => {
                self.loading.insert(namespace.clone());
                true
            }
            AppEvent::Meta(MetaEvent::Loaded { namespace, objects }) => {
                self.loading.remove(namespace);
                self.objects.insert(namespace.clone(), objects.clone());
                // 补全候选跟随当前库刷新（切库后重新收集）。
                {
                    let mut catalog = self.completion.borrow_mut();
                    catalog.objects = objects
                        .iter()
                        .filter(|object| {
                            matches!(
                                object.path.kind,
                                ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
                            )
                        })
                        .map(|object| object.path.name.to_string())
                        .collect();
                    catalog.tables = objects
                        .iter()
                        .filter(|object| {
                            matches!(
                                object.path.kind,
                                ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
                            )
                        })
                        .map(|object| crate::completion::TableHint {
                            name: object.path.name.to_string(),
                            kind: object.path.kind,
                            schema: object.path.ns.schema.as_ref().map(|s| s.to_string()),
                            row_estimate: object.row_estimate,
                            comment: object.comment.clone(),
                        })
                        .collect();
                    catalog.schemas = if let Some(ns) = self
                        .active()
                        .and_then(|tab| tab.ns.clone())
                        .filter(|ns| ns.display_label() == *namespace)
                    {
                        ns.schema
                            .map(|schema| vec![schema.to_string()])
                            .unwrap_or_default()
                    } else {
                        catalog.schemas.clone()
                    };
                }
                self.diag_dirty = true;
                self.status = format!("{} · 已加载 {} 个对象", namespace, objects.len());
                self.status_kind = StatusKind::Ok;
                // 截图钩子：挑第一张表把右键菜单摆出来。
                if self.demo_obj_menu && self.obj_menu.is_none() {
                    if let Some(object) = objects
                        .iter()
                        .find(|object| object.path.kind == ObjectKind::Table)
                    {
                        self.obj_menu = Some(ObjectMenu {
                            path: object.path.clone(),
                            at: gpui::point(px(150.), px(300.)),
                        });
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::LoadFailed { namespace, message }) => {
                self.loading.remove(namespace);
                self.status = format!("元数据加载失败：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Meta(MetaEvent::DdlLoaded { path, ddl }) => {
                // 事件里拿不到 window，先存起来，下一帧渲染时写进编辑器。
                self.pending_sql = Some((self.pending_ddl_ns.take(), ddl.text.clone()));
                self.status = format!("{} · DDL（{}）", path, ddl.source.label());
                self.status_kind = StatusKind::Ok;
                true
            }
            AppEvent::Meta(MetaEvent::DdlFailed { path, message }) => {
                self.status = format!("{path}：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Meta(MetaEvent::RoutineSourceLoaded { path, text, source }) => {
                // 事件里拿不到 window，源码先存着，下一帧渲染时写进编辑器。
                if let Some(panel) = self.routine.as_mut() {
                    panel.original = text.clone();
                    panel.loading = false;
                    panel.failure = None;
                    self.pending_routine_source = Some(text.clone());
                }
                self.status = format!("{path} · 源码（{}）", source.label());
                self.status_kind = StatusKind::Ok;
                true
            }
            AppEvent::Meta(MetaEvent::RoutineSourceFailed { path, message }) => {
                if let Some(panel) = self.routine.as_mut() {
                    panel.loading = false;
                    panel.failure = Some(DesignerFailure {
                        hint: rustdb_core::recovery_hint(&message),
                        message: message.clone(),
                    });
                }
                self.status = format!("{path}：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Meta(MetaEvent::DependenciesLoaded { path, dependencies }) => {
                if let Some(panel) = self.routine.as_mut() {
                    if panel.path.display_label() == path.as_str() {
                        panel.deps.loading = false;
                        panel.deps.error = None;
                        panel.deps.list = Some(dependencies.clone());
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::DependenciesFailed { path, message }) => {
                if let Some(panel) = self.routine.as_mut() {
                    if panel.path.display_label() == path.as_str() {
                        panel.deps.loading = false;
                        panel.deps.error = Some(message.clone());
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::PartitionsLoaded { path, partitions }) => {
                let accepts_default = partitions
                    .strategy
                    .as_deref()
                    .and_then(rustdb_core::PartitionStrategy::parse)
                    .is_some_and(rustdb_core::PartitionStrategy::accepts_default);
                if let Some(panel) = self.partition.as_mut() {
                    if panel.path.display_label() == path.as_str() {
                        panel.loading = false;
                        panel.error = None;
                        panel.list = Some(partitions.clone());
                        if !accepts_default {
                            panel.is_default = false;
                        }
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::PartitionsFailed { path, message }) => {
                if let Some(panel) = self.partition.as_mut() {
                    if panel.path.display_label() == path.as_str() {
                        panel.loading = false;
                        panel.error = Some(message.clone());
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::LineageLoaded { namespace, input }) => {
                let mut status = None;
                if let Some(panel) = self.lineage.as_mut() {
                    if panel.namespace.display_label() == *namespace {
                        panel.loading = false;
                        panel.error = None;
                        panel.input = Some(input.clone());
                        panel.columns = input
                            .tables
                            .iter()
                            .find(|item| item.path.display_label() == panel.table.display_label())
                            .map(|item| item.columns.clone())
                            .unwrap_or_default();
                        let keep = panel.selected.as_ref().is_some_and(|column| {
                            panel
                                .columns
                                .iter()
                                .any(|item| item.eq_ignore_ascii_case(column))
                        });
                        if !keep {
                            panel.selected = panel.columns.first().cloned();
                        }
                        if panel.columns.is_empty() {
                            panel.error = Some("读不到该表的字段（表结构可能不可见）".into());
                        }
                        status = Some(format!(
                            "血缘快照 {} 张表 / {} 个视图 / {} 条历史 SQL",
                            input.tables.len(),
                            input.views.len(),
                            input.queries.len()
                        ));
                    }
                }
                if let Some(status) = status {
                    self.refresh_lineage_report();
                    self.status = status;
                    self.status_kind = StatusKind::Ok;
                }
                true
            }
            AppEvent::Meta(MetaEvent::LineageFailed { namespace, message }) => {
                if let Some(panel) = self.lineage.as_mut() {
                    if panel.namespace.display_label() == *namespace {
                        panel.loading = false;
                        panel.error = Some(message.clone());
                        self.status = format!("血缘汇总失败：{message}");
                        self.status_kind = StatusKind::Error;
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::SchemaSnapshotLoaded { slot, snapshot }) => {
                let index = match slot.as_str() {
                    "source" => 0,
                    "target" => 1,
                    _ => return false,
                };
                if let Some(panel) = self.compare.as_mut() {
                    panel.loading[index] = false;
                    panel.error[index] = None;
                    if index == 0 {
                        panel.source = Some(snapshot.clone());
                    } else {
                        panel.target = Some(snapshot.clone());
                    }
                    panel.dirty = true;
                    let label = if index == 0 { "源" } else { "目标" };
                    self.status = if snapshot.failed.is_empty() {
                        format!("{label}快照：{} 张表", snapshot.tables.len())
                    } else {
                        format!(
                            "{label}快照：{} 张表，{} 张取详情失败",
                            snapshot.tables.len(),
                            snapshot.failed.len()
                        )
                    };
                    self.status_kind = if snapshot.failed.is_empty() {
                        StatusKind::Ok
                    } else {
                        StatusKind::Idle
                    };
                }
                true
            }
            AppEvent::Meta(MetaEvent::SchemaSnapshotFailed { slot, message }) => {
                let index = match slot.as_str() {
                    "source" => 0,
                    "target" => 1,
                    _ => return false,
                };
                if let Some(panel) = self.compare.as_mut() {
                    panel.loading[index] = false;
                    panel.error[index] = Some(message.clone());
                    panel.dirty = true;
                }
                self.status = format!("结构快照失败：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            AppEvent::Meta(MetaEvent::DocumentLoaded { namespace, input }) => {
                if let Some(panel) = self.docs.as_mut() {
                    if panel.namespace.display_label() == *namespace {
                        panel.loading = false;
                        panel.error = None;
                        panel.input = Some(input.clone());
                        panel.selected_table = None;
                        panel.selected_column = None;
                        panel.preview = Some(rustdb_core::generate_document(input, &panel.notes));
                        self.status = format!("文档已汇总 {} 张表", input.tables.len());
                        self.status_kind = StatusKind::Ok;
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::DocumentFailed { namespace, message }) => {
                if let Some(panel) = self.docs.as_mut() {
                    if panel.namespace.display_label() == *namespace {
                        panel.loading = false;
                        panel.error = Some(message.clone());
                        self.status = format!("文档汇总失败：{message}");
                        self.status_kind = StatusKind::Error;
                    }
                }
                true
            }
            AppEvent::Meta(MetaEvent::TableDetailLoaded { path, detail }) => {
                // F-73：外键跳转依赖这份缓存（键 = 展示名）。
                self.table_fks
                    .insert(path.clone(), detail.foreign_keys.clone());
                // F-93：AI 上下文的表结构回填；不打开结构面板。
                if self
                    .ai_ctx_pending
                    .as_ref()
                    .is_some_and(|(label, _)| label == path)
                {
                    self.ai_ctx_pending = None;
                    let item = rustdb_core::table_item(&detail);
                    let label = item.label.clone();
                    self.ai_context.push(item);
                    self.status = format!("已附加表 {label}");
                    self.status_kind = StatusKind::Ok;
                    return true;
                }
                // 导入入口先取表结构：拿到列就弹面板，不再走设计器 / 表结构面板。
                if self
                    .import_pending
                    .as_ref()
                    .is_some_and(|pending| pending.path.display_label() == *path)
                {
                    if let Some(pending) = self.import_pending.take() {
                        self.open_import_panel(pending, detail.columns.clone());
                        return true;
                    }
                }
                // 表结构也喂给语义诊断与悬停（按表名缓存）。
                {
                    let name = detail.summary.path.name.to_string();
                    let columns: Vec<crate::completion::ColumnHint> = detail
                        .columns
                        .iter()
                        .map(|column| crate::completion::ColumnHint {
                            name: column.meta.name.to_string(),
                            type_label: if column.meta.native_type.is_empty() {
                                column.meta.logical_type.label().to_string()
                            } else {
                                column.meta.native_type.clone()
                            },
                            nullable: column.meta.nullable,
                            primary_key: column.is_primary_key,
                            comment: column.comment.clone(),
                        })
                        .collect();
                    let mut catalog = self.completion.borrow_mut();
                    catalog.columns.insert(name.to_lowercase(), columns);
                    if let Some(hint) = catalog
                        .tables
                        .iter_mut()
                        .find(|hint| hint.name.eq_ignore_ascii_case(&name))
                    {
                        hint.row_estimate = detail.summary.row_estimate.or(hint.row_estimate);
                        hint.comment = detail.summary.comment.clone().or(hint.comment.take());
                    }
                }
                self.diag_dirty = true;
                // 「打开表」顺手取的主键：填进编辑目标，不弹设计面板。
                if let Some(target) = self.table_edit.as_mut() {
                    if target.pk.is_none() && target.path.display_label() == *path {
                        target.pk = Some(
                            detail
                                .columns
                                .iter()
                                .filter(|column| column.is_primary_key)
                                .map(|column| column.meta.name.to_string())
                                .collect(),
                        );
                        self.attach_editable();
                        return true;
                    }
                }
                self.table_panel = Some((path.clone(), detail.clone()));
                // 设计器：以当前列为草稿基准（后续 diff 出 MigrationPlan）。
                if let Some(object_path) = self.design_pending.clone() {
                    let mut draft = rustdb_core::DesignerDraft::default();
                    draft.columns = detail
                        .columns
                        .iter()
                        .map(|column| {
                            let mut column_draft = rustdb_core::ColumnDraft::new(
                                column.meta.name.to_string(),
                                column.meta.native_type.clone(),
                            );
                            column_draft.nullable = column.meta.nullable;
                            column_draft
                        })
                        .collect();
                    draft.indexes = detail
                        .indexes
                        .iter()
                        .filter(|index| !index.primary)
                        .map(|index| rustdb_core::IndexDraft {
                            name: index.name.clone(),
                            columns: index.columns.clone(),
                            unique: index.unique,
                            method: index.method.clone(),
                        })
                        .collect();
                    draft.foreign_keys = detail
                        .foreign_keys
                        .iter()
                        .map(|fk| rustdb_core::ForeignKeyDraft {
                            name: fk.name.clone(),
                            columns: fk.columns.clone(),
                            referenced_table: fk.referenced_table.clone(),
                            referenced_columns: fk.referenced_columns.clone(),
                            on_delete: fk.on_delete.clone(),
                            on_update: fk.on_update.clone(),
                        })
                        .collect();
                    draft.checks = detail
                        .checks
                        .iter()
                        .map(|check| {
                            rustdb_core::CheckDraft::new(
                                check.name.clone(),
                                check.expression.clone(),
                            )
                        })
                        .collect();
                    draft.options = rustdb_core::TableOptionsDraft {
                        engine: detail.engine.clone(),
                        collation: detail.collation.clone(),
                        comment: detail.comment.clone(),
                    };
                    self.designer = Some(DesignerState {
                        path: object_path,
                        before: detail.columns.clone(),
                        before_indexes: detail.indexes.clone(),
                        before_foreign_keys: detail.foreign_keys.clone(),
                        before_checks: detail.checks.clone(),
                        triggers: detail.triggers.clone(),
                        draft,
                        tab: rustdb_core::DesignerTab::Columns,
                        issues: Vec::new(),
                        failure: None,
                        inputs: DesignerInputs::default(),
                    });
                }
                self.status = format!(
                    "{} · {} 列 / {} 索引",
                    path,
                    detail.columns.len(),
                    detail.indexes.len()
                );
                self.status_kind = StatusKind::Ok;
                true
            }
            AppEvent::Meta(MetaEvent::TableDetailFailed { path, message }) => {
                // F-93：结构取不到时仍附加一条最小上下文，不让芯片悬空。
                if self
                    .ai_ctx_pending
                    .as_ref()
                    .is_some_and(|(label, _)| label.as_str() == path)
                {
                    self.ai_ctx_pending = None;
                    self.ai_context.push(rustdb_core::ContextItem::new(
                        rustdb_core::ContextKind::Table,
                        path.clone(),
                        format!("表：{path}\n（未能读取列结构：{message}）"),
                    ));
                    self.status = format!("表 {path} 结构读取失败：{message}");
                    self.status_kind = StatusKind::Error;
                    return true;
                }
                self.import_pending = None;
                self.status = format!("{path}：{message}");
                self.status_kind = StatusKind::Error;
                true
            }
            // F-76：导出进度只更新面板，不写历史 / 标签页。
            AppEvent::Dump(event) => {
                let Some(panel) = self.dump.as_mut() else {
                    return false;
                };
                match event {
                    DumpEvent::Started {
                        namespace,
                        tables,
                        path,
                        gzip,
                    } => {
                        panel.running = true;
                        panel.total = *tables;
                        panel.done = 0;
                        panel.rows = 0;
                        panel.objects = 0;
                        panel.failed = 0;
                        panel.current = None;
                        panel.path = Some(PathBuf::from(path));
                        panel.bytes = 0;
                        panel.error = None;
                        self.status = format!(
                            "正在导出 {namespace} · {tables} 张表{}",
                            if *gzip { "（gzip）" } else { "" }
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    DumpEvent::TableDone { table, rows, error } => {
                        panel.done += 1;
                        panel.rows += *rows;
                        panel.current = Some(table.clone());
                        if let Some(error) = error {
                            panel.failed += 1;
                            panel.error = Some(format!("{table}：{error}"));
                        }
                        self.status = format!(
                            "导出中 · {}/{} 表 · {} 行",
                            panel.done, panel.total, panel.rows
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    DumpEvent::ObjectDone { object, error } => {
                        panel.objects += 1;
                        if let Some(error) = error {
                            panel.failed += 1;
                            panel.error = Some(format!("{object}：{error}"));
                        }
                    }
                    DumpEvent::Finished {
                        path,
                        tables,
                        objects,
                        rows,
                        failed,
                        bytes,
                    } => {
                        panel.running = false;
                        panel.done = *tables;
                        panel.rows = *rows;
                        panel.objects = *objects;
                        panel.failed = *failed;
                        panel.current = None;
                        panel.path = Some(PathBuf::from(path));
                        panel.bytes = *bytes;
                        self.status = format!(
                            "已导出 {tables} 张表 / {objects} 个对象 · {rows} 行 · {}",
                            rustdb_core::human_bytes(*bytes)
                        );
                        self.status_kind = if *failed == 0 {
                            StatusKind::Ok
                        } else {
                            StatusKind::Error
                        };
                    }
                    DumpEvent::Failed { message } => {
                        panel.running = false;
                        panel.error = Some(message.clone());
                        self.status = format!("导出失败：{message}");
                        self.status_kind = StatusKind::Error;
                    }
                }
                true
            }
            // F-75：传输只在面板内展示进度 / 失败清单，不写历史 / 标签页。
            AppEvent::Transfer(event) => {
                let Some(panel) = self.transfer.as_mut() else {
                    return false;
                };
                match event {
                    TransferEvent::Preview { lines, warnings } => {
                        panel.previewing = false;
                        if panel.preview_stale {
                            panel.preview_stale = false;
                            return true;
                        }
                        panel.preview_ready = true;
                        panel.lines = lines.clone();
                        panel.warnings = warnings.clone();
                        panel.error = None;
                        self.status = format!(
                            "传输计划已生成：{} 段 · {} 条警告",
                            lines.len(),
                            warnings.len()
                        );
                        self.status_kind = if warnings.is_empty() {
                            StatusKind::Ok
                        } else {
                            StatusKind::Error
                        };
                    }
                    TransferEvent::Started {
                        source,
                        target,
                        tables,
                        mode,
                        backup,
                    } => {
                        panel.running = true;
                        panel.previewing = false;
                        panel.total = *tables;
                        panel.done = 0;
                        panel.rows = 0;
                        panel.table_rows = 0;
                        panel.failed = 0;
                        panel.failures.clear();
                        panel.current = None;
                        panel.backup = None;
                        panel.elapsed_ms = 0;
                        panel.error = None;
                        self.status = format!(
                            "正在传输 {source} → {target} · {tables} 张表 · {mode}{}",
                            if *backup { " · 先备份目标" } else { "" }
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    TransferEvent::BackupDone { path, bytes } => {
                        panel.backup = Some((path.clone(), *bytes));
                        self.status = format!(
                            "目标已备份：{} · {}",
                            path,
                            rustdb_core::human_bytes(*bytes)
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    TransferEvent::TableStarted { table, .. } => {
                        panel.current = Some(table.clone());
                        panel.table_rows = 0;
                        self.status = format!("正在传输 {table}…");
                        self.status_kind = StatusKind::Busy;
                    }
                    TransferEvent::TableProgress { rows, .. } => {
                        panel.table_rows = *rows;
                        self.status = format!(
                            "传输中 · {}/{} 表 · {} 行{}",
                            panel.done,
                            panel.total,
                            panel.rows + *rows,
                            panel
                                .current
                                .as_ref()
                                .map(|table| format!(" · {table}"))
                                .unwrap_or_default()
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    TransferEvent::TableDone { table, rows, error } => {
                        panel.done += 1;
                        panel.rows += *rows;
                        panel.table_rows = 0;
                        panel.current = Some(table.clone());
                        if let Some(error) = error {
                            panel.failed += 1;
                            panel.failures.push(format!("{table}：{error}"));
                            panel.error = Some(format!("{table}：{error}"));
                        }
                        self.status = format!(
                            "传输中 · {}/{} 表 · {} 行",
                            panel.done, panel.total, panel.rows
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    TransferEvent::Finished {
                        tables,
                        rows,
                        failed,
                        elapsed_ms,
                    } => {
                        panel.running = false;
                        panel.done = *tables;
                        panel.rows = *rows;
                        panel.failed = *failed;
                        panel.table_rows = 0;
                        panel.current = None;
                        panel.elapsed_ms = *elapsed_ms;
                        self.status = format!(
                            "传输完成：{tables} 张表 · {rows} 行{} · {:.1}s",
                            if *failed > 0 {
                                format!(" · {failed} 张失败")
                            } else {
                                String::new()
                            },
                            *elapsed_ms as f64 / 1000.0
                        );
                        self.status_kind = if *failed == 0 {
                            StatusKind::Ok
                        } else {
                            StatusKind::Error
                        };
                    }
                    TransferEvent::Failed { message } => {
                        panel.running = false;
                        panel.previewing = false;
                        panel.error = Some(message.clone());
                        self.status = format!("传输失败：{message}");
                        self.status_kind = StatusKind::Error;
                    }
                }
                true
            }
            // F-78：数据对比只在面板内展示计数 / 脚本 / 进度。
            AppEvent::DataCompare(event) => {
                let Some(panel) = self.data_compare.as_mut() else {
                    return false;
                };
                match event {
                    DataCompareEvent::Started { tables, apply, .. } => {
                        panel.running = true;
                        panel.applying = *apply;
                        panel.previewing = false;
                        panel.total = *tables;
                        panel.done = 0;
                        panel.insert = 0;
                        panel.update = 0;
                        panel.delete = 0;
                        panel.failed = 0;
                        panel.failures.clear();
                        panel.current = None;
                        panel.applied = 0;
                        panel.statements = 0;
                        panel.elapsed_ms = 0;
                        panel.error = None;
                        self.status = format!(
                            "正在对比 {tables} 张表{}…",
                            if *apply { "（结束后同步）" } else { "" }
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    DataCompareEvent::TableStarted { table, .. } => {
                        panel.current = Some(table.clone());
                        self.status = format!("正在对比 {table}…");
                        self.status_kind = StatusKind::Busy;
                    }
                    DataCompareEvent::TableDone {
                        table,
                        insert,
                        update,
                        delete,
                        same,
                        warnings,
                        error,
                    } => {
                        panel.done += 1;
                        panel.insert += insert;
                        panel.update += update;
                        panel.delete += delete;
                        panel.current = Some(table.clone());
                        for warning in warnings {
                            panel.warnings.push(format!("{table}：{warning}"));
                        }
                        if let Some(error) = error {
                            panel.failed += 1;
                            panel.failures.push(format!("{table}：{error}"));
                            panel.error = Some(format!("{table}：{error}"));
                        }
                        self.status = format!(
                            "对比中 · {}/{} 表 · 新增 {} / 更新 {} / 删除 {} · {table} 相同 {same}",
                            panel.done, panel.total, panel.insert, panel.update, panel.delete
                        );
                        self.status_kind = StatusKind::Busy;
                    }
                    DataCompareEvent::Preview {
                        lines,
                        warnings,
                        insert,
                        update,
                        delete,
                        ..
                    } => {
                        panel.previewing = false;
                        if panel.preview_stale {
                            panel.preview_stale = false;
                            return true;
                        }
                        panel.preview_ready = true;
                        panel.lines = lines.clone();
                        panel.warnings = warnings.clone();
                        panel.insert = *insert;
                        panel.update = *update;
                        panel.delete = *delete;
                        panel.error = None;
                        self.status = format!(
                            "差异已生成：新增 {} / 更新 {} / 删除 {} · {} 条警告",
                            insert,
                            update,
                            delete,
                            warnings.len()
                        );
                        self.status_kind = if warnings.is_empty() {
                            StatusKind::Ok
                        } else {
                            StatusKind::Error
                        };
                    }
                    DataCompareEvent::SyncStarted { statements } => {
                        panel.statements = *statements;
                        panel.applied = 0;
                        self.status = format!("正在同步 {statements} 条语句…");
                        self.status_kind = StatusKind::Busy;
                    }
                    DataCompareEvent::SyncProgress {
                        table,
                        applied,
                        statements,
                    } => {
                        panel.applied = *applied;
                        panel.current = Some(table.clone());
                        self.status = format!("同步中 · {applied}/{statements} 条 · {table}");
                        self.status_kind = StatusKind::Busy;
                    }
                    DataCompareEvent::SyncTableFailed { table, message } => {
                        panel.failures.push(format!("{table}：{message}"));
                        panel.error = Some(format!("{table}：{message}"));
                        self.status = format!("同步 {table} 失败：{message}");
                        self.status_kind = StatusKind::Error;
                    }
                    DataCompareEvent::SyncDone { applied, failed } => {
                        panel.applied = *applied;
                        self.status = format!(
                            "同步结束：{applied} 条成功{}",
                            if *failed > 0 {
                                format!(" · {failed} 个失败")
                            } else {
                                String::new()
                            }
                        );
                        self.status_kind = if *failed == 0 {
                            StatusKind::Ok
                        } else {
                            StatusKind::Error
                        };
                    }
                    DataCompareEvent::Finished {
                        tables,
                        failed,
                        elapsed_ms,
                        ..
                    } => {
                        panel.running = false;
                        panel.done = *tables;
                        panel.failed = *failed;
                        panel.current = None;
                        panel.elapsed_ms = *elapsed_ms;
                        if panel.applying {
                            // 数据已变化，旧脚本不能再用。
                            panel.preview_ready = false;
                            panel.lines.clear();
                        }
                        panel.applying = false;
                        self.status = format!(
                            "对比完成：{tables} 张表 · 新增 {} / 更新 {} / 删除 {}{} · {:.1}s",
                            panel.insert,
                            panel.update,
                            panel.delete,
                            if *failed > 0 {
                                format!(" · {failed} 个失败")
                            } else {
                                String::new()
                            },
                            *elapsed_ms as f64 / 1000.0
                        );
                        self.status_kind = if *failed == 0 {
                            StatusKind::Ok
                        } else {
                            StatusKind::Error
                        };
                    }
                    DataCompareEvent::Failed { message } => {
                        panel.running = false;
                        panel.previewing = false;
                        panel.applying = false;
                        panel.error = Some(message.clone());
                        self.status = format!("数据对比失败：{message}");
                        self.status_kind = StatusKind::Error;
                    }
                }
                true
            }
            AppEvent::Ai(event) => {
                use rustdb_core::AiEvent;
                match event {
                    AiEvent::Started { .. } => {
                        self.ai_running = true;
                        self.ai_stream = None;
                    }
                    AiEvent::Thinking { .. } => {}
                    AiEvent::Delta { text, .. } => match self.ai_stream {
                        Some(index) => {
                            if let Some(line) = self.ai_lines.get_mut(index) {
                                line.1.push_str(text);
                            }
                        }
                        None => {
                            self.ai_lines.push(("assistant".to_string(), text.clone()));
                            self.ai_stream = Some(self.ai_lines.len() - 1);
                        }
                    },
                    AiEvent::ToolStarted { tool, risk, .. } => {
                        self.ai_stream = None;
                        self.ai_lines.push((
                            "tool".to_string(),
                            format!("调用 {tool}（{}）…", risk.label()),
                        ));
                    }
                    AiEvent::ToolCompleted { ok, summary, .. } => {
                        self.ai_lines.push((
                            "tool".to_string(),
                            format!("{} {summary}", if *ok { "✓" } else { "✕" }),
                        ));
                    }
                    AiEvent::ConfirmationRequired {
                        run_id,
                        call_id,
                        tool,
                        risk,
                        summary,
                    } => {
                        self.ai_pending = Some((
                            run_id.clone(),
                            call_id.clone(),
                            tool.clone(),
                            summary.clone(),
                        ));
                        self.ai_lines.push((
                            "tool".to_string(),
                            format!("等待确认：{}（{}）", summary, risk.label()),
                        ));
                    }
                    AiEvent::Completed { .. } => {
                        self.ai_running = false;
                        self.ai_stream = None;
                        // 回答已进对话记录，落盘一次（F-48a）。
                        self.persist_ai_session();
                    }
                    AiEvent::Failed { message, .. } => {
                        self.ai_running = false;
                        self.ai_stream = None;
                        self.ai_lines.push(("error".to_string(), message.clone()));
                    }
                    AiEvent::Cancelled { .. } => {
                        self.ai_running = false;
                        self.ai_stream = None;
                    }
                }
                true
            }
            _ => false,
        }
    }
}
