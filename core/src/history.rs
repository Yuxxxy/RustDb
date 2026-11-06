//! 查询历史：按数据源分桶、可搜索、可固定（F-31 / F-68）。
//!
//! 只保存 SQL 文本与执行摘要（耗时 / 影响行数 / 成败 / 来源），**不保存结果数据**。
//! 容量按总条数封顶，超限时优先淘汰「未固定且最旧」的一条。
//!
//! v1.1（F-68）增补：来源枚举（manual / ai / mcp / cli / file）、影响行数、
//! 日期范围 + 来源 + 固定项筛选、可配置保留条数（调小立即清理）。
//! F-68 持久化：`with_path` 绑定 JSONL 文件，执行结束 / 固定 / 删除 / 清空 /
//! 改上限时写全量快照（先写临时文件再替换）；未完成的执行不落盘。

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 默认保留条数（需求 F-31 / F-68）。
pub const DEFAULT_HISTORY_LIMIT: usize = 1000;

/// 设置页可选的保留条数；`0` 表示不限制。
pub const HISTORY_LIMIT_CHOICES: [usize; 5] = [200, 1000, 5000, 10000, 0];

/// 历史来源（F-68）：区分是谁发起的执行，便于审计与筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    #[default]
    Manual,
    Ai,
    Mcp,
    Cli,
    /// 执行 `.sql` 文件产生的语句。
    File,
}

impl HistorySource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "手动",
            Self::Ai => "AI",
            Self::Mcp => "MCP",
            Self::Cli => "CLI",
            Self::File => "文件",
        }
    }

    pub const ALL: [HistorySource; 5] = [Self::Manual, Self::Ai, Self::Mcp, Self::Cli, Self::File];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u64,
    /// 数据源（连接配置）id，用于分桶。
    pub profile_id: String,
    pub sql: String,
    pub ts_ms: i64,
    /// 执行完成前为 `None`。
    pub elapsed_ms: Option<u64>,
    /// `Some(true)` 成功 / `Some(false)` 失败 / `None` 执行中。
    pub ok: Option<bool>,
    pub pinned: bool,
    #[serde(default)]
    pub source: HistorySource,
    /// DML 的影响行数（查询类为 `None`）。
    #[serde(default)]
    pub affected: Option<u64>,
}

/// 历史筛选条件（UI 的历史面板一次传全量）。
#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    pub profile_id: Option<String>,
    pub needle: Option<String>,
    /// 起始时间（含），毫秒时间戳。
    pub from_ms: Option<i64>,
    /// 结束时间（含），毫秒时间戳。
    pub to_ms: Option<i64>,
    pub source: Option<HistorySource>,
    pub pinned_only: bool,
    /// 最多返回条数（`None` = 不限）。
    pub limit: Option<usize>,
}

impl HistoryFilter {
    pub fn for_profile(profile_id: &str) -> Self {
        Self {
            profile_id: Some(profile_id.to_string()),
            ..Self::default()
        }
    }

    pub fn with_needle(mut self, needle: &str) -> Self {
        self.needle = Some(needle.to_string());
        self
    }

    fn matches(&self, entry: &HistoryEntry) -> bool {
        if let Some(profile) = &self.profile_id {
            if &entry.profile_id != profile {
                return false;
            }
        }
        if let Some(from) = self.from_ms {
            if entry.ts_ms < from {
                return false;
            }
        }
        if let Some(to) = self.to_ms {
            if entry.ts_ms > to {
                return false;
            }
        }
        if let Some(source) = self.source {
            if entry.source != source {
                return false;
            }
        }
        if self.pinned_only && !entry.pinned {
            return false;
        }
        if let Some(needle) = self.needle.as_ref().map(|n| n.trim().to_lowercase()) {
            if !needle.is_empty() && !entry.sql.to_lowercase().contains(&needle) {
                return false;
            }
        }
        true
    }
}

pub struct HistoryStore {
    entries: RwLock<VecDeque<HistoryEntry>>,
    limit: RwLock<usize>,
    next_id: AtomicU64,
    /// 落盘文件；`None` 表示纯内存（测试 / 无可用数据目录）。
    path: Option<PathBuf>,
    /// 串行化快照写入，避免并发持久化互相覆盖。
    persist_lock: Mutex<()>,
}

impl HistoryStore {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: RwLock::new(VecDeque::new()),
            limit: RwLock::new(limit),
            next_id: AtomicU64::new(1),
            path: None,
            persist_lock: Mutex::new(()),
        }
    }

    /// 绑定 JSONL 文件并载入已有历史（文件不存在视为空历史）。
    ///
    /// 损坏行跳过并记日志；未完成的执行（`ok == None`）无法恢复，载入时丢弃。
    pub fn with_path(limit: usize, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut store = Self::new(limit);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                log::warn!("读取查询历史 {} 失败：{err}", path.display());
                None
            }
        };
        if let Some(text) = text {
            let mut entries = VecDeque::new();
            let mut max_id = 0u64;
            let mut skipped = 0usize;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<HistoryEntry>(line) {
                    Ok(entry) => {
                        max_id = max_id.max(entry.id);
                        if entry.ok.is_some() {
                            entries.push_back(entry);
                        }
                    }
                    Err(err) => {
                        skipped += 1;
                        log::warn!("跳过损坏的查询历史：{err}");
                    }
                }
            }
            if skipped > 0 {
                log::info!("载入查询历史时跳过 {skipped} 条无效记录");
            }
            trim(&mut entries, limit);
            store.entries = RwLock::new(entries);
            store.next_id = AtomicU64::new(max_id + 1);
        }
        store.path = Some(path);
        store
    }

    /// 把当前快照写入磁盘（仅已完成的条目；先写临时文件再替换）。
    ///
    /// 失败只记日志：持久化不应影响查询本身。
    fn persist(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let _guard = self.persist_lock.lock();
        let snapshot = {
            let entries = self.entries.read();
            let mut text = String::new();
            for entry in entries.iter().filter(|entry| entry.ok.is_some()) {
                match serde_json::to_string(entry) {
                    Ok(line) => {
                        text.push_str(&line);
                        text.push('\n');
                    }
                    Err(err) => log::warn!("序列化查询历史失败：{err}"),
                }
            }
            text
        };
        if let Err(err) = write_atomic(path, &snapshot) {
            log::warn!("写入查询历史 {} 失败：{err}", path.display());
        }
    }

    /// 执行提交时登记一条（此时还没有耗时与成败），来源默认手动。
    pub fn begin(&self, profile_id: &str, sql: &str) -> u64 {
        self.begin_with(profile_id, sql, HistorySource::Manual)
    }

    pub fn begin_with(&self, profile_id: &str, sql: &str, source: HistorySource) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.write();
        entries.push_front(HistoryEntry {
            id,
            profile_id: profile_id.to_string(),
            sql: sql.to_string(),
            ts_ms: now_ms(),
            elapsed_ms: None,
            ok: None,
            pinned: false,
            source,
            affected: None,
        });
        let limit = *self.limit.read();
        trim(&mut entries, limit);
        id
    }

    /// 执行结束：补耗时与成败。
    pub fn finish(&self, id: u64, elapsed_ms: u64, ok: bool) {
        self.finish_with(id, elapsed_ms, ok, None);
    }

    pub fn finish_with(&self, id: u64, elapsed_ms: u64, ok: bool, affected: Option<u64>) {
        {
            let mut entries = self.entries.write();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.elapsed_ms = Some(elapsed_ms);
                entry.ok = Some(ok);
                entry.affected = affected;
            }
        }
        self.persist();
    }

    /// 列出历史：固定项优先，其余按时间倒序；可按数据源与关键字过滤。
    pub fn list(&self, profile_id: Option<&str>, needle: Option<&str>) -> Vec<HistoryEntry> {
        self.list_filtered(&HistoryFilter {
            profile_id: profile_id.map(|s| s.to_string()),
            needle: needle.map(|s| s.to_string()),
            ..HistoryFilter::default()
        })
    }

    /// 完整筛选（F-68）：日期范围 / 来源 / 固定项 / 条数。
    pub fn list_filtered(&self, filter: &HistoryFilter) -> Vec<HistoryEntry> {
        let entries = self.entries.read();
        let mut out: Vec<HistoryEntry> = entries
            .iter()
            .filter(|entry| filter.matches(entry))
            .cloned()
            .collect();
        // 稳定排序：pinned 在前，其余保持插入顺序（已是新→旧）。
        out.sort_by_key(|entry| std::cmp::Reverse(entry.pinned));
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        out
    }

    /// 切换固定状态，返回新状态；条目不存在返回 `None`。
    pub fn toggle_pin(&self, id: u64) -> Option<bool> {
        let pinned = {
            let mut entries = self.entries.write();
            let entry = entries.iter_mut().find(|e| e.id == id)?;
            entry.pinned = !entry.pinned;
            entry.pinned
        };
        self.persist();
        Some(pinned)
    }

    pub fn remove(&self, id: u64) -> bool {
        let removed = {
            let mut entries = self.entries.write();
            let before = entries.len();
            entries.retain(|entry| entry.id != id);
            entries.len() != before
        };
        if removed {
            self.persist();
        }
        removed
    }

    /// 清空（可按数据源）。
    pub fn clear(&self, profile_id: Option<&str>) {
        let changed = {
            let mut entries = self.entries.write();
            let before = entries.len();
            match profile_id {
                Some(id) => entries.retain(|entry| entry.profile_id != id),
                None => entries.clear(),
            }
            entries.len() != before
        };
        if changed {
            self.persist();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 当前保留上限；`0` 表示不限制。
    pub fn limit(&self) -> usize {
        *self.limit.read()
    }

    /// 调整保留上限：调小立即清理（下次追加时也会再清理一次）。
    pub fn set_limit(&self, limit: usize) {
        *self.limit.write() = limit;
        {
            let mut entries = self.entries.write();
            trim(&mut entries, limit);
        }
        self.persist();
    }
}

/// 先写同目录临时文件再替换，避免中断留下半截历史。
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 超限时丢掉最旧的「未固定」条目；全被固定则丢最旧一条。
///
/// `limit == 0` 表示不限制。
fn trim(entries: &mut VecDeque<HistoryEntry>, limit: usize) {
    if limit == 0 {
        return;
    }
    while entries.len() > limit {
        let position = entries
            .iter()
            .rposition(|entry| !entry.pinned)
            .unwrap_or(entries.len().saturating_sub(1));
        if position < entries.len() {
            entries.remove(position);
        } else {
            break;
        }
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

    #[test]
    fn newest_first_and_profile_buckets() {
        let store = HistoryStore::new(10);
        store.begin("a", "select 1");
        store.begin("b", "select 2");
        store.begin("a", "select 3");

        let a = store.list(Some("a"), None);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].sql, "select 3", "最新的在前");
        assert_eq!(store.list(Some("b"), None).len(), 1);
        assert_eq!(store.list(None, None).len(), 3);
    }

    #[test]
    fn search_is_case_insensitive() {
        let store = HistoryStore::new(10);
        store.begin("a", "SELECT * FROM Orders");
        store.begin("a", "select 1");
        let hits = store.list(Some("a"), Some("orders"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].sql, "SELECT * FROM Orders");
    }

    #[test]
    fn pinned_survive_trim() {
        let store = HistoryStore::new(2);
        let keep = store.begin("a", "select 'keep'");
        assert_eq!(store.toggle_pin(keep), Some(true));
        store.begin("a", "select 2");
        store.begin("a", "select 3");

        assert_eq!(store.len(), 2);
        let all = store.list(None, None);
        assert_eq!(all[0].sql, "select 'keep'", "固定项排最前且不被淘汰");
        assert!(all.iter().any(|e| e.sql == "select 3"));
    }

    #[test]
    fn finish_fills_elapsed_and_status() {
        let store = HistoryStore::new(10);
        let id = store.begin("a", "update t set x = 1");
        assert_eq!(store.list(None, None)[0].ok, None, "执行中状态未知");
        store.finish(id, 42, true);
        let entry = &store.list(None, None)[0];
        assert_eq!(entry.elapsed_ms, Some(42));
        assert_eq!(entry.ok, Some(true));
    }

    #[test]
    fn clear_by_profile_keeps_others() {
        let store = HistoryStore::new(10);
        store.begin("a", "select 1");
        store.begin("b", "select 2");
        store.clear(Some("a"));
        assert_eq!(store.list(Some("a"), None).len(), 0);
        assert_eq!(store.list(Some("b"), None).len(), 1);
    }

    // --- F-68 ---

    #[test]
    fn source_and_affected_are_recorded() {
        let store = HistoryStore::new(10);
        let ai = store.begin_with("p", "select 1", HistorySource::Ai);
        store.finish_with(ai, 5, true, Some(1));
        store.begin_with("p", "select 2", HistorySource::Mcp);
        store.begin_with("p", "select 3", HistorySource::File);

        let entry = &store.list(None, None)[2];
        assert_eq!(entry.source, HistorySource::Ai);
        assert_eq!(entry.affected, Some(1));
        assert_eq!(entry.source.label(), "AI");

        let ai_only = store.list_filtered(&HistoryFilter {
            source: Some(HistorySource::Ai),
            ..HistoryFilter::default()
        });
        assert_eq!(ai_only.len(), 1);
        assert_eq!(ai_only[0].source, HistorySource::Ai);
    }

    #[test]
    fn date_range_filter_is_inclusive() {
        let store = HistoryStore::new(10);
        store.begin("p", "select 1");
        let mid = now_ms();
        store.begin("p", "select 2");

        let newer = store.list_filtered(&HistoryFilter {
            from_ms: Some(mid),
            ..HistoryFilter::default()
        });
        assert!(newer.iter().all(|e| e.ts_ms >= mid));

        let future = store.list_filtered(&HistoryFilter {
            from_ms: Some(mid + 60_000),
            ..HistoryFilter::default()
        });
        assert!(future.is_empty(), "未来时间点不应命中任何记录");
    }

    #[test]
    fn pinned_only_filter_works() {
        let store = HistoryStore::new(10);
        let a = store.begin("p", "select 1");
        store.toggle_pin(a);
        store.begin("p", "select 2");
        let pinned = store.list_filtered(&HistoryFilter {
            pinned_only: true,
            ..HistoryFilter::default()
        });
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].sql, "select 1");
    }

    #[test]
    fn limit_zero_means_unbounded_and_shrinking_trims_now() {
        let store = HistoryStore::new(0);
        for i in 0..50 {
            store.begin("p", &format!("select {i}"));
        }
        assert_eq!(store.len(), 50);

        store.set_limit(10);
        assert_eq!(store.len(), 10);
        assert_eq!(store.limit(), 10);
        assert_eq!(store.list(None, None)[0].sql, "select 49");
    }

    #[test]
    fn filter_limit_caps_result_size() {
        let store = HistoryStore::new(100);
        for i in 0..30 {
            store.begin("p", &format!("select {i}"));
        }
        let page = store.list_filtered(&HistoryFilter {
            limit: Some(5),
            ..HistoryFilter::default()
        });
        assert_eq!(page.len(), 5);
    }

    // --- F-68 持久化 ---

    fn temp_history_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustdb-history-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.jsonl")
    }

    fn entry(id: u64, sql: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            profile_id: "p".into(),
            sql: sql.into(),
            ts_ms: 1,
            elapsed_ms: Some(1),
            ok: Some(true),
            pinned: false,
            source: HistorySource::Manual,
            affected: None,
        }
    }

    #[test]
    fn history_survives_restart_with_pin_and_source() {
        let path = temp_history_path("roundtrip");
        let first_id = {
            let store = HistoryStore::with_path(100, &path);
            let id = store.begin_with("p", "select 1", HistorySource::File);
            store.finish_with(id, 12, true, Some(3));
            // 未完成的执行不应落盘。
            store.begin("p", "select 2");
            let pinned = store.begin("p", "select 3");
            store.finish(pinned, 1, false);
            assert_eq!(store.toggle_pin(pinned), Some(true));
            id
        };

        let store = HistoryStore::with_path(100, &path);
        assert_eq!(store.len(), 2, "未完成条目不落盘");
        let all = store.list(None, None);
        assert_eq!(all[0].sql, "select 3", "固定项排最前");
        assert!(all[0].pinned);
        assert_eq!(all[0].ok, Some(false));
        let file_entry = all.iter().find(|e| e.sql == "select 1").unwrap();
        assert_eq!(file_entry.source, HistorySource::File);
        assert_eq!(file_entry.affected, Some(3));
        assert_eq!(file_entry.elapsed_ms, Some(12));
        assert!(
            store.begin("p", "select 4") > first_id,
            "重启后 id 继续递增，不复用"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_lines_are_skipped_on_load() {
        let path = temp_history_path("corrupt");
        std::fs::write(
            &path,
            format!(
                "{}\nnot json\n\n{}\n",
                serde_json::to_string(&entry(7, "select 7")).unwrap(),
                serde_json::to_string(&entry(9, "select 9")).unwrap()
            ),
        )
        .unwrap();

        let store = HistoryStore::with_path(10, &path);
        assert_eq!(store.len(), 2);
        assert_eq!(store.begin("p", "x"), 10, "损坏行不影响 id 推导");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn deletions_and_limit_changes_persist() {
        let path = temp_history_path("mutations");
        {
            let store = HistoryStore::with_path(0, &path);
            let a = store.begin("p", "a");
            store.finish(a, 1, true);
            let b = store.begin("p", "b");
            store.finish(b, 1, true);
            let c = store.begin("p", "c");
            store.finish(c, 1, true);
            assert!(store.remove(a));
            store.set_limit(1);
            assert_eq!(store.len(), 1, "调小上限立即淘汰最旧的未固定项");
        }

        let store = HistoryStore::with_path(0, &path);
        let all = store.list(None, None);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].sql, "c");
        store.clear(None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        assert!(HistoryStore::with_path(0, &path).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
