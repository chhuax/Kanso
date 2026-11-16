//! SFTP 客户端与流水线传输引擎（ssh.md §7、§8、§9）。
//!
//! 协议（SFTP v3，draft-ietf-secsh-filexfer-02）由本模块自己实现，没有使用 `russh-sftp`：
//! 传输性能要靠"同一通道上同时有几十个在途请求"，需要完全控制请求的发送与响应的消费顺序；
//! 协议本身很小（十几个包类型），自实现的维护成本低于适配外部库的 API 与它引入的第二份 russh。
//!
//! - [`SftpClient`]：按需打开子系统、目录与文件操作、目录列表流；
//! - [`TransferOptions`] / [`TransferReport`]：单文件与目录的上传下载；
//! - [`SftpError`]：结构化错误，可映射到 `kanso-types` 消息。

mod client;
mod error;
pub(crate) mod proto;
mod session;
mod transfer;
mod tree;

pub use client::{DirListing, SftpClient};
pub use error::{SftpError, StatusKind};
pub use proto::FileAttrs;
pub use transfer::{
    BufferGauge, CancelToken, DEFAULT_CHUNK, DEFAULT_WINDOW, MAX_CHUNK, PROGRESS_INTERVAL,
    ProgressFn, SkipReason, SkippedItem, TransferError, TransferOptions, TransferOutcome,
    TransferProgress, TransferReport,
};
pub use tree::{MAX_DEPTH, is_safe_name};
