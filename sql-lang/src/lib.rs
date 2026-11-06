//! 轻量 SQL 语言服务：语句切分、分类、只读判定、参数、脚本、位置换算。
//!
//! **刻意不做完整 AST**（见需求文档 R-04）：六种方言的完整语法是無底洞，
//! 而产品真正需要的只是「光标在哪条语句」「这是不是写操作」「补全候选」
//! 「占位符在哪」「错误在第几行第几列」。这里用宽容词法实现这些，
//! 补全在 M2 基于元数据做。
//!
//! v1.1（M6）增补：
//! * [`lex`]：共享词法器（dollar quote / 嵌套块注释 / 引用标识符）；
//! * [`params`]：SQL 参数与 `@set` 脚本变量（F-62）；
//! * [`script`]：脚本切分，支持 `GO` / `DELIMITER` / `$tag$`（F-64）；
//! * [`position`]：行列 ↔ 字节偏移（F-61 错误定位）；
//! * [`flavor`]：方言词法差异归类。

pub mod flavor;
pub mod lex;
pub mod params;
pub mod position;
pub mod script;

pub use flavor::Flavor;
pub use lex::{tokens, Token, TokenKind};
pub use params::{
    ParamKind, ParamRef, ParamValue, PlaceholderPolicy, PlaceholderStyle, RenderedSql, SetDirective,
};
pub use position::LineIndex;
pub use script::{split_script, ScriptStatement};

/// 方言感知的语句切分：在通用分号切分之上补齐脚本语法。
///
/// `Flavor::Generic` 保持原有行为；其它方言走 `split_script`，因此
/// PostgreSQL dollar quote、MySQL `#` 注释 / `DELIMITER`、T-SQL `GO`
/// 都不会被误当成语句边界。
pub fn split_statements_for(sql: &str, flavor: Flavor) -> Vec<Statement> {
    if flavor == Flavor::Generic {
        return split_statements(sql);
    }
    split_script(sql, flavor)
        .into_iter()
        .map(|item| Statement {
            text: item.text,
            start: item.start,
            end: item.end,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub text: String,
    /// 相对原始 SQL 的字节偏移（含前导空白）。
    pub start: usize,
    /// 结束位置（不含分号）。
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
    Ddl,
    Tcl,
    Explain,
    Other,
}

impl StatementKind {
    pub fn is_write(self) -> bool {
        matches!(self, Self::Insert | Self::Update | Self::Delete | Self::Ddl)
    }
}

/// 按分号切分语句，忽略字符串 / 标识符 / 注释里的分号。
pub fn split_statements(sql: &str) -> Vec<Statement> {
    let mut out = Vec::new();
    let bytes = sql.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'$' => match lex::skip_dollar_quote(sql, i) {
                // PostgreSQL dollar-quoted body（函数 / DO 块）里的分号不是语句边界。
                Some(end) => i = end,
                None => i += 1,
            },
            b';' => {
                push_statement(&mut out, sql, start, i);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push_statement(&mut out, sql, start, bytes.len());
    out
}

fn push_statement(out: &mut Vec<Statement>, sql: &str, start: usize, end: usize) {
    let slice = &sql[start..end];
    let trimmed = slice.trim();
    // 纯注释块不是语句：交给驱动会把空语句当成 SQLITE_MISUSE（如导出脚本末尾的说明）。
    if trimmed.is_empty() || first_keyword(trimmed).is_empty() {
        return;
    }
    let lead = slice.len() - slice.trim_start().len();
    let actual_start = start + lead;
    out.push(Statement {
        text: trimmed.to_string(),
        start: actual_start,
        end: actual_start + trimmed.len(),
    });
}

/// 光标（字节偏移）所在的语句；光标在语句之间时取前一条，没有则取第一条。
pub fn statement_at(sql: &str, cursor: usize) -> Option<Statement> {
    let statements = split_statements(sql);
    if statements.is_empty() {
        return None;
    }
    if let Some(s) = statements
        .iter()
        .find(|s| cursor >= s.start && cursor <= s.end)
    {
        return Some(s.clone());
    }
    statements
        .into_iter()
        .rev()
        .find(|s| s.end <= cursor)
        .or_else(|| split_statements(sql).into_iter().next())
}

/// 第一条语句（无分号时就是整段）。
pub fn first_statement(sql: &str) -> Option<Statement> {
    split_statements(sql).into_iter().next()
}

pub fn classify(sql: &str) -> StatementKind {
    let toks = significant_tokens(sql);
    let Some(head) = toks.first() else {
        return StatementKind::Other;
    };
    if head.is_word("WITH") {
        return classify_with(&toks);
    }
    match head.upper().as_str() {
        "SELECT" => StatementKind::Select,
        "INSERT" | "REPLACE" => StatementKind::Insert,
        "UPDATE" | "MERGE" => StatementKind::Update,
        "DELETE" => StatementKind::Delete,
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" => StatementKind::Ddl,
        "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => StatementKind::Tcl,
        "EXPLAIN" | "DESCRIBE" | "DESC" => StatementKind::Explain,
        _ => StatementKind::Other,
    }
}

/// CTE 的写入可能发生在括号内部（PostgreSQL 的 data-modifying CTE），
/// 因此这里扫描所有非 trivia token，而不是只看顶层第一个关键字。
fn classify_with(toks: &[Token<'_>]) -> StatementKind {
    let mut found = StatementKind::Select;
    for token in toks.iter().skip(1) {
        let candidate = match token.upper().as_str() {
            "DELETE" => Some(StatementKind::Delete),
            "UPDATE" | "MERGE" => Some(StatementKind::Update),
            "INSERT" | "REPLACE" => Some(StatementKind::Insert),
            "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" => {
                Some(StatementKind::Ddl)
            }
            _ => None,
        };
        if let Some(candidate) = candidate {
            // 多段 CTE 同时出现多种写入时，取风险最高的一种用于展示与审查。
            found = match (found, candidate) {
                (StatementKind::Delete, _) | (_, StatementKind::Delete) => StatementKind::Delete,
                (StatementKind::Ddl, _) | (_, StatementKind::Ddl) => StatementKind::Ddl,
                (StatementKind::Update, _) | (_, StatementKind::Update) => StatementKind::Update,
                (StatementKind::Insert, _) | (_, StatementKind::Insert) => StatementKind::Insert,
                _ => candidate,
            };
        }
    }
    found
}

/// 只读判定（保守白名单）：只有明确不会写入 / 加锁的语句才算只读。
///
/// 这里不依赖 `classify` 的“首关键字”结果：CTE、`SELECT ... INTO`、
/// `SELECT ... FOR UPDATE` 和带 `ANALYZE` 的 `EXPLAIN` 都不能按首关键字放行。
pub fn is_read_only(sql: &str) -> bool {
    let stmts = split_statements(sql);
    if stmts.len() != 1 {
        return false;
    }
    let text = &stmts[0].text;
    let toks = significant_tokens(text);
    let Some(head) = toks.first() else {
        return false;
    };
    match head.upper().as_str() {
        "SELECT" => !toks.iter().any(|token| token.is_word("INTO")) && !has_locking_clause(&toks),
        "SHOW" | "DESCRIBE" | "DESC" => true,
        "EXPLAIN" => !toks.iter().any(|token| token.is_word("ANALYZE")),
        // SQLite 的 PRAGMA 既能读也能写：只放行明确无赋值的白名单。
        "PRAGMA" => pragma_is_read_only(&toks),
        "WITH" => {
            // CTE 括号内也可能出现 INSERT / UPDATE / DELETE（data-modifying CTE）。
            let kind = classify_with(&toks);
            kind == StatementKind::Select
                && !toks.iter().any(|token| token.is_word("INTO"))
                && !has_locking_clause(&toks)
        }
        "BEGIN" | "START" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" => true,
        _ => false,
    }
}

pub fn is_single_statement(sql: &str) -> bool {
    split_statements(sql).len() == 1
}

/// 取第一个非 trivia token（跳过空白、注释、字符串），返回大写形式。
pub fn first_keyword(sql: &str) -> String {
    significant_tokens(sql)
        .first()
        .map(Token::upper)
        .unwrap_or_default()
}

fn significant_tokens(sql: &str) -> Vec<Token<'_>> {
    tokens(sql)
        .into_iter()
        .filter(|token| !token.is_trivia())
        .collect()
}

fn has_locking_clause(toks: &[Token<'_>]) -> bool {
    for (index, token) in toks.iter().enumerate() {
        if token.is_word("FOR")
            && toks
                .get(index + 1)
                .is_some_and(|next| next.is_word("UPDATE") || next.is_word("SHARE"))
        {
            return true;
        }
        // MySQL: LOCK IN SHARE MODE
        if token.is_word("LOCK")
            && toks.get(index + 1).is_some_and(|next| next.is_word("IN"))
            && toks
                .get(index + 2)
                .is_some_and(|next| next.is_word("SHARE"))
        {
            return true;
        }
    }
    false
}

fn pragma_is_read_only(toks: &[Token<'_>]) -> bool {
    // 有赋值 / 写入参数的一律按写入处理。
    if toks.iter().any(|token| token.text == "=") {
        return false;
    }
    // 取 `(` 前的最后一个标识符，兼容 `PRAGMA main.table_info(t)`。
    let end = toks
        .iter()
        .position(|token| token.text == "(")
        .unwrap_or(toks.len());
    let name = toks[..end]
        .iter()
        .rev()
        .find(|token| matches!(token.kind, TokenKind::Word | TokenKind::QuotedIdent))
        .map(Token::upper)
        .unwrap_or_default();
    matches!(
        name.as_str(),
        "TABLE_INFO"
            | "TABLE_XINFO"
            | "INDEX_LIST"
            | "INDEX_INFO"
            | "INDEX_XINFO"
            | "FOREIGN_KEY_LIST"
            | "DATABASE_LIST"
            | "COLLATION_LIST"
            | "COMPILE_OPTIONS"
            | "ENCODING"
            | "PAGE_SIZE"
            | "PAGE_COUNT"
            | "FREELIST_COUNT"
            | "INTEGRITY_CHECK"
            | "QUICK_CHECK"
            | "USER_VERSION"
            | "SCHEMA_VERSION"
            | "JOURNAL_MODE"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ignores_semicolons_in_literals_and_comments() {
        let sql = "SELECT ';' AS a; -- x;y\nSELECT 2; /* ; */ SELECT 3";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 3);
        assert!(stmts[0].text.contains("';'"));
    }

    #[test]
    fn split_skips_comment_only_chunks() {
        let sql = "-- header\nDROP TABLE t;\n-- footer\n";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1, "{stmts:?}");
        assert!(stmts[0].text.starts_with("-- header"));
        assert!(split_statements("-- only\n/* comment */").is_empty());
        assert!(first_statement("-- only").is_none());
    }

    #[test]
    fn statement_at_prefers_the_containing_statement() {
        let sql = "SELECT 1;\nSELECT 2;";
        let second = statement_at(sql, sql.find("2").unwrap()).unwrap();
        assert!(second.text.contains('2'));
    }

    #[test]
    fn classification_is_conservative() {
        assert_eq!(classify(" select 1"), StatementKind::Select);
        assert!(classify("WITH x AS (SELECT 1) DELETE FROM t").is_write());
        assert!(is_read_only("SELECT 1"));
        assert!(!is_read_only("SELECT 1; DELETE FROM t"));
        assert!(!is_read_only("WITH x AS (SELECT 1) DELETE FROM t"));
    }

    #[test]
    fn cte_writes_survive_newlines_and_data_modifying_ctes() {
        for sql in [
            "WITH x AS (SELECT 1)\nUPDATE t SET a = 1",
            "WITH x AS (SELECT 1)\tDELETE FROM t",
            "WITH x AS (SELECT 1) MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "WITH x AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM x",
        ] {
            assert!(classify(sql).is_write(), "{sql}");
            assert!(!is_read_only(sql), "{sql}");
        }
        assert!(is_read_only("WITH x AS (SELECT 1) SELECT * FROM x"));
    }

    #[test]
    fn select_into_call_and_hash_comments_are_not_read_only() {
        assert!(!is_read_only("SELECT * INTO backup FROM t"));
        assert!(!is_read_only("CALL cleanup()"));
        assert!(!is_read_only("# 注释\nDELETE FROM t"));
        assert_eq!(first_keyword("# 注释\nDELETE FROM t"), "DELETE");
        assert!(!is_read_only("SELECT * FROM t FOR UPDATE"));
        assert!(!is_read_only("PRAGMA journal_mode = WAL"));
        assert!(is_read_only("PRAGMA table_info(t)"));
    }

    #[test]
    fn dollar_quoted_bodies_are_one_statement() {
        let sql =
            "CREATE FUNCTION f() RETURNS void AS $$ BEGIN; DELETE FROM t; END; $$ LANGUAGE plpgsql";
        assert_eq!(split_statements(sql).len(), 1);
        assert!(!is_read_only(sql));
    }
}
