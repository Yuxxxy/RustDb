//! 分区管理（F-85）：DDL 规划与方言差异。
//!
//! 首期只覆盖 PostgreSQL 的声明式分区；其它方言显式返回不支持，
//! UI 只消费 `PartitionPlan` / `PartitionStrategy`，不做 `match DbKind`。

use rustdb_db_api::{DbKind, ObjectPath};

/// 父表的分区策略（来自元数据的 `range` / `list` / `hash`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionStrategy {
    Range,
    List,
    Hash,
}

impl PartitionStrategy {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "range" | "r" => Some(Self::Range),
            "list" | "l" => Some(Self::List),
            "hash" | "h" => Some(Self::Hash),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Range => "范围",
            Self::List => "列表",
            Self::Hash => "哈希",
        }
    }

    /// DEFAULT 分区只对范围 / 列表有意义。
    pub fn accepts_default(self) -> bool {
        !matches!(self, Self::Hash)
    }
}

/// 一个分区的边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionBound {
    /// `FOR VALUES FROM (...) TO (...)`；两端可写 `MINVALUE` / `MAXVALUE`。
    Range {
        from: String,
        to: String,
    },
    /// `FOR VALUES IN (...)`；每个值原样拼入（字符串需自带引号）。
    List {
        values: Vec<String>,
    },
    /// `FOR VALUES WITH (MODULUS m, REMAINDER r)`。
    Hash {
        modulus: u32,
        remainder: u32,
    },
    Default,
}

impl PartitionBound {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Range { .. } => "范围",
            Self::List { .. } => "列表",
            Self::Hash { .. } => "哈希",
            Self::Default => "DEFAULT",
        }
    }

    fn sql(&self) -> Result<String, String> {
        match self {
            Self::Range { from, to } => {
                let from = checked_fragment(from, "范围起点")?;
                let to = checked_fragment(to, "范围终点")?;
                if from.eq_ignore_ascii_case(&to) {
                    return Err("范围起点与终点不能相同".into());
                }
                Ok(format!("FOR VALUES FROM ({from}) TO ({to})"))
            }
            Self::List { values } => {
                let mut parts = Vec::new();
                for value in values {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    parts.push(checked_fragment(trimmed, "列表值")?);
                }
                if parts.is_empty() {
                    return Err("列表边界至少需要一个值".into());
                }
                Ok(format!("FOR VALUES IN ({})", parts.join(", ")))
            }
            Self::Hash { modulus, remainder } => {
                if *modulus == 0 {
                    return Err("MODULUS 必须大于 0".into());
                }
                if remainder >= modulus {
                    return Err("REMAINDER 必须小于 MODULUS".into());
                }
                Ok(format!(
                    "FOR VALUES WITH (MODULUS {modulus}, REMAINDER {remainder})"
                ))
            }
            Self::Default => Ok("DEFAULT".into()),
        }
    }
}

/// 一次分区操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionAction {
    Create {
        name: String,
        bound: PartitionBound,
    },
    Attach {
        table: String,
        bound: PartitionBound,
    },
    Detach {
        partition: String,
        concurrently: bool,
    },
    Drop {
        partition: String,
    },
}

impl PartitionAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Create { .. } => "创建分区",
            Self::Attach { .. } => "附加分区",
            Self::Detach { .. } => "分离分区",
            Self::Drop { .. } => "删除分区",
        }
    }
}

/// 分区操作的执行计划（一条 DDL）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionPlan {
    pub statements: Vec<String>,
    /// 执行前必须让用户看到的警示（不可回滚 / 版本要求等）。
    pub warning: Option<String>,
    /// 无法执行的原因（显式降级）。
    pub error: Option<String>,
    /// 预览里的一句说明。
    pub summary: String,
}

impl PartitionPlan {
    fn fail(summary: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            statements: Vec::new(),
            warning: None,
            error: Some(error.into()),
            summary: summary.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none() && !self.statements.is_empty()
    }

    /// 预览文本：说明 + 警告 + 语句。
    pub fn preview_text(&self) -> String {
        let mut lines = vec![format!("-- {}", self.summary)];
        if let Some(warning) = &self.warning {
            lines.push(format!("-- ⚠ {warning}"));
        }
        lines.extend(self.statements.iter().cloned());
        lines.join("\n")
    }
}

/// 分区管理首期只支持 PostgreSQL。
pub fn supports(kind: DbKind) -> bool {
    kind == DbKind::Postgres
}

/// 生成分区 DDL。
pub fn plan(kind: DbKind, parent: &ObjectPath, action: &PartitionAction) -> PartitionPlan {
    let summary = format!("{}（{}）", action.label(), parent.display_label());
    if !supports(kind) {
        return PartitionPlan::fail(
            summary,
            "分区管理首期仅支持 PostgreSQL，其它方言按能力矩阵后续补充",
        );
    }
    let bound = match action {
        PartitionAction::Create { bound, .. } | PartitionAction::Attach { bound, .. } => {
            match bound.sql() {
                Ok(bound) => Some(bound),
                Err(error) => return PartitionPlan::fail(summary, error),
            }
        }
        _ => None,
    };
    let parent_sql = crate::core::object_from_clause(kind, parent);
    let child_sql = |name: &str| -> Result<String, String> {
        let name = checked_identifier(name)?;
        let path = ObjectPath::new(parent.ns.clone(), name, parent.kind);
        Ok(crate::core::object_from_clause(kind, &path))
    };

    let (statement, warning) = match action {
        PartitionAction::Create { name, .. } => match child_sql(name) {
            Ok(child) => (
                format!(
                    "CREATE TABLE {child} PARTITION OF {parent_sql} {};",
                    bound.unwrap_or_default()
                ),
                None,
            ),
            Err(error) => return PartitionPlan::fail(summary, error),
        },
        PartitionAction::Attach { table, .. } => match child_sql(table) {
            Ok(child) => (
                format!(
                    "ALTER TABLE {parent_sql} ATTACH PARTITION {child} {};",
                    bound.unwrap_or_default()
                ),
                Some(
                    "被附加的表必须已存在且数据满足边界；PostgreSQL 会做全表校验，大表可能较慢"
                        .to_string(),
                ),
            ),
            Err(error) => return PartitionPlan::fail(summary, error),
        },
        PartitionAction::Detach {
            partition,
            concurrently,
        } => match child_sql(partition) {
            Ok(child) => {
                let statement = if *concurrently {
                    format!("ALTER TABLE {parent_sql} DETACH PARTITION {child} CONCURRENTLY;")
                } else {
                    format!("ALTER TABLE {parent_sql} DETACH PARTITION {child};")
                };
                let warning = if *concurrently {
                    "CONCURRENTLY 需要 PostgreSQL 14+，且不能在事务块内执行".to_string()
                } else {
                    "分离后分区变成独立表；分离期间对该父表的查询会被短暂阻塞".to_string()
                };
                (statement, Some(warning))
            }
            Err(error) => return PartitionPlan::fail(summary, error),
        },
        PartitionAction::Drop { partition } => match child_sql(partition) {
            Ok(child) => (
                format!("DROP TABLE {child};"),
                Some("会连同分区内的全部数据一起删除，且不可回滚".to_string()),
            ),
            Err(error) => return PartitionPlan::fail(summary, error),
        },
    };

    PartitionPlan {
        statements: vec![statement],
        warning,
        error: None,
        summary,
    }
}

/// 边界片段原样拼进 DDL，但拒绝用分号夹带第二条语句。
fn checked_fragment(text: &str, label: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    if trimmed.contains(';') {
        return Err(format!("{label}不能包含分号"));
    }
    Ok(trimmed.to_string())
}

/// 分区名只允许未加引号的常规标识符（首字母 / 下划线开头）。
fn checked_identifier(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("分区名不能为空".into());
    }
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return Err("分区名不能为空".into());
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err("分区名必须以字母或下划线开头".into());
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err("分区名只能包含字母、数字与下划线".into());
    }
    if trimmed.len() > 63 {
        return Err("分区名超过 PostgreSQL 的 63 字节上限".into());
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{Namespace, ObjectKind};

    fn parent() -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("shop"), Some("public")),
            "orders",
            ObjectKind::Table,
        )
    }

    #[test]
    fn create_range_partition_matches_postgres_syntax() {
        let plan = plan(
            DbKind::Postgres,
            &parent(),
            &PartitionAction::Create {
                name: "orders_2026_01".into(),
                bound: PartitionBound::Range {
                    from: "'2026-01-01'".into(),
                    to: "'2026-02-01'".into(),
                },
            },
        );
        assert!(plan.is_ok(), "{:?}", plan.error);
        assert_eq!(
            plan.statements[0],
            "CREATE TABLE \"shop\".\"public\".\"orders_2026_01\" PARTITION OF \
             \"shop\".\"public\".\"orders\" FOR VALUES FROM ('2026-01-01') TO ('2026-02-01');"
        );
        assert!(plan.preview_text().contains("-- 创建分区"));
        assert!(plan.warning.is_none());
    }

    #[test]
    fn bound_variants_build_expected_fragments() {
        assert_eq!(
            PartitionBound::List {
                values: vec!["'a'".into(), "'b'".into()]
            }
            .sql()
            .unwrap(),
            "FOR VALUES IN ('a', 'b')"
        );
        assert_eq!(
            PartitionBound::Hash {
                modulus: 4,
                remainder: 1
            }
            .sql()
            .unwrap(),
            "FOR VALUES WITH (MODULUS 4, REMAINDER 1)"
        );
        assert_eq!(PartitionBound::Default.sql().unwrap(), "DEFAULT");
        assert!(PartitionBound::Hash {
            modulus: 0,
            remainder: 0
        }
        .sql()
        .is_err());
        assert!(PartitionBound::Hash {
            modulus: 4,
            remainder: 4
        }
        .sql()
        .is_err());
    }

    #[test]
    fn attach_detach_and_drop_carry_warnings() {
        let attach = plan(
            DbKind::Postgres,
            &parent(),
            &PartitionAction::Attach {
                table: "orders_2025".into(),
                bound: PartitionBound::Default,
            },
        );
        assert_eq!(
            attach.statements[0],
            "ALTER TABLE \"shop\".\"public\".\"orders\" ATTACH PARTITION \
             \"shop\".\"public\".\"orders_2025\" DEFAULT;"
        );
        assert!(attach.warning.unwrap().contains("全表校验"));

        let detach = plan(
            DbKind::Postgres,
            &parent(),
            &PartitionAction::Detach {
                partition: "orders_2025".into(),
                concurrently: true,
            },
        );
        assert!(detach.statements[0].contains("DETACH PARTITION"));
        assert!(detach.statements[0].ends_with("CONCURRENTLY;"));
        assert!(detach.warning.unwrap().contains("PostgreSQL 14+"));

        let drop = plan(
            DbKind::Postgres,
            &parent(),
            &PartitionAction::Drop {
                partition: "orders_2025".into(),
            },
        );
        assert_eq!(
            drop.statements[0],
            "DROP TABLE \"shop\".\"public\".\"orders_2025\";"
        );
        assert!(drop.warning.unwrap().contains("不可回滚"));
    }

    #[test]
    fn rejects_bad_identifiers_bounds_and_dialects() {
        let bad_name = plan(
            DbKind::Postgres,
            &parent(),
            &PartitionAction::Create {
                name: "1drop".into(),
                bound: PartitionBound::Default,
            },
        );
        assert!(bad_name.error.unwrap().contains("字母或下划线"));

        let smuggled = plan(
            DbKind::Postgres,
            &parent(),
            &PartitionAction::Create {
                name: "p1".into(),
                bound: PartitionBound::Range {
                    from: "'a'; DROP TABLE x".into(),
                    to: "'b'".into(),
                },
            },
        );
        assert!(smuggled.error.unwrap().contains("分号"));

        let mysql = plan(
            DbKind::Mysql,
            &parent(),
            &PartitionAction::Drop {
                partition: "p1".into(),
            },
        );
        assert!(mysql.error.unwrap().contains("仅支持 PostgreSQL"));
    }

    #[test]
    fn strategy_parses_driver_codes() {
        assert_eq!(
            PartitionStrategy::parse("r"),
            Some(PartitionStrategy::Range)
        );
        assert_eq!(
            PartitionStrategy::parse("LIST"),
            Some(PartitionStrategy::List)
        );
        assert_eq!(PartitionStrategy::parse("h"), Some(PartitionStrategy::Hash));
        assert_eq!(PartitionStrategy::parse("x"), None);
        assert!(!PartitionStrategy::Hash.accepts_default());
        assert!(PartitionStrategy::Range.accepts_default());
    }
}
