//! 对象面板的按连接状态（F-84）：置顶、最近访问、分组展开。
//!
//! 只存展示用的键（`库|种类|名称`），不存对象元数据；键失配时静默忽略。
//! 写入与 `session.json` 一致：临时文件 + 原子替换。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 最近访问列表上限。
pub const MAX_RECENT: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileObjectState {
    /// 置顶对象键，按固定顺序排列。
    #[serde(default)]
    pub pins: Vec<String>,
    /// 最近访问对象键，最近的在最前。
    #[serde(default)]
    pub recent: Vec<String>,
    /// 分组展开状态：`库|种类` → 是否展开；未记录时沿用默认（表展开）。
    #[serde(default)]
    pub groups: BTreeMap<String, bool>,
}

impl ProfileObjectState {
    pub fn is_pinned(&self, key: &str) -> bool {
        self.pins.iter().any(|item| item == key)
    }

    /// 切换置顶；返回切换后的状态。
    pub fn toggle_pin(&mut self, key: &str) -> bool {
        if let Some(index) = self.pins.iter().position(|item| item == key) {
            self.pins.remove(index);
            false
        } else {
            self.pins.push(key.to_string());
            true
        }
    }

    pub fn record_recent(&mut self, key: &str) {
        self.recent.retain(|item| item != key);
        self.recent.insert(0, key.to_string());
        self.recent.truncate(MAX_RECENT);
    }

    pub fn group_open(&self, key: &str) -> Option<bool> {
        self.groups.get(key).copied()
    }

    pub fn set_group(&mut self, key: &str, open: bool) {
        self.groups.insert(key.to_string(), open);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectState {
    /// 连接配置 id → 该连接下的对象状态。
    #[serde(default)]
    pub by_profile: BTreeMap<String, ProfileObjectState>,
}

impl ObjectState {
    pub fn profile(&self, id: &str) -> Option<&ProfileObjectState> {
        self.by_profile.get(id)
    }

    pub fn profile_mut(&mut self, id: &str) -> &mut ProfileObjectState {
        self.by_profile.entry(id.to_string()).or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.by_profile.values().all(|state| {
            state.pins.is_empty() && state.recent.is_empty() && state.groups.is_empty()
        })
    }
}

/// 落盘路径；`RUSTDB_OBJECT_STATE_PATH` 可覆盖（测试/便携模式）。
pub fn state_path() -> PathBuf {
    if let Ok(path) = std::env::var("RUSTDB_OBJECT_STATE_PATH") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    crate::paths::data_dir().join("object_state.json")
}

pub fn load_from(path: &Path) -> Option<ObjectState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_to(path: &Path, state: &ObjectState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(state)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn load() -> Option<ObjectState> {
    load_from(&state_path())
}

pub fn save(state: &ObjectState) -> std::io::Result<()> {
    save_to(&state_path(), state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rustdb-object-state-{tag}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn pins_toggle_and_recent_is_deduped() {
        let mut state = ProfileObjectState::default();
        assert!(state.toggle_pin("main|table|orders"));
        assert!(state.is_pinned("main|table|orders"));
        assert!(!state.toggle_pin("main|table|orders"));
        assert!(!state.is_pinned("main|table|orders"));

        state.record_recent("a");
        state.record_recent("b");
        state.record_recent("a");
        assert_eq!(state.recent, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn recent_is_capped() {
        let mut state = ProfileObjectState::default();
        for index in 0..(MAX_RECENT + 5) {
            state.record_recent(&format!("o{index}"));
        }
        assert_eq!(state.recent.len(), MAX_RECENT);
        assert_eq!(state.recent[0], format!("o{}", MAX_RECENT + 4));
    }

    #[test]
    fn round_trip_is_per_profile() {
        let path = temp_path("rt");
        let mut state = ObjectState::default();
        state.profile_mut("prod").toggle_pin("s|table|orders");
        state.profile_mut("prod").set_group("s|Table", false);
        state.profile_mut("dev").record_recent("d|view|v1");
        save_to(&path, &state).expect("写入");
        assert_eq!(load_from(&path), Some(state));
        let _ = std::fs::remove_file(&path);
    }
}
