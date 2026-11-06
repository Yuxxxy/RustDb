//! 数据生成器（F-71）：序列、随机整数、日期、UUID、正则与外键引用。
//!
//! 纯逻辑、无 IO；随机源是固定的 xorshift，同一种子可复现，
//! 测试与「生成结果可解释」都依赖这一点。

use chrono::{Duration as ChronoDuration, NaiveDate};
use rustdb_db_api::Value;

/// 生成规则。`ForeignKey` 由 UI 从当前结果或目标表取候选值后构造。
#[derive(Debug, Clone, PartialEq)]
pub enum GeneratorSpec {
    Sequence { start: i64, step: i64 },
    Integer { min: i64, max: i64 },
    Date { start: NaiveDate, step_days: i64 },
    Uuid,
    Regex { pattern: String },
    ForeignKey { values: Vec<Value> },
}

impl GeneratorSpec {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Sequence { .. } => "序列",
            Self::Integer { .. } => "随机整数",
            Self::Date { .. } => "日期",
            Self::Uuid => "UUID",
            Self::Regex { .. } => "正则文本",
            Self::ForeignKey { .. } => "外键引用",
        }
    }
}

/// 解析一行生成表达式（面板里的紧凑写法）。
///
/// * `seq:起始:步长`（后两项可省，默认 `1:1`）
/// * `int:最小:最大`（默认 `1:10000`）
/// * `date:YYYY-MM-DD:天数`（日期可省，默认今天；步长默认 1）
/// * `uuid`
/// * `regex:模式`
/// * `fk:列名` 由 UI 解析，这里明确报错
pub fn parse_spec(expr: &str) -> Result<GeneratorSpec, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("生成表达式不能为空".into());
    }
    let (kind, rest) = expr.split_once(':').unwrap_or((expr, ""));
    match kind.trim().to_ascii_lowercase().as_str() {
        "seq" | "sequence" => {
            let (start, step) = split_two(rest, 1, 1, "序列")?;
            Ok(GeneratorSpec::Sequence { start, step })
        }
        "int" | "integer" | "number" => {
            let (min, max) = split_two(rest, 1, 10_000, "随机整数")?;
            if min > max {
                return Err("随机整数的最小值不能大于最大值".into());
            }
            Ok(GeneratorSpec::Integer { min, max })
        }
        "date" => {
            let mut parts = rest.split(':');
            let start = match parts.next().map(str::trim).filter(|s| !s.is_empty()) {
                Some(text) => NaiveDate::parse_from_str(text, "%Y-%m-%d")
                    .map_err(|_| format!("日期需要 YYYY-MM-DD，收到「{text}」"))?,
                None => chrono::Local::now().date_naive(),
            };
            let step_days = match parts.next().map(str::trim).filter(|s| !s.is_empty()) {
                Some(text) => text
                    .parse::<i64>()
                    .map_err(|_| format!("日期步长需要整数，收到「{text}」"))?,
                None => 1,
            };
            Ok(GeneratorSpec::Date { start, step_days })
        }
        "uuid" => Ok(GeneratorSpec::Uuid),
        "regex" | "text" => {
            let pattern = rest.trim();
            if pattern.is_empty() {
                return Err("正则表达式不能为空，例如 regex:[a-z]{8}".into());
            }
            Ok(GeneratorSpec::Regex {
                pattern: pattern.to_string(),
            })
        }
        "fk" | "ref" => Err("外键引用请填写列名（fk:列名），由界面解析".into()),
        other => Err(format!(
            "未知生成器「{other}」；可用 seq / int / date / uuid / regex / fk"
        )),
    }
}

fn split_two(
    rest: &str,
    default_a: i64,
    default_b: i64,
    label: &str,
) -> Result<(i64, i64), String> {
    let mut parts = rest.split(':');
    let a = match parts.next().map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => text
            .parse::<i64>()
            .map_err(|_| format!("{label}的起始值需要整数，收到「{text}」"))?,
        None => default_a,
    };
    let b = match parts.next().map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => text
            .parse::<i64>()
            .map_err(|_| format!("{label}的步长需要整数，收到「{text}」"))?,
        None => default_b,
    };
    Ok((a, b))
}

/// 生成 `count` 个值（固定种子，结果可复现）。
pub fn generate(spec: &GeneratorSpec, count: usize) -> Result<Vec<Value>, String> {
    generate_seeded(spec, count, 0x9E37_79B9_7F4A_7C15)
}

pub fn generate_seeded(
    spec: &GeneratorSpec,
    count: usize,
    seed: u64,
) -> Result<Vec<Value>, String> {
    if count > 100_000 {
        return Err("单次最多生成 100000 个值".into());
    }
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let value = match spec {
            GeneratorSpec::Sequence { start, step } => {
                let offset = i128::try_from(index).map_err(|_| "生成数量过大")?;
                let value = i128::from(*start) + i128::from(*step) * offset;
                Value::Int(i64::try_from(value).map_err(|_| "序列超出整数范围")?)
            }
            GeneratorSpec::Integer { min, max } => {
                let span = (*max as i128) - (*min as i128) + 1;
                let pick = if span <= 1 {
                    *min
                } else {
                    let raw = rng.next() as i128;
                    *min + (raw % span) as i64
                };
                Value::Int(pick)
            }
            GeneratorSpec::Date { start, step_days } => {
                let offset = i128::try_from(index).map_err(|_| "生成数量过大")?;
                let days = i128::from(*step_days) * offset;
                let days = i64::try_from(days).map_err(|_| "日期步长超出范围")?;
                let date = start
                    .checked_add_signed(ChronoDuration::days(days))
                    .ok_or_else(|| "日期超出可表示范围".to_string())?;
                Value::Date(date)
            }
            GeneratorSpec::Uuid => Value::Uuid(rng.uuid()),
            GeneratorSpec::Regex { pattern } => {
                let node = parse_regex(pattern)?;
                let mut text = String::new();
                render_node(&node, &mut rng, &mut text, 0)?;
                if text.len() > 4096 {
                    return Err("正则生成结果超过 4096 字节".into());
                }
                Value::Text(text.into())
            }
            GeneratorSpec::ForeignKey { values } => {
                if values.is_empty() {
                    return Err("外键引用没有候选值".into());
                }
                values[index % values.len()].clone()
            }
        };
        out.push(value);
    }
    Ok(out)
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn pick(&mut self, len: usize) -> usize {
        (self.next() % len.max(1) as u64) as usize
    }

    fn uuid(&mut self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        for chunk in bytes.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        bytes
    }
}

#[derive(Debug, Clone)]
enum RegexNode {
    Literal(String),
    Class(Vec<char>),
    Sequence(Vec<RegexNode>),
    Alternative(Vec<RegexNode>),
    Repeat(Box<RegexNode>, usize, usize),
}

fn parse_regex(pattern: &str) -> Result<RegexNode, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut parser = RegexParser { chars, at: 0 };
    let node = parser.alternative(0)?;
    if parser.at < parser.chars.len() {
        return Err(format!("正则在第 {} 个字符处无法解析", parser.at + 1));
    }
    Ok(node)
}

struct RegexParser {
    chars: Vec<char>,
    at: usize,
}

impl RegexParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.at += 1;
        Some(ch)
    }

    fn alternative(&mut self, depth: usize) -> Result<RegexNode, String> {
        if depth > 16 {
            return Err("正则嵌套过深".into());
        }
        let mut branches = vec![self.sequence(depth)?];
        while self.peek() == Some('|') {
            self.bump();
            branches.push(self.sequence(depth)?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().expect("至少一条分支")
        } else {
            RegexNode::Alternative(branches)
        })
    }

    fn sequence(&mut self, depth: usize) -> Result<RegexNode, String> {
        let mut nodes = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == '|' || ch == ')' {
                break;
            }
            let atom = self.atom(depth)?;
            nodes.push(self.quantifier(atom)?);
        }
        Ok(if nodes.len() == 1 {
            nodes.pop().expect("至少一个节点")
        } else {
            RegexNode::Sequence(nodes)
        })
    }

    fn atom(&mut self, depth: usize) -> Result<RegexNode, String> {
        let ch = self.bump().ok_or_else(|| "正则意外结束".to_string())?;
        match ch {
            '^' | '$' => Ok(RegexNode::Literal(String::new())),
            '.' => Ok(RegexNode::Class(alnum_chars())),
            '\\' => {
                let escaped = self.bump().ok_or_else(|| "正则转义不完整".to_string())?;
                Ok(match escaped {
                    'd' => RegexNode::Class(('0'..='9').collect()),
                    'w' => Ok::<_, String>(RegexNode::Class(alnum_chars()))?,
                    's' => RegexNode::Class(vec![' ']),
                    'n' => RegexNode::Literal("\n".into()),
                    't' => RegexNode::Literal("\t".into()),
                    'r' => RegexNode::Literal("\r".into()),
                    'D' | 'W' | 'S' => {
                        return Err("正则暂不支持取反字符类（\\D / \\W / \\S）".into())
                    }
                    'b' | 'B' => RegexNode::Literal(String::new()),
                    other => RegexNode::Literal(other.to_string()),
                })
            }
            '[' => self.class(),
            '(' => {
                // 支持 (?:...)；捕获与非捕获语义对生成没有区别。
                if self.peek() == Some('?') {
                    self.bump();
                    match self.bump() {
                        Some(':') => {}
                        Some(other) => {
                            return Err(format!("正则分组标志 (?{other} 暂不支持，仅支持 (?:...)"))
                        }
                        None => return Err("正则分组不完整".into()),
                    }
                }
                let node = self.alternative(depth + 1)?;
                if self.bump() != Some(')') {
                    return Err("正则缺少右括号".into());
                }
                Ok(node)
            }
            ')' => Err("正则出现多余的右括号".into()),
            '*' | '+' | '?' | '{' => Err(format!("正则量词「{ch}」前面没有可重复的内容")),
            other => Ok(RegexNode::Literal(other.to_string())),
        }
    }

    fn class(&mut self) -> Result<RegexNode, String> {
        let negated = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut chars = Vec::new();
        let mut first = true;
        while let Some(ch) = self.peek() {
            if ch == ']' && !first {
                self.bump();
                if negated {
                    let mut out: Vec<char> = alnum_chars()
                        .into_iter()
                        .chain(" _-".chars())
                        .filter(|c| !chars.contains(c))
                        .collect();
                    out.sort_unstable();
                    out.dedup();
                    return Ok(RegexNode::Class(out));
                }
                if chars.is_empty() {
                    return Err("正则字符类为空".into());
                }
                return Ok(RegexNode::Class(chars));
            }
            first = false;
            self.bump();
            let start = match ch {
                '\\' => match self.bump() {
                    Some('d') => {
                        chars.extend('0'..='9');
                        continue;
                    }
                    Some('w') => {
                        chars.extend(alnum_chars());
                        continue;
                    }
                    Some('s') => {
                        chars.push(' ');
                        continue;
                    }
                    Some(other) => other,
                    None => return Err("正则字符类转义不完整".into()),
                },
                other => other,
            };
            if self.peek() == Some('-') && self.chars.get(self.at + 1).copied() != Some(']') {
                self.bump();
                let end = match self.bump() {
                    Some('\\') => self
                        .bump()
                        .ok_or_else(|| "正则字符类转义不完整".to_string())?,
                    Some(ch) => ch,
                    None => return Err("正则字符类缺少范围终点".into()),
                };
                if end < start {
                    return Err(format!("正则字符类范围无效：{start}-{end}"));
                }
                chars.extend(start..=end);
            } else {
                chars.push(start);
            }
        }
        Err("正则字符类缺少右方括号".into())
    }

    fn quantifier(&mut self, node: RegexNode) -> Result<RegexNode, String> {
        let (min, max) = match self.peek() {
            Some('?') => {
                self.bump();
                (0, 1)
            }
            Some('*') => {
                self.bump();
                (0, 3)
            }
            Some('+') => {
                self.bump();
                (1, 3)
            }
            Some('{') => {
                self.bump();
                let min = self.number()?;
                let max = if self.peek() == Some(',') {
                    self.bump();
                    if self.peek() == Some('}') {
                        (min + 3).min(64)
                    } else {
                        self.number()?
                    }
                } else {
                    min
                };
                if self.bump() != Some('}') {
                    return Err("正则量词缺少右花括号".into());
                }
                if max < min {
                    return Err("正则量词范围无效".into());
                }
                (min.min(64), max.min(64))
            }
            _ => return Ok(node),
        };
        // 惰性量词（`+?` / `{2,3}?`）不影响生成，吞掉尾部 `?`。
        if self.peek() == Some('?') {
            self.bump();
        }
        Ok(RegexNode::Repeat(Box::new(node), min, max))
    }

    fn number(&mut self) -> Result<usize, String> {
        let mut digits = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                digits.push(ch);
                self.bump();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            return Err("正则量词需要数字".into());
        }
        digits
            .parse::<usize>()
            .map_err(|_| "正则量词数字过大".to_string())
    }
}

fn alnum_chars() -> Vec<char> {
    ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain(std::iter::once('_'))
        .collect()
}

fn render_node(
    node: &RegexNode,
    rng: &mut Rng,
    out: &mut String,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 || out.len() > 4096 {
        return Err("正则生成嵌套过深或结果过长".into());
    }
    match node {
        RegexNode::Literal(text) => out.push_str(text),
        RegexNode::Class(chars) => {
            if chars.is_empty() {
                return Err("正则字符类没有可用字符".into());
            }
            out.push(chars[rng.pick(chars.len())]);
        }
        RegexNode::Sequence(nodes) => {
            for node in nodes {
                render_node(node, rng, out, depth + 1)?;
            }
        }
        RegexNode::Alternative(branches) => {
            let branch = rng.pick(branches.len());
            render_node(&branches[branch], rng, out, depth + 1)?;
        }
        RegexNode::Repeat(node, min, max) => {
            let span = max.saturating_sub(*min) + 1;
            let count = min + rng.pick(span);
            for _ in 0..count {
                render_node(node, rng, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_and_integer_are_bounded() {
        let seq = generate(&GeneratorSpec::Sequence { start: 10, step: 5 }, 3).unwrap();
        assert_eq!(seq, vec![Value::Int(10), Value::Int(15), Value::Int(20)]);

        let ints = generate_seeded(&GeneratorSpec::Integer { min: 7, max: 9 }, 50, 42).unwrap();
        assert!(ints.iter().all(|value| matches!(value, Value::Int(7..=9))));
        assert!(ints.iter().any(|value| *value == Value::Int(7)));
        assert!(ints.iter().any(|value| *value == Value::Int(9)));
    }

    #[test]
    fn dates_step_by_days() {
        let start = NaiveDate::from_ymd_opt(2026, 12, 30).unwrap();
        let dates = generate(
            &GeneratorSpec::Date {
                start,
                step_days: 2,
            },
            2,
        )
        .unwrap();
        assert_eq!(
            dates,
            vec![
                Value::Date(NaiveDate::from_ymd_opt(2026, 12, 30).unwrap()),
                Value::Date(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap())
            ]
        );
    }

    #[test]
    fn uuid_is_v4_and_unique() {
        let values = generate(&GeneratorSpec::Uuid, 16).unwrap();
        let mut seen = std::collections::HashSet::new();
        for value in values {
            let Value::Uuid(bytes) = value else {
                panic!("应为 UUID");
            };
            assert_eq!(bytes[6] & 0xF0, 0x40, "版本位应为 4");
            assert_eq!(bytes[8] & 0xC0, 0x80, "variant 位应为 10");
            assert!(seen.insert(bytes), "UUID 不应重复");
        }
    }

    #[test]
    fn regex_generates_matching_text() {
        let values = generate(
            &GeneratorSpec::Regex {
                pattern: "user_[a-z]{3}\\d{2}".into(),
            },
            20,
        )
        .unwrap();
        for value in values {
            let Value::Text(text) = value else {
                panic!("应为文本");
            };
            assert!(text.starts_with("user_"), "{text}");
            assert_eq!(text.len(), "user_".len() + 3 + 2, "{text}");
            let tail = &text["user_".len()..];
            let (letters, digits) = tail.split_at(3);
            assert!(letters.chars().all(|c| c.is_ascii_lowercase()), "{text}");
            assert!(digits.chars().all(|c| c.is_ascii_digit()), "{text}");
        }

        let alternatives = generate(
            &GeneratorSpec::Regex {
                pattern: "(cat|dog)-[0-9]{4}".into(),
            },
            10,
        )
        .unwrap();
        for value in alternatives {
            let Value::Text(text) = value else {
                panic!("应为文本");
            };
            assert!(
                text.starts_with("cat-") || text.starts_with("dog-"),
                "{text}"
            );
            assert_eq!(text.len(), 8, "{text}");
        }
    }

    #[test]
    fn regex_rejects_unsupported_or_broken_patterns() {
        for pattern in ["[a-z", "(abc", "\\D", "*abc", "(?i)abc"] {
            let err = generate(
                &GeneratorSpec::Regex {
                    pattern: pattern.into(),
                },
                1,
            )
            .expect_err("应拒绝");
            assert!(!err.is_empty(), "{pattern}");
        }
    }

    #[test]
    fn foreign_key_cycles_and_rejects_empty() {
        let values = generate(
            &GeneratorSpec::ForeignKey {
                values: vec![Value::Int(1), Value::Int(2)],
            },
            5,
        )
        .unwrap();
        assert_eq!(
            values,
            vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(1),
                Value::Int(2),
                Value::Int(1)
            ]
        );
        assert!(generate(&GeneratorSpec::ForeignKey { values: Vec::new() }, 1).is_err());
    }

    #[test]
    fn parse_spec_covers_compact_forms() {
        assert_eq!(
            parse_spec("seq:5:10").unwrap(),
            GeneratorSpec::Sequence { start: 5, step: 10 }
        );
        assert_eq!(
            parse_spec("int:3:4").unwrap(),
            GeneratorSpec::Integer { min: 3, max: 4 }
        );
        assert!(matches!(
            parse_spec("date:2026-01-02:3").unwrap(),
            GeneratorSpec::Date { step_days: 3, .. }
        ));
        assert_eq!(parse_spec("uuid").unwrap(), GeneratorSpec::Uuid);
        assert_eq!(
            parse_spec("regex:[a-z]{2}").unwrap(),
            GeneratorSpec::Regex {
                pattern: "[a-z]{2}".into()
            }
        );
        assert!(parse_spec("nope").is_err());
        assert!(parse_spec("fk:id").is_err());
    }
}
