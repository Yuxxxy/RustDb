//! 子命令实现。

pub mod connections;
pub mod context;
pub mod dump;
pub mod query;
pub mod schema;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rustdb_core::{ConnectionProfile, Core, ProfileStore};

use crate::error::CliError;

/// 元数据请求的等待上限。
pub const META_TIMEOUT: Duration = Duration::from_secs(60);

/// 组装 core：CLI 与桌面端共用同一个组合根（驱动只在 `rustdb-bootstrap` 注册）。
pub fn build_core(connections: Option<PathBuf>) -> Arc<Core> {
    let store = match connections {
        Some(path) => ProfileStore::with_path(path),
        None => rustdb_bootstrap::open_profiles(),
    };
    rustdb_bootstrap::build_core_with(store, rustdb_bootstrap::open_history())
}

/// 按 id 找连接配置。
pub fn find_profile(core: &Core, id: &str) -> Result<ConnectionProfile, CliError> {
    core.profiles()
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or_else(|| CliError::config(format!("找不到连接配置 {id}")))
}

/// 口令来源：环境变量优先，其次配置文件里的内存口令。
///
/// * `RUSTDB_PASSWORD_<连接 id 大写>`（连字符换成下划线）
/// * `RUSTDB_PASSWORD`
///
/// 口令只写进内存（`dev_secret` 是 `#[serde(skip)]`），**不落盘**。
pub fn resolve_secret(profile: &ConnectionProfile) -> Option<String> {
    let scoped = format!(
        "RUSTDB_PASSWORD_{}",
        profile
            .id
            .to_ascii_uppercase()
            .replace(['-', '.', ' '], "_")
    );
    std::env::var(scoped)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("RUSTDB_PASSWORD")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| profile.dev_secret.clone())
}

/// 把口令注进内存里的连接配置（不写文件）。
pub fn inject_secret(core: &Core, profile_id: &str, profile: &ConnectionProfile) {
    if let Some(secret) = resolve_secret(profile) {
        let _ = core.set_profile_secret(profile_id, Some(secret));
    }
}

/// 建链等待时长：连接超时 + 余量。
pub fn connect_timeout(profile: &ConnectionProfile) -> Duration {
    Duration::from_millis(profile.connect_timeout_ms.saturating_add(5_000))
}

/// 选择命名空间：指定了就按显示名匹配，否则用当前库。
pub fn pick_namespace(
    core: &Core,
    namespaces: &[rustdb_db_api::Namespace],
    wanted: Option<&str>,
) -> Result<rustdb_db_api::Namespace, CliError> {
    match wanted {
        Some(label) => namespaces
            .iter()
            .find(|namespace| namespace.display_label() == label)
            .cloned()
            .ok_or_else(|| {
                let known: Vec<String> = namespaces
                    .iter()
                    .map(|namespace| namespace.display_label())
                    .collect();
                CliError::config(format!(
                    "找不到命名空间 {label}（可用：{}）",
                    if known.is_empty() {
                        "无".to_string()
                    } else {
                        known.join("、")
                    }
                ))
            }),
        None => core
            .current_namespace()
            .or_else(|| namespaces.first().cloned())
            .ok_or_else(|| CliError::config("该连接没有可用命名空间")),
    }
}

/// 输出到文件或标准输出。
pub fn emit(text: &str, output: Option<&std::path::PathBuf>) -> Result<(), CliError> {
    match output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| CliError::io(format!("创建目录失败：{error}")))?;
                }
            }
            std::fs::write(path, text)
                .map_err(|error| CliError::io(format!("写入 {} 失败：{error}", path.display())))
        }
        None => {
            print!("{text}");
            Ok(())
        }
    }
}
