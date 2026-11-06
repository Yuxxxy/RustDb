//! SSH 隧道接线（F-91）：把 `db-api` 的纯契约翻译成 `db-tunnel` 的执行参数。
//!
//! 这里只做翻译与校验，不碰 IO；真正的建链在 `db-tunnel`。

use rustdb_db_api::{
    HostKeySpec, ProxyKind as ApiProxyKind, ProxyOrder as ApiProxyOrder, ProxySpec,
    TunnelAuthMethod, TunnelSpec,
};
use rustdb_db_tunnel::{
    AuthMethod, HostKeyPolicy, ProxyAuth, ProxyConfig, ProxyKind, ProxyOrder, SshHop, TunnelConfig,
};
use std::path::PathBuf;

/// `~` 展开成用户主目录（Windows 用 `%USERPROFILE%`）。
pub fn expand_tilde(path: &str) -> PathBuf {
    let rest = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\"))
        .or_else(|| path.strip_prefix("~"));
    if let Some(rest) = rest {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            let rest = rest.trim_start_matches(['/', '\\']);
            if rest.is_empty() {
                return PathBuf::from(home);
            }
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// 把契约翻译成建链参数。
pub fn build_config(spec: &TunnelSpec) -> Result<TunnelConfig, String> {
    spec.validate()?;
    let mut hops = Vec::with_capacity(spec.hops.len());
    for (index, hop) in spec.hops.iter().enumerate() {
        let layer = index + 1;
        let mut auth = Vec::with_capacity(hop.auth.methods.len());
        for method in &hop.auth.methods {
            auth.push(map_auth(method).map_err(|error| format!("第 {layer} 层：{error}"))?);
        }
        hops.push(SshHop {
            host: hop.host.clone(),
            port: hop.port,
            user: hop.user.clone(),
            auth,
            host_keys: map_host_keys(&hop.host_keys),
        });
    }
    Ok(TunnelConfig {
        hops,
        ..TunnelConfig::default()
    })
}

fn map_auth(method: &TunnelAuthMethod) -> Result<AuthMethod, String> {
    match method {
        TunnelAuthMethod::KeyFile {
            path,
            passphrase_ref,
            dev_passphrase,
        } => {
            let passphrase = match (dev_passphrase, passphrase_ref) {
                (Some(value), _) => Some(value.clone()),
                (None, Some(reference)) => Some(
                    crate::secrets::resolve(reference)
                        .map_err(|error| format!("私钥口令：{error}"))?,
                ),
                (None, None) => None,
            };
            Ok(AuthMethod::KeyFile {
                path: expand_tilde(path),
                passphrase,
            })
        }
        TunnelAuthMethod::Password {
            dev_secret,
            secret_ref,
        } => {
            let secret = match (dev_secret, secret_ref) {
                (Some(value), _) => value.clone(),
                (None, Some(reference)) => crate::secrets::resolve(reference)
                    .map_err(|error| format!("SSH 口令：{error}"))?,
                (None, None) => {
                    return Err("SSH 口令未录入：请配置 secret_ref（env:/file:/裸键）".into())
                }
            };
            Ok(AuthMethod::Password(secret))
        }
        TunnelAuthMethod::None => Ok(AuthMethod::None),
    }
}

fn map_host_keys(spec: &HostKeySpec) -> HostKeyPolicy {
    match spec {
        HostKeySpec::KnownHosts { path: Some(path) } => {
            HostKeyPolicy::KnownHosts(expand_tilde(path))
        }
        HostKeySpec::KnownHosts { path: None } => HostKeyPolicy::KnownHostsDefault,
        HostKeySpec::KnownHostsDefault => HostKeyPolicy::KnownHostsDefault,
        HostKeySpec::InsecureAcceptAny => HostKeyPolicy::InsecureAcceptAny,
    }
}

/// 把代理契约翻译成建链参数。
///
/// 口令优先取内存中的 `dev_secret`；只配了 `secret_ref` 时按
/// `env:` / `file:` / 裸键规则解析，解析失败直接报错，不静默当成空口令。
pub fn build_proxy_config(spec: &ProxySpec) -> Result<ProxyConfig, String> {
    spec.validate()?;
    let auth = match &spec.username {
        Some(username) => {
            let password = match (&spec.dev_secret, &spec.secret_ref) {
                (Some(secret), _) => secret.clone(),
                (None, Some(reference)) => crate::secrets::resolve(reference)
                    .map_err(|error| format!("代理口令：{error}"))?,
                (None, None) => String::new(),
            };
            Some(ProxyAuth {
                username: username.trim().to_string(),
                password,
            })
        }
        None => None,
    };
    Ok(ProxyConfig {
        kind: match spec.kind {
            ApiProxyKind::Socks5 => ProxyKind::Socks5,
            ApiProxyKind::HttpConnect => ProxyKind::HttpConnect,
        },
        host: spec.host.trim().to_string(),
        port: spec.port,
        auth,
        remote_dns: spec.remote_dns,
        order: match spec.order {
            ApiProxyOrder::BeforeTunnel => ProxyOrder::BeforeTunnel,
            ApiProxyOrder::AfterTunnel => ProxyOrder::AfterTunnel,
        },
        connect_timeout: std::time::Duration::from_secs(15),
    })
}

/// 该跳板用的 known_hosts 文件路径（`None` 表示用默认路径）。
pub fn known_hosts_path(spec: &HostKeySpec) -> Option<PathBuf> {
    match spec {
        HostKeySpec::KnownHosts { path: Some(path) } => Some(expand_tilde(path)),
        HostKeySpec::KnownHosts { path: None } | HostKeySpec::KnownHostsDefault => {
            rustdb_db_tunnel::known_hosts::default_path()
        }
        HostKeySpec::InsecureAcceptAny => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{TunnelAuthSpec, TunnelHopSpec};

    fn hop(host: &str) -> TunnelHopSpec {
        TunnelHopSpec {
            host: host.into(),
            port: 22,
            user: "ops".into(),
            auth: TunnelAuthSpec::single(TunnelAuthMethod::None),
            host_keys: HostKeySpec::KnownHostsDefault,
        }
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .expect("测试环境应有主目录");
        let expanded = expand_tilde("~/.ssh/id_ed25519");
        assert_eq!(
            expanded,
            PathBuf::from(home).join(".ssh").join("id_ed25519")
        );
        assert_eq!(expand_tilde("/etc/hosts"), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn config_keeps_the_configured_layer_order() {
        let spec = TunnelSpec {
            hops: vec![hop("first"), hop("second")],
        };
        let config = build_config(&spec).expect("应能建链");
        assert_eq!(config.hops.len(), 2);
        assert_eq!(config.hops[0].host, "first");
        assert_eq!(config.hops[1].host, "second");
    }

    #[test]
    fn password_without_value_is_rejected() {
        let mut h = hop("gw");
        h.auth = TunnelAuthSpec::single(TunnelAuthMethod::Password {
            secret_ref: Some("k".into()),
            dev_secret: None,
        });
        let spec = TunnelSpec { hops: vec![h] };
        let error = build_config(&spec).unwrap_err();
        assert!(error.contains("第 1 层"), "{error}");
        assert!(error.contains("口令"), "{error}");
    }

    #[test]
    fn empty_hops_are_rejected() {
        let spec = TunnelSpec { hops: Vec::new() };
        assert!(build_config(&spec).is_err());
    }

    fn proxy(order: ApiProxyOrder) -> ProxySpec {
        ProxySpec {
            kind: ApiProxyKind::Socks5,
            host: " 10.0.0.2 ".into(),
            port: 1080,
            username: Some(" ops ".into()),
            secret_ref: None,
            dev_secret: Some("pw".into()),
            remote_dns: false,
            order,
        }
    }

    #[test]
    fn proxy_config_maps_contract_fields() {
        let config = build_proxy_config(&proxy(ApiProxyOrder::AfterTunnel)).expect("应能翻译");
        assert_eq!(config.kind, ProxyKind::Socks5);
        assert_eq!(config.host, "10.0.0.2");
        assert_eq!(config.port, 1080);
        assert!(!config.remote_dns);
        assert_eq!(config.order, ProxyOrder::AfterTunnel);
        assert_eq!(config.connect_timeout, std::time::Duration::from_secs(15));
        let auth = config.auth.expect("应带认证");
        assert_eq!(auth.username, "ops");
        assert_eq!(auth.password, "pw");
    }

    #[test]
    fn proxy_kind_and_order_follow_the_contract() {
        let mut spec = proxy(ApiProxyOrder::BeforeTunnel);
        spec.kind = ApiProxyKind::HttpConnect;
        let config = build_proxy_config(&spec).expect("应能翻译");
        assert_eq!(config.kind, ProxyKind::HttpConnect);
        assert_eq!(config.order, ProxyOrder::BeforeTunnel);
    }

    #[test]
    fn proxy_secret_ref_without_value_is_rejected() {
        let mut spec = proxy(ApiProxyOrder::BeforeTunnel);
        spec.dev_secret = None;
        // 用一个必然解析不到的引用键：解析失败必须显式报错，不能当空口令。
        spec.secret_ref = Some(format!("missing-{}", std::process::id()));
        let error = build_proxy_config(&spec).unwrap_err();
        assert!(error.starts_with("代理口令："), "{error}");
        assert!(error.contains("找不到口令"), "{error}");
    }

    #[test]
    fn proxy_without_username_skips_auth() {
        let mut spec = proxy(ApiProxyOrder::BeforeTunnel);
        spec.username = None;
        spec.dev_secret = None;
        let config = build_proxy_config(&spec).expect("无认证代理应可翻译");
        assert!(config.auth.is_none());
    }

    #[test]
    fn proxy_contract_errors_surface_verbatim() {
        let mut spec = proxy(ApiProxyOrder::BeforeTunnel);
        spec.host = "  ".into();
        assert_eq!(build_proxy_config(&spec).unwrap_err(), "代理缺少主机地址");
        spec.host = "10.0.0.2".into();
        spec.username = Some("  ".into());
        assert!(build_proxy_config(&spec)
            .unwrap_err()
            .contains("代理用户名"));
    }
}
