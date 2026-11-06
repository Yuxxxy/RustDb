//! known_hosts 解析与校验（F-91 / X-05）。
//!
//! 校验发生在 SSH 握手阶段、任何认证凭据发送之前：未知或变更的主机密钥
//! 一律返回错误，由调用方决定是否让用户确认后写入 known_hosts。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use data_encoding::BASE64;
use hmac::{Hmac, Mac};
use russh::keys::ssh_key::{HashAlg, PublicKey};
use sha1::Sha1;

/// 主机密钥与记录比对的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// 与记录完全一致。
    Match,
    /// 该主机没有记录（首次连接）。
    Unknown,
    /// 记录了同算法的密钥，但本次不同（可能被中间人替换）。
    Changed { recorded: String },
}

/// OpenSSH 的 host 字段：非 22 端口写成 `[host]:port`。
pub fn host_field(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// SHA-256 指纹（与 `ssh-keygen -lf` 展示的一致）。
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 默认 known_hosts 路径：`$HOME/.ssh/known_hosts`（Windows 用 `%USERPROFILE%`）。
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".ssh").join("known_hosts"))
}

/// 校验主机密钥。文件不存在视为未知（首次连接）。
pub fn check(
    path: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<HostKeyVerdict, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostKeyVerdict::Unknown);
        }
        Err(error) => {
            return Err(format!("读取 {} 失败：{error}", path.display()));
        }
    };
    let host_port = host_field(host, port);
    let mut changed: Option<String> = None;
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(pattern), Some(algorithm), Some(encoded)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !matches_pattern(&host_port, pattern) {
            continue;
        }
        let Ok(recorded) = PublicKey::from_openssh(&format!("{algorithm} {encoded}")) else {
            continue;
        };
        if recorded == *key {
            return Ok(HostKeyVerdict::Match);
        }
        if recorded.algorithm() == key.algorithm() {
            changed = Some(format!(
                "{}:{} {}",
                path.display(),
                index + 1,
                fingerprint(&recorded)
            ));
        }
    }
    Ok(match changed {
        Some(recorded) => HostKeyVerdict::Changed { recorded },
        None => HostKeyVerdict::Unknown,
    })
}

/// 追加一条记录（用户确认后调用）。
pub fn record(path: &Path, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let openssh = key
        .to_openssh()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{} {openssh}", host_field(host, port))
}

/// 追加一条**已确认**的记录（公钥内容来自握手错误里带回的 OpenSSH 行）。
///
/// 与 [`record`] 的区别：这里不重新解析公钥，只按原样落盘，避免二次解析失败。
pub fn record_openssh(path: &Path, host: &str, port: u16, openssh: &str) -> std::io::Result<()> {
    let openssh = openssh.trim();
    if openssh.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "公钥内容为空",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{} {openssh}", host_field(host, port))
}

/// 用新公钥**替换**该主机的旧记录（用户确认「密钥已变更」后调用）。
///
/// 匹配到该主机的行（含通配与 hashed 形式）会被删除，再写入新记录；
/// 其它主机的记录原样保留。
pub fn replace_host(path: &Path, host: &str, port: u16, openssh: &str) -> std::io::Result<()> {
    let openssh = openssh.trim();
    if openssh.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "公钥内容为空",
        ));
    }
    let host_port = host_field(host, port);
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut kept: Vec<&str> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            kept.push(line);
            continue;
        }
        let pattern = trimmed.split_whitespace().next().unwrap_or_default();
        if matches_pattern(&host_port, pattern) {
            continue;
        }
        kept.push(line);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = kept.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("{host_port} {openssh}\n"));
    let tmp = path.with_extension("known_hosts.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn matches_pattern(host_port: &str, pattern: &str) -> bool {
    for entry in pattern.split(',') {
        if let Some(rest) = entry.strip_prefix("|1|") {
            let mut parts = rest.split('|');
            let (Some(salt), Some(expected)) = (parts.next(), parts.next()) else {
                continue;
            };
            let (Ok(salt), Ok(expected)) = (
                BASE64.decode(salt.as_bytes()),
                BASE64.decode(expected.as_bytes()),
            ) else {
                continue;
            };
            if let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&salt) {
                mac.update(host_port.as_bytes());
                if mac.verify_slice(&expected).is_ok() {
                    return true;
                }
            }
        } else if let Some(suffix) = entry.strip_prefix("*.") {
            let suffix = format!(".{suffix}");
            if host_port.len() > suffix.len()
                && host_port
                    .to_ascii_lowercase()
                    .ends_with(&suffix.to_ascii_lowercase())
            {
                return true;
            }
        } else if entry.eq_ignore_ascii_case(host_port) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::{getrandom::SysRng, rand_core::UnwrapErr, Algorithm, PrivateKey};

    fn key() -> PublicKey {
        let mut rng = UnwrapErr(SysRng);
        PrivateKey::random(&mut rng, Algorithm::Ed25519)
            .expect("生成测试密钥失败")
            .public_key()
            .clone()
    }

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("rustdb-known-hosts-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn missing_file_is_first_connect() {
        let file = TempFile::new("missing");
        assert_eq!(
            check(file.path(), "db.example.com", 22, &key()).unwrap(),
            HostKeyVerdict::Unknown
        );
    }

    #[test]
    fn recorded_key_matches_and_changed_key_is_flagged() {
        let file = TempFile::new("match");
        let first = key();
        record(file.path(), "db.example.com", 22, &first).unwrap();
        assert_eq!(
            check(file.path(), "db.example.com", 22, &first).unwrap(),
            HostKeyVerdict::Match
        );

        let second = key();
        match check(file.path(), "db.example.com", 22, &second).unwrap() {
            HostKeyVerdict::Changed { recorded } => {
                assert!(recorded.contains(&fingerprint(&first)), "{recorded}");
            }
            other => panic!("应变更为 Changed，实际 {other:?}"),
        }
    }

    #[test]
    fn non_default_port_uses_brackets() {
        let file = TempFile::new("port");
        let key = key();
        record(file.path(), "db.example.com", 2222, &key).unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(
            text.starts_with("[db.example.com]:2222 ssh-ed25519 "),
            "{text}"
        );
        assert_eq!(
            check(file.path(), "db.example.com", 2222, &key).unwrap(),
            HostKeyVerdict::Match
        );
        assert_eq!(
            check(file.path(), "db.example.com", 22, &key).unwrap(),
            HostKeyVerdict::Unknown,
            "端口不同不能复用记录"
        );
    }

    #[test]
    fn wildcard_and_hashed_patterns_are_supported() {
        let file = TempFile::new("patterns");
        let key = key();
        let openssh = key.to_openssh().unwrap();
        let salt = b"0123456789abcdef";
        let mut mac = Hmac::<Sha1>::new_from_slice(salt).unwrap();
        mac.update(b"db.example.com");
        let digest = BASE64.encode(&mac.finalize().into_bytes());
        let hashed = format!("|1|{}|{} {openssh}\n", BASE64.encode(salt), digest);
        std::fs::write(
            file.path(),
            format!("*.wild.example.com {openssh}\n{hashed}"),
        )
        .unwrap();

        assert_eq!(
            check(file.path(), "db.wild.example.com", 22, &key).unwrap(),
            HostKeyVerdict::Match
        );
        assert_eq!(
            check(file.path(), "db.example.com", 22, &key).unwrap(),
            HostKeyVerdict::Match,
            "hashed 记录应命中"
        );
        assert_eq!(
            check(file.path(), "other.wild.example.com", 22, &key).unwrap(),
            HostKeyVerdict::Match
        );
    }

    #[test]
    fn confirmed_key_is_appended_as_openssh_line() {
        let file = TempFile::new("tofu");
        let key = key();
        let openssh = key.to_openssh().unwrap();
        record_openssh(file.path(), "db.example.com", 22, &openssh).unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(text.starts_with("db.example.com ssh-ed25519 "), "{text}");
        assert_eq!(
            check(file.path(), "db.example.com", 22, &key).unwrap(),
            HostKeyVerdict::Match
        );
    }

    #[test]
    fn replacing_a_host_drops_only_its_own_records() {
        let file = TempFile::new("replace");
        let first = key();
        let second = key();
        let other = key();
        record(file.path(), "db.example.com", 22, &first).unwrap();
        record(file.path(), "other.example.com", 22, &other).unwrap();

        replace_host(
            file.path(),
            "db.example.com",
            22,
            &second.to_openssh().unwrap(),
        )
        .unwrap();

        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(!text.contains(&first.to_openssh().unwrap()), "{text}");
        assert!(text.contains(&other.to_openssh().unwrap()), "{text}");
        assert_eq!(
            check(file.path(), "db.example.com", 22, &second).unwrap(),
            HostKeyVerdict::Match
        );
        assert_eq!(
            check(file.path(), "other.example.com", 22, &other).unwrap(),
            HostKeyVerdict::Match
        );
    }

    #[test]
    fn replacing_removes_matching_hashed_records() {
        let file = TempFile::new("replace-hashed");
        let first = key();
        let second = key();
        let salt = b"0123456789abcdef";
        let mut mac = Hmac::<Sha1>::new_from_slice(salt).unwrap();
        mac.update(b"db.example.com");
        let digest = BASE64.encode(&mac.finalize().into_bytes());
        let hashed = format!(
            "|1|{}|{} {}",
            BASE64.encode(salt),
            digest,
            first.to_openssh().unwrap()
        );
        std::fs::write(file.path(), format!("{hashed}\n")).unwrap();

        replace_host(
            file.path(),
            "db.example.com",
            22,
            &second.to_openssh().unwrap(),
        )
        .unwrap();
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(!text.contains("|1|"), "hashed 旧记录应被移除：{text}");
        assert_eq!(
            check(file.path(), "db.example.com", 22, &second).unwrap(),
            HostKeyVerdict::Match
        );
    }
}
