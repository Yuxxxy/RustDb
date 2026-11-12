//! 批量更新面板：条件、预览、执行与外键跳转。

use super::*;

impl MainView {
    /// F-73：打开批量更新面板（目标 = 当前可编辑结果集对应的表）。
    pub(crate) fn open_bulk_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self.active_result() else {
            self.status = "没有可更新的结果集".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let Some((path, _)) = result.editable.clone() else {
            self.status = "只有「打开表」的结果能批量更新".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let columns: Vec<String> = result
            .columns
            .iter()
            .map(|column| column.name.to_string())
            .collect();
        let set_column = designer_input(window, cx, "要更新的字段", None);
        let set_value = designer_input(window, cx, "新值", None);
        let column = designer_input(window, cx, "条件字段", None);
        let value = designer_input(window, cx, "条件值", None);
        let custom_where = designer_input(window, cx, "自定义 WHERE", None);
        self.bulk = Some(BulkPanel {
            target: path,
            columns,
            set_column,
            set_value,
            conditions: vec![BulkConditionRow {
                column,
                op: rustdb_core::BulkOp::Eq,
                value,
            }],
            join: rustdb_core::BulkJoin::And,
            custom: false,
            custom_where,
            plan: None,
            count_query: None,
            count: None,
            exec_query: None,
            error: None,
        });
        cx.notify();
    }

    /// 从面板输入组装请求（空字段交给 `core::bulk_update::plan` 校验）。
    pub(crate) fn bulk_request(
        &self,
        cx: &Context<Self>,
    ) -> Result<rustdb_core::BulkUpdateRequest, String> {
        let panel = self.bulk.as_ref().ok_or("批量更新面板未打开")?;
        let filter = if panel.custom {
            rustdb_core::BulkFilter::Custom(panel.custom_where.read(cx).value().to_string())
        } else {
            rustdb_core::BulkFilter::Structured {
                join: panel.join,
                conditions: panel
                    .conditions
                    .iter()
                    .map(|row| rustdb_core::BulkCondition {
                        column: row.column.read(cx).value().to_string(),
                        op: row.op,
                        value: row.value.read(cx).value().to_string(),
                    })
                    .collect(),
            }
        };
        Ok(rustdb_core::BulkUpdateRequest {
            target: panel.target.clone(),
            set_column: panel.set_column.read(cx).value().to_string(),
            set_value: panel.set_value.read(cx).value().to_string(),
            filter,
        })
    }

    /// 面板内报错（输入非法 / 执行失败都只影响面板，不改标签页）。
    pub(crate) fn bulk_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        let message = message.into();
        if let Some(panel) = self.bulk.as_mut() {
            panel.error = Some(message.clone());
        }
        self.status = message;
        self.status_kind = StatusKind::Error;
        cx.notify();
    }

    /// 预览影响行数：先生成计划，再跑 COUNT（结果不进标签页）。
    pub(crate) fn bulk_preview(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.bulk_error("请先连接数据源", cx);
            return;
        };
        let request = match self.bulk_request(cx) {
            Ok(request) => request,
            Err(message) => {
                self.bulk_error(message, cx);
                return;
            }
        };
        let plan = match self.core.plan_bulk_update(&session, &request) {
            Ok(plan) => plan,
            Err(message) => {
                self.bulk_error(message, cx);
                return;
            }
        };
        match self
            .core
            .execute_with_autocommit(&session, &plan.count_sql, self.auto_commit)
        {
            Ok(query_id) => {
                if let Some(panel) = self.bulk.as_mut() {
                    panel.plan = Some(plan);
                    panel.count = None;
                    panel.count_query = Some(query_id);
                    panel.error = None;
                }
                self.status = "正在统计预计影响行数…".into();
                self.status_kind = StatusKind::Busy;
                self.diag_dirty = true;
            }
            Err(message) => self.bulk_error(message, cx),
        }
        cx.notify();
    }

    /// 执行前再算一次计划：条件改过就要求重新预览，避免拿旧行数确认。
    pub(crate) fn bulk_ask_execute(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.bulk_error("请先连接数据源", cx);
            return;
        };
        let request = match self.bulk_request(cx) {
            Ok(request) => request,
            Err(message) => {
                self.bulk_error(message, cx);
                return;
            }
        };
        let current = match self.core.plan_bulk_update(&session, &request) {
            Ok(plan) => plan,
            Err(message) => {
                self.bulk_error(message, cx);
                return;
            }
        };
        let Some(panel) = self.bulk.as_ref() else {
            return;
        };
        if panel.plan.is_none() || panel.count.is_none() {
            self.bulk_error("请先预览预计影响行数", cx);
            return;
        }
        let previewed = panel.plan.as_ref().map(|plan| plan.update_sql.clone());
        if previewed.as_deref() != Some(current.update_sql.as_str()) {
            self.bulk_error("条件或新值已修改，请重新预览影响行数", cx);
            return;
        }
        let path = panel.target.clone();
        self.danger_input = None;
        self.danger = Some(DangerAction::BulkUpdate { path });
        cx.notify();
    }

    /// 确认后真正下发 UPDATE（写入门控在 `Core::apply_bulk_update`）。
    pub(crate) fn bulk_execute(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.bulk_error("请先连接数据源", cx);
            return;
        };
        let Some(plan) = self.bulk.as_ref().and_then(|panel| panel.plan.clone()) else {
            self.bulk_error("请先预览影响行数", cx);
            return;
        };
        match self
            .core
            .apply_bulk_update(&session, &plan, self.auto_commit)
        {
            Ok(query_id) => {
                if let Some(panel) = self.bulk.as_mut() {
                    panel.exec_query = Some(query_id);
                    panel.error = None;
                }
                self.status = format!("正在批量更新 {}…", plan.target.display_label());
                self.status_kind = StatusKind::Busy;
                self.diag_dirty = true;
            }
            Err(message) => self.bulk_error(message, cx),
        }
        cx.notify();
    }

    /// 面板消化 COUNT / UPDATE 的完成事件；返回 true 表示不再走标签页逻辑。
    pub(crate) fn finish_bulk_query(
        &mut self,
        query_id: &QueryId,
        results: &[rustdb_core::ResultSummary],
    ) -> bool {
        let Some(panel) = self.bulk.as_ref() else {
            return false;
        };
        let is_count = panel.count_query.as_ref() == Some(query_id);
        let is_exec = panel.exec_query.as_ref() == Some(query_id);
        if !is_count && !is_exec {
            return false;
        }
        let count = if is_count {
            results.first().and_then(|summary| {
                self.core
                    .results()
                    .rows(summary.set_id, 0, 1)
                    .into_iter()
                    .next()
                    .and_then(|row| row.first().map(|value| value.display_text()))
                    .and_then(|text| text.trim().parse::<u64>().ok())
            })
        } else {
            None
        };
        // 预览 / 执行的结果集不挂标签页，直接归还预算。
        for summary in results {
            self.core.results().remove(summary.set_id);
        }
        if is_count {
            if let Some(panel) = self.bulk.as_mut() {
                panel.count_query = None;
                panel.count = count;
                panel.error = count
                    .is_none()
                    .then(|| "行数预览没有返回可解析的计数".to_string());
            }
            self.status = match count {
                Some(count) => format!("预计影响 {count} 行"),
                None => "行数预览失败".into(),
            };
            self.status_kind = if count.is_some() {
                StatusKind::Ok
            } else {
                StatusKind::Error
            };
        } else {
            if let Some(panel) = self.bulk.as_mut() {
                panel.exec_query = None;
            }
            self.bulk = None;
            self.bulk_refresh = true;
            self.status = "批量更新已完成；未加载行无法生成可靠回滚 SQL，请刷新后核对".into();
            self.status_kind = StatusKind::Ok;
            self.diag_dirty = true;
        }
        true
    }

    /// 预览 / 执行失败：只回填面板错误。
    pub(crate) fn fail_bulk_query(
        &mut self,
        query_id: &QueryId,
        error: &rustdb_core::EventError,
    ) -> bool {
        let Some(panel) = self.bulk.as_mut() else {
            return false;
        };
        let is_count = panel.count_query.as_ref() == Some(query_id);
        let is_exec = panel.exec_query.as_ref() == Some(query_id);
        if !is_count && !is_exec {
            return false;
        }
        if is_count {
            panel.count_query = None;
            panel.count = None;
        } else {
            panel.exec_query = None;
        }
        panel.error = Some(error.message.clone());
        self.status = format!("批量更新失败：{}", error.message);
        self.status_kind = StatusKind::Error;
        true
    }

    /// 执行完在下一帧刷新当前表的数据（事件处理阶段拿不到 window）。
    pub(crate) fn refresh_after_bulk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.active_result().and_then(|result| result.page.clone()) else {
            return;
        };
        let tab_id = self.tabs[self.active_tab].id;
        self.reload_page(
            tab_id,
            page.path,
            page.page,
            page.page_size,
            page.order,
            window,
            cx,
        );
    }

    /// F-73：从选中单元格跳到外键指向的那一条记录。
    pub(crate) fn foreign_key_jump(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resolved: Result<(ObjectPath, ForeignKeyInfo, Vec<Value>, ObjectPath), String> =
            (|| {
                let result = self.active_result().ok_or("没有可跳转的结果集")?;
                let (path, _) = result
                    .editable
                    .clone()
                    .ok_or("只有「打开表」的结果能跳到被引用记录")?;
                let (row, col) = self.selected_cell.ok_or("请先选中一个单元格")?;
                let column = result
                    .columns
                    .get(col)
                    .map(|column| column.name.to_string())
                    .ok_or("选中的列不存在")?;
                let Some(foreign_keys) = self.table_fks.get(&path.display_label()) else {
                    if let Some(session) = self.session.as_ref() {
                        self.core.request_table_detail(session, path.clone());
                    }
                    return Err("外键信息还没取回，请稍后重试".into());
                };
                let foreign_key = rustdb_core::foreign_key_for_column(foreign_keys, &column)
                    .cloned()
                    .ok_or_else(|| format!("列 {column} 不属于任何外键"))?;
                let mut values = Vec::with_capacity(foreign_key.columns.len());
                for name in &foreign_key.columns {
                    let index = result
                        .columns
                        .iter()
                        .position(|column| column.name.eq_ignore_ascii_case(name))
                        .ok_or_else(|| format!("结果里没有外键列 {name}"))?;
                    let value = result
                        .rows
                        .get(row)
                        .and_then(|values| values.get(index))
                        .cloned()
                        .ok_or_else(|| format!("选中行没有列 {name} 的值"))?;
                    values.push(value);
                }
                let target = ObjectPath::new(
                    path.ns.clone(),
                    foreign_key.referenced_table.clone(),
                    ObjectKind::Table,
                );
                Ok((path, foreign_key, values, target))
            })();
        let (source, foreign_key, values, target) = match resolved {
            Ok(resolved) => resolved,
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Idle;
                cx.notify();
                return;
            }
        };
        let Some(session) = self.session.clone() else {
            self.status = "请先连接数据源".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let sql =
            match self
                .core
                .foreign_key_lookup_sql(&session, &source.ns, &foreign_key, &values)
            {
                Ok(sql) => sql,
                Err(message) => {
                    self.status = message;
                    self.status_kind = StatusKind::Error;
                    cx.notify();
                    return;
                }
            };
        self.record_recent_object(&target);
        self.open_tab(source.ns.clone(), window, cx);
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        self.query_count += 1;
        self.run(window, cx);
    }

    /// F-73：批量更新面板（SET 单列 + 条件预览 + 二次确认执行）。
    pub(crate) fn render_bulk_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.bulk.clone() else {
            return div().into_any_element();
        };
        let mut body = v_flex().gap_2();
        body = body.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(conn_field(&t, "字段", &panel.set_column, px(180.)))
                .child(conn_field(&t, "新值", &panel.set_value, px(220.))),
        );
        if !panel.columns.is_empty() {
            body = body.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .truncate()
                    .child(SharedString::from(format!(
                        "可用列：{}",
                        panel.columns.join(", ")
                    ))),
            );
        }

        let mut mode_row = h_flex().gap_1().items_center();
        for (custom, label, key) in [
            (false, "结构化条件", "struct"),
            (true, "自定义 WHERE", "custom"),
        ] {
            let active = panel.custom == custom;
            let selector = format!("bulk-mode-{key}");
            mode_row = mode_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(18.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(crate::design::TS_CAPTION))
                    .cursor_pointer()
                    .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                    .when(!active, |d| {
                        d.text_color(hex(t.fg_secondary))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(panel) = view.bulk.as_mut() {
                            panel.custom = custom;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(label)),
            );
        }
        body = body.child(mode_row);

        if panel.custom {
            body = body.child(
                div()
                    .debug_selector(|| "bulk-custom-where".to_string())
                    .child(Input::new(&panel.custom_where)),
            );
        } else {
            let mut join_row = h_flex().gap_1().items_center().child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("条件之间"),
            );
            for option in rustdb_core::BulkJoin::ALL {
                let active = option == panel.join;
                let key = match option {
                    rustdb_core::BulkJoin::And => "and",
                    rustdb_core::BulkJoin::Or => "or",
                };
                let selector = format!("bulk-join-{key}");
                join_row = join_row.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .h(px(18.))
                        .px_2()
                        .rounded(px(4.))
                        .text_size(px(crate::design::TS_CAPTION))
                        .cursor_pointer()
                        .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                        .when(!active, |d| {
                            d.text_color(hex(t.fg_secondary))
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if let Some(panel) = view.bulk.as_mut() {
                                panel.join = option;
                            }
                            cx.notify();
                        }))
                        .child(SharedString::from(option.label())),
                );
            }
            body = body.child(join_row);

            let mut conditions = v_flex().gap_1();
            for (index, row) in panel.conditions.iter().enumerate() {
                let mut ops = h_flex().gap_1().items_center();
                for option in rustdb_core::BulkOp::ALL {
                    let active = option == row.op;
                    let selector = format!("bulk-cond-{index}-op-{}", option.slug());
                    ops = ops.child(
                        div()
                            .id(SharedString::from(selector.clone()))
                            .debug_selector(move || selector.clone())
                            .h(px(16.))
                            .px_1()
                            .rounded(px(4.))
                            .text_size(px(crate::design::TS_CAPTION))
                            .cursor_pointer()
                            .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                            .when(!active, |d| {
                                d.text_color(hex(t.fg_secondary))
                                    .hover(|s| s.bg(hex(t.bg_overlay)))
                            })
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(row) = view
                                    .bulk
                                    .as_mut()
                                    .and_then(|panel| panel.conditions.get_mut(index))
                                {
                                    row.op = option;
                                }
                                cx.notify();
                            }))
                            .child(SharedString::from(option.label())),
                    );
                }
                let column_selector = format!("bulk-cond-{index}-column");
                let value_selector = format!("bulk-cond-{index}-value");
                let mut head = h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(150.))
                            .debug_selector(move || column_selector.clone())
                            .child(Input::new(&row.column)),
                    )
                    .child(
                        div()
                            .w(px(200.))
                            .debug_selector(move || value_selector.clone())
                            .child(Input::new(&row.value)),
                    );
                if panel.conditions.len() > 1 {
                    head = head.child(
                        div()
                            .debug_selector(|| format!("bulk-remove-{index}"))
                            .child(
                                Button::new(SharedString::from(format!("bulk-remove-btn-{index}")))
                                    .small()
                                    .ghost()
                                    .label("移除")
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        if let Some(panel) = view.bulk.as_mut() {
                                            if index < panel.conditions.len() {
                                                panel.conditions.remove(index);
                                            }
                                        }
                                        cx.notify();
                                    })),
                            ),
                    );
                }
                conditions = conditions.child(
                    v_flex()
                        .gap_1()
                        .p_2()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hex(t.border_subtle))
                        .child(head)
                        .child(ops),
                );
            }
            body = body.child(conditions).child(
                div()
                    .debug_selector(|| "bulk-add-condition".to_string())
                    .child(
                        Button::new("bulk-add-condition-btn")
                            .small()
                            .ghost()
                            .label("添加条件")
                            .on_click(cx.listener(|view, _, window, cx| {
                                let column = designer_input(window, cx, "条件字段", None);
                                let value = designer_input(window, cx, "条件值", None);
                                if let Some(panel) = view.bulk.as_mut() {
                                    panel.conditions.push(BulkConditionRow {
                                        column,
                                        op: rustdb_core::BulkOp::Eq,
                                        value,
                                    });
                                }
                                cx.notify();
                            })),
                    ),
            );
        }

        if let Some(plan) = panel.plan.clone() {
            let count_label = match (panel.count, panel.count_query.is_some()) {
                (Some(count), _) => format!("预计影响 {count} 行"),
                (None, true) => "正在统计预计影响行数…".into(),
                (None, false) => "尚未统计影响行数".into(),
            };
            let mut preview = v_flex().gap_1().child(
                div()
                    .debug_selector(|| "bulk-count".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.accent))
                    .child(SharedString::from(count_label)),
            );
            for warning in &plan.warnings {
                preview = preview.child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(warning.clone())),
                );
            }
            preview = preview.child(
                div()
                    .debug_selector(|| "bulk-sql".to_string())
                    .max_h(px(140.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .whitespace_normal()
                    .child(crate::ddl::render_sql(&plan.update_sql, &t)),
            );
            body = body.child(preview);
        }
        if let Some(error) = panel.error.clone() {
            body = body.child(
                div()
                    .debug_selector(|| "bulk-error".to_string())
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error)),
            );
        }

        let busy = panel.count_query.is_some() || panel.exec_query.is_some();
        v_flex()
            .id("bulk-panel")
            .debug_selector(|| "bulk-panel".to_string())
            .w(px(620.))
            .max_h(px(600.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            // 卡片吃掉鼠标事件，避免点击被遮罩当成「关闭」。
            .occlude()
            .child(
                v_flex()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(crate::design::TS_BODY))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hex(t.fg_primary))
                            .child("按条件批量更新"),
                    )
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_secondary))
                            .child(SharedString::from(panel.target.display_label())),
                    ),
            )
            .child(
                div()
                    .id("bulk-body")
                    .max_h(px(430.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(body),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div().debug_selector(|| "bulk-cancel".to_string()).child(
                            Button::new("bulk-cancel-btn")
                                .small()
                                .ghost()
                                .label("取消")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.bulk = None;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "bulk-preview".to_string()).child(
                            Button::new("bulk-preview-btn")
                                .small()
                                .label("预览影响行数")
                                .disabled(busy)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.bulk_preview(cx);
                                })),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "bulk-execute".to_string()).child(
                            Button::new("bulk-execute-btn")
                                .small()
                                .label("执行更新")
                                .disabled(busy)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.bulk_ask_execute(cx);
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }
}
