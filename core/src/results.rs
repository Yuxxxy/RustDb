//! 结果集存储：行数据只在这里，事件只携带 `ResultSetId`。
//!
//! M0 实现分页 + 行数上限；LRU 淘汰与磁盘溢写在 M2 补齐。

use parking_lot::RwLock;
use rustdb_db_api::{ColumnMeta, QueryId, Row, RowBatch};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

pub const PAGE_ROWS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResultSetId(pub u64);

impl fmt::Display for ResultSetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rs_{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultState {
    Streaming,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ResultMeta {
    pub id: ResultSetId,
    pub query_id: QueryId,
    pub columns: Arc<[ColumnMeta]>,
    pub loaded_rows: u64,
    pub total_hint: Option<u64>,
    pub state: ResultState,
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub affected: Option<u64>,
    pub truncated: bool,
    /// F-65：固定的结果不参与 LRU 淘汰。
    pub pinned: bool,
}

#[derive(Debug)]
pub struct Page {
    pub rows: Vec<Row>,
    pub bytes: usize,
}

#[derive(Debug)]
pub struct ResultSet {
    meta: RwLock<ResultMeta>,
    pages: RwLock<BTreeMap<usize, Arc<Page>>>,
}

impl ResultSet {
    pub fn meta(&self) -> ResultMeta {
        self.meta.read().clone()
    }
}

pub struct ResultStore {
    sets: RwLock<HashMap<ResultSetId, Arc<ResultSet>>>,
    next_id: AtomicU64,
    bytes: AtomicUsize,
    budget_bytes: usize,
}

impl ResultStore {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            sets: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            bytes: AtomicUsize::new(0),
            budget_bytes,
        }
    }

    pub fn create(
        &self,
        columns: Arc<[ColumnMeta]>,
        query_id: QueryId,
    ) -> (ResultSetId, Arc<ResultSet>) {
        let id = ResultSetId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let set = Arc::new(ResultSet {
            meta: RwLock::new(ResultMeta {
                id,
                query_id,
                columns,
                loaded_rows: 0,
                total_hint: None,
                state: ResultState::Streaming,
                elapsed_ms: 0,
                bytes: 0,
                affected: None,
                truncated: false,
                pinned: false,
            }),
            pages: RwLock::new(BTreeMap::new()),
        });
        self.sets.write().insert(id, set.clone());
        (id, set)
    }

    /// 追加一批行；返回（累计行数、累计字节）。
    pub fn append(&self, id: ResultSetId, batch: RowBatch) -> Option<(u64, usize)> {
        let set = self.sets.read().get(&id).cloned()?;
        let bytes = batch.approx_bytes;
        {
            let mut pages = set.pages.write();
            let start_page = {
                let meta = set.meta.read();
                meta.loaded_rows as usize / PAGE_ROWS
            };
            let mut page_no = start_page;
            let mut current: Vec<Row> = pages
                .get(&page_no)
                .map(|p| p.rows.clone())
                .unwrap_or_default();
            for row in batch.rows {
                current.push(row);
                if current.len() >= PAGE_ROWS {
                    let page = Arc::new(Page {
                        bytes: current.iter().flatten().map(|v| v.approx_bytes()).sum(),
                        rows: std::mem::take(&mut current),
                    });
                    pages.insert(page_no, page);
                    page_no += 1;
                }
            }
            if !current.is_empty() {
                let page = Arc::new(Page {
                    bytes: current.iter().flatten().map(|v| v.approx_bytes()).sum(),
                    rows: current,
                });
                pages.insert(page_no, page);
            }
        }
        let mut meta = set.meta.write();
        let loaded = set.pages.read().values().map(|p| p.rows.len() as u64).sum();
        meta.loaded_rows = loaded;
        meta.bytes += bytes;
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        Some((loaded, meta.bytes))
    }

    pub fn complete(&self, id: ResultSetId, elapsed_ms: u64, affected: Option<u64>) {
        if let Some(set) = self.sets.read().get(&id).cloned() {
            let mut meta = set.meta.write();
            meta.state = ResultState::Complete;
            meta.elapsed_ms = elapsed_ms;
            meta.affected = affected;
        }
    }

    pub fn fail(&self, id: ResultSetId) {
        if let Some(set) = self.sets.read().get(&id).cloned() {
            set.meta.write().state = ResultState::Failed;
        }
    }

    pub fn cancel(&self, id: ResultSetId) {
        if let Some(set) = self.sets.read().get(&id).cloned() {
            set.meta.write().state = ResultState::Cancelled;
        }
    }

    pub fn mark_truncated(&self, id: ResultSetId, total_hint: Option<u64>) {
        if let Some(set) = self.sets.read().get(&id).cloned() {
            let mut meta = set.meta.write();
            meta.truncated = true;
            meta.total_hint = total_hint;
        }
    }

    pub fn meta(&self, id: ResultSetId) -> Option<ResultMeta> {
        self.sets.read().get(&id).map(|s| s.meta())
    }

    pub fn rows(&self, id: ResultSetId, offset: usize, limit: usize) -> Vec<Row> {
        let Some(set) = self.sets.read().get(&id).cloned() else {
            return Vec::new();
        };
        let pages = set.pages.read();
        let mut out = Vec::with_capacity(limit.min(PAGE_ROWS));
        let mut index = 0usize;
        for page in pages.values() {
            if index + page.rows.len() <= offset {
                index += page.rows.len();
                continue;
            }
            for row in &page.rows {
                if index >= offset && out.len() < limit {
                    out.push(row.clone());
                }
                index += 1;
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    pub fn total_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// 单个结果集还能接收多少字节；未创建的集合按 0 处理。
    ///
    /// 口径是「单集合不得超过总预算」，避免一条没有 `LIMIT` 的查询独占全部内存；
    /// 多个会话并发时仍由 [`Self::evict_unpinned`] 做全局回收。
    pub fn set_remaining(&self, id: ResultSetId) -> usize {
        let Some(set) = self.sets.read().get(&id).cloned() else {
            return 0;
        };
        let used = set.meta.read().bytes;
        self.budget_bytes.saturating_sub(used)
    }

    /// 本次追加是否会超出预算。调用方据此**在读取前**决定停止拉流。
    pub fn would_exceed(&self, id: ResultSetId, incoming: usize) -> bool {
        self.set_remaining(id) < incoming
    }

    /// 全局预算是否已经用满（用于流式过程中触发淘汰）。
    pub fn over_budget(&self) -> bool {
        self.total_bytes() >= self.budget_bytes
    }

    pub fn set_count(&self) -> usize {
        self.sets.read().len()
    }

    // --- F-65：固定与释放 ---

    /// 固定 / 取消固定一个结果集；返回新状态。
    pub fn set_pinned(&self, id: ResultSetId, pinned: bool) -> Option<bool> {
        let set = self.sets.read().get(&id).cloned()?;
        let mut meta = set.meta.write();
        meta.pinned = pinned;
        Some(pinned)
    }

    pub fn is_pinned(&self, id: ResultSetId) -> bool {
        self.sets
            .read()
            .get(&id)
            .map(|s| s.meta.read().pinned)
            .unwrap_or(false)
    }

    /// 释放结果集并归还预算；返回释放的字节数。
    pub fn remove(&self, id: ResultSetId) -> Option<usize> {
        let set = self.sets.write().remove(&id)?;
        let bytes = set.meta.read().bytes;
        let now = self.bytes.load(Ordering::Relaxed).saturating_sub(bytes);
        self.bytes.store(now, Ordering::Relaxed);
        Some(bytes)
    }

    /// 只有当结果集没有被 `keep` 引用时才释放（关闭运行记录用）。
    pub fn remove_if_unreferenced(&self, id: ResultSetId, keep: &[ResultSetId]) -> Option<usize> {
        if keep.contains(&id) {
            return None;
        }
        self.remove(id)
    }

    /// 超出内存预算时按「最旧且未固定」淘汰；返回被释放的结果集。
    ///
    /// 正在写入（`Streaming`）的集合与 `keep` 里的集合一律跳过。
    pub fn evict_unpinned(&self, keep: &[ResultSetId]) -> Vec<ResultSetId> {
        let mut evicted = Vec::new();
        while self.total_bytes() > self.budget_bytes {
            let victim = {
                let sets = self.sets.read();
                let mut victim: Option<ResultSetId> = None;
                for (id, set) in sets.iter() {
                    let meta = set.meta.read();
                    if meta.pinned || meta.state == ResultState::Streaming || keep.contains(id) {
                        continue;
                    }
                    // id 单调递增 ⇒ 最小 id 就是最早创建的。
                    if victim.is_none_or(|v: ResultSetId| id.0 < v.0) {
                        victim = Some(*id);
                    }
                }
                victim
            };
            match victim {
                Some(id) => {
                    if self.remove(id).is_none() {
                        break;
                    }
                    evicted.push(id);
                }
                None => break,
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{LogicalType, Value};

    fn columns() -> Arc<[ColumnMeta]> {
        vec![ColumnMeta::new(
            "payload",
            LogicalType::Text { max_len: None },
        )]
        .into()
    }

    fn batch(len: usize) -> RowBatch {
        RowBatch::new(vec![vec![Value::Text("x".repeat(len).into())]])
    }

    #[test]
    fn streaming_stops_before_exceeding_budget() {
        let store = ResultStore::new(1024);
        let (id, _) = store.create(columns(), QueryId("q1".into()));

        let first = batch(400);
        assert!(!store.would_exceed(id, first.approx_bytes));
        store.append(id, first);

        // 再追加 700 字节会超过 1024：调用方必须先看到 false。
        assert!(store.would_exceed(id, 700));
        assert!(!store.would_exceed(id, 600));
        assert_eq!(store.set_remaining(id), 1024 - 400);
    }

    #[test]
    fn completed_sets_are_evicted_to_reclaim_budget() {
        let store = ResultStore::new(1024);
        let (old, _) = store.create(columns(), QueryId("q1".into()));
        store.append(old, batch(800));
        store.complete(old, 1, None);
        assert!(!store.over_budget());

        let (new, _) = store.create(columns(), QueryId("q2".into()));
        store.append(new, batch(800));
        assert!(store.over_budget());

        // 新集合在写入中，旧集合可淘汰；淘汰后旧集合不应再可读。
        let evicted = store.evict_unpinned(&[]);
        assert_eq!(evicted, vec![old]);
        assert!(store.meta(old).is_none());
        assert!(!store.over_budget());
        assert!(store.meta(new).is_some());
    }

    #[test]
    fn pinned_or_streaming_sets_are_never_evicted() {
        let store = ResultStore::new(512);
        let (kept, _) = store.create(columns(), QueryId("q1".into()));
        store.append(kept, batch(400));
        store.complete(kept, 1, None);
        store.set_pinned(kept, true);

        let (streaming, _) = store.create(columns(), QueryId("q2".into()));
        store.append(streaming, batch(400));
        assert!(store.over_budget());

        assert!(store.evict_unpinned(&[]).is_empty());
        assert!(store.meta(kept).is_some());
        assert!(store.meta(streaming).is_some());
    }
}
