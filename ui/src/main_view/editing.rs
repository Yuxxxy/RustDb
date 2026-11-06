//! Result editing helpers: cell parsing, row insert/delete, paste, generators,
//! export data extraction. Split out of `main_view.rs`; these functions do not
//! touch `MainView` state and only operate on `ResultView` / `SqlTab`.

use super::*;

pub(super) fn parse_cell_text(
    text: &str,
    logical: &rustdb_db_api::LogicalType,
) -> Result<Value, String> {
    use rustdb_db_api::LogicalType;
    let raw = text.trim();
    match logical {
        LogicalType::Text { .. } => Ok(Value::Text(text.into())),
        LogicalType::Json => {
            serde_json::from_str::<serde_json::Value>(raw)
                .map_err(|_| "JSON 格式不合法".to_string())?;
            Ok(Value::Json(raw.into()))
        }
        LogicalType::Int { .. } => raw
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("需要整数，收到「{text}」")),
        LogicalType::Float { .. } => raw
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("需要数字，收到「{text}」")),
        LogicalType::Decimal { .. } => {
            if raw.is_empty()
                || !raw
                    .chars()
                    .all(|c| c.is_ascii_digit() || "+-.eE".contains(c))
            {
                return Err(format!("需要定点数，收到「{text}」"));
            }
            Ok(Value::Decimal(raw.into()))
        }
        LogicalType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" | "是" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" | "否" => Ok(Value::Bool(false)),
            _ => Err(format!("需要布尔值（true/false），收到「{text}」")),
        },
        LogicalType::Date => chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("需要日期 YYYY-MM-DD，收到「{text}」")),
        LogicalType::Time => chrono::NaiveTime::parse_from_str(raw, "%H:%M:%S")
            .or_else(|_| chrono::NaiveTime::parse_from_str(raw, "%H:%M:%S%.f"))
            .map(Value::Time)
            .map_err(|_| format!("需要时间 HH:MM:SS，收到「{text}」")),
        LogicalType::Timestamp => chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S"))
            .map(Value::Timestamp)
            .map_err(|_| format!("需要时间戳 YYYY-MM-DD HH:MM:SS，收到「{text}」")),
        LogicalType::TimestampTz => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|value| Value::TimestampTz(value.with_timezone(&chrono::Utc)))
            .map_err(|_| format!("需要带时区的时间（RFC3339），收到「{text}」")),
        LogicalType::Uuid => {
            let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            if hex.len() != 32 {
                return Err(format!("需要 UUID，收到「{text}」"));
            }
            let mut bytes = [0u8; 16];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                    .map_err(|_| "UUID 解析失败".to_string())?;
            }
            Ok(Value::Uuid(bytes))
        }
        LogicalType::Bytes => Ok(Value::Bytes(raw.as_bytes().to_vec().into())),
        LogicalType::Array(_) => Err("数组列暂不支持在网格里编辑".into()),
        LogicalType::Unknown(name) => Err(format!("未知类型 {name} 不支持编辑")),
    }
}

/// 确保结果集有变更集（懒创建）。
pub(super) fn ensure_change_set(result: &mut ResultView) -> Result<(), String> {
    if result.changes.is_some() {
        return Ok(());
    }
    let Some((path, key)) = result.editable.clone() else {
        return Err("该结果集不可编辑：只有「打开表」的结果支持编辑".into());
    };
    let columns: Vec<String> = result
        .columns
        .iter()
        .map(|column| column.name.to_string())
        .collect();
    result.changes = Some(rustdb_core::ChangeSet::new(path, columns, key));
    Ok(())
}

/// 一次单元格编辑：写入变更集并同步网格显示。返回待提交条数。
pub(super) fn apply_cell_edit(
    result: &mut ResultView,
    row: usize,
    col: usize,
    text: Option<String>,
) -> Result<usize, String> {
    let Some(column) = result.columns.get(col).cloned() else {
        return Err("列不存在".into());
    };
    let Some(old) = result.rows.get(row).cloned() else {
        return Err("行不存在".into());
    };
    let value = match text {
        None => Value::Null,
        Some(text) => parse_cell_text(&text, &column.logical_type)?,
    };
    if !column.nullable && value.is_null() {
        return Err(format!("{} 不允许为 NULL", column.name));
    }
    ensure_change_set(result)?;
    let set = result.changes.as_mut().expect("刚刚创建");
    set.record_cell(row, col, old, value.clone())?;
    result.rows[row][col] = value;
    Ok(set.len())
}

/// 标记删除一行。新增行还没进库，删除它等于撤销这条新增。
pub(super) fn delete_row(result: &mut ResultView, row: usize) -> Result<usize, String> {
    let old = result
        .rows
        .get(row)
        .cloned()
        .ok_or_else(|| "行不存在".to_string())?;
    ensure_change_set(result)?;
    let is_insert = result
        .changes
        .as_ref()
        .and_then(|set| set.row_state(row))
        .map(|kind| kind == rustdb_core::ChangeKind::Insert)
        .unwrap_or(false);
    let count = {
        let set = result.changes.as_mut().expect("刚刚创建");
        set.record_delete(row, old)?;
        set.len()
    };
    if is_insert {
        result.rows.remove(row);
        result.loaded = result.rows.len() as u64;
    }
    Ok(count)
}

/// 追加一行（所有列 NULL），并记入变更集。
pub(super) fn insert_empty_row(result: &mut ResultView) -> Result<usize, String> {
    ensure_change_set(result)?;
    let row = result.rows.len();
    let values = vec![Value::Null; result.columns.len()];
    result.rows.push(values.clone());
    result.loaded = result.rows.len() as u64;
    let set = result.changes.as_mut().expect("刚刚创建");
    set.record_insert(row, values)?;
    Ok(set.len())
}

/// 追加 N 行（所有列 NULL），并记入变更集。
pub(super) fn insert_rows(result: &mut ResultView, count: usize) -> Result<usize, String> {
    for _ in 0..count {
        insert_empty_row(result)?;
    }
    Ok(result.changes.as_ref().map(|set| set.len()).unwrap_or(0))
}

/// 克隆一行：主键交给数据库默认值（自增 / 序列才可靠）。
pub(super) fn clone_row(result: &mut ResultView, row: usize) -> Result<usize, String> {
    let old = result
        .rows
        .get(row)
        .cloned()
        .ok_or_else(|| "行不存在".to_string())?;
    ensure_change_set(result)?;
    let new_row = result.rows.len();
    result.rows.push(old.clone());
    result.loaded = result.rows.len() as u64;
    let set = result.changes.as_mut().expect("刚刚创建");
    set.record_clone(new_row, old, true)?;
    Ok(set.len())
}

/// 把某单元格交回数据库默认值（新增行省略该列，修改行用 `SET 列 = DEFAULT`）。
pub(super) fn set_cell_default(
    result: &mut ResultView,
    row: usize,
    col: usize,
) -> Result<usize, String> {
    let old = result
        .rows
        .get(row)
        .cloned()
        .ok_or_else(|| "行不存在".to_string())?;
    ensure_change_set(result)?;
    let set = result.changes.as_mut().expect("刚刚创建");
    set.record_set_default(row, col, old)?;
    result.rows[row][col] = Value::Null;
    Ok(set.len())
}

/// 撤销一行的待提交变更：修改 / 删除行恢复原值，新增行整行作废。
pub(super) fn revert_row(result: &mut ResultView, row: usize) -> Result<usize, String> {
    let change = result
        .changes
        .as_ref()
        .and_then(|set| {
            set.changes()
                .iter()
                .find(|change| change.row == row)
                .cloned()
        })
        .ok_or_else(|| "这一行没有待提交的变更".to_string())?;
    {
        let set = result.changes.as_mut().expect("存在变更集");
        set.revert(row);
    }
    match change.kind {
        rustdb_core::ChangeKind::Insert => {
            result.rows.remove(row);
            result.loaded = result.rows.len() as u64;
        }
        _ => {
            if let Some(values) = result.rows.get_mut(row) {
                *values = change.old;
            }
        }
    }
    Ok(result.changes.as_ref().map(|set| set.len()).unwrap_or(0))
}

/// TSV 首行是否是表头（与结果集列名一致）。
pub(super) fn is_header_row(result: &ResultView, row: &[String]) -> bool {
    !row.is_empty()
        && row.len() == result.columns.len()
        && row
            .iter()
            .zip(result.columns.iter())
            .all(|(cell, column)| cell.trim().eq_ignore_ascii_case(column.name.as_ref()))
}

/// 把 TSV 粘贴进网格：从选中单元格开始，行数不够就补新行。
///
/// 空字段按 NULL 处理（与导出格式一致：NULL 导出成空字段）。
pub(super) fn paste_tsv_into(
    result: &mut ResultView,
    grid: &[Vec<String>],
    start_row: usize,
    start_col: usize,
) -> Result<usize, String> {
    let columns = result.columns.len();
    ensure_change_set(result)?;
    let mut written = 0usize;
    for (row_offset, cells) in grid.iter().enumerate() {
        let row = start_row + row_offset;
        if row >= result.rows.len() {
            let values = vec![Value::Null; columns];
            result.rows.push(values.clone());
            result.loaded = result.rows.len() as u64;
            let set = result.changes.as_mut().expect("刚刚创建");
            set.record_insert(row, values)?;
        }
        for (col_offset, text) in cells.iter().enumerate() {
            let col = start_col + col_offset;
            if col >= columns {
                break;
            }
            let Some(column) = result.columns.get(col).cloned() else {
                break;
            };
            let value = if text.is_empty() {
                Value::Null
            } else {
                parse_cell_text(text, &column.logical_type)?
            };
            if !column.nullable && value.is_null() {
                return Err(format!("{} 不允许为 NULL", column.name));
            }
            let old = result.rows[row].clone();
            let set = result.changes.as_mut().expect("刚刚创建");
            set.record_cell(row, col, old, value.clone())?;
            result.rows[row][col] = value;
            written += 1;
        }
    }
    Ok(written)
}

/// 解析生成表达式；`fk:列名` 在这里从当前结果取候选值。
pub(super) fn resolve_generator_spec(
    expr: &str,
    result: &ResultView,
) -> Result<rustdb_core::GeneratorSpec, String> {
    let trimmed = expr.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("fk:") || lower.starts_with("ref:") {
        let name = trimmed
            .split_once(':')
            .map(|(_, rest)| rest.trim())
            .unwrap_or_default();
        if name.is_empty() {
            return Err("外键引用需要列名，例如 fk:customer_id".into());
        }
        let index = result
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("结果集里没有列「{name}」"))?;
        let mut values: Vec<Value> = Vec::new();
        for row in &result.rows {
            let Some(value) = row.get(index) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            if !values.iter().any(|seen| seen == value) {
                values.push(value.clone());
            }
        }
        if values.is_empty() {
            return Err(format!(
                "列「{name}」没有可引用的值（结果集为空或全为 NULL）"
            ));
        }
        return Ok(rustdb_core::GeneratorSpec::ForeignKey { values });
    }
    rustdb_core::parse_spec(trimmed)
}

/// 生成值 → 目标列的类型；同类型直接用，异类型按文本重新解析。
pub(super) fn coerce_generated(
    value: Value,
    logical: &rustdb_db_api::LogicalType,
) -> Result<Value, String> {
    use rustdb_db_api::LogicalType;
    match (&value, logical) {
        (Value::Bool(_), LogicalType::Bool)
        | (Value::Int(_), LogicalType::Int { .. })
        | (Value::UInt(_), LogicalType::Int { .. })
        | (Value::Float(_), LogicalType::Float { .. })
        | (Value::Decimal(_), LogicalType::Decimal { .. })
        | (Value::Text(_), LogicalType::Text { .. })
        | (Value::Json(_), LogicalType::Json)
        | (Value::Date(_), LogicalType::Date)
        | (Value::Time(_), LogicalType::Time)
        | (Value::Timestamp(_), LogicalType::Timestamp)
        | (Value::TimestampTz(_), LogicalType::TimestampTz)
        | (Value::Uuid(_), LogicalType::Uuid)
        | (Value::Bytes(_), LogicalType::Bytes) => Ok(value),
        _ => parse_cell_text(&value.display_text(), logical),
    }
}

/// 把生成的值写成一组新行：目标列填生成值，其余列交给数据库默认值。
pub(super) fn apply_generated(
    result: &mut ResultView,
    column: usize,
    values: &[Value],
) -> Result<usize, String> {
    let Some(meta) = result.columns.get(column).cloned() else {
        return Err("列不存在".to_string());
    };
    let columns = result.columns.len();
    ensure_change_set(result)?;
    for value in values {
        let value = coerce_generated(value.clone(), &meta.logical_type)?;
        if !meta.nullable && value.is_null() {
            return Err(format!("{} 不允许为 NULL", meta.name));
        }
        let row = result.rows.len();
        let mut row_values = vec![Value::Null; columns];
        row_values[column] = value;
        result.rows.push(row_values.clone());
        result.loaded = result.rows.len() as u64;
        let set = result.changes.as_mut().expect("刚刚创建");
        set.record_insert(row, row_values.clone())?;
        // 其余列交给数据库默认值，否则 NOT NULL 列会让整批插入失败。
        for col in 0..columns {
            if col == column {
                continue;
            }
            set.record_set_default(row, col, row_values.clone())?;
        }
    }
    Ok(result.changes.as_ref().expect("刚刚创建").len())
}

/// 流式追加：刷新已加载行，并把视图切到新出现的结果集。
pub(super) fn refresh_result(
    tab: &mut SqlTab,
    store: &ResultStore,
    set_id: ResultSetId,
    loaded: u64,
) {
    if ensure_result(tab, store, set_id) {
        tab.active_result = tab.results.len().saturating_sub(1);
    }
    let Some(meta) = store.meta(set_id) else {
        return;
    };
    let rows = store.rows(set_id, 0, MAX_VIEW_ROWS);
    if let Some(view) = tab.results.iter_mut().find(|r| r.id == set_id) {
        view.rows = rows;
        view.loaded = loaded.max(meta.loaded_rows);
        view.truncated = meta.truncated || view.loaded > MAX_VIEW_ROWS as u64;
        view.affected = meta.affected;
    }
}

/// 导出数据：表头 + 全部已加载行，按当前客户端排序展开。
pub(super) fn tab_export_rows(
    tab: &SqlTab,
    store: &ResultStore,
) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let view = tab.results.get(tab.active_result)?;
    if view.columns.is_empty() {
        return None;
    }
    let values = store.rows(view.id, 0, view.loaded.max(1) as usize);
    let indices = ordered_indices(&values, view.sort, &[]);
    let headers: Vec<String> = view.columns.iter().map(|c| c.name.to_string()).collect();
    let rows: Vec<Vec<String>> = indices
        .into_iter()
        .map(|index| {
            values[index]
                .iter()
                .map(|value| value.export_text())
                .collect()
        })
        .collect();
    Some((headers, rows))
}

/// 结构化导出（F-72）：列名 + 带类型的行（SQL 导出需要真实类型）+ 表名/主键列。
#[allow(clippy::type_complexity)]
pub(super) fn tab_export_typed(
    tab: &SqlTab,
    store: &ResultStore,
) -> Option<(Vec<String>, Vec<Vec<Value>>, Option<String>, Vec<usize>)> {
    let view = tab.results.get(tab.active_result)?;
    if view.columns.is_empty() {
        return None;
    }
    let values = store.rows(view.id, 0, view.loaded.max(1) as usize);
    let indices = ordered_indices(&values, view.sort, &[]);
    let columns: Vec<String> = view.columns.iter().map(|c| c.name.to_string()).collect();
    let rows: Vec<Vec<Value>> = indices.iter().map(|index| values[*index].clone()).collect();
    let (table, keys) = match view.editable.as_ref() {
        Some((path, pk)) => (Some(table_ref(path)), pk.clone()),
        None => (None, Vec::new()),
    };
    Some((columns, rows, table, keys))
}
