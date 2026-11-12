//! 分区管理面板（F-85）：列表、新建、附加 / 分离与删除。

use super::*;

impl MainView {
    /// F-85：打开分区管理面板；驱动不支持时显式降级（R5：不假装支持）。
    pub(crate) fn open_partitions(
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
        let supported = self
            .capabilities
            .as_ref()
            .is_some_and(|caps| caps.partition_admin);
        if !supported {
            self.status = "该驱动暂不支持分区管理".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        }
        let name = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx).placeholder("分区名 / 已存在的表名")
        });
        let range_from = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx).placeholder("起点，如 '2026-01-01'")
        });
        let range_to = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx).placeholder("终点，如 '2026-02-01'")
        });
        let list_values = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder("逗号分隔，如 'cn', 'us'")
        });
        let modulus =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("MODULUS"));
        let remainder = cx
            .new(|cx| gpui_component::input::InputState::new(window, cx).placeholder("REMAINDER"));
        self.partition = Some(PartitionPanel {
            label: path.display_label(),
            path: path.clone(),
            loading: true,
            error: None,
            list: None,
            form: PartitionForm::None,
            name,
            range_from,
            range_to,
            list_values,
            modulus,
            remainder,
            is_default: false,
            detach_concurrently: false,
            preview: None,
            running: None,
            failure: None,
        });
        self.status = format!("正在读取 {} 的分区…", path.name);
        self.status_kind = StatusKind::Busy;
        self.core.request_partitions(&session, path);
        cx.notify();
    }

    /// F-85：父表策略（来自元数据；`None` = 不是分区表 / 驱动没给）。
    pub(crate) fn partition_strategy(&self) -> Option<rustdb_core::PartitionStrategy> {
        self.partition
            .as_ref()?
            .list
            .as_ref()?
            .strategy
            .as_deref()
            .and_then(rustdb_core::PartitionStrategy::parse)
    }

    /// F-85：按当前表单与策略拼边界；校验失败给出可读原因。
    pub(crate) fn partition_bound(
        &self,
        panel: &PartitionPanel,
        cx: &App,
    ) -> Result<rustdb_core::PartitionBound, String> {
        use rustdb_core::{PartitionBound, PartitionStrategy};
        if panel.is_default {
            return Ok(PartitionBound::Default);
        }
        let strategy = self
            .partition_strategy()
            .ok_or("该表不是分区表；PostgreSQL 需要先创建分区父表")?;
        Ok(match strategy {
            PartitionStrategy::Range => PartitionBound::Range {
                from: panel.range_from.read(cx).value().to_string(),
                to: panel.range_to.read(cx).value().to_string(),
            },
            PartitionStrategy::List => PartitionBound::List {
                values: panel
                    .list_values
                    .read(cx)
                    .value()
                    .split(',')
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect(),
            },
            PartitionStrategy::Hash => {
                let modulus: u32 = panel
                    .modulus
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .map_err(|_| "MODULUS 需要正整数")?;
                let remainder: u32 = panel
                    .remainder
                    .read(cx)
                    .value()
                    .trim()
                    .parse()
                    .map_err(|_| "REMAINDER 需要非负整数")?;
                PartitionBound::Hash { modulus, remainder }
            }
        })
    }

    /// F-85：新建 / 附加 → 生成预览（DDL 必须显式确认后才执行）。
    pub(crate) fn partition_plan(&mut self, form: PartitionForm, cx: &mut Context<Self>) {
        use rustdb_core::PartitionAction;
        let Some(session) = self.session.clone() else {
            self.partition_fail("请先连接数据源".into(), cx);
            return;
        };
        let Some(kind) = self.core.session_kind(&session) else {
            self.partition_fail("无法识别连接类型，请重新连接".into(), cx);
            return;
        };
        let Some(panel) = self.partition.as_ref() else {
            return;
        };
        let name = panel.name.read(cx).value().trim().to_string();
        let path = panel.path.clone();
        let bound = match self.partition_bound(panel, cx) {
            Ok(bound) => bound,
            Err(message) => {
                self.partition_fail(message, cx);
                return;
            }
        };
        let action = match form {
            PartitionForm::Create => PartitionAction::Create { name, bound },
            PartitionForm::Attach => PartitionAction::Attach { table: name, bound },
            PartitionForm::None => return,
        };
        let plan = rustdb_core::plan_partition(kind, &path, &action);
        if let Some(error) = &plan.error {
            let error = error.clone();
            self.partition_fail(error, cx);
            return;
        }
        if let Some(panel) = self.partition.as_mut() {
            panel.failure = None;
            panel.preview = Some(PartitionPreview { plan });
        }
        cx.notify();
    }

    /// F-85：分离 / 删除某个分区 → 生成预览。
    pub(crate) fn partition_row_action(
        &mut self,
        partition: String,
        detach: bool,
        cx: &mut Context<Self>,
    ) {
        use rustdb_core::PartitionAction;
        let Some(session) = self.session.clone() else {
            self.partition_fail("请先连接数据源".into(), cx);
            return;
        };
        let Some(kind) = self.core.session_kind(&session) else {
            self.partition_fail("无法识别连接类型，请重新连接".into(), cx);
            return;
        };
        let Some(panel) = self.partition.as_ref() else {
            return;
        };
        let path = panel.path.clone();
        let concurrently = panel.detach_concurrently;
        let action = if detach {
            PartitionAction::Detach {
                partition,
                concurrently,
            }
        } else {
            PartitionAction::Drop { partition }
        };
        let plan = rustdb_core::plan_partition(kind, &path, &action);
        if let Some(error) = &plan.error {
            let error = error.clone();
            self.partition_fail(error, cx);
            return;
        }
        if let Some(panel) = self.partition.as_mut() {
            panel.failure = None;
            panel.preview = Some(PartitionPreview { plan });
        }
        cx.notify();
    }

    /// F-85：面板内报错（面板保持打开）。
    pub(crate) fn partition_fail(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(panel) = self.partition.as_mut() {
            panel.failure = Some(DesignerFailure {
                hint: rustdb_core::recovery_hint(&message),
                message: message.clone(),
            });
        }
        self.status = message;
        self.status_kind = StatusKind::Error;
        cx.notify();
    }

    /// F-85：执行预览里的分区 DDL。
    pub(crate) fn partition_execute(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self
            .partition
            .as_mut()
            .and_then(|panel| panel.preview.take())
        else {
            return;
        };
        let Some(session) = self.session.clone() else {
            self.partition_fail("请先连接数据源".into(), cx);
            return;
        };
        match self.core.apply_partition_plan(&session, &preview.plan) {
            Ok(query_id) => {
                if let Some(panel) = self.partition.as_mut() {
                    panel.running = Some(query_id);
                    panel.failure = None;
                }
                self.status = "正在执行分区变更…".into();
                self.status_kind = StatusKind::Busy;
            }
            Err(message) => {
                self.partition_fail(message, cx);
                return;
            }
        }
        cx.notify();
    }

    /// F-85：打开 / 收起分区表单。
    pub(crate) fn partition_set_form(&mut self, form: PartitionForm, cx: &mut Context<Self>) {
        if let Some(panel) = self.partition.as_mut() {
            panel.form = form;
            panel.failure = None;
            cx.notify();
        }
    }

    /// F-85：切换「DEFAULT 分区」。
    pub(crate) fn partition_toggle_default(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.partition.as_mut() {
            panel.is_default = !panel.is_default;
            cx.notify();
        }
    }

    /// F-85：切换分离是否用 CONCURRENTLY。
    pub(crate) fn partition_toggle_concurrently(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.partition.as_mut() {
            panel.detach_concurrently = !panel.detach_concurrently;
            cx.notify();
        }
    }
    pub(crate) fn render_partition_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use rustdb_core::PartitionStrategy;
        let t = current(cx);
        let Some(panel) = self.partition.clone() else {
            return div().into_any_element();
        };
        let strategy = panel
            .list
            .as_ref()
            .and_then(|list| list.strategy.as_deref())
            .and_then(PartitionStrategy::parse);
        let partitioned = strategy.is_some();
        let accepts_default = strategy.is_some_and(|strategy| strategy.accepts_default());
        let form_kind = panel.form;
        let partitions = panel
            .list
            .as_ref()
            .map(|list| list.partitions.clone())
            .unwrap_or_default();
        let truncated = panel.list.as_ref().is_some_and(|list| list.truncated);
        let key = panel.list.as_ref().and_then(|list| list.key.clone());
        let note = panel.list.as_ref().and_then(|list| list.note.clone());

        let header = h_flex()
            .h(px(34.))
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .child(
                Icon::new(IconName::Layers)
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
                    .child(SharedString::from(format!("分区 · {}", panel.label))),
            )
            .child(
                div()
                    .id("partition-panel-close")
                    .debug_selector(|| "partition-panel-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|s| s.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.partition = None;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconName::Close)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );

        // 信息条：策略 / 加载 / 错误 / 说明 / 失败明细。
        let mut info = v_flex()
            .px_3()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(hex(t.border_subtle));
        if let Some(strategy) = strategy {
            info = info.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_secondary))
                    .child(SharedString::from(match key {
                        Some(key) => format!("{} 分区 · {key}", strategy.label()),
                        None => format!("{} 分区", strategy.label()),
                    })),
            );
        }
        if panel.loading {
            info = info.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("正在读取分区…"),
            );
        } else if let Some(error) = &panel.error {
            info = info.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error.clone())),
            );
        }
        if let Some(failure) = &panel.failure {
            info = info
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
        }
        if let Some(note) = note {
            info = info.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(note)),
            );
        }

        // 分区列表。
        let mut list = v_flex()
            .id("partition-list")
            .max_h(px(210.))
            .min_h(px(40.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        if !panel.loading && panel.error.is_none() && partitions.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .py_3()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(if partitioned {
                        "还没有分区"
                    } else {
                        "该表不是分区表；PostgreSQL 需要先创建分区父表"
                    }),
            );
        }
        for (index, item) in partitions.iter().enumerate() {
            let name = item.name.clone();
            let detach_name = name.clone();
            let drop_name = name.clone();
            let mut row = h_flex()
                .id(SharedString::from(format!("partition-row-{index}")))
                .debug_selector(move || format!("partition-row-{index}"))
                .px_3()
                .py_1()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(hex(t.border_subtle))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(crate::design::TS_CAPTION))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(hex(t.fg_primary))
                                        .text_ellipsis()
                                        .child(SharedString::from(name)),
                                )
                                .when(item.is_default, |this| {
                                    this.child(
                                        div()
                                            .px_1()
                                            .rounded(px(3.))
                                            .bg(hex(t.bg_overlay))
                                            .text_size(px(crate::design::TS_CAPTION))
                                            .text_color(hex(t.fg_muted))
                                            .child("DEFAULT"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .text_ellipsis()
                                .child(SharedString::from(item.bounds.clone())),
                        ),
                );
            if let Some(rows) = item.row_estimate {
                row = row.child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_muted))
                        .child(SharedString::from(format!("~{rows} 行"))),
                );
            }
            row = row
                .child(
                    div()
                        .debug_selector(move || format!("partition-detach-{index}"))
                        .child(
                            Button::new(SharedString::from(format!(
                                "partition-detach-btn-{index}"
                            )))
                            .small()
                            .ghost()
                            .label("分离")
                            .disabled(panel.running.is_some())
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    view.partition_row_action(detach_name.clone(), true, cx);
                                },
                            )),
                        ),
                )
                .child(
                    div()
                        .debug_selector(move || format!("partition-drop-{index}"))
                        .child(
                            Button::new(SharedString::from(format!("partition-drop-btn-{index}")))
                                .small()
                                .ghost()
                                .label("删除")
                                .disabled(panel.running.is_some())
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.partition_row_action(drop_name.clone(), false, cx);
                                })),
                        ),
                );
            list = list.child(row);
        }
        if truncated {
            list = list.child(
                div()
                    .px_3()
                    .py_1()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("分区过多，仅显示前 500 个"),
            );
        }

        // 表单（按策略给出边界输入）。
        let mut form = v_flex().px_3().py_2().gap_2();
        if panel.form != PartitionForm::None {
            let attach = panel.form == PartitionForm::Attach;
            form = form.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child(if attach {
                        "附加已有表"
                    } else {
                        "新建分区"
                    }),
            );
            form = form.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(210.)).child(Input::new(&panel.name).small()))
                    .child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.fg_muted))
                            .child(if attach {
                                "要附加的表名（同库）"
                            } else {
                                "分区名（同库）"
                            }),
                    ),
            );
            if !panel.is_default {
                match strategy {
                    Some(PartitionStrategy::Range) => {
                        form = form.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(210.))
                                        .child(Input::new(&panel.range_from).small()),
                                )
                                .child(
                                    div().w(px(210.)).child(Input::new(&panel.range_to).small()),
                                ),
                        );
                    }
                    Some(PartitionStrategy::List) => {
                        form = form.child(
                            div()
                                .w(px(430.))
                                .child(Input::new(&panel.list_values).small()),
                        );
                    }
                    Some(PartitionStrategy::Hash) => {
                        form = form.child(
                            h_flex()
                                .gap_2()
                                .child(div().w(px(140.)).child(Input::new(&panel.modulus).small()))
                                .child(
                                    div()
                                        .w(px(140.))
                                        .child(Input::new(&panel.remainder).small()),
                                ),
                        );
                    }
                    None => {}
                }
            }
            let mut actions = h_flex().gap_2().items_center();
            if accepts_default {
                actions = actions.child(
                    div()
                        .id("partition-default-toggle")
                        .debug_selector(|| "partition-default-toggle".to_string())
                        .px_2()
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .text_size(px(crate::design::TS_CAPTION))
                        .when(panel.is_default, |this| {
                            this.bg(hex(t.bg_overlay)).text_color(hex(t.accent))
                        })
                        .when(!panel.is_default, |this| {
                            this.text_color(hex(t.fg_muted))
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                        })
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.partition_toggle_default(cx);
                        }))
                        .child("DEFAULT 分区"),
                );
            }
            actions = actions
                .child(
                    div()
                        .debug_selector(|| "partition-form-cancel".to_string())
                        .child(
                            Button::new("partition-form-cancel-btn")
                                .small()
                                .ghost()
                                .label("取消")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.partition_set_form(PartitionForm::None, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "partition-form-preview".to_string())
                        .child(
                            Button::new("partition-form-preview-btn")
                                .small()
                                .primary()
                                .label("预览 DDL")
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.partition_plan(form_kind, cx);
                                })),
                        ),
                );
            form = form.child(actions);
        }

        // 底部：新建 / 附加入口 + CONCURRENTLY 开关。
        let mut footer = h_flex()
            .h(px(36.))
            .px_3()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(hex(t.border_subtle))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .text_ellipsis()
                    .child(if partitioned {
                        "DDL 会先预览再执行"
                    } else {
                        "非分区表不能新建分区"
                    }),
            );
        if partitioned {
            footer = footer
                .child(
                    div()
                        .id("partition-concurrently")
                        .debug_selector(|| "partition-concurrently".to_string())
                        .px_2()
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .text_size(px(crate::design::TS_CAPTION))
                        .when(panel.detach_concurrently, |this| {
                            this.bg(hex(t.bg_overlay)).text_color(hex(t.accent))
                        })
                        .when(!panel.detach_concurrently, |this| {
                            this.text_color(hex(t.fg_muted))
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                        })
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.partition_toggle_concurrently(cx);
                        }))
                        .child("分离用 CONCURRENTLY"),
                )
                .child(
                    div()
                        .debug_selector(|| "partition-create".to_string())
                        .child(
                            Button::new("partition-create-btn")
                                .small()
                                .ghost()
                                .label("新建分区")
                                .disabled(panel.running.is_some())
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.partition_set_form(PartitionForm::Create, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "partition-attach".to_string())
                        .child(
                            Button::new("partition-attach-btn")
                                .small()
                                .ghost()
                                .label("附加已有表")
                                .disabled(panel.running.is_some())
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.partition_set_form(PartitionForm::Attach, cx);
                                })),
                        ),
                );
        }

        v_flex()
            .id("partition-panel")
            .debug_selector(|| "partition-panel".to_string())
            .w(px(560.))
            .max_h(px(480.))
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .overflow_hidden()
            // 吃掉面板内的点击，避免穿透到遮罩把面板关掉。
            .on_click(cx.listener(|_, _, _, _| {}))
            .child(header)
            .child(info)
            .child(list)
            .when(panel.form != PartitionForm::None, |this| {
                this.child(
                    div()
                        .border_t_1()
                        .border_color(hex(t.border_subtle))
                        .child(form),
                )
            })
            .child(footer)
            .into_any_element()
    }

    /// F-85：分区 DDL 预览（盖在分区面板之上，执行前必须显式确认）。
    pub(crate) fn render_partition_preview(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some(preview) = self
            .partition
            .as_ref()
            .and_then(|panel| panel.preview.clone())
        else {
            return div().into_any_element();
        };
        let text = preview.plan.preview_text();
        let warning = preview.plan.warning.clone();
        let running = self
            .partition
            .as_ref()
            .is_some_and(|panel| panel.running.is_some());
        v_flex()
            .id("partition-preview")
            .debug_selector(|| "partition-preview".to_string())
            .w(px(560.))
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
                    .child("分区变更预览"),
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
                            .debug_selector(|| "partition-preview-cancel".to_string())
                            .child(
                                Button::new("partition-preview-cancel-btn")
                                    .small()
                                    .ghost()
                                    .label("取消")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        if let Some(panel) = view.partition.as_mut() {
                                            panel.preview = None;
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "partition-preview-execute".to_string())
                            .child(
                                Button::new("partition-preview-execute-btn")
                                    .small()
                                    .primary()
                                    .label(if running { "执行中…" } else { "执行" })
                                    .disabled(running)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.partition_execute(cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
