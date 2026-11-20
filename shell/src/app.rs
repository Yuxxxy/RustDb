//! GPUI Application 装配：无边框窗口、主题、Root。

use gpui_kit::{px, size, App, AppContext as _, Bounds, WindowBounds, WindowKind, WindowOptions};
use gpui_kit::component::TitleBar;
use rustdb_core::Core;
use rustdb_ui::MainView;
use std::sync::Arc;

pub fn run(core: Arc<Core>) {
    gpui_kit::platform::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_kit::component::init(cx);
            // 把组件内建占位文案切到中文（设置页搜索框等），避免回落英文。
            gpui_kit::component::set_locale("zh-CN");
            rustdb_ui::theme::apply_theme_mode(cx, rustdb_core::theme::ThemeMode::Dark);
            cx.activate(true);

            let mut window_size = size(px(1200.), px(680.));
            if let Some(display) = cx.primary_display() {
                let display_size = display.bounds().size;
                window_size.width = window_size.width.min(display_size.width * 0.85);
                window_size.height = window_size.height.min(display_size.height * 0.85);
            }
            let bounds = Bounds::centered(None, window_size, cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitleBar::title_bar_options()),
                window_min_size: Some(gpui_kit::Size {
                    width: px(1024.),
                    height: px(640.),
                }),
                kind: WindowKind::Normal,
                #[cfg(target_os = "linux")]
                window_decorations: Some(gpui_kit::WindowDecorations::Client),
                ..Default::default()
            };

            let core_for_window = core.clone();
            let _ = cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| MainView::new(window, cx, core_for_window.clone()));
                cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
            });

            log::info!("主窗口初始化完成");
        });
}
