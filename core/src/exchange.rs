//! F-90 加密配置交换与他端导入。
//!
//! 导出：选中连接 → 剥离密钥引用（`secret_ref` / `passphrase_ref` / `dev_*`）→
//! 可选 SSH 隧道 → JSON 负载；带口令时用 Argon2id 派生 256 位密钥、AES-256-GCM 加密。
//! 导入：先预览（按「名称 + 主机 + 端口」去重）→ 用户勾选 → 生成草稿连接，
//! **口令一概不导入**，导入后需重新输入。
//!
//! 安全口径（需求文档 19.6 / 附录 A）：
//! - 明文导出必须由调用方做二次风险确认；
//! - KDF 参数写在文件里，导入时按文件参数派生（不信任默认值），并做上限校验防 DoS；
//! - 从其他客户端只生成草稿：不导入驱动、密钥、代理与口令。

use crate::profile::ConnectionProfile;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rustdb_db_api::{DbKind, ProxySpec, TunnelAuthMethod, TunnelAuthSpec, TunnelSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// 文件格式标识（同时也是 GCM 附加认证数据的一部分）。
pub const EXCHANGE_FORMAT: &str = "rustdb-config-exchange";
pub const EXCHANGE_VERSION: u32 = 1;

const KDF_ID: &str = "argon2id";
const CIPHER_ID: &str = "aes-256-gcm";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// 默认 KDF 成本：19 MiB / 2 次 / 单线程（Argon2 推荐基线）。
const DEFAULT_M_COST: u32 = 19 * 1024;
const DEFAULT_T_COST: u32 = 2;
const DEFAULT_P_COST: u32 = 1;

/// 导入时接受的 KDF 参数上限，防止恶意文件拖垮本机。
const MAX_M_COST: u32 = 256 * 1024;
const MAX_T_COST: u32 = 16;
const MAX_P_COST: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    /// 不是本应用导出的交换文件。
    NotBundle,
    /// 文件版本高于当前应用支持的版本。
    UnsupportedVersion(u32),
    /// 结构损坏 / 字段非法。
    Malformed(String),
    /// 文件已加密但没有提供口令。
    PassphraseRequired,
    /// 口令错误（GCM 校验失败）。
    WrongPassphrase,
    /// 没有可导入的连接。
    Empty,
}

impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBundle => write!(f, "不是 RustDb 连接配置文件"),
            Self::UnsupportedVersion(version) => {
                write!(f, "文件版本 {version} 高于当前支持的 {EXCHANGE_VERSION}")
            }
            Self::Malformed(detail) => write!(f, "配置文件内容无效：{detail}"),
            Self::PassphraseRequired => write!(f, "文件已加密，需要输入口令"),
            Self::WrongPassphrase => write!(f, "口令错误或文件已损坏"),
            Self::Empty => write!(f, "文件里没有可导入的连接"),
        }
    }
}

impl std::error::Error for ExchangeError {}

/// 交换文件里的连接配置：只含**非密钥**字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeProfile {
    pub name: String,
    #[serde(default)]
    pub folder: Option<String>,
    pub driver: DbKind,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub ssl_mode: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub production: bool,
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(default)]
    pub connect_timeout_ms: Option<u64>,
    #[serde(default)]
    pub tunnel: Option<TunnelSpec>,
    /// F-91a：代理配置（导出时同样剥掉口令引用）。
    #[serde(default)]
    pub proxy: Option<ProxySpec>,
    /// F-91a：本地转发是否对局域网开放。
    #[serde(default)]
    pub forward_lan: bool,
}

impl ExchangeProfile {
    /// 从当前配置生成导出用的副本；`include_tunnels` 为假时丢弃 SSH 隧道。
    pub fn from_profile(profile: &ConnectionProfile, include_tunnels: bool) -> Self {
        Self {
            name: profile.name.clone(),
            folder: profile.folder.clone(),
            driver: profile.driver,
            host: profile.host.clone(),
            port: profile.port,
            database: profile.database.clone(),
            username: profile.username.clone(),
            ssl_mode: profile.ssl_mode.clone(),
            params: profile.params.clone(),
            read_only: profile.read_only,
            production: profile.production,
            color: profile.color,
            connect_timeout_ms: Some(profile.connect_timeout_ms),
            tunnel: if include_tunnels {
                profile.tunnel.as_ref().map(sanitize_tunnel)
            } else {
                None
            },
            proxy: if include_tunnels {
                profile.proxy.as_ref().map(sanitize_proxy)
            } else {
                None
            },
            forward_lan: profile.forward_lan,
        }
    }

    /// 生成草稿连接：口令 / 密钥引用一律清空，`id` 由调用方保证唯一。
    pub fn to_draft(&self, id: String) -> ConnectionProfile {
        ConnectionProfile {
            id,
            name: self.name.clone(),
            folder: self.folder.clone(),
            driver: self.driver,
            host: self.host.clone(),
            port: self.port,
            database: self.database.clone(),
            username: self.username.clone(),
            secret_ref: None,
            dev_secret: None,
            ssl_mode: self.ssl_mode.clone(),
            params: self.params.clone(),
            read_only: self.read_only,
            production: self.production,
            tunnel: self.tunnel.clone(),
            proxy: self.proxy.clone(),
            forward_lan: self.forward_lan,
            connect_timeout_ms: self.connect_timeout_ms.unwrap_or(10_000),
            color: self.color,
            last_used_at: None,
        }
    }

    /// 去重键：名称（忽略大小写与首尾空白）+ 主机 / SQLite 文件 + 端口。
    pub fn conflict_key(&self) -> (String, String, u16) {
        let name = self.name.trim().to_lowercase();
        let endpoint = match self.driver {
            DbKind::Sqlite => self
                .database
                .clone()
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
            _ => self.host.clone().unwrap_or_default().trim().to_lowercase(),
        };
        let port = self
            .port
            .or_else(|| self.driver.default_port())
            .unwrap_or(0);
        (name, endpoint, port)
    }

    pub fn endpoint_label(&self) -> String {
        match self.driver {
            DbKind::Sqlite => self
                .database
                .clone()
                .unwrap_or_else(|| "(未选择文件)".into()),
            _ => format!(
                "{}:{}",
                self.host.clone().unwrap_or_else(|| "localhost".into()),
                self.port.or(self.driver.default_port()).unwrap_or(0)
            ),
        }
    }
}

/// 剥掉隧道里的口令引用：密钥库键只在原机器有效。
fn sanitize_tunnel(spec: &TunnelSpec) -> TunnelSpec {
    let hops = spec
        .hops
        .iter()
        .map(|hop| {
            let mut hop = hop.clone();
            hop.auth = TunnelAuthSpec::new(
                hop.auth
                    .methods
                    .iter()
                    .map(|method| match method {
                        TunnelAuthMethod::KeyFile { path, .. } => TunnelAuthMethod::KeyFile {
                            path: path.clone(),
                            passphrase_ref: None,
                            dev_passphrase: None,
                        },
                        TunnelAuthMethod::Password { .. } => TunnelAuthMethod::Password {
                            secret_ref: None,
                            dev_secret: None,
                        },
                        TunnelAuthMethod::None => TunnelAuthMethod::None,
                    })
                    .collect(),
            );
            hop
        })
        .collect();
    TunnelSpec { hops }
}

/// 剥掉代理口令引用（用户名 / 端点 / 顺序保留）。
fn sanitize_proxy(spec: &ProxySpec) -> ProxySpec {
    let mut proxy = spec.clone();
    proxy.secret_ref = None;
    proxy.dev_secret = None;
    proxy
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ExchangePayload {
    #[serde(default)]
    profiles: Vec<ExchangeProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedSection {
    kdf: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: String,
    cipher: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExchangeBundle {
    format: String,
    version: u32,
    #[serde(default)]
    exported_at: Option<i64>,
    /// 连接条数提示（加密时也对外可见，便于预览页先显示数量）。
    #[serde(default)]
    profiles: usize,
    #[serde(default)]
    encrypted: Option<EncryptedSection>,
    #[serde(default)]
    payload: Option<ExchangePayload>,
}

/// 预览条目：`conflict` 表示当前已存在同名 + 同主机 + 同端口的连接。
#[derive(Debug, Clone)]
pub struct PreviewProfile {
    pub profile: ExchangeProfile,
    pub conflict: bool,
}

#[derive(Debug, Clone)]
pub struct ExchangePreview {
    pub profiles: Vec<PreviewProfile>,
    pub encrypted: bool,
    pub drafts: bool,
    pub exported_at: Option<i64>,
}

impl ExchangePreview {
    pub fn importable(&self) -> usize {
        self.profiles.iter().filter(|row| !row.conflict).count()
    }
}

/// 导出选中的连接。`passphrase = None` 表示明文（调用方必须先做风险确认）。
pub fn export(
    profiles: &[ConnectionProfile],
    passphrase: Option<&str>,
    include_tunnels: bool,
) -> Result<String, ExchangeError> {
    if profiles.is_empty() {
        return Err(ExchangeError::Empty);
    }
    let payload = ExchangePayload {
        profiles: profiles
            .iter()
            .map(|profile| ExchangeProfile::from_profile(profile, include_tunnels))
            .collect(),
    };
    let count = payload.profiles.len();
    let mut bundle = ExchangeBundle {
        format: EXCHANGE_FORMAT.into(),
        version: EXCHANGE_VERSION,
        exported_at: Some(chrono::Utc::now().timestamp_millis()),
        profiles: count,
        encrypted: None,
        payload: None,
    };
    match passphrase {
        Some(passphrase) => {
            let plain = serde_json::to_vec(&payload).map_err(malformed)?;
            bundle.encrypted = Some(encrypt(&plain, passphrase)?);
        }
        None => bundle.payload = Some(payload),
    }
    serde_json::to_string_pretty(&bundle).map_err(malformed)
}

/// 解析交换文件（必要时解密）并与当前连接做去重预览。
pub fn preview_bundle(
    text: &str,
    passphrase: Option<&str>,
    existing: &[ConnectionProfile],
) -> Result<ExchangePreview, ExchangeError> {
    let bundle = parse_bundle(text)?;
    let (payload, encrypted) = match (&bundle.encrypted, &bundle.payload) {
        (Some(section), _) => {
            let passphrase = passphrase.ok_or(ExchangeError::PassphraseRequired)?;
            let plain = decrypt(section, passphrase)?;
            let payload: ExchangePayload = serde_json::from_slice(&plain).map_err(malformed)?;
            (payload, true)
        }
        (None, Some(payload)) => (payload.clone(), false),
        (None, None) => return Err(ExchangeError::Malformed("缺少连接数据".into())),
    };
    if payload.profiles.is_empty() {
        return Err(ExchangeError::Empty);
    }
    let existing_keys: Vec<_> = existing.iter().map(profile_key).collect();
    let profiles = payload
        .profiles
        .into_iter()
        .map(|profile| {
            let conflict = existing_keys.contains(&profile.conflict_key());
            PreviewProfile { profile, conflict }
        })
        .collect();
    Ok(ExchangePreview {
        profiles,
        encrypted,
        drafts: false,
        exported_at: bundle.exported_at,
    })
}

/// 从常见客户端配置生成草稿：DBeaver `data-sources.json`、JDBC / 连接 URI / Oracle thin。
pub fn drafts_from_client_config(text: &str) -> Result<ExchangePreview, ExchangeError> {
    let profiles = parse_dbeaver(text)
        .or_else(|| {
            let parsed = parse_connection_lines(text);
            (!parsed.is_empty()).then_some(parsed)
        })
        .ok_or_else(|| {
            ExchangeError::Malformed(
                "没有识别到 DBeaver data-sources.json 或 JDBC / 连接 URI".into(),
            )
        })?;
    Ok(ExchangePreview {
        profiles: profiles
            .into_iter()
            .map(|profile| PreviewProfile {
                profile,
                conflict: false,
            })
            .collect(),
        encrypted: false,
        drafts: true,
        exported_at: None,
    })
}

/// 按勾选生成草稿连接。冲突项（已存在同名 + 同主机 + 同端口）始终跳过；
/// `existing` 用于生成不重复的 id。
pub fn profiles_from_preview(
    preview: &ExchangePreview,
    selected: &[bool],
    existing: &[ConnectionProfile],
) -> Vec<ConnectionProfile> {
    let mut ids: Vec<String> = existing.iter().map(|profile| profile.id.clone()).collect();
    let existing_keys: Vec<_> = existing.iter().map(profile_key).collect();
    let mut profiles = Vec::new();
    for (index, row) in preview.profiles.iter().enumerate() {
        if !selected.get(index).copied().unwrap_or(false) || row.conflict {
            continue;
        }
        if existing_keys.contains(&row.profile.conflict_key()) {
            continue;
        }
        let id = unique_id(&row.profile.name, &ids);
        ids.push(id.clone());
        profiles.push(row.profile.to_draft(id));
    }
    profiles
}

fn profile_key(profile: &ConnectionProfile) -> (String, String, u16) {
    ExchangeProfile::from_profile(profile, false).conflict_key()
}

fn unique_id(name: &str, taken: &[String]) -> String {
    let base: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let base = if base.is_empty() {
        "imported".to_string()
    } else {
        base
    };
    if !taken.iter().any(|id| id == &base) {
        return base;
    }
    let mut index = 2;
    loop {
        let candidate = format!("{base}-{index}");
        if !taken.iter().any(|id| id == &candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn parse_bundle(text: &str) -> Result<ExchangeBundle, ExchangeError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| ExchangeError::NotBundle)?;
    let format = value.get("format").and_then(serde_json::Value::as_str);
    if format != Some(EXCHANGE_FORMAT) {
        return Err(ExchangeError::NotBundle);
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    if version == 0 {
        return Err(ExchangeError::Malformed("缺少格式版本".into()));
    }
    if version > EXCHANGE_VERSION {
        return Err(ExchangeError::UnsupportedVersion(version));
    }
    serde_json::from_value(value).map_err(malformed)
}

fn encrypt(plain: &[u8], passphrase: &str) -> Result<EncryptedSection, ExchangeError> {
    encrypt_with(
        plain,
        passphrase,
        DEFAULT_M_COST,
        DEFAULT_T_COST,
        DEFAULT_P_COST,
    )
}

fn encrypt_with(
    plain: &[u8],
    passphrase: &str,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<EncryptedSection, ExchangeError> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|error| malformed(format!("随机数失败：{error}")))?;
    getrandom::fill(&mut nonce_bytes).map_err(|error| malformed(format!("随机数失败：{error}")))?;

    let key = derive_key(passphrase, &salt, m_cost, t_cost, p_cost)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| malformed("AES 密钥长度无效"))?;
    let nonce =
        Nonce::try_from(nonce_bytes.as_slice()).map_err(|_| malformed("AES nonce 长度无效"))?;
    let ciphertext = cipher
        .encrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: plain,
                aad: aad(),
            },
        )
        .map_err(|_| malformed("加密失败"))?;
    Ok(EncryptedSection {
        kdf: KDF_ID.into(),
        m_cost,
        t_cost,
        p_cost,
        salt: B64.encode(salt),
        cipher: CIPHER_ID.into(),
        nonce: B64.encode(nonce_bytes),
        ciphertext: B64.encode(ciphertext),
    })
}

fn decrypt(section: &EncryptedSection, passphrase: &str) -> Result<Vec<u8>, ExchangeError> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

    if section.kdf != KDF_ID {
        return Err(ExchangeError::Malformed(format!(
            "不支持的 KDF：{}",
            section.kdf
        )));
    }
    if section.cipher != CIPHER_ID {
        return Err(ExchangeError::Malformed(format!(
            "不支持的加密算法：{}",
            section.cipher
        )));
    }
    if section.m_cost < 8
        || section.m_cost > MAX_M_COST
        || section.t_cost == 0
        || section.t_cost > MAX_T_COST
        || section.p_cost == 0
        || section.p_cost > MAX_P_COST
    {
        return Err(ExchangeError::Malformed("KDF 参数超出支持范围".into()));
    }
    let salt = B64
        .decode(section.salt.as_bytes())
        .map_err(|_| ExchangeError::Malformed("salt 不是有效 Base64".into()))?;
    let nonce_bytes = B64
        .decode(section.nonce.as_bytes())
        .map_err(|_| ExchangeError::Malformed("nonce 不是有效 Base64".into()))?;
    let ciphertext = B64
        .decode(section.ciphertext.as_bytes())
        .map_err(|_| ExchangeError::Malformed("密文不是有效 Base64".into()))?;
    if salt.len() < 8 || nonce_bytes.len() != NONCE_LEN {
        return Err(ExchangeError::Malformed("salt / nonce 长度无效".into()));
    }
    let key = derive_key(
        passphrase,
        &salt,
        section.m_cost,
        section.t_cost,
        section.p_cost,
    )?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| malformed("AES 密钥长度无效"))?;
    let nonce = Nonce::try_from(nonce_bytes.as_slice())
        .map_err(|_| ExchangeError::Malformed("nonce 长度无效".into()))?;
    cipher
        .decrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: &ciphertext,
                aad: aad(),
            },
        )
        .map_err(|_| ExchangeError::WrongPassphrase)
}

fn aad() -> &'static [u8] {
    b"rustdb-config-exchange:1"
}

fn derive_key(
    passphrase: &str,
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<[u8; KEY_LEN], ExchangeError> {
    let params = argon2::Params::new(m_cost, t_cost, p_cost, Some(KEY_LEN))
        .map_err(|error| malformed(format!("KDF 参数无效：{error}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|error| malformed(format!("口令派生失败：{error}")))?;
    Ok(key)
}

fn malformed(error: impl fmt::Display) -> ExchangeError {
    ExchangeError::Malformed(error.to_string())
}

// ---------------------------------------------------------------------------
// 从其他客户端生成草稿
// ---------------------------------------------------------------------------

fn parse_dbeaver(text: &str) -> Option<Vec<ExchangeProfile>> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    let connections = root
        .get("connections")
        .and_then(serde_json::Value::as_object)?;
    let mut profiles = Vec::new();
    for value in connections.values() {
        let config = value.get("configuration");
        let provider = value
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("driver").and_then(serde_json::Value::as_str))
            .unwrap_or_default();
        let Some(driver) = dbeaver_driver(provider) else {
            continue;
        };
        let name = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                config
                    .and_then(|c| c.get("name"))
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} 导入", driver.display_name()));
        let get = |key: &str| {
            config
                .and_then(|c| c.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let port = get("port")
            .and_then(|value| value.parse::<u16>().ok())
            .or_else(|| {
                config
                    .and_then(|c| c.get("port"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as u16)
            });
        let host = get("host");
        let database = get("database").or_else(|| get("databaseName"));
        let username = get("user").or_else(|| get("username"));
        profiles.push(ExchangeProfile {
            name,
            folder: None,
            driver,
            host,
            port,
            database,
            username,
            ssl_mode: None,
            params: BTreeMap::new(),
            read_only: false,
            production: false,
            color: None,
            connect_timeout_ms: Some(10_000),
            tunnel: None,
            proxy: None,
            forward_lan: false,
        });
    }
    // 文件里的键顺序不稳定，按名称排序保证预览顺序可预期。
    profiles.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    (!profiles.is_empty()).then_some(profiles)
}

fn dbeaver_driver(provider: &str) -> Option<DbKind> {
    let provider = provider.to_ascii_lowercase();
    match provider.as_str() {
        "postgresql" | "postgres" | "postgres-jdbc" => Some(DbKind::Postgres),
        "mysql" => Some(DbKind::Mysql),
        "mariadb" => Some(DbKind::Mariadb),
        "sqlserver" | "mssql" => Some(DbKind::Mssql),
        "oracle" => Some(DbKind::Oracle),
        "sqlite" => Some(DbKind::Sqlite),
        _ => None,
    }
}

fn parse_connection_lines(text: &str) -> Vec<ExchangeProfile> {
    text.lines().filter_map(parse_connection_line).collect()
}

fn parse_connection_line(line: &str) -> Option<ExchangeProfile> {
    let mut line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with("--") {
        return None;
    }
    if let Some(rest) = line.strip_prefix("jdbc:") {
        line = rest;
    }
    if let Some(rest) = line.strip_prefix("oracle:thin:@") {
        return parse_oracle_thin(rest);
    }
    if let Some(path) = line.strip_prefix("sqlite:") {
        let path = path.trim_start_matches("//").trim_end_matches('/');
        let path = path.split('?').next().unwrap_or(path);
        if path.is_empty() {
            return None;
        }
        let database = path.rsplit('/').next().unwrap_or(path);
        return Some(ExchangeProfile {
            name: database.to_string(),
            folder: None,
            driver: DbKind::Sqlite,
            host: None,
            port: None,
            database: Some(path.to_string()),
            username: None,
            ssl_mode: None,
            params: BTreeMap::new(),
            read_only: false,
            production: false,
            color: None,
            connect_timeout_ms: Some(10_000),
            tunnel: None,
            proxy: None,
            forward_lan: false,
        });
    }
    let (scheme, rest) = line.split_once("://")?;
    let driver = match scheme.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" => DbKind::Postgres,
        "mysql" => DbKind::Mysql,
        "mariadb" => DbKind::Mariadb,
        "sqlserver" | "mssql" => DbKind::Mssql,
        "sqlite" | "file" => DbKind::Sqlite,
        _ => return None,
    };
    if driver == DbKind::Sqlite {
        let path = rest.split('?').next().unwrap_or(rest).trim_end_matches('/');
        if path.is_empty() {
            return None;
        }
        let database = path.rsplit('/').next().unwrap_or(path);
        return Some(ExchangeProfile {
            name: database.to_string(),
            folder: None,
            driver,
            host: None,
            port: None,
            database: Some(path.to_string()),
            username: None,
            ssl_mode: None,
            params: BTreeMap::new(),
            read_only: false,
            production: false,
            color: None,
            connect_timeout_ms: Some(10_000),
            tunnel: None,
            proxy: None,
            forward_lan: false,
        });
    }
    let (authority, tail) = rest
        .split_once('/')
        .map(|(authority, tail)| (authority, Some(tail)))
        .unwrap_or((rest, None));
    let (authority, extra) = authority
        .split_once(';')
        .map(|(authority, extra)| (authority, Some(extra)))
        .unwrap_or((authority, None));
    let (userinfo, hostport) = authority
        .rsplit_once('@')
        .map(|(userinfo, hostport)| (Some(userinfo), hostport))
        .unwrap_or((None, authority));
    let (host, port) = split_host_port(hostport);
    let host = host.filter(|value| !value.is_empty())?;
    let user = userinfo
        .and_then(|userinfo| {
            userinfo
                .split_once(':')
                .map(|(user, _)| user)
                .or(Some(userinfo))
        })
        .filter(|user| !user.is_empty());

    let mut database = tail.map(|tail| tail.split('?').next().unwrap_or(tail).to_string());
    let mut params = BTreeMap::new();
    if let Some(tail) = tail {
        if let Some((_, query)) = tail.split_once('?') {
            for pair in query.split('&') {
                if let Some((key, value)) = pair.split_once('=') {
                    if key.eq_ignore_ascii_case("sslmode") {
                        params.insert("sslmode".into(), value.to_string());
                    }
                }
            }
        }
    }
    let mut username = user.map(str::to_string);
    if let Some(extra) = extra {
        for pair in extra.split(';') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key.trim().to_ascii_lowercase().as_str() {
                "databasename" | "database" | "initial catalog" => {
                    database = Some(value.trim().to_string());
                }
                "user" | "user id" | "uid" => username = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    let database = database.filter(|value| !value.is_empty());
    let ssl_mode = params.remove("sslmode");
    let name = database
        .clone()
        .or_else(|| Some(host.clone()))
        .unwrap_or_else(|| "导入连接".into());
    Some(ExchangeProfile {
        name,
        folder: None,
        driver,
        host: Some(host),
        port,
        database,
        username,
        ssl_mode,
        params,
        read_only: false,
        production: false,
        color: None,
        connect_timeout_ms: Some(10_000),
        tunnel: None,
        proxy: None,
        forward_lan: false,
    })
}

fn parse_oracle_thin(rest: &str) -> Option<ExchangeProfile> {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.split('?').next().unwrap_or(rest);
    let (authority, database) = match rest.split_once('/') {
        Some((authority, database)) => (authority, Some(database.to_string())),
        None => (rest, None),
    };
    let (host, port) = split_host_port(authority);
    let host = host.filter(|value| !value.is_empty())?;
    Some(ExchangeProfile {
        name: database.clone().unwrap_or_else(|| host.clone()),
        folder: None,
        driver: DbKind::Oracle,
        host: Some(host),
        port,
        database,
        username: None,
        ssl_mode: None,
        params: BTreeMap::new(),
        read_only: false,
        production: false,
        color: None,
        connect_timeout_ms: Some(10_000),
        tunnel: None,
        proxy: None,
        forward_lan: false,
    })
}

/// 拆 `host:port` 或 `[::1]:5432`；端口非法时保留主机、丢弃端口。
fn split_host_port(value: &str) -> (Option<String>, Option<u16>) {
    if let Some(rest) = value.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|port| port.parse::<u16>().ok());
            return (Some(host.to_string()), port);
        }
    }
    match value.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (Some(host.to_string()), Some(port)),
            Err(_) => (Some(value.to_string()), None),
        },
        None => (Some(value.to_string()), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, host: &str, port: u16) -> ConnectionProfile {
        let mut profile = ConnectionProfile::demo_sqlite();
        profile.id = name.to_lowercase();
        profile.name = name.into();
        profile.driver = DbKind::Postgres;
        profile.host = Some(host.into());
        profile.port = Some(port);
        profile.database = Some("shop".into());
        profile.username = Some("postgres".into());
        profile.secret_ref = Some("keyring-ref".into());
        profile.dev_secret = Some("hunter2".into());
        profile
    }

    #[test]
    fn encrypted_round_trip_and_wrong_passphrase() {
        let mut original = profile("生产库", "db.internal", 5432);
        original.tunnel = Some(TunnelSpec {
            hops: vec![rustdb_db_api::TunnelHopSpec {
                host: "bastion".into(),
                port: 22,
                user: "ops".into(),
                auth: TunnelAuthSpec::single(TunnelAuthMethod::Password {
                    secret_ref: Some("ssh-ref".into()),
                    dev_secret: Some("ssh-pass".into()),
                }),
                host_keys: rustdb_db_api::HostKeySpec::KnownHostsDefault,
            }],
        });
        let text = export(&[original], Some("correct horse"), true).expect("导出");
        assert!(!text.contains("hunter2"), "数据库口令不得外泄");
        assert!(!text.contains("ssh-pass"), "隧道口令不得外泄");
        assert!(!text.contains("keyring-ref"), "密钥库引用不得外泄");
        assert!(!text.contains("db.internal"), "加密后不应出现明文主机");

        let preview = preview_bundle(&text, Some("correct horse"), &[]).expect("解密");
        assert!(preview.encrypted);
        assert_eq!(preview.profiles.len(), 1);
        let row = &preview.profiles[0];
        assert_eq!(row.profile.host.as_deref(), Some("db.internal"));
        let hop = &row.profile.tunnel.as_ref().expect("隧道应保留").hops[0];
        match &hop.auth.methods[0] {
            TunnelAuthMethod::Password {
                secret_ref,
                dev_secret,
            } => {
                assert!(secret_ref.is_none(), "口令引用要清空");
                assert!(dev_secret.is_none(), "内存口令不得写入");
            }
            other => panic!("认证方式不对：{other:?}"),
        }

        assert_eq!(
            preview_bundle(&text, Some("wrong"), &[]).unwrap_err(),
            ExchangeError::WrongPassphrase
        );
        assert_eq!(
            preview_bundle(&text, None, &[]).unwrap_err(),
            ExchangeError::PassphraseRequired
        );
    }

    #[test]
    fn plaintext_export_omits_tunnels_when_disabled() {
        let mut original = profile("本地", "127.0.0.1", 5432);
        original.tunnel = Some(TunnelSpec {
            hops: vec![rustdb_db_api::TunnelHopSpec {
                host: "bastion".into(),
                port: 22,
                user: "ops".into(),
                auth: TunnelAuthSpec::default(),
                host_keys: rustdb_db_api::HostKeySpec::KnownHostsDefault,
            }],
        });
        let text = export(&[original], None, false).expect("明文导出");
        assert!(!text.contains("bastion"));
        let preview = preview_bundle(&text, Some("ignored"), &[]).expect("明文不需要口令");
        assert!(!preview.encrypted);
        assert!(preview.profiles[0].profile.tunnel.is_none());
    }

    #[test]
    fn preview_marks_conflicts_by_name_host_port() {
        let existing = vec![{
            let mut profile = profile("生产库", "DB.Internal", 5432);
            profile.id = "prod".into();
            profile
        }];
        let mut same = profile("生产库", " db.internal ", 5432);
        same.database = Some("other".into());
        let mut different_port = profile("生产库", "db.internal", 5433);
        different_port.name = "生产库-副本".into();
        let text = export(&[same, different_port], None, false).expect("导出");
        let preview = preview_bundle(&text, None, &existing).expect("预览");
        assert!(preview.profiles[0].conflict, "同名同主机同端口应判冲突");
        assert!(!preview.profiles[1].conflict);
        assert_eq!(preview.importable(), 1);
    }

    #[test]
    fn drafts_skip_conflicts_and_get_unique_ids() {
        let mut existing = profile("生产库", "db.internal", 5432);
        existing.id = "prod".into();
        let mut duplicate = profile("生产库", "db.internal", 5432);
        duplicate.id = "x".into();
        let mut fresh = profile("测试库", "10.0.0.2", 5432);
        fresh.id = "y".into();
        let text = export(&[duplicate, fresh], None, false).expect("导出");
        let preview = preview_bundle(&text, None, &[existing.clone()]).expect("预览");
        let profiles = profiles_from_preview(&preview, &[true, true], &[existing]);
        assert_eq!(profiles.len(), 1, "冲突项应跳过");
        assert!(profiles[0].secret_ref.is_none());
        assert!(profiles[0].dev_secret.is_none());
        assert_eq!(profiles[0].name, "测试库");
        assert_ne!(profiles[0].id, "生产库", "id 需转成 ASCII 安全字符");
    }

    #[test]
    fn dbeaver_data_sources_become_drafts() {
        let text = r#"{
            "connections": {
                "pg-1": {
                    "provider": "postgresql",
                    "name": "Analytics",
                    "configuration": {"host": "analytics.internal", "port": "5432", "database": "warehouse", "user": "reader"}
                },
                "my-1": {
                    "provider": "mysql",
                    "name": "Legacy",
                    "configuration": {"host": "10.0.0.9", "port": 3306, "database": "legacy"}
                },
                "unknown": {"provider": "clickhouse", "name": "CH", "configuration": {}}
            }
        }"#;
        let preview = drafts_from_client_config(text).expect("解析 DBeaver");
        assert!(preview.drafts);
        assert_eq!(preview.profiles.len(), 2, "未知驱动应跳过");
        let first = &preview.profiles[0].profile;
        assert_eq!(first.name, "Analytics");
        assert_eq!(first.driver, DbKind::Postgres);
        assert_eq!(first.host.as_deref(), Some("analytics.internal"));
        assert_eq!(first.port, Some(5432));
        assert_eq!(first.username.as_deref(), Some("reader"));
        assert_eq!(preview.profiles[1].profile.driver, DbKind::Mysql);
        assert_eq!(preview.profiles[1].profile.port, Some(3306));
    }

    #[test]
    fn connection_uris_become_drafts_without_passwords() {
        let text = "\
            postgresql://reader:s3cret@db.internal:5432/warehouse?sslmode=require\n\
            jdbc:mysql://10.0.0.9:3306/legacy\n\
            jdbc:sqlserver://srva\\inst:1433;databaseName=erp;user=sa;password=topsecret\n\
            oracle:thin:@oracle.internal:1521/ORCLPDB1\n\
            jdbc:sqlite:/data/local.db\n\
            not a connection\n";
        let preview = drafts_from_client_config(text).expect("解析 URI");
        assert_eq!(preview.profiles.len(), 5);
        let pg = &preview.profiles[0].profile;
        assert_eq!(pg.driver, DbKind::Postgres);
        assert_eq!(pg.host.as_deref(), Some("db.internal"));
        assert_eq!(pg.port, Some(5432));
        assert_eq!(pg.username.as_deref(), Some("reader"));
        assert_eq!(pg.database.as_deref(), Some("warehouse"));
        assert_eq!(pg.ssl_mode.as_deref(), Some("require"));
        assert_eq!(preview.profiles[1].profile.name, "legacy");
        let mssql = &preview.profiles[2].profile;
        assert_eq!(mssql.database.as_deref(), Some("erp"));
        assert_eq!(mssql.username.as_deref(), Some("sa"));
        assert_eq!(preview.profiles[3].profile.driver, DbKind::Oracle);
        assert_eq!(
            preview.profiles[3].profile.database.as_deref(),
            Some("ORCLPDB1")
        );
        assert_eq!(preview.profiles[4].profile.driver, DbKind::Sqlite);
        let serialized = serde_json::to_string(
            &preview
                .profiles
                .iter()
                .map(|row| &row.profile)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(!serialized.contains("s3cret"));
        assert!(!serialized.contains("topsecret"));
    }

    #[test]
    fn malformed_and_foreign_files_are_rejected() {
        assert_eq!(
            preview_bundle("{ not json", None, &[]).unwrap_err(),
            ExchangeError::NotBundle
        );
        assert_eq!(
            preview_bundle(r#"{"format":"other","version":1}"#, None, &[]).unwrap_err(),
            ExchangeError::NotBundle
        );
        let future =
            format!(r#"{{"format":"{EXCHANGE_FORMAT}","version":99,"payload":{{"profiles":[]}}}}"#);
        assert_eq!(
            preview_bundle(&future, None, &[]).unwrap_err(),
            ExchangeError::UnsupportedVersion(99)
        );
        let empty =
            format!(r#"{{"format":"{EXCHANGE_FORMAT}","version":1,"payload":{{"profiles":[]}}}}"#);
        assert_eq!(
            preview_bundle(&empty, None, &[]).unwrap_err(),
            ExchangeError::Empty
        );
        assert_eq!(export(&[], None, false).unwrap_err(), ExchangeError::Empty);
    }

    #[test]
    fn only_the_selected_rows_are_imported() {
        let a = profile("A", "a.internal", 5432);
        let b = profile("B", "b.internal", 5432);
        let text = export(&[a, b], None, false).expect("导出");
        let preview = preview_bundle(&text, None, &[]).expect("预览");
        let profiles = profiles_from_preview(&preview, &[false, true], &[]);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "B");
        let none = profiles_from_preview(&preview, &[false, false], &[]);
        assert!(none.is_empty());
    }
}
