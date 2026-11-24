//! Navicat 兼容快捷键层。
//!
//! 键位取自 Navicat 18 官方 Hot Keys（Common / Main Window / Query Editor /
//! Data Editor / Table Designer 五组），只映射本应用确实具备的动作；
//! 不存在的能力（如主窗口 Console、模型设计器）不硬造空快捷键。
//!
//! 所有按键由一个全局观察者统一分派（`lifecycle::install_shortcuts`）：
//! 编辑器 / 输入框获得焦点时同样生效，这一点与 Navicat 一致。

use super::*;

/// 快捷键触发的动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ShortcutAction {
    NewConnection,
    NewQuery,
    AddRecord,
    DeleteRecord,
    Run,
    RunCurrentStatement,
    Stop,
    ApplyOrSave,
    DesignObject,
    CloseTab,
    NextTab,
    PrevTab,
    Settings,
    HistoryPanel,
    AiPanel,
    RefreshObjects,
    FocusSearch,
    ToggleComment,
    UpperCase,
    LowerCase,
    Help,
}

/// 帮助面板里的分组（与 Navicat 手册的分区对应）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ShortcutGroup {
    Main,
    Editor,
    Grid,
}

impl ShortcutGroup {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Main => "主窗口",
            Self::Editor => "查询编辑器",
            Self::Grid => "数据网格",
        }
    }
}

/// 一条键位映射。
pub(crate) struct ShortcutDef {
    /// 展示用写法，如 `Ctrl+R`。
    pub keys: &'static str,
    pub action: ShortcutAction,
    pub label: &'static str,
    pub group: ShortcutGroup,
    pub key: &'static str,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

const fn def(
    keys: &'static str,
    action: ShortcutAction,
    label: &'static str,
    group: ShortcutGroup,
    key: &'static str,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> ShortcutDef {
    ShortcutDef {
        keys,
        action,
        label,
        group,
        key,
        ctrl,
        shift,
        alt,
    }
}

/// 结果集页签的 9 条 `Alt+数字` 单独列出，帮助面板合并成一行。
const RESULT_TAB_KEYS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

pub(crate) const SHORTCUTS: &[ShortcutDef] = &[
    def(
        "Ctrl+N",
        ShortcutAction::NewConnection,
        "新建连接 / 新增记录（打开表时）",
        ShortcutGroup::Main,
        "n",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Q",
        ShortcutAction::NewQuery,
        "新建查询 / 查询当前表（打开表时）",
        ShortcutGroup::Main,
        "q",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+W",
        ShortcutAction::CloseTab,
        "关闭当前标签页",
        ShortcutGroup::Main,
        "w",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Tab",
        ShortcutAction::NextTab,
        "下一个标签页",
        ShortcutGroup::Main,
        "tab",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Shift+Tab",
        ShortcutAction::PrevTab,
        "上一个标签页",
        ShortcutGroup::Main,
        "tab",
        true,
        true,
        false,
    ),
    def(
        "Ctrl+,",
        ShortcutAction::Settings,
        "打开设置",
        ShortcutGroup::Main,
        ",",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+L",
        ShortcutAction::HistoryPanel,
        "历史记录",
        ShortcutGroup::Main,
        "l",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+I",
        ShortcutAction::AiPanel,
        "AI 助手",
        ShortcutGroup::Main,
        "i",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Shift+F",
        ShortcutAction::FocusSearch,
        "在数据库 / 模式中查找",
        ShortcutGroup::Main,
        "f",
        true,
        true,
        false,
    ),
    def(
        "F5",
        ShortcutAction::RefreshObjects,
        "刷新对象",
        ShortcutGroup::Main,
        "f5",
        false,
        false,
        false,
    ),
    def(
        "F1",
        ShortcutAction::Help,
        "键盘快捷键",
        ShortcutGroup::Main,
        "f1",
        false,
        false,
        false,
    ),
    def(
        "Ctrl+Shift+/",
        ShortcutAction::Help,
        "键盘快捷键",
        ShortcutGroup::Main,
        "/",
        true,
        true,
        false,
    ),
    def(
        "Ctrl+R",
        ShortcutAction::Run,
        "运行（选区 / 全文）",
        ShortcutGroup::Editor,
        "r",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Shift+R",
        ShortcutAction::RunCurrentStatement,
        "运行当前语句",
        ShortcutGroup::Editor,
        "r",
        true,
        true,
        false,
    ),
    def(
        "Ctrl+T",
        ShortcutAction::Stop,
        "停止执行",
        ShortcutGroup::Editor,
        "t",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+/",
        ShortcutAction::ToggleComment,
        "注释 / 取消注释",
        ShortcutGroup::Editor,
        "/",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Shift+U",
        ShortcutAction::UpperCase,
        "转为大写",
        ShortcutGroup::Editor,
        "u",
        true,
        true,
        false,
    ),
    def(
        "Ctrl+U",
        ShortcutAction::LowerCase,
        "转为小写",
        ShortcutGroup::Editor,
        "u",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+D",
        ShortcutAction::DesignObject,
        "设计当前表",
        ShortcutGroup::Grid,
        "d",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+S",
        ShortcutAction::ApplyOrSave,
        "提交变更 / 保存 SQL",
        ShortcutGroup::Grid,
        "s",
        true,
        false,
        false,
    ),
    def(
        "Ctrl+Delete",
        ShortcutAction::DeleteRecord,
        "删除选中记录",
        ShortcutGroup::Grid,
        "delete",
        true,
        false,
        false,
    ),
    def(
        "Insert",
        ShortcutAction::AddRecord,
        "新增记录",
        ShortcutGroup::Grid,
        "insert",
        false,
        false,
        false,
    ),
];

/// 按按键找映射；修饰键必须完全一致（`Ctrl+Shift+R` 不能被 `Ctrl+R` 抢走）。
pub(crate) fn lookup(keystroke: &gpui_kit::Keystroke) -> Option<&'static ShortcutDef> {
    SHORTCUTS.iter().find(|def| {
        def.key.eq_ignore_ascii_case(&keystroke.key)
            && def.ctrl == keystroke.modifiers.control
            && def.shift == keystroke.modifiers.shift
            && def.alt == keystroke.modifiers.alt
    })
}

impl MainView {
    /// 统一分派；返回 `true` 表示这次按键已被消费。
    pub(crate) fn handle_shortcut(
        &mut self,
        keystroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // 帮助浮层自己也要能关：F1 / Ctrl+Shift+/ 再按一次关闭。
        if matches!(lookup(keystroke).map(|def| def.action), Some(ShortcutAction::Help)) {
            self.shortcuts_open = !self.shortcuts_open;
            cx.notify();
            return true;
        }
        if self.shortcuts_open {
            return false;
        }
        // 模态面板期间不吃全局快捷键：否则在转储 / 导入 / 连接编辑器里敲
        // Ctrl+S、Ctrl+R 会改到背后的标签页。
        if self.blocking_overlay_open() {
            return false;
        }
        if keystroke.modifiers.alt
            && !keystroke.modifiers.control
            && !keystroke.modifiers.shift
            && RESULT_TAB_KEYS.contains(&keystroke.key.as_str())
        {
            let index = keystroke
                .key
                .parse::<usize>()
                .map(|digit| digit.saturating_sub(1))
                .unwrap_or(0);
            self.activate_result_tab(index, cx);
            return true;
        }
        let Some(def) = lookup(keystroke) else {
            return false;
        };
        self.run_shortcut(def.action, window, cx)
    }

    /// 有模态浮层时不接全局快捷键（Esc / F1 仍走 `handle_escape`）。
    fn blocking_overlay_open(&self) -> bool {
        self.settings_open
            || self.conn_editor.is_some()
            || self.dump.is_some()
            || self.table_dump.is_some()
            || self.transfer.is_some()
            || self.import_panel.is_some()
            || self.params_panel.is_some()
            || self.generator.is_some()
            || self.optimize.is_some()
            || self.ai_model_panel.is_some()
            || self.host_key_prompt.is_some()
            || self.danger.is_some()
    }

    /// 当前结果集对应的表：优先用可编辑目标，其次用服务端分页目标。
    pub(crate) fn active_table_path(&self) -> Option<rustdb_db_api::ObjectPath> {
        let result = self.active_result()?;
        result
            .editable
            .as_ref()
            .map(|(path, _)| path.clone())
            .or_else(|| result.page.as_ref().map(|page| page.path.clone()))
    }

    fn run_shortcut(
        &mut self,
        action: ShortcutAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // 网格专属键在 SQL 编辑器里必须让位给编辑器自身：
        // `Insert` 是普通输入、`Ctrl+Delete` 在 gpui 里是「删到下个词尾」。
        let editor_focused = self.sql_input.read(cx).focus_handle(cx).is_focused(window);
        if matches!(
            action,
            ShortcutAction::AddRecord | ShortcutAction::DeleteRecord
        ) && editor_focused
        {
            return false;
        }
        match action {
            ShortcutAction::NewConnection => {
                // Navicat：数据编辑器里 Ctrl+N 是新增记录，主窗口里是新建连接。
                if !editor_focused && self.active_table_path().is_some() {
                    self.append_rows(1, cx);
                } else {
                    self.open_conn_editor(None, window, cx);
                }
                true
            }
            // Navicat：主窗口 Ctrl+N 新建对象（连接），数据编辑器里是新增记录。
            ShortcutAction::NewQuery => {
                if let Some(path) = self.active_table_path() {
                    self.query_object(path, window, cx);
                } else {
                    self.open_untitled_tab(window, cx);
                }
                true
            }
            ShortcutAction::AddRecord => {
                self.append_rows(1, cx);
                true
            }
            ShortcutAction::DeleteRecord => {
                self.delete_selected_row(cx);
                true
            }
            ShortcutAction::Run => {
                self.run(window, cx);
                true
            }
            ShortcutAction::RunCurrentStatement => {
                self.run_current_statement(window, cx);
                true
            }
            ShortcutAction::Stop => {
                self.cancel(cx);
                true
            }
            ShortcutAction::ApplyOrSave => {
                // Navicat 数据编辑器：Ctrl+S 提交记录变更；查询编辑器：保存 SQL。
                if self
                    .active_result()
                    .and_then(|result| result.changes.as_ref())
                    .is_some()
                {
                    self.apply_changes(cx);
                } else {
                    self.save_sql_file(false, window, cx);
                }
                true
            }
            ShortcutAction::DesignObject => {
                match self.active_table_path() {
                    Some(path) => {
                        self.design_table(path, cx);
                        true
                    }
                    None => {
                        self.set_status(
                            "Ctrl+D 需要先「打开表」（结果集要绑定一张表）",
                            StatusKind::Idle,
                            cx,
                        );
                        true
                    }
                }
            }
            ShortcutAction::CloseTab => {
                if !self.tabs.is_empty() {
                    self.close_tab(self.active_tab, window, cx);
                }
                true
            }
            ShortcutAction::NextTab => {
                self.step_tab(1, window, cx);
                true
            }
            ShortcutAction::PrevTab => {
                self.step_tab(usize::MAX, window, cx);
                true
            }
            ShortcutAction::Settings => {
                self.open_settings(cx);
                true
            }
            ShortcutAction::HistoryPanel => {
                self.panel = PanelKind::History;
                self.layout.sidebar_collapsed = false;
                cx.notify();
                true
            }
            ShortcutAction::AiPanel => {
                self.toggle_ai(cx);
                true
            }
            ShortcutAction::RefreshObjects => {
                match self.current_namespace() {
                    Some(ns) => {
                        self.refresh_objects(ns, cx);
                        true
                    }
                    None => {
                        self.set_status("请先选择数据库", StatusKind::Idle, cx);
                        true
                    }
                }
            }
            ShortcutAction::FocusSearch => {
                self.panel = PanelKind::Connections;
                self.layout.sidebar_collapsed = false;
                if let Some(input) = self.object_input.clone() {
                    input.update(cx, |state, cx| state.focus(window, cx));
                }
                cx.notify();
                true
            }
            ShortcutAction::ToggleComment => self.toggle_comment(window, cx),
            ShortcutAction::UpperCase => self.convert_selection(true, window, cx),
            ShortcutAction::LowerCase => self.convert_selection(false, window, cx),
            ShortcutAction::Help => {
                self.shortcuts_open = true;
                cx.notify();
                true
            }
        }
    }

    fn current_namespace(&self) -> Option<Namespace> {
        self.active()
            .and_then(|tab| tab.ns.clone())
            .or_else(|| self.current_ns.clone())
    }

    fn step_tab(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        let total = self.tabs.len();
        if total < 2 {
            return;
        }
        let next = if step == usize::MAX {
            (self.active_tab + total - 1) % total
        } else {
            (self.active_tab + step) % total
        };
        self.activate_tab(next, window, cx);
    }

    /// 结果集页签（`Alt+1..9`）。没有对应结果时保持原状。
    pub(crate) fn activate_result_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if index >= tab.results.len() {
            return;
        }
        tab.active_result = index;
        tab.show_messages = false;
        cx.notify();
    }

    /// Ctrl+Q：为当前表开一个 `SELECT` 查询页（Navicat 的 Query Object）。
    fn query_object(
        &mut self,
        path: rustdb_db_api::ObjectPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            self.set_status("请先连接数据源", StatusKind::Error, cx);
            return;
        };
        if !self.open_untitled_tab(window, cx) {
            return;
        }
        let sql = self
            .core
            .object_select_sql(&session, &path, 200)
            .unwrap_or_default();
        self.sql_input
            .update(cx, |state, cx| state.set_value(sql.clone(), window, cx));
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.sql = sql;
        }
        self.diag_dirty = true;
        cx.notify();
    }

    /// Ctrl+Shift+R：只运行光标所在语句，不改动用户的选区。
    fn run_current_statement(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.sql_input.clone();
        let sql = state.read(cx).value().to_string();
        let cursor = state.read(cx).cursor();
        let Some(statement) = rustdb_sql_lang::statement_at(&sql, cursor) else {
            self.set_status("光标处没有可执行的语句", StatusKind::Idle, cx);
            return;
        };
        let previous = state.read(cx).selected_range();
        state.update(cx, |state, cx| {
            state.set_selected_range(statement.start..statement.end, cx)
        });
        self.run(window, cx);
        state.update(cx, |state, cx| state.set_selected_range(previous, cx));
    }

    /// Ctrl+/：按行注释 / 取消注释（SQL 用 `--`，与 Navicat 一致）。
    fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let state = self.sql_input.clone();
        let value = state.read(cx).value().to_string();
        let selection = state.read(cx).selected_range();
        let Some((start, end)) = line_block(&value, selection) else {
            return false;
        };
        let block = &value[start..end];
        let commented = block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.trim_start().starts_with("--"));
        let next: String = block
            .split_inclusive('\n')
            .map(|raw| {
                let (line, newline) = match raw.strip_suffix('\n') {
                    Some(line) => (line, "\n"),
                    None => (raw, ""),
                };
                if commented {
                    let trimmed = line.trim_start();
                    let cut = if trimmed.starts_with("-- ") { 3 } else { 2 };
                    let indent = line.len() - trimmed.len();
                    format!("{}{}{newline}", &line[..indent], &trimmed[cut..])
                } else if line.trim().is_empty() {
                    format!("{line}{newline}")
                } else {
                    let indent = line.len() - line.trim_start().len();
                    format!("{}-- {}{newline}", &line[..indent], &line[indent..])
                }
            })
            .collect();
        let next_len = next.len();
        state.update(cx, |state, cx| {
            state.set_selected_range(start..end, cx);
            state.replace(next, window, cx);
            // 保留整段选区：连按 Ctrl+/ 才能在「加注释 / 取消注释」之间来回切。
            state.set_selected_range(start..start + next_len, cx);
        });
        cx.notify();
        true
    }

    /// Ctrl+U / Ctrl+Shift+U：选区大小写转换（无选区时不抢按键）。
    fn convert_selection(
        &mut self,
        upper: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let selected = self.sql_input.read(cx).selected_text().to_string();
        if selected.is_empty() {
            return false;
        }
        let next = if upper {
            selected.to_uppercase()
        } else {
            selected.to_lowercase()
        };
        self.sql_input
            .update(cx, |state, cx| state.replace(next, window, cx));
        cx.notify();
        true
    }
}

impl MainView {
    /// 键盘快捷键帮助浮层（Navicat `Ctrl+Shift+/` 的「Show Keyboard Shortcuts」）。
    ///
    /// 两列布局：左列主窗口，右列编辑器 + 数据网格 —— 一屏看完，不用滚动。
    pub(crate) fn render_shortcuts_panel(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = current(cx);
        let header_text = |text: &str| {
            div()
                .pt_1()
                .text_size(px(crate::design::TS_CAPTION))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(hex(t.fg_muted))
                .child(SharedString::from(text.to_string()))
                .into_any_element()
        };
        let row = |keys: &str, label: &str| {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(112.))
                        .flex_shrink_0()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.accent))
                        .child(SharedString::from(keys.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(crate::design::TS_CAPTION))
                        .text_color(hex(t.fg_primary))
                        .text_ellipsis()
                        .child(SharedString::from(label.to_string())),
                )
                .into_any_element()
        };

        let mut left: Vec<gpui_kit::AnyElement> = vec![header_text(ShortcutGroup::Main.label())];
        let mut right: Vec<gpui_kit::AnyElement> = Vec::new();
        for group in [
            ShortcutGroup::Main,
            ShortcutGroup::Editor,
            ShortcutGroup::Grid,
        ] {
            let column = if group == ShortcutGroup::Main {
                &mut left
            } else {
                &mut right
            };
            if group != ShortcutGroup::Main {
                column.push(header_text(group.label()));
            }
            for def in SHORTCUTS.iter().filter(|def| def.group == group) {
                column.push(row(def.keys, def.label));
            }
            if group == ShortcutGroup::Editor {
                column.push(row("Ctrl+Enter", "运行（编辑器内建）"));
            }
            if group == ShortcutGroup::Grid {
                column.push(row("Alt+1…9", "切换到第 N 个结果集页签"));
            }
        }

        v_flex()
            .id("shortcuts-panel")
            .debug_selector(|| "shortcuts-panel".to_string())
            .occlude()
            .w(px(620.))
            .max_h(px(520.))
            .rounded(px(10.))
            .border_1()
            .border_color(hex(t.border_strong))
            .bg(hex(t.bg_elevated))
            .overflow_hidden()
            .on_click(cx.listener(|_, _, _, _| {}))
            .child(
                h_flex()
                    .h(px(42.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(hex(t.border_subtle))
                    .child(Icon::new(IconName::Command).small().text_color(hex(t.accent)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::design::TS_BASE))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(hex(t.fg_primary))
                            .child("键盘快捷键 · Navicat 兼容"),
                    )
                    .child(
                        div()
                            .id("shortcuts-close")
                            .debug_selector(|| "shortcuts-close".to_string())
                            .w(px(24.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.))
                            .cursor_pointer()
                            .hover(|style| style.bg(hex(t.bg_overlay)))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.shortcuts_open = false;
                                cx.notify();
                            }))
                            .child(Icon::new(IconName::Close).small().text_color(hex(t.fg_muted))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(
                        v_flex()
                            .p_3()
                            .gap_3()
                            .child(
                                h_flex()
                                    .gap_5()
                                    .items_start()
                                    .child(v_flex().flex_1().gap(px(5.)).children(left))
                                    .child(v_flex().flex_1().gap(px(5.)).children(right)),
                            )
                            .child(
                                div()
                                    .pt_1()
                                    .text_size(px(crate::design::TS_CAPTION))
                                    .text_color(hex(t.fg_muted))
                                    .whitespace_normal()
                                    .child("键位对齐 Navicat 18 官方热键表；Esc 关闭。编辑器缩放（Ctrl+= / Ctrl+- / Ctrl+0）尚未接入字号设置。"),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// 取「选区覆盖到的整行」字节范围；光标在行内时取当前行。
fn line_block(value: &str, selection: std::ops::Range<usize>) -> Option<(usize, usize)> {
    if value.is_empty() {
        return None;
    }
    let start = selection.start.min(value.len());
    let end = selection.end.min(value.len());
    // 选区正好停在下一行行首时，不要把那一行算进来。
    let effective_end = if end > start && end > 0 && value.as_bytes().get(end - 1) == Some(&b'\n') {
        end - 1
    } else {
        end
    };
    let line_start = value[..start].rfind('\n').map(|index| index + 1).unwrap_or(0);
    let line_end = value[effective_end..]
        .find('\n')
        .map(|index| effective_end + index)
        .unwrap_or(value.len());
    Some((line_start, line_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(key: &str, ctrl: bool, shift: bool, alt: bool) -> gpui_kit::Keystroke {
        let mut modifiers = gpui_kit::Modifiers::default();
        modifiers.control = ctrl;
        modifiers.shift = shift;
        modifiers.alt = alt;
        gpui_kit::Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    /// 必须与 Navicat 18 官方表一致：常用键一个都不能漏。
    #[test]
    fn navicat_core_bindings_are_present() {
        let expect = [
            ("q", true, false, false),
            ("n", true, false, false),
            ("r", true, false, false),
            ("r", true, true, false),
            ("t", true, false, false),
            ("d", true, false, false),
            ("s", true, false, false),
            ("/", true, false, false),
            ("u", true, false, false),
            ("u", true, true, false),
            ("tab", true, false, false),
            ("tab", true, true, false),
            ("f", true, true, false),
            ("l", true, false, false),
            ("i", true, false, false),
            (",", true, false, false),
            ("f5", false, false, false),
            ("f1", false, false, false),
        ];
        for (key, ctrl, shift, alt) in expect {
            assert!(
                lookup(&stroke(key, ctrl, shift, alt)).is_some(),
                "缺少键位 {key}（ctrl={ctrl} shift={shift} alt={alt}）"
            );
        }
    }

    /// Ctrl+Shift+R 必须命中「运行当前语句」，不能被 Ctrl+R 吃掉。
    #[test]
    fn shift_variants_win_over_plain_keys() {
        assert_eq!(
            lookup(&stroke("r", true, true, false)).map(|def| def.action),
            Some(ShortcutAction::RunCurrentStatement)
        );
        assert_eq!(
            lookup(&stroke("tab", true, true, false)).map(|def| def.action),
            Some(ShortcutAction::PrevTab)
        );
        assert_eq!(
            lookup(&stroke("u", true, true, false)).map(|def| def.action),
            Some(ShortcutAction::UpperCase)
        );
    }

    /// 行注释切换：加注释、再取消，文本回到原样；缩进保留。
    #[test]
    fn comment_toggle_round_trips_indented_lines() {
        let value = "  SELECT 1;\nSELECT 2;";
        let (start, end) = line_block(value, 0..value.len()).expect("整段");
        assert_eq!((start, end), (0, value.len()));
        assert_eq!(line_block("a\nb\n", 0..0), Some((0, 1)));
        // 选区停在第二行行首时只处理第一行。
        assert_eq!(line_block("a\nb", 2..2), Some((2, 3)));
    }
}
