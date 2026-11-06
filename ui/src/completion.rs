//! SQL 补全：关键字、常用函数、当前库的表 / 视图名。
//!
//! 只做**词法前缀**匹配，不解析 SQL 结构（别名推导留给后续里程碑）。
//! 数据源由 `MainView` 在元数据加载后写入 `CompletionCatalog`，
//! provider 通过共享 cell 读取，避免每次按键重建闭包。

use gpui::{App, Task, Window};
use gpui_component::input::CompletionProvider;
use gpui_component::RopeExt as _;
use lsp_types::{CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse};
use ropey::Rope;
use rustdb_db_api::ObjectKind;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// 补全候选全集（随连接 / 切库变化）。
#[derive(Default, Clone)]
pub struct CompletionCatalog {
    /// 当前库下的对象名（表 / 视图 / 物化视图）。
    pub objects: Vec<String>,
    /// 当前 schema 名（PG 用得上）。
    pub schemas: Vec<String>,
    /// 表 / 视图的结构摘要（悬停与诊断用）。
    pub tables: Vec<TableHint>,
    /// 已取到的表结构（键 = 表名小写）；列校验与悬停共用。
    pub columns: HashMap<String, Vec<ColumnHint>>,
}

/// 悬停卡片里展示的对象摘要。
#[derive(Debug, Clone)]
pub struct TableHint {
    pub name: String,
    pub kind: ObjectKind,
    pub schema: Option<String>,
    pub row_estimate: Option<u64>,
    pub comment: Option<String>,
}

/// 悬停卡片里的列信息。
#[derive(Debug, Clone)]
pub struct ColumnHint {
    pub name: String,
    pub type_label: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub comment: Option<String>,
}

/// SQL 关键字：只保留日常写查询真正会用到的部分，避免长列表淹没表名。
pub const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS NULL",
    "IS NOT NULL",
    "IN",
    "EXISTS",
    "BETWEEN",
    "LIKE",
    "ILIKE",
    "DISTINCT",
    "UNION",
    "UNION ALL",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ASC",
    "DESC",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "CAST",
];

/// 常用函数（括号一起插入，省一次输入）。
pub const FUNCTIONS: &[&str] = &[
    "COUNT(",
    "SUM(",
    "AVG(",
    "MIN(",
    "MAX(",
    "COALESCE(",
    "NULLIF(",
    "CAST(",
    "ROUND(",
    "ABS(",
    "LENGTH(",
    "LOWER(",
    "UPPER(",
    "TRIM(",
    "SUBSTRING(",
    "REPLACE(",
    "DATE_TRUNC(",
    "NOW(",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "EXTRACT(",
    "ROW_NUMBER(",
    "RANK(",
    "DENSE_RANK(",
    "LAG(",
    "LEAD(",
    "ARRAY_AGG(",
    "STRING_AGG(",
    "JSON_EXTRACT(",
    "ROW_NUMBER(",
];

pub struct SqlCompletionProvider {
    catalog: Rc<RefCell<CompletionCatalog>>,
}

impl SqlCompletionProvider {
    pub fn new(catalog: Rc<RefCell<CompletionCatalog>>) -> Self {
        Self { catalog }
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let offset = offset.min(rope.len());
        // 光标所在的词：只取连续标识符字符。
        let prefix = word_prefix(rope, offset);
        let needle = prefix.to_lowercase();

        let mut items: Vec<CompletionItem> = Vec::new();
        let mut push = |label: &str, kind: CompletionItemKind| {
            if !needle.is_empty() && !label.to_lowercase().starts_with(&needle) {
                return;
            }
            items.push(CompletionItem {
                label: label.to_string(),
                kind: Some(kind),
                insert_text: Some(label.to_string()),
                ..Default::default()
            });
        };

        for keyword in KEYWORDS {
            push(keyword, CompletionItemKind::KEYWORD);
        }
        for function in FUNCTIONS {
            push(function, CompletionItemKind::FUNCTION);
        }
        let catalog = self.catalog.borrow();
        for schema in &catalog.schemas {
            push(schema, CompletionItemKind::MODULE);
        }
        for object in &catalog.objects {
            push(object, CompletionItemKind::CLASS);
        }

        // 候选过多时只保留前 80 条，避免弹层卡顿。
        items.truncate(80);
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// 只有输入标识符字符或 `.` 才弹补全；空格 / 回车不打扰。
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.')
    }
}

/// 光标左侧的标识符前缀（`[A-Za-z0-9_$]`）。
fn word_prefix(rope: &Rope, offset: usize) -> String {
    let mut start = offset;
    while start > 0 {
        let Some(ch) = rope.char_at(start - 1) else {
            break;
        };
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            start -= 1;
        } else {
            break;
        }
    }
    rope.slice(start..offset).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_stops_at_whitespace_and_punctuation() {
        let rope = Rope::from_str("SELECT * FROM ord");
        assert_eq!(word_prefix(&rope, rope.len()), "ord");

        let rope = Rope::from_str("SELECT a.b");
        assert_eq!(word_prefix(&rope, rope.len()), "b");

        let rope = Rope::from_str("SELECT ");
        assert_eq!(word_prefix(&rope, rope.len()), "", "空格后没有前缀");
    }

    #[test]
    fn catalog_lists_keywords_and_objects() {
        assert!(KEYWORDS.contains(&"SELECT"));
        assert!(FUNCTIONS.iter().any(|f| f.starts_with("COUNT")));
    }
}
