//! AI 执行前安全审查（F-98）：本地静态规则，不依赖模型自检。
//!
//! 输入一条 SQL 与连接上下文，输出：
//! * 整条 SQL 的**有效风险**：多语句取最高，`EXPLAIN ANALYZE` 按底层语句定级；
//! * 命中项：DDL、无 WHERE 改删、多语句、跨库限定名、只读冲突、生产写入；
//! * `blocked`：命中即拒绝（只读 / 生产写入 / AI 多语句 / 无 WHERE 改删）。
//!
//! 审查只做**词法级**判断（复用 `sql-lang` 的宽容词法器），不实现 AST（R-04）。

use crate::agent::RiskLevel;
use rustdb_sql_lang::{
    classify, is_read_only, split_statements, tokens, StatementKind, Token, TokenKind,
};

/// 命中项的严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Blocking,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "提示",
            Self::Warning => "警告",
            Self::Blocking => "阻断",
        }
    }
}

/// 一条审查结论；`code` 是稳定标识，便于测试与后续筛选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
}

impl Finding {
    fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Info,
            message: message.into(),
        }
    }

    fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    fn blocking(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Blocking,
            message: message.into(),
        }
    }
}

/// 审查上下文：来自连接档案（F-89 的生产标记接上后写 `production`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewContext {
    /// 只读连接：任何写语句直接拒绝。
    pub read_only: bool,
    /// 生产连接：写语句只能回填编辑器，永不自动执行。
    pub production: bool,
}

/// 审查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// 整条 SQL 的有效风险（多语句取最高）。
    pub risk: RiskLevel,
    pub findings: Vec<Finding>,
    /// 命中阻断项：调用方必须拒绝执行。
    pub blocked: bool,
}

impl Review {
    /// 给确认卡 / 审计用的一行摘要。
    pub fn summary(&self) -> String {
        self.findings
            .iter()
            .map(|finding| format!("{}：{}", finding.severity.label(), finding.message))
            .collect::<Vec<_>>()
            .join("；")
    }

    pub fn has(&self, code: &str) -> bool {
        self.findings.iter().any(|finding| finding.code == code)
    }
}

/// 审查一条 SQL。
pub fn review(sql: &str, ctx: &ReviewContext) -> Review {
    let statements = split_statements(sql);
    if statements.is_empty() {
        let findings = vec![Finding::blocking("empty", "没有可执行的语句")];
        return finish(RiskLevel::Write, findings, ctx);
    }

    let mut findings = Vec::new();
    if statements.len() > 1 {
        findings.push(Finding::blocking(
            "multi_statement",
            format!(
                "包含 {} 条语句；AI 工具只允许单条执行，请拆分后逐条确认",
                statements.len()
            ),
        ));
    }

    let mut risk = RiskLevel::ReadOnly;
    for statement in &statements {
        let (statement_risk, mut statement_findings) = review_statement(&statement.text);
        risk = risk.max(statement_risk);
        findings.append(&mut statement_findings);
    }
    finish(risk, findings, ctx)
}

fn finish(risk: RiskLevel, mut findings: Vec<Finding>, ctx: &ReviewContext) -> Review {
    if ctx.read_only && risk != RiskLevel::ReadOnly {
        findings.push(Finding::blocking(
            "read_only",
            "该连接为只读模式，写语句已被拦截",
        ));
    }
    if ctx.production && risk != RiskLevel::ReadOnly {
        findings.push(Finding::blocking(
            "production",
            "生产连接不允许自动执行写语句，只能回填编辑器由人工执行",
        ));
    }

    // 多语句里同一条规则可能重复命中，按 (code, message) 去重。
    let mut seen: Vec<(&'static str, String)> = Vec::new();
    findings.retain(|finding| {
        let key = (finding.code, finding.message.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });

    let blocked = findings
        .iter()
        .any(|finding| finding.severity == Severity::Blocking);
    Review {
        risk,
        findings,
        blocked,
    }
}

fn review_statement(text: &str) -> (RiskLevel, Vec<Finding>) {
    let toks: Vec<Token<'_>> = tokens(text)
        .into_iter()
        .filter(|token| !token.is_trivia())
        .collect();
    let head = toks.first().map(|token| token.upper()).unwrap_or_default();
    let kind = classify(text);
    let mut findings = Vec::new();
    let risk = statement_risk(text, &toks, &mut findings);

    if kind == StatementKind::Ddl {
        let irreversible = matches!(head.as_str(), "DROP" | "TRUNCATE");
        findings.push(Finding::warning(
            "ddl",
            if irreversible {
                "DROP / TRUNCATE 会移除数据或结构，执行后无法回滚"
            } else {
                "DDL 会改变数据库结构，可能无法回滚"
            },
        ));
    }

    let has_where = toks.iter().any(|token| token.is_word("WHERE"));
    let has_merge = toks.iter().any(|token| token.is_word("MERGE"));
    let has_update = toks.iter().any(|token| token.is_word("UPDATE"));
    let has_delete = toks.iter().any(|token| token.is_word("DELETE"));
    // CTE 里的 WHERE 不能替外层的全表 DELETE / UPDATE 免责：
    // 先在顶层找 DML，再退回“任何位置”的保守检查。
    let top_level_delete = has_top_level_word(&toks, "DELETE");
    let top_level_update = has_top_level_word(&toks, "UPDATE");
    let top_level_where = has_top_level_word(&toks, "WHERE");
    if top_level_delete && !top_level_where {
        findings.push(Finding::blocking(
            "delete_without_where",
            "DELETE 未带 WHERE，AI 路径一律拒绝；确需全表操作请在编辑器手动执行",
        ));
    } else if top_level_update && !has_merge && !top_level_where {
        findings.push(Finding::blocking(
            "update_without_where",
            "UPDATE 未带 WHERE，AI 路径一律拒绝；确需全表操作请在编辑器手动执行",
        ));
    } else if has_delete && !has_where {
        findings.push(Finding::blocking(
            "delete_without_where",
            "DELETE 未带 WHERE，AI 路径一律拒绝；确需全表操作请在编辑器手动执行",
        ));
    } else if has_update && !has_merge && !has_where {
        findings.push(Finding::blocking(
            "update_without_where",
            "UPDATE 未带 WHERE，AI 路径一律拒绝；确需全表操作请在编辑器手动执行",
        ));
    }

    if is_upsert(&toks, &head) {
        findings.push(Finding::warning(
            "upsert",
            "REPLACE / MERGE / UPSERT 可能覆盖已有行",
        ));
    }

    if head == "USE" {
        findings.push(Finding::warning(
            "use_database",
            "USE 会切换当前库，影响后续语句",
        ));
    }

    let account_head = matches!(head.as_str(), "CREATE" | "ALTER" | "DROP")
        && toks.get(1).is_some_and(|token| {
            ["USER", "ROLE", "LOGIN"]
                .iter()
                .any(|word| token.is_word(word))
        });
    if matches!(head.as_str(), "GRANT" | "REVOKE" | "DENY") || account_head {
        findings.push(Finding::warning(
            "privilege",
            "涉及账号 / 权限变更，确认前请核对目标",
        ));
    }

    for (label, depth) in qualified_targets(&toks) {
        let message = format!("{label} 是 {depth} 段限定名，可能操作其他库");
        findings.push(if risk == RiskLevel::ReadOnly {
            Finding::info("cross_database", message)
        } else {
            Finding::warning("cross_database", message)
        });
    }

    if kind == StatementKind::Other && !is_read_only(text) {
        findings.push(Finding::warning(
            "unclassified",
            "无法静态判定该语句的影响（CALL / EXEC / SET 等），已按写入处理",
        ));
    }

    (risk, findings)
}

fn statement_risk(text: &str, toks: &[Token<'_>], findings: &mut Vec<Finding>) -> RiskLevel {
    match explain_analyze(toks, text) {
        Some(Some(target)) => {
            findings.push(Finding::warning(
                "explain_analyze",
                "EXPLAIN ANALYZE 会真实执行语句，已按底层语句定级",
            ));
            return target;
        }
        Some(None) => {
            findings.push(Finding::warning(
                "explain_analyze",
                "无法定位 EXPLAIN ANALYZE 后的语句，按写入处理",
            ));
            return RiskLevel::Write;
        }
        None => {}
    }

    if is_read_only(text) {
        return RiskLevel::ReadOnly;
    }

    let kind = classify(text);
    if kind == StatementKind::Select {
        if has_top_level_word(toks, "INTO") {
            findings.push(Finding::warning(
                "select_into",
                "SELECT ... INTO 会写入新表 / 文件，不是纯查询",
            ));
        } else {
            findings.push(Finding::warning(
                "locking_select",
                "SELECT ... FOR UPDATE / SHARE 会加锁，不是纯只读查询",
            ));
        }
        return RiskLevel::Write;
    }

    match kind {
        StatementKind::Select | StatementKind::Explain | StatementKind::Tcl => RiskLevel::ReadOnly,
        StatementKind::Insert | StatementKind::Update => RiskLevel::Write,
        StatementKind::Delete | StatementKind::Ddl => RiskLevel::Destructive,
        StatementKind::Other => RiskLevel::Write,
    }
}

/// `EXPLAIN ANALYZE` 会真实执行语句，按 `ANALYZE` 之后的语句定级。
///
/// 返回 `Some(Some(risk))` 表示找到底层语句；`Some(None)` 表示有 ANALYZE
/// 但后面找不到可识别的语句；`None` 表示不是 EXPLAIN ANALYZE。
fn explain_analyze(toks: &[Token<'_>], text: &str) -> Option<Option<RiskLevel>> {
    let head = toks.first()?;
    if !head.is_word("EXPLAIN") {
        return None;
    }
    let analyze = toks.iter().position(|token| token.is_word("ANALYZE"))?;
    let next = toks[analyze + 1..]
        .iter()
        .find(|token| is_statement_start(token));
    Some(next.map(|token| crate::agent::classify_sql(&text[token.start..])))
}

fn is_statement_start(token: &Token<'_>) -> bool {
    token.kind == TokenKind::Word
        && matches!(
            token.upper().as_str(),
            "SELECT"
                | "WITH"
                | "INSERT"
                | "REPLACE"
                | "UPDATE"
                | "DELETE"
                | "MERGE"
                | "CREATE"
                | "ALTER"
                | "DROP"
                | "TRUNCATE"
                | "CALL"
                | "EXEC"
                | "EXECUTE"
        )
}

/// 只看括号外（顶层）的关键字，避免把子查询里的 WHERE 当成过滤条件。
fn has_top_level_word(toks: &[Token<'_>], word: &str) -> bool {
    let mut depth = 0i32;
    for token in toks {
        if token.kind == TokenKind::Punct {
            match token.text {
                "(" => depth += 1,
                ")" => depth -= 1,
                _ => {}
            }
            continue;
        }
        if depth == 0 && token.is_word(word) {
            return true;
        }
    }
    false
}

fn is_upsert(toks: &[Token<'_>], head: &str) -> bool {
    if matches!(head, "REPLACE" | "MERGE") {
        return true;
    }
    if head != "INSERT" {
        return false;
    }
    toks.iter().take(4).any(|token| token.is_word("REPLACE"))
        || has_top_level_word(toks, "DUPLICATE")
        || has_top_level_word(toks, "CONFLICT")
}

/// `FROM / JOIN / UPDATE / INTO / TABLE / REFERENCES` 之后的三段及以上限定名：
/// 可能引用其他库（`db.schema.table` / `db.table.column`）。
fn qualified_targets(toks: &[Token<'_>]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for (index, token) in toks.iter().enumerate() {
        if !(token.is_word("FROM")
            || token.is_word("JOIN")
            || token.is_word("UPDATE")
            || token.is_word("INTO")
            || token.is_word("TABLE")
            || token.is_word("REFERENCES"))
        {
            continue;
        }
        let mut start = index + 1;
        while toks
            .get(start)
            .is_some_and(|token| token.is_word("ONLY") || token.is_word("LATERAL"))
        {
            start += 1;
        }
        let Some((label, depth)) = chain_at(toks, start) else {
            continue;
        };
        if depth >= 3 && !out.iter().any(|(existing, _)| existing == &label) {
            out.push((label, depth));
        }
    }
    out
}

fn chain_at(toks: &[Token<'_>], start: usize) -> Option<(String, usize)> {
    let is_ident =
        |token: &Token<'_>| matches!(token.kind, TokenKind::Word | TokenKind::QuotedIdent);
    if !toks.get(start).is_some_and(is_ident) {
        return None;
    }
    let mut end = start;
    let mut depth = 1;
    while end + 2 < toks.len() && toks[end + 1].text == "." && is_ident(&toks[end + 2]) {
        depth += 1;
        end += 2;
    }
    let label = toks[start..=end]
        .iter()
        .map(|token| token.text)
        .collect::<Vec<_>>()
        .join("");
    Some((label, depth))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(sql: &str) -> Review {
        review(sql, &ReviewContext::default())
    }

    #[test]
    fn read_only_statements_pass_cleanly() {
        for sql in ["SELECT 1", "SELECT * FROM users", "SHOW TABLES"] {
            let result = plain(sql);
            assert_eq!(result.risk, RiskLevel::ReadOnly, "{sql}");
            assert!(!result.blocked, "{sql}");
            assert!(result.findings.is_empty(), "{sql}: {:?}", result.findings);
        }
    }

    #[test]
    fn update_and_delete_without_where_are_blocked() {
        let update = plain("UPDATE t SET a = 1");
        assert_eq!(update.risk, RiskLevel::Write);
        assert!(update.has("update_without_where"));
        assert!(update.blocked, "AC-AI-04：无 WHERE 的改写必须拒绝");

        let delete = plain("DELETE FROM t");
        assert_eq!(delete.risk, RiskLevel::Destructive);
        assert!(delete.has("delete_without_where"));
        assert!(delete.blocked);

        assert!(!plain("UPDATE t SET a = 1 WHERE id = 1").has("update_without_where"));
        // 子查询里的 WHERE 不算顶层过滤条件
        let nested = plain("DELETE FROM t WHERE id IN (SELECT id FROM x)");
        assert!(nested.findings.is_empty());
        assert!(!nested.blocked);
    }

    #[test]
    fn ddl_and_upsert_escalate_with_findings() {
        let drop = plain("DROP TABLE t");
        assert_eq!(drop.risk, RiskLevel::Destructive);
        assert!(drop.has("ddl"));
        assert!(!drop.has("cross_database"));

        let replace = plain("REPLACE INTO t VALUES (1)");
        assert_eq!(replace.risk, RiskLevel::Write);
        assert!(replace.has("upsert"));

        let merge = plain("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE");
        assert_eq!(merge.risk, RiskLevel::Write);
        assert!(merge.has("upsert"));
    }

    #[test]
    fn multi_statement_is_blocked_and_takes_highest_risk() {
        let result = plain("SELECT 1; DELETE FROM t");
        assert!(result.blocked);
        assert!(result.has("multi_statement"));
        assert_eq!(result.risk, RiskLevel::Destructive);

        // 即使全是只读，AI 工具也只允许单条
        assert!(plain("SELECT 1; SELECT 2").blocked);
    }

    #[test]
    fn explain_analyze_uses_the_underlying_statement() {
        let delete = plain("EXPLAIN ANALYZE DELETE FROM t");
        assert_eq!(delete.risk, RiskLevel::Destructive);
        assert!(delete.has("explain_analyze"));

        let select = plain("EXPLAIN ANALYZE SELECT * FROM t");
        assert_eq!(select.risk, RiskLevel::ReadOnly);
        assert!(select.has("explain_analyze"));

        // 普通 EXPLAIN 不执行语句，仍然只读
        let explain = plain("EXPLAIN DELETE FROM t");
        assert_eq!(explain.risk, RiskLevel::ReadOnly);
        assert!(!explain.has("explain_analyze"));
    }

    #[test]
    fn select_into_is_a_write() {
        let result = plain("SELECT * INTO backup FROM t");
        assert_eq!(result.risk, RiskLevel::Write);
        assert!(result.has("select_into"));
    }

    #[test]
    fn cte_writes_on_new_lines_are_not_treated_as_read_only() {
        let update = plain("WITH x AS (SELECT 1)\nUPDATE t SET a = 1 WHERE id = 1");
        assert_eq!(update.risk, RiskLevel::Write);

        let delete = plain("WITH x AS (SELECT 1)\nDELETE FROM t WHERE id = 1");
        assert_eq!(delete.risk, RiskLevel::Destructive);

        let data_modifying =
            plain("WITH x AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM x");
        assert_eq!(data_modifying.risk, RiskLevel::Write);

        // CTE 内部的 WHERE 不能替外层全表 DELETE 免责。
        assert!(plain("WITH x AS (SELECT 1 WHERE 1 = 1) DELETE FROM t").blocked);
    }

    #[test]
    fn call_and_select_for_update_are_writes() {
        assert_eq!(plain("CALL cleanup()").risk, RiskLevel::Write);
        assert_eq!(plain("SELECT * FROM t FOR UPDATE").risk, RiskLevel::Write);
        assert_eq!(plain("SELECT * INTO backup FROM t").risk, RiskLevel::Write);
    }

    #[test]
    fn read_only_and_production_connections_block_writes() {
        let read_only = ReviewContext {
            read_only: true,
            production: false,
        };
        let write = review("UPDATE t SET a = 1 WHERE id = 1", &read_only);
        assert!(write.blocked);
        assert!(write.has("read_only"));
        assert!(!review("SELECT 1", &read_only).blocked);

        let production = ReviewContext {
            read_only: false,
            production: true,
        };
        let insert = review("INSERT INTO t VALUES (1)", &production);
        assert!(insert.blocked);
        assert!(insert.has("production"));
        assert!(!review("SELECT 1", &production).blocked);
    }

    #[test]
    fn cross_database_targets_and_use_are_flagged() {
        let delete = plain("DELETE FROM prod.dbo.users WHERE id = 1");
        assert!(delete.has("cross_database"), "{:?}", delete.findings);

        let select = plain("SELECT * FROM public.users");
        assert!(
            !select.has("cross_database"),
            "两段式 schema.table 不算跨库"
        );

        let use_db = plain("USE prod");
        assert!(use_db.has("use_database"));
        assert_eq!(use_db.risk, RiskLevel::Write);

        assert!(plain("GRANT SELECT ON t TO bob").has("privilege"));
        assert!(plain("CREATE USER audit IDENTIFIED BY 'x'").has("privilege"));
    }

    #[test]
    fn unknown_statements_are_treated_as_writes() {
        let call = plain("CALL cleanup()");
        assert_eq!(call.risk, RiskLevel::Write);
        assert!(call.has("unclassified"));

        let explain = plain("EXPLAIN ANALYZE");
        assert_eq!(explain.risk, RiskLevel::Write);
        assert!(explain.has("explain_analyze"));
    }

    #[test]
    fn summary_lists_each_finding_once() {
        let result = plain("DELETE FROM prod.dbo.users; DELETE FROM prod.dbo.users");
        let summary = result.summary();
        assert!(summary.contains("阻断：包含 2 条语句"), "{summary}");
        assert_eq!(summary.matches("DELETE 未带 WHERE").count(), 1, "{summary}");
    }
}
