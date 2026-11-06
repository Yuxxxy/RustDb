//! F-94：多模型配置（命名配置、预设、发现、连通性、Markdown 导出）。
//!
//! 边界（需求 19.5）：
//! * 配置只存**端点、模型名与密钥的引用**，密钥本身只留在内存 / 环境变量；
//! * 发现与连通性测试只发最小请求（`GET /models` 或一次最小 CLI 调用）；
//! * 本模块不执行 SQL，也不把模型输出当执行授权。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const AI_CONFIG_VERSION: u32 = 1;
pub const DEFAULT_MAX_TURNS: u32 = 8;
pub const DEFAULT_RETRIES: u32 = 1;
pub const MIN_CONTEXT_WINDOW: u32 = 1_024;
pub const MAX_CONTEXT_WINDOW: u32 = 2_000_000;
pub const MAX_TURNS_LIMIT: u32 = 32;
pub const MAX_RETRY_LIMIT: u32 = 5;
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// 配置来源。`Cli` 表示本地 CLI Agent（无 HTTP 端点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiKind {
    /// 云服务商的官方预设。
    Cloud,
    /// 任意 OpenAI 兼容端点。
    Compatible,
    /// 本机模型服务（Ollama / LM Studio 等）。
    Local,
    /// 本地 CLI Agent。
    Cli,
}

impl AiKind {
    pub const ALL: [AiKind; 4] = [Self::Cloud, Self::Compatible, Self::Local, Self::Cli];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cloud => "云服务",
            Self::Compatible => "兼容端点",
            Self::Local => "本地模型",
            Self::Cli => "本地 CLI",
        }
    }

    pub fn needs_url(self) -> bool {
        !matches!(self, Self::Cli)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// 自定义请求头（名称放 `auth_header`）
    Header,
    /// 无需认证（本地模型常见）
    None,
}

impl AuthKind {
    pub const ALL: [AuthKind; 3] = [Self::Bearer, Self::Header, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Bearer => "Bearer",
            Self::Header => "自定义头",
            Self::None => "无认证",
        }
    }
}

/// 一套命名模型配置。
///
/// `api_key_ref` 只是**环境变量名 / 密钥库键名**，不是密钥本身。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiProfile {
    pub id: String,
    pub name: String,
    pub kind: AiKind,
    pub base_url: String,
    pub model: String,
    pub auth: AuthKind,
    pub auth_header: Option<String>,
    pub api_key_ref: Option<String>,
    /// 额外请求头（值里可能出现租户 ID，但不放密钥）。
    pub headers: Vec<(String, String)>,
    pub proxy: Option<String>,
    pub context_window: Option<u32>,
    pub max_turns: u32,
    pub retries: u32,
    pub temperature: f32,
    /// 全局指令：拼进每次请求的系统提示。
    pub global_instruction: Option<String>,
    /// 最近一次模板快照（模型、上下文窗口、全局指令的定型文本）。
    pub template_snapshot: Option<String>,
    /// `AiKind::Cli` 的命令行（含参数）。
    pub cli_command: Option<String>,
}

impl Default for AiProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: AiKind::Compatible,
            base_url: String::new(),
            model: String::new(),
            auth: AuthKind::Bearer,
            auth_header: None,
            api_key_ref: Some("RUSTDB_AI_KEY".into()),
            headers: Vec::new(),
            proxy: None,
            context_window: None,
            max_turns: DEFAULT_MAX_TURNS,
            retries: DEFAULT_RETRIES,
            temperature: 0.2,
            global_instruction: None,
            template_snapshot: None,
            cli_command: None,
        }
    }
}

fn profile(
    id: &str,
    name: &str,
    kind: AiKind,
    base_url: &str,
    model: &str,
    context_window: u32,
) -> AiProfile {
    AiProfile {
        id: id.into(),
        name: name.into(),
        kind,
        base_url: base_url.into(),
        model: model.into(),
        auth: if matches!(kind, AiKind::Local) {
            AuthKind::None
        } else {
            AuthKind::Bearer
        },
        context_window: Some(context_window),
        ..AiProfile::default()
    }
}

/// 内置预设：云服务 + 本地服务 + 本地 CLI。密钥引用统一走环境变量。
pub fn presets() -> Vec<AiProfile> {
    vec![
        profile(
            "openai",
            "OpenAI",
            AiKind::Cloud,
            "https://api.openai.com/v1",
            "gpt-4o-mini",
            128_000,
        ),
        profile(
            "deepseek",
            "DeepSeek",
            AiKind::Cloud,
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            64_000,
        ),
        profile(
            "moonshot",
            "Moonshot",
            AiKind::Cloud,
            "https://api.moonshot.cn/v1",
            "moonshot-v1-8k",
            8_192,
        ),
        profile(
            "dashscope",
            "通义千问（兼容模式）",
            AiKind::Cloud,
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "qwen-plus",
            128_000,
        ),
        profile(
            "zhipu",
            "智谱 GLM",
            AiKind::Cloud,
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-4-flash",
            128_000,
        ),
        profile(
            "ollama",
            "Ollama（本地）",
            AiKind::Local,
            "http://127.0.0.1:11434/v1",
            "llama3.1",
            8_192,
        ),
        profile(
            "lmstudio",
            "LM Studio（本地）",
            AiKind::Local,
            "http://127.0.0.1:1234/v1",
            "local-model",
            8_192,
        ),
        AiProfile {
            id: "cli".into(),
            name: "本地 CLI Agent".into(),
            kind: AiKind::Cli,
            auth: AuthKind::None,
            cli_command: Some("ollama run llama3.1".into()),
            max_turns: 1,
            retries: 0,
            context_window: None,
            api_key_ref: None,
            ..AiProfile::default()
        },
    ]
}

impl AiProfile {
    /// 密钥读取顺序：显式 `api_key_ref` → 按 id 生成的环境变量名。
    pub fn key_env(&self) -> String {
        self.api_key_ref
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("RUSTDB_AI_KEY_{}", self.id.to_ascii_uppercase()))
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    pub fn models_endpoint(&self) -> String {
        self.endpoint("models")
    }

    pub fn chat_endpoint(&self) -> String {
        self.endpoint("chat/completions")
    }

    /// 校验返回全部错误（空表表示通过）。
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.id.trim().is_empty() {
            errors.push("配置 id 不能为空".into());
        } else if !self
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            errors.push("配置 id 只能用小写字母、数字、连字符与下划线".into());
        }
        if self.name.trim().is_empty() {
            errors.push("名称不能为空".into());
        }
        if self.kind.needs_url() {
            if !valid_url(&self.base_url) {
                errors.push("端点地址必须以 http:// 或 https:// 开头".into());
            }
            if self.model.trim().is_empty() {
                errors.push("模型名不能为空".into());
            }
        } else if self
            .cli_command
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            errors.push("本地 CLI 需要填写命令".into());
        }
        if self.auth == AuthKind::Header {
            match self.auth_header.as_deref().map(str::trim) {
                Some(name) if header_name_ok(name) => {}
                _ => errors.push("自定义认证头需要一个合法名称".into()),
            }
        }
        for (name, value) in &self.headers {
            if !header_name_ok(name) {
                errors.push(format!("请求头名称不合法：{name}"));
            }
            if value.contains('\r') || value.contains('\n') {
                errors.push(format!("请求头 {name} 的值不能含换行"));
            }
        }
        if let Some(proxy) = self.proxy.as_deref().map(str::trim) {
            if proxy.is_empty() {
                // 空串等价于未设置。
            } else if !(proxy.starts_with("http://") || proxy.starts_with("https://")) {
                errors.push("代理只支持 http:// 或 https://".into());
            }
        }
        if let Some(window) = self.context_window {
            if !(MIN_CONTEXT_WINDOW..=MAX_CONTEXT_WINDOW).contains(&window) {
                errors.push(format!(
                    "上下文窗口需在 {MIN_CONTEXT_WINDOW} ~ {MAX_CONTEXT_WINDOW} 之间"
                ));
            }
        }
        if !(1..=MAX_TURNS_LIMIT).contains(&self.max_turns) {
            errors.push(format!("最大轮次需在 1 ~ {MAX_TURNS_LIMIT} 之间"));
        }
        if self.retries > MAX_RETRY_LIMIT {
            errors.push(format!("重试次数不能超过 {MAX_RETRY_LIMIT}"));
        }
        if !self.temperature.is_finite() || !(0.0..=2.0).contains(&self.temperature) {
            errors.push("temperature 需在 0 ~ 2 之间".into());
        }
        errors
    }

    /// 不阻塞保存、但值得提醒的情况。
    pub fn warnings(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.kind == AiKind::Local && !is_loopback(&self.base_url) {
            notes.push("本地模型通常指向 127.0.0.1 / localhost".into());
        }
        if self.auth != AuthKind::None && self.api_key_ref.as_deref().unwrap_or("").contains("KEY")
        {
            notes.push(format!(
                "密钥只从环境变量 {} 读取，不会写进配置文件",
                self.key_env()
            ));
        }
        if self.kind == AiKind::Cli {
            notes.push("本地 CLI 只返回文本，工具调用不可用".into());
        }
        notes
    }

    /// 系统提示 = 基础提示 + 全局指令。
    pub fn effective_system(&self, base: &str) -> String {
        match self
            .global_instruction
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(extra) => format!("{base}\n\n## 全局指令（用户配置，仅约束表达方式）\n{extra}"),
            None => base.to_string(),
        }
    }

    /// 固化当前模板（模型 / 窗口 / 全局指令），便于导出时复现。
    pub fn capture_snapshot(&mut self, base: &str) {
        self.template_snapshot = Some(format!(
            "model: {}\ncontext_window: {}\ntemperature: {}\n---\n{}",
            if self.model.trim().is_empty() {
                "-"
            } else {
                &self.model
            },
            self.context_window
                .map(|window| window.to_string())
                .unwrap_or_else(|| "-".into()),
            self.temperature,
            self.effective_system(base)
        ));
    }
}

fn valid_url(value: &str) -> bool {
    let value = value.trim();
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let host = rest.split('/').next().unwrap_or("");
    !host.is_empty() && !host.contains(char::is_whitespace)
}

fn is_loopback(value: &str) -> bool {
    let rest = value.split("://").nth(1).unwrap_or(value);
    let host = rest.split('/').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

fn header_name_ok(name: &str) -> bool {
    !name.trim().is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// 配置表：命名配置 + 当前激活项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub version: u32,
    pub active: Option<String>,
    pub profiles: Vec<AiProfile>,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            version: AI_CONFIG_VERSION,
            active: None,
            profiles: Vec::new(),
        }
    }
}

impl AiSettings {
    pub fn active_profile(&self) -> Option<&AiProfile> {
        self.active
            .as_deref()
            .and_then(|id| self.find(id))
            .or_else(|| self.profiles.first())
    }

    pub fn find(&self, id: &str) -> Option<&AiProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut AiProfile> {
        self.profiles.iter_mut().find(|profile| profile.id == id)
    }

    /// 新建或更新一套配置；`id` 重复视为更新，名称重复报错。
    pub fn upsert(&mut self, profile: AiProfile) -> Result<(), Vec<String>> {
        let mut errors = profile.validate();
        if self
            .profiles
            .iter()
            .any(|existing| existing.name == profile.name && existing.id != profile.id)
        {
            errors.push(format!("名称「{}」已被另一套配置使用", profile.name));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        match self.find_mut(&profile.id) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
        if self.active.is_none() {
            self.active = self.profiles.first().map(|profile| profile.id.clone());
        }
        Ok(())
    }

    /// 删除配置；删掉激活项时回退到第一条。
    pub fn remove(&mut self, id: &str) {
        self.profiles.retain(|profile| profile.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = self.profiles.first().map(|profile| profile.id.clone());
        }
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for profile in &self.profiles {
            errors.extend(profile.validate());
            if !ids.insert(profile.id.clone()) {
                errors.push(format!("配置 id 重复：{}", profile.id));
            }
            if !names.insert(profile.name.clone()) {
                errors.push(format!("配置名称重复：{}", profile.name));
            }
        }
        if let Some(active) = &self.active {
            if self.find(active).is_none() {
                errors.push(format!("激活的配置不存在：{active}"));
            }
        }
        errors
    }

    /// 导出成 Markdown（不含密钥；请求头里的敏感值打码）。
    pub fn export_markdown(&self, reports: &[(String, ConnectivityReport)]) -> String {
        use std::fmt::Write;
        let mut out = String::from("# RustDb AI 模型配置\n\n");
        let _ = writeln!(
            out,
            "- 激活：{}\n- 配置数：{}\n",
            self.active_profile()
                .map(|profile| profile.name.as_str())
                .unwrap_or("（无，离线演示）"),
            self.profiles.len()
        );
        for profile in &self.profiles {
            let _ = writeln!(out, "## {}\n", profile.name);
            let _ = writeln!(out, "- id：`{}`", profile.id);
            let _ = writeln!(out, "- 类型：{}", profile.kind.label());
            if profile.kind.needs_url() {
                let _ = writeln!(out, "- 端点：`{}`", profile.base_url);
                let _ = writeln!(out, "- 模型：`{}`", profile.model);
                let _ = writeln!(out, "- 认证：{}", profile.auth.label());
                let _ = writeln!(
                    out,
                    "- 上下文窗口：{}",
                    profile
                        .context_window
                        .map(|window| window.to_string())
                        .unwrap_or_else(|| "未知".into())
                );
            } else {
                let _ = writeln!(
                    out,
                    "- 命令：`{}`",
                    profile.cli_command.as_deref().unwrap_or("-")
                );
            }
            let _ = writeln!(
                out,
                "- 最大轮次 / 重试：{} / {}",
                profile.max_turns, profile.retries
            );
            if let Some(proxy) = profile.proxy.as_deref().filter(|text| !text.is_empty()) {
                let _ = writeln!(out, "- 代理：`{proxy}`");
            }
            if !profile.headers.is_empty() {
                let _ = writeln!(
                    out,
                    "- 请求头：{}",
                    profile
                        .headers
                        .iter()
                        .map(|(name, value)| format!("{name}={}", mask_header(value)))
                        .collect::<Vec<_>>()
                        .join("，")
                );
            }
            if let Some(instruction) = profile
                .global_instruction
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                let _ = writeln!(out, "- 全局指令：{}", instruction.replace('\n', " "));
            }
            if let Some((_, report)) = reports.iter().find(|(id, _)| id == &profile.id) {
                let _ = writeln!(out, "- 连通性：{}", report.line());
            }
            let _ = writeln!(out);
        }
        out
    }
}

fn mask_header(value: &str) -> String {
    if value.chars().count() <= 4 {
        "****".into()
    } else {
        format!(
            "****{}",
            value
                .chars()
                .rev()
                .take(2)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        )
    }
}

/// 读取配置；文件缺失或损坏返回默认（空表 → 离线演示）。
pub fn load_settings(path: &Path) -> AiSettings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return AiSettings::default();
    };
    match serde_json::from_str::<AiSettings>(&text) {
        Ok(settings) => settings,
        Err(error) => {
            log::warn!("AI 配置解析失败：{error}（{}）", path.display());
            AiSettings::default()
        }
    }
}

/// 原子写入配置。
pub fn save_settings(path: &Path, settings: &AiSettings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn settings_path(dir: &Path) -> PathBuf {
    dir.join("ai_models.json")
}

/// 解析 `/models` 响应：OpenAI `data[].id`、Ollama `models[].name`、纯字符串数组。
pub fn parse_models(body: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("模型列表不是合法 JSON：{error}"))?;
    let mut models = Vec::new();
    if let Some(items) = value.get("data").and_then(|data| data.as_array()) {
        for item in items {
            if let Some(id) = item.get("id").and_then(|id| id.as_str()) {
                models.push(id.to_string());
            }
        }
    }
    if let Some(items) = value.get("models").and_then(|data| data.as_array()) {
        for item in items {
            if let Some(id) = item
                .get("name")
                .or_else(|| item.get("model"))
                .and_then(|id| id.as_str())
            {
                models.push(id.to_string());
            }
        }
    }
    if models.is_empty() {
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(id) = item.as_str() {
                    models.push(id.to_string());
                }
            }
        }
    }
    if models.is_empty() {
        return Err("响应里没有模型列表".into());
    }
    models.sort();
    models.dedup();
    Ok(models)
}

/// 过滤掉明显不是对话模型的条目（向量、语音、图像等）。
pub fn is_chat_model(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    ![
        "embed",
        "whisper",
        "tts",
        "dall-e",
        "moderation",
        "rerank",
        "stable-diffusion",
        "flux",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// 名称过滤 + 排序；空关键字返回全部对话模型。
pub fn filter_models(models: &[String], needle: &str) -> Vec<String> {
    let needle = needle.trim().to_ascii_lowercase();
    let mut out: Vec<String> = models
        .iter()
        .filter(|id| is_chat_model(id))
        .filter(|id| needle.is_empty() || id.to_ascii_lowercase().contains(&needle))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiErrorKind {
    InvalidConfig,
    Auth,
    NotFound,
    ModelMissing,
    RateLimit,
    Timeout,
    Network,
    Server,
    Protocol,
    Unsupported,
    Unknown,
}

impl AiErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::InvalidConfig => "配置无效",
            Self::Auth => "认证失败",
            Self::NotFound => "端点不存在",
            Self::ModelMissing => "模型不可用",
            Self::RateLimit => "限流",
            Self::Timeout => "超时",
            Self::Network => "网络错误",
            Self::Server => "服务端错误",
            Self::Protocol => "协议错误",
            Self::Unsupported => "不支持",
            Self::Unknown => "未知错误",
        }
    }

    pub fn advice(self) -> &'static str {
        match self {
            Self::InvalidConfig => "检查端点、模型名与窗口设置",
            Self::Auth => "检查密钥是否有效、是否发给了正确的端点",
            Self::NotFound => "检查端点地址（通常以 /v1 结尾）",
            Self::ModelMissing => "先用「发现模型」列出可用模型再选",
            Self::RateLimit => "稍后重试或降低频率",
            Self::Timeout => "检查网络 / 代理，或换更快的端点",
            Self::Network => "检查 DNS、代理与 TLS 证书",
            Self::Server => "服务端暂时不可用，稍后重试",
            Self::Protocol => "端点可能不是 OpenAI 兼容协议",
            Self::Unsupported => "该能力只在 HTTP 端点上提供",
            Self::Unknown => "查看错误原文后重试",
        }
    }
}

pub fn classify_status(status: u16, body: &str) -> AiErrorKind {
    match status {
        401 | 403 => AiErrorKind::Auth,
        404 => {
            if body.to_ascii_lowercase().contains("model") {
                AiErrorKind::ModelMissing
            } else {
                AiErrorKind::NotFound
            }
        }
        429 => AiErrorKind::RateLimit,
        400 | 422 => {
            if body.to_ascii_lowercase().contains("model") {
                AiErrorKind::ModelMissing
            } else {
                AiErrorKind::Protocol
            }
        }
        500..=599 => AiErrorKind::Server,
        _ => AiErrorKind::Unknown,
    }
}

pub fn classify_transport(message: &str) -> AiErrorKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("超时") {
        AiErrorKind::Timeout
    } else if lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("network")
        || lower.contains("tls")
        || lower.contains("certificate")
        || lower.contains("proxy")
    {
        AiErrorKind::Network
    } else {
        AiErrorKind::Unknown
    }
}

/// 一次连通性测试的结果（导出 Markdown 时也用它）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectivityReport {
    pub ok: bool,
    pub latency_ms: u64,
    pub model: String,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub kind: Option<AiErrorKind>,
    /// 端点返回的模型数（发现成功时）。
    pub model_count: Option<usize>,
    pub checked_ms: u64,
}

impl ConnectivityReport {
    pub fn line(&self) -> String {
        if self.ok {
            format!(
                "可用 · {} ms · 模型 {}（端点返回 {} 个）",
                self.latency_ms,
                self.model,
                self.model_count.unwrap_or(0)
            )
        } else {
            format!(
                "{} · {} · {}",
                self.kind.unwrap_or(AiErrorKind::Unknown).label(),
                self.latency_ms,
                self.error.clone().unwrap_or_else(|| "无错误详情".into())
            )
        }
    }

    fn failure(kind: AiErrorKind, message: impl Into<String>, started: Instant) -> Self {
        Self {
            ok: false,
            latency_ms: elapsed_ms(started),
            model: String::new(),
            status: None,
            error: Some(message.into()),
            kind: Some(kind),
            model_count: None,
            checked_ms: now_ms(),
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn http_agent(profile: &AiProfile) -> Result<ureq::Agent, AiErrorKind> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(PROBE_TIMEOUT)
        .timeout_read(PROBE_TIMEOUT)
        .timeout_write(PROBE_TIMEOUT);
    if let Some(proxy) = profile
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let proxy = ureq::Proxy::new(proxy).map_err(|_| AiErrorKind::InvalidConfig)?;
        builder = builder.proxy(proxy);
    }
    Ok(builder.build())
}

fn authorize(request: ureq::Request, profile: &AiProfile, api_key: &str) -> ureq::Request {
    let mut request = request;
    match profile.auth {
        AuthKind::Bearer if !api_key.is_empty() => {
            request = request.set("Authorization", &format!("Bearer {api_key}"));
        }
        AuthKind::Header if !api_key.is_empty() => {
            let name = profile.auth_header.as_deref().unwrap_or("api-key");
            request = request.set(name, api_key);
        }
        _ => {}
    }
    for (name, value) in &profile.headers {
        request = request.set(name, value);
    }
    request
}

/// 连通性测试：`GET /models`，只发最小请求；测量延迟并解析实际可用模型。
pub fn probe(profile: &AiProfile, api_key: &str) -> ConnectivityReport {
    let started = Instant::now();
    let errors = profile.validate();
    if !errors.is_empty() {
        return ConnectivityReport::failure(AiErrorKind::InvalidConfig, errors.join("；"), started);
    }
    if profile.kind == AiKind::Cli {
        return crate::agent::CliProvider::new(
            profile.cli_command.clone().unwrap_or_default(),
            PROBE_TIMEOUT,
        )
        .probe()
        .into_report(started, "");
    }
    let agent = match http_agent(profile) {
        Ok(agent) => agent,
        Err(kind) => {
            return ConnectivityReport::failure(kind, "代理地址不合法", started);
        }
    };
    let request = authorize(agent.get(&profile.models_endpoint()), profile, api_key);
    match request.call() {
        Ok(response) => {
            let latency = elapsed_ms(started);
            let body = response.into_string().unwrap_or_default();
            let models = parse_models(&body).unwrap_or_default();
            let model_count = Some(models.len());
            let model_available = models.is_empty() || models.iter().any(|id| id == &profile.model);
            if !model_available {
                return ConnectivityReport {
                    ok: false,
                    latency_ms: latency,
                    model: profile.model.clone(),
                    status: None,
                    error: Some(format!("端点没有模型「{}」", profile.model)),
                    kind: Some(AiErrorKind::ModelMissing),
                    model_count,
                    checked_ms: now_ms(),
                };
            }
            ConnectivityReport {
                ok: true,
                latency_ms: latency,
                model: profile.model.clone(),
                status: None,
                error: None,
                kind: None,
                model_count,
                checked_ms: now_ms(),
            }
        }
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            let kind = classify_status(status, &body);
            let mut report = ConnectivityReport::failure(kind, first_line(&body), started);
            report.status = Some(status);
            report.model = profile.model.clone();
            report
        }
        Err(ureq::Error::Transport(error)) => {
            let message = error.to_string();
            let kind = classify_transport(&message);
            ConnectivityReport::failure(kind, message, started)
        }
    }
}

/// 发现模型：同样只发 `GET /models`，失败时返回带分类的报告。
pub fn discover_models(
    profile: &AiProfile,
    api_key: &str,
) -> Result<Vec<String>, ConnectivityReport> {
    let started = Instant::now();
    let errors = profile.validate();
    if !errors.is_empty() {
        return Err(ConnectivityReport::failure(
            AiErrorKind::InvalidConfig,
            errors.join("；"),
            started,
        ));
    }
    if profile.kind == AiKind::Cli {
        return Err(ConnectivityReport::failure(
            AiErrorKind::Unsupported,
            "本地 CLI 不提供模型列表",
            started,
        ));
    }
    let agent = match http_agent(profile) {
        Ok(agent) => agent,
        Err(kind) => return Err(ConnectivityReport::failure(kind, "代理地址不合法", started)),
    };
    let request = authorize(agent.get(&profile.models_endpoint()), profile, api_key);
    match request.call() {
        Ok(response) => {
            let body = response.into_string().unwrap_or_default();
            parse_models(&body).map_err(|message| {
                ConnectivityReport::failure(AiErrorKind::Protocol, message, started)
            })
        }
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            let kind = classify_status(status, &body);
            let mut report = ConnectivityReport::failure(kind, first_line(&body), started);
            report.status = Some(status);
            Err(report)
        }
        Err(ureq::Error::Transport(error)) => {
            let message = error.to_string();
            let kind = classify_transport(&message);
            Err(ConnectivityReport::failure(kind, message, started))
        }
    }
}

fn first_line(body: &str) -> String {
    body.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect()
}

/// 单个模型结论导出 Markdown（F-94「结论可导出」）。
pub fn conclusion_markdown(
    profile: &AiProfile,
    report: &ConnectivityReport,
    models: &[String],
    conclusion: &str,
) -> String {
    use std::fmt::Write;
    let mut out = String::from("# AI 模型结论\n\n");
    let _ = writeln!(out, "- 配置：{}（`{}`）", profile.name, profile.id);
    let _ = writeln!(out, "- 类型：{}", profile.kind.label());
    if profile.kind.needs_url() {
        let _ = writeln!(out, "- 端点：`{}`", profile.base_url);
        let _ = writeln!(out, "- 模型：`{}`", profile.model);
    }
    let _ = writeln!(out, "- 连通性：{}", report.line());
    if !models.is_empty() {
        let _ = writeln!(
            out,
            "- 可用模型（前 20）：{}",
            models
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    if let Some(snapshot) = &profile.template_snapshot {
        let _ = writeln!(out, "\n## 模板快照\n\n```\n{snapshot}\n```");
    }
    let _ = writeln!(out, "\n## 结论\n\n{}", conclusion.trim());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_of(id: &str) -> AiProfile {
        AiProfile {
            id: id.into(),
            name: id.into(),
            base_url: "https://example.com/v1".into(),
            model: "gpt-test".into(),
            ..AiProfile::default()
        }
    }

    #[test]
    fn presets_are_valid_and_unique() {
        let list = presets();
        assert!(list.len() >= 8);
        let mut ids = std::collections::BTreeSet::new();
        for item in &list {
            assert!(
                item.validate().is_empty(),
                "{}: {:?}",
                item.id,
                item.validate()
            );
            assert!(ids.insert(item.id.clone()), "重复 id：{}", item.id);
        }
        assert!(list.iter().any(|item| item.kind == AiKind::Local));
        assert!(list.iter().any(|item| item.kind == AiKind::Cli));
    }

    #[test]
    fn endpoints_join_without_double_slash() {
        let item = profile_of("a");
        assert_eq!(item.models_endpoint(), "https://example.com/v1/models");
        assert_eq!(
            item.chat_endpoint(),
            "https://example.com/v1/chat/completions"
        );
        let mut trailing = item.clone();
        trailing.base_url = "https://x.test/v1/".into();
        assert_eq!(trailing.models_endpoint(), "https://x.test/v1/models");
    }

    #[test]
    fn validation_rejects_bad_urls_and_limits() {
        let mut item = profile_of("ok");
        item.base_url = "ftp://x".into();
        item.max_turns = 0;
        item.retries = 99;
        item.temperature = 9.0;
        item.context_window = Some(10);
        let errors = item.validate();
        assert!(errors.iter().any(|error| error.contains("http")));
        assert!(errors.iter().any(|error| error.contains("轮次")));
        assert!(errors.iter().any(|error| error.contains("重试")));
        assert!(errors.iter().any(|error| error.contains("temperature")));
        assert!(errors.iter().any(|error| error.contains("上下文窗口")));
    }

    #[test]
    fn header_auth_requires_name_and_no_newlines() {
        let mut item = profile_of("h");
        item.auth = AuthKind::Header;
        item.auth_header = Some("bad header".into());
        item.headers = vec![("X-Tenant".into(), "a\nb".into())];
        let errors = item.validate();
        assert!(errors.iter().any(|error| error.contains("自定义认证头")));
        assert!(errors.iter().any(|error| error.contains("换行")));
    }

    #[test]
    fn cli_profile_requires_command_and_skips_url() {
        let mut item = AiProfile::default();
        item.id = "cli".into();
        item.name = "CLI".into();
        item.kind = AiKind::Cli;
        assert!(item.validate().iter().any(|error| error.contains("命令")));
        item.cli_command = Some("llm run".into());
        assert!(item.validate().is_empty(), "{:?}", item.validate());
    }

    #[test]
    fn upsert_updates_and_rejects_duplicate_names() {
        let mut settings = AiSettings::default();
        settings.upsert(profile_of("a")).unwrap();
        let mut second = profile_of("b");
        second.name = "a".into();
        assert!(settings.upsert(second.clone()).is_err());
        let mut renamed = profile_of("a");
        renamed.name = "甲".into();
        settings.upsert(renamed).unwrap();
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.find("a").unwrap().name, "甲");
        assert_eq!(settings.active.as_deref(), Some("a"));
    }

    #[test]
    fn remove_falls_back_to_first_profile() {
        let mut settings = AiSettings::default();
        settings.upsert(profile_of("a")).unwrap();
        settings.upsert(profile_of("b")).unwrap();
        settings.active = Some("b".into());
        settings.remove("b");
        assert_eq!(settings.active.as_deref(), Some("a"));
    }

    #[test]
    fn parse_models_supports_three_shapes() {
        let openai = r#"{"data":[{"id":"gpt-4o"},{"id":"text-embedding-3"}]}"#;
        assert_eq!(
            parse_models(openai).unwrap(),
            vec!["gpt-4o".to_string(), "text-embedding-3".to_string()]
        );
        let ollama = r#"{"models":[{"name":"llama3.1"},{"name":"qwen2.5"}]}"#;
        assert_eq!(
            parse_models(ollama).unwrap(),
            vec!["llama3.1".to_string(), "qwen2.5".to_string()]
        );
        let bare = r#"["a","b"]"#;
        assert_eq!(
            parse_models(bare).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(parse_models("{}").is_err());
        assert!(parse_models("not json").is_err());
    }

    #[test]
    fn filter_models_drops_non_chat_and_matches() {
        let models = vec![
            "gpt-4o".to_string(),
            "text-embedding-3-small".to_string(),
            "whisper-1".to_string(),
            "gpt-4o-mini".to_string(),
        ];
        assert_eq!(
            filter_models(&models, ""),
            vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]
        );
        assert_eq!(
            filter_models(&models, "MINI"),
            vec!["gpt-4o-mini".to_string()]
        );
    }

    #[test]
    fn error_classification_is_stable() {
        assert_eq!(classify_status(401, ""), AiErrorKind::Auth);
        assert_eq!(
            classify_status(404, "model not found"),
            AiErrorKind::ModelMissing
        );
        assert_eq!(classify_status(404, "no route"), AiErrorKind::NotFound);
        assert_eq!(classify_status(429, ""), AiErrorKind::RateLimit);
        assert_eq!(classify_status(503, ""), AiErrorKind::Server);
        assert_eq!(classify_transport("timed out"), AiErrorKind::Timeout);
        assert_eq!(classify_transport("dns error"), AiErrorKind::Network);
        assert_eq!(classify_transport("weird"), AiErrorKind::Unknown);
    }

    #[test]
    fn system_prompt_appends_global_instruction() {
        let mut item = profile_of("g");
        assert_eq!(item.effective_system("base"), "base");
        item.global_instruction = Some("  用中文回答  ".into());
        let system = item.effective_system("base");
        assert!(system.starts_with("base\n\n## 全局指令"));
        assert!(system.contains("用中文回答"));
        item.capture_snapshot("base");
        let snapshot = item.template_snapshot.clone().unwrap();
        assert!(snapshot.contains("gpt-test"));
        assert!(snapshot.contains("用中文回答"));
    }

    #[test]
    fn markdown_export_masks_headers_and_lists_reports() {
        let mut settings = AiSettings::default();
        let mut item = profile_of("a");
        item.name = "甲".into();
        item.headers = vec![("X-Token".into(), "secret-token".into())];
        item.global_instruction = Some("只说 SQL".into());
        settings.upsert(item).unwrap();
        let report = ConnectivityReport {
            ok: true,
            latency_ms: 42,
            model: "gpt-test".into(),
            status: None,
            error: None,
            kind: None,
            model_count: Some(3),
            checked_ms: 1,
        };
        let markdown = settings.export_markdown(&[("a".into(), report)]);
        assert!(markdown.contains("## 甲"));
        assert!(markdown.contains("可用 · 42 ms"));
        assert!(markdown.contains("X-Token=****"));
        assert!(!markdown.contains("secret-token"));
        assert!(markdown.contains("只说 SQL"));
    }

    #[test]
    fn conclusion_markdown_includes_snapshot_and_conclusion() {
        let mut item = profile_of("a");
        item.capture_snapshot("base");
        let report = ConnectivityReport {
            ok: false,
            latency_ms: 12,
            model: "gpt-test".into(),
            status: Some(401),
            error: Some("unauthorized".into()),
            kind: Some(AiErrorKind::Auth),
            model_count: None,
            checked_ms: 1,
        };
        let markdown = conclusion_markdown(&item, &report, &["a".into()], "换密钥后可用");
        assert!(markdown.contains("认证失败"));
        assert!(markdown.contains("模板快照"));
        assert!(markdown.contains("换密钥后可用"));
    }

    #[test]
    fn settings_round_trip_and_corrupt_file() {
        let dir = std::env::temp_dir().join(format!("rustdb-ai-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings_path(&dir);
        let mut settings = AiSettings::default();
        settings.upsert(profile_of("a")).unwrap();
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path), settings);
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(load_settings(&path), AiSettings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_env_falls_back_to_id() {
        let mut item = profile_of("deep");
        item.api_key_ref = None;
        assert_eq!(item.key_env(), "RUSTDB_AI_KEY_DEEP");
        item.api_key_ref = Some(" ".into());
        assert_eq!(item.key_env(), "RUSTDB_AI_KEY_DEEP");
        item.api_key_ref = Some("MY_KEY".into());
        assert_eq!(item.key_env(), "MY_KEY");
    }
}
