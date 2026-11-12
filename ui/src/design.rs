//! UI 设计 Token：TypeScale 与可复跑护栏。
//!
//! 颜色 Token 冻结在 `rustdb_core::theme`（基础层 + 扩展层）；本模块只负责排版层级。
//! 业务代码禁止写死 `text_size(px(数字))`，一律经 `TS_*` 常量取字号。
//! 规范与验收口径见 `docs/UI-设计规范.md` 第 3、9 节。

/// 辅助文字 / 徽标 / 页脚 / 时间戳。下限 12px，低于此值不满足可读性。
pub const TS_CAPTION: f32 = 12.;
/// 正文 / 表格单元格 / 菜单项 / 按钮标签。
pub const TS_BODY: f32 = 13.;
/// 面板标题 / 区块标题，与组件库 rem 基准一致。
pub const TS_BASE: f32 = 15.;
/// 对话框 / 空态主标题。
pub const TS_TITLE: f32 = 18.;

/// 统一空态：32px 图标区 + 13px 说明 + 12px 快捷键提示（规范 §5）。
pub fn empty_state(
    cx: &gpui::App,
    icon: gpui_kit_assets::IconName,
    message: impl Into<gpui::SharedString>,
    hint: impl Into<gpui::SharedString>,
) -> gpui::AnyElement {
    use gpui::{div, px, IntoElement as _, ParentElement as _, Styled as _};
    use gpui_component::{v_flex, Icon, Sizable as _, Size};

    let t = crate::theme::current(cx);
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .w(px(32.))
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(icon)
                        .with_size(Size::Large)
                        .text_color(crate::theme::to_hsla(t.fg_muted)),
                ),
        )
        .child(
            div()
                .text_size(px(TS_BODY))
                .text_color(crate::theme::to_hsla(t.fg_secondary))
                .child(message.into()),
        )
        .child(
            div()
                .text_size(px(TS_CAPTION))
                .text_color(crate::theme::to_hsla(t.fg_muted))
                .child(hint.into()),
        )
        .into_any_element()
}

/// 骨架条：用低对比中性色，不做循环动画（规范 §5 / §7）。
pub fn skeleton_bar(cx: &gpui::App, width: f32, height: f32) -> gpui::AnyElement {
    use gpui::{div, px, IntoElement as _, Styled as _};

    let t = crate::theme::current(cx);
    div()
        .w(px(width))
        .h(px(height))
        .rounded(px(6.))
        .bg(crate::theme::to_hsla(t.fg_muted).opacity(0.18))
        .into_any_element()
}

/// 列表骨架：默认 3 行（网格可传 8 行）。
pub fn skeleton_rows(cx: &gpui::App, count: usize, row_h: f32) -> gpui::AnyElement {
    use gpui::{div, px, IntoElement as _, ParentElement as _, Styled as _};
    use gpui_component::v_flex;

    let t = crate::theme::current(cx);
    let mut list = v_flex().w_full().gap_2().p_2();
    for _ in 0..count {
        list = list.child(
            div()
                .w_full()
                .h(px(row_h))
                .rounded(px(6.))
                .bg(crate::theme::to_hsla(t.fg_muted).opacity(0.18)),
        );
    }
    list.into_any_element()
}

/// 加载态延迟：短于 400ms 的请求不显示骨架，避免快速响应闪烁（规范 §5）。
pub const LOADING_DELAY: std::time::Duration = std::time::Duration::from_millis(400);
/// 超时且存在旧内容时，旧内容降到该不透明度（不用转圈遮罩）。
pub const STALE_CONTENT_OPACITY: f32 = 0.55;

/// 加载态三种视觉：400ms 内不打扰 / 旧内容降透明度 / 无旧内容时骨架屏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadingVisual {
    #[default]
    Normal,
    Stale,
    Skeleton,
}

/// 一次加载的相位。由既有 80ms 轮询推进，不新建定时器、不逐帧起异步任务。
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadingPhase {
    started_at: Option<std::time::Instant>,
    has_stale_content: bool,
    revealed: bool,
}

impl LoadingPhase {
    pub fn begin(has_stale_content: bool) -> Self {
        Self::begin_at(std::time::Instant::now(), has_stale_content)
    }

    /// 可注入时钟：测试用回拨的 now 精确验证 400ms 边界。
    pub fn begin_at(now: std::time::Instant, has_stale_content: bool) -> Self {
        Self {
            started_at: Some(now),
            has_stale_content,
            revealed: false,
        }
    }

    pub fn finish(&mut self) {
        *self = Self::default();
    }

    pub fn is_loading(&self) -> bool {
        self.started_at.is_some()
    }

    /// 跨过阈值时返回 true，调用方据此触发一次重绘；只触发一次。
    pub fn reveal_if_due(&mut self, now: std::time::Instant) -> bool {
        if self.revealed {
            return false;
        }
        let Some(started) = self.started_at else {
            return false;
        };
        if now.saturating_duration_since(started) < LOADING_DELAY {
            return false;
        }
        self.revealed = true;
        true
    }

    pub fn visual(&self) -> LoadingVisual {
        if !self.revealed {
            return LoadingVisual::Normal;
        }
        if self.has_stale_content {
            LoadingVisual::Stale
        } else {
            LoadingVisual::Skeleton
        }
    }

    pub fn opacity(&self) -> f32 {
        if self.visual() == LoadingVisual::Stale {
            STALE_CONTENT_OPACITY
        } else {
            1.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }
    }

    /// 护栏：源码中不得出现字号字面量，必须走 `TS_*` Token。
    #[test]
    fn no_hardcoded_text_size() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect(&root, &mut files);
        let mut violations = Vec::new();
        for file in &files {
            let text = fs::read_to_string(file).unwrap_or_default();
            for (line_no, line) in text.lines().enumerate() {
                let mut rest = line;
                while let Some(pos) = rest.find("text_size(px(") {
                    let after = &rest[pos + "text_size(px(".len()..];
                    if after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        violations.push(format!(
                            "{}:{} {}",
                            file.display(),
                            line_no + 1,
                            line.trim()
                        ));
                    }
                    rest = after;
                }
            }
        }
        assert!(
            violations.is_empty(),
            "字号字面量必须走 TS_* Token：\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn type_scale_minimum_is_readable() {
        assert!(TS_CAPTION >= 12.);
        assert!(TS_CAPTION < TS_BODY && TS_BODY < TS_BASE && TS_BASE < TS_TITLE);
    }

    #[test]
    fn loading_phase_delays_skeleton_by_400ms() {
        let now = std::time::Instant::now();
        let mut phase = LoadingPhase::begin_at(now, false);
        assert!(phase.is_loading());
        assert_eq!(phase.visual(), LoadingVisual::Normal);
        assert_eq!(phase.opacity(), 1.);
        assert!(!phase.reveal_if_due(now + std::time::Duration::from_millis(399)));
        assert_eq!(phase.visual(), LoadingVisual::Normal);
        assert!(phase.reveal_if_due(now + LOADING_DELAY));
        assert_eq!(phase.visual(), LoadingVisual::Skeleton);
        assert_eq!(phase.opacity(), 1.);
        assert!(!phase.reveal_if_due(now + LOADING_DELAY), "只触发一次重绘");
    }

    #[test]
    fn loading_phase_dims_stale_content_instead_of_skeleton() {
        let now = std::time::Instant::now();
        let mut phase = LoadingPhase::begin_at(now, true);
        assert!(phase.reveal_if_due(now + LOADING_DELAY));
        assert_eq!(phase.visual(), LoadingVisual::Stale);
        assert!((phase.opacity() - STALE_CONTENT_OPACITY).abs() < f32::EPSILON);
    }

    #[test]
    fn loading_phase_finish_resets_for_next_request() {
        let now = std::time::Instant::now();
        let mut phase = LoadingPhase::begin_at(now, false);
        assert!(phase.reveal_if_due(now + LOADING_DELAY));
        phase.finish();
        assert!(!phase.is_loading());
        assert_eq!(phase.visual(), LoadingVisual::Normal);
        assert!(!phase.reveal_if_due(now + std::time::Duration::from_secs(5)));
    }

    #[test]
    fn loading_delay_is_400ms_per_spec() {
        assert_eq!(LOADING_DELAY.as_millis(), 400);
        assert!(STALE_CONTENT_OPACITY > 0.4 && STALE_CONTENT_OPACITY < 1.);
    }
}
