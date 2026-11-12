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
}
