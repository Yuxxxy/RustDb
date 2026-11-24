use serde::{Deserialize, Serialize};
use std::fmt;

/// 驱动错误的统一分类。UI 的展示策略与 AI 的自修复策略都按它分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverErrorKind {
    Connection,
    Auth,
    Tls,
    Timeout,
    Cancelled,
    Syntax,
    Permission,
    NotFound,
    Constraint,
    Conflict,
    Deadlock,
    Serialization,
    ReadOnly,
    ResourceExhausted,
    Unsupported,
    Protocol,
    Io,
    Internal,
}

impl DriverErrorKind {
    /// 服务端/网络类错误：UI 可以提示「重试」。
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::Connection | Self::Timeout | Self::Deadlock | Self::Serialization
        )
    }
}

/// 驱动原始错误（保留服务端原文，供诊断与 AI 使用）。
#[derive(Debug, Clone)]
pub struct RawDbError {
    pub message: String,
    pub sqlstate: Option<String>,
    pub code: Option<i64>,
    pub position: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct DriverError {
    pub kind: DriverErrorKind,
    pub message: String,
    pub sqlstate: Option<String>,
    pub code: Option<i64>,
    /// 语法错误在 SQL 中的字符偏移（可用于编辑器定位）。
    pub position: Option<u32>,
    pub hint: Option<String>,
    pub retryable: bool,
}

impl DriverError {
    pub fn new(kind: DriverErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            sqlstate: None,
            code: None,
            position: None,
            hint: None,
            retryable: kind.retryable(),
        }
    }

    pub fn unsupported(feature: impl Into<String>) -> Self {
        let feature = feature.into();
        Self::new(
            DriverErrorKind::Unsupported,
            format!("当前数据库不支持该功能：{feature}"),
        )
    }

    pub fn connection(message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Connection, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Protocol, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(DriverErrorKind::Internal, message)
    }

    pub fn cancelled() -> Self {
        Self::new(DriverErrorKind::Cancelled, "查询已取消")
    }

    pub fn with_sqlstate(mut self, sqlstate: impl Into<String>) -> Self {
        self.sqlstate = Some(sqlstate.into());
        self
    }

    pub fn with_code(mut self, code: i64) -> Self {
        self.code = Some(code);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 用户可读的一句话（不含 SQLSTATE 等技术细节）。
    pub fn user_message(&self) -> String {
        self.hint
            .as_ref()
            .map(|h| format!("{}（{h}）", self.message))
            .unwrap_or_else(|| self.message.clone())
    }
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if let Some(code) = self.code {
            write!(f, " (code={code})")?;
        }
        if let Some(state) = &self.sqlstate {
            write!(f, " (sqlstate={state})")?;
        }
        Ok(())
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(value: std::io::Error) -> Self {
        Self::new(DriverErrorKind::Io, value.to_string())
    }
}
