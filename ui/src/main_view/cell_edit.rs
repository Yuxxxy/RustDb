//! 单元格编辑与行操作：进入编辑、提交、置空、删行与 Esc 层级。

use super::*;

impl MainView {
    /// 在活动标签页里打开对象的数据：切到对象所在库、填 SELECT、立即执行。
    /// 双击单元格进入编辑（F-35）。只有「打开表」产生的结果集可编辑。
    pub(crate) fn begin_cell_edit(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = self.active_result() else {
            return;
        };
        if result.editable.is_none() {
            self.status = "该结果集不可编辑：只有「打开表」的结果支持编辑".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        }
        let Some(column) = result.columns.get(col).cloned() else {
            return;
        };
        let Some(value) = result.rows.get(row).and_then(|values| values.get(col)) else {
            return;
        };
        let text = if value.is_null() {
            String::new()
        } else {
            value.display_text()
        };
        let placeholder = format!("{}（{}）", column.name, column.logical_type.label());
        let input = cx
            .new(|cx| gpui_component::input::InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| state.set_value(text, window, cx));
        self.edit_cell = Some((row, col, column.name.to_string()));
        self.edit_input = Some(input);
        cx.notify();
    }

    /// 编辑条「确定」：类型校验 → 写入变更集 → 同步网格显示。
    pub(crate) fn commit_cell_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, col, _)) = self.edit_cell.clone() else {
            return;
        };
        let text = self
            .edit_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => apply_cell_edit(result, row, col, Some(text)),
            None => Ok(0),
        };
        match outcome {
            Ok(count) => {
                self.status = if count == 0 {
                    "没有变化".into()
                } else {
                    format!("待提交 {count} 条变更")
                };
                self.status_kind = StatusKind::Ok;
                self.edit_cell = None;
                self.edit_input = None;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        let _ = window;
        cx.notify();
    }

    /// 编辑条「设为 NULL」：直接落下 NULL（与空串严格区分）。
    pub(crate) fn set_cell_null(&mut self, cx: &mut Context<Self>) {
        let Some((row, col, _)) = self.edit_cell.clone() else {
            return;
        };
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => apply_cell_edit(result, row, col, None),
            None => Ok(0),
        };
        match outcome {
            Ok(_) => {
                self.edit_cell = None;
                self.edit_input = None;
                self.status = "已设为 NULL".into();
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    pub(crate) fn cancel_cell_edit(&mut self, cx: &mut Context<Self>) {
        self.edit_cell = None;
        self.edit_input = None;
        cx.notify();
    }

    /// 删除选中行（变更集 Delete）：行保留可见但标红，提交后消失。
    pub(crate) fn delete_selected_row(&mut self, cx: &mut Context<Self>) {
        let Some((row, _, _)) = self.edit_cell.clone().or_else(|| {
            self.selected_cell
                .map(|(row, col)| (row, col, String::new()))
        }) else {
            self.status = "请先选中一行".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => delete_row(result, row),
            None => Ok(0),
        };
        match outcome {
            Ok(count) => {
                self.status = format!("待提交 {count} 条变更");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 追加 N 行（变更集 Insert，所有列先置 NULL）。
    pub(crate) fn append_rows(&mut self, count: usize, cx: &mut Context<Self>) {
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => insert_rows(result, count),
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(total) => {
                self.status = format!("待提交 {total} 条变更（新增 {count} 行，其余列待填写）");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 当前选中的行下标（正在编辑的单元格优先）。
    pub(crate) fn selected_row(&self) -> Option<usize> {
        self.edit_cell
            .as_ref()
            .map(|(row, _, _)| *row)
            .or_else(|| self.selected_cell.map(|(row, _)| row))
    }

    /// 当前选中的列下标（正在编辑的单元格优先）。
    pub(crate) fn selected_column(&self) -> Option<usize> {
        self.edit_cell
            .as_ref()
            .map(|(_, col, _)| *col)
            .or_else(|| self.selected_cell.map(|(_, col)| col))
    }

    /// 打开行操作菜单（锚点用按钮的点击位置）。
    pub(crate) fn open_row_menu(
        &mut self,
        event: &gpui::ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = match event {
            gpui::ClickEvent::Mouse(mouse) => mouse.down.position,
            _ => gpui::point(px(0.), px(0.)),
        };
        self.row_menu = Some(at);
        cx.notify();
    }

    /// 行操作菜单（F-71）：新增 / 克隆 / 粘贴 / 生成器 / 默认值 / 撤销。
    pub(crate) fn row_menu_groups(&self) -> Vec<Vec<MenuEntry>> {
        vec![
            vec![
                MenuEntry::new(MenuAction::AppendRows(1), "追加 1 行", IconName::ListPlus)
                    .key("row-append-1"),
                MenuEntry::new(MenuAction::AppendRows(10), "追加 10 行", IconName::ListPlus)
                    .key("row-append-10"),
            ],
            vec![
                MenuEntry::new(MenuAction::CloneRow, "克隆选中行", IconName::CopyPlus)
                    .key("row-clone")
                    .hint("主键留空"),
                MenuEntry::new(MenuAction::PasteTsv, "粘贴 TSV", IconName::ClipboardPaste)
                    .key("row-paste")
                    .hint("剪贴板"),
            ],
            vec![
                MenuEntry::new(MenuAction::OpenGenerator, "数据生成器", IconName::Wand)
                    .key("row-generator"),
                MenuEntry::new(
                    MenuAction::SetCellDefault,
                    "设为数据库默认值",
                    IconName::RotateCcw,
                )
                .key("row-default"),
                MenuEntry::new(MenuAction::RevertRow, "撤销本行变更", IconName::Undo2)
                    .key("row-revert"),
            ],
        ]
    }

    pub(crate) fn render_row_menu(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.render_menu(self.row_menu_groups(), cx)
    }

    /// 克隆选中行（主键交给数据库默认值）。
    pub(crate) fn clone_selected_row(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            self.status = "请先选中一行".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => clone_row(result, row),
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(total) => {
                self.status = format!("待提交 {total} 条变更（已克隆一行，主键留空由数据库生成）");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 从剪贴板粘贴 TSV（首行与列名一致时当表头跳过）。
    pub(crate) fn paste_tsv(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.status = "剪贴板里没有文本".into();
            self.status_kind = StatusKind::Error;
            cx.notify();
            return;
        };
        let grid = match rustdb_core::parse_tsv(&text) {
            Ok(grid) => grid,
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
                cx.notify();
                return;
            }
        };
        let start_row = self.selected_row().unwrap_or(0);
        let start_col = self.selected_column().unwrap_or(0);
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => {
                let mut grid = grid;
                if grid.first().map_or(false, |row| is_header_row(result, row)) {
                    grid.remove(0);
                }
                if grid.is_empty() {
                    Err("剪贴板里没有可粘贴的数据".to_string())
                } else {
                    paste_tsv_into(result, &grid, start_row, start_col)
                }
            }
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(written) => {
                self.status = format!("已粘贴 {written} 个单元格（待提交）");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 把选中单元格设为数据库默认值。
    pub(crate) fn set_selected_cell_default(&mut self, cx: &mut Context<Self>) {
        let (Some(row), Some(col)) = (self.selected_row(), self.selected_column()) else {
            self.status = "请先选中一个单元格".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => set_cell_default(result, row, col),
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(total) => {
                self.status = format!("待提交 {total} 条变更（该列改回数据库默认值）");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 撤销选中行的待提交变更（新增行整行作废）。
    pub(crate) fn revert_selected_row(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            self.status = "请先选中一行".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        let outcome = match self
            .tabs
            .get_mut(self.active_tab)
            .and_then(|tab| tab.results.get_mut(tab.active_result))
        {
            Some(result) => revert_row(result, row),
            None => Err("没有可编辑的结果集".to_string()),
        };
        match outcome {
            Ok(total) => {
                self.status = format!("已撤销该行变更（待提交 {total} 条）");
                self.status_kind = StatusKind::Ok;
            }
            Err(message) => {
                self.status = message;
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }
}
