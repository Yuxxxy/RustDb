//! 崩溃恢复：把「最近一次工作现场」（活动数据源 + 各标签页 SQL）落到
//! `session.json`，下次启动时回填。只保存**可重建**的东西，不存结果集。
//!
//! 写入策略与 `connections.json` 一致：先写临时文件再原子替换，避免半截文件。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一个标签页的可恢复现场。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSnapshot {
    /// 标签页绑定的库（`catalog` / `schema` 已拼成展示名）。
    pub namespace: Option<String>,
    pub sql: String,
}

/// 启动时要恢复的现场。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub profile_id: Option<String>,
    pub tabs: Vec<TabSnapshot>,
    pub active_tab: usize,
    /// 写入时刻（Unix 秒），用于展示「上次会话」。
    pub saved_at: u64,
}

impl SessionSnapshot {
    pub fn is_empty(&self) -> bool {
        self.tabs.iter().all(|tab| tab.sql.trim().is_empty())
    }
}

pub fn snapshot_path() -> PathBuf {
    crate::paths::data_dir().join("session.json")
}

/// 读取上次现场；文件不存在或损坏都返回 `None`（不阻塞启动）。
pub fn load_from(path: &Path) -> Option<SessionSnapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子写入：临时文件 + rename。
pub fn save_to(path: &Path, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(snapshot)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn load() -> Option<SessionSnapshot> {
    load_from(&snapshot_path())
}

pub fn save(snapshot: &SessionSnapshot) -> std::io::Result<()> {
    save_to(&snapshot_path(), snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rustdb-session-{tag}-{}.json", std::process::id()))
    }

    #[test]
    fn round_trip_preserves_tabs() {
        let path = temp_path("rt");
        let snapshot = SessionSnapshot {
            profile_id: Some("demo-sqlite".into()),
            tabs: vec![TabSnapshot {
                namespace: Some("main".into()),
                sql: "SELECT 1;".into(),
            }],
            active_tab: 0,
            saved_at: 42,
        };
        save_to(&path, &snapshot).expect("写入");
        assert_eq!(load_from(&path), Some(snapshot));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_is_ignored() {
        let path = temp_path("bad");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_from(&path).is_none(), "坏文件不能拖垮启动");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_snapshot_is_detected() {
        let snapshot = SessionSnapshot {
            tabs: vec![TabSnapshot {
                namespace: None,
                sql: "   ".into(),
            }],
            ..Default::default()
        };
        assert!(snapshot.is_empty(), "全空白 SQL 视为空现场");
    }
}
