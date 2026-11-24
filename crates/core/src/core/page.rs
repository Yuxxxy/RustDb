//! 服务端分页的驱动兜底。
//!
//! 只依赖 `db-api` 的分页 SQL 生成器与会话接口，不碰 `Core` 的其他状态；
//! 从 `core.rs` 拆出后便于单独加各驱动方言的分页回归用例。

use super::*;

impl Core {
    /// 没有实现服务端分页的驱动兜底：用方言分页 SQL + 字面量过滤执行一页。
    ///
    /// 这一层让 dump / transfer / data_compare 不再被“只有 SQLite 实现了
    /// fetch_page”卡死；它仍然只取 `page_size + 1` 行，不物化整表。
    pub(super) async fn fetch_page_fallback(
        &self,
        session: Arc<dyn Session>,
        kind: DbKind,
        req: &PageRequest,
    ) -> Result<PageResult, DriverError> {
        let columns = session.metadata().columns(&req.path).await?;
        let mut tiebreak: Vec<String> = columns
            .iter()
            .filter(|column| column.is_primary_key)
            .map(|column| column.meta.name.to_string())
            .collect();
        // 部分驱动的 columns() 不回填主键位（如 PostgreSQL），从表详情的
        // 主键索引再补一次。
        if tiebreak.is_empty() {
            if let Ok(detail) = session.metadata().table(&req.path).await {
                for index in detail.indexes.iter().filter(|index| index.primary) {
                    for column in &index.columns {
                        if !tiebreak.contains(column) {
                            tiebreak.push(column.clone());
                        }
                    }
                }
            }
        }
        // 无主键表没有唯一兜底；退化为全列排序，至少保证同一查询内顺序稳定。
        if tiebreak.is_empty() {
            tiebreak = columns
                .iter()
                .map(|column| column.meta.name.to_string())
                .collect();
        }
        let sql = rustdb_db_api::build_page_sql_literal(
            session.dialect().as_ref(),
            req,
            &tiebreak,
            |value| crate::edit::literal(kind, value).map_err(DriverError::internal),
        )?;
        let request = ExecuteRequest {
            sql,
            params: Vec::new(),
            fetch: FetchOptions {
                max_rows: req.page_size as u64 + 1,
                page_size: req.page_size,
                timeout_ms: Some(30_000),
                first_screen: req.page_size,
            },
            tx: None,
            read_only: true,
            request_id: None,
            query_id: Some(QueryId::new()),
        };
        let handle = session.execute(request).await?;
        let mut rows: Vec<Row> = Vec::new();
        let mut stream = handle.stream;
        while let Some(item) = stream.next().await {
            rows.extend(item?.rows);
        }
        let has_more = rows.len() > req.page_size as usize;
        rows.truncate(req.page_size as usize);
        let columns: Arc<[rustdb_db_api::ColumnMeta]> = columns
            .into_iter()
            .map(|column| column.meta)
            .collect::<Vec<_>>()
            .into();
        Ok(PageResult {
            columns,
            rows,
            page: req.page,
            page_size: req.page_size,
            offset: req.offset(),
            has_more,
        })
    }
}
