//! Navicat 兼容快捷键回归：帮助浮层、新建查询、标签切换、注释切换、新建连接。

use super::tests_helpers::*;
use super::*;

/// F1 打开快捷键帮助；Esc 关闭（Navicat 的 Show Keyboard Shortcuts）。
#[gpui_kit::test]
fn f1_opens_and_escape_closes_shortcut_help(cx: &mut gpui_kit::TestAppContext) {
    let (vcx, _holder) = test_view(cx);
    vcx.run_until_parked();
    vcx.simulate_keystrokes("f1");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("shortcuts-panel").is_some(),
        "F1 应打开快捷键帮助"
    );
    assert!(
        vcx.debug_bounds("shortcuts-scrim").is_some(),
        "帮助浮层需要遮罩吃掉背后点击"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("shortcuts-panel").is_none(),
        "Esc 应关闭快捷键帮助"
    );
}

/// Ctrl+Q：在当前库新建查询页（Navicat Common Keys）。
#[gpui_kit::test]
fn ctrl_q_opens_a_new_query_tab(cx: &mut gpui_kit::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, _| view.current_ns = Some(ns("main")));
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("ctrl-q");
    vcx.run_until_parked();
    let tabs = vcx.update(|_, app| view.read(app).tabs.len());
    assert_eq!(tabs, 1, "Ctrl+Q 应新建一个查询标签页");
}

/// Ctrl+N：新建连接（主窗口无「打开表」上下文时）。
#[gpui_kit::test]
fn ctrl_n_opens_the_connection_editor(cx: &mut gpui_kit::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-n");
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    assert!(
        vcx.update(|_, app| view.read(app).conn_editor.is_some()),
        "Ctrl+N 应打开新建连接"
    );
}

/// Ctrl+/：整段加注释，再按一次还原（保留缩进）。
#[gpui_kit::test]
fn ctrl_slash_toggles_line_comments(cx: &mut gpui_kit::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let text = "  SELECT 1;\nSELECT 2;";
    let len = text.len();
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.sql_input.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.set_selected_range(0..len, cx);
            });
        });
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("ctrl-/");
    vcx.run_until_parked();
    let commented = vcx.update(|_, app| view.read(app).sql_input.read(app).value().to_string());
    assert_eq!(
        commented, "  -- SELECT 1;\n-- SELECT 2;",
        "Ctrl+/ 应按行加注释并保留缩进"
    );

    vcx.simulate_keystrokes("ctrl-/");
    vcx.run_until_parked();
    let restored = vcx.update(|_, app| view.read(app).sql_input.read(app).value().to_string());
    assert_eq!(restored, text, "再按一次应取消注释");
}

/// Ctrl+R：即使焦点在编辑器里也要走到执行链路（Navicat Query Editor）。
#[gpui_kit::test]
fn ctrl_r_runs_from_the_editor(cx: &mut gpui_kit::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, _| {
            view.tabs = vec![SqlTab::new(1, Some(ns("main")))];
            view.active_tab = 0;
        });
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("ctrl-r");
    vcx.run_until_parked();
    let status = vcx.update(|_, app| view.read(app).status.clone());
    assert!(
        status.contains("连接"),
        "Ctrl+R 应进入执行链路（未连接时提示连接），实际：{status}"
    );
}

/// Ctrl+Tab / Ctrl+Shift+Tab：在标签页之间循环（Navicat Common Keys）。
#[gpui_kit::test]
fn ctrl_tab_cycles_tabs(cx: &mut gpui_kit::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, _| {
            view.tabs = vec![
                SqlTab::new(1, Some(ns("main"))),
                SqlTab::new(2, Some(ns("main"))),
            ];
            view.active_tab = 0;
        });
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("ctrl-tab");
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view.read(app).active_tab),
        1,
        "Ctrl+Tab 应切到下一个标签"
    );

    vcx.simulate_keystrokes("ctrl-shift-tab");
    vcx.run_until_parked();
    assert_eq!(
        vcx.update(|_, app| view.read(app).active_tab),
        0,
        "Ctrl+Shift+Tab 应切回上一个标签"
    );
}
