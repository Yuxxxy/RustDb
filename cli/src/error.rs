//! 稳定错误码与退出码（F-96 / AC-37）。
//!
//! 脚本按退出码判断成败，按 `code` 区分原因；两者都不随文案变化。

use std::fmt;

/// 错误类别：码与退出码一一对应，**不再新增中间值**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 参数用法错误。
    Usage,
    /// 连接配置问题（找不到连接 / 配置损坏）。
    Config,
    /// 建链失败。
    Connect,
    /// 权限拒绝：只读连接、生产连接、缺开关。
    Permission,
    /// SQL 执行失败。
    Query,
    /// 等待超时。
    Timeout,
    /// 文件读写失败。
    Io,
    /// 内部错误。
    Internal,
}

impl ErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Usage => "E_USAGE",
            Self::Config => "E_CONFIG",
            Self::Connect => "E_CONNECT",
            Self::Permission => "E_PERMISSION",
            Self::Query => "E_QUERY",
            Self::Timeout => "E_TIMEOUT",
            Self::Io => "E_IO",
            Self::Internal => "E_INTERNAL",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Self::Internal => 1,
            Self::Usage => 2,
            Self::Config => 3,
            Self::Connect => 4,
            Self::Permission => 5,
            Self::Query => 6,
            Self::Timeout => 7,
            Self::Io => 8,
        }
    }
}

/// CLI 错误：稳定码 + 退出码 + 人类可读原因。
#[derive(Debug, Clone)]
pub struct CliError {
    pub kind: ErrorKind,
    pub message: String,
}

impl CliError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    pub fn connect(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Connect, message)
    }

    pub fn permission(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Permission, message)
    }

    pub fn query(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Query, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    pub fn exit_code(&self) -> i32 {
        self.kind.exit_code()
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CliError::io(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_and_exit_codes_are_stable() {
        // 退出码是脚本契约的一部分，改动必须显式进行。
        assert_eq!(ErrorKind::Usage.exit_code(), 2);
        assert_eq!(ErrorKind::Config.exit_code(), 3);
        assert_eq!(ErrorKind::Connect.exit_code(), 4);
        assert_eq!(ErrorKind::Permission.exit_code(), 5);
        assert_eq!(ErrorKind::Query.exit_code(), 6);
        assert_eq!(ErrorKind::Timeout.exit_code(), 7);
        assert_eq!(ErrorKind::Io.exit_code(), 8);
        assert_eq!(ErrorKind::Internal.exit_code(), 1);
    }

    #[test]
    fn permission_error_keeps_its_code() {
        let error = CliError::permission("缺少 --write 开关");
        assert_eq!(error.code(), "E_PERMISSION");
        assert_eq!(error.exit_code(), 5);
        assert!(error.to_string().contains("E_PERMISSION"));
    }
}
