//! 服务端分页契约（X-02，M7）。
//!
//! * 页码 **1 起**，`page_size` 上限 [`MAX_PAGE_SIZE`]；
//! * 排序默认 **NULL 排最后**（升/降序一致，与结果网格客户端排序一致）；
//! * 驱动必须把主键（或唯一键）追加为排序兜底，避免跨页漏行/重行（R-24）；
//! * 过滤条件按 **AND** 组合；`Eq`/`Ne` 的 `Value::Null` 翻译为 `IS NULL` / `IS NOT NULL`；
//! * 本模块只做纯 SQL 构建，方言差异（标识符引用、占位符、LIMIT/OFFSET）交给 [`Dialect`]。

use crate::dialect::Dialect;
use crate::error::DriverError;
use crate::metadata::ObjectPath;
use crate::session::ColumnMeta;
use crate::value::{Row, Value};
use std::sync::Arc;

/// 单页最多返回的行数（防止一次拉爆内存）。
pub const MAX_PAGE_SIZE: u32 = 5_000;
/// 未指定时的默认页大小。
pub const DEFAULT_PAGE_SIZE: u32 = 200;

/// 一页的请求。
#[derive(Debug, Clone, PartialEq)]
pub struct PageRequest {
    pub path: ObjectPath,
    /// 1 起的页码。
    pub page: u64,
    pub page_size: u32,
    pub order_by: Vec<PageOrder>,
    /// 多个条件按 AND 组合。
    pub filters: Vec<PageFilter>,
}

impl PageRequest {
    pub fn new(path: ObjectPath) -> Self {
        Self {
            path,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            order_by: Vec::new(),
            filters: Vec::new(),
        }
    }

    pub fn offset(&self) -> u64 {
        self.page.saturating_sub(1) * self.page_size as u64
    }

    pub fn validate(&self) -> Result<(), DriverError> {
        if self.page == 0 {
            return Err(invalid("页码从 1 开始"));
        }
        if self.page_size == 0 || self.page_size > MAX_PAGE_SIZE {
            return Err(invalid(format!(
                "页大小必须在 1~{MAX_PAGE_SIZE} 之间，收到 {}",
                self.page_size
            )));
        }
        for order in &self.order_by {
            if order.column.trim().is_empty() {
                return Err(invalid("排序列不能为空"));
            }
        }
        for filter in &self.filters {
            if filter.column.trim().is_empty() {
                return Err(invalid("过滤列不能为空"));
            }
            if filter.value.is_null() && !matches!(filter.op, PageOp::Eq | PageOp::Ne) {
                return Err(invalid("NULL 只能用于 = 或 <>（IS NULL / IS NOT NULL）"));
            }
            if matches!(filter.op, PageOp::Like | PageOp::NotLike) && filter.value.is_null() {
                return Err(invalid("LIKE 的条件值不能是 NULL"));
            }
        }
        Ok(())
    }
}

/// 排序列。`nulls_last = false` 时显式排最前，不依赖方言默认。
#[derive(Debug, Clone, PartialEq)]
pub struct PageOrder {
    pub column: String,
    pub descending: bool,
    pub nulls_last: bool,
}

impl PageOrder {
    pub fn asc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            descending: false,
            nulls_last: true,
        }
    }

    pub fn desc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            descending: true,
            nulls_last: true,
        }
    }

    pub fn nulls_first(mut self) -> Self {
        self.nulls_last = false;
        self
    }
}

/// 单列过滤条件。
#[derive(Debug, Clone, PartialEq)]
pub struct PageFilter {
    pub column: String,
    pub op: PageOp,
    pub value: Value,
}

impl PageFilter {
    pub fn eq(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: PageOp::Eq,
            value,
        }
    }

    pub fn like(column: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: PageOp::Like,
            value: Value::Text(pattern.into().into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Like,
    NotLike,
}

impl PageOp {
    fn sql(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Like => "LIKE",
            Self::NotLike => "NOT LIKE",
        }
    }
}

/// 一页的结果。
#[derive(Debug, Clone)]
pub struct PageResult {
    /// 表的完整列定义（不是只选中的列）。
    pub columns: Arc<[ColumnMeta]>,
    /// 已截断到 `page_size` 的行。
    pub rows: Vec<Row>,
    pub page: u64,
    pub page_size: u32,
    pub offset: u64,
    /// 多取一行探测：`true` 表示还有下一页。
    pub has_more: bool,
}

/// 构建后的 SQL 与按占位符顺序排列的参数。
#[derive(Debug, Clone, PartialEq)]
pub struct PageSql {
    pub sql: String,
    pub params: Vec<Value>,
}

/// 构建分页 SELECT。`tiebreak` 为驱动从元数据取到的主键列（按主键顺序）。
pub fn build_page_sql(
    dialect: &dyn Dialect,
    req: &PageRequest,
    tiebreak: &[String],
) -> Result<PageSql, DriverError> {
    build_page_sql_with(dialect, req, tiebreak, |value, index| {
        Ok((dialect.placeholder(index), Some(value.clone())))
    })
}

/// 同 [`build_page_sql`]，但把过滤值直接渲染成 SQL 字面量。
///
/// 用于没有实现参数绑定的驱动（core 的兜底分页）：`literal` 必须按目标方言
/// 安全转义，调用方负责保证它不会把用户输入当作 SQL 片段执行。
pub fn build_page_sql_literal<F>(
    dialect: &dyn Dialect,
    req: &PageRequest,
    tiebreak: &[String],
    mut literal: F,
) -> Result<String, DriverError>
where
    F: FnMut(&Value) -> Result<String, DriverError>,
{
    let built = build_page_sql_with(dialect, req, tiebreak, |value, _index| {
        Ok((literal(value)?, None))
    })?;
    Ok(built.sql)
}

fn build_page_sql_with<F>(
    dialect: &dyn Dialect,
    req: &PageRequest,
    tiebreak: &[String],
    mut render_value: F,
) -> Result<PageSql, DriverError>
where
    F: FnMut(&Value, usize) -> Result<(String, Option<Value>), DriverError>,
{
    req.validate()?;
    let mut sql = format!("SELECT * FROM {}", dialect.quote_qualified(&req.path));
    let mut params = Vec::new();

    if !req.filters.is_empty() {
        let mut clauses = Vec::with_capacity(req.filters.len());
        for filter in &req.filters {
            clauses.push(filter_clause(
                dialect,
                filter,
                &mut params,
                &mut render_value,
            )?);
        }
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }

    let mut order: Vec<(&str, bool, bool)> =
        Vec::with_capacity(req.order_by.len() + tiebreak.len());
    let mut seen: Vec<String> = Vec::new();
    for key in &req.order_by {
        // 按原样比较：大小写敏感的方言里 `"ID"` 与 `"id"` 是不同列。
        if seen.contains(&key.column) {
            continue;
        }
        seen.push(key.column.clone());
        order.push((key.column.as_str(), key.descending, key.nulls_last));
    }
    for column in tiebreak {
        if seen.contains(column) {
            continue;
        }
        seen.push(column.clone());
        order.push((column.as_str(), false, true));
    }
    if !order.is_empty() {
        let rendered: Vec<String> = order
            .iter()
            .map(|(column, descending, nulls_last)| {
                dialect.order_clause(column, *descending, *nulls_last)
            })
            .collect();
        sql.push_str(" ORDER BY ");
        sql.push_str(&rendered.join(", "));
    }

    // 多取一行判断是否还有下一页，避免额外 COUNT。
    sql = dialect.paginate(&sql, req.page_size as u64 + 1, req.offset());
    Ok(PageSql { sql, params })
}

fn filter_clause<F>(
    dialect: &dyn Dialect,
    filter: &PageFilter,
    params: &mut Vec<Value>,
    render_value: &mut F,
) -> Result<String, DriverError>
where
    F: FnMut(&Value, usize) -> Result<(String, Option<Value>), DriverError>,
{
    let column = dialect.quote_ident(&filter.column);
    if filter.value.is_null() {
        return Ok(match filter.op {
            PageOp::Eq => format!("{column} IS NULL"),
            PageOp::Ne => format!("{column} IS NOT NULL"),
            _ => return Err(invalid("NULL 只能用于 = 或 <>")),
        });
    }
    let (rendered, param) = render_value(&filter.value, params.len() + 1)?;
    if let Some(param) = param {
        params.push(param);
    }
    Ok(format!("{column} {} {rendered}", filter.op.sql()))
}

fn invalid(message: impl Into<String>) -> DriverError {
    DriverError::internal(format!("分页参数无效：{}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{Namespace, ObjectKind};
    use crate::value::Value;

    struct TestDialect;
    impl Dialect for TestDialect {
        fn kind(&self) -> crate::DbKind {
            crate::DbKind::Sqlite
        }
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn placeholder(&self, _index: usize) -> String {
            "?".into()
        }
        fn paginate(&self, sql: &str, limit: u64, offset: u64) -> String {
            format!("{sql} LIMIT {limit} OFFSET {offset}")
        }
    }

    fn path() -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("main"), None::<&str>),
            "orders",
            ObjectKind::Table,
        )
    }

    #[test]
    fn sql_has_filters_tiebreak_and_nulls_last() {
        let mut req = PageRequest::new(path());
        req.page = 3;
        req.page_size = 20;
        req.order_by = vec![PageOrder::desc("total")];
        req.filters = vec![
            PageFilter::eq("status", Value::Text("paid".into())),
            PageFilter {
                column: "channel_id".into(),
                op: PageOp::Gt,
                value: Value::Int(1),
            },
        ];
        let built = build_page_sql(&TestDialect, &req, &["id".into()]).expect("应构建成功");
        assert_eq!(
            built.sql,
            "SELECT * FROM \"main\".\"orders\" WHERE \"status\" = ? AND \"channel_id\" > ? \
             ORDER BY \"total\" DESC NULLS LAST, \"id\" ASC NULLS LAST LIMIT 21 OFFSET 40"
        );
        assert_eq!(built.params.len(), 2);
        assert_eq!(req.offset(), 40);
    }

    #[test]
    fn null_filters_use_is_null_and_like_rejects_null() {
        let mut req = PageRequest::new(path());
        req.filters = vec![PageFilter::eq("status", Value::Null)];
        let built = build_page_sql(&TestDialect, &req, &[]).expect("应构建成功");
        assert!(built.sql.contains("\"status\" IS NULL"));
        assert!(built.params.is_empty());

        let mut bad = PageRequest::new(path());
        bad.filters = vec![PageFilter {
            column: "name".into(),
            op: PageOp::Like,
            value: Value::Null,
        }];
        assert!(build_page_sql(&TestDialect, &bad, &[]).is_err());

        bad.filters = vec![PageFilter {
            column: "total".into(),
            op: PageOp::Gt,
            value: Value::Null,
        }];
        assert!(
            build_page_sql(&TestDialect, &bad, &[]).is_err(),
            "NULL 不能用于大小比较"
        );
    }

    #[test]
    fn tiebreak_is_appended_once_and_page_bounds_are_checked() {
        let mut req = PageRequest::new(path());
        req.order_by = vec![PageOrder::asc("id")];
        let built = build_page_sql(&TestDialect, &req, &["id".into()]).expect("应构建成功");
        assert_eq!(
            built.sql.matches("\"id\"").count(),
            1,
            "主键已在排序列里就不重复"
        );

        let mut bad = PageRequest::new(path());
        bad.page = 0;
        assert!(build_page_sql(&TestDialect, &bad, &[]).is_err());

        bad.page = 1;
        bad.page_size = MAX_PAGE_SIZE + 1;
        assert!(build_page_sql(&TestDialect, &bad, &[]).is_err());
    }

    #[test]
    fn literal_page_sql_renders_filters_without_placeholders() {
        let mut req = PageRequest::new(path());
        req.page_size = 10;
        req.filters = vec![PageFilter::eq("status", Value::Text("paid".into()))];
        let sql = build_page_sql_literal(&TestDialect, &req, &["id".into()], |value| {
            Ok::<_, DriverError>(format!("'{}'", value.export_text()))
        })
        .expect("应构建成功");
        assert!(sql.contains("\"status\" = 'paid'"), "{sql}");
        assert!(sql.contains("ORDER BY \"id\" ASC NULLS LAST"), "{sql}");
        assert!(!sql.contains('?'), "{sql}");
    }

    #[test]
    fn explicit_nulls_first_and_stable_tiebreak() {
        let mut req = PageRequest::new(path());
        req.order_by = vec![PageOrder::asc("total").nulls_first()];
        req.page_size = 10;
        let built = build_page_sql(&TestDialect, &req, &["id".into()]).expect("应构建成功");
        assert!(built.sql.contains("\"total\" ASC NULLS FIRST"));
        assert!(built.sql.contains("\"id\" ASC NULLS LAST"));
        assert!(built.sql.ends_with("LIMIT 11 OFFSET 0"));
    }
}
