//! 宽容语义诊断（F-61）。
//!
//! 只基于**当前已加载的元数据**做三件低风险的事：
//! * 标记 FROM / JOIN / UPDATE / INTO 后面不存在的表；
//! * 对已缓存表结构的 `别名.列` 标记不存在的列；
//! * 标记指向不存在表的限定名（未知别名）。
//!
//! 明确不做完整解析：CTE、子查询、表函数、动态 SQL、其他 schema 下的对象
//! 一律跳过；元数据或列信息缺失时把报告标为「无法完全校验」而不是报错。
//! 诊断只用于编辑器波浪线与悬停，**从不阻止执行**。

use crate::completion::{ColumnHint, CompletionCatalog};
use gpui::{App, SharedString, Task, Window};
use gpui_component::input::HoverProvider;
use gpui_component::RopeExt as _;
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use ropey::Rope;
use rustdb_sql_lang::{classify, split_statements, tokens, StatementKind, Token, TokenKind};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

const CLAUSE_END: &[&str] = &[
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "ON",
    "USING",
    "SET",
    "VALUES",
    "RETURNING",
    "WINDOW",
    "QUALIFY",
    "FOR",
    "INTO",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "FULL",
    "CROSS",
    "NATURAL",
    "OUTER",
    "AS",
];

/// 可以出现在表名之前、但不代表表名的词。
const REF_PREFIX: &[&str] = &["ONLY", "LATERAL", "IF", "NOT", "EXISTS", "TABLE"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// 相对整段 SQL 的字节范围。
    pub range: Range<usize>,
    pub severity: Severity,
    /// 稳定的诊断代码，供后续 quick fix 使用。
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub issues: Vec<Issue>,
    /// 元数据缺失、含动态 SQL 或部分表没有列信息：只能部分校验。
    pub incomplete: bool,
    pub note: Option<String>,
}

impl Report {
    fn issue(
        &mut self,
        range: Range<usize>,
        severity: Severity,
        code: &'static str,
        message: String,
    ) {
        self.issues.push(Issue {
            range,
            severity,
            code,
            message,
        });
    }

    fn incomplete(&mut self, note: impl Into<String>) {
        self.incomplete = true;
        if self.note.is_none() {
            self.note = Some(note.into());
        }
    }
}

struct Lookup<'a> {
    tables: HashSet<String>,
    schemas: HashSet<String>,
    columns: &'a HashMap<String, Vec<ColumnHint>>,
}

impl<'a> Lookup<'a> {
    fn new(catalog: &'a CompletionCatalog) -> Option<Self> {
        let mut tables = HashSet::new();
        for table in &catalog.tables {
            tables.insert(table.name.to_lowercase());
            if let Some(schema) = &table.schema {
                tables.insert(format!(
                    "{}.{}",
                    schema.to_lowercase(),
                    table.name.to_lowercase()
                ));
            }
        }
        // 还没填富信息的旧状态：至少用对象名兜底。
        if tables.is_empty() {
            for object in &catalog.objects {
                tables.insert(object.to_lowercase());
            }
        }
        if tables.is_empty() {
            return None;
        }
        Some(Self {
            tables,
            schemas: catalog
                .schemas
                .iter()
                .map(|schema| schema.to_lowercase())
                .collect(),
            columns: &catalog.columns,
        })
    }

    fn has_table(&self, name: &str) -> bool {
        self.tables.contains(&name.to_lowercase())
    }
}

/// 校验整段编辑器文本；元数据为空时返回「无法完全校验」。
pub fn validate(sql: &str, catalog: &CompletionCatalog) -> Report {
    let mut report = Report::default();
    let Some(lookup) = Lookup::new(catalog) else {
        report.incomplete("元数据未加载，无法校验对象名");
        return report;
    };

    let mut saw_statement = false;
    for statement in split_statements(sql) {
        let kind = classify(&statement.text);
        if !matches!(
            kind,
            StatementKind::Select
                | StatementKind::Insert
                | StatementKind::Update
                | StatementKind::Delete
        ) {
            continue;
        }
        saw_statement = true;

        let all = tokens(&statement.text);
        let significant: Vec<&Token> = all.iter().filter(|token| !token.is_trivia()).collect();
        if significant.iter().any(|token| {
            matches!(
                token.upper().as_str(),
                "EXEC" | "EXECUTE" | "SP_EXECUTESQL" | "PREPARE"
            )
        }) {
            report.incomplete("包含动态 SQL，无法完全校验");
            continue;
        }

        let ctes = collect_ctes(&significant);
        let mut refs = Vec::new();
        let mut consumed = HashSet::new();
        collect_table_refs(&significant, &mut refs, &mut consumed);

        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut alias_count = 0usize;
        for table_ref in &refs {
            if table_ref.is_function || table_ref.dynamic {
                continue;
            }
            let name = table_ref.name.to_lowercase();
            let is_cte = table_ref.schema.is_none() && ctes.contains(&name);
            let schema_known = table_ref.schema.as_ref().map_or(true, |schema| {
                lookup.schemas.contains(&schema.to_lowercase())
            });
            if !is_cte && schema_known && !lookup.has_table(&name) {
                report.issue(
                    offset(&table_ref.range, statement.start),
                    Severity::Error,
                    "unknown_table",
                    format!("表 “{}” 不在当前元数据中", table_ref.name),
                );
                continue;
            }
            let key = table_ref
                .alias
                .as_deref()
                .unwrap_or(&table_ref.name)
                .to_lowercase();
            if table_ref.alias.is_some() {
                alias_count += 1;
            }
            aliases.entry(key).or_insert_with(|| name.clone());
            aliases.entry(name.clone()).or_insert_with(|| name.clone());
        }

        check_qualified_columns(
            &significant,
            &consumed,
            statement.start,
            &aliases,
            alias_count,
            &ctes,
            &lookup,
            &mut report,
        );

        // 已知表但没缓存列结构：列校验只能跳过。
        for table in aliases.values() {
            if lookup.has_table(table) && !lookup.columns.contains_key(table) {
                report.incomplete(format!("表 {} 的列信息未加载，列校验已跳过", table));
                break;
            }
        }
    }

    if !saw_statement && !sql.trim().is_empty() {
        // DDL / 事务 / SHOW 等不在本检查器的范围内。
        report.incomplete("该语句不在语义校验范围内");
    }
    report
}

/// 一段 FROM / JOIN 后解析出的表引用。
struct TableRef {
    schema: Option<String>,
    name: String,
    /// 表名 token 在语句内的字节范围。
    range: Range<usize>,
    alias: Option<String>,
    is_function: bool,
    /// 名称里带占位符 / 变量：无法静态判断。
    dynamic: bool,
}

fn collect_table_refs(
    significant: &[&Token],
    out: &mut Vec<TableRef>,
    consumed: &mut HashSet<usize>,
) {
    let mut index = 0usize;
    while index < significant.len() {
        let token = significant[index];
        let upper = token.upper();
        let keyword = token.kind == TokenKind::Word;
        if keyword && (upper == "FROM" || upper == "USING") {
            index += 1;
            loop {
                let Some((table_ref, next)) = parse_table_ref(significant, index, false) else {
                    index += 1;
                    break;
                };
                consumed.insert(significant[index].start);
                out.push(table_ref);
                index = next;
                if significant
                    .get(index)
                    .is_some_and(|token| token.text == ",")
                {
                    index += 1;
                    continue;
                }
                break;
            }
            continue;
        }
        if keyword && (upper == "JOIN" || upper == "UPDATE" || upper == "INTO") {
            let allow_columns = upper == "INTO";
            if let Some((table_ref, next)) = parse_table_ref(significant, index + 1, allow_columns)
            {
                consumed.insert(significant[index + 1].start);
                out.push(table_ref);
                index = next;
                continue;
            }
        }
        if keyword && upper == "TABLE" {
            let previous = index.checked_sub(1).map(|i| significant[i].upper());
            if matches!(previous.as_deref(), Some("ALTER" | "DROP" | "TRUNCATE")) {
                if let Some((table_ref, next)) = parse_table_ref(significant, index + 1, true) {
                    consumed.insert(significant[index + 1].start);
                    out.push(table_ref);
                    index = next;
                    continue;
                }
            }
        }
        index += 1;
    }
}

fn parse_table_ref(
    significant: &[&Token],
    mut index: usize,
    allow_columns: bool,
) -> Option<(TableRef, usize)> {
    while significant.get(index).is_some_and(|token| {
        token.kind == TokenKind::Word && REF_PREFIX.contains(&token.upper().as_str())
    }) {
        // `ALTER TABLE IF EXISTS t` 里的 TABLE 已经由调用方跳过。
        if significant[index].is_word("TABLE") {
            index += 1;
            continue;
        }
        index += 1;
    }
    let first = *significant.get(index)?;
    if first.text == "(" {
        return None;
    }
    let name = ident_text(first)?;
    if name.starts_with('#') || name.starts_with('@') || name.starts_with('$') {
        return Some((
            TableRef {
                schema: None,
                name,
                range: first.start..first.end,
                alias: None,
                is_function: false,
                dynamic: true,
            },
            index + 1,
        ));
    }

    let mut parts = vec![(name, first.start..first.end)];
    let mut cursor = index + 1;
    while significant
        .get(cursor)
        .is_some_and(|token| token.text == ".")
        && significant
            .get(cursor + 1)
            .is_some_and(|token| ident_text(token).is_some())
    {
        let token = significant[cursor + 1];
        parts.push((
            ident_text(token).unwrap_or_default(),
            token.start..token.end,
        ));
        cursor += 2;
    }
    let (name, range) = parts.pop().expect("至少有一个表名");
    let schema = parts.pop().map(|(schema, _)| schema);
    let dynamic = name.contains('{') || name.contains('$');

    // 表函数 / 子查询括号；`INSERT INTO t (a, b)` 的括号是列清单，不是调用。
    let is_function = !allow_columns
        && significant
            .get(cursor)
            .is_some_and(|token| token.text == "(");
    if is_function {
        cursor = skip_parens(significant, cursor);
    }

    let mut alias = None;
    if let Some(token) = significant.get(cursor) {
        if token.is_word("AS") {
            if let Some(alias_token) = significant
                .get(cursor + 1)
                .and_then(|token| ident_text(token))
            {
                alias = Some(alias_token);
                cursor += 2;
            }
        } else if token.kind == TokenKind::Word
            && !CLAUSE_END.contains(&token.upper().as_str())
            && !REF_PREFIX.contains(&token.upper().as_str())
        {
            alias = ident_text(token);
            cursor += 1;
        }
    }

    Some((
        TableRef {
            schema,
            name,
            range,
            alias,
            is_function,
            dynamic,
        },
        cursor,
    ))
}

#[allow(clippy::too_many_arguments)]
fn check_qualified_columns(
    significant: &[&Token],
    consumed: &HashSet<usize>,
    statement_start: usize,
    aliases: &HashMap<String, String>,
    alias_count: usize,
    ctes: &HashSet<String>,
    lookup: &Lookup<'_>,
    report: &mut Report,
) {
    let mut index = 0usize;
    while index + 2 < significant.len() {
        let qualifier_token = significant[index];
        let Some(qualifier) = ident_text(qualifier_token) else {
            index += 1;
            continue;
        };
        if significant[index + 1].text != "." {
            index += 1;
            continue;
        }
        let Some(column) = ident_text(significant[index + 2]) else {
            index += 1;
            continue;
        };
        if significant
            .get(index + 3)
            .is_some_and(|token| token.text == ".")
        {
            // 三段限定（db.schema.table / schema.table.column）不在这里猜。
            index += 3;
            continue;
        }
        let is_call = significant
            .get(index + 3)
            .is_some_and(|token| token.text == "(");
        if is_call || consumed.contains(&qualifier_token.start) {
            index += 3;
            continue;
        }

        let qualifier_lower = qualifier.to_lowercase();
        if let Some(table) = aliases.get(&qualifier_lower) {
            if column != "*" {
                if let Some(columns) = lookup.columns.get(table) {
                    if !columns
                        .iter()
                        .any(|hint| hint.name.eq_ignore_ascii_case(&column))
                    {
                        report.issue(
                            statement_start + significant[index + 2].start
                                ..statement_start + significant[index + 2].end,
                            Severity::Warning,
                            "unknown_column",
                            format!("列 “{column}” 不在表 {table} 的元数据中"),
                        );
                    }
                }
            }
            index += 3;
            continue;
        }

        let known = lookup.has_table(&qualifier)
            || lookup.schemas.contains(&qualifier_lower)
            || ctes.contains(&qualifier_lower);
        if !known && alias_count > 0 {
            report.issue(
                statement_start + qualifier_token.start..statement_start + qualifier_token.end,
                Severity::Warning,
                "unknown_alias",
                format!("别名 “{qualifier}” 未在 FROM / JOIN 中定义"),
            );
        }
        index += 3;
    }
}

/// 收集 `WITH name AS (...)`（含多个 CTE）的名字。
fn collect_ctes(significant: &[&Token]) -> HashSet<String> {
    let mut ctes = HashSet::new();
    let mut index = 0usize;
    while index < significant.len() {
        if !significant[index].is_word("WITH") {
            index += 1;
            continue;
        }
        index += 1;
        if significant
            .get(index)
            .is_some_and(|token| token.is_word("RECURSIVE"))
        {
            index += 1;
        }
        loop {
            let Some(token) = significant.get(index) else {
                break;
            };
            let Some(name) = ident_text(token) else {
                break;
            };
            ctes.insert(name.to_lowercase());
            index += 1;
            if significant
                .get(index)
                .is_some_and(|token| token.text == "(")
            {
                index = skip_parens(significant, index);
            }
            if !significant
                .get(index)
                .is_some_and(|token| token.is_word("AS"))
            {
                break;
            }
            index += 1;
            if significant
                .get(index)
                .is_some_and(|token| token.is_word("NOT"))
            {
                index += 1;
            }
            if significant
                .get(index)
                .is_some_and(|token| token.is_word("MATERIALIZED"))
            {
                index += 1;
            }
            if !significant
                .get(index)
                .is_some_and(|token| token.text == "(")
            {
                break;
            }
            index = skip_parens(significant, index);
            if significant
                .get(index)
                .is_some_and(|token| token.text == ",")
            {
                index += 1;
                continue;
            }
            break;
        }
    }
    ctes
}

/// `index` 指向 `(`，返回匹配 `)` 之后的位置。
fn skip_parens(significant: &[&Token], index: usize) -> usize {
    let mut depth = 0usize;
    let mut cursor = index;
    while let Some(token) = significant.get(cursor) {
        match token.text {
            "(" => depth += 1,
            ")" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return cursor + 1;
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    cursor
}

fn ident_text(token: &Token<'_>) -> Option<String> {
    match token.kind {
        TokenKind::Word => Some(token.text.to_string()),
        TokenKind::QuotedIdent => {
            let quote = token.text.chars().next()?;
            let body = token.text.strip_prefix(quote)?.strip_suffix(quote)?;
            let doubled = format!("{quote}{quote}");
            Some(body.replace(&doubled, &quote.to_string()))
        }
        _ => None,
    }
}

fn offset(range: &Range<usize>, base: usize) -> Range<usize> {
    base + range.start..base + range.end
}

/// 编辑器悬停：表 / 视图的结构摘要与已缓存列的类型。
pub struct SqlHoverProvider {
    catalog: Rc<std::cell::RefCell<CompletionCatalog>>,
}

impl SqlHoverProvider {
    pub fn new(catalog: Rc<std::cell::RefCell<CompletionCatalog>>) -> Self {
        Self { catalog }
    }
}

impl HoverProvider for SqlHoverProvider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<Option<Hover>>> {
        let Some(range) = text.word_range(offset) else {
            return Task::ready(Ok(None));
        };
        let word = text.slice(range.clone()).to_string();
        if word.is_empty() {
            return Task::ready(Ok(None));
        }
        let catalog = self.catalog.borrow();
        let lower = word.to_lowercase();
        let markdown = if let Some(table) = catalog
            .tables
            .iter()
            .find(|table| table.name.eq_ignore_ascii_case(&word))
        {
            let mut lines = vec![format!("**{}** `{}`", table.kind.label(), table.name)];
            let mut meta = Vec::new();
            if let Some(schema) = &table.schema {
                meta.push(schema.clone());
            }
            if let Some(rows) = table.row_estimate {
                meta.push(format!("约 {rows} 行"));
            }
            if !meta.is_empty() {
                lines.push(meta.join(" · "));
            }
            if let Some(comment) = &table.comment {
                lines.push(String::new());
                lines.push(comment.clone());
            }
            if let Some(columns) = catalog.columns.get(&lower) {
                let names: Vec<&str> = columns
                    .iter()
                    .take(12)
                    .map(|column| column.name.as_str())
                    .collect();
                lines.push(String::new());
                lines.push(format!(
                    "列（{}）：{}{}",
                    columns.len(),
                    names.join("、"),
                    if columns.len() > names.len() {
                        " …"
                    } else {
                        ""
                    }
                ));
            }
            Some(lines.join("\n\n"))
        } else {
            catalog.columns.iter().find_map(|(table, columns)| {
                let column = columns
                    .iter()
                    .find(|column| column.name.eq_ignore_ascii_case(&word))?;
                let mut lines = vec![format!("**列** `{table}.{}`", column.name)];
                let mut meta = vec![format!("`{}`", column.type_label)];
                if column.primary_key {
                    meta.push("主键".into());
                }
                meta.push(if column.nullable { "NULL" } else { "NOT NULL" }.into());
                lines.push(meta.join(" · "));
                if let Some(comment) = &column.comment {
                    lines.push(String::new());
                    lines.push(comment.clone());
                }
                Some(lines.join("\n\n"))
            })
        };
        let hover = markdown.map(|value| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(lsp_range(text, range)),
        });
        Task::ready(Ok(hover))
    }
}

fn lsp_range(text: &Rope, range: Range<usize>) -> lsp_types::Range {
    lsp_types::Range {
        start: text.offset_to_position(range.start),
        end: text.offset_to_position(range.end),
    }
}

/// 供状态栏 / 工具栏展示的一句话。
pub fn summary(report: &Report) -> Option<SharedString> {
    if report.issues.is_empty() {
        return report
            .incomplete
            .then(|| SharedString::from("无法完全校验"));
    }
    Some(SharedString::from(format!(
        "诊断 · {} 个问题",
        report.issues.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::TableHint;
    use rustdb_db_api::ObjectKind;

    fn catalog(tables: &[&str], columns: &[(&str, &[&str])]) -> CompletionCatalog {
        let mut catalog = CompletionCatalog::default();
        catalog.tables = tables
            .iter()
            .map(|name| TableHint {
                name: (*name).to_string(),
                kind: ObjectKind::Table,
                schema: Some("public".into()),
                row_estimate: Some(42),
                comment: None,
            })
            .collect();
        catalog.schemas = vec!["public".into()];
        for (table, names) in columns {
            catalog.columns.insert(
                (*table).to_string(),
                names
                    .iter()
                    .map(|name| ColumnHint {
                        name: (*name).to_string(),
                        type_label: "int".into(),
                        nullable: false,
                        primary_key: false,
                        comment: None,
                    })
                    .collect(),
            );
        }
        catalog
    }

    #[test]
    fn known_tables_are_clean_and_unknown_tables_are_marked() {
        let catalog = catalog(&["orders"], &[]);
        let report = validate("SELECT * FROM orders", &catalog);
        assert!(report.issues.is_empty());

        let report = validate("SELECT * FROM ordrs", &catalog);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, "unknown_table");
        assert_eq!(report.issues[0].range, 14..19);
        // 未知表不算「无法校验」：结论是确定的。
        assert!(!report.incomplete);
    }

    #[test]
    fn cte_subquery_function_and_schema_are_tolerated() {
        let catalog = catalog(&["orders"], &[]);
        for sql in [
            "WITH recent AS (SELECT * FROM orders) SELECT * FROM recent",
            "SELECT * FROM (SELECT 1) x",
            "SELECT * FROM generate_series(1, 10)",
            "SELECT * FROM other_schema.orders",
            "SELECT * FROM orders WHERE id IN (SELECT order_id FROM orders)",
        ] {
            let report = validate(sql, &catalog);
            assert!(
                report.issues.is_empty(),
                "{sql} 不应误报：{:?}",
                report.issues
            );
        }
    }

    #[test]
    fn qualified_columns_are_checked_when_structure_is_cached() {
        let catalog = catalog(&["orders"], &[("orders", &["id", "total"])]);
        let report = validate("SELECT o.id, o.totl FROM orders o", &catalog);
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].code, "unknown_column");
        assert!(report.issues[0].message.contains("totl"));
        assert!(!report.incomplete);

        let report = validate("SELECT o.total FROM orders o", &catalog);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn undefined_alias_is_marked_only_when_aliases_exist() {
        let catalog = catalog(&["orders"], &[("orders", &["id"])]);
        let report = validate("SELECT x.id FROM orders o", &catalog);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "unknown_alias"));
        // 没有别名的 `schema.object` 不当成别名。
        let report = validate("SELECT public.orders FROM orders", &catalog);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn missing_metadata_or_columns_marks_incomplete() {
        let report = validate("SELECT * FROM orders", &CompletionCatalog::default());
        assert!(report.incomplete);

        let catalog = catalog(&["orders"], &[]);
        let report = validate("SELECT * FROM orders", &catalog);
        assert!(report.incomplete);
        assert!(report.note.unwrap().contains("列信息"));

        let report = validate("CREATE TABLE t (id int)", &catalog);
        assert!(report.incomplete);
    }

    #[test]
    fn dynamic_sql_marks_incomplete() {
        let catalog = catalog(&["orders"], &[("orders", &["id"])]);
        let report = validate("EXECUTE stmt", &catalog);
        assert!(report.incomplete);
    }

    #[test]
    fn quoted_and_multibyte_identifiers_keep_byte_ranges() {
        let mut catalog = catalog(&["订单"], &[]);
        catalog.tables[0].name = "订单".into();
        let sql = "SELECT * FROM \"订单\"";
        let report = validate(sql, &catalog);
        assert!(report.issues.is_empty());
    }
}
