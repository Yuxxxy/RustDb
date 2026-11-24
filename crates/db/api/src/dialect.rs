use crate::driver::DbKind;
use crate::metadata::ObjectPath;

/// 方言策略对象：**纯函数、不做 IO**，因此可用 golden test 全覆盖。
pub trait Dialect: Send + Sync {
    fn kind(&self) -> DbKind;

    fn quote_ident(&self, ident: &str) -> String;

    fn quote_qualified(&self, path: &ObjectPath) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = &path.ns.catalog {
            parts.push(self.quote_ident(c));
        }
        if let Some(s) = &path.ns.schema {
            parts.push(self.quote_ident(s));
        }
        parts.push(self.quote_ident(&path.name));
        parts.join(".")
    }

    fn placeholder(&self, index: usize) -> String;

    fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
        format!("{sql} LIMIT {limit} OFFSET {offset}")
    }

    /// 单列排序片段。默认使用标准 `NULLS FIRST/LAST`；MySQL / SQL Server
    /// 不支持该语法，驱动需覆盖为 `col IS NULL` 形式。
    fn order_clause(&self, column: &str, descending: bool, nulls_last: bool) -> String {
        format!(
            "{} {}{}",
            self.quote_ident(column),
            if descending { "DESC" } else { "ASC" },
            if nulls_last {
                " NULLS LAST"
            } else {
                " NULLS FIRST"
            }
        )
    }

    fn limit_only(&self, sql: &str, limit: u64) -> String {
        format!("{sql} LIMIT {limit}")
    }

    fn explain_sql(&self, sql: &str, analyze: bool) -> String {
        let _ = analyze;
        format!("EXPLAIN {sql}")
    }
}

pub fn quote_double(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

pub fn quote_backtick(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

pub fn quote_bracket(ident: &str) -> String {
    format!("[{}]", ident.replace(']', "]]"))
}

/// 单引号字符串字面量（用于无法参数绑定的位置，例如 DDL 默认值）。
pub fn quote_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
