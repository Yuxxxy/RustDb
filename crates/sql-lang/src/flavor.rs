//! 方言归类：只描述**词法/脚本层**的差异（不涉及连接与类型系统）。
//!
//! `db-api::DbKind` → `Flavor` 的映射放在 core，避免 sql-lang 依赖任何 crate。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// 不区分方言（默认按标准 SQL 处理）。
    Generic,
    Sqlite,
    Postgres,
    MySql,
    MsSql,
    Oracle,
}

impl Flavor {
    /// `@name` 在该方言里是原生语法（T-SQL 变量 / MySQL 用户变量），不能当占位符。
    pub fn at_sign_is_native(self) -> bool {
        matches!(self, Self::MySql | Self::MsSql)
    }

    /// `$tag$ … $tag$` 是否可用（只有 PostgreSQL 系）。
    pub fn supports_dollar_quote(self) -> bool {
        matches!(self, Self::Postgres)
    }

    /// `#` 是否行注释（MySQL 系）。
    pub fn hash_line_comment(self) -> bool {
        matches!(self, Self::MySql)
    }

    /// 是否支持 `GO` 批分隔符（T-SQL）。
    pub fn supports_go_batch(self) -> bool {
        matches!(self, Self::MsSql)
    }

    /// 是否支持客户端 `DELIMITER` 指令（MySQL 系命令行习惯）。
    pub fn supports_delimiter_directive(self) -> bool {
        matches!(self, Self::MySql)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MsSql => "mssql",
            Self::Oracle => "oracle",
        }
    }
}

impl Default for Flavor {
    fn default() -> Self {
        Self::Generic
    }
}
