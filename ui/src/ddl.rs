//! DDL 预览的轻量着色：关键字 / 字符串 / 数字 / 注释。
//!
//! 只做词法切分，不解析 SQL 语义；着色失败也只是「没颜色」，不影响内容。

use gpui::{div, prelude::*, px, rgb, Div, SharedString};
use rustdb_core::theme::ThemeTokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlTokenKind {
    Keyword,
    String,
    Number,
    Comment,
    Plain,
}

const KEYWORDS: &[&str] = &[
    "ADD",
    "ALTER",
    "AND",
    "AS",
    "BY",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COMMENT",
    "CONSTRAINT",
    "CREATE",
    "DEFAULT",
    "DELETE",
    "DROP",
    "ENGINE",
    "FOREIGN",
    "FROM",
    "INDEX",
    "INSERT",
    "INTO",
    "KEY",
    "MODIFY",
    "NOT",
    "NULL",
    "ON",
    "PRIMARY",
    "REFERENCES",
    "RENAME",
    "REPLACE",
    "SELECT",
    "SET",
    "TABLE",
    "TO",
    "TRUNCATE",
    "UNIQUE",
    "USING",
    "VALUES",
    "VIEW",
    "WHERE",
    "WITH",
];

/// 把一段 SQL 切成带类型的 token。
pub fn tokenize(text: &str) -> Vec<(DdlTokenKind, String)> {
    let mut out: Vec<(DdlTokenKind, String)> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut plain = String::new();
    while i < chars.len() {
        let ch = chars[i];
        // 行注释 `-- ...`
        if ch == '-' && chars.get(i + 1) == Some(&'-') {
            if !plain.is_empty() {
                out.push((DdlTokenKind::Plain, std::mem::take(&mut plain)));
            }
            let mut comment = String::new();
            while i < chars.len() {
                comment.push(chars[i]);
                if chars[i] == '\n' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((DdlTokenKind::Comment, comment));
            continue;
        }
        // 块注释 `/* ... */`
        if ch == '/' && chars.get(i + 1) == Some(&'*') {
            if !plain.is_empty() {
                out.push((DdlTokenKind::Plain, std::mem::take(&mut plain)));
            }
            let mut comment = String::new();
            comment.push(chars[i]);
            comment.push(chars[i + 1]);
            i += 2;
            while i < chars.len() {
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    comment.push('*');
                    comment.push('/');
                    i += 2;
                    break;
                }
                comment.push(chars[i]);
                i += 1;
            }
            out.push((DdlTokenKind::Comment, comment));
            continue;
        }
        // 字符串字面量 `'...'`（`''` 转义）
        if ch == '\'' {
            if !plain.is_empty() {
                out.push((DdlTokenKind::Plain, std::mem::take(&mut plain)));
            }
            let mut literal = String::new();
            literal.push(ch);
            i += 1;
            while i < chars.len() {
                literal.push(chars[i]);
                let done = chars[i] == '\'';
                i += 1;
                if done {
                    if chars.get(i) == Some(&'\'') {
                        literal.push('\'');
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            out.push((DdlTokenKind::String, literal));
            continue;
        }
        // 数字
        if ch.is_ascii_digit() {
            if !plain.is_empty() {
                out.push((DdlTokenKind::Plain, std::mem::take(&mut plain)));
            }
            let mut number = String::new();
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                number.push(chars[i]);
                i += 1;
            }
            out.push((DdlTokenKind::Number, number));
            continue;
        }
        // 标识符 / 关键字
        if ch.is_alphabetic() || ch == '_' {
            if !plain.is_empty() {
                out.push((DdlTokenKind::Plain, std::mem::take(&mut plain)));
            }
            let mut word = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                word.push(chars[i]);
                i += 1;
            }
            let upper = word.to_uppercase();
            let kind = if KEYWORDS.contains(&upper.as_str()) {
                DdlTokenKind::Keyword
            } else {
                DdlTokenKind::Plain
            };
            out.push((kind, word));
            continue;
        }
        plain.push(ch);
        i += 1;
    }
    if !plain.is_empty() {
        out.push((DdlTokenKind::Plain, plain));
    }
    out
}

fn color_of(kind: DdlTokenKind, t: &ThemeTokens) -> Option<u32> {
    match kind {
        DdlTokenKind::Keyword => Some(t.sql_keyword),
        DdlTokenKind::String => Some(t.sql_string),
        DdlTokenKind::Number => Some(t.sql_number),
        DdlTokenKind::Comment => Some(t.sql_comment),
        DdlTokenKind::Plain => None,
    }
}

/// 把 SQL 文本渲染成带颜色的行（保留换行）。
///
/// `div()` 默认是 block，token 直接挂在下面会竖排；这里显式用「列 + 每行 flex 行」，
/// 行内 token 横向排列，超长时按 token 换行。
pub fn render_sql(text: &str, t: &ThemeTokens) -> Div {
    let mut root = div()
        .flex()
        .flex_col()
        .min_w_0()
        .debug_selector(|| "ddl-sql".to_string())
        .text_size(px(crate::design::TS_CAPTION))
        .text_color(rgb(t.fg_primary));
    for line in text.lines() {
        let mut row = div().flex().flex_row().flex_wrap().items_start().min_w_0();
        if line.is_empty() {
            row = row.child(div().child(SharedString::from(" ")));
        }
        for (kind, chunk) in tokenize(line) {
            let span = div().child(SharedString::from(chunk));
            let span = match color_of(kind, t) {
                Some(color) => span.text_color(rgb(color)),
                None => span,
            };
            row = row.child(span);
        }
        root = root.child(row);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_keywords_strings_and_comments() {
        let tokens = tokenize("ALTER TABLE t ADD COLUMN n text DEFAULT 'x'; -- note");
        let kinds: Vec<DdlTokenKind> = tokens.iter().map(|(k, _)| *k).collect();
        assert!(kinds.contains(&DdlTokenKind::Keyword));
        assert!(kinds.contains(&DdlTokenKind::String));
        assert!(kinds.contains(&DdlTokenKind::Comment));
        let text: String = tokens.iter().map(|(_, s)| s.as_str()).collect();
        assert!(text.starts_with("ALTER TABLE t ADD COLUMN n text DEFAULT 'x';"));
        assert!(text.ends_with("-- note"));
    }

    #[test]
    fn handles_escaped_quotes_inside_literals() {
        let tokens = tokenize("SELECT 'a''b' FROM t");
        let text: String = tokens.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(text, "SELECT 'a''b' FROM t");
        assert!(tokens
            .iter()
            .any(|(kind, chunk)| *kind == DdlTokenKind::String && chunk == "'a''b'"));
    }

    #[test]
    fn block_comment_is_one_token() {
        let tokens = tokenize("/* a\nb */ SELECT 1");
        let comment = tokens
            .iter()
            .find(|(kind, _)| *kind == DdlTokenKind::Comment)
            .map(|(_, s)| s.as_str())
            .unwrap_or_default();
        assert_eq!(comment, "/* a\nb */");
    }

    /// `div()` 默认 block，token 直接挂会竖排——回归到「一行内横排」。
    #[gpui::test]
    fn render_sql_keeps_one_line_inline(cx: &mut gpui::TestAppContext) {
        struct Probe;
        impl gpui::Render for Probe {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                div()
                    .w(px(600.))
                    .child(render_sql("WHERE o.total > 0", &ThemeTokens::dark()))
            }
        }
        let (_, vcx) = cx.add_window_view(|_, _| Probe);
        vcx.run_until_parked();
        let bounds = vcx.debug_bounds("ddl-sql").expect("缺少 SQL 渲染根");
        assert!(
            bounds.size.height < px(30.),
            "token 应在一行内横排（竖排时高度会接近 7 行）：{bounds:?}"
        );
    }
}
