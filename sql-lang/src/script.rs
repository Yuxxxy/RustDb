//! 脚本级切分（F-64）：在 `split_statements` 之上补齐方言脚本语法。
//!
//! 与 `split_statements` 的区别：
//! * 输出带 **1-based 行号**（UI 显示「文件 / 第 N 条 / 行号」）；
//! * 支持 T-SQL `GO` 批分隔符、MySQL `DELIMITER` 指令、PostgreSQL `$tag$` 块；
//! * 只有注释的片段不算语句（否则会被当成空 SQL 执行并报错）。
//!
//! `split_statements` 保持原样：执行链路（core）用它，不承担脚本语法。

use crate::flavor::Flavor;
use crate::lex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptStatement {
    /// 1-based 语句序号（脚本内）。
    pub index: usize,
    pub text: String,
    /// 相对整个脚本的字节偏移（已去掉前导空白）。
    pub start: usize,
    pub end: usize,
    /// 1-based 行号。
    pub line: usize,
}

/// 按方言脚本规则切分；返回顺序与文件顺序一致。
pub fn split_script(sql: &str, flavor: Flavor) -> Vec<ScriptStatement> {
    let b = sql.as_bytes();
    let mut out: Vec<ScriptStatement> = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut stmt_start = 0usize;
    let mut stmt_line = 1usize;
    let mut delimiter = String::from(";");

    while i < b.len() {
        // 行首：DELIMITER / GO
        if i == 0 || b[i - 1] == b'\n' {
            let line_end = sql[i..].find('\n').map(|p| i + p).unwrap_or(sql.len());
            let trimmed = sql[i..line_end].trim();

            if flavor.supports_delimiter_directive() {
                if let Some(rest) = strip_directive(trimmed, "DELIMITER") {
                    if !rest.is_empty() {
                        delimiter = rest.to_string();
                    }
                    // 跳过整行（含换行），行号自增。
                    advance(sql, &mut i, &mut line, (line_end + 1).min(sql.len()));
                    stmt_start = i;
                    stmt_line = line;
                    continue;
                }
            }
            if flavor.supports_go_batch() && is_go_line(trimmed) {
                push_statement(&mut out, sql, stmt_start, i, stmt_line);
                advance(sql, &mut i, &mut line, (line_end + 1).min(sql.len()));
                stmt_start = i;
                stmt_line = line;
                continue;
            }
        }

        match b[i] {
            b'\'' => {
                let end = lex::skip_single_quote(b, i);
                advance(sql, &mut i, &mut line, end);
            }
            b'"' => {
                let end = lex::skip_doubled(b, i, b'"');
                advance(sql, &mut i, &mut line, end);
            }
            b'`' => {
                let end = lex::skip_doubled(b, i, b'`');
                advance(sql, &mut i, &mut line, end);
            }
            b'-' if i + 1 < b.len() && b[i + 1] == b'-' => {
                let end = sql[i..].find('\n').map(|p| i + p).unwrap_or(sql.len());
                advance(sql, &mut i, &mut line, end);
            }
            b'#' if flavor.hash_line_comment() && !(i + 1 < b.len() && b[i + 1] == b'{') => {
                let end = sql[i..].find('\n').map(|p| i + p).unwrap_or(sql.len());
                advance(sql, &mut i, &mut line, end);
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                let end = lex::skip_block_comment(b, i);
                advance(sql, &mut i, &mut line, end);
            }
            b'$' => match lex::skip_dollar_quote(sql, i) {
                Some(end) => advance(sql, &mut i, &mut line, end),
                None => i += 1,
            },
            _ => {
                if sql[i..].starts_with(delimiter.as_str()) {
                    push_statement(&mut out, sql, stmt_start, i, stmt_line);
                    let next = i + delimiter.len();
                    advance(sql, &mut i, &mut line, next);
                    stmt_start = i;
                    stmt_line = line;
                } else {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += utf8_len(b[i]);
                }
            }
        }
    }

    push_statement(&mut out, sql, stmt_start, sql.len(), stmt_line);
    out
}

/// `DELIMITER $$` / `delimiter //` → `Some("$$")`。
fn strip_directive<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    if line.len() <= keyword.len() {
        return None;
    }
    if !line[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &line[keyword.len()..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim())
}

/// `GO` / `GO 5` / `GO -- 说明`。
fn is_go_line(trimmed: &str) -> bool {
    if trimmed.len() < 2 || !trimmed[..2].eq_ignore_ascii_case("GO") {
        return false;
    }
    let rest = trimmed[2..].trim();
    rest.is_empty()
        || rest.starts_with("--")
        || rest.chars().all(|c| c.is_ascii_digit())
        || rest
            .split_once(char::is_whitespace)
            .is_some_and(|(count, tail)| {
                count.chars().all(|c| c.is_ascii_digit()) && tail.trim().starts_with("--")
            })
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn advance(sql: &str, i: &mut usize, line: &mut usize, to: usize) {
    let to = to.min(sql.len());
    if to > *i {
        *line += sql[*i..to].matches('\n').count();
        *i = to;
    }
}

fn push_statement(
    out: &mut Vec<ScriptStatement>,
    sql: &str,
    start: usize,
    end: usize,
    stmt_line: usize,
) {
    if start >= end {
        return;
    }
    let slice = &sql[start..end];
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return;
    }
    // 只有注释的片段不是语句。
    if lex::tokens(trimmed).iter().all(|t| t.is_trivia()) {
        return;
    }
    let lead = slice.len() - slice.trim_start().len();
    let actual = start + lead;
    let line = stmt_line + sql[start..actual].matches('\n').count();
    out.push(ScriptStatement {
        index: out.len() + 1,
        text: trimmed.to_string(),
        start: actual,
        end: actual + trimmed.len(),
        line,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str, flavor: Flavor) -> Vec<String> {
        split_script(sql, flavor)
            .into_iter()
            .map(|s| s.text)
            .collect()
    }

    #[test]
    fn plain_script_splits_on_semicolons() {
        let sql = "SELECT 1;\nSELECT 2;\n";
        let stmts = split_script(sql, Flavor::Generic);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].index, 1);
        assert_eq!(stmts[1].line, 2);
        assert_eq!(stmts[1].text, "SELECT 2");
    }

    #[test]
    fn postgres_dollar_quote_stays_one_statement() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1; -- 内部\nEND;\n$$ LANGUAGE plpgsql;\nSELECT f();";
        let stmts = split_script(sql, Flavor::Postgres);
        assert_eq!(stmts.len(), 2, "dollar quote 内部的分号不能切分");
        assert!(stmts[0].text.contains("END;"));
        assert_eq!(stmts[1].text, "SELECT f()");
    }

    #[test]
    fn mysql_delimiter_keeps_procedure_body_whole() {
        let sql = "DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND //\nDELIMITER ;\nSELECT 3;";
        let stmts = split_script(sql, Flavor::MySql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].text.contains("SELECT 2;"));
        assert_eq!(stmts[0].line, 2);
        assert_eq!(stmts[1].text, "SELECT 3");
    }

    #[test]
    fn tsql_go_separates_batches() {
        let sql = "SELECT 1\nGO\nSELECT 2\nGO 5 -- 重复执行\n";
        let stmts = split_script(sql, Flavor::MsSql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].text, "SELECT 1");
        assert_eq!(stmts[1].text, "SELECT 2");
        assert_eq!(stmts[1].line, 3);
    }

    #[test]
    fn comment_only_chunks_are_dropped() {
        let sql = "-- 只是说明\n/* 也不是语句 */\n;\nSELECT 1;";
        let stmts = split_script(sql, Flavor::Generic);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].text, "SELECT 1");
        assert_eq!(stmts[0].line, 4);
    }

    #[test]
    fn crlf_line_numbers_are_correct() {
        let sql = "SELECT 1;\r\n\r\nSELECT 2;";
        let stmts = split_script(sql, Flavor::Generic);
        assert_eq!(stmts[1].line, 3);
    }

    #[test]
    fn semicolons_in_identifiers_are_ignored() {
        let sql = "SELECT `a;b` FROM t; SELECT ';';";
        assert_eq!(texts(sql, Flavor::MySql).len(), 2);
    }
}
