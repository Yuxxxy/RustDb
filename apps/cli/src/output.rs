//! 输出渲染：网格表格 / JSON 信封。
//!
//! 单元格文本统一走 core 的 `cell_text`（NULL 处理与桌面端一致），
//! 行编码复用 core 的 JSON 导出，CLI 不另写一套格式规则。

use rustdb_core::export::{cell_text, encode, ExportFormat, ExportOptions};
use rustdb_db_api::Row;

/// 网格表格：按显示宽度对齐（中文按两列宽计算）。
pub fn grid(headers: &[String], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let mut widths: Vec<usize> = headers.iter().map(|head| display_width(head)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(display_width(cell));
            }
        }
    }
    let mut out = String::new();
    out.push_str(&join_padded(headers, &widths));
    out.push('\n');
    let separator: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    out.push_str(&separator.join("-+-"));
    out.push('\n');
    for row in rows {
        out.push_str(&join_padded(row, &widths));
        out.push('\n');
    }
    out
}

fn join_padded(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            let width = widths.get(index).copied().unwrap_or(0);
            format!("{cell}{}", " ".repeat(width - display_width(cell)))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// 单元格文本：NULL 与桌面端一致（表格里显示 `NULL`，CSV 里是空字段）。
pub fn cells(row: &Row, options: &ExportOptions) -> Vec<String> {
    row.iter().map(|value| cell_text(value, options)).collect()
}

/// 行数组的 JSON 值：复用 core 的行编码（值 → JSON 的规则只有一处）。
pub fn rows_json(columns: &[String], rows: &[Row], options: &ExportOptions) -> serde_json::Value {
    let text = encode(ExportFormat::Json, columns, rows, options).unwrap_or_else(|_| {
        serde_json::to_string(&Vec::<serde_json::Value>::new()).unwrap_or_default()
    });
    serde_json::from_str(&text).unwrap_or(serde_json::Value::Array(Vec::new()))
}

/// 显示宽度：CJK / 全角按两列计算，其余按一列。
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(ch: char) -> usize {
    let code = ch as u32;
    let wide = (0x1100..=0x115F).contains(&code)   // 谚文字母
        || (0x2E80..=0xA4CF).contains(&code) // CJK 部首、假名、汉字
        || (0xAC00..=0xD7A3).contains(&code) // 谚文音节
        || (0xF900..=0xFAFF).contains(&code) // CJK 兼容表意
        || (0xFE30..=0xFE6F).contains(&code) // CJK 兼容形式
        || (0xFF00..=0xFF60).contains(&code) // 全角
        || (0xFFE0..=0xFFE6).contains(&code);
    if wide {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::Value;

    #[test]
    fn grid_aligns_columns_by_display_width() {
        let headers = vec!["id".to_string(), "name".to_string()];
        let rows = vec![vec!["1".to_string(), "张三".to_string()]];
        let text = grid(&headers, &rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("id | name"));
        // 中文占两列：第二列的表头与数据右边界对齐。
        assert_eq!(display_width(lines[0]), display_width(lines[2]));
    }

    #[test]
    fn null_cells_use_the_configured_null_text() {
        let options = ExportOptions {
            null_text: "NULL".into(),
            ..ExportOptions::default()
        };
        let row: Row = vec![Value::Null, Value::Text("".into())];
        let texts = cells(&row, &options);
        assert_eq!(texts[0], "NULL");
        assert_eq!(texts[1], "", "空串与 NULL 必须区分");
    }

    #[test]
    fn rows_json_uses_the_core_encoding() {
        let columns = vec!["id".to_string()];
        let rows: Vec<Row> = vec![vec![Value::Int(1)], vec![Value::Null]];
        let value = rows_json(&columns, &rows, &ExportOptions::default());
        let items = value.as_array().expect("数组");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], serde_json::json!(1));
        assert!(items[1]["id"].is_null());
    }
}
