//! 写入门控（F-96）：**默认只读**，写与危险 SQL 各需一个显式开关。
//!
//! 口径：
//! * 只读语句（SELECT / SHOW / EXPLAIN …）不需要任何开关；
//! * 写语句（INSERT / UPDATE / DDL / 无法判定的语句）需要 `--write`；
//! * 危险语句（DELETE / DROP / TRUNCATE / 无 WHERE 的改删）另需 `--danger`；
//! * 只读连接与生产连接**一律拒绝写**（CLI 没有解锁窗口）。

use rustdb_core::review::{review, ReviewContext};
use rustdb_core::{ConnectionProfile, RiskLevel};
use rustdb_sql_lang::{classify, first_keyword, split_statements_for, Flavor, StatementKind};

use crate::error::CliError;

/// 用户给出的开关。
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteGate {
    pub write: bool,
    pub danger: bool,
}

/// 语句级判定结果。
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub statements: usize,
    pub needs_write: bool,
    pub needs_danger: bool,
    /// 危险语句的位置说明（第几条 + 首个关键字）。
    pub dangerous: Vec<String>,
}

/// 逐语句判定风险；只读语句不触发任何开关要求。
pub fn analyze(sql: &str, flavor: Flavor) -> Result<Plan, CliError> {
    let statements = split_statements_for(sql, flavor);
    if statements.is_empty() {
        return Err(CliError::usage("无法解析 SQL"));
    }
    let mut plan = Plan {
        statements: statements.len(),
        ..Plan::default()
    };
    for (index, statement) in statements.iter().enumerate() {
        // 逐条审查：整段审查会把「多语句」一律判为阻断，CLI 允许多语句批次。
        let reviewed = review(&statement.text, &ReviewContext::default());
        if reviewed.risk != RiskLevel::ReadOnly {
            plan.needs_write = true;
        }
        let kind = classify(&statement.text);
        let head = first_keyword(&statement.text);
        let without_where =
            reviewed.has("update_without_where") || reviewed.has("delete_without_where");
        let destructive = kind == StatementKind::Delete
            || (kind == StatementKind::Ddl && matches!(head.as_str(), "DROP" | "TRUNCATE"))
            || without_where;
        if destructive {
            plan.needs_write = true;
            plan.needs_danger = true;
            plan.dangerous
                .push(format!("第 {} 条（{}）", index + 1, head));
        }
    }
    Ok(plan)
}

/// 门控总入口：返回 `Err` 即拒绝执行。
pub fn check(sql: &str, gate: WriteGate, profile: &ConnectionProfile) -> Result<(), CliError> {
    let plan = analyze(sql, rustdb_core::params::flavor_for(profile.driver))?;
    if !plan.needs_write {
        return Ok(());
    }
    // 连接级限制优先：即使给了开关也不放行。
    if profile.read_only {
        return Err(CliError::permission("该连接为只读模式，写操作已被拦截"));
    }
    if profile.production {
        return Err(CliError::permission(
            "生产连接的写操作在 CLI 中一律拒绝（CLI 没有解锁窗口）",
        ));
    }
    if !gate.write {
        let detail = if plan.statements > 1 {
            format!("（本次共 {} 条语句）", plan.statements)
        } else {
            String::new()
        };
        return Err(CliError::permission(format!(
            "CLI 默认只读：写操作需要 --write 开关{detail}"
        )));
    }
    if plan.needs_danger && !gate.danger {
        return Err(CliError::permission(format!(
            "危险语句需要 --danger 开关：{}",
            plan.dangerous.join("、")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> ConnectionProfile {
        ConnectionProfile::demo_sqlite()
    }

    #[test]
    fn read_only_sql_needs_no_switch() {
        assert!(check("SELECT 1", WriteGate::default(), &profile()).is_ok());
        assert!(check("SHOW TABLES", WriteGate::default(), &profile()).is_ok());
    }

    #[test]
    fn write_needs_the_write_switch() {
        let error = check("INSERT INTO t VALUES (1)", WriteGate::default(), &profile())
            .expect_err("应拒绝");
        assert_eq!(error.code(), "E_PERMISSION");
        assert!(error.message.contains("--write"), "{}", error.message);

        assert!(check(
            "INSERT INTO t VALUES (1)",
            WriteGate {
                write: true,
                danger: false
            },
            &profile()
        )
        .is_ok());
    }

    #[test]
    fn dangerous_sql_needs_a_second_switch() {
        let gate = WriteGate {
            write: true,
            danger: false,
        };
        let error = check("DELETE FROM t WHERE id = 1", gate, &profile()).expect_err("应拒绝");
        assert_eq!(error.code(), "E_PERMISSION");
        assert!(error.message.contains("--danger"), "{}", error.message);

        let error = check("DROP TABLE t", gate, &profile()).expect_err("应拒绝");
        assert!(error.message.contains("DROP"), "{}", error.message);

        assert!(check(
            "DELETE FROM t WHERE id = 1",
            WriteGate {
                write: true,
                danger: true
            },
            &profile()
        )
        .is_ok());
    }

    #[test]
    fn update_without_where_counts_as_dangerous() {
        let gate = WriteGate {
            write: true,
            danger: false,
        };
        let error = check("UPDATE t SET a = 1", gate, &profile()).expect_err("应拒绝");
        assert!(error.message.contains("--danger"), "{}", error.message);
        assert!(check("UPDATE t SET a = 1 WHERE id = 1", gate, &profile()).is_ok());
    }

    #[test]
    fn read_only_and_production_profiles_always_refuse_writes() {
        let gate = WriteGate {
            write: true,
            danger: true,
        };
        let mut read_only = profile();
        read_only.read_only = true;
        let error = check("INSERT INTO t VALUES (1)", gate, &read_only).expect_err("应拒绝");
        assert!(error.message.contains("只读"), "{}", error.message);

        let mut production = profile();
        production.production = true;
        let error = check("INSERT INTO t VALUES (1)", gate, &production).expect_err("应拒绝");
        assert!(error.message.contains("生产"), "{}", error.message);
    }

    #[test]
    fn cte_update_across_newlines_still_needs_write() {
        let sql = "WITH x AS (SELECT 1)\nUPDATE t SET a = 1 WHERE id = 1";
        let error = check(sql, WriteGate::default(), &profile()).expect_err("应拒绝");
        assert!(error.message.contains("--write"), "{}", error.message);
    }

    #[test]
    fn multi_statement_is_checked_statement_by_statement() {
        let gate = WriteGate::default();
        let error =
            check("SELECT 1; INSERT INTO t VALUES (1)", gate, &profile()).expect_err("应拒绝");
        assert!(error.message.contains("--write"), "{}", error.message);
        assert!(check("SELECT 1; SELECT 2", WriteGate::default(), &profile()).is_ok());
    }

    #[test]
    fn plan_reports_statement_count_and_dangerous_positions() {
        let plan = analyze("SELECT 1; DELETE FROM t", Flavor::Generic).expect("分析");
        assert_eq!(plan.statements, 2);
        assert!(plan.needs_write);
        assert!(plan.needs_danger);
        assert_eq!(plan.dangerous.len(), 1);
        assert!(plan.dangerous[0].contains("第 2 条"));
    }
}
