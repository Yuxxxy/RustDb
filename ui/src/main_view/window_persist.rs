//! 窗口几何的采集与落盘（移植自 RustShell 的 `ui/src/main_view/window_persist.rs`）。
//!
//! 与布局持久化同一手法，只是对象换成了窗口本身：
//! - `observe_window_bounds` 只把**最新值**记在内存里；
//! - `poll` 里比对 + 2s 节流写进 `layout.json`；
//! - 退出时 `on_app_quit` 兜底 flush 一次（拖完最后一秒就关窗的那次改动还在内存里）。
//!
//! 为什么不直接在 resize 回调里落盘：拖窗口边缘时那个回调每秒触发几十次，每次都
//! 序列化整份 `layout.json` 并写盘，拖拽会发涩；写失败还会在拖拽途中刷一屏日志。
//! 2s 的代价最多是「拖完立刻断电」丢一次调整 —— 而那种情况下窗口本来就还没定性。
//!
//! 几何的钳制 / 多屏挑选在 `core::window_bounds`（纯函数 + 单测）。本模块只负责
//! 「从 `Window` 读出来」和「什么时候写进去」。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::{App, Context, Entity, Window};
use rustdb_core::window_bounds::{WindowBoundsRecord, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};

use super::MainView;

/// 平台层读出来的几何是否可信。
///
/// Windows 的 `gpui-fast-windows` 在 `GetWindowPlacement` 失败时只记日志，然后照样
/// 把全 0 的 `WINDOWPLACEMENT` 算成窗口矩形（`showCmd=0` ⇒ 判成「未最大化」）。
/// 窗口销毁 / 覆盖安装重启那一刻，`render` 的每帧兜底读取会撞上无效句柄，于是
/// `read_window()` 拿到 `0×0` 并写进 `layout.json`。下次启动 `fitted_to` 把它钳到
/// 最小尺寸并居中 —— 用户看到的就是「每次启动都变成最小的小窗，尺寸完全没记住」。
///
/// 判据**不用** `MIN_WINDOW_*` 做硬下限：gpui 会从 `rcNormalPosition` 里扣掉窗口
/// 边框，边界情况下合法尺寸可能略小于最小值。这里只拦「明显是失败占位」的值
/// （0 / 负 / 非有限 / 小于半个最小尺寸），正常窗口一律放行。
fn is_plausible(rec: WindowBoundsRecord) -> bool {
    let finite =
        rec.x.is_finite() && rec.y.is_finite() && rec.width.is_finite() && rec.height.is_finite();
    finite && rec.width >= MIN_WINDOW_WIDTH * 0.5 && rec.height >= MIN_WINDOW_HEIGHT * 0.5
}

/// 窗口几何两次落盘之间的最小间隔（毫秒）。
const SAVE_MIN_GAP_MS: u64 = 2_000;

/// 窗口几何的跨帧状态。`Default` 即可直接用。
#[derive(Default)]
pub(crate) struct WindowMemory {
    /// 内存里的最新几何。`None` = 本次启动还没观察到窗口（退出时不必写）。
    current: Option<WindowBoundsRecord>,
    /// 上次写进文件的**那一份**：脏检测拿它比对，而不是在每个改动点打标记
    /// —— 错过一处的症状是「尺寸停在旧值」，只有重启才看得见。
    saved: Option<WindowBoundsRecord>,
    /// 最早允许落盘的时刻（unix 毫秒；`0` = 立即）。
    save_at_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 从窗口读当前几何。
///
/// **最大化 / 全屏**都由 `WindowBounds` 自己带着还原尺寸（`get_bounds()` 三种态都返回
/// 那个尺寸），所以这里只额外记一个布尔：下次启动以最大化状态打开，取消最大化时
/// 窗口回到用户上次拖出来的大小，而不是系统随手给的一个尺寸。
///
/// 全屏（`Fullscreen`）按「非最大化」记：用户退出时正在全屏，下次以窗口态回到那个
/// 尺寸是更符合预期的。
fn read_window(window: &Window) -> WindowBoundsRecord {
    let bounds = window.window_bounds();
    let rect = bounds.get_bounds();
    WindowBoundsRecord::new(
        f32::from(rect.origin.x),
        f32::from(rect.origin.y),
        f32::from(rect.size.width),
        f32::from(rect.size.height),
        matches!(bounds, gpui_kit::WindowBounds::Maximized(_)),
    )
}

impl MainView {
    /// 窗口几何变了（gpui 在每次 resize / 移动 / 最大化切换后回调）。
    ///
    /// 只更新内存、**不 notify**：窗口尺寸变化本身就会重绘整帧，为了「记住尺寸」
    /// 再标一次脏，等于给每次拖拽多加一帧。
    pub(crate) fn window_bounds_observed(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        let rec = read_window(window);
        // 窗口句柄已失效时平台层给的是 0×0 占位值。丢掉它，让 `current` 保持在
        // **最后一次有效**几何上：退出钩子随后写的仍是那一份。
        if !is_plausible(rec) {
            return;
        }
        self.window_mem.current = Some(rec);
    }

    /// 到点就把窗口几何写进 `layout.json`（`poll` 每轮调）。
    pub(crate) fn window_poll(&mut self) {
        let Some(cur) = self.window_mem.current else {
            return;
        };
        if self.window_mem.saved == Some(cur) {
            return;
        }
        let now = now_ms();
        // 节流窗口只在「干净 → 脏」那一次定下时刻：拖拽途中不顺延，否则拖得越久
        // 落盘越晚。
        if self.window_mem.save_at_ms == 0 {
            self.window_mem.save_at_ms = now.saturating_add(SAVE_MIN_GAP_MS);
        }
        if now < self.window_mem.save_at_ms {
            return;
        }
        let path = self.layout_path.clone();
        self.window_flush(&path);
    }

    /// 退出钩子：不等节流，立刻写一次。
    pub(crate) fn window_flush_on_quit(&mut self) {
        let path = self.layout_path.clone();
        self.window_flush(&path);
    }

    /// 落盘本体。`path` 是参数而不是写死：用例要把它导到临时目录。
    ///
    /// 返回是否真的写了 —— 调用方**不该**据此 `cx.notify()`：写文件不改任何被渲染
    /// 的东西，为一个设置项重绘整棵树不值。
    pub(crate) fn window_flush(&mut self, path: &Path) -> bool {
        let Some(cur) = self.window_mem.current else {
            return false;
        };
        // 双保险：即便有别处把脏值塞进 `current`，也绝不让它落到用户配置里。
        if !is_plausible(cur) {
            return false;
        }
        if self.window_mem.saved == Some(cur) {
            return false;
        }
        self.layout.window_bounds = Some(cur);
        match rustdb_core::save_layout(path, &self.layout) {
            Ok(()) => {
                self.window_mem.saved = Some(cur);
                self.window_mem.save_at_ms = 0;
                true
            }
            Err(error) => {
                // 写失败（磁盘满 / 目录只读）就保持脏、下一个节流窗再试：
                // 静默丢掉用户的窗口尺寸，比多试几次糟得多。
                self.window_mem.save_at_ms = now_ms().saturating_add(SAVE_MIN_GAP_MS);
                log::warn!("窗口几何落盘失败（下轮重试）: {error}");
                false
            }
        }
    }
}

/// 退出前把窗口几何落盘（`shell/src/app.rs` 在开窗后挂上）。
///
/// `on_app_quit` 给处理器 100ms：一次 `layout.json` 写只有几百字节，远在这个量级内。
pub fn ui_install_flush_window_bounds_on_quit(cx: &App, view: &Entity<MainView>) {
    let view = view.clone();
    cx.on_app_quit(move |cx| {
        view.update(cx, |main, _cx| main.window_flush_on_quit());
        async {}
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_view::tests_helpers::{temp_layout, test_view};

    /// 落盘 → 读回：写进去的必须是 `layout.window_bounds`，且内容就是内存里那一份。
    ///
    /// 只断言「内存里有值」是不够的：`save_layout` 写的是整个 `LayoutSettings`，
    /// 字段没进 serde（被 `skip` 或拼错）时内存断言照样绿，而用户重启后窗口回到
    /// 默认尺寸 —— 那正是这个功能唯一的失败模式。
    #[gpui_kit::test]
    fn flush_persists_the_window_geometry(cx: &mut gpui_kit::TestAppContext) {
        let (vcx, holder) = test_view(cx);
        let view = holder.borrow().clone().expect("view 未创建");
        let path = temp_layout("window-flush");
        let rec = WindowBoundsRecord::new(120.0, 80.0, 1300.0, 860.0, true);

        vcx.update(|_, app| {
            view.update(app, |main, _cx| {
                main.window_mem.current = Some(rec);
                assert!(main.window_flush(&path), "有改动却没写");
                assert!(!main.window_flush(&path), "没改动还重写了一次文件");
            });
        });

        let back = rustdb_core::load_layout(&path);
        assert_eq!(back.window_bounds, Some(rec), "落盘的不是内存里那一份几何");
        assert!(back.window_bounds.expect("无记录").maximized);
        let _ = std::fs::remove_file(&path);
    }

    /// `GetWindowPlacement` 失败时平台层返回全 0 的矩形，这类值绝不能被当成
    /// 「用户把窗口缩到了最小」记住 —— 那正是「每次启动都是小窗」的成因。
    #[gpui_kit::test]
    fn flush_refuses_implausible_geometry(cx: &mut gpui_kit::TestAppContext) {
        let (vcx, holder) = test_view(cx);
        let view = holder.borrow().clone().expect("view 未创建");
        let path = temp_layout("window-bad");

        vcx.update(|_, app| {
            view.update(app, |main, _cx| {
                main.window_mem.current = Some(WindowBoundsRecord::new(0.0, 0.0, 0.0, 0.0, false));
                assert!(!main.window_flush(&path), "0×0 不该被落盘");
                assert!(!path.exists(), "不该为一份脏几何写出 layout.json");

                main.window_mem.current =
                    Some(WindowBoundsRecord::new(120.0, 80.0, 1300.0, 860.0, true));
                assert!(main.window_flush(&path), "恢复有效几何后要能正常落盘");
            });
        });

        let back = rustdb_core::load_layout(&path);
        assert_eq!(back.window_bounds.map(|r| r.width), Some(1300.0));
        let _ = std::fs::remove_file(&path);
    }

    /// 判据本身：0 / 负 / NaN / INF / 1×1 拒绝，正常小窗与负坐标放行。
    #[test]
    fn implausible_geometry_is_rejected() {
        for bad in [
            WindowBoundsRecord::new(0.0, 0.0, 0.0, 0.0, false),
            WindowBoundsRecord::new(100.0, 100.0, -1024.0, -640.0, false),
            WindowBoundsRecord::new(100.0, 100.0, f32::NAN, 800.0, false),
            WindowBoundsRecord::new(100.0, 100.0, f32::INFINITY, 800.0, false),
            WindowBoundsRecord::new(100.0, 100.0, 1.0, 1.0, false),
        ] {
            assert!(!is_plausible(bad), "无效几何不该被记住: {bad:?}");
        }
        for good in [
            WindowBoundsRecord::new(120.0, 80.0, 1300.0, 860.0, true),
            WindowBoundsRecord::new(600.0, 336.0, 1024.0, 640.0, false),
            WindowBoundsRecord::new(-8.0, -8.0, 1000.0, 620.0, true),
        ] {
            assert!(is_plausible(good), "正常窗口被误杀: {good:?}");
        }
    }

    /// 接线护栏：过滤只写在函数里不算数，观察与落盘两个入口都必须真的调它。
    #[test]
    fn geometry_filter_is_wired_to_both_entry_points() {
        let me = include_str!("window_persist.rs");
        assert!(me.contains("fn is_plausible"), "有效性判据本身不能丢");

        let observe_at = me
            .find("fn window_bounds_observed")
            .expect("window_bounds_observed 必须存在");
        let observe_end = me[observe_at..]
            .find("fn window_poll(")
            .map(|i| observe_at + i)
            .unwrap_or(me.len());
        assert!(
            me[observe_at..observe_end].contains("is_plausible"),
            "观察回调没过滤无效几何：窗口销毁时的 0×0 会被记住"
        );

        let flush_at = me.find("fn window_flush(").expect("window_flush 必须存在");
        let flush_end = me[flush_at..]
            .find("match rustdb_core::save_layout")
            .map(|i| flush_at + i)
            .unwrap_or(me.len());
        assert!(
            me[flush_at..flush_end].contains("is_plausible"),
            "落盘本体没挡脏值：一旦被 0×0 污染，下次启动就钳成小窗"
        );
    }

    /// 四条接线缺一条，这个功能都等于没做，且都不会编译失败：
    /// ① `new` 里挂观察者；② `render` 每帧兜底读（Windows 纯拖动不触发观察者）；
    /// ③ `poll` 里调 `window_poll`；④ 退出钩子。
    #[test]
    fn the_entry_points_are_wired() {
        let lifecycle = include_str!("lifecycle.rs");
        assert!(
            lifecycle.contains("observe_window_bounds(window"),
            "`with_layout_path` 里没挂窗口几何观察者"
        );
        assert!(
            lifecycle.contains("self.window_poll();"),
            "`poll` 里没调 `window_poll` —— 几何只留在内存，重启即丢"
        );

        let render = include_str!("render.rs");
        assert!(
            render.contains("self.window_bounds_observed(window, cx)"),
            "`render` 里没有兜底读 —— 纯拖动位置记不到"
        );

        let me = include_str!("window_persist.rs");
        assert!(
            me.contains("on_app_quit") && me.contains("window_flush_on_quit()"),
            "退出钩子缺失：拖完立刻关窗的那次调整会丢"
        );
        assert!(
            me.contains(".detach()"),
            "`on_app_quit` 的返回值必须就地 detach()，否则钩子根本没挂上"
        );
    }
}
