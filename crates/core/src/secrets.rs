//! 口令解析：把 `secret_ref` 解析成真正的口令字符串。
//!
//! 支持的引用形式（大小写不敏感的前缀）：
//!
//! | 形式 | 含义 |
//! |---|---|
//! | `env:NAME` | 读取环境变量 `NAME` |
//! | `file:/abs/path` 或 `file:relative` | 读取文件内容（去掉尾部换行） |
//! | 裸键 `prod-pg` | 先查 `RUSTDB_SECRET_PROD_PG`，再读 `<密钥目录>/prod-pg` |
//!
//! 密钥目录默认 `~/.rustdb/secrets`，可用 `RUSTDB_SECRET_DIR` 覆盖。
//!
//! 不引入系统密钥库 crate：Windows / macOS / Linux 的实现差异与额外依赖代价
//! 都不小，而「环境变量 / 受限文件」已能覆盖 CI 与本机两种主要场景。`keyring:`
//! 形式的引用会得到明确的「未接入」提示，不会静默当成空口令。
//!
//! 安全约定：文件口令只在本地读取，解析结果不写日志、不进事件、不落盘。

use std::path::{Path, PathBuf};

/// 解析口令引用。
pub fn resolve(reference: &str) -> Result<String, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("口令引用为空".into());
    }
    let lower = reference.to_ascii_lowercase();
    if lower.starts_with("env:") {
        let name = reference[4..].trim();
        return env_secret(name)
            .ok_or_else(|| format!("环境变量 {name} 未设置（口令引用 env:{name}）"));
    }
    if lower.starts_with("file:") {
        let path = expand_tilde(reference[5..].trim());
        return read_secret_file(&path);
    }
    if lower.starts_with("keyring:") {
        return Err(
            "系统密钥库尚未接入；请改用 env:<变量名> 或 file:<路径> 形式的 secret_ref".into(),
        );
    }

    // 裸键：环境变量优先，其次密钥目录下的同名文件。
    if let Some(secret) = env_secret(&env_name_for(reference)) {
        return Ok(secret);
    }
    let path = secrets_dir().join(reference);
    if path.exists() {
        return read_secret_file(&path);
    }
    Err(format!(
        "找不到口令 {reference}：请设置环境变量 {}，或把口令写入 {}",
        env_name_for(reference),
        path.display()
    ))
}

/// `prod-pg` → `RUSTDB_SECRET_PROD_PG`。
fn env_name_for(key: &str) -> String {
    let mut name = String::from("RUSTDB_SECRET_");
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name
}

fn env_secret(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let value = std::env::var(name).ok()?;
    let value = value.trim_end_matches(['\r', '\n']).to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn read_secret_file(path: &Path) -> Result<String, String> {
    // Windows 没有 POSIX 权限位；Unix 上顺带提示过宽权限，避免口令文件对同机用户可读。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                log::warn!(
                    "口令文件 {} 权限为 {:o}，建议 chmod 600",
                    path.display(),
                    mode
                );
            }
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取口令文件 {} 失败：{error}", path.display()))?;
    let text = text.trim_end_matches(['\r', '\n']).to_string();
    if text.is_empty() {
        return Err(format!("口令文件 {} 为空", path.display()));
    }
    Ok(text)
}

fn secrets_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUSTDB_SECRET_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".rustdb").join("secrets")
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_secret(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustdb-secret-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn file_reference_strips_trailing_newline() {
        let path = temp_secret("a.txt", "hunter2\r\n");
        let reference = format!("file:{}", path.display());
        assert_eq!(resolve(&reference).unwrap(), "hunter2");
    }

    #[test]
    fn bare_key_falls_back_to_secret_dir() {
        let dir = temp_secret("key", "s3cret\n")
            .parent()
            .unwrap()
            .to_path_buf();
        // 密钥目录里的文件名就是裸键。
        std::fs::write(dir.join("prod-pg"), "s3cret\n").unwrap();
        // 环境变量优先级更高，先清掉可能存在的同名变量。
        std::env::remove_var("RUSTDB_SECRET_PROD_PG");
        // 该测试不并行修改全局状态：使用唯一变量名规避竞态。
        let previous = std::env::var_os("RUSTDB_SECRET_DIR");
        std::env::set_var("RUSTDB_SECRET_DIR", &dir);
        let resolved = resolve("prod-pg");
        match previous {
            Some(value) => std::env::set_var("RUSTDB_SECRET_DIR", value),
            None => std::env::remove_var("RUSTDB_SECRET_DIR"),
        }
        assert_eq!(resolved.unwrap(), "s3cret");
    }

    #[test]
    fn empty_reference_and_missing_file_are_errors() {
        assert!(resolve("   ").is_err());
        let missing = std::env::temp_dir().join("rustdb-secret-definitely-missing");
        let error = resolve(&format!("file:{}", missing.display())).unwrap_err();
        assert!(error.contains("读取口令文件"), "{error}");
    }

    #[test]
    fn keyring_reference_reports_not_available() {
        let error = resolve("keyring:prod").unwrap_err();
        assert!(error.contains("尚未接入"), "{error}");
    }
}
