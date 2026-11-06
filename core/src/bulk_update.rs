//! F-73：外键跳转与按条件批量更新的纯逻辑层。
//!
//! 只生成 SQL 与校验，不执行、不碰 IO。所有标识符按方言引用，值统一走
//! [`crate::edit::literal`]；因此条件值只作为字面量出现，不做字符串拼接。

use rustdb_db_api::{DbKind, ForeignKeyInfo, Namespace, ObjectKind, ObjectPath, Value};

use crate::core::object_from_clause;
use crate::edit::{ident, literal};

/// 条件运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    In,
    IsNull,
    IsNotNull,
}

impl BulkOp {
    pub const ALL: [Self; 10] = [
        Self::Eq,
        Self::Ne,
        Self::Lt,
        Self::Le,
        Self::Gt,
        Self::Ge,
        Self::Like,
        Self::In,
        Self::IsNull,
        Self::IsNotNull,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Like => "LIKE",
            Self::In => "IN",
            Self::IsNull => "IS NULL",
            Self::IsNotNull => "IS NOT NULL",
        }
    }

    pub fn needs_value(self) -> bool {
        !matches!(self, Self::IsNull | Self::IsNotNull)
    }

    /// 稳定的 ASCII 标识（UI 选择器 / 测试用）。
    pub fn slug(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::Like => "like",
            Self::In => "in",
            Self::IsNull => "isnull",
            Self::IsNotNull => "isnotnull",
        }
    }
}

/// 多个条件之间的连接词。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkJoin {
    And,
    Or,
}

impl BulkJoin {
    pub const ALL: [Self; 2] = [Self::And, Self::Or];

    pub fn label(self) -> &'static str {
        match self {
            Self::And => "AND",
            Self::Or => "OR",
        }
    }
}

/// 一行条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkCondition {
    pub column: String,
    pub op: BulkOp,
    pub value: String,
}

/// 条件模式：结构化条件或用户自定义 WHERE。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkFilter {
    Structured {
        join: BulkJoin,
        conditions: Vec<BulkCondition>,
    },
    Custom(String),
}

/// 一次批量更新的请求（SET 单个字段 + 条件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdateRequest {
    pub target: ObjectPath,
    pub set_column: String,
    pub set_value: String,
    pub filter: BulkFilter,
}

/// 生成的预览：先 COUNT，再 UPDATE。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkUpdatePlan {
    pub target: ObjectPath,
    pub set_column: String,
    pub where_sql: String,
    pub count_sql: String,
    pub update_sql: String,
    pub warnings: Vec<String>,
}

impl BulkUpdatePlan {
    pub fn preview_text(&self) -> String {
        format!(
            "-- 影响行数预览\n{}\n-- 批量更新\n{}",
            self.count_sql, self.update_sql
        )
    }
}

const ROLLBACK_WARNING: &str =
    "影响行数预览与执行之间数据可能变化；未加载的行无法生成可靠回滚 SQL。";
const CUSTOM_WARNING: &str = "自定义 WHERE 由你保证语义正确，且不能包含分号或注释。";

/// 生成批量更新计划；任何会让 WHERE 变空或注入多语句的输入都直接拒绝。
pub fn plan(kind: DbKind, request: &BulkUpdateRequest) -> Result<BulkUpdatePlan, String> {
    let set_column = request.set_column.trim();
    if set_column.is_empty() {
        return Err("请填写要更新的字段".into());
    }
    let from = object_from_clause(kind, &request.target);
    let (where_sql, mut warnings) = build_filter(kind, &request.filter)?;
    warnings.insert(0, ROLLBACK_WARNING.to_string());

    let set_literal = literal(kind, &Value::Text(request.set_value.clone().into()))?;
    let count_sql = format!("SELECT COUNT(*) FROM {from} WHERE {where_sql};");
    let update_sql = format!(
        "UPDATE {from} SET {} = {set_literal} WHERE {where_sql};",
        ident(kind, set_column)
    );
    Ok(BulkUpdatePlan {
        target: request.target.clone(),
        set_column: set_column.to_string(),
        where_sql,
        count_sql,
        update_sql,
        warnings,
    })
}

fn build_filter(kind: DbKind, filter: &BulkFilter) -> Result<(String, Vec<String>), String> {
    match filter {
        BulkFilter::Custom(where_sql) => {
            let sql = where_sql.trim();
            if sql.is_empty() {
                return Err("自定义 WHERE 不能为空".into());
            }
            if sql.contains(';') {
                return Err("自定义 WHERE 不能包含分号（不允许堆叠语句）".into());
            }
            for marker in ["--", "/*", "*/"] {
                if sql.contains(marker) {
                    return Err(format!("自定义 WHERE 不能包含注释标记 {marker}"));
                }
            }
            if sql.contains('\0') {
                return Err("自定义 WHERE 含有非法字符".into());
            }
            Ok((sql.to_string(), vec![CUSTOM_WARNING.to_string()]))
        }
        BulkFilter::Structured { join, conditions } => {
            if conditions.is_empty() {
                return Err("至少需要一个条件（批量更新不允许无条件执行）".into());
            }
            let mut parts = Vec::with_capacity(conditions.len());
            for condition in conditions {
                parts.push(build_condition(kind, condition)?);
            }
            Ok((parts.join(&format!(" {} ", join.label())), Vec::new()))
        }
    }
}

fn build_condition(kind: DbKind, condition: &BulkCondition) -> Result<String, String> {
    let column = condition.column.trim();
    if column.is_empty() {
        return Err("条件的字段不能为空".into());
    }
    let column = ident(kind, column);
    match condition.op {
        BulkOp::IsNull => Ok(format!("{column} IS NULL")),
        BulkOp::IsNotNull => Ok(format!("{column} IS NOT NULL")),
        BulkOp::In => {
            let values: Vec<&str> = condition
                .value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect();
            if values.is_empty() {
                return Err(format!("{column} 的 IN 条件至少需要一个值"));
            }
            let mut literals = Vec::with_capacity(values.len());
            for value in values {
                literals.push(literal(kind, &Value::Text(value.to_string().into()))?);
            }
            Ok(format!("{column} IN ({})", literals.join(", ")))
        }
        op => {
            let value = literal(kind, &Value::Text(condition.value.clone().into()))?;
            Ok(format!("{column} {} {value}", op.label()))
        }
    }
}

/// 选中列对应的外键（单列或多列复合外键都返回整条约束）。
pub fn foreign_key_for_column<'a>(
    foreign_keys: &'a [ForeignKeyInfo],
    column: &str,
) -> Option<&'a ForeignKeyInfo> {
    foreign_keys.iter().find(|fk| {
        fk.columns
            .iter()
            .any(|name| name.eq_ignore_ascii_case(column))
    })
}

/// 生成「跳到被引用记录」的查询。
///
/// 复合外键按列顺序全部带上；NULL 用 `IS NULL`。目标表沿用源对象所在命名空间。
pub fn foreign_key_lookup_sql(
    kind: DbKind,
    source_ns: &Namespace,
    foreign_key: &ForeignKeyInfo,
    values: &[Value],
) -> Result<String, String> {
    if foreign_key.referenced_columns.is_empty() {
        return Err("外键缺少被引用列，无法跳转".into());
    }
    if values.len() != foreign_key.referenced_columns.len() {
        return Err(format!(
            "外键 {} 需要 {} 个值，实际拿到 {} 个",
            foreign_key.name,
            foreign_key.referenced_columns.len(),
            values.len()
        ));
    }
    let target = ObjectPath::new(
        source_ns.clone(),
        foreign_key.referenced_table.clone(),
        ObjectKind::Table,
    );
    let mut parts = Vec::with_capacity(values.len());
    for (column, value) in foreign_key.referenced_columns.iter().zip(values) {
        let column = ident(kind, column);
        if value.is_null() {
            parts.push(format!("{column} IS NULL"));
        } else {
            parts.push(format!("{column} = {}", literal(kind, value)?));
        }
    }
    let from = object_from_clause(kind, &target);
    let where_sql = parts.join(" AND ");
    // 分页语法按方言走：T-SQL 用 TOP，Oracle 用 FETCH FIRST，其余用 LIMIT。
    Ok(match kind {
        DbKind::Mssql => format!("SELECT TOP 200 *\nFROM {from}\nWHERE {where_sql};"),
        DbKind::Oracle => {
            format!("SELECT *\nFROM {from}\nWHERE {where_sql}\nFETCH FIRST 200 ROWS ONLY;")
        }
        _ => format!("SELECT *\nFROM {from}\nWHERE {where_sql}\nLIMIT 200;"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("shop"), None::<&str>),
            "orders",
            ObjectKind::Table,
        )
    }

    fn request(filter: BulkFilter) -> BulkUpdateRequest {
        BulkUpdateRequest {
            target: target(),
            set_column: "status".into(),
            set_value: "paid".into(),
            filter,
        }
    }

    fn structured(conditions: Vec<BulkCondition>) -> BulkFilter {
        BulkFilter::Structured {
            join: BulkJoin::And,
            conditions,
        }
    }

    fn condition(column: &str, op: BulkOp, value: &str) -> BulkCondition {
        BulkCondition {
            column: column.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn structured_conditions_quote_columns_and_values() {
        let plan = plan(
            DbKind::Mysql,
            &request(structured(vec![
                condition("user_id", BulkOp::Eq, "7"),
                condition("state", BulkOp::In, "new, paid"),
            ])),
        )
        .expect("应生成计划");
        assert_eq!(
            plan.where_sql,
            "`user_id` = '7' AND `state` IN ('new', 'paid')"
        );
        assert!(plan
            .update_sql
            .starts_with("UPDATE `shop`.`orders` SET `status` = 'paid'"));
        assert!(plan
            .count_sql
            .starts_with("SELECT COUNT(*) FROM `shop`.`orders`"));
        assert!(plan.warnings.iter().any(|w| w.contains("回滚")));
    }

    #[test]
    fn null_operators_ignore_the_value() {
        let plan = plan(
            DbKind::Postgres,
            &request(structured(vec![
                condition("deleted_at", BulkOp::IsNull, ""),
                condition("note", BulkOp::IsNotNull, "ignored"),
            ])),
        )
        .expect("应生成计划");
        assert_eq!(
            plan.where_sql,
            "\"deleted_at\" IS NULL AND \"note\" IS NOT NULL"
        );
    }

    #[test]
    fn custom_where_is_kept_but_stacking_is_rejected() {
        let built = plan(
            DbKind::Mysql,
            &request(BulkFilter::Custom("amount > 100 OR vip = 1".into())),
        )
        .expect("自定义 WHERE 应可用");
        assert!(built.warnings.iter().any(|w| w.contains("自定义 WHERE")));

        let error = plan(
            DbKind::Mysql,
            &request(BulkFilter::Custom("1 = 1; DROP TABLE orders".into())),
        )
        .unwrap_err();
        assert!(error.contains("分号"), "{error}");

        let error = plan(
            DbKind::Mysql,
            &request(BulkFilter::Custom("1 = 1 -- bypass".into())),
        )
        .unwrap_err();
        assert!(error.contains("注释"), "{error}");
    }

    #[test]
    fn empty_conditions_and_columns_are_rejected() {
        assert!(plan(DbKind::Mysql, &request(structured(Vec::new()))).is_err());
        assert!(plan(DbKind::Mysql, &request(BulkFilter::Custom("  ".into()))).is_err());

        let mut req = request(structured(vec![condition("", BulkOp::Eq, "1")]));
        assert!(plan(DbKind::Mysql, &req).is_err());
        req.set_column = "  ".into();
        assert!(plan(DbKind::Mysql, &req).is_err());
        req.set_column = "status".into();
        req.filter = structured(vec![condition("id", BulkOp::In, " , ")]);
        assert!(plan(DbKind::Mysql, &req).is_err());
    }

    #[test]
    fn lookup_sql_targets_referenced_table_and_handles_null() {
        let fk = ForeignKeyInfo {
            name: "fk_orders_customer".into(),
            columns: vec!["customer_id".into()],
            referenced_table: "customers".into(),
            referenced_columns: vec!["id".into()],
            on_delete: None,
            on_update: None,
        };
        let sql = foreign_key_lookup_sql(
            DbKind::Mysql,
            &target().ns,
            &fk,
            &[Value::Text("42".into())],
        )
        .expect("应生成查询");
        assert!(sql.contains("FROM `shop`.`customers`"), "{sql}");
        assert!(sql.contains("WHERE `id` = '42'"), "{sql}");
        assert!(sql.ends_with("LIMIT 200;"), "{sql}");

        let null_sql = foreign_key_lookup_sql(DbKind::Mysql, &target().ns, &fk, &[Value::Null])
            .expect("NULL 应可跳转");
        assert!(null_sql.contains("WHERE `id` IS NULL"), "{null_sql}");

        let mssql = foreign_key_lookup_sql(DbKind::Mssql, &target().ns, &fk, &[Value::Int(7)])
            .expect("应生成查询");
        assert!(mssql.starts_with("SELECT TOP 200 *"), "{mssql}");
        assert!(!mssql.contains("LIMIT"), "{mssql}");

        let oracle = foreign_key_lookup_sql(DbKind::Oracle, &target().ns, &fk, &[Value::Int(7)])
            .expect("应生成查询");
        assert!(oracle.ends_with("FETCH FIRST 200 ROWS ONLY;"), "{oracle}");
        assert!(!oracle.contains("LIMIT"), "{oracle}");
    }

    #[test]
    fn lookup_rejects_value_count_mismatch() {
        let fk = ForeignKeyInfo {
            name: "fk_pair".into(),
            columns: vec!["a".into(), "b".into()],
            referenced_table: "parent".into(),
            referenced_columns: vec!["x".into(), "y".into()],
            on_delete: None,
            on_update: None,
        };
        assert!(foreign_key_lookup_sql(DbKind::Postgres, &target().ns, &fk, &[]).is_err());
    }

    #[test]
    fn column_lookup_finds_member_of_composite_key() {
        let fk = ForeignKeyInfo {
            name: "fk_pair".into(),
            columns: vec!["a".into(), "b".into()],
            referenced_table: "parent".into(),
            referenced_columns: vec!["x".into(), "y".into()],
            on_delete: None,
            on_update: None,
        };
        let fks = vec![fk];
        assert!(foreign_key_for_column(&fks, "A").is_some());
        assert!(foreign_key_for_column(&fks, "missing").is_none());
    }
}
