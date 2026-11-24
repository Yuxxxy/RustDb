//! SSH 隧道契约（F-91）：纯数据类型 + 校验，**不含任何 SSH 实现依赖**。
//!
//! 这里只描述「要连哪几跳、每跳用什么认证、怎么校验主机密钥」，
//! 真正的握手在 `db-tunnel`（russh）里完成，core 负责把契约翻译过去。
//!
//! 安全约定（19.6）：
//! - 主机密钥校验**先于任何凭据发送**；未知 / 变更都由上层阻断并要求确认。
//! - 多层跳板**按配置顺序建链**，失败时必须指出具体层。
//! - 口令只留 `secret_ref`（系统密钥库引用键）；`*_dev` 字段是内存中的临时值，
//!   `#[serde(skip)]` 保证永不落盘。

use serde::{Deserialize, Serialize};

/// SSH 认证方式。跳板可以配置多个方式，**按顺序尝试**：前一个失败才试下一个。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelAuthMethod {
    /// 私钥认证。`passphrase_ref` 指向系统密钥库中的私钥口令。
    KeyFile {
        path: String,
        /// 私钥口令的密钥库引用键。
        #[serde(default)]
        passphrase_ref: Option<String>,
        /// **仅开发/本地验证**：内存中的临时私钥口令，永不写盘。
        #[serde(skip)]
        dev_passphrase: Option<String>,
    },
    /// 密码认证。`secret_ref` 指向系统密钥库中的口令条目。
    Password {
        /// 口令的密钥库引用键。
        #[serde(default)]
        secret_ref: Option<String>,
        /// **仅开发/本地验证**：内存中的临时口令，永不写盘。
        #[serde(skip)]
        dev_secret: Option<String>,
    },
    /// `none` 认证（少数测试服务端）。
    None,
}
impl TunnelAuthMethod {
    /// 界面上的短标签。
    pub fn label(&self) -> &'static str {
        match self {
            Self::KeyFile { passphrase_ref, .. } => {
                if passphrase_ref.is_some() {
                    "私钥 + 口令"
                } else {
                    "私钥"
                }
            }
            Self::Password { .. } => "密码",
            Self::None => "无认证",
        }
    }

    /// 是否需要外部提供口令（决定连接前要不要弹输入框）。
    pub fn needs_secret(&self) -> bool {
        match self {
            Self::KeyFile {
                passphrase_ref,
                dev_passphrase,
                ..
            } => passphrase_ref.is_some() && dev_passphrase.is_none(),
            Self::Password {
                secret_ref,
                dev_secret,
            } => secret_ref.is_some() && dev_secret.is_none(),
            Self::None => false,
        }
    }
}

/// 一跳的认证方式集合（按顺序回退）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelAuthSpec {
    #[serde(default)]
    pub methods: Vec<TunnelAuthMethod>,
}

impl TunnelAuthSpec {
    pub fn new(methods: Vec<TunnelAuthMethod>) -> Self {
        Self { methods }
    }

    /// 单一方式。
    pub fn single(method: TunnelAuthMethod) -> Self {
        Self {
            methods: vec![method],
        }
    }

    /// 界面摘要：`私钥 → 密码`。
    pub fn label(&self) -> String {
        if self.methods.is_empty() {
            return TunnelAuthMethod::None.label().to_string();
        }
        self.methods
            .iter()
            .map(TunnelAuthMethod::label)
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

/// 主机密钥策略。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostKeySpec {
    /// 使用指定的 known_hosts 文件；`path` 为空时用默认路径。
    KnownHosts {
        #[serde(default)]
        path: Option<String>,
    },
    /// 使用 `$HOME/.ssh/known_hosts`（取不到时按未知处理）。
    #[default]
    KnownHostsDefault,
    /// 信任任意主机密钥：**仅供本地测试**，界面不暴露。
    InsecureAcceptAny,
}

impl HostKeySpec {
    pub fn label(&self) -> &'static str {
        match self {
            Self::KnownHosts { .. } => "指定 known_hosts",
            Self::KnownHostsDefault => "默认 known_hosts",
            Self::InsecureAcceptAny => "不校验（仅测试）",
        }
    }

    /// 是否允许在界面上选择（`InsecureAcceptAny` 不暴露）。
    pub fn user_selectable(&self) -> bool {
        !matches!(self, Self::InsecureAcceptAny)
    }
}

/// 一层 SSH 跳板。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelHopSpec {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: TunnelAuthSpec,
    #[serde(default)]
    pub host_keys: HostKeySpec,
}

fn default_ssh_port() -> u16 {
    22
}

impl TunnelHopSpec {
    /// `user@host:port`。
    pub fn endpoint(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

/// SSH 隧道配置：跳板按顺序建链，最后一段从最内层跳到目标库。
///
/// 目标库地址取自连接配置的 `host` / `port`，这里**不重复存一份**，
/// 避免两处地址不一致。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelSpec {
    #[serde(default)]
    pub hops: Vec<TunnelHopSpec>,
}

impl TunnelSpec {
    /// 配置是否为空（没有跳板）。
    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }

    /// 校验配置；返回第一条可读的错误。
    pub fn validate(&self) -> Result<(), String> {
        if self.hops.is_empty() {
            return Err("至少需要配置一层 SSH 跳板".into());
        }
        for (index, hop) in self.hops.iter().enumerate() {
            let layer = index + 1;
            if hop.host.trim().is_empty() {
                return Err(format!("第 {layer} 层缺少主机地址"));
            }
            if hop.port == 0 {
                return Err(format!("第 {layer} 层端口无效"));
            }
            if hop.user.trim().is_empty() {
                return Err(format!("第 {layer} 层缺少用户名"));
            }
            if let HostKeySpec::KnownHosts { path } = &hop.host_keys {
                if let Some(path) = path {
                    if path.trim().is_empty() {
                        return Err(format!("第 {layer} 层的 known_hosts 路径为空"));
                    }
                }
            }
            for method in &hop.auth.methods {
                match method {
                    TunnelAuthMethod::KeyFile { path, .. } => {
                        if path.trim().is_empty() {
                            return Err(format!("第 {layer} 层缺少私钥路径"));
                        }
                    }
                    TunnelAuthMethod::Password {
                        secret_ref,
                        dev_secret,
                    } => {
                        if secret_ref.is_none() && dev_secret.is_none() {
                            return Err(format!("第 {layer} 层缺少口令来源"));
                        }
                    }
                    TunnelAuthMethod::None => {}
                }
            }
        }
        Ok(())
    }

    /// 建链顺序摘要：第几层 → 哪台机器 → 什么认证 → 目标。
    ///
    /// 例如 `1. ops@bastion:22（私钥） → 2. dba@gw:22（密码） → 目标 db.internal:5432`。
    pub fn chain_summary(&self, target: &str) -> String {
        let hops = self.hops_summary();
        if hops.is_empty() {
            format!("目标 {target}")
        } else {
            format!("{hops} → 目标 {target}")
        }
    }

    /// 只包含跳板层的摘要（不含目标），供组合代理时拼接。
    pub fn hops_summary(&self) -> String {
        self.hops
            .iter()
            .enumerate()
            .map(|(index, hop)| {
                format!("{}. {}（{}）", index + 1, hop.endpoint(), hop.auth.label())
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }

    /// 是否还有需要用户补录的口令（连接前要先问）。
    pub fn needs_secret(&self) -> bool {
        self.hops
            .iter()
            .any(|hop| hop.auth.methods.iter().any(TunnelAuthMethod::needs_secret))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hop(host: &str, user: &str) -> TunnelHopSpec {
        TunnelHopSpec {
            host: host.into(),
            port: 22,
            user: user.into(),
            auth: TunnelAuthSpec::single(TunnelAuthMethod::None),
            host_keys: HostKeySpec::KnownHostsDefault,
        }
    }

    fn spec(hops: Vec<TunnelHopSpec>) -> TunnelSpec {
        TunnelSpec { hops }
    }

    #[test]
    fn empty_hops_is_invalid() {
        let err = spec(vec![]).validate().unwrap_err();
        assert!(err.contains("至少"), "{err}");
    }

    #[test]
    fn missing_fields_point_at_the_layer() {
        let mut bad = hop("", "ops");
        bad.port = 22;
        let err = spec(vec![hop("ok", "ops"), bad]).validate().unwrap_err();
        assert!(err.contains("第 2 层"), "{err}");

        let mut no_user = hop("gw", "");
        no_user.user = "  ".into();
        let err = spec(vec![no_user]).validate().unwrap_err();
        assert!(err.contains("用户名"), "{err}");
    }

    #[test]
    fn password_without_source_is_invalid() {
        let mut h = hop("gw", "ops");
        h.auth = TunnelAuthSpec::single(TunnelAuthMethod::Password {
            secret_ref: None,
            dev_secret: None,
        });
        let err = spec(vec![h]).validate().unwrap_err();
        assert!(err.contains("口令"), "{err}");
    }

    #[test]
    fn chain_summary_lists_layers_in_order() {
        let mut first = hop("bastion", "ops");
        first.auth = TunnelAuthSpec::single(TunnelAuthMethod::KeyFile {
            path: "~/.ssh/id_ed25519".into(),
            passphrase_ref: None,
            dev_passphrase: None,
        });
        let mut second = hop("gw", "dba");
        second.port = 2222;
        second.auth = TunnelAuthSpec::single(TunnelAuthMethod::Password {
            secret_ref: Some("k".into()),
            dev_secret: None,
        });
        let summary = spec(vec![first, second]).chain_summary("db.internal:5432");
        assert!(summary.contains("1. ops@bastion（私钥）"), "{summary}");
        assert!(summary.contains("2. dba@gw:2222（密码）"), "{summary}");
        assert!(summary.ends_with("目标 db.internal:5432"), "{summary}");
    }

    #[test]
    fn dev_secrets_never_reach_disk() {
        let mut h = hop("gw", "ops");
        h.auth = TunnelAuthSpec::single(TunnelAuthMethod::Password {
            secret_ref: Some("key".into()),
            dev_secret: Some("hunter2".into()),
        });
        let text = serde_json::to_string(&spec(vec![h])).unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("secret_ref"), "{text}");
    }

    #[test]
    fn missing_secret_is_flagged_for_prompting() {
        let mut h = hop("gw", "ops");
        h.auth = TunnelAuthSpec::single(TunnelAuthMethod::Password {
            secret_ref: Some("key".into()),
            dev_secret: None,
        });
        assert!(spec(vec![h]).needs_secret());

        let mut with_value = hop("gw", "ops");
        with_value.auth = TunnelAuthSpec::single(TunnelAuthMethod::Password {
            secret_ref: Some("key".into()),
            dev_secret: Some("x".into()),
        });
        assert!(!spec(vec![with_value]).needs_secret());
    }

    #[test]
    fn old_profiles_without_tunnel_still_deserialize() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default)]
            tunnel: Option<TunnelSpec>,
        }
        let holder: Holder = serde_json::from_str("{}").unwrap();
        assert!(holder.tunnel.is_none());
    }
}
