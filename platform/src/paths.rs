//! 平台能力：配置路径。密钥 / 托盘 / 单实例在后续里程碑补齐。

use std::path::{Path, PathBuf};

/// 当前数据目录名（更名后为 `RustDb`）。
pub const APP_DIR: &str = "RustDb";
/// 更名前（RustSql 时代）的数据目录名，仅用于一次性迁移配置。
pub const LEGACY_APP_DIR: &str = "RustSql";

/// 平台相关的配置根目录（由 `dir_name` 决定子目录名）。
fn config_root(dir_name: &str) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join(dir_name);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(dir_name);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Linux 习惯用全小写目录名。
        let dir_name = dir_name.to_ascii_lowercase();
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join(&dir_name);
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".config").join(&dir_name);
        }
    }
    std::env::temp_dir().join(dir_name)
}

pub fn data_dir() -> PathBuf {
    config_root(APP_DIR)
}

/// 更名前使用的数据目录（迁移来源）。
pub fn legacy_data_dir() -> PathBuf {
    config_root(LEGACY_APP_DIR)
}

pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(dir.join("logs"))?;
    // 更名后首次启动：把旧目录里的配置复制过来（只复制，不删除旧目录）。
    let copied = migrate_legacy_config(&legacy_data_dir(), &dir);
    if copied > 0 {
        log::info!(
            "已从旧数据目录迁移 {copied} 个配置文件：{} → {}",
            legacy_data_dir().display(),
            dir.display()
        );
    }
    Ok(dir)
}

/// 把 `from` 里的配置文件复制到 `to`（**目标已存在的文件不覆盖**）。
///
/// 返回实际复制的文件数。任何单文件失败都跳过、不阻塞启动（R5：失败有日志，不静默）。
pub fn migrate_legacy_config(from: &Path, to: &Path) -> usize {
    const FILES: [&str; 2] = ["connections.json", "settings.json"];
    if from == to || !from.is_dir() || !to.is_dir() {
        return 0;
    }
    let mut copied = 0;
    for name in FILES {
        let src = from.join(name);
        let dst = to.join(name);
        if !src.is_file() || dst.exists() {
            continue;
        }
        match std::fs::copy(&src, &dst) {
            Ok(_) => copied += 1,
            Err(err) => log::warn!("迁移 {name} 失败：{err}"),
        }
    }
    copied
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn connections_path() -> PathBuf {
    data_dir().join("connections.json")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustdb-paths-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn migration_copies_missing_files_only() {
        let from = temp_dir("from");
        let to = temp_dir("to");
        std::fs::write(from.join("connections.json"), "{\"a\":1}").unwrap();
        std::fs::write(from.join("settings.json"), "{\"theme\":\"dark\"}").unwrap();
        // 目标已有 settings.json：不得被旧值覆盖。
        std::fs::write(to.join("settings.json"), "{\"theme\":\"light\"}").unwrap();

        let copied = migrate_legacy_config(&from, &to);
        assert_eq!(copied, 1, "只应复制缺失的 connections.json");
        assert_eq!(
            std::fs::read_to_string(to.join("connections.json")).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("settings.json")).unwrap(),
            "{\"theme\":\"light\"}",
            "已有配置不能被覆盖"
        );
        // 幂等：再跑一次不再复制。
        assert_eq!(migrate_legacy_config(&from, &to), 0);

        let _ = std::fs::remove_dir_all(&from);
        let _ = std::fs::remove_dir_all(&to);
    }

    #[test]
    fn migration_is_noop_without_legacy_dir() {
        let to = temp_dir("to2");
        let missing = temp_dir("missing").join("nope");
        assert_eq!(migrate_legacy_config(&missing, &to), 0);
        let _ = std::fs::remove_dir_all(&to);
    }
}
