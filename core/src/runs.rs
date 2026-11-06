//! 运行记录（F-65）：每次执行都是一条独立记录，**不覆盖旧结果**。
//!
//! 约束（需求文档 19.2）：
//! * 固定（pinned）项不参与 LRU 淘汰；
//! * 关闭运行记录只释放引用，数据仍被其它标签使用时不删除 —— 释放动作由调用方
//!   结合 [`crate::results::ResultStore::remove_if_unreferenced`] 决定；
//! * 记录条数有上限，超限按「最旧的未固定」淘汰，并把被淘汰的记录返回给调用方。

use crate::results::ResultSetId;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "执行中",
            Self::Completed => "完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: u64,
    pub profile_id: String,
    /// 执行文本摘要（截断），完整 SQL 在标签页 / 历史里。
    pub sql_preview: String,
    pub statement_kind: String,
    pub started_ms: i64,
    pub elapsed_ms: Option<u64>,
    pub state: RunState,
    /// 本条运行产出的结果集（多语句时多条）。
    pub result_sets: Vec<ResultSetId>,
    pub pinned: bool,
    pub rows: u64,
    pub affected: Option<u64>,
}

impl RunRecord {
    pub fn is_running(&self) -> bool {
        self.state == RunState::Running
    }

    /// 结果区标签文案：`#12 · 3 行 · 12 ms`。
    pub fn tab_label(&self) -> String {
        let mut label = format!("#{}", self.id);
        match self.state {
            RunState::Running => label.push_str(" · 执行中"),
            RunState::Failed => label.push_str(" · 失败"),
            RunState::Cancelled => label.push_str(" · 已取消"),
            RunState::Completed => {
                if let Some(affected) = self.affected {
                    label.push_str(&format!(" · {affected} 行受影响"));
                } else {
                    label.push_str(&format!(" · {} 行", self.rows));
                }
                if let Some(ms) = self.elapsed_ms {
                    label.push_str(&format!(" · {ms} ms"));
                }
            }
        }
        if self.pinned {
            label.push('・');
            label.push_str("已固定");
        }
        label
    }
}

pub struct RunLog {
    runs: RwLock<VecDeque<RunRecord>>,
    limit: std::sync::atomic::AtomicUsize,
    next_id: AtomicU64,
}

impl RunLog {
    pub const DEFAULT_LIMIT: usize = 50;

    pub fn new(limit: usize) -> Self {
        Self {
            runs: RwLock::new(VecDeque::new()),
            limit: std::sync::atomic::AtomicUsize::new(limit.max(1)),
            next_id: AtomicU64::new(1),
        }
    }

    /// 执行提交时登记（此时还没有结果与耗时）。
    pub fn begin(&self, profile_id: &str, sql_preview: &str, statement_kind: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.runs.write().push_front(RunRecord {
            id,
            profile_id: profile_id.to_string(),
            sql_preview: sql_preview.chars().take(200).collect(),
            statement_kind: statement_kind.to_string(),
            started_ms: now_ms(),
            elapsed_ms: None,
            state: RunState::Running,
            result_sets: Vec::new(),
            pinned: false,
            rows: 0,
            affected: None,
        });
        id
    }

    /// 结果集先到（`RowsAppended` 里带 `set_id`），登记到运行记录上。
    pub fn attach_result(&self, id: u64, set: ResultSetId) -> bool {
        let mut runs = self.runs.write();
        let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        if !run.result_sets.contains(&set) {
            run.result_sets.push(set);
        }
        true
    }

    pub fn finish(
        &self,
        id: u64,
        state: RunState,
        elapsed_ms: u64,
        rows: u64,
        affected: Option<u64>,
    ) -> bool {
        let mut runs = self.runs.write();
        let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        run.state = state;
        run.elapsed_ms = Some(elapsed_ms);
        run.rows = rows;
        run.affected = affected;
        true
    }

    pub fn toggle_pin(&self, id: u64) -> Option<bool> {
        let mut runs = self.runs.write();
        let run = runs.iter_mut().find(|r| r.id == id)?;
        run.pinned = !run.pinned;
        Some(run.pinned)
    }

    /// 关闭一条记录（固定项也允许显式关闭），返回被移除的记录。
    pub fn remove(&self, id: u64) -> Option<RunRecord> {
        let mut runs = self.runs.write();
        let pos = runs.iter().position(|r| r.id == id)?;
        runs.remove(pos)
    }

    /// 新 → 旧。
    pub fn list(&self) -> Vec<RunRecord> {
        self.runs.read().iter().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<RunRecord> {
        self.runs.read().iter().find(|r| r.id == id).cloned()
    }

    pub fn len(&self) -> usize {
        self.runs.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::Relaxed)
    }

    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Relaxed);
    }

    /// 所有固定记录引用的结果集（ResultStore 淘汰时必须跳过）。
    pub fn pinned_sets(&self) -> Vec<ResultSetId> {
        self.runs
            .read()
            .iter()
            .filter(|r| r.pinned)
            .flat_map(|r| r.result_sets.iter().copied())
            .collect()
    }

    /// 超限淘汰：返回被移除的**未固定**记录（新→旧之外的最旧优先）。
    pub fn trim(&self) -> Vec<RunRecord> {
        let mut runs = self.runs.write();
        let limit = self.limit.load(Ordering::Relaxed);
        let mut evicted = Vec::new();
        while runs.len() > limit {
            // VecDeque 是「新在前」，所以从尾部找最旧的未固定项。
            let pos = runs.iter().rposition(|r| !r.pinned);
            let Some(pos) = pos else { break };
            if let Some(run) = runs.remove(pos) {
                evicted.push(run);
            }
        }
        evicted
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(id: u64) -> ResultSetId {
        ResultSetId(id)
    }

    #[test]
    fn runs_are_independent_and_newest_first() {
        let log = RunLog::new(10);
        let a = log.begin("p", "select 1", "Select");
        let b = log.begin("p", "select 2", "Select");
        log.finish(a, RunState::Completed, 5, 3, None);
        log.finish(b, RunState::Completed, 8, 1, Some(1));

        let list = log.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, b, "最新的运行在最前");
        assert_eq!(list[0].tab_label(), "#2 · 1 行受影响 · 8 ms");
        assert_eq!(list[1].tab_label(), "#1 · 3 行 · 5 ms");
    }

    #[test]
    fn pinned_survives_trim_and_reports_its_sets() {
        let log = RunLog::new(2);
        let keep = log.begin("p", "select keep", "Select");
        log.attach_result(keep, set(7));
        assert_eq!(log.toggle_pin(keep), Some(true));

        let a = log.begin("p", "select a", "Select");
        let b = log.begin("p", "select b", "Select");
        let evicted = log.trim();
        assert_eq!(log.len(), 2);
        assert!(!evicted.is_empty());
        assert!(log.list().iter().any(|r| r.id == keep), "固定项不能被淘汰");
        assert!(log.list().iter().any(|r| r.id == b));
        assert!(!log.list().iter().any(|r| r.id == a));
        assert_eq!(log.pinned_sets(), vec![set(7)]);
    }

    #[test]
    fn attach_and_finish_unknown_run_is_rejected() {
        let log = RunLog::new(5);
        assert!(!log.attach_result(99, set(1)));
        assert!(!log.finish(99, RunState::Failed, 1, 0, None));
        assert!(log.toggle_pin(99).is_none());
        assert!(log.remove(99).is_none());
    }

    #[test]
    fn explicit_close_returns_the_record() {
        let log = RunLog::new(5);
        let id = log.begin("p", "delete from t", "Delete");
        log.attach_result(id, set(3));
        log.finish(id, RunState::Failed, 2, 0, None);
        let closed = log.remove(id).expect("应能关闭");
        assert_eq!(closed.result_sets, vec![set(3)]);
        assert_eq!(closed.state, RunState::Failed);
        assert!(log.is_empty());
    }

    #[test]
    fn all_pinned_never_evicts() {
        let log = RunLog::new(1);
        let a = log.begin("p", "a", "Select");
        let b = log.begin("p", "b", "Select");
        log.toggle_pin(a);
        log.toggle_pin(b);
        assert!(log.trim().is_empty());
        assert_eq!(log.len(), 2);
    }
}
