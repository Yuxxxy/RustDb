//! AI SQL 解释 / 优化 / 改写 / 跨方言转换（F-92）。
//!
//! 设计要点（对齐需求文档 F-92）：
//! * **只产出文本，不执行**：整条链路没有 session，也没有写入门控——结果只能由用户
//!   手动插回编辑器，执行仍要经过原来的 `run` / `execute_with_source`；
//! * **逐段采纳**：模型给的 SQL 与原文做行级 diff，按 hunk 展示，采纳/拒绝由用户决定；
//! * **不兼容点本地判定**：`check_incompatible` 用词法器做确定性检查（不靠模型自检），
//!   模型漏说的 `LIMIT` / 反引号 / `AUTO_INCREMENT` 等问题也能提示；
//! * **Provider 复用**：与 Agent 共用 `LlmProvider`（F-51），离线测试用 `ScriptedProvider`。

use crate::agent::{AgentContext, AssistantTurn, ChatMessage, LlmProvider};
use rustdb_db_api::DbKind;
use rustdb_sql_lang::{tokens, Token, TokenKind};
use std::sync::Arc;

/// 优化面板的四种任务。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizeKind {
    Explain,
    Optimize,
    Rewrite,
    Convert,
}

impl OptimizeKind {
    pub const ALL: [OptimizeKind; 4] = [
        OptimizeKind::Explain,
        OptimizeKind::Optimize,
        OptimizeKind::Rewrite,
        OptimizeKind::Convert,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Optimize => "optimize",
            Self::Rewrite => "rewrite",
            Self::Convert => "convert",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Explain => "解释",
            Self::Optimize => "优化",
            Self::Rewrite => "改写",
            Self::Convert => "转换",
        }
    }

    /// 「改写」需要用户补充要求；其余任务只用原文。
    pub fn needs_instruction(self) -> bool {
        matches!(self, Self::Rewrite)
    }

    /// 只有「转换」需要目标方言。
    pub fn needs_target(self) -> bool {
        matches!(self, Self::Convert)
    }

    /// 是否必须拿到 SQL（解释模式允许只给文字）。
    pub fn needs_sql(self) -> bool {
        !matches!(self, Self::Explain)
    }
}

/// 一次优化请求；`sql` 是编辑器里选中（或全文）的原文。
#[derive(Debug, Clone, PartialEq)]
pub struct OptimizeRequest {
    pub kind: OptimizeKind,
    pub sql: String,
    /// 源方言：当前连接的类型。
    pub source: DbKind,
    /// 目标方言：仅 `Convert` 使用。
    pub target: Option<DbKind>,
    /// 用户补充要求：仅 `Rewrite` 使用。
    pub instruction: Option<String>,
    /// 已知对象摘要（可空），让模型少猜表名。
    pub schema_digest: String,
}

/// 本地判定的严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    Info,
    Warning,
}

impl NoteLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "提示",
            Self::Warning => "警告",
        }
    }
}

/// 一条方言不兼容提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialectNote {
    pub level: NoteLevel,
    pub message: String,
}

/// 从模型回答里解析出的结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAnswer {
    pub explanation: String,
    pub sql: Option<String>,
    pub notes: Vec<String>,
}

/// 行级 diff 的一段连续改动。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 原文行下标（0 起）。
    pub old_start: usize,
    /// 被替换的原文行数；纯插入为 0。
    pub old_len: usize,
    /// 替换后的行（纯删除为空）。
    pub new_lines: Vec<String>,
    /// 展示用的上文（最多 2 行）。
    pub context_before: Vec<String>,
    /// 展示用的下文（最多 2 行）。
    pub context_after: Vec<String>,
}

impl Hunk {
    pub fn added(&self) -> usize {
        self.new_lines.len()
    }

    pub fn removed(&self) -> usize {
        self.old_len
    }
}

/// 一次优化结果。
#[derive(Debug, Clone, PartialEq)]
pub struct OptimizeResult {
    pub explanation: String,
    pub sql: Option<String>,
    /// 模型列出的改动 / 风险点。
    pub notes: Vec<String>,
    /// 本地判定的方言不兼容点（仅转换且拿到 SQL 时）。
    pub dialect_notes: Vec<DialectNote>,
    /// 原文 → 新 SQL 的行级 diff；解释模式为空。
    pub hunks: Vec<Hunk>,
}

impl OptimizeResult {
    /// 采纳哪些 hunk 之后拼出的 SQL。
    pub fn accepted_sql(&self, original: &str, accepted: &[bool]) -> String {
        apply_hunks(original, &self.hunks, accepted)
    }
}

/// diff 的行数上限：超过就退化为「整段替换」，避免大 SQL 卡住界面。
const MAX_DIFF_LINES: usize = 800;
/// hunk 前后展示的上下文行数。
const CONTEXT_LINES: usize = 2;

#[derive(Debug, Clone, Copy)]
enum Op {
    Equal,
    Del,
    Ins(usize),
}

/// 行级 diff：把原文与模型 SQL 的差异切成若干 hunk。
pub fn diff_hunks(old: &str, new: &str) -> Vec<Hunk> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    if old_lines == new_lines {
        return Vec::new();
    }
    if old_lines.len() > MAX_DIFF_LINES || new_lines.len() > MAX_DIFF_LINES {
        return vec![Hunk {
            old_start: 0,
            old_len: old_lines.len(),
            new_lines: new_lines.iter().map(|line| line.to_string()).collect(),
            context_before: Vec::new(),
            context_after: Vec::new(),
        }];
    }

    let n = old_lines.len();
    let m = new_lines.len();
    let width = m + 1;
    let mut lcs = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if old_lines[i] == new_lines[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }

    let mut ops: Vec<Op> = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            ops.push(Op::Equal);
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            ops.push(Op::Del);
            i += 1;
        } else {
            ops.push(Op::Ins(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del);
        i += 1;
    }
    while j < m {
        ops.push(Op::Ins(j));
        j += 1;
    }

    let mut hunks = Vec::new();
    let mut index = 0usize;
    let mut old_pos = 0usize;
    while index < ops.len() {
        if matches!(ops[index], Op::Equal) {
            old_pos += 1;
            index += 1;
            continue;
        }
        let old_start = old_pos;
        let mut old_len = 0usize;
        let mut added: Vec<String> = Vec::new();
        while index < ops.len() {
            match ops[index] {
                Op::Equal => break,
                Op::Del => {
                    old_len += 1;
                    old_pos += 1;
                }
                Op::Ins(line) => added.push(new_lines[line].to_string()),
            }
            index += 1;
        }
        let before_from = old_start.saturating_sub(CONTEXT_LINES);
        let after_to = (old_start + old_len + CONTEXT_LINES).min(old_lines.len());
        hunks.push(Hunk {
            old_start,
            old_len,
            new_lines: added,
            context_before: old_lines[before_from..old_start]
                .iter()
                .map(|line| line.to_string())
                .collect(),
            context_after: old_lines[old_start + old_len..after_to]
                .iter()
                .map(|line| line.to_string())
                .collect(),
        });
    }
    hunks
}

/// 按 hunk 反向拼回 SQL；`accepted[i] == false` 的 hunk 保持原文。
pub fn apply_hunks(old: &str, hunks: &[Hunk], accepted: &[bool]) -> String {
    let mut lines: Vec<String> = old.lines().map(|line| line.to_string()).collect();
    let mut order: Vec<usize> = (0..hunks.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(hunks[index].old_start));
    for index in order {
        if !accepted.get(index).copied().unwrap_or(false) {
            continue;
        }
        let hunk = &hunks[index];
        let start = hunk.old_start.min(lines.len());
        let end = (start + hunk.old_len).min(lines.len());
        lines.splice(start..end, hunk.new_lines.iter().cloned());
    }
    let mut text = lines.join("\n");
    if old.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    text
}

/// 解析模型回答：第一个 ```sql 代码块 + 块外的文字。
///
/// 块外以 `-` / `*` / `•` / `·` 开头的行视为改动或风险点，其余作为说明。
pub fn parse_answer(text: &str) -> ParsedAnswer {
    let mut prose: Vec<String> = Vec::new();
    let mut block: Option<Vec<String>> = None;
    let mut taken = false;
    let mut sql: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if let Some(lines) = block.take() {
                taken = true;
                sql = Some(trim_lines(lines));
            } else if !taken {
                block = Some(Vec::new());
            } else {
                prose.push(line.to_string());
            }
            continue;
        }
        match block.as_mut() {
            Some(lines) => lines.push(line.to_string()),
            None => prose.push(line.to_string()),
        }
    }
    if let Some(lines) = block {
        sql = Some(trim_lines(lines));
    }
    let sql = sql.filter(|text| !text.trim().is_empty());

    let mut explanation: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for line in prose {
        match bullet(line.trim()) {
            Some(rest) => notes.push(rest.to_string()),
            None => explanation.push(line),
        }
    }
    ParsedAnswer {
        explanation: explanation.join("\n").trim().to_string(),
        sql,
        notes,
    }
}

fn trim_lines(lines: Vec<String>) -> String {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

fn bullet(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("• "))
        .or_else(|| line.strip_prefix("· "))?;
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

/// 目标方言的写法要点（进系统提示）。
pub fn dialect_rules(kind: DbKind) -> &'static str {
    match kind {
        DbKind::Mysql | DbKind::Mariadb => {
            "标识符用反引号；分页 LIMIT n OFFSET m；自增 AUTO_INCREMENT；日期 NOW()；\
             字符串拼接 CONCAT()；没有 RETURNING / FULL OUTER JOIN；布尔值用 1/0。"
        }
        DbKind::Postgres => {
            "标识符用双引号（小写可省略）；分页 LIMIT/OFFSET 或 FETCH FIRST；\
             自增 GENERATED … AS IDENTITY 或 serial；日期 now()；拼接 ||；\
             支持 RETURNING、ILIKE 与 :: 类型转换。"
        }
        DbKind::Sqlite => {
            "标识符可用双引号；分页 LIMIT/OFFSET；自增 INTEGER PRIMARY KEY AUTOINCREMENT；\
             日期 datetime('now')；拼接 ||；类型系统宽松，转换建议 CAST(...)。"
        }
        DbKind::Mssql => {
            "标识符用 [方括号]；分页 TOP n 或 OFFSET … FETCH NEXT；自增 IDENTITY(1,1)；\
             日期 GETDATE()/SYSDATETIME()；拼接 +；空值合并 ISNULL()；没有 LIMIT / RETURNING。"
        }
        DbKind::Oracle => {
            "标识符用双引号且大小写敏感；分页 FETCH FIRST n ROWS ONLY 或 ROWNUM；\
             自增 GENERATED … AS IDENTITY；日期 SYSDATE/CURRENT_TIMESTAMP；拼接 ||；\
             23c 之前没有布尔字面量。"
        }
    }
}

/// 系统提示：限定任务与输出格式，并给出两端方言要点。
pub fn system_prompt(kind: OptimizeKind, source: DbKind, target: Option<DbKind>) -> String {
    let mut text = format!(
        "你是 RustDb 的 SQL 助手，只做「{}」，不执行任何 SQL，也不编造不存在的表或列。\n",
        kind.label()
    );
    text.push_str(
        "输出格式：\n\
         1. 先用不超过 80 字说明结论或改动理由；\n\
         2. 需要给出 SQL 时，放在唯一的 ```sql 代码块里，只给一条语句；\n\
         3. 代码块后用「- 」列出关键改动或风险点，不要重复 SQL 内容。\n",
    );
    match kind {
        OptimizeKind::Explain => {
            text.push_str("本次只解释语义、执行代价与潜在风险，不输出 SQL 代码块。\n");
        }
        OptimizeKind::Optimize => {
            text.push_str("在保持结果语义不变的前提下优化；给出优化后的完整 SQL。\n");
        }
        OptimizeKind::Rewrite => {
            text.push_str("按用户要求改写为一条可执行语句；要求不明确时按最保守的理解处理。\n");
        }
        OptimizeKind::Convert => {
            text.push_str(
                "把 SQL 改写成目标方言，尽量保持语义等价；无法等价表达时在说明里明确指出。\n",
            );
        }
    }
    text.push_str(&format!(
        "源方言：{}（{}）。\n方言要点：{}\n",
        source.display_name(),
        source.key(),
        dialect_rules(source)
    ));
    if let Some(target) = target {
        text.push_str(&format!(
            "目标方言：{}（{}）。\n目标方言要点：{}\n",
            target.display_name(),
            target.key(),
            dialect_rules(target)
        ));
    }
    text
}

/// 用户提示：方言、要求、已知对象与原文。
pub fn user_prompt(req: &OptimizeRequest) -> String {
    let mut text = format!("【源方言】{}\n", req.source.display_name());
    if let Some(target) = req.target {
        text.push_str(&format!("【目标方言】{}\n", target.display_name()));
    }
    if let Some(instruction) = req
        .instruction
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        text.push_str(&format!("【用户要求】{instruction}\n"));
    }
    let digest = req.schema_digest.trim();
    if !digest.is_empty() {
        let digest: String = digest.chars().take(1500).collect();
        text.push_str("【已知对象（可能不完整）】\n");
        text.push_str(&digest);
        text.push('\n');
    }
    text.push_str("【SQL】\n```sql\n");
    text.push_str(req.sql.trim());
    text.push_str("\n```");
    text
}

/// 确定性方言检查：只看代码，不看字符串与注释。
pub fn check_incompatible(sql: &str, target: DbKind) -> Vec<DialectNote> {
    let tokens: Vec<Token<'_>> = tokens(sql)
        .into_iter()
        .filter(|token| !token.is_trivia())
        .collect();
    let mut notes = Vec::new();
    let mut warn = |hit: bool, message: &str| {
        if hit {
            notes.push(DialectNote {
                level: NoteLevel::Warning,
                message: message.to_string(),
            });
        }
    };
    match target {
        DbKind::Mysql | DbKind::Mariadb => {
            warn(
                has_word(&tokens, "TOP"),
                "MySQL/MariaDB 没有 TOP n，请改用 LIMIT n。",
            );
            warn(
                has_double_colon(&tokens),
                "MySQL/MariaDB 不支持 :: 类型转换，请改用 CAST(x AS type)。",
            );
            warn(
                has_word(&tokens, "RETURNING"),
                "MySQL/MariaDB 不支持 RETURNING，请改用 LAST_INSERT_ID() 或再查一次。",
            );
            warn(
                has_word(&tokens, "IDENTITY"),
                "MySQL 没有 IDENTITY 列，请改用 AUTO_INCREMENT。",
            );
            warn(
                has_call(&tokens, "GETDATE"),
                "MySQL 没有 GETDATE()，请改用 NOW()。",
            );
            warn(
                has_brackets(&tokens),
                "MySQL 不支持 [ident] 标识符，请改用 `ident`。",
            );
        }
        DbKind::Postgres => {
            warn(
                has_backtick(&tokens),
                "PostgreSQL 不支持反引号标识符，请改用双引号或小写标识符。",
            );
            warn(
                has_word(&tokens, "AUTO_INCREMENT"),
                "PostgreSQL 没有 AUTO_INCREMENT，请改用 GENERATED … AS IDENTITY。",
            );
            warn(
                has_word(&tokens, "TOP"),
                "PostgreSQL 没有 TOP n，请改用 LIMIT n 或 FETCH FIRST n ROWS ONLY。",
            );
            warn(
                has_call(&tokens, "GETDATE"),
                "PostgreSQL 没有 GETDATE()，请改用 now()。",
            );
            warn(
                has_call(&tokens, "ISNULL"),
                "PostgreSQL 没有 ISNULL()，请改用 COALESCE()。",
            );
            warn(
                has_brackets(&tokens),
                "PostgreSQL 不支持 [ident]，请改用双引号引用。",
            );
        }
        DbKind::Sqlite => {
            warn(
                has_word(&tokens, "TOP"),
                "SQLite 没有 TOP n，请改用 LIMIT n。",
            );
            warn(
                has_word(&tokens, "AUTO_INCREMENT"),
                "SQLite 用 INTEGER PRIMARY KEY AUTOINCREMENT（没有下划线）。",
            );
            warn(
                has_word(&tokens, "ILIKE"),
                "SQLite 不支持 ILIKE，请改用 LOWER(x) LIKE LOWER(y)。",
            );
            warn(
                has_call(&tokens, "GETDATE"),
                "SQLite 没有 GETDATE()，请改用 datetime('now')。",
            );
            warn(
                has_call(&tokens, "ISNULL"),
                "SQLite 没有 ISNULL()，请改用 IFNULL() 或 COALESCE()。",
            );
        }
        DbKind::Mssql => {
            warn(
                has_word(&tokens, "LIMIT"),
                "T-SQL 没有 LIMIT，请改用 TOP n 或 OFFSET … FETCH NEXT。",
            );
            warn(
                has_backtick(&tokens),
                "SQL Server 不支持反引号，请改用 [ident]。",
            );
            warn(
                has_word(&tokens, "AUTO_INCREMENT"),
                "SQL Server 用 IDENTITY(1,1)，没有 AUTO_INCREMENT。",
            );
            warn(
                has_call(&tokens, "NOW"),
                "SQL Server 没有 NOW()，请改用 GETDATE() 或 SYSDATETIME()。",
            );
            warn(
                has_double_colon(&tokens),
                "T-SQL 不支持 :: 类型转换，请改用 CAST / CONVERT。",
            );
            warn(
                has_word(&tokens, "ILIKE"),
                "T-SQL 不支持 ILIKE，请改用 LOWER(x) LIKE LOWER(y)。",
            );
            warn(
                has_word(&tokens, "RETURNING"),
                "T-SQL 用 OUTPUT 子句代替 RETURNING。",
            );
        }
        DbKind::Oracle => {
            warn(
                has_word(&tokens, "LIMIT"),
                "Oracle 没有 LIMIT，请改用 FETCH FIRST n ROWS ONLY 或 ROWNUM。",
            );
            warn(
                has_word(&tokens, "TOP"),
                "Oracle 没有 TOP n，请改用 FETCH FIRST n ROWS ONLY。",
            );
            warn(has_backtick(&tokens), "Oracle 不支持反引号，请改用双引号。");
            warn(
                has_word(&tokens, "AUTO_INCREMENT"),
                "Oracle 用 GENERATED … AS IDENTITY 或 SEQUENCE。",
            );
            warn(
                has_call(&tokens, "NOW"),
                "Oracle 没有 NOW()，请改用 SYSDATE 或 CURRENT_TIMESTAMP。",
            );
            warn(
                has_call(&tokens, "GETDATE"),
                "Oracle 没有 GETDATE()，请改用 SYSDATE 或 CURRENT_TIMESTAMP。",
            );
            warn(
                has_double_colon(&tokens),
                "Oracle 不支持 :: 类型转换，请改用 CAST(x AS type)。",
            );
            warn(
                has_brackets(&tokens),
                "Oracle 不支持 [ident]，请改用双引号引用。",
            );
            warn(
                has_word(&tokens, "TRUE") || has_word(&tokens, "FALSE"),
                "Oracle 23c 之前没有布尔字面量，请改用 1/0。",
            );
        }
    }
    notes
}

fn has_word(tokens: &[Token<'_>], word: &str) -> bool {
    tokens
        .iter()
        .any(|token| token.kind == TokenKind::Word && token.text.eq_ignore_ascii_case(word))
}

fn has_call(tokens: &[Token<'_>], name: &str) -> bool {
    tokens.windows(2).any(|pair| {
        pair[0].kind == TokenKind::Word
            && pair[0].text.eq_ignore_ascii_case(name)
            && pair[1].kind == TokenKind::Punct
            && pair[1].text == "("
    })
}

fn has_backtick(tokens: &[Token<'_>]) -> bool {
    tokens
        .iter()
        .any(|token| token.kind == TokenKind::QuotedIdent && token.text.starts_with('`'))
}

fn has_double_colon(tokens: &[Token<'_>]) -> bool {
    tokens.windows(2).any(|pair| {
        pair[0].kind == TokenKind::Punct
            && pair[0].text == ":"
            && pair[1].kind == TokenKind::Punct
            && pair[1].text == ":"
    })
}

fn has_brackets(tokens: &[Token<'_>]) -> bool {
    tokens.windows(3).any(|triple| {
        triple[0].kind == TokenKind::Punct
            && triple[0].text == "["
            && matches!(
                triple[1].kind,
                TokenKind::Word | TokenKind::QuotedIdent | TokenKind::Number
            )
            && triple[2].kind == TokenKind::Punct
            && triple[2].text == "]"
    })
}

/// 一次性 SQL 任务：单轮提问、单轮回答，不挂工具、不执行。
pub struct Optimizer {
    provider: Arc<dyn LlmProvider>,
}

impl Optimizer {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self { provider }
    }

    pub fn provider_name(&self) -> String {
        self.provider.name()
    }

    pub fn run(&self, req: &OptimizeRequest) -> Result<OptimizeResult, String> {
        if req.sql.trim().is_empty() {
            return Err("请先选中或输入 SQL".into());
        }
        if req.kind.needs_target() && req.target.is_none() {
            return Err("请选择目标方言".into());
        }
        if req.kind.needs_target() && req.target == Some(req.source) {
            return Err("目标方言与源方言相同".into());
        }
        let ctx = AgentContext {
            system: system_prompt(req.kind, req.source, req.target),
            messages: vec![ChatMessage::user(user_prompt(req))],
            tools: Vec::new(),
            schema_digest: req.schema_digest.clone(),
        };
        let turn = self.provider.next(&ctx)?;
        let text = match turn {
            AssistantTurn::Answer(text) => text,
            AssistantTurn::ToolCall(_) => {
                return Err("模型返回了工具调用，无法用于单轮生成；请换用支持文本回答的模型".into())
            }
        };
        let parsed = parse_answer(&text);
        if req.kind.needs_sql() && parsed.sql.is_none() {
            return Err("模型回答里没有 ```sql 代码块，请重试".into());
        }
        let dialect_notes = match (parsed.sql.as_deref(), req.target) {
            (Some(sql), Some(target)) => check_incompatible(sql, target),
            _ => Vec::new(),
        };
        let hunks = parsed
            .sql
            .as_deref()
            .map(|sql| diff_hunks(&req.sql, sql))
            .unwrap_or_default();
        Ok(OptimizeResult {
            explanation: parsed.explanation,
            sql: parsed.sql,
            notes: parsed.notes,
            dialect_notes,
            hunks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ScriptedProvider;

    fn request(kind: OptimizeKind, sql: &str) -> OptimizeRequest {
        OptimizeRequest {
            kind,
            sql: sql.to_string(),
            source: DbKind::Mysql,
            target: None,
            instruction: None,
            schema_digest: String::new(),
        }
    }

    #[test]
    fn diff_detects_single_line_change() {
        let old = "SELECT a\nFROM t";
        let new = "SELECT a,\n       b\nFROM t";
        let hunks = diff_hunks(old, new);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 0);
        assert_eq!(hunks[0].old_len, 1);
        assert_eq!(hunks[0].new_lines, vec!["SELECT a,", "       b"]);
        assert_eq!(hunks[0].context_after, vec!["FROM t"]);
    }

    #[test]
    fn diff_is_empty_for_identical_sql() {
        assert!(diff_hunks("SELECT 1", "SELECT 1").is_empty());
    }

    #[test]
    fn diff_splits_far_apart_changes() {
        let old = "a\nb\nc\nd\ne\nf\ng";
        let new = "a\nB\nc\nd\ne\nf\nG";
        let hunks = diff_hunks(old, new);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[1].old_start, 6);
        assert_eq!(hunks[1].context_before, vec!["e", "f"]);
    }

    #[test]
    fn apply_respects_accepted_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng";
        let new = "a\nB\nc\nd\ne\nf\nG";
        let hunks = diff_hunks(old, new);
        assert_eq!(apply_hunks(old, &hunks, &[true, true]), new);
        assert_eq!(
            apply_hunks(old, &hunks, &[true, false]),
            "a\nB\nc\nd\ne\nf\ng"
        );
        assert_eq!(apply_hunks(old, &hunks, &[false, false]), old);
    }

    #[test]
    fn apply_keeps_trailing_newline() {
        let old = "SELECT 1;\n";
        let new = "SELECT 2;\n";
        let hunks = diff_hunks(old, new);
        assert_eq!(apply_hunks(old, &hunks, &[true]), "SELECT 2;\n");
    }

    #[test]
    fn huge_input_falls_back_to_full_replacement() {
        let old = "a\n".repeat(MAX_DIFF_LINES + 1);
        let new = "b\n".repeat(MAX_DIFF_LINES + 1);
        let hunks = diff_hunks(&old, &new);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 0);
        assert_eq!(hunks[0].old_len, MAX_DIFF_LINES + 1);
    }

    #[test]
    fn parse_answer_extracts_sql_and_notes() {
        let text = "把子查询改成 JOIN。\n\n```sql\nSELECT a\nFROM t\n```\n\n- 去掉了一次全表扫描\n- 需要确认索引";
        let parsed = parse_answer(text);
        assert_eq!(parsed.explanation, "把子查询改成 JOIN。");
        assert_eq!(parsed.sql.as_deref(), Some("SELECT a\nFROM t"));
        assert_eq!(parsed.notes, vec!["去掉了一次全表扫描", "需要确认索引"]);
    }

    #[test]
    fn parse_answer_without_fence_is_all_explanation() {
        let parsed = parse_answer("这条 SQL 会走全表扫描。");
        assert_eq!(parsed.sql, None);
        assert_eq!(parsed.explanation, "这条 SQL 会走全表扫描。");
    }

    #[test]
    fn incompatible_flags_limit_for_mssql() {
        let notes = check_incompatible("SELECT * FROM t LIMIT 10", DbKind::Mssql);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].message.contains("LIMIT"));
    }

    #[test]
    fn incompatible_ignores_string_literals_and_comments() {
        let notes = check_incompatible(
            "SELECT 'LIMIT 10' AS note -- LIMIT 10\nFROM t /* LIMIT */",
            DbKind::Mssql,
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn incompatible_flags_backticks_for_postgres() {
        let notes = check_incompatible("SELECT `id` FROM `users`", DbKind::Postgres);
        assert!(notes[0].message.contains("反引号"));
    }

    #[test]
    fn incompatible_clean_sql_has_no_notes() {
        let notes = check_incompatible(
            "SELECT id, name FROM users ORDER BY id LIMIT 10",
            DbKind::Postgres,
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn incompatible_flags_boolean_literals_for_oracle() {
        let notes = check_incompatible("SELECT * FROM t WHERE active = TRUE", DbKind::Oracle);
        assert!(notes.iter().any(|note| note.message.contains("布尔")));
    }

    #[test]
    fn prompts_mention_both_dialects() {
        let mut req = request(OptimizeKind::Convert, "SELECT 1 LIMIT 1");
        req.target = Some(DbKind::Postgres);
        let system = system_prompt(req.kind, req.source, req.target);
        assert!(system.contains("MySQL"));
        assert!(system.contains("PostgreSQL"));
        let user = user_prompt(&req);
        assert!(user.contains("【目标方言】PostgreSQL"));
        assert!(user.contains("```sql"));
    }

    #[test]
    fn optimizer_requires_target_for_convert() {
        let provider = ScriptedProvider::new(vec![AssistantTurn::Answer(String::new())]);
        let optimizer = Optimizer::new(Arc::new(provider));
        let error = optimizer
            .run(&request(OptimizeKind::Convert, "SELECT 1"))
            .unwrap_err();
        assert!(error.contains("目标方言"));
    }

    #[test]
    fn optimizer_rejects_missing_sql_block() {
        let provider = ScriptedProvider::new(vec![AssistantTurn::Answer("不知道".into())]);
        let optimizer = Optimizer::new(Arc::new(provider));
        let error = optimizer
            .run(&request(OptimizeKind::Optimize, "SELECT 1"))
            .unwrap_err();
        assert!(error.contains("```sql"));
    }

    #[test]
    fn optimizer_rejects_tool_calls() {
        let provider = ScriptedProvider::new(vec![AssistantTurn::ToolCall(
            crate::agent::ToolCall::new("run_read_query", serde_json::json!({})),
        )]);
        let optimizer = Optimizer::new(Arc::new(provider));
        let error = optimizer
            .run(&request(OptimizeKind::Explain, "SELECT 1"))
            .unwrap_err();
        assert!(error.contains("工具调用"));
    }

    #[test]
    fn optimizer_returns_hunks_and_dialect_notes() {
        let provider = ScriptedProvider::new(vec![AssistantTurn::Answer(
            "替换分页写法。\n```sql\nSELECT id FROM t LIMIT 10\n```\n- 目标库用 LIMIT".into(),
        )]);
        let optimizer = Optimizer::new(Arc::new(provider));
        let mut req = request(OptimizeKind::Convert, "SELECT id FROM t TOP 10");
        req.target = Some(DbKind::Postgres);
        let result = optimizer.run(&req).unwrap();
        assert_eq!(result.explanation, "替换分页写法。");
        assert_eq!(result.sql.as_deref(), Some("SELECT id FROM t LIMIT 10"));
        assert_eq!(result.notes, vec!["目标库用 LIMIT"]);
        assert_eq!(result.hunks.len(), 1);
        assert!(result.dialect_notes.is_empty());
        assert_eq!(
            result.accepted_sql(&req.sql, &[true]),
            "SELECT id FROM t LIMIT 10"
        );
    }

    #[test]
    fn optimizer_allows_explanation_without_sql() {
        let provider = ScriptedProvider::new(vec![AssistantTurn::Answer(
            "这条 SQL 会做全表扫描。\n- 建议加索引".into(),
        )]);
        let optimizer = Optimizer::new(Arc::new(provider));
        let result = optimizer
            .run(&request(OptimizeKind::Explain, "SELECT * FROM t"))
            .unwrap();
        assert_eq!(result.explanation, "这条 SQL 会做全表扫描。");
        assert_eq!(result.sql, None);
        assert_eq!(result.notes, vec!["建议加索引"]);
        assert!(result.hunks.is_empty());
    }
}
