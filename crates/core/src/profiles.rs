//! 连接配置持久化（F-02）：JSON 文件，**不含任何口令**。
//!
//! 口令只留 `secret_ref`（系统密钥库引用键）；`dev_secret` 这类内存字段是
//! `#[serde(skip)]`，落盘时自动丢弃。写入用「临时文件 + 替换」，避免半截文件。

use crate::profile::ConnectionProfile;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 连接配置落盘文件的格式版本。
pub const PROFILE_FILE_VERSION: u32 = 1;

/// 连接配置仓库：`path = None` 时只在内存里。
#[derive(Debug, Clone, Default)]
pub struct ProfileStore {
    path: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfileFile {
    version: u32,
    #[serde(default)]
    profiles: Vec<ConnectionProfile>,
}

impl ProfileStore {
    /// 只在内存里（不落盘）。
    pub fn in_memory() -> Self {
        Self { path: None }
    }

    /// 落到指定文件。
    pub fn with_path(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 读取配置。文件不存在返回空列表；文件损坏也返回空列表并记日志（不阻塞启动）。
    pub fn load(&self) -> Vec<ConnectionProfile> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                log::warn!("读取 {} 失败：{error}", path.display());
                return Vec::new();
            }
        };
        match serde_json::from_str::<ProfileFile>(&text) {
            Ok(file) => {
                if file.version > PROFILE_FILE_VERSION {
                    log::warn!(
                        "{} 的版本 {} 高于当前支持的 {}，已忽略",
                        path.display(),
                        file.version,
                        PROFILE_FILE_VERSION
                    );
                    return Vec::new();
                }
                file.profiles
            }
            Err(error) => {
                log::warn!("解析 {} 失败：{error}", path.display());
                Vec::new()
            }
        }
    }

    /// 原子写入。
    pub fn save(&self, profiles: &[ConnectionProfile]) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = ProfileFile {
            version: PROFILE_FILE_VERSION,
            profiles: profiles.to_vec(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::DbKind;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rustdb-profiles-{tag}-{}.json", std::process::id()))
    }

    fn profile(id: &str) -> ConnectionProfile {
        let mut profile = ConnectionProfile::demo_sqlite();
        profile.id = id.into();
        profile.name = id.into();
        profile
    }

    #[test]
    fn round_trip_keeps_profiles_and_drops_secrets() {
        let path = temp_path("round-trip");
        let store = ProfileStore::with_path(path.clone());
        let mut first = profile("a");
        first.dev_secret = Some("hunter2".into());
        first.host = Some("10.0.0.1".into());
        store.save(&[first]).expect("写入");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter2"), "口令不得落盘：{text}");

        let loaded = store.load();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "a");
        assert_eq!(loaded[0].host.as_deref(), Some("10.0.0.1"));
        assert!(loaded[0].dev_secret.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_yields_empty_list() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        assert!(ProfileStore::with_path(path).load().is_empty());
    }

    #[test]
    fn broken_file_yields_empty_list() {
        let path = temp_path("broken");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(ProfileStore::with_path(path.clone()).load().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn in_memory_store_never_writes() {
        let store = ProfileStore::in_memory();
        assert!(store.save(&[profile("a")]).is_ok());
        assert!(store.load().is_empty());
    }

    #[test]
    fn tunnel_spec_survives_the_round_trip() {
        let path = temp_path("tunnel");
        let store = ProfileStore::with_path(path.clone());
        let mut first = profile("a");
        first.driver = DbKind::Postgres;
        first.tunnel = Some(rustdb_db_api::TunnelSpec {
            hops: vec![rustdb_db_api::TunnelHopSpec {
                host: "bastion".into(),
                port: 22,
                user: "ops".into(),
                auth: rustdb_db_api::TunnelAuthSpec::single(
                    rustdb_db_api::TunnelAuthMethod::KeyFile {
                        path: "~/.ssh/id_ed25519".into(),
                        passphrase_ref: None,
                        dev_passphrase: None,
                    },
                ),
                host_keys: rustdb_db_api::HostKeySpec::KnownHostsDefault,
            }],
        });
        store.save(&[first]).expect("写入");
        let loaded = store.load();
        assert_eq!(loaded.len(), 1);
        let tunnel = loaded[0].tunnel.as_ref().expect("隧道配置应保留");
        assert_eq!(tunnel.hops.len(), 1);
        assert_eq!(tunnel.hops[0].host, "bastion");
        let _ = std::fs::remove_file(&path);
    }
}
