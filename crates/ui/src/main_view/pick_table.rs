//! 面板里的「表清单」表格：两列（复选框 + 表名），复选后回写各自面板。
//!
//! 转储 SQL / 跨库传输 / 数据对比三个面板原本各写一套「✓ / ·」文本列表，
//! 现在统一用 gpui-kit `DataTable`，复选框与表头全选的行为一致。
//! 表格实体只保留一个，按 `PickKind` 切换（同一时间只有一个模态面板）。

use std::collections::HashSet;

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::{
    div, px, AnyElement, App, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled,
    WeakEntity, Window,
};

use super::MainView;

/// 哪个面板的清单。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickKind {
    Dump,
    Transfer,
    Compare,
}

impl PickKind {
    /// 行选择器前缀（保留既有测试与调试入口）。
    pub(crate) fn row_prefix(self) -> &'static str {
        match self {
            Self::Dump => "dump-table",
            Self::Transfer => "transfer-table",
            Self::Compare => "dc-table",
        }
    }

    /// 表头全选复选框的调试选择器。
    fn toggle_id(self) -> &'static str {
        match self {
            Self::Dump => "dump-toggle-all",
            Self::Transfer => "transfer-toggle-all",
            Self::Compare => "compare-toggle-all",
        }
    }

    fn panel_label(self) -> &'static str {
        "表"
    }
}

pub(crate) struct PickListTable {
    kind: PickKind,
    rows: Vec<String>,
    checked: HashSet<String>,
    main: WeakEntity<MainView>,
}

impl PickListTable {
    fn sync(&mut self, rows: Vec<String>, checked: HashSet<String>) -> bool {
        if self.rows == rows && self.checked == checked {
            return false;
        }
        self.rows = rows;
        self.checked = checked;
        true
    }

    fn all_checked(&self) -> bool {
        !self.rows.is_empty() && self.rows.iter().all(|name| self.checked.contains(name))
    }
}

impl TableDelegate for PickListTable {
    fn columns_count(&self, _: &App) -> usize {
        2
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let mut column = if col_ix == 0 {
            Column::new("select", "").width(px(44.))
        } else {
            Column::new("name", "表").width(px(360.))
        };
        column.selectable = false;
        column.movable = false;
        column.min_width = px(40.);
        column
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if col_ix != 0 {
            return div()
                .h_full()
                .flex()
                .items_center()
                .text_size(px(crate::design::TS_CAPTION))
                .child(self.kind.panel_label())
                .into_any_element();
        }
        let table = cx.entity();
        let kind = self.kind;
        let all = self.all_checked();
        div()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .debug_selector(move || kind.toggle_id().to_string())
                    .child(
                        Checkbox::new(SharedString::from(kind.toggle_id()))
                            .checked(all)
                            .tooltip("全选 / 全不选")
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                table.update(cx, |state, cx| {
                                    state.delegate_mut().checked = if checked {
                                        state.delegate().rows.iter().cloned().collect()
                                    } else {
                                        HashSet::new()
                                    };
                                    if let Some(main) = state.delegate().main.upgrade() {
                                        let _ = main.update(cx, |view, cx| {
                                            view.pick_toggle_all(kind, checked, cx)
                                        });
                                    }
                                    cx.notify();
                                });
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let prefix = self.kind.row_prefix();
        div()
            .id(("pick-row", row_ix))
            .debug_selector({
                let selector = format!("{prefix}-{row_ix}");
                move || selector.clone()
            })
            .h(px(24.))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(name) = self.rows.get(row_ix).cloned() else {
            return div().into_any_element();
        };
        let checked = self.checked.contains(&name);
        let kind = self.kind;
        let main = self.main.clone();
        if col_ix == 0 {
            let click_name = name.clone();
            return div()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Checkbox::new(SharedString::from(format!("pick-check-{row_ix}")))
                        .checked(checked)
                        .on_click(move |value, _, cx| {
                            let value = *value;
                            let _ = main.update(cx, |view, cx| {
                                view.pick_toggle(kind, &click_name, value, cx)
                            });
                        }),
                )
                .into_any_element();
        }
        let click_name = name.clone();
        div()
            .id(("pick-name", row_ix))
            .h_full()
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(crate::design::TS_CAPTION))
            .child(SharedString::from(name))
            .on_click(move |_, _, cx| {
                let _ = main.update(cx, |view, cx| {
                    let next = view
                        .pick_checked(kind)
                        .is_some_and(|set| !set.contains(&click_name));
                    view.pick_toggle(kind, &click_name, next, cx)
                });
            })
            .into_any_element()
    }
}

impl MainView {
    /// 当前面板的表清单与勾选集合。
    fn pick_state(&self, kind: PickKind) -> Option<(Vec<String>, HashSet<String>)> {
        match kind {
            PickKind::Dump => self
                .dump
                .as_ref()
                .map(|panel| (panel.tables.clone(), panel.selected.clone())),
            PickKind::Transfer => self
                .transfer
                .as_ref()
                .map(|panel| (panel.tables.clone(), panel.selected.clone())),
            PickKind::Compare => self
                .data_compare
                .as_ref()
                .map(|panel| (panel.tables.clone(), panel.selected.clone())),
        }
    }

    fn pick_checked(&self, kind: PickKind) -> Option<HashSet<String>> {
        self.pick_state(kind).map(|(_, checked)| checked)
    }

    /// 勾选 / 取消一张表；传输与对比会顺带让旧预览失效。
    pub(crate) fn pick_toggle(
        &mut self,
        kind: PickKind,
        name: &str,
        checked: bool,
        cx: &mut Context<Self>,
    ) {
        match kind {
            PickKind::Dump => {
                if let Some(panel) = self.dump.as_mut() {
                    if checked {
                        panel.selected.insert(name.to_string());
                    } else {
                        panel.selected.remove(name);
                    }
                }
            }
            PickKind::Transfer => {
                if let Some(panel) = self.transfer.as_mut() {
                    if checked {
                        panel.selected.insert(name.to_string());
                    } else {
                        panel.selected.remove(name);
                    }
                    panel.reset_preview();
                }
            }
            PickKind::Compare => {
                if let Some(panel) = self.data_compare.as_mut() {
                    if checked {
                        panel.selected.insert(name.to_string());
                    } else {
                        panel.selected.remove(name);
                    }
                    panel.reset_preview();
                }
            }
        }
        cx.notify();
    }

    /// 表清单全选 / 全不选。
    pub(crate) fn pick_toggle_all(&mut self, kind: PickKind, checked: bool, cx: &mut Context<Self>) {
        match kind {
            PickKind::Dump => {
                if let Some(panel) = self.dump.as_mut() {
                    panel.selected.clear();
                    if checked {
                        panel.selected.extend(panel.tables.iter().cloned());
                    }
                }
            }
            PickKind::Transfer => {
                if let Some(panel) = self.transfer.as_mut() {
                    panel.selected.clear();
                    if checked {
                        panel.selected.extend(panel.tables.iter().cloned());
                    }
                    panel.reset_preview();
                }
            }
            PickKind::Compare => {
                if let Some(panel) = self.data_compare.as_mut() {
                    panel.selected.clear();
                    if checked {
                        panel.selected.extend(panel.tables.iter().cloned());
                    }
                    panel.reset_preview();
                }
            }
        }
        cx.notify();
    }

    /// 确保当前面板的清单表格存在；面板切换时重建委托。
    pub(crate) fn ensure_pick_table(
        &mut self,
        kind: PickKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TableState<PickListTable>>> {
        let (rows, checked) = self.pick_state(kind)?;
        if let Some(table) = self.pick_table.clone() {
            if table.read(cx).delegate().kind == kind {
                return Some(table);
            }
        }
        let delegate = PickListTable {
            kind,
            rows,
            checked,
            main: cx.entity().downgrade(),
        };
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(false)
                .col_selectable(false)
                .cell_selectable(false)
                .sortable(false)
                .col_movable(false)
                .loop_selection(false)
        });
        self.pick_table = Some(table.clone());
        Some(table)
    }

    /// 把面板里的勾选状态同步到表格。
    pub(crate) fn sync_pick_table(&mut self, kind: PickKind, cx: &mut Context<Self>) {
        let Some((rows, checked)) = self.pick_state(kind) else {
            return;
        };
        let Some(table) = self.pick_table.clone() else {
            return;
        };
        table.update(cx, |state, cx| {
            if state.delegate_mut().sync(rows, checked) {
                state.refresh(cx);
            }
        });
    }
}

/// 表格外框的统一高度（表多时内部滚动）。
pub(crate) fn pick_table_box(
    table: Entity<TableState<PickListTable>>,
    height: f32,
) -> AnyElement {
    div()
        .h(px(height))
        .flex_shrink_0()
        .child(
            gpui_kit::component::table::DataTable::new(&table)
                .bordered(false)
                .stripe(false)
                .header_row_height(px(24.)),
        )
        .into_any_element()
}
