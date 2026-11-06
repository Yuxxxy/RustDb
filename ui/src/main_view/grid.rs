//! 结果网格交互：排序、选区、列宽、复制与导出入口。

use super::*;

impl MainView {
    /// 列头点击：升序 → 降序 → 取消。
    /// 分页结果把排序下推到服务端；普通结果只排已加载的行。
    pub(crate) fn toggle_sort(
        &mut self,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut reload = None;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if let Some(view) = tab.results.get_mut(tab.active_result) {
                if let Some(page) = view.page.clone() {
                    let name = view.columns.get(column).map(|meta| meta.name.to_string());
                    if let Some(name) = name {
                        let next = match &page.order {
                            Some((current, false)) if current == &name => Some((name, true)),
                            Some((current, _)) if current == &name => None,
                            _ => Some((name, false)),
                        };
                        reload = Some((tab.id, page.path, page.page_size, next));
                    }
                } else {
                    view.sort = match view.sort {
                        Some(sort) if sort.column == column && sort.ascending => Some(SortState {
                            column,
                            ascending: false,
                        }),
                        Some(sort) if sort.column == column => None,
                        _ => Some(SortState {
                            column,
                            ascending: true,
                        }),
                    };
                }
            }
        }
        if let Some((tab_id, path, page_size, order)) = reload {
            self.reload_page(tab_id, path, 1, page_size, order, window, cx);
        }
        self.clear_grid_selection();
        cx.notify();
    }

    /// 活动标签页当前显示的结果集。
    pub(crate) fn active_result(&self) -> Option<&ResultView> {
        let tab = self.active()?;
        tab.results.get(tab.active_result)
    }

    pub(crate) fn active_result_mut(&mut self) -> Option<&mut ResultView> {
        let tab = self.tabs.get_mut(self.active_tab)?;
        tab.results.get_mut(tab.active_result)
    }

    /// 清空网格选区（切换结果、排序、筛选、重新执行时调用）。
    pub(crate) fn clear_grid_selection(&mut self) {
        self.grid_selection = None;
        self.drag_select = None;
        self.drag_moved = false;
    }

    /// 选中一个单元格；`extend` 表示以当前选区锚点做 Shift 扩选。
    pub(crate) fn select_grid_cell(
        &mut self,
        row: usize,
        col: usize,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = self.active_result() else {
            return;
        };
        let result_id = result.id;
        if col >= result.columns.len() {
            return;
        }
        let anchor = if extend {
            self.grid_selection
                .filter(|selection| selection.result == result_id)
                .map(|selection| selection.anchor)
                .unwrap_or((row, col))
        } else {
            (row, col)
        };
        self.grid_selection = Some(GridSelection {
            result: result_id,
            anchor,
            focus: (row, col),
        });
        self.selected_cell = Some((row, col));
        cx.notify();
    }

    /// 鼠标按下：开始拖拽选区（双击与 Shift 点击不进入拖拽）。
    pub(crate) fn begin_grid_drag(
        &mut self,
        row: usize,
        col: usize,
        event: &gpui::MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        // 每次按下都重置上一次拖拽的收尾状态：即使上次松手在网格外，也不会吞掉这次点击。
        self.drag_select = None;
        self.drag_moved = false;
        if event.click_count > 1 || event.modifiers.shift {
            return;
        }
        self.drag_select = Some((row, col));
        self.select_grid_cell(row, col, false, cx);
    }

    /// 拖过其它单元格时把焦点移过去，形成矩形选区。
    pub(crate) fn extend_grid_drag(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        if self.drag_select.is_none() {
            return;
        }
        let Some(result) = self.active_result() else {
            return;
        };
        let result_id = result.id;
        if col >= result.columns.len() {
            return;
        }
        let Some(selection) = self
            .grid_selection
            .filter(|selection| selection.result == result_id)
        else {
            return;
        };
        if selection.focus == (row, col) {
            return;
        }
        self.grid_selection = Some(GridSelection {
            focus: (row, col),
            ..selection
        });
        self.drag_moved = true;
        self.selected_cell = Some((row, col));
        cx.notify();
    }

    /// click 收尾：返回本次点击是否只是拖拽的尾巴（是则不再复制/折叠选区）。
    pub(crate) fn finish_grid_click(&mut self) -> bool {
        let dragged = self.drag_moved;
        self.drag_select = None;
        self.drag_moved = false;
        dragged
    }

    /// 按下表头右侧把手：记住起点与起始列宽，后续位移都相对它计算。
    pub(crate) fn begin_col_resize(
        &mut self,
        column: usize,
        x: gpui::Pixels,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.active_result() else {
            return;
        };
        if column >= view.columns.len() {
            return;
        }
        let start_w = self.col_display_width(view, column);
        self.col_resize = Some(ColResize {
            column,
            start_x: f32::from(x),
            start_w,
        });
        cx.notify();
    }

    /// 双击列分隔线：按「表头 + 前 200 行」的最宽文本自适应列宽。
    /// 交互与 gpui-kit 0.8.0 DataTable 的 `fit_col` 保持一致。
    pub(crate) fn fit_column(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.col_resize = None;
        let want = {
            let Some(result) = self.active_result() else {
                return;
            };
            let Some(column) = result.columns.get(index) else {
                return;
            };
            let mut widest = text_width(window, &column.name, CELL_FONT_SIZE);
            for row in result.rows.iter().take(FIT_SAMPLE_ROWS) {
                if let Some(value) = row.get(index) {
                    widest = widest.max(text_width(window, &value.display_text(), CELL_FONT_SIZE));
                }
            }
            // 两侧内边距 + 右边框 1px + 拖拽把手。
            widest + CELL_PAD_X + 1. + COL_HANDLE_W
        };

        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let Some(result) = tab.results.get_mut(tab.active_result) else {
            return;
        };
        if result.set_width(index, want) {
            cx.notify();
        }
    }

    /// 某列当前的显示宽度：拖过的用定值，未拖过的按视口剩余空间均分估算。
    /// 拖拽全程走 `start_w + 位移`，估算误差只影响起手的第一帧。
    pub(crate) fn col_display_width(&self, result: &ResultView, index: usize) -> f32 {
        if let Some(width) = result.fixed_width(index) {
            return width;
        }
        let auto = (0..result.columns.len())
            .filter(|i| result.fixed_width(*i).is_none())
            .count();
        if auto == 0 {
            return MIN_COL_W;
        }
        let fixed: f32 = (0..result.columns.len())
            .filter_map(|i| result.fixed_width(i))
            .sum();
        let each = (self.grid_viewport_width() - fixed) / auto as f32;
        each.clamp(MIN_COL_W, MAX_COL_W)
    }

    /// 拖拽中：左键仍按着就按绝对位移改宽度；已松开说明鼠标离开网格后释放，直接收尾。
    pub(crate) fn drag_col_resize(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(resize) = self.col_resize else {
            return;
        };
        if !event.dragging() {
            self.col_resize = None;
            return;
        }
        let width = resize.start_w + f32::from(event.position.x) - resize.start_x;
        if let Some(view) = self.active_result_mut() {
            if view.set_width(resize.column, width) {
                cx.notify();
            }
        }
    }

    /// 松开左键：结束拖拽。宽度已逐帧写入结果集，这里只清状态。
    pub(crate) fn end_col_resize(&mut self, cx: &mut Context<Self>) {
        if self.col_resize.take().is_some() {
            cx.notify();
        }
    }

    /// 行体视口宽度（上一帧量测值）。
    pub(crate) fn grid_viewport_width(&self) -> f32 {
        f32::from(self.grid_viewport.bounds().size.width)
    }

    /// 横向可滚动的最大距离：内容最小宽减去视口宽度。
    pub(crate) fn max_grid_h(&self) -> f32 {
        let total = self.active_result().map(|result| result.min_total_width());
        total.map_or(0., |total| (total - self.grid_viewport_width()).max(0.))
    }

    /// 滚轮在网格上：横向偏移自己算（竖向交给虚拟列表）。
    pub(crate) fn scroll_grid_h(&mut self, event: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(24.));
        let dx = if event.modifiers.shift {
            delta.y
        } else {
            delta.x
        };
        if dx == px(0.) || self.max_grid_h() <= 0. {
            return;
        }
        self.grid_h = (self.grid_h - f32::from(dx)).clamp(0., self.max_grid_h());
        cx.notify();
    }

    pub(crate) fn active_export_rows(&self) -> Option<(Vec<String>, Vec<Vec<String>>)> {
        let tab = self.active()?;
        tab_export_rows(tab, &self.core.results())
    }

    pub(crate) fn export_csv(&mut self, cx: &mut Context<Self>) {
        let Some((headers, rows)) = self.active_export_rows() else {
            self.status = "没有可导出的结果".into();
            self.status_kind = StatusKind::Idle;
            cx.notify();
            return;
        };
        match crate::csv::write_csv(&headers, &rows) {
            Ok(path) => {
                self.status = format!("已导出 {} 行 · {}", rows.len(), path.display());
                self.status_kind = StatusKind::Ok;
            }
            Err(error) => {
                self.status = format!("导出失败：{error}");
                self.status_kind = StatusKind::Error;
            }
        }
        cx.notify();
    }

    /// 「导出格式」菜单（F-72）：CSV / TSV / JSON / Markdown / SQL 脚本。
    pub(crate) fn export_menu_groups(&self) -> Vec<Vec<MenuEntry>> {
        use ExportFormat as F;
        let item = |action: MenuAction, label: &str, icon: IconName, key: &'static str| {
            MenuEntry::new(action, label, icon).key(key)
        };
        vec![
            vec![
                item(
                    MenuAction::Export(F::Csv),
                    "导出为 CSV",
                    IconName::Download,
                    "fmt-csv",
                ),
                item(
                    MenuAction::Export(F::Tsv),
                    "导出为 TSV",
                    IconName::Download,
                    "fmt-tsv",
                ),
                item(
                    MenuAction::Export(F::Json),
                    "导出为 JSON",
                    IconName::Braces,
                    "fmt-json",
                ),
                item(
                    MenuAction::Export(F::Markdown),
                    "导出为 Markdown",
                    IconName::SquareCode,
                    "fmt-markdown",
                ),
                item(
                    MenuAction::Export(F::Xlsx),
                    "导出为 Excel",
                    IconName::Table,
                    "fmt-xlsx",
                ),
            ],
            vec![
                item(
                    MenuAction::Export(F::SqlInsert),
                    "导出为 SQL INSERT",
                    IconName::SquareCode,
                    "fmt-sql-insert",
                ),
                item(
                    MenuAction::Export(F::SqlUpdate),
                    "导出为 SQL UPDATE",
                    IconName::SquareCode,
                    "fmt-sql-update",
                ),
            ],
            vec![
                item(
                    MenuAction::CopyAs(F::Tsv),
                    "复制为 TSV",
                    IconName::Copy,
                    "copy-tsv",
                ),
                item(
                    MenuAction::CopyAs(F::Json),
                    "复制为 JSON",
                    IconName::Copy,
                    "copy-json",
                ),
                item(
                    MenuAction::CopyAs(F::Markdown),
                    "复制为 Markdown",
                    IconName::Copy,
                    "copy-markdown",
                ),
                item(
                    MenuAction::CopyAs(F::SqlInsert),
                    "复制为 SQL INSERT",
                    IconName::Copy,
                    "copy-sql-insert",
                ),
            ],
        ]
    }

    pub(crate) fn render_export_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = self.export_menu_groups();
        self.render_menu(groups, cx)
    }

    /// 按格式导出当前结果（F-72）。类型信息与排序都取自当前结果集。
    pub(crate) fn set_status(
        &mut self,
        message: impl Into<String>,
        kind: StatusKind,
        cx: &mut Context<Self>,
    ) {
        self.status = message.into();
        self.status_kind = kind;
        cx.notify();
    }

    /// 收集当前结果的导出负载（列名 / 带类型的行 / 表名 / 主键 / 方言）。
    pub(crate) fn export_payload(&self, scope: PayloadScope) -> Result<ExportPayload, String> {
        let session = self
            .session
            .clone()
            .ok_or_else(|| "请先在「连接」面板连接一个数据源".to_string())?;
        let dialect = self
            .core
            .session_kind(&session)
            .unwrap_or(rustdb_db_api::DbKind::Sqlite);
        let tab = self
            .active()
            .ok_or_else(|| "从左侧连接打开查询".to_string())?;
        // 只有多格选区才走子矩阵；单格 / 无选区保持「整结果」语义。
        let selected = if scope == PayloadScope::Selection {
            tab_selection_typed(tab, self.grid_selection)
        } else {
            None
        };
        match selected {
            Some(selected) => Ok(ExportPayload {
                dialect,
                columns: selected.columns,
                rows: selected.rows,
                table: selected.table,
                key_columns: selected.key_columns,
                selection: Some(selected.dimensions),
                missing_key_columns: selected.missing_key_columns,
            }),
            None => {
                let (columns, rows, table, key_columns) =
                    tab_export_typed(tab, &self.core.results())
                        .ok_or_else(|| "没有可导出的结果".to_string())?;
                Ok(ExportPayload {
                    dialect,
                    columns,
                    rows,
                    table,
                    key_columns,
                    selection: None,
                    missing_key_columns: false,
                })
            }
        }
    }

    pub(crate) fn export_result(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        let payload = match self.export_payload(PayloadScope::All) {
            Ok(payload) => payload,
            Err(message) => {
                let kind = if message == "没有可导出的结果" || message.starts_with("从左侧连接")
                {
                    StatusKind::Idle
                } else {
                    StatusKind::Error
                };
                self.set_status(message, kind, cx);
                return;
            }
        };
        if format.is_sql() && payload.table.is_none() {
            self.set_status(
                "SQL 导出仅在「打开表」的结果上可用（需要表名与主键列）",
                StatusKind::Error,
                cx,
            );
            return;
        }
        let options = payload.options(format);
        match rustdb_core::export::encode_bytes(format, &payload.columns, &payload.rows, &options) {
            Ok(bytes) => match crate::csv::write_bytes(&bytes, format.extension()) {
                Ok(path) => {
                    let message = format!(
                        "已导出 {} 行 · {} · {}",
                        payload.rows.len(),
                        format.label(),
                        path.display()
                    );
                    self.set_status(message, StatusKind::Ok, cx);
                }
                Err(error) => {
                    self.set_status(format!("导出失败：{error}"), StatusKind::Error, cx);
                }
            },
            Err(error) => {
                self.set_status(format!("导出失败：{error}"), StatusKind::Error, cx);
            }
        }
    }

    /// 复制为指定格式（F-72）：与导出共用同一编码器，只换目的地。
    /// 有多格选区时复制选区子矩阵，否则复制整结果。
    pub(crate) fn copy_result_as(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        let payload = match self.export_payload(PayloadScope::Selection) {
            Ok(payload) => payload,
            Err(message) => {
                let kind = if message == "没有可导出的结果" || message.starts_with("从左侧连接")
                {
                    StatusKind::Idle
                } else {
                    StatusKind::Error
                };
                self.set_status(message, kind, cx);
                return;
            }
        };
        if format.is_sql() && payload.table.is_none() {
            self.set_status(
                "SQL 复制仅在「打开表」的结果上可用（需要表名与主键列）",
                StatusKind::Error,
                cx,
            );
            return;
        }
        if matches!(format, ExportFormat::SqlUpdate) && payload.missing_key_columns {
            self.set_status(
                "SQL UPDATE 复制需要选区包含全部主键列",
                StatusKind::Error,
                cx,
            );
            return;
        }
        let options = payload.options(format);
        match rustdb_core::export::encode(format, &payload.columns, &payload.rows, &options) {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                let scope = match payload.selection {
                    Some((rows, cols)) => format!("{rows} 行 × {cols} 列"),
                    None => format!("{} 行", payload.rows.len()),
                };
                let message = format!("已复制 {scope} · {}", format.label());
                self.set_status(message, StatusKind::Ok, cx);
            }
            Err(error) => {
                self.set_status(format!("复制失败：{error}"), StatusKind::Error, cx);
            }
        }
    }
}
