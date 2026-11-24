//! AI 会话持久化（F-48a）：`ai_sessions.json`。
//!
//! 只存**对话文本**（角色 + 内容），不存结果集、不存凭据。
//! 写入策略与 `session.json` 一致：临时文件 + 原子替换。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 上限：超过就丢最旧的（需求 11.8）。
pub const MAX_SESSIONS: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
}

impl StoredMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiSession {
    pub id: String,
    pub title: String,
    /// Unix 秒。
    pub updated_at: u64,
    pub messages: Vec<StoredMessage>,
}

impl AiSession {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            updated_at: now_secs(),
            messages: Vec::new(),
        }
    }

    /// 会话标题缺省用第一条用户消息（截断 30 字）。
    pub fn default_title(messages: &[StoredMessage]) -> String {
        messages
            .iter()
            .find(|message| message.role == "user")
            .map(|message| {
                let text = message.content.trim().replace('\n', " ");
                if text.chars().count() > 30 {
                    format!("{}…", text.chars().take(30).collect::<String>())
                } else {
                    text
                }
            })
            .unwrap_or_else(|| "新对话".to_string())
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn sessions_path() -> PathBuf {
    crate::paths::data_dir().join("ai_sessions.json")
}

/// 读全部会话（按 `updated_at` 倒序，上限 50）。文件缺失/损坏返回空表。
pub fn load_all_from(path: &Path) -> Vec<AiSession> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(mut sessions) = serde_json::from_str::<Vec<AiSession>>(&text) else {
        log::warn!("AI 会话文件损坏，已忽略：{}", path.display());
        return Vec::new();
    };
    sort_and_trim(&mut sessions);
    sessions
}

pub fn load_all() -> Vec<AiSession> {
    load_all_from(&sessions_path())
}

fn sort_and_trim(sessions: &mut Vec<AiSession>) {
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions.truncate(MAX_SESSIONS);
}

/// 原子写入整表。
pub fn save_all_to(path: &Path, sessions: &[AiSession]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut table = sessions.to_vec();
    sort_and_trim(&mut table);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&table)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn save_all(sessions: &[AiSession]) -> std::io::Result<()> {
    save_all_to(&sessions_path(), sessions)
}

/// 插入或更新一个会话（按 id 合并），返回整表。
pub fn upsert_to(path: &Path, session: AiSession) -> std::io::Result<Vec<AiSession>> {
    let mut table = load_all_from(path);
    match table.iter_mut().find(|item| item.id == session.id) {
        Some(existing) => *existing = session,
        None => table.push(session),
    }
    sort_and_trim(&mut table);
    save_all_to(path, &table)?;
    Ok(table)
}

pub fn upsert(session: AiSession) -> std::io::Result<Vec<AiSession>> {
    upsert_to(&sessions_path(), session)
}

/// 删除一个会话。
pub fn remove_to(path: &Path, id: &str) -> std::io::Result<Vec<AiSession>> {
    let mut table = load_all_from(path);
    table.retain(|item| item.id != id);
    save_all_to(path, &table)?;
    Ok(table)
}

pub fn remove(id: &str) -> std::io::Result<Vec<AiSession>> {
    remove_to(&sessions_path(), id)
}

/// 重命名（只改标题，消息不动）。
pub fn rename_to(path: &Path, id: &str, title: &str) -> std::io::Result<Vec<AiSession>> {
    let mut table = load_all_from(path);
    if let Some(item) = table.iter_mut().find(|item| item.id == id) {
        item.title = title.to_string();
        item.updated_at = now_secs();
    }
    save_all_to(path, &table)?;
    Ok(table)
}

pub fn rename(id: &str, title: &str) -> std::io::Result<Vec<AiSession>> {
    rename_to(&sessions_path(), id, title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustdb-ai-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir.join("ai_sessions.json")
    }

    fn session(id: &str, title: &str) -> AiSession {
        let mut s = AiSession::new(id, title);
        s.messages
            .push(StoredMessage::new("user", "查一下最近订单"));
        s.messages.push(StoredMessage::new("assistant", "好的"));
        s
    }

    #[test]
    fn round_trip_keeps_messages() {
        let path = temp_path("rt");
        let original = session("s1", "订单");
        save_all_to(&path, &[original.clone()]).unwrap();
        let loaded = load_all_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].messages.len(), 2);
        assert_eq!(loaded[0].title, "订单");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upsert_merges_by_id_and_sorts_by_time() {
        let path = temp_path("upsert");
        let mut older = session("a", "旧");
        older.updated_at = 100;
        let mut newer = session("b", "新");
        newer.updated_at = 200;
        upsert_to(&path, older).unwrap();
        let table = upsert_to(&path, newer).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].id, "b", "应按更新时间倒序");
        // 同 id 再写 → 覆盖而不是追加
        let mut updated = session("a", "旧改");
        updated.updated_at = 300;
        let table = upsert_to(&path, updated).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].id, "a");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remove_and_rename_work() {
        let path = temp_path("rm");
        save_all_to(&path, &[session("a", "A"), session("b", "B")]).unwrap();
        let table = rename_to(&path, "a", "改名").unwrap();
        assert_eq!(table.iter().find(|s| s.id == "a").unwrap().title, "改名");
        let table = remove_to(&path, "b").unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.iter().all(|s| s.id != "b"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_is_ignored() {
        let path = temp_path("bad");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_all_from(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn table_is_capped_at_max_sessions() {
        let path = temp_path("cap");
        let many: Vec<AiSession> = (0..MAX_SESSIONS + 10)
            .map(|index| {
                let mut s = session(&format!("s{index}"), "x");
                s.updated_at = index as u64;
                s
            })
            .collect();
        save_all_to(&path, &many).unwrap();
        assert_eq!(load_all_from(&path).len(), MAX_SESSIONS);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_title_uses_first_user_message() {
        let messages = vec![
            StoredMessage::new("assistant", "你好"),
            StoredMessage::new("user", "  帮我查上周 GMV  并按渠道分组  "),
        ];
        assert_eq!(
            AiSession::default_title(&messages),
            "帮我查上周 GMV  并按渠道分组"
        );
        assert_eq!(AiSession::default_title(&[]), "新对话");
    }
}
