//! F-62 SQL 参数的方言接线：`DbKind` → `Flavor`，避免 sql-lang 依赖 db-api。
//!
//! 这里只做映射与薄封装；占位符扫描 / 渲染的规则都在 `rustdb_sql_lang::params`。

use rustdb_db_api::DbKind;
use rustdb_sql_lang::params;
use rustdb_sql_lang::{Flavor, ParamRef, ParamValue, PlaceholderPolicy, RenderedSql};
use std::collections::BTreeMap;

pub fn flavor_for(kind: DbKind) -> Flavor {
    match kind {
        DbKind::Mysql | DbKind::Mariadb => Flavor::MySql,
        DbKind::Postgres => Flavor::Postgres,
        DbKind::Sqlite => Flavor::Sqlite,
        DbKind::Mssql => Flavor::MsSql,
        DbKind::Oracle => Flavor::Oracle,
    }
}

/// 占位符开关：`@name` 在 MySQL / T-SQL 是原生语法，默认不当参数。
pub fn policy_for(kind: DbKind) -> PlaceholderPolicy {
    PlaceholderPolicy::for_flavor(flavor_for(kind))
}

pub fn scan(sql: &str, kind: DbKind) -> Vec<ParamRef> {
    params::scan(sql, policy_for(kind))
}

pub fn render(sql: &str, values: &BTreeMap<String, ParamValue>, kind: DbKind) -> RenderedSql {
    params::render(sql, values, policy_for(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_named_is_native_for_mysql_and_mssql() {
        assert!(!policy_for(DbKind::Mysql).at_named);
        assert!(!policy_for(DbKind::Mariadb).at_named);
        assert!(!policy_for(DbKind::Mssql).at_named);
        assert!(policy_for(DbKind::Postgres).at_named);
        assert!(policy_for(DbKind::Sqlite).at_named);

        let refs = scan("SELECT * FROM t WHERE id = @id", DbKind::Mssql);
        assert!(refs.is_empty(), "T-SQL 变量不应被当成参数");

        let refs = scan("SELECT * FROM t WHERE id = @id", DbKind::Postgres);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "id");
    }

    #[test]
    fn render_uses_dialect_policy() {
        let mut values = BTreeMap::new();
        values.insert("id".to_string(), ParamValue::Number("7".into()));
        let rendered = render(
            "SELECT * FROM t WHERE id = :id AND name = ':id'",
            &values,
            DbKind::Sqlite,
        );
        assert_eq!(
            rendered.sql,
            "SELECT * FROM t WHERE id = 7 AND name = ':id'"
        );
        assert!(rendered.is_complete());
    }
}
