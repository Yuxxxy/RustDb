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
    colors.border = to_hsla(t.border_subtle);
    colors.accent = to_hsla(t.accent);
    colors.accent_foreground = to_hsla(t.on_accent);
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
    colors.input = to_hsla(t.border_subtle);
    colors.primary = to_hsla(t.accent);
    colors.primary_foreground = to_hsla(t.on_accent);
    colors.ring = to_hsla(t.accent);
    colors.link = to_hsla(t.accent);
    colors.selection = to_hsla(t.accent).opacity(0.35);
    colors.danger = to_hsla(t.danger);
    colors.danger_foreground = to_hsla(t.on_danger);
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
    colors.button_danger_foreground = to_hsla(t.on_danger);

    let fitted = theme.colors;
    theme.tokens = ComponentThemeTokens::from(&fitted);
    theme.font_size = px(15.);
    theme.focus_ring = false;
}
