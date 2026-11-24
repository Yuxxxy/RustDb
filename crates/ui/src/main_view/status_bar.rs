//! 状态栏与自定义标题栏。

use super::*;

impl MainView {
    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        let (dot, label) = match self.status_kind {
            StatusKind::Idle => (t.fg_muted, "空闲"),
            StatusKind::Busy => (t.warning, "执行中"),
            StatusKind::Ok => (t.success, "就绪"),
            StatusKind::Warn => (t.warning, "注意"),
            StatusKind::Error => (t.danger, "错误"),
        };
        let mut bar = h_flex()
            .id("status-bar")
            .debug_selector(|| "status-bar".to_string())
            .h(px(STATUS_BAR_HEIGHT))
            .flex_none()
            .w_full()
            .px_2()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(hex(t.border_strong))
            .bg(hex(t.bg_elevated))
            .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(hex(dot)))
            .child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_secondary))
                    .child(SharedString::from(label)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .text_ellipsis()
                    .child(SharedString::from(self.status.clone())),
            );
        // P2：常驻字段——连接 / 事务 / 行数 / 耗时 / 只读（规范 §5 状态栏）。
        let fields = self.status_field_labels();
        bar = bar.child(
            status_field("status-conn", fields.conn, fields.conn_tip, t.fg_secondary)
                .max_w(px(140.))
                .text_ellipsis(),
        );
        let tx_color = if fields.tx_active { t.warning } else { t.fg_muted };
        bar = bar
            .child(status_field("status-tx", fields.tx, fields.tx_tip, tx_color))
            .child(status_field("status-rows", fields.rows, fields.rows_tip, t.fg_muted))
            .child(status_field(
                "status-elapsed",
                fields.elapsed,
                "最近一次查询的耗时".to_string(),
                t.fg_muted,
            ));
        if fields.read_only {
            bar = bar.child(
                status_field(
                    "status-readonly",
                    "只读".to_string(),
                    "连接配置为只读：写入语句会被拒绝".to_string(),
                    t.fg_muted,
                )
                .px_1()
                .rounded(px(3.))
                .border_1()
                .border_color(hex(t.border_strong)),
            );
        }
        // F-89：生产连接常驻提示；解锁中显示剩余时间。
        let active = self.active_production_profile();
        if let Some((profile_id, true)) = active {
            bar = bar.child(
                div()
                    .debug_selector(|| "status-production".to_string())
                    .px_1()
                    .rounded(px(3.))
                    .bg(hex(t.danger))
                    .text_size(px(crate::design::TS_CAPTION))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(hex(t.on_danger_resolved))
                    .child("生产"),
            );
            if let Some(left) = self.core.write_unlock_remaining(&profile_id) {
                let secs = left.as_secs();
                bar = bar.child(
                    div()
                        .debug_selector(|| "status-unlock".to_string())
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.success))
                        .child(SharedString::from(format!(
                            "写已解锁 {:02}:{:02}",
                            secs / 60,
                            secs % 60
                        ))),
                );
            }
        }
        if let Some((remaining, total)) = self.resumable() {
            bar = bar.child(
                div().debug_selector(|| "resume-batch".to_string()).child(
                    Button::new("resume-batch-btn")
                        .small()
                        .ghost()
                        .label(SharedString::from(format!("继续 · {remaining}/{total}")))
                        .tooltip("从失败语句之后继续执行，已提交语句不回滚")
                        .on_click(cx.listener(|view, _, _, cx| view.resume_remaining(cx))),
                ),
            );
        }
        if let Some(mark) = self.active().and_then(|tab| tab.sql_error.clone()) {
            let text = self.sql_input.read(cx).value().to_string();
            let line = rustdb_sql_lang::position::LineIndex::new(&text)
                .line_col(&text, mark.start)
                .0;
            bar = bar
                .child(
                    div().debug_selector(|| "goto-error".to_string()).child(
                        Button::new("goto-error-btn")
                            .small()
                            .ghost()
                            .label(SharedString::from(format!("跳到错误 · 第 {line} 行")))
                            .tooltip(mark.message)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.jump_to_error(window, cx)),
                            ),
                    ),
                )
                .child(
                    div().debug_selector(|| "ai-fix-error".to_string()).child(
                        Button::new("ai-fix-error-btn")
                            .small()
                            .ghost()
                            .label("AI 修复")
                            .tooltip("把出错语句和报错交给 AI 改写，先看建议再应用")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.ai_fix_error(window, cx)),
                            ),
                    ),
                );
        }
        bar
    }

    fn active_production_profile(&self) -> Option<(String, bool)> {
        self.active_profile
            .as_deref()
            .and_then(|id| self.profiles.iter().find(|p| p.id == id))
            .map(|p| (p.id.clone(), p.production))
    }

    /// P2：常驻字段的文案与 tooltip（纯计算，便于无头测试断言文本）。
    pub(crate) fn status_field_labels(&self) -> StatusFieldLabels {
        let profile = self
            .active_profile
            .as_deref()
            .and_then(|id| self.profiles.iter().find(|p| p.id == id));
        let (conn, conn_tip) = match profile {
            Some(p) => {
                let mut tip = format!("{} · {}", p.name, p.driver.display_name());
                if let Some(host) = p.host.as_deref().filter(|h| !h.is_empty()) {
                    tip.push_str(" · ");
                    tip.push_str(host);
                    if let Some(port) = p.port {
                        tip.push_str(&format!(":{port}"));
                    }
                }
                if let Some(db) = p.database.as_deref().filter(|d| !d.is_empty()) {
                    tip.push_str(" · ");
                    tip.push_str(db);
                }
                (p.name.clone(), tip)
            }
            None => ("未连接".to_string(), "还没有活动连接".to_string()),
        };
        let (tx, tx_tip, tx_active) = match self.tx.as_ref() {
            Some(tx) => {
                let secs = tx.started.elapsed().as_secs();
                (
                    format!("事务 · {} 条 · {:02}:{:02}", tx.statements, secs / 60, secs % 60),
                    "手动事务：语句不会自动提交，请在工具栏提交或回滚".to_string(),
                    true,
                )
            }
            None => (
                "自动提交".to_string(),
                "每条语句执行后自动提交".to_string(),
                false,
            ),
        };
        let result = self
            .active()
            .and_then(|tab| tab.results.get(tab.active_result));
        let (rows, rows_tip) = match result {
            Some(rv) => match rv.affected {
                Some(affected) => (
                    format!("影响 {affected} 行"),
                    "写入语句影响的行数".to_string(),
                ),
                None if rv.truncated => (
                    format!("{} 行 · 截断", rv.rows.len()),
                    format!("结果过大，仅加载前 {} 行", rv.rows.len()),
                ),
                None => (
                    format!("{} 行", rv.rows.len()),
                    "当前结果集已加载的行数".to_string(),
                ),
            },
            None => ("无结果".to_string(), "当前标签页还没有结果集".to_string()),
        };
        let elapsed = match result {
            Some(rv) if rv.elapsed_ms < 1000 => format!("{} ms", rv.elapsed_ms),
            Some(rv) => format!("{:.2} s", rv.elapsed_ms as f64 / 1000.),
            None => "—".to_string(),
        };
        StatusFieldLabels {
            conn,
            conn_tip,
            tx,
            tx_tip,
            tx_active,
            rows,
            rows_tip,
            elapsed,
            read_only: profile.map(|p| p.read_only).unwrap_or(false),
        }
    }

    pub(crate) fn render_title_bar(&self, cx: &App) -> impl IntoElement {
        let t = current(cx);
        TitleBar::new()
            .h(px(TITLE_BAR_HEIGHT))
            .border_b_0()
            // 外框（标题栏 + 竖栏）比内容卡片亮一档，与 RustShell 的壳 / 内容层次一致。
            .bg(hex(t.bg_elevated))
            .text_size(px(crate::design::TS_BASE))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_BASE))
                                    .font_weight(gpui_kit::FontWeight::BOLD)
                                    .text_color(hex(t.accent))
                                    .child("RustDb"),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_muted))
                                    .child(SharedString::from(format!(
                                        "v{}",
                                        rustdb_core::APP_VERSION
                                    ))),
                            ),
                    )
                    .child(div().flex_1()),
            )
    }
}

/// P2：状态栏常驻字段的文案快照。
pub(crate) struct StatusFieldLabels {
    pub conn: String,
    pub conn_tip: String,
    pub tx: String,
    pub tx_tip: String,
    pub tx_active: bool,
    pub rows: String,
    pub rows_tip: String,
    pub elapsed: String,
    pub read_only: bool,
}

/// P2：状态栏常驻字段——12px、不被压缩，悬停 tooltip 显示完整说明。
fn status_field(
    selector: &'static str,
    label: String,
    tip: String,
    color: u32,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(selector)
        .debug_selector(move || selector.to_string())
        .flex_none()
        .text_size(px(crate::design::TS_CAPTION))
        .text_color(hex(color))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(SharedString::from(tip.clone()))
                .build(window, cx)
        })
        .child(SharedString::from(label))
}
