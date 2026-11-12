//! 字段血缘面板（F-88）：外键、视图映射与历史 SQL 的分级聚合。

use super::*;

impl MainView {
    /// F-88：打开字段血缘面板（只读聚合：外键 / 视图定义 / 历史 SQL / 同名字段）。
    pub(crate) fn open_lineage_panel(
        &mut self,
        path: ObjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        let Some(ns) = self
            .current_ns
            .clone()
            .or_else(|| self.active().and_then(|tab| tab.ns.clone()))
        else {
            self.set_status("请先选择数据库", StatusKind::Error, cx);
            return;
        };
        let filter = designer_input(window, cx, "筛选字段", None);
        let profile = self
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".to_string());
        self.lineage = Some(LineagePanel {
            namespace: ns.clone(),
            loading: true,
            input: None,
            table: path,
            columns: Vec::new(),
            selected: None,
            filter,
            min: rustdb_core::Confidence::Possible,
            report: None,
            error: None,
        });
        self.core.request_lineage(&session, ns, Some(profile));
        self.set_status("正在汇总血缘…", StatusKind::Busy, cx);
    }

    /// 按当前列与最低置信度重算报告（只读，无请求）。
    pub(crate) fn refresh_lineage_report(&mut self) {
        let Some(panel) = self.lineage.as_mut() else {
            return;
        };
        let report = match (panel.input.as_ref(), panel.selected.as_ref()) {
            (Some(input), Some(column)) => {
                Some(rustdb_core::lineage_report(input, &panel.table, column))
            }
            _ => None,
        };
        panel.report = report;
    }

    pub(crate) fn select_lineage_column(&mut self, column: String, cx: &mut Context<Self>) {
        if let Some(panel) = self.lineage.as_mut() {
            panel.selected = Some(column);
        }
        self.refresh_lineage_report();
        cx.notify();
    }

    pub(crate) fn set_lineage_min(&mut self, min: rustdb_core::Confidence, cx: &mut Context<Self>) {
        if let Some(panel) = self.lineage.as_mut() {
            panel.min = min;
        }
        cx.notify();
    }

    /// F-88：血缘面板（左字段、右上/下游证据）。
    pub(crate) fn render_lineage_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let Some(panel) = self.lineage.clone() else {
            return div().into_any_element();
        };
        let header = h_flex()
            .h(px(30.))
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::ArrowLeftRight)
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
                        "字段血缘 · {}",
                        panel.table.display_label()
                    ))),
            )
            .child(
                div()
                    .id("lineage-close")
                    .debug_selector(|| "lineage-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.lineage = None;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(IconName::Close)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );

        let label = |text: &str| {
            div()
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(t.fg_muted))
                .child(SharedString::from(text.to_string()))
        };

        let needle = panel.filter.read(cx).value().to_string();
        let mut left = v_flex().w(px(200.)).flex_none().gap_1();
        left = left.child(label("字段")).child(Input::new(&panel.filter));
        let mut columns = v_flex()
            .gap(px(1.))
            .max_h(px(300.))
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        let shown: Vec<String> = panel
            .columns
            .iter()
            .filter(|column| {
                needle.trim().is_empty()
                    || column
                        .to_lowercase()
                        .contains(&needle.trim().to_lowercase())
            })
            .cloned()
            .collect();
        if shown.is_empty() {
            columns = columns.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(if panel.loading {
                        "正在读取字段…"
                    } else {
                        "（没有字段）"
                    }),
            );
        }
        for (index, name) in shown.iter().enumerate() {
            let active = panel.selected.as_deref() == Some(name.as_str());
            let click = name.clone();
            columns = columns.child(
                div()
                    .id(("lineage-col", index))
                    .debug_selector(move || format!("lineage-col-{index}"))
                    .h(px(21.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(active, |row| row.bg(hex(t.bg_overlay)))
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.select_lineage_column(click.clone(), cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_ellipsis()
                            .child(SharedString::from(name.clone())),
                    ),
            );
        }
        left = left.child(columns);

        let mut right = v_flex().flex_1().min_w_0().min_h_0().gap_1();
        let mut chips = h_flex().gap_1().items_center();
        chips = chips.child(label("置信度"));
        for (chip_index, level) in rustdb_core::Confidence::ALL.into_iter().rev().enumerate() {
            let active = panel.min == level;
            let slug = match level {
                rustdb_core::Confidence::Certain => "certain",
                rustdb_core::Confidence::High => "high",
                rustdb_core::Confidence::Possible => "possible",
            };
            chips = chips.child(
                div()
                    .id(("lineage-min", chip_index))
                    .debug_selector(move || format!("lineage-min-{slug}"))
                    .h(px(18.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(hex(if active { t.accent } else { t.border_subtle }))
                    .when(active, |chip| chip.bg(hex(t.bg_overlay)))
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(if active { t.accent } else { t.fg_muted }))
                    .on_click(cx.listener(move |view, _, _, cx| view.set_lineage_min(level, cx)))
                    .child(SharedString::from(format!("{} 及以上", level.label()))),
            );
        }
        right = right.child(chips);

        if panel.loading {
            right = right.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child("正在汇总外键、视图与历史 SQL…"),
            );
        }
        if let Some(error) = &panel.error {
            right = right.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.danger))
                    .child(SharedString::from(error.clone())),
            );
        }
        if let Some(input) = panel.input.as_ref() {
            let skipped_tables = input.skipped_tables + input.failed_tables;
            let skipped_views = input.skipped_views + input.failed_views;
            if skipped_tables + skipped_views + input.skipped_queries > 0 {
                right = right.child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.warning))
                        .child(SharedString::from(format!(
                            "已跳过：表 {skipped_tables} / 视图 {skipped_views} / 历史 SQL {}",
                            input.skipped_queries
                        ))),
                );
            }
        }
        let mut edges_area = v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .overflow_y_scrollbar()
            .scroll_bounce(false);
        match panel.report.as_ref() {
            Some(report) => {
                for warning in &report.warnings {
                    edges_area = edges_area.child(
                        div()
                            .text_size(px(crate::design::TS_CAPTION))
                            .text_color(hex(t.warning))
                            .child(SharedString::from(format!("警告：{warning}"))),
                    );
                }
                let mut edge_seq = 0usize;
                for (title, direction, edges) in [
                    ("上游", "up", &report.upstream),
                    ("下游", "down", &report.downstream),
                ] {
                    let filtered = rustdb_core::filter_lineage_edges(edges, panel.min, &needle);
                    edges_area = edges_area.child(
                        div()
                            .debug_selector(move || format!("lineage-{direction}-count"))
                            .text_size(px(crate::design::TS_CAPTION))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hex(t.fg_secondary))
                            .child(SharedString::from(format!(
                                "{title} · {} 条",
                                filtered.len()
                            ))),
                    );
                    if filtered.is_empty() {
                        edges_area = edges_area.child(
                            div()
                                .text_size(px(crate::design::TS_CAPTION))
                                .text_color(hex(t.fg_muted))
                                .child("（无）"),
                        );
                        continue;
                    }
                    for (index, edge) in filtered.iter().enumerate() {
                        let target = if direction == "up" {
                            edge.from.clone()
                        } else {
                            edge.to.clone()
                        };
                        let row_label = if direction == "up" {
                            edge.from_label()
                        } else {
                            edge.to_label()
                        };
                        let color = match edge.confidence {
                            rustdb_core::Confidence::Certain => t.success,
                            rustdb_core::Confidence::High => t.accent,
                            rustdb_core::Confidence::Possible => t.fg_muted,
                        };
                        // 证据会把同一对字段的多次命中拼在一起，全量铺开会淹没列表。
                        let evidence_items: Vec<&str> = edge.evidence.split('；').collect();
                        let shown_evidence = 3usize.min(evidence_items.len());
                        let hidden_evidence = evidence_items.len().saturating_sub(shown_evidence);
                        let mut evidence_lines: Vec<String> = evidence_items
                            .iter()
                            .take(shown_evidence)
                            .map(|item| {
                                let mut text: String = item.trim().chars().take(120).collect();
                                if item.trim().chars().count() > 120 {
                                    text.push('…');
                                }
                                text
                            })
                            .collect();
                        if hidden_evidence > 0 {
                            evidence_lines.push(format!("另 {hidden_evidence} 条证据"));
                        }
                        edge_seq += 1;
                        let mut row = div()
                            .id(("lineage-edge", edge_seq))
                            .debug_selector(move || format!("lineage-edge-{direction}-{index}"))
                            .px_2()
                            .py_1()
                            .gap(px(1.))
                            .flex()
                            .flex_col()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(hex(t.border_subtle))
                            .hover(|style| style.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                let target = target.clone();
                                view.lineage = None;
                                view.preview_object(target, 100, window, cx);
                            }))
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_secondary))
                                    .text_ellipsis()
                                    .child(SharedString::from(row_label)),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(color))
                                    .text_ellipsis()
                                    .child(SharedString::from(format!(
                                        "{} · 置信度 {}",
                                        edge.kind.label(),
                                        edge.confidence.label()
                                    ))),
                            );
                        for (line_index, line) in evidence_lines.iter().enumerate() {
                            let muted =
                                hidden_evidence > 0 && line_index + 1 == evidence_lines.len();
                            row = row.child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(if muted { t.fg_muted } else { color }))
                                    .text_ellipsis()
                                    .child(SharedString::from(line.clone())),
                            );
                        }
                        edges_area = edges_area.child(row);
                    }
                }
            }
            None if !panel.loading => {
                edges_area = edges_area.child(
                    div()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_muted))
                        .child("请选择一个字段查看血缘"),
                );
            }
            None => {}
        }

        right = right.child(edges_area);
        let footer = h_flex()
            .gap_2()
            .justify_end()
            .child(label("点行打开对象"))
            .child(
                Button::new("lineage-close-btn")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.lineage = None;
                        cx.notify();
                    })),
            );

        v_flex()
            .id("lineage-panel")
            .debug_selector(|| "lineage-panel".to_string())
            .occlude()
            .w(px(820.))
            .h(px(520.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .gap_3()
                    .child(left)
                    .child(right),
            )
            .child(footer)
            .into_any_element()
    }
}
