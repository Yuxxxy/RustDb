//! 代理契约（F-91a / F-08）：纯数据类型 + 校验，**不含任何网络实现**。
//!
//! 安全约定（19.6）：
//! - 代理口令只留 `secret_ref`（系统密钥库引用键）或内存中的 `dev_secret`；
//!   `dev_secret` 是 `#[serde(skip)]`，永远不会写进配置文件。
//! - 代理与 SSH 隧道的组合顺序由 `order` 明确声明，失败时必须指出具体层。
//! - 局域网监听是连接级开关（`ConnectionProfile::forward_lan`），默认关闭。

use serde::{Deserialize, Serialize};

/// 代理类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    /// SOCKS5（RFC 1928，含用户名 / 口令认证）。
    #[default]
    Socks5,
    /// HTTP CONNECT 隧道。
    HttpConnect,
}

impl ProxyKind {
    pub const ALL: [Self; 2] = [Self::Socks5, Self::HttpConnect];

    pub fn label(self) -> &'static str {
        match self {
            Self::Socks5 => "SOCKS5",
            Self::HttpConnect => "HTTP CONNECT",
        }
    }

    /// 连接串里的 scheme。
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Socks5 => "socks5",
            Self::HttpConnect => "http",
        }
    }

    /// 界面首次启用代理时的默认端口。
    pub fn default_port(self) -> u16 {
        match self {
            Self::Socks5 => 1080,
            Self::HttpConnect => 8080,
        }
    }
}

/// 代理相对 SSH 跳板的位置。没有隧道时该字段无意义。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyOrder {
    /// 先连代理，再通过代理连第一层跳板（没有隧道时直接连目标库）。
    #[default]
    BeforeTunnel,
    /// 先连最后一层跳板，再从跳板内部经代理连目标库。
    AfterTunnel,
}

impl ProxyOrder {
    pub const ALL: [Self; 2] = [Self::BeforeTunnel, Self::AfterTunnel];

    pub fn label(self) -> &'static str {
        match self {
            Self::BeforeTunnel => "代理 → SSH",
            Self::AfterTunnel => "SSH → 代理",
        }
    }
}

/// 连接级代理配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySpec {
    #[serde(default)]
    pub kind: ProxyKind,
    pub host: String,
    #[serde(default = "default_proxy_port")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    /// 代理口令的密钥库引用键。
    #[serde(default)]
    pub secret_ref: Option<String>,
    /// **仅开发 / 本地验证**：内存中的临时口令，永不写盘。
    #[serde(skip)]
    pub dev_secret: Option<String>,
    /// SOCKS5：把域名交给代理解析（默认开）；HTTP CONNECT 始终传域名。
    #[serde(default = "default_true")]
    pub remote_dns: bool,
    #[serde(default)]
    pub order: ProxyOrder,
}

fn default_proxy_port() -> u16 {
    ProxyKind::Socks5.default_port()
}

fn default_true() -> bool {
    true
}

impl Default for ProxySpec {
    fn default() -> Self {
        Self {
            kind: ProxyKind::default(),
            host: String::new(),
            port: default_proxy_port(),
            username: None,
            secret_ref: None,
            dev_secret: None,
            remote_dns: true,
            order: ProxyOrder::default(),
        }
    }
}

impl ProxySpec {
    /// 校验配置；返回第一条可读的错误。
    pub fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("代理缺少主机地址".into());
        }
        if self.port == 0 {
            return Err("代理端口无效".into());
        }
        if let Some(username) = &self.username {
            if username.trim().is_empty() {
                return Err("代理用户名不能为空（不需要认证时请留空）".into());
            }
        }
        if self.username.is_none() && (self.dev_secret.is_some() || self.secret_ref.is_some()) {
            return Err("填写代理口令时必须同时提供用户名".into());
        }
        Ok(())
    }

    /// `socks5://host:port` / `http://host:port`（IPv6 自动加方括号）。
    pub fn endpoint(&self) -> String {
        format!(
            "{}://{}:{}",
            self.kind.scheme(),
            bracket_ipv6(self.host.trim()),
            self.port
        )
    }

    /// 界面摘要：`SOCKS5 10.0.0.2:1080`。
    pub fn label(&self) -> String {
        format!(
            "{} {}:{}",
            self.kind.label(),
            bracket_ipv6(self.host.trim()),
            self.port
        )
    }

    /// 是否需要连接前补录口令。
    pub fn needs_secret(&self) -> bool {
        self.username.is_some() && self.secret_ref.is_some() && self.dev_secret.is_none()
    }

    /// 带用户名时的摘要：`SOCKS5 host:port（用户 ops）`。
    pub fn summary(&self) -> String {
        match self.username.as_deref() {
            Some(username) if !username.trim().is_empty() => {
                format!("{}（用户 {username}）", self.label())
            }
            _ => self.label(),
        }
    }
}

/// IPv6 字面量需要方括号才能和端口拼在一起。
pub fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ProxySpec {
        ProxySpec {
            kind: ProxyKind::Socks5,
            host: "proxy.internal".into(),
            port: 1080,
            username: None,
            secret_ref: None,
            dev_secret: None,
            remote_dns: true,
            order: ProxyOrder::BeforeTunnel,
        }
    }

    #[test]
    fn defaults_are_socks5_and_before_tunnel() {
        let proxy = ProxySpec {
            host: "p".into(),
            ..ProxySpec::default()
        };
        assert_eq!(proxy.kind, ProxyKind::Socks5);
        assert_eq!(proxy.port, 1080);
        assert_eq!(proxy.order, ProxyOrder::BeforeTunnel);
        assert!(proxy.remote_dns);
    }

    #[test]
    fn validation_points_at_the_problem() {
        let mut bad = spec();
        bad.host = "  ".into();
        assert!(bad.validate().unwrap_err().contains("主机"));

        let mut no_port = spec();
        no_port.port = 0;
        assert!(no_port.validate().unwrap_err().contains("端口"));

        let mut no_user = spec();
        no_user.dev_secret = Some("x".into());
        assert!(no_user.validate().unwrap_err().contains("用户名"));
    }

    #[test]
    fn endpoint_brackets_ipv6() {
        let mut proxy = spec();
        proxy.host = "2001:db8::1".into();
        proxy.port = 1080;
        assert_eq!(proxy.endpoint(), "socks5://[2001:db8::1]:1080");
        assert_eq!(proxy.label(), "SOCKS5 [2001:db8::1]:1080");
    }

    #[test]
    fn secrets_never_reach_disk() {
        let mut proxy = spec();
        proxy.username = Some("ops".into());
        proxy.secret_ref = Some("key".into());
        proxy.dev_secret = Some("hunter2".into());
        let text = serde_json::to_string(&proxy).unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("secret_ref"), "{text}");
        assert!(!proxy.needs_secret(), "内存里已有口令时不再要求补录");
    }

    #[test]
    fn order_labels_are_explicit() {
        assert_eq!(ProxyOrder::BeforeTunnel.label(), "代理 → SSH");
        assert_eq!(ProxyOrder::AfterTunnel.label(), "SSH → 代理");
        assert_eq!(ProxyKind::HttpConnect.scheme(), "http");
    }
}
