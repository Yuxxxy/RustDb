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
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(t.fg_muted))
                    .child(SharedString::from(self.status.clone())),
            )
            .child(div().flex_1());
        // F-89：生产连接常驻提示；解锁中显示剩余时间。
        let active = self
            .active_profile
            .as_deref()
            .and_then(|id| self.profiles.iter().find(|p| p.id == id))
            .map(|p| (p.id.clone(), p.production));
        if let Some((profile_id, true)) = active {
            bar = bar.child(
                div()
                    .debug_selector(|| "status-production".to_string())
                    .px_1()
                    .rounded(px(3.))
                    .bg(hex(t.danger))
                    .text_size(px(crate::design::TS_CAPTION))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
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
                                    .font_weight(gpui::FontWeight::BOLD)
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
