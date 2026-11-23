//! GPUI Application 装配：无边框窗口、主题、Root。

use gpui_kit::{
    point, px, size, App, AppContext as _, Bounds, PlatformDisplay, WindowBounds, WindowKind,
    WindowOptions,
};
use gpui_kit::component::TitleBar;
use rustdb_core::window_bounds::{pick_screen, ScreenRect, WindowBoundsRecord};
use rustdb_core::Core;
use rustdb_ui::MainView;
use std::sync::Arc;

/// 一块显示器的**可见区域**（逻辑像素）：任务栏 / Dock / 菜单栏已排除。
///
/// 用 `visible_bounds` 而不是 `bounds`：拿整块屏当可用区，窗口会被摆到任务栏底下，
/// 而 Windows 上那一条拖不动 —— 用户只能靠快捷键把窗口救回来。
fn screen_rect_of(display: &dyn PlatformDisplay) -> ScreenRect {
    let b = display.visible_bounds();
    ScreenRect::new(
        f32::from(b.origin.x),
        f32::from(b.origin.y),
        f32::from(b.size.width),
        f32::from(b.size.height),
    )
}

/// 启动时的窗口几何。
///
/// 有记录 ⇒ 还原（挑重叠最大的那块屏，收进它的可见区）。没有记录、或者记录已被
/// 无效句柄污染 ⇒ 「1200×680、屏幕 85%、居中」那套默认行为原样保留。
///
/// 判据只拦明显无效值：正常的小窗（哪怕被边框扣减过）仍按原尺寸恢复。
fn restorable(saved: Option<WindowBoundsRecord>) -> Option<WindowBoundsRecord> {
    saved.filter(|rec| {
        rec.width.is_finite() && rec.height.is_finite() && rec.width > 0.0 && rec.height > 0.0
    })
}

fn resolve_window_bounds(cx: &mut App, saved: Option<WindowBoundsRecord>) -> WindowBounds {
    if let Some(rec) = restorable(saved) {
        let screens: Vec<ScreenRect> = cx
            .displays()
            .iter()
            .map(|d| screen_rect_of(d.as_ref()))
            .collect();
        let home = pick_screen(&screens, &rec).or_else(|| {
            cx.primary_display()
                .map(|d| screen_rect_of(d.as_ref()))
                .or_else(|| screens.first().copied())
        });
        if let Some(screen) = home {
            let fitted = rec.fitted_to(screen);
            let bounds = Bounds::new(
                point(px(fitted.x), px(fitted.y)),
                size(px(fitted.width), px(fitted.height)),
            );
            log::info!(
                "窗口几何：按上次记录恢复（{} 屏）。{:.0},{:.0} {:.0}x{:.0}{}",
                screens.len(),
                fitted.x,
                fitted.y,
                fitted.width,
                fitted.height,
                if fitted.maximized { "，最大化" } else { "" }
            );
            return if fitted.maximized {
                // 最大化态带的是**还原尺寸**：取消最大化时窗口回到这个大小。
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
        }
        // 一块屏都问不到（无头会话、远程桌面断开）：走默认分支。
    }

    let mut window_size = size(px(1200.), px(680.));
    if let Some(display) = cx.primary_display() {
        let display_size = display.bounds().size;
        window_size.width = window_size.width.min(display_size.width * 0.85);
        window_size.height = window_size.height.min(display_size.height * 0.85);
    }
    WindowBounds::Windowed(Bounds::centered(None, window_size, cx))
}

pub fn run(core: Arc<Core>) {
    gpui_kit::platform::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_kit::component::init(cx);
            // 把组件内建占位文案切到中文（设置页搜索框等），避免回落英文。
            gpui_kit::component::set_locale("zh-CN");
            rustdb_ui::theme::apply_theme_mode(cx, rustdb_core::theme::ThemeMode::Dark);
            cx.activate(true);

            // 开窗前就算好几何：作为 WindowOptions 传入，避免先居中再跳到恢复位置的闪烁。
            let layout_path = rustdb_core::layout_path(&rustdb_platform::paths::data_dir());
            let saved = rustdb_core::load_layout(&layout_path).window_bounds;
            let window_bounds = resolve_window_bounds(cx, saved);

            let options = WindowOptions {
                window_bounds: Some(window_bounds),
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
            let quit_slot: std::rc::Rc<
                std::cell::RefCell<Option<gpui_kit::Entity<MainView>>>,
            > = Default::default();
            let slot = quit_slot.clone();
            let _ = cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| MainView::new(window, cx, core_for_window.clone()));
                *slot.borrow_mut() = Some(view.clone());
                cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
            });
            if let Some(view) = quit_slot.borrow().clone() {
                rustdb_ui::main_view::ui_install_flush_window_bounds_on_quit(cx, &view);
            }

            log::info!("主窗口初始化完成");
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无效句柄写出的 0×0 / NaN 记录一律当「没记过」，
    /// 否则 `fitted_to` 会把它钳成最小窗，用户每次启动都看到小窗。
    #[test]
    fn corrupted_window_bounds_are_ignored_at_startup() {
        for bad in [
            WindowBoundsRecord::new(0.0, 0.0, 0.0, 0.0, false),
            WindowBoundsRecord::new(1.0, 1.0, -1200.0, -680.0, false),
            WindowBoundsRecord::new(1.0, 1.0, f32::NAN, 680.0, false),
            WindowBoundsRecord::new(1.0, 1.0, f32::INFINITY, 680.0, false),
        ] {
            assert_eq!(restorable(Some(bad)), None, "脏记录不该交给恢复逻辑: {bad:?}");
        }
        let good = WindowBoundsRecord::new(120.0, 80.0, 1300.0, 860.0, true);
        assert_eq!(restorable(Some(good)), Some(good), "正常记录必须还原");
    }

    /// 接线护栏：读记录 → 开窗前算好 → 退出落盘，三条缺一条功能都等于没做。
    #[test]
    fn startup_restores_and_persists_through_the_expected_entry_points() {
        let src = include_str!("app.rs");
        // 先切掉测试模块，避免断言在自身源码里自匹配假绿。
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(
            production.contains("resolve_window_bounds(cx, saved)"),
            "启动时没读持久化几何"
        );
        assert!(
            production.contains("window_bounds: Some(window_bounds)"),
            "恢复的几何没传给开窗选项"
        );
        assert!(
            production.contains("ui_install_flush_window_bounds_on_quit(cx, &view)"),
            "退出没挂落盘钩子：拖完立刻关窗的那次调整会丢"
        );

        // 拔掉外接屏后记录必须回到主屏正中，而不是留在屏幕外。
        let rec = WindowBoundsRecord::new(3000.0, 200.0, 1300.0, 860.0, false);
        let fitted = rec.fitted_to(ScreenRect::new(0.0, 0.0, 1920.0, 1040.0));
        assert_eq!(fitted.x, (1920.0 - 1300.0) / 2.0);
        assert_eq!(fitted.y, (1040.0 - 860.0) / 2.0);
    }
}
