//! 表设计器的字段表格：基于 gpui-kit `DataTable`，带行复选框多选。
//!
//! 字段草稿仍以 `DesignerState.draft.columns` 为唯一事实源；这里只保存一份
//! 渲染快照 + 勾选集合，任何草稿变化都由 `MainView::sync_designer_table`
//! 同步进来。单元格本身不就地编辑，编辑走下方「字段属性」面板。

use std::collections::HashSet;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{h_flex, v_flex, Icon, Sizable as _};
use gpui_kit::{
    div, px, AnyElement, App, Context, Div, Entity, Hsla, IntoElement, ParentElement, SharedString,
    InteractiveElement as _, Stateful, Styled, WeakEntity, Window,
};
use rustdb_core::ColumnDraft;
use rustdb_db_api::ColumnInfo;

use super::MainView;

/// 一行字段的渲染快照。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DesignerRow {
    pub name: String,
    pub base_type: String,
    pub length: String,
    pub decimals: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub auto_increment: bool,
    pub default: String,
    pub comment: String,
}

impl DesignerRow {
    /// 从草稿 + 现状列构造快照：注释与自增来自现状（草稿不承载这两项）。
    pub(crate) fn rows(columns: &[ColumnDraft], before: &[ColumnInfo]) -> Vec<Self> {
        columns
            .iter()
            .map(|column| {
                let (base_type, length, decimals) = split_type(&column.type_text);
                let old = before
                    .iter()
                    .find(|item| item.meta.name.eq_ignore_ascii_case(&column.name));
                Self {
                    name: column.name.clone(),
                    base_type,
                    length,
                    decimals,
                    nullable: column.nullable,
                    primary_key: column.primary_key,
                    auto_increment: old.is_some_and(|item| item.is_auto_increment),
                    default: column.default.clone().unwrap_or_default(),
                    comment: old
                        .and_then(|item| item.comment.clone())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }
}

/// `varchar(50)` / `numeric(12,2)` →（类型 / 长度 / 小数位）。
fn split_type(text: &str) -> (String, String, String) {
    let Some(open) = text.find('(') else {
        return (text.trim().to_string(), String::new(), String::new());
    };
    let Some(close) = text.rfind(')') else {
        return (text.trim().to_string(), String::new(), String::new());
    };
    if close <= open {
        return (text.trim().to_string(), String::new(), String::new());
    }
    let base = text[..open].trim().to_string();
    let inner = &text[open + 1..close];
    let mut parts = inner.split(',');
    let length = parts.next().unwrap_or("").trim().to_string();
    let decimals = parts.next().unwrap_or("").trim().to_string();
    (base, length, decimals)
}

/// 表设计器字段页的 DataTable 委托。
pub(crate) struct DesignerTable {
    rows: Vec<DesignerRow>,
    pub(crate) checked: HashSet<usize>,
    main: WeakEntity<MainView>,
}

impl DesignerTable {
    pub(crate) fn new(rows: Vec<DesignerRow>, main: WeakEntity<MainView>) -> Self {
        Self {
            rows,
            checked: HashSet::new(),
            main,
        }
    }

    /// 同步快照；结构变化时丢弃越界的勾选。返回是否有变化。
    pub(crate) fn sync(&mut self, rows: Vec<DesignerRow>) -> bool {
        if self.rows == rows {
            return false;
        }
        self.rows = rows;
        let len = self.rows.len();
        self.checked.retain(|index| *index < len);
        true
    }

    pub(crate) fn checked_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = self.checked.iter().copied().collect();
        indices.sort_unstable();
        indices
    }

    pub(crate) fn clear_checked(&mut self) {
        self.checked.clear();
    }

    fn all_checked(&self) -> bool {
        !self.rows.is_empty() && self.checked.len() == self.rows.len()
    }

    fn set_checked(&mut self, row: usize, checked: bool) {
        if checked {
            self.checked.insert(row);
        } else {
            self.checked.remove(&row);
        }
    }

    fn set_all(&mut self, checked: bool) {
        self.checked.clear();
        if checked {
            self.checked.extend(0..self.rows.len());
        }
    }

    fn text(&self, row: usize, text: &str, color: Hsla) -> AnyElement {
        let _ = row;
        div()
            .h_full()
            .flex()
            .items_center()
            .text_size(px(crate::design::TS_CAPTION))
            .text_color(color)
            .text_ellipsis()
            .child(SharedString::from(text.to_string()))
            .into_any_element()
    }
}

impl TableDelegate for DesignerTable {
    fn columns_count(&self, _: &App) -> usize {
        11
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (key, name, width) = match col_ix {
            0 => ("select", "", 44.),
            1 => ("no", "#", 38.),
            2 => ("name", "名", 168.),
            3 => ("type", "类型", 110.),
            4 => ("length", "长度", 60.),
            5 => ("decimals", "小数位", 64.),
            6 => ("not_null", "不是 null", 82.),
            7 => ("auto_inc", "自动递增", 82.),
            8 => ("key", "键", 52.),
            9 => ("default", "默认值", 140.),
            _ => ("comment", "注释", 220.),
        };
        let mut column = Column::new(key, name).width(px(width));
        column.min_width = px(32.);
        column.max_width = px(520.);
        column.selectable = false;
        column.movable = false;
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
                .child(self.column(col_ix, cx).name.clone())
                .into_any_element();
        }
        let table = cx.entity();
        let all = self.all_checked();
        div()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .debug_selector(|| "designer-select-all".to_string())
                    .child(Checkbox::new("designer-select-all")
                    .checked(all)
                    .tooltip("全选 / 全不选")
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        table.update(cx, |state, cx| {
                            state.delegate_mut().set_all(checked);
                            cx.notify();
                        });
                    })),
            )
            .into_any_element()
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let checked = self.checked.contains(&row_ix);
        let accent = crate::theme::to_hsla(crate::theme::current(cx).accent);
        div()
            .id(("designer-row", row_ix))
            .debug_selector({
                let selector = format!("designer-column-{row_ix}");
                move || selector.clone()
            })
            .h(px(28.))
            .when(checked, |row| row.bg(accent.opacity(0.08)))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.rows.get(row_ix).cloned() else {
            return div().into_any_element();
        };
        let t = crate::theme::current(cx);
        let muted = crate::theme::to_hsla(t.fg_muted);
        let primary = crate::theme::to_hsla(t.fg_primary);
        let secondary = crate::theme::to_hsla(t.fg_secondary);
        let accent = crate::theme::to_hsla(t.accent);
        match col_ix {
            0 => {
                let table = cx.entity();
                let checked = self.checked.contains(&row_ix);
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .debug_selector({
                                let selector = format!("designer-select-{row_ix}");
                                move || selector.clone()
                            })
                            .child(Checkbox::new(SharedString::from(format!("designer-select-{row_ix}")))
                            .checked(checked)
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                table.update(cx, |state, cx| {
                                    state.delegate_mut().set_checked(row_ix, checked);
                                    cx.notify();
                                });
                            })),
                    )
                    .into_any_element()
            }
            1 => self.text(row_ix, &format!("{}", row_ix + 1), muted),
            2 => {
                let color = if row.primary_key { accent } else { primary };
                self.text(row_ix, &row.name, color)
            }
            3 => self.text(row_ix, &row.base_type, secondary),
            4 => self.text(row_ix, &row.length, secondary),
            5 => self.text(row_ix, &row.decimals, secondary),
            6 => {
                let main = self.main.clone();
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Checkbox::new(SharedString::from(format!(
                            "designer-not-null-{row_ix}"
                        )))
                        .checked(!row.nullable)
                        .tooltip("不是 null")
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            let _ = main.update(cx, |view, cx| {
                                view.designer_set_column_null(row_ix, checked, cx)
                            });
                        }),
                    )
                    .into_any_element()
            }
            7 => {
                if row.auto_increment {
                    h_flex()
                        .h_full()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::Check)
                                .small()
                                .text_color(crate::theme::to_hsla(t.success)),
                        )
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            }
            8 => {
                let main = self.main.clone();
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Checkbox::new(SharedString::from(format!("designer-pk-{row_ix}")))
                            .checked(row.primary_key)
                            .tooltip("主键")
                            .on_click(move |checked, _, cx| {
                                let checked = *checked;
                                let _ = main.update(cx, |view, cx| {
                                    view.designer_set_column_pk(row_ix, checked, cx)
                                });
                            }),
                    )
                    .into_any_element()
            }
            9 => self.text(row_ix, &row.default, secondary),
            _ => self.text(row_ix, &row.comment, muted),
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let t = crate::theme::current(cx);
        v_flex_center(
            "还没有字段",
            "点上方「添加字段」新增一行",
            crate::theme::to_hsla(t.fg_muted),
            crate::theme::to_hsla(t.fg_primary),
        )
    }
}

/// 空表占位（DataTable 的 `render_empty` 需要一个具体元素）。
fn v_flex_center(
    title: &str,
    hint: &str,
    muted: Hsla,
    primary: Hsla,
) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            div()
                .text_size(px(crate::design::TS_BODY))
                .text_color(primary)
                .child(SharedString::from(title.to_string())),
        )
        .child(
            div()
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(muted)
                .child(SharedString::from(hint.to_string())),
        )
        .into_any_element()
}

/// 由视图侧创建 / 读取表格实体时用到的类型别名。
pub(crate) type DesignerTableEntity = Entity<TableState<DesignerTable>>;
