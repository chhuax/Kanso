//! core 推送给 UI 的事件。回调里只做轻量分发，重活放回 UI 自己的线程。

use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;
use crate::ids::{PromptId, SessionId, TransferId};
use crate::message::{Endpoint, Message};
use crate::prompt::PromptRequest;
use crate::transfer::XferProtocol;

/// 会话关闭的原因。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseReason {
    /// 用户主动关闭；UI 不提示“会话已结束”（local-shell.md §7、ssh.md §6）。
    ByUser,
    /// 进程退出。
    Exited {
        /// 退出码；被信号终止或未知时为 `None`。
        code: Option<i32>,
    },
    /// 因错误关闭。
    Error(Message),
}

/// 会话状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// 正在连接 / 启动。
    Connecting,
    /// 已就绪。
    Connected,
    /// 已关闭。
    Closed(CloseReason),
}

/// 会话当前活动。
///
/// “未读完成”由 UI 根据 Running→Idle / AgentWorking→AgentIdle 的跳变与标签是否被选中
/// 自行维护：core 不知道哪个标签可见（terminal.md §5、§6，ui-shell.md §2.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activity {
    /// 空闲，停在提示符。
    Idle,
    /// 有命令在前台运行。
    CommandRunning,
    /// agent 在等待用户输入。
    AgentIdle(AgentKind),
    /// agent 正在工作。
    AgentWorking(AgentKind),
}

/// 连接链上某一跳协商到的旧算法。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyHop {
    /// 这一跳的连接对象。
    pub endpoint: Endpoint,
    /// 被使用的旧算法名（英文技术名，不本地化）。
    pub algorithms: Vec<String>,
}

/// 通知级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeLevel {
    /// 信息。
    Info,
    /// 警告。
    Warning,
    /// 错误。
    Error,
}

/// 传输方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    /// 本地到远端。
    Upload,
    /// 远端到本地。
    Download,
}

/// 传输结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferOutcome {
    /// 完成。
    Completed {
        /// 因目标已存在且策略为 `ConflictPolicy::Skip` 而跳过的文件数；其他策略恒为 0。
        ///
        /// 不含因文件名不安全、符号链接、特殊文件等原因被引擎跳过的项（那些只记日志）。
        skipped: u32,
    },
    /// 被取消（用户或对端）。
    Cancelled,
    /// 失败，附用户可见原因。
    Failed {
        /// 用户可见的失败原因。
        message: Message,
        /// 失败原因是"目标已存在"（上传：远端；下载：本地），且调用时策略为 `ConflictPolicy::Fail`。
        ///
        /// 单独给出布尔值，是为了让 UI 不必解析 `message` 的 key：上传与下载的"已存在"消息
        /// key 不同（`sftp-file-exists` / `xfer-local-write-failed`），UI 只需据此弹"覆盖 / 跳过 / 取消"，
        /// 确认后用 `ConflictPolicy::Overwrite` 或 `Skip` 重试。
        already_exists: bool,
    },
}

/// core 到 UI 的事件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreEvent {
    /// 会话状态变化。
    SessionStateChanged {
        /// 会话。
        session: SessionId,
        /// 新状态。
        state: SessionState,
    },
    /// 终端内容变脏，UI 应在下一帧拉取。
    ///
    /// 唤醒合并约定：只有终端从干净变脏时发一次；UI 按帧拉取（把终端重新置干净）之后
    /// 才会再次触发，所以连续输出不会刷出大量事件（architecture §4）。
    TerminalDirty {
        /// 会话。
        session: SessionId,
    },
    /// 当前目录 / 主机 / git 分支变化。
    LocationChanged {
        /// 会话。
        session: SessionId,
        /// 当前目录；未知为 `None`。
        cwd: Option<String>,
        /// 目录所属主机（OSC 7 带来的主机名）；未知为 `None`。
        cwd_host: Option<String>,
        /// git 分支；不在仓库内为 `None`。
        git_branch: Option<String>,
    },
    /// 会话活动变化。
    ActivityChanged {
        /// 会话。
        session: SessionId,
        /// 新活动。
        activity: Activity,
    },
    /// 连接使用了旧算法的提示；`hops` 为空表示没有（用于清除之前的提示）。
    LegacyAlgorithms {
        /// 会话。
        session: SessionId,
        /// 使用旧算法的各跳。
        hops: Vec<LegacyHop>,
    },
    /// 一条用户可见通知。
    Notice {
        /// 关联会话；全局通知为 `None`。
        session: Option<SessionId>,
        /// 级别。
        level: NoticeLevel,
        /// 内容。
        message: Message,
    },
    /// 需要用户应答的请求。
    Prompt {
        /// 请求 id，应答时带回。
        prompt: PromptId,
        /// 关联会话；与会话无关为 `None`。
        session: Option<SessionId>,
        /// 请求内容。
        request: PromptRequest,
    },
    /// 请求已失效（超时或会话关闭），UI 应关闭对应界面。
    PromptDismissed {
        /// 请求 id。
        prompt: PromptId,
    },
    /// 传输开始。
    TransferStarted {
        /// 传输 id。
        transfer: TransferId,
        /// 所属会话。
        session: SessionId,
        /// 协议。
        protocol: XferProtocol,
        /// 方向。
        direction: TransferDirection,
        /// 文件名。
        name: String,
        /// 总字节数；未知为 `None`。
        total_bytes: Option<u64>,
    },
    /// 传输进度。core 最多每 50 ms 发一次（ssh.md §9.1）。
    TransferProgress {
        /// 传输 id。
        transfer: TransferId,
        /// 已完成字节数。
        done_bytes: u64,
        /// 总字节数；未知为 `None`。
        total_bytes: Option<u64>,
    },
    /// 传输结束。
    TransferFinished {
        /// 传输 id。
        transfer: TransferId,
        /// 结果。
        outcome: TransferOutcome,
    },
}
