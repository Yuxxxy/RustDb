//! 宽容词法器（F-61 / F-62 / F-64 的共同底座）。
//!
//! 只区分「空白 / 注释 / 字符串 / 引用标识符 / 数字 / 词 / 标点」，
//! **不做关键字表、不做语法分析**（需求文档 R-04：不实现完整 AST）。
//!
//! 六方言的词法差异按「宁可多认字符串」处理：
//! * `'…'` 支持 `''` 与反斜杠转义（MySQL / T-SQL）；
//! * `"…"` / `` `…` `` 视为引用标识符并支持双写转义；
//! * `$tag$ … $tag$`（PostgreSQL dollar quote）整体视为字符串；
//! * `/* … */` 支持嵌套（PostgreSQL 允许）。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    Str,
    QuotedIdent,
    Number,
    Word,
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

impl<'a> Token<'a> {
    /// 空白与注释：替换占位符 / 找关键字时必须跳过。
    pub fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
        )
    }

    pub fn upper(&self) -> String {
        self.text.to_ascii_uppercase()
    }

    /// 该 token 是否是一个完整的关键字（不区分大小写）。
    pub fn is_word(&self, word: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(word)
    }
}

/// 把 SQL 切成 token；拼接所有 token 的 `text` 就是原始 SQL（无损）。
pub fn tokens(sql: &str) -> Vec<Token<'_>> {
    let b = sql.as_bytes();
    let mut out: Vec<Token<'_>> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let start = i;
        let kind;
        match b[i] {
            c if c.is_ascii_whitespace() => {
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                kind = TokenKind::Whitespace;
            }
            b'-' if i + 1 < b.len() && b[i + 1] == b'-' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                kind = TokenKind::LineComment;
            }
            // MySQL 的 `#` 行注释；但 `#{name}` 是占位符，不能吞。
            b'#' if !(i + 1 < b.len() && b[i + 1] == b'{') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                kind = TokenKind::LineComment;
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i = skip_block_comment(b, i);
                kind = TokenKind::BlockComment;
            }
            b'\'' => {
                i = skip_single_quote(b, i);
                kind = TokenKind::Str;
            }
            b'"' => {
                i = skip_doubled(b, i, b'"');
                kind = TokenKind::QuotedIdent;
            }
            b'`' => {
                i = skip_doubled(b, i, b'`');
                kind = TokenKind::QuotedIdent;
            }
            b'$' => match skip_dollar_quote(sql, i) {
                Some(end) => {
                    i = end;
                    kind = TokenKind::Str;
                }
                None => {
                    i += 1;
                    kind = TokenKind::Punct;
                }
            },
            c if c.is_ascii_digit()
                || (c == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) =>
            {
                i = skip_number(b, i);
                kind = TokenKind::Number;
            }
            c if is_word_start(c) => {
                while i < b.len() && is_word_continue(b[i]) {
                    i += 1;
                }
                kind = TokenKind::Word;
            }
            _ => {
                i += 1;
                kind = TokenKind::Punct;
            }
        }
        out.push(Token {
            kind,
            text: &sql[start..i],
            start,
            end: i,
        });
    }
    out
}

fn is_word_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic() || c >= 0x80
}

fn is_word_continue(c: u8) -> bool {
    is_word_start(c) || c.is_ascii_digit() || c == b'$'
}

/// `'…'`：支持 `''` 与反斜杠转义；返回闭合引号之后的位置（未闭合则到末尾）。
pub fn skip_single_quote(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => i += 2,
            b'\'' => {
                if i + 1 < b.len() && b[i + 1] == b'\'' {
                    i += 2;
                } else {
                    return i + 1;
                }
            }
            _ => i += 1,
        }
    }
    b.len()
}

/// `"…"` / `` `…` ``：双写转义。
pub fn skip_doubled(b: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        if b[i] == quote {
            if i + 1 < b.len() && b[i + 1] == quote {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// `/* … */`：支持嵌套。
pub fn skip_block_comment(b: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    let mut depth = 1usize;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// `$tag$ … $tag$`；不是 dollar quote 时返回 `None`。
pub fn skip_dollar_quote(sql: &str, start: usize) -> Option<usize> {
    let b = sql.as_bytes();
    if b.get(start) != Some(&b'$') {
        return None;
    }
    let mut j = start + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    if j >= b.len() || b[j] != b'$' {
        return None;
    }
    let tag = &sql[start..=j];
    let body = j + 1;
    sql[body..].find(tag).map(|rel| body + rel + tag.len())
}

fn skip_number(b: &[u8], start: usize) -> usize {
    let mut i = start;
    if b[i] == b'0' && i + 1 < b.len() && (b[i + 1] == b'x' || b[i + 1] == b'X') {
        i += 2;
        while i < b.len() && b[i].is_ascii_hexdigit() {
            i += 1;
        }
        return i;
    }
    let mut seen_dot = false;
    let mut seen_exp = false;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_digit() => i += 1,
            b'.' if !seen_dot && !seen_exp => {
                seen_dot = true;
                i += 1;
            }
            b'e' | b'E' if !seen_exp => {
                let next = b.get(i + 1).copied();
                let has_sign = matches!(next, Some(b'+') | Some(b'-'));
                let digit_at = if has_sign { i + 2 } else { i + 1 };
                if b.get(digit_at).is_some_and(|c| c.is_ascii_digit()) {
                    seen_exp = true;
                    i = digit_at;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    i.max(start + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<TokenKind> {
        tokens(sql).into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn tokens_are_lossless() {
        let sql = "SELECT 'a''b', \"x\" FROM t -- 注释\nWHERE 1";
        let joined: String = tokens(sql).into_iter().map(|t| t.text).collect();
        assert_eq!(joined, sql);
    }

    #[test]
    fn dollar_quote_is_one_string_token() {
        let sql = "CREATE FUNCTION f() RETURNS void AS $$ BEGIN; END; $$ LANGUAGE plpgsql";
        let toks = tokens(sql);
        let strings: Vec<_> = toks.iter().filter(|t| t.kind == TokenKind::Str).collect();
        assert_eq!(strings.len(), 1);
        assert!(strings[0].text.contains("BEGIN; END;"));
    }

    #[test]
    fn nested_block_comment_is_skipped() {
        let sql = "/* a /* b */ c */ SELECT 1";
        let toks = tokens(sql);
        assert_eq!(toks[0].kind, TokenKind::BlockComment);
        assert_eq!(toks[0].text, "/* a /* b */ c */");
        assert_eq!(toks[1].kind, TokenKind::Whitespace);
        assert_eq!(toks[2].text, "SELECT");
    }

    #[test]
    fn mysql_hash_comment_does_not_eat_placeholder() {
        let sql = "SELECT #{id}, 1 # tail";
        let toks = tokens(sql);
        assert!(toks.iter().any(|t| t.text == "#"));
        assert!(toks.iter().any(|t| t.text == "{"));
        assert!(toks
            .iter()
            .any(|t| t.kind == TokenKind::LineComment && t.text.starts_with('#')));
    }

    #[test]
    fn numbers_and_identifiers() {
        let toks = tokens("SELECT 1.5e-3, x$y, 0xFF, 中文列 FROM t");
        let nums: Vec<_> = toks
            .iter()
            .filter(|t| t.kind == TokenKind::Number)
            .map(|t| t.text)
            .collect();
        assert_eq!(nums, vec!["1.5e-3", "0xFF"]);
        assert!(toks.iter().any(|t| t.text == "x$y"));
        assert!(toks.iter().any(|t| t.text == "中文列"));
    }

    #[test]
    fn backtick_identifiers_are_not_code() {
        let toks = tokens("SELECT `a?b` FROM t");
        assert!(toks.iter().any(|t| t.kind == TokenKind::QuotedIdent));
        assert_eq!(
            kinds("SELECT `a?b`"),
            vec![
                TokenKind::Word,
                TokenKind::Whitespace,
                TokenKind::QuotedIdent
            ]
        );
    }
}
