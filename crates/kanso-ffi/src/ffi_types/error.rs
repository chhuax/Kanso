//! FFI 错误类型。

use std::fmt;

use kanso_core::CoreError;

use super::message::FfiMessage;

/// core 返回的错误（镜像 `kanso_core::CoreError`）。
///
/// Swift 侧表现为 `Error`，`async throws` / `throws` 方法抛出它。
/// 只有 `User` 带可展示给用户的消息；其余情况 UI 显示通用失败提示，`detail` 只进日志。
/// 注意：`NoSuch*` 变体不携带 id（调用方自己知道刚才传了哪个 id）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum FfiCoreError {
    /// 可直接展示给用户的错误；用 `message.key` 查本地化目录。
    User {
        /// 本地化用的消息。
        message: FfiMessage,
    },
    /// 会话不存在（已关闭或 id 无效）。
    NoSuchSession,
    /// Profile 不存在。
    NoSuchProfile,
    /// 分组不存在。
    NoSuchGroup,
    /// 该会话不支持此操作。
    NotSupported,
    /// 内部错误；`detail` 为英文技术细节，只进日志，不展示给用户。
    Internal {
        /// 技术细节。
        detail: String,
    },
}

impl fmt::Display for FfiCoreError {
    /// 英文，供日志使用；不得直接展示给用户。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FfiCoreError::User { message } => write!(f, "user-facing error: {}", message.key),
            FfiCoreError::NoSuchSession => f.write_str("no such session"),
            FfiCoreError::NoSuchProfile => f.write_str("no such profile"),
            FfiCoreError::NoSuchGroup => f.write_str("no such group"),
            FfiCoreError::NotSupported => f.write_str("operation not supported for this session"),
            FfiCoreError::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

impl std::error::Error for FfiCoreError {}

impl From<CoreError> for FfiCoreError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::User(m) => FfiCoreError::User {
                message: (&m).into(),
            },
            CoreError::NoSuchSession(_) => FfiCoreError::NoSuchSession,
            CoreError::NoSuchProfile(_) => FfiCoreError::NoSuchProfile,
            CoreError::NoSuchGroup(_) => FfiCoreError::NoSuchGroup,
            CoreError::NotSupported => FfiCoreError::NotSupported,
            CoreError::Internal { detail } => FfiCoreError::Internal { detail },
        }
    }
}
