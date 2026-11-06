//! SQL 参数与脚本变量（F-62）。
//!
//! 约定（需求文档 19.2）：
//! * 解析与重写**只在代码区**进行，字符串 / 注释 / 引用标识符里的占位符一律不动；
//! * 占位符按方言开关：T-SQL 与 MySQL 的 `@name` 是原生语法，默认不当参数；
//! * 原始 SQL 参数不引号包裹，必须显式警示（面板 + 确认卡两处）；
//! * 列表参数展开为 `v1, v2, …`，每个元素按「数字字面量 / 文本」分别渲染。

use crate::flavor::Flavor;
use crate::lex::{self, Token, TokenKind};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderStyle {
    /// `?`：位置参数，按出现次序编号（从 1 开始）。
    Question,
    /// `:name`
    ColonNamed,
    /// `${name}`
    DollarBrace,
    /// `#{name}`
    HashBrace,
    /// `@name`（T-SQL / MySQL 原生语法方言默认关闭）
    AtNamed,
}

impl PlaceholderStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Question => "?",
            Self::ColonNamed => ":name",
            Self::DollarBrace => "${name}",
            Self::HashBrace => "#{name}",
            Self::AtNamed => "@name",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderPolicy {
    pub question: bool,
    pub colon_named: bool,
    pub dollar_brace: bool,
    pub hash_brace: bool,
    pub at_named: bool,
}

impl PlaceholderPolicy {
    pub fn all() -> Self {
        Self {
            question: true,
            colon_named: true,
            dollar_brace: true,
            hash_brace: true,
            at_named: true,
        }
    }

    /// 按方言关掉会与原生语法冲突的占位符。
    pub fn for_flavor(flavor: Flavor) -> Self {
        let mut policy = Self::all();
        if flavor.at_sign_is_native() {
            policy.at_named = false;
        }
        policy
    }

    pub fn allows(self, style: PlaceholderStyle) -> bool {
        match style {
            PlaceholderStyle::Question => self.question,
            PlaceholderStyle::ColonNamed => self.colon_named,
            PlaceholderStyle::DollarBrace => self.dollar_brace,
            PlaceholderStyle::HashBrace => self.hash_brace,
            PlaceholderStyle::AtNamed => self.at_named,
        }
    }

    /// 用户可逐项开关（设置页「占位符」）。
    pub fn with_style(mut self, style: PlaceholderStyle, on: bool) -> Self {
        match style {
            PlaceholderStyle::Question => self.question = on,
            PlaceholderStyle::ColonNamed => self.colon_named = on,
            PlaceholderStyle::DollarBrace => self.dollar_brace = on,
            PlaceholderStyle::HashBrace => self.hash_brace = on,
            PlaceholderStyle::AtNamed => self.at_named = on,
        }
        self
    }
}

impl Default for PlaceholderPolicy {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamRef {
    pub style: PlaceholderStyle,
    /// 位置参数用序号字符串（"1"、"2"…），具名参数用原名。
    pub name: String,
    pub ordinal: Option<usize>,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Text,
    Number,
    Bool,
    Null,
    Raw,
    List,
}

impl ParamKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "字符串",
            Self::Number => "数字",
            Self::Bool => "布尔",
            Self::Null => "NULL",
            Self::Raw => "原始 SQL",
            Self::List => "列表",
        }
    }

    pub const ALL: [ParamKind; 6] = [
        Self::Text,
        Self::Number,
        Self::Bool,
        Self::Null,
        Self::Raw,
        Self::List,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    Text(String),
    /// 以字符串保存的数字字面量；渲染时校验，非法值退回文本并告警。
    Number(String),
    Bool(bool),
    Null,
    /// 原样拼接，不加引号 —— 必须警示。
    Raw(String),
    List(Vec<String>),
}

impl ParamValue {
    pub fn kind(&self) -> ParamKind {
        match self {
            Self::Text(_) => ParamKind::Text,
            Self::Number(_) => ParamKind::Number,
            Self::Bool(_) => ParamKind::Bool,
            Self::Null => ParamKind::Null,
            Self::Raw(_) => ParamKind::Raw,
            Self::List(_) => ParamKind::List,
        }
    }

    /// 参数面板的默认推断：`null` / `true` / `false` / 数字字面量，其余按文本。
    pub fn infer(raw: &str) -> Self {
        let t = raw.trim();
        if t.eq_ignore_ascii_case("null") {
            Self::Null
        } else if t.eq_ignore_ascii_case("true") {
            Self::Bool(true)
        } else if t.eq_ignore_ascii_case("false") {
            Self::Bool(false)
        } else if is_number_literal(t) {
            Self::Number(t.to_string())
        } else {
            Self::Text(raw.to_string())
        }
    }

    /// 面板输入框展示的文本。
    pub fn display(&self) -> String {
        match self {
            Self::Text(s) | Self::Number(s) | Self::Raw(s) => s.clone(),
            Self::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Self::Null => "NULL".to_string(),
            Self::List(items) => items.join(", "),
        }
    }
}

/// 扫描占位符（跳过字符串 / 注释 / 引用标识符）。
pub fn scan(sql: &str, policy: PlaceholderPolicy) -> Vec<ParamRef> {
    let toks = lex::tokens(sql);
    let mut out = Vec::new();
    let mut ordinal = 0usize;
    let mut i = 0usize;
    while i < toks.len() {
        let t = toks[i];
        if t.is_trivia() {
            i += 1;
            continue;
        }
        match (t.kind, t.text) {
            (TokenKind::Punct, "?") if policy.question => {
                ordinal += 1;
                out.push(ParamRef {
                    style: PlaceholderStyle::Question,
                    name: ordinal.to_string(),
                    ordinal: Some(ordinal),
                    start: t.start,
                    end: t.end,
                });
                i += 1;
            }
            (TokenKind::Punct, ":") if policy.colon_named => {
                // `::` 是类型转换，不是参数。
                let prev_colon = t.start > 0 && sql.as_bytes()[t.start - 1] == b':';
                let next_is_colon = toks
                    .get(i + 1)
                    .is_some_and(|n| n.kind == TokenKind::Punct && n.text == ":");
                if prev_colon || next_is_colon {
                    i += 1;
                    continue;
                }
                match named_after(&toks, i) {
                    Some((name, end, next)) => {
                        out.push(ParamRef {
                            style: PlaceholderStyle::ColonNamed,
                            name,
                            ordinal: None,
                            start: t.start,
                            end,
                        });
                        i = next;
                    }
                    None => i += 1,
                }
            }
            (TokenKind::Punct, "$") if policy.dollar_brace => match brace_after(&toks, i) {
                Some((name, end, next)) => {
                    out.push(ParamRef {
                        style: PlaceholderStyle::DollarBrace,
                        name,
                        ordinal: None,
                        start: t.start,
                        end,
                    });
                    i = next;
                }
                None => i += 1,
            },
            (TokenKind::Punct, "#") if policy.hash_brace => match brace_after(&toks, i) {
                Some((name, end, next)) => {
                    out.push(ParamRef {
                        style: PlaceholderStyle::HashBrace,
                        name,
                        ordinal: None,
                        start: t.start,
                        end,
                    });
                    i = next;
                }
                None => i += 1,
            },
            (TokenKind::Punct, "@") if policy.at_named => {
                // `@@version` 这类全局变量不是参数（第二个 `@` 也被吞掉）。
                let prev_at = t.start > 0 && sql.as_bytes()[t.start - 1] == b'@';
                if prev_at || toks.get(i + 1).is_some_and(|n| n.text == "@") {
                    i += 1;
                    continue;
                }
                match named_after(&toks, i) {
                    Some((name, end, next)) => {
                        out.push(ParamRef {
                            style: PlaceholderStyle::AtNamed,
                            name,
                            ordinal: None,
                            start: t.start,
                            end,
                        });
                        i = next;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn named_after(toks: &[Token<'_>], i: usize) -> Option<(String, usize, usize)> {
    let head = toks[i];
    let name = toks.get(i + 1)?;
    if name.start != head.end {
        return None;
    }
    if !matches!(name.kind, TokenKind::Word | TokenKind::Number) {
        return None;
    }
    Some((name.text.to_string(), name.end, i + 2))
}

fn brace_after(toks: &[Token<'_>], i: usize) -> Option<(String, usize, usize)> {
    let head = toks[i];
    let open = toks.get(i + 1)?;
    if open.kind != TokenKind::Punct || open.text != "{" || open.start != head.end {
        return None;
    }
    let name = toks.get(i + 2)?;
    if name.kind != TokenKind::Word || name.start != open.end {
        return None;
    }
    let close = toks.get(i + 3)?;
    if close.kind != TokenKind::Punct || close.text != "}" || close.start != name.end {
        return None;
    }
    Some((name.text.to_string(), close.end, i + 4))
}

/// 去重后的参数名（面板按此顺序列行）。
pub fn names(sql: &str, policy: PlaceholderPolicy) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in scan(sql, policy) {
        if !out.contains(&r.name) {
            out.push(r.name);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedSql {
    pub sql: String,
    pub used: Vec<String>,
    pub missing: Vec<String>,
    /// 渲染告警：非法数字、原始 SQL 拼接等（去重、保序）。
    pub warnings: Vec<String>,
    /// 使用了原始 SQL 拼接的参数名（确认卡必须再警示一次）。
    pub raw_params: Vec<String>,
}

impl RenderedSql {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

pub fn render(
    sql: &str,
    values: &BTreeMap<String, ParamValue>,
    policy: PlaceholderPolicy,
) -> RenderedSql {
    let mut out = RenderedSql::default();
    let refs = scan(sql, policy);
    let mut cursor = 0usize;
    let mut buf = String::with_capacity(sql.len() + 32);
    for r in refs {
        buf.push_str(&sql[cursor..r.start]);
        match values.get(&r.name) {
            Some(value) => {
                if value.kind() == ParamKind::Raw && !out.raw_params.contains(&r.name) {
                    out.raw_params.push(r.name.clone());
                    out.warnings.push(format!(
                        "参数 {} 是原始 SQL，未加引号直接拼接，请确认来源可信",
                        r.name
                    ));
                }
                buf.push_str(&render_value(value, &mut out.warnings));
                if !out.used.contains(&r.name) {
                    out.used.push(r.name.clone());
                }
            }
            None => {
                // 没有值的占位符保持原样：不猜、不静默替换（R5）。
                buf.push_str(&sql[r.start..r.end]);
                if !out.missing.contains(&r.name) {
                    out.missing.push(r.name.clone());
                }
            }
        }
        cursor = r.end;
    }
    buf.push_str(&sql[cursor..]);
    out.warnings.dedup();
    out.sql = buf;
    out
}

fn render_value(value: &ParamValue, warnings: &mut Vec<String>) -> String {
    match value {
        ParamValue::Text(s) => quote(s),
        ParamValue::Number(s) => {
            let t = s.trim();
            if is_number_literal(t) {
                t.to_string()
            } else {
                warnings.push(format!("参数值 “{s}” 不是合法数字，已按文本加引号"));
                quote(s)
            }
        }
        ParamValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        ParamValue::Null => "NULL".to_string(),
        ParamValue::Raw(s) => s.clone(),
        ParamValue::List(items) => items
            .iter()
            .map(|item| {
                let t = item.trim();
                if is_number_literal(t) {
                    t.to_string()
                } else {
                    quote(item)
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// 数字字面量判定：可选符号 + 十进制/科学计数法，必须整串匹配。
pub fn is_number_literal(text: &str) -> bool {
    let s = text.trim();
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0usize;
    if b[i] == b'+' || b[i] == b'-' {
        i += 1;
    }
    let mut digits = 0usize;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut exp_digits = 0usize;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            exp_digits += 1;
        }
        if exp_digits == 0 {
            return false;
        }
    }
    i == b.len()
}

// --- `@set` 脚本变量 ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDirective {
    pub name: String,
    pub value: ParamValue,
    /// 1-based 行号。
    pub line: usize,
}

/// 提取 `@set name = value` 行，返回（抹掉指令后的 SQL、指令列表）。
///
/// 抹除时保留换行，保证行号与原文一致（错误定位不能偏）。
pub fn extract_set_directives(sql: &str) -> (String, Vec<SetDirective>) {
    let mut cleaned = String::with_capacity(sql.len());
    let mut directives = Vec::new();
    for (idx, line) in sql.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        let is_set = trimmed.len() >= 4
            && trimmed[..4].eq_ignore_ascii_case("@set")
            && trimmed[4..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace());
        if !is_set {
            cleaned.push_str(line);
            continue;
        }
        let rest = trimmed[4..].trim();
        let (name, raw_value) = match rest.split_once('=') {
            Some((name, value)) => (name.trim(), value.trim()),
            None => match rest.split_once(char::is_whitespace) {
                Some((name, value)) => (name.trim(), value.trim()),
                None => (rest, ""),
            },
        };
        if !name.is_empty() {
            directives.push(SetDirective {
                name: name.to_string(),
                value: ParamValue::infer(raw_value),
                line: idx + 1,
            });
        }
        // 保留换行，行号不变；非换行字符用空格占位会污染 SQL，直接留空。
        if line.ends_with('\n') {
            cleaned.push('\n');
        }
    }
    (cleaned, directives)
}

/// 把指令合并进参数表（同名以 `@set` 为准，脚本更近）。
pub fn merge_directives(values: &mut BTreeMap<String, ParamValue>, directives: &[SetDirective]) {
    for d in directives {
        values.insert(d.name.clone(), d.value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PlaceholderPolicy {
        PlaceholderPolicy::all()
    }

    #[test]
    fn scans_all_styles_but_skips_literals() {
        let sql = "SELECT ?, :a, ${b}, #{c}, @d, '? :a' AS lit -- @d\nFROM t WHERE x = ':a'";
        let refs = scan(sql, policy());
        let names: Vec<_> = refs.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["1", "a", "b", "c", "d"]);
    }

    #[test]
    fn colon_cast_is_not_a_parameter() {
        let sql = "SELECT '1'::int, a::text, :real";
        let names: Vec<_> = scan(sql, policy()).into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn at_sign_placeholders_disabled_for_native_dialects() {
        let sql = "SET @x = 1; SELECT @y";
        assert_eq!(
            scan(sql, PlaceholderPolicy::for_flavor(Flavor::MsSql)).len(),
            0
        );
        assert_eq!(
            scan(sql, PlaceholderPolicy::for_flavor(Flavor::MySql)).len(),
            0
        );
        assert_eq!(
            scan(sql, PlaceholderPolicy::for_flavor(Flavor::Postgres)).len(),
            2
        );
    }

    #[test]
    fn global_variables_are_not_parameters() {
        let sql = "SELECT @@version";
        assert!(scan(sql, policy()).is_empty());
    }

    #[test]
    fn render_quotes_and_escapes_text() {
        let mut values = BTreeMap::new();
        values.insert("name".to_string(), ParamValue::Text("O'Brien".into()));
        let out = render("SELECT * FROM t WHERE n = :name", &values, policy());
        assert_eq!(out.sql, "SELECT * FROM t WHERE n = 'O''Brien'");
        assert!(out.is_complete());
    }

    #[test]
    fn render_keeps_missing_placeholders_verbatim() {
        let out = render("SELECT :a, :b", &BTreeMap::new(), policy());
        assert_eq!(out.sql, "SELECT :a, :b");
        assert_eq!(out.missing, vec!["a", "b"]);
        assert!(!out.is_complete());
    }

    #[test]
    fn raw_params_are_flagged() {
        let mut values = BTreeMap::new();
        values.insert("order".into(), ParamValue::Raw("id DESC".into()));
        let out = render("SELECT * FROM t ORDER BY ${order}", &values, policy());
        assert_eq!(out.sql, "SELECT * FROM t ORDER BY id DESC");
        assert_eq!(out.raw_params, vec!["order"]);
        assert!(out.warnings.iter().any(|w| w.contains("原始 SQL")));
    }

    #[test]
    fn bad_number_falls_back_to_text_with_warning() {
        let mut values = BTreeMap::new();
        values.insert("n".into(), ParamValue::Number("12abc".into()));
        let out = render("SELECT #{n}", &values, policy());
        assert_eq!(out.sql, "SELECT '12abc'");
        assert_eq!(out.warnings.len(), 1);
    }

    #[test]
    fn list_expands_with_mixed_types() {
        let mut values = BTreeMap::new();
        values.insert(
            "ids".into(),
            ParamValue::List(vec!["1".into(), "2".into(), "abc".into()]),
        );
        let out = render("SELECT * FROM t WHERE id IN (${ids})", &values, policy());
        assert_eq!(out.sql, "SELECT * FROM t WHERE id IN (1, 2, 'abc')");
    }

    #[test]
    fn question_mark_ordinals_are_one_based() {
        let mut values = BTreeMap::new();
        values.insert("1".into(), ParamValue::Text("a".into()));
        values.insert("2".into(), ParamValue::Number("7".into()));
        let out = render("SELECT ?, ?", &values, policy());
        assert_eq!(out.sql, "SELECT 'a', 7");
    }

    #[test]
    fn set_directives_are_extracted_and_lines_preserved() {
        let sql = "@set id = 42\nSELECT * FROM t WHERE id = ${id}\n@set note='hi there'\n";
        let (cleaned, directives) = extract_set_directives(sql);
        assert_eq!(cleaned.lines().count(), sql.lines().count());
        assert_eq!(directives.len(), 2);
        assert_eq!(directives[0].name, "id");
        assert_eq!(directives[0].line, 1);
        assert_eq!(directives[1].line, 3);
        assert_eq!(directives[1].value, ParamValue::Text("'hi there'".into()));
    }

    #[test]
    fn number_literal_detection() {
        for ok in ["1", "-2", "3.5", ".5", "1e9", "1.2E-3", "+7"] {
            assert!(is_number_literal(ok), "{ok} 应判为数字");
        }
        for bad in ["", "1abc", "--1", "1e", "1.2.3", "0x10"] {
            assert!(!is_number_literal(bad), "{bad} 不应判为数字");
        }
    }
}
