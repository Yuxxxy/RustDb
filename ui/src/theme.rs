//! 将 core 的 Token 写入 gpui-component 的全局 Theme。

use gpui::{px, rgb, App, Hsla};
use gpui_component::theme::{
    Theme, ThemeMode as ComponentThemeMode, ThemeTokens as ComponentThemeTokens,
};
use rustdb_core::theme::{ThemeMode, ThemeTokens};

pub fn to_hsla(hex: u32) -> Hsla {
    Hsla::from(rgb(hex))
}

pub struct CurrentTheme(pub ThemeTokens, pub bool);

impl gpui::Global for CurrentTheme {}

pub fn current(cx: &App) -> ThemeTokens {
    cx.try_global::<CurrentTheme>()
        .map(|g| g.0)
        .unwrap_or_else(ThemeTokens::dark)
}

pub fn is_dark(cx: &App) -> bool {
    cx.try_global::<CurrentTheme>().map(|g| g.1).unwrap_or(true)
}

pub fn apply_theme_mode(cx: &mut App, mode: ThemeMode) {
    let system_dark = matches!(
        cx.window_appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    );
    let t = ThemeTokens::resolved(mode, system_dark);
    let dark = t.bg_base == ThemeTokens::dark().bg_base;

    cx.set_global(CurrentTheme(t, dark));

    Theme::change(
        if dark {
            ComponentThemeMode::Dark
        } else {
            ComponentThemeMode::Light
        },
        None,
        cx,
    );

    let theme = Theme::global_mut(cx);
    let colors = &mut theme.colors;
    colors.background = to_hsla(t.bg_base);
    colors.foreground = to_hsla(t.fg_primary);
    colors.border = to_hsla(t.border_strong);
    colors.accent = to_hsla(t.accent);
    // gpui-component 的 ghost 悬停底色在深色下是 accent@50%（浅色才是实心 accent），
    // 但文字色两态都取 `accent_foreground`。深色若沿用 `on_accent`(#0D1117) 就与卡片
    // 背景同色，悬停时按钮文字整个消失，所以深色必须改用常规前景色。
    colors.accent_foreground = if dark {
        to_hsla(t.fg_primary)
    } else {
        to_hsla(t.on_accent)
    };
    colors.muted = to_hsla(t.bg_elevated);
    colors.muted_foreground = to_hsla(t.fg_muted);
    colors.secondary = to_hsla(t.bg_elevated);
    colors.secondary_foreground = to_hsla(t.fg_primary);
    colors.secondary_hover = if dark {
        to_hsla(t.bg_overlay)
    } else {
        to_hsla(t.fg_primary).opacity(0.09)
    };
    colors.secondary_active = if dark {
        to_hsla(t.bg_elevated)
    } else {
        to_hsla(t.fg_primary).opacity(0.16)
    };
    colors.title_bar = to_hsla(t.bg_overlay);
    colors.title_bar_border = to_hsla(t.border_subtle);
    colors.tab_bar = to_hsla(t.bg_base);
    colors.tab_bar_segmented = to_hsla(if dark { t.bg_overlay } else { t.bg_elevated });
    colors.tab_foreground = to_hsla(t.fg_secondary);
    colors.tab_active = to_hsla(t.bg_elevated);
    colors.tab_active_foreground = to_hsla(t.fg_primary);
    colors.sidebar = to_hsla(t.bg_elevated);
    colors.sidebar_border = to_hsla(t.border_subtle);
    colors.sidebar_foreground = to_hsla(t.fg_primary);
    colors.sidebar_accent = to_hsla(t.bg_overlay);
    colors.sidebar_accent_foreground = to_hsla(t.fg_primary);
    colors.popover = to_hsla(t.bg_overlay);
    colors.popover_foreground = to_hsla(t.fg_primary);
    colors.input = to_hsla(t.border_strong);
    colors.primary = to_hsla(t.accent);
    colors.primary_foreground = to_hsla(t.on_accent);
    colors.ring = to_hsla(t.accent);
    colors.link = to_hsla(t.accent);
    colors.selection = to_hsla(t.accent).opacity(0.35);
    colors.danger = to_hsla(t.danger);
    colors.danger_foreground = to_hsla(t.on_danger_resolved);
    colors.success = to_hsla(t.success);
    colors.warning = to_hsla(t.warning);
    colors.info = to_hsla(t.accent);
    colors.info_foreground = to_hsla(t.on_accent);
    colors.status_bar = to_hsla(t.bg_elevated);
    colors.status_bar_border = to_hsla(t.border_subtle);
    // hover 比选中更淡：同色会让「扫过列表」看起来像「已选中」。
    colors.list_hover = to_hsla(t.accent).opacity(0.08);
    colors.list_active = to_hsla(t.accent).opacity(0.18);
    colors.list_active_border = to_hsla(t.accent);

    let accent = to_hsla(t.accent);
    let on_accent = to_hsla(t.on_accent);
    let shift = |c: Hsla, dl: f32| Hsla {
        l: (c.l + dl).clamp(0.0, 1.0),
        ..c
    };
    let (acc_hover, acc_active) = if dark {
        (shift(accent, 0.06), shift(accent, 0.12))
    } else {
        (shift(accent, -0.05), shift(accent, -0.10))
    };
    let (dan_hover, dan_active) = if dark {
        (
            shift(to_hsla(t.danger), 0.06),
            shift(to_hsla(t.danger), 0.12),
        )
    } else {
        (
            shift(to_hsla(t.danger), -0.05),
            shift(to_hsla(t.danger), -0.10),
        )
    };
    colors.button_primary = accent;
    colors.button_primary_hover = acc_hover;
    colors.button_primary_active = acc_active;
    colors.button_primary_foreground = on_accent;
    colors.button_danger = to_hsla(t.danger);
    colors.button_danger_hover = dan_hover;
    colors.button_danger_active = dan_active;
    colors.button_danger_foreground = to_hsla(t.on_danger_resolved);

    let fitted = theme.colors;
    theme.tokens = ComponentThemeTokens::from(&fitted);
    theme.font_size = px(crate::design::TS_BASE);
    theme.focus_ring = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(color: Hsla) -> u32 {
        let rgba = gpui::Rgba::from(color);
        let byte = |v: f32| (v * 255.).round() as u32;
        (byte(rgba.r) << 16) | (byte(rgba.g) << 8) | byte(rgba.b)
    }

    fn luminance(color: u32) -> f32 {
        let channel = |shift: u32| {
            let c = ((color >> shift) & 0xFF) as f32 / 255.;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn over(fg: u32, alpha: f32, bg: u32) -> u32 {
        let mix = |shift: u32| {
            let f = ((fg >> shift) & 0xFF) as f32;
            let b = ((bg >> shift) & 0xFF) as f32;
            (f * alpha + b * (1. - alpha)).round() as u32
        };
        (mix(16) << 16) | (mix(8) << 8) | mix(0)
    }

    /// 护栏：gpui-component 的 ghost 悬停文字色固定取 `accent_foreground`，悬停底色深色为
    /// accent@50%、浅色为实心 accent（button.rs hovered 分支）。深色若映射成 `on_accent`
    /// 就与卡片背景同色，悬停时按钮文字整个消失（P1-④ 实测标签区 1917/1920 px 纯背景）。
    #[gpui::test]
    fn ghost_hover_label_keeps_aa_contrast(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| gpui_component::init(cx));
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            cx.update(|cx| apply_theme_mode(cx, mode));
            let (t, dark, fg) = cx.update(|cx| {
                (
                    current(cx),
                    is_dark(cx),
                    hex(Theme::global(cx).colors.accent_foreground),
                )
            });
            for surface in [t.bg_base, t.bg_elevated, t.bg_overlay] {
                let hover_bg = if dark {
                    over(t.accent, 0.5, surface)
                } else {
                    t.accent
                };
                let ratio = contrast(fg, hover_bg);
                assert!(
                    ratio >= 4.5,
                    "{mode:?} 悬停文字 #{fg:06X} 对悬停底 #{hover_bg:06X} 对比度 {ratio:.2} < 4.5"
                );
                // 实测深色悬停区底色就是卡片裸背景，半透明 accent 层不可见，裸底也要达标。
                if dark {
                    let bare = contrast(fg, surface);
                    assert!(
                        bare >= 4.5,
                        "{mode:?} 悬停文字 #{fg:06X} 对裸底 #{surface:06X} 对比度 {bare:.2} < 4.5"
                    );
                }
            }
        }
    }
}
