//! SFTP 错误与到 `kanso-types` 消息的映射。

use std::fmt;

use kanso_types::message::{Endpoint, Message, Reason, SftpOp};
use kanso_types::transfer::XferProtocol;

use super::proto;
use crate::SshError;

/// STATUS 码里本库关心的分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    /// `SSH_FX_NO_SUCH_FILE`。
    NotFound,
    /// `SSH_FX_PERMISSION_DENIED`。
    PermissionDenied,
    /// 目标已存在。协议没有专门的码（OpenSSH 对 `O_EXCL` 冲突回 `FAILURE`），
    /// 由 [`crate::sftp::SftpClient::create_file`] 在 `FAILURE` 后再 LSTAT 确认后改写成这一类。
    AlreadyExists,
    /// `SSH_FX_OP_UNSUPPORTED`。
    Unsupported,
    /// `SSH_FX_FAILURE`、`BAD_MESSAGE` 与其他码。
    Failure,
}

impl StatusKind {
    pub(crate) fn from_code(code: u32) -> Self {
        match code {
            proto::FX_NO_SUCH_FILE => StatusKind::NotFound,
            proto::FX_PERMISSION_DENIED => StatusKind::PermissionDenied,
            proto::FX_OP_UNSUPPORTED => StatusKind::Unsupported,
            _ => StatusKind::Failure,
        }
    }
}

/// SFTP 客户端错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SftpError {
    /// 服务器对某个请求回了非 OK 的 STATUS。
    Status {
        /// 发生在哪类操作上。
        op: SftpOp,
        /// 操作的远端路径（没有路径的请求，如 READ，用打开时的路径）。
        path: String,
        /// 分类。
        kind: StatusKind,
        /// 原始 STATUS 码，只进日志。
        code: u32,
        /// 服务器附带的说明，只进日志。
        detail: String,
    },
    /// 服务器拒绝或没有启用 `sftp` 子系统。
    SubsystemFailed {
        /// 连接对象。
        endpoint: Endpoint,
        /// 英文技术细节。
        detail: String,
    },
    /// 打开子系统通道前连接就已断开 / 被关闭。
    Ssh(SshError),
    /// 子系统通道在请求在途时结束（连接断开或服务器关闭通道）。
    Disconnected,
    /// 对端违反协议（畸形包、版本过低、响应类型不符）。
    Protocol {
        /// 英文技术细节。
        detail: String,
    },
}

impl SftpError {
    /// 映射成面向用户的消息，只用 `kanso-types` 里已有的 key。
    pub fn to_message(&self) -> Message {
        match self {
            SftpError::Status {
                kind: StatusKind::AlreadyExists,
                path,
                ..
            } => Message::SftpFileExists { path: path.clone() },
            SftpError::Status {
                op,
                path,
                kind,
                detail,
                code,
            } => Message::SftpOpFailed {
                op: *op,
                path: path.clone(),
                reason: match kind {
                    StatusKind::NotFound => Reason::NotFound,
                    StatusKind::PermissionDenied => Reason::PermissionDenied,
                    _ => Reason::Other {
                        detail: format!("status {code}: {detail}"),
                    },
                },
            },
            SftpError::SubsystemFailed { endpoint, detail } => Message::SftpSubsystemFailed {
                endpoint: endpoint.clone(),
                reason: Reason::Other {
                    detail: detail.clone(),
                },
            },
            SftpError::Ssh(e) => e.to_message(),
            SftpError::Disconnected => Message::XferSessionEnded {},
            SftpError::Protocol { detail } => Message::XferProtocolError {
                protocol: XferProtocol::Sftp,
                detail: detail.clone(),
            },
        }
    }

    /// 是否"目标不存在"。
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            SftpError::Status {
                kind: StatusKind::NotFound,
                ..
            }
        )
    }

    /// 是否"目标已存在"。
    pub fn is_already_exists(&self) -> bool {
        matches!(
            self,
            SftpError::Status {
                kind: StatusKind::AlreadyExists,
                ..
            }
        )
    }
}

impl fmt::Display for SftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SftpError::Status {
                op,
                path,
                kind,
                code,
                detail,
            } => write!(f, "sftp {op:?} {path}: {kind:?} (status {code}: {detail})"),
            SftpError::SubsystemFailed { endpoint, detail } => {
                write!(
                    f,
                    "sftp subsystem failed on {}: {detail}",
                    endpoint.host_port()
                )
            }
            SftpError::Ssh(e) => write!(f, "{e}"),
            SftpError::Disconnected => write!(f, "sftp channel closed"),
            SftpError::Protocol { detail } => write!(f, "sftp protocol error: {detail}"),
        }
    }
}

impl std::error::Error for SftpError {}

impl From<SshError> for SftpError {
    fn from(e: SshError) -> Self {
        SftpError::Ssh(e)
    }
}
