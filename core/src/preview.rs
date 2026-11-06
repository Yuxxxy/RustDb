//! DML 执行前预览（F-63）：把 UPDATE / INSERT / DELETE **保守改写**为等价只读 SELECT。
//!
//! 三条硬约束（需求文档 19.2）：
//! 1. 只在「单语句 + 可识别目标表 + 无不可改写结构」时改写，否则显式
//!    [`PreviewOutcome::Unsupported`] 并允许用户执行原语句；
//! 2. 预览链路**零写入**：构建器只产出 `SELECT`，且 [`build`] 内部断言
//!    `is_read_only`；
//! 3. 含序列 / volatile / 非确定性函数时仍给预览，但必须带「预览值可能不同」提示。
//!
//! 改写口径：
//! * `UPDATE t SET a = e1, b = e2 [WHERE …]` → `SELECT *, e1 AS "a(new)", e2 AS "b(new)" FROM t [WHERE …]`；
//! * `DELETE FROM t [WHERE …]` → `SELECT * FROM t [WHERE …]`；
//! * `INSERT INTO t (a, b) VALUES (…), (…)` → `SELECT v1 AS a, v2 AS b UNION ALL SELECT …`。

use rustdb_db_api::DbKind;
use rustdb_sql_lang::{classify, lex, split_statements, StatementKind};

use crate::edit::ident;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmlPreview {
    pub kind: StatementKind,
    /// 目标表（原样片段，已含别名 / schema 限定）。
    pub target: String,
    /// 等价的只读 SQL。
    pub sql: String,
    /// 追加的「新值」列名（已带 `(new)` 后缀），UI 可在表头标注。
    pub new_value_columns: Vec<String>,
    /// 预览提示（非确定性函数、无 WHERE、列名按位置编号…）。
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewOutcome {
    Ready(DmlPreview),
    /// 不可安全改写：UI 显示原因，但**不阻止**执行原语句。
    Unsupported {
        kind: StatementKind,
        reason: String,
    },
    /// 不是 DML（SELECT / DDL / TCL），不需要预览。
    NotDml,
}

impl PreviewOutcome {
    pub fn preview(&self) -> Option<&DmlPreview> {
        match self {
            Self::Ready(p) => Some(p),
            _ => None,
        }
    }

    pub fn unsupported_reason(&self) -> Option<&str> {
        match self {
            Self::Unsupported { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// 构建预览。
pub fn build(kind: DbKind, sql: &str) -> PreviewOutcome {
    let statements = split_statements(sql);
    if statements.len() != 1 {
        return PreviewOutcome::Unsupported {
            kind: StatementKind::Other,
            reason: "多语句不支持预览（请逐条执行）".to_string(),
        };
    }
    let text = statements[0].text.as_str();
    let stmt_kind = classify(text);
    let tokens = Sig::parse(text);
    if tokens.is_empty() {
        return PreviewOutcome::NotDml;
    }
    let outcome = match stmt_kind {
        StatementKind::Update => build_update(kind, text, &tokens),
        StatementKind::Delete => build_delete(kind, text, &tokens),
        StatementKind::Insert => build_insert(kind, text, &tokens),
        _ => return PreviewOutcome::NotDml,
    };
    if let PreviewOutcome::Ready(p) = &outcome {
        // 零写入的机器可验证断言：改写出错就 panic 在开发期，而不是在执行期写库。
        debug_assert!(
            rustdb_sql_lang::is_read_only(&p.sql),
            "DML 预览必须是只读语句：{}",
            p.sql
        );
    }
    outcome
}

// --- 记号与切片 ---

#[derive(Debug, Clone, Copy)]
struct Sig<'a> {
    tok: lex::Token<'a>,
    /// 该 token 所在括号深度（顶层为 0）。
    depth: i32,
}

impl<'a> Sig<'a> {
    fn parse(sql: &'a str) -> Vec<Sig<'a>> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        for tok in lex::tokens(sql) {
            if tok.is_trivia() {
                continue;
            }
            if tok.kind == lex::TokenKind::Punct && tok.text == ")" {
                depth -= 1;
            }
            out.push(Sig {
                tok,
                depth: depth.max(0),
            });
            if tok.kind == lex::TokenKind::Punct && tok.text == "(" {
                depth += 1;
            }
        }
        out
    }

    fn is(&self, word: &str) -> bool {
        self.tok.is_word(word)
    }
}

fn find_kw(sig: &[Sig<'_>], word: &str, from: usize, depth: i32) -> Option<usize> {
    (from..sig.len()).find(|&i| sig[i].depth == depth && sig[i].is(word))
}

fn find_any_kw(sig: &[Sig<'_>], words: &[&str], from: usize, depth: i32) -> Option<usize> {
    (from..sig.len()).find(|&i| sig[i].depth == depth && words.iter().any(|w| sig[i].is(w)))
}

fn text_between(sql: &str, sig: &[Sig<'_>], from: usize, to: usize) -> String {
    if from >= to || from >= sig.len() {
        return String::new();
    }
    let end = to.min(sig.len());
    let start = sig[from].tok.start;
    let stop = sig[end - 1].tok.end;
    sql[start..stop].trim().to_string()
}

/// 从 `from` 起按顶层逗号切组，返回每组的 `(start, end)` 下标区间。
fn split_groups(sig: &[Sig<'_>], from: usize, to: usize, depth: i32) -> Vec<(usize, usize)> {
    let mut groups = Vec::new();
    let mut start = from;
    for i in from..to.min(sig.len()) {
        if sig[i].depth == depth
            && sig[i].tok.kind == lex::TokenKind::Punct
            && sig[i].tok.text == ","
        {
            groups.push((start, i));
            start = i + 1;
        }
    }
    if start < to {
        groups.push((start, to));
    }
    groups
}

fn unquote_ident(raw: &str) -> String {
    let t = raw.trim();
    let b = t.as_bytes();
    if b.len() >= 2 {
        let (open, close) = (b[0], b[b.len() - 1]);
        if (open == b'"' && close == b'"')
            || (open == b'`' && close == b'`')
            || (open == b'[' && close == b']')
        {
            let inner = &t[1..t.len() - 1];
            return match open {
                b'"' => inner.replace("\"\"", "\""),
                b'`' => inner.replace("``", "`"),
                _ => inner.replace("]]", "]"),
            };
        }
    }
    // `schema.table` / `alias.col`：取最后一段。
    match t.rsplit_once('.') {
        Some((_, last)) => unquote_ident(last),
        None => t.to_string(),
    }
}

// --- UPDATE ---

fn build_update(kind: DbKind, sql: &str, sig: &[Sig<'_>]) -> PreviewOutcome {
    let unsupported = |reason: &str| PreviewOutcome::Unsupported {
        kind: StatementKind::Update,
        reason: reason.to_string(),
    };
    if !sig[0].is("UPDATE") {
        return unsupported("无法识别 UPDATE 语句");
    }
    let mut idx = 1usize;
    if sig.get(idx).is_some_and(|s| s.is("ONLY")) {
        idx += 1;
    }
    let Some(set_i) = find_kw(sig, "SET", idx, 0) else {
        return unsupported("未找到 SET 子句");
    };
    if set_i <= idx {
        return unsupported("未找到目标表");
    }
    if (idx..set_i).any(|i| sig[i].is("FROM") || sig[i].is("JOIN")) {
        return unsupported("UPDATE … FROM / JOIN 暂不支持预览");
    }
    let table = text_between(sql, sig, idx, set_i);
    if table.is_empty() {
        return unsupported("未找到目标表");
    }

    let tail_words = [
        "WHERE",
        "FROM",
        "ORDER",
        "LIMIT",
        "RETURNING",
        "GROUP",
        "HAVING",
        "WINDOW",
    ];
    let tail_i = find_any_kw(sig, &tail_words, set_i + 1, 0);
    if let Some(t) = tail_i {
        if sig[t].is("FROM") {
            return unsupported("UPDATE … FROM 暂不支持预览");
        }
    }
    let set_end = tail_i.unwrap_or(sig.len());

    let mut assignments: Vec<(String, String)> = Vec::new();
    for (g_start, g_end) in split_groups(sig, set_i + 1, set_end, 0) {
        let eq = (g_start..g_end)
            .find(|&i| sig[i].tok.kind == lex::TokenKind::Punct && sig[i].tok.text == "=");
        let Some(eq) = eq else {
            return unsupported("SET 子句里有无法识别的赋值");
        };
        let col = unquote_ident(&text_between(sql, sig, g_start, eq));
        let expr = text_between(sql, sig, eq + 1, g_end);
        if col.is_empty() || expr.is_empty() {
            return unsupported("SET 子句里有空赋值");
        }
        assignments.push((col, expr));
    }
    if assignments.is_empty() {
        return unsupported("SET 子句为空");
    }

    let mut notes = Vec::new();
    let mut select = String::from("SELECT *");
    let mut new_cols = Vec::new();
    for (col, expr) in &assignments {
        if let Some(found) = volatile_in(expr) {
            push_unique(
                &mut notes,
                format!("表达式含 {found}，预览值可能与实际写入不同"),
            );
        }
        let alias = format!("{col}(new)");
        select.push_str(&format!(", {expr} AS {}", ident(kind, &alias)));
        new_cols.push(alias);
    }
    select.push_str(" FROM ");
    select.push_str(&table);
    if let Some(t) = tail_i {
        let tail = text_between(sql, sig, t, sig.len());
        if !tail.is_empty() {
            select.push(' ');
            select.push_str(&tail);
        }
        if !sig.iter().any(|s| s.depth == 0 && s.is("WHERE")) {
            push_unique(&mut notes, "没有 WHERE：预览将扫描整表".to_string());
        }
    } else {
        push_unique(&mut notes, "没有 WHERE：预览将扫描整表".to_string());
    }

    PreviewOutcome::Ready(DmlPreview {
        kind: StatementKind::Update,
        target: table,
        sql: select,
        new_value_columns: new_cols,
        notes,
    })
}

// --- DELETE ---

fn build_delete(_kind: DbKind, sql: &str, sig: &[Sig<'_>]) -> PreviewOutcome {
    let unsupported = |reason: &str| PreviewOutcome::Unsupported {
        kind: StatementKind::Delete,
        reason: reason.to_string(),
    };
    if !sig[0].is("DELETE") {
        return unsupported("无法识别 DELETE 语句");
    }
    let from_i = match find_kw(sig, "FROM", 1, 0) {
        Some(i) => i + 1,
        None => return unsupported("DELETE 需要 FROM 子句"),
    };
    let tail_words = [
        "WHERE",
        "ORDER",
        "LIMIT",
        "RETURNING",
        "USING",
        "JOIN",
        "OUTPUT",
    ];
    let tail_i = find_any_kw(sig, &tail_words, from_i, 0);
    if let Some(t) = tail_i {
        if sig[t].is("USING") || sig[t].is("JOIN") {
            return unsupported("DELETE … USING / JOIN 暂不支持预览");
        }
    }
    let table = text_between(sql, sig, from_i, tail_i.unwrap_or(sig.len()));
    if table.is_empty() {
        return unsupported("未找到目标表");
    }
    let mut notes = Vec::new();
    if tail_i.is_none() || !sig.iter().any(|s| s.depth == 0 && s.is("WHERE")) {
        push_unique(&mut notes, "没有 WHERE：预览将扫描整表".to_string());
    }
    let mut select = format!("SELECT * FROM {table}");
    if let Some(t) = tail_i {
        let tail = text_between(sql, sig, t, sig.len());
        if !tail.is_empty() {
            select.push(' ');
            select.push_str(&tail);
        }
    }
    PreviewOutcome::Ready(DmlPreview {
        kind: StatementKind::Delete,
        target: table,
        sql: select,
        new_value_columns: Vec::new(),
        notes,
    })
}

// --- INSERT ---

fn build_insert(kind: DbKind, sql: &str, sig: &[Sig<'_>]) -> PreviewOutcome {
    let unsupported = |reason: &str| PreviewOutcome::Unsupported {
        kind: StatementKind::Insert,
        reason: reason.to_string(),
    };
    if !sig[0].is("INSERT") && !sig[0].is("REPLACE") {
        return unsupported("无法识别 INSERT 语句");
    }
    let mut idx = 1usize;
    if sig.get(idx).is_some_and(|s| s.is("INTO")) {
        idx += 1;
    }
    // 目标表：直到 '(' 或 VALUES / SELECT / DEFAULT / RETURNING / ON
    let table_end = (idx..sig.len())
        .find(|&i| {
            (sig[i].tok.kind == lex::TokenKind::Punct && sig[i].tok.text == "(")
                || sig[i].is("VALUES")
                || sig[i].is("SELECT")
                || sig[i].is("DEFAULT")
                || sig[i].is("RETURNING")
                || sig[i].is("ON")
                || sig[i].is("SET")
        })
        .unwrap_or(sig.len());
    if table_end <= idx {
        return unsupported("未找到目标表");
    }
    let table = text_between(sql, sig, idx, table_end);
    let mut cursor = table_end;
    let mut columns: Vec<String> = Vec::new();

    if sig
        .get(cursor)
        .is_some_and(|s| s.tok.kind == lex::TokenKind::Punct && s.tok.text == "(")
    {
        // 列清单：括号内按顶层（相对深度 1）逗号切
        let close = (cursor..sig.len())
            .find(|&i| sig[i].depth == 0 && sig[i].tok.text == ")")
            .unwrap_or(sig.len());
        for (g_start, g_end) in split_groups(sig, cursor + 1, close, 1) {
            let name = unquote_ident(&text_between(sql, sig, g_start, g_end));
            if name.is_empty() {
                return unsupported("列清单里有空列名");
            }
            columns.push(name);
        }
        cursor = close + 1;
    }

    if sig.get(cursor).is_some_and(|s| s.is("SELECT")) {
        return unsupported("INSERT … SELECT 无法预览（源数据不可枚举）");
    }
    if sig.get(cursor).is_some_and(|s| s.is("DEFAULT")) {
        return unsupported("DEFAULT VALUES 无法预览");
    }
    let Some(values_i) = find_kw(sig, "VALUES", cursor, 0) else {
        return unsupported("未找到 VALUES 子句");
    };

    // 逐行解析括号组
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut i = values_i + 1;
    while i < sig.len() {
        if sig[i].tok.kind == lex::TokenKind::Punct && sig[i].tok.text == "(" {
            let close = find_matching_paren(sig, i);
            if close.is_none() {
                return unsupported("VALUES 括号不匹配");
            }
            let close = close.unwrap();
            let mut values = Vec::new();
            for (g_start, g_end) in split_groups(sig, i + 1, close, 1) {
                let value = text_between(sql, sig, g_start, g_end);
                if value.is_empty() {
                    return unsupported("VALUES 里有空值");
                }
                if value.eq_ignore_ascii_case("DEFAULT") {
                    return unsupported("含 DEFAULT 的 VALUES 无法预览");
                }
                values.push(value);
            }
            rows.push(values);
            i = close + 1;
            continue;
        }
        if sig[i].is("ON") || sig[i].is("RETURNING") {
            break;
        }
        i += 1;
    }
    if rows.is_empty() {
        return unsupported("VALUES 里没有数据行");
    }

    let mut notes = Vec::new();
    if columns.is_empty() {
        push_unique(
            &mut notes,
            "未指定列名：预览列按位置编号（c1…）".to_string(),
        );
    } else if rows.iter().any(|r| r.len() != columns.len()) {
        return unsupported("VALUES 列数与列清单不一致");
    }

    let width = rows[0].len();
    if rows.iter().any(|r| r.len() != width) {
        return unsupported("VALUES 各行表达式个数不一致");
    }
    let names: Vec<String> = (0..width)
        .map(|c| {
            columns
                .get(c)
                .cloned()
                .unwrap_or_else(|| format!("c{}", c + 1))
        })
        .collect();

    let mut parts = Vec::new();
    for row in &rows {
        let mut part = String::from("SELECT ");
        for (c, value) in row.iter().enumerate() {
            if c > 0 {
                part.push_str(", ");
            }
            part.push_str(value);
            part.push_str(" AS ");
            part.push_str(&ident(kind, &names[c]));
            if let Some(found) = volatile_in(value) {
                push_unique(
                    &mut notes,
                    format!("表达式含 {found}，预览值可能与实际写入不同"),
                );
            }
        }
        parts.push(part);
    }
    let select = parts.join(" UNION ALL ");
    if sig.iter().any(|s| s.depth == 0 && s.is("RETURNING")) {
        push_unique(&mut notes, "预览不包含 RETURNING 的输出".to_string());
    }
    if sig.iter().any(|s| s.is("CONFLICT")) || sig.iter().any(|s| s.is("DUPLICATE")) {
        push_unique(
            &mut notes,
            "含冲突处理子句，实际生效行可能少于预览".to_string(),
        );
    }
    if select.is_empty() {
        return unsupported("未能构建 INSERT 预览");
    }
    // 表名只用于展示，SELECT 里不引用：INSERT 预览的目标是「将要写入的值」。
    PreviewOutcome::Ready(DmlPreview {
        kind: StatementKind::Insert,
        target: table,
        sql: select,
        new_value_columns: names,
        notes,
    })
}

fn find_matching_paren(sig: &[Sig<'_>], open: usize) -> Option<usize> {
    let depth = sig[open].depth;
    (open + 1..sig.len()).find(|&i| {
        sig[i].depth == depth && sig[i].tok.kind == lex::TokenKind::Punct && sig[i].tok.text == ")"
    })
}

// --- 非确定性函数检测 ---

const VOLATILE_FUNCS: &[&str] = &[
    "NOW(",
    "CURRENT_TIMESTAMP",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "CLOCK_TIMESTAMP(",
    "STATEMENT_TIMESTAMP(",
    "TRANSACTION_TIMESTAMP(",
    "RANDOM(",
    "RAND(",
    "UUID(",
    "UUID_SHORT(",
    "GEN_RANDOM_UUID",
    "RANDOM_BYTES(",
    "NEWID(",
    "NEXTVAL(",
    "NEXT VALUE FOR",
    "SYSDATE",
    "SYSTIMESTAMP",
    "GETDATE(",
    "GETUTCDATE(",
    "CURRENT_USER",
];

/// 命中则返回第一个命中的函数名（用于提示文案）。
pub fn volatile_in(expr: &str) -> Option<&'static str> {
    let upper = expr.to_ascii_uppercase();
    VOLATILE_FUNCS.iter().find(|f| upper.contains(**f)).copied()
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(kind: DbKind, sql: &str) -> DmlPreview {
        match build(kind, sql) {
            PreviewOutcome::Ready(p) => p,
            other => panic!("期望可预览，实际 {other:?}"),
        }
    }

    fn reason(kind: DbKind, sql: &str) -> String {
        match build(kind, sql) {
            PreviewOutcome::Unsupported { reason, .. } => reason,
            other => panic!("期望不支持，实际 {other:?}"),
        }
    }

    #[test]
    fn update_appends_new_value_columns() {
        let p = ready(
            DbKind::Postgres,
            "UPDATE shop.orders SET amount = amount + 1, note = 'x' WHERE id = 3",
        );
        assert_eq!(p.target, "shop.orders");
        assert_eq!(p.new_value_columns, vec!["amount(new)", "note(new)"]);
        assert!(p.sql.starts_with("SELECT *,"));
        assert!(p.sql.contains("amount + 1 AS \"amount(new)\""));
        assert!(p.sql.ends_with("FROM shop.orders WHERE id = 3"));
        assert!(rustdb_sql_lang::is_read_only(&p.sql));
        assert!(p.notes.is_empty());
    }

    #[test]
    fn update_without_where_is_flagged_not_blocked() {
        let p = ready(DbKind::Sqlite, "UPDATE t SET a = 1");
        assert!(p.notes.iter().any(|n| n.contains("没有 WHERE")));
        assert_eq!(p.sql, "SELECT *, 1 AS \"a(new)\" FROM t");
    }

    #[test]
    fn update_keeps_alias_and_quotes_per_dialect() {
        let p = ready(
            DbKind::Mysql,
            "UPDATE `t` AS `x` SET `a` = 2 WHERE `x`.`id` = 1",
        );
        assert!(p.sql.contains("FROM `t` AS `x`"));
        assert!(p.sql.contains("`a(new)`"));
    }

    #[test]
    fn update_from_is_unsupported() {
        let r = reason(
            DbKind::Postgres,
            "UPDATE t SET a = s.a FROM src s WHERE s.id = t.id",
        );
        assert!(r.contains("FROM"));
    }

    #[test]
    fn delete_previews_the_rows() {
        let p = ready(DbKind::Mssql, "DELETE FROM [dbo].[users] WHERE [id] > 10");
        assert_eq!(p.target, "[dbo].[users]");
        assert_eq!(p.sql, "SELECT * FROM [dbo].[users] WHERE [id] > 10");
        assert!(p.new_value_columns.is_empty());
    }

    #[test]
    fn delete_using_is_unsupported() {
        assert!(reason(DbKind::Postgres, "DELETE FROM t USING src WHERE 1=1").contains("USING"));
    }

    #[test]
    fn insert_expands_rows_to_union_all() {
        let p = ready(
            DbKind::Postgres,
            "INSERT INTO t (a, b) VALUES (1, 'x'), (2, 'y')",
        );
        assert_eq!(p.new_value_columns, vec!["a", "b"]);
        assert_eq!(
            p.sql,
            "SELECT 1 AS \"a\", 'x' AS \"b\" UNION ALL SELECT 2 AS \"a\", 'y' AS \"b\""
        );
        assert!(rustdb_sql_lang::is_read_only(&p.sql));
    }

    #[test]
    fn insert_without_column_list_numbers_columns() {
        let p = ready(DbKind::Sqlite, "INSERT INTO t VALUES (1, 2)");
        assert_eq!(p.new_value_columns, vec!["c1", "c2"]);
        assert!(p.notes.iter().any(|n| n.contains("按位置编号")));
    }

    #[test]
    fn insert_select_and_default_are_unsupported() {
        assert!(reason(DbKind::Postgres, "INSERT INTO t SELECT * FROM u").contains("SELECT"));
        assert!(reason(DbKind::Sqlite, "INSERT INTO t DEFAULT VALUES").contains("DEFAULT"));
        assert!(reason(DbKind::Postgres, "INSERT INTO t (a) VALUES (DEFAULT)").contains("DEFAULT"));
    }

    #[test]
    fn insert_arity_mismatch_is_unsupported() {
        assert!(reason(DbKind::Postgres, "INSERT INTO t (a, b) VALUES (1)").contains("不一致"));
    }

    #[test]
    fn volatile_functions_are_reported() {
        let p = ready(
            DbKind::Postgres,
            "UPDATE t SET ts = NOW(), k = uuid_generate_v4() WHERE id = 1",
        );
        assert!(p.notes.iter().any(|n| n.contains("NOW(")));
        let p2 = ready(DbKind::Mysql, "INSERT INTO t (id) VALUES (UUID())");
        assert!(p2.notes.iter().any(|n| n.contains("UUID(")));
    }

    #[test]
    fn select_and_ddl_need_no_preview() {
        assert_eq!(build(DbKind::Postgres, "SELECT 1"), PreviewOutcome::NotDml);
        assert_eq!(
            build(DbKind::Postgres, "CREATE TABLE t (id int)"),
            PreviewOutcome::NotDml
        );
    }

    #[test]
    fn multi_statement_is_unsupported() {
        assert!(reason(DbKind::Postgres, "UPDATE t SET a=1; UPDATE t SET b=2").contains("多语句"));
    }

    #[test]
    fn subquery_and_comma_in_expressions_are_respected() {
        let p = ready(
            DbKind::Postgres,
            "UPDATE t SET a = coalesce(b, 0), c = (SELECT max(x) FROM u) WHERE id = 1",
        );
        assert_eq!(p.new_value_columns, vec!["a(new)", "c(new)"]);
        assert!(p.sql.contains("(SELECT max(x) FROM u)"));
    }

    #[test]
    fn volatile_detection_ignores_plain_columns() {
        assert!(volatile_in("created_at").is_none());
        assert!(volatile_in("now()").is_some());
    }
}
