//! MCP 服务用到的 core 契约类型（spec/mcp.md，architecture §16.3）。
//!
//! 使用方只有 `kanso-mcp-server`（Rust），这些类型**不经过 FFI**，所以可以自由使用
//! `Duration`、`Vec<u8>` 等类型。Swift 侧只通过将来 `kanso-ffi` 导出的 `McpServer` 对象
//! 与 MCP 打交道，以及读写授权与审计（那部分类型在 `kanso_types::mcp`）。
//!
//! # 输入来源
//!
//! 现有的 `Core::key`、`text_input`、`paste` 一律视为**用户**输入；MCP 的输入只走
//! [`Core::mcp_terminal_run`](crate::Core::mcp_terminal_run) 与
//! [`Core::mcp_terminal_send`](crate::Core::mcp_terminal_send)。core 据此区分"用户接管"
//! （spec/mcp.md §4.3），不需要给现有输入接口加来源参数（那会牵动 FFI 与 Swift）。
//!
//! # 无界面会话
//!
//! MCP 在没有标签的情况下也要用连接（后台执行、SFTP）。[`Core::mcp_connect`](crate::Core::mcp_connect)
//! 返回一个**无界面会话**：机制同纯 SFTP 会话，但不出现在 `Core::sessions()` 里，也不发
//! `SessionStateChanged` 等会话事件给 UI。它发起的传输照常发传输事件（传输列表要能看到，
//! spec/mcp.md §3.5），所以 UI 必须容忍传输事件里出现自己没打开过的 `SessionId`。

use std::sync::Arc;
use std::time::Duration;

use kanso_types::event::Activity;
use kanso_types::{KeyEvent, SessionId};

use crate::api::SessionInfo;

/// MCP 授权变化的通知器，见 [`Core::mcp_policy_watch`](crate::Core::mcp_policy_watch)。
///
/// 由 `Core` 创建并交给后端，后端在授权数据**提交到存储之后立即**调用 [`bump`](Self::bump)。
/// 不能由 `Core` 在 await 后端之后再通知：调用方中途丢弃 future 时，后台的删除仍会提交，
/// 而 `Core` 那一行已经执行不到，订阅者就收不到通知。
#[derive(Clone)]
pub(crate) struct PolicyNotifier(Arc<tokio::sync::watch::Sender<u64>>);

impl PolicyNotifier {
    pub(crate) fn new() -> PolicyNotifier {
        PolicyNotifier(Arc::new(tokio::sync::watch::Sender::new(0)))
    }

    /// 计数加一并唤醒全部订阅者；没有订阅者时也照常递增。
    pub(crate) fn bump(&self) {
        self.0.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0.subscribe()
    }
}

/// 单次结果中文本的默认上限：256 KiB（spec/mcp.md §5）。
pub const MCP_TEXT_LIMIT: usize = 256 * 1024;

/// 截取后的文本。超出上限时保留**末尾**（命令输出的结论通常在后面，spec/mcp.md §5）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturedText {
    /// 文本，UTF-8；已按会话编码转换，不含控制序列。
    pub text: String,
    /// 截取前的字节数（UTF-8）。
    pub original_bytes: u64,
    /// 是否被截断。
    pub truncated: bool,
    /// 截取范围内发生过清屏，只保留了清屏之后的部分（spec/mcp.md §4.2）。
    pub cleared: bool,
}

/// 读取终端的范围。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextScope {
    /// 实时屏幕，不受用户滚动位置影响。
    Screen,
    /// 用户当前滚动到的视口。
    Viewport,
    /// 回滚缓冲加屏幕的最后若干行（spec/mcp.md §3.2：默认 200，最多 2000，由调用方夹取）。
    Recent {
        /// 行数。
        lines: u32,
    },
}

/// 终端文本快照（`kanso_terminal_read`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalText {
    /// 文本，规则见 spec/mcp.md §5：软换行拼回整行，行尾空格去掉。
    pub content: CapturedText,
    /// 光标坐标（行、列，从 0 开始）；滚出视口时为空。
    pub cursor: Option<(u16, u16)>,
    /// 屏幕行数。
    pub rows: u16,
    /// 屏幕列数。
    pub cols: u16,
    /// 是否处于备用屏幕（全屏 TUI）。
    pub alternate_screen: bool,
}

/// 标签不能接受 MCP 写操作的原因（spec/mcp.md §4.2 的前置条件）。
///
/// "客户端所在标签"与"另一个 MCP 调用正在操作"由 `kanso-mcp-server` 判断，不在这里。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalBusy {
    /// 未连接或已关闭。会话结束时终端会被锁定（terminal.md §13），锁定也归入这一项，
    /// 不单独设"已锁定"：两者是同一个条件，分成两项只会让调用方映射出两种说法。
    NotConnected,
    /// 有命令在运行，没有停在提示符上。
    CommandRunning,
    /// 正在运行 AI CLI。
    AgentRunning,
    /// 输入行不为空：自提示符出现后有过用户输入（spec/mcp.md §4.2）。
    InputNotEmpty,
    /// 正在进行 ZMODEM / XMODEM 传输。
    Transferring,
    /// Sender 正在向这个标签发送多行内容。
    SenderActive,
}

/// [`Core::mcp_terminal_run`](crate::Core::mcp_terminal_run) 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalRunOutcome {
    /// 前置条件不满足，什么也没发送。
    Rejected(TerminalBusy),
    /// 提示符再次出现，命令已完成。
    Completed {
        /// 命令所在行之后到新提示符之前的输出。
        output: CapturedText,
        /// 有 OSC 133 D 时为退出码，否则为 `None`。
        exit_code: Option<i32>,
    },
    /// 超时仍未确认完成：可能还在运行，也可能已结束但没有识别出来（spec/mcp.md §4.2）。
    /// 不会向终端发送 Ctrl+C。
    Unconfirmed {
        /// 到超时为止的输出。
        output: CapturedText,
        /// 标签有没有 OSC 133；没有时完成判断只能靠提示符特征，调用方据此补充说明。
        osc133: bool,
    },
    /// 等待期间用户在这个标签里按键或粘贴（spec/mcp.md §4.3）。
    TakenOver {
        /// 到接管为止的输出。
        output: CapturedText,
    },
}

/// MCP 发给终端的一段输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpInput {
    /// 文字，原样输入（不经按键编码，不当作粘贴）。
    Text(String),
    /// 一个按键，按终端当前模式编码，与用户按键走同一套编码器（terminal.md §9）。
    Key(KeyEvent),
}

/// [`Core::mcp_terminal_send`](crate::Core::mcp_terminal_send) 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalSendOutcome {
    /// 不能输入：只检查 `NotConnected`、`Transferring` 两项，
    /// 因为这个接口本来就用于和运行中的程序交互。
    Rejected(TerminalBusy),
    /// 已发送；请求了等待安静时附带等待结束后的屏幕。
    Sent {
        /// 屏幕快照；没有请求等待时为 `None`。
        screen: Option<TerminalText>,
    },
    /// 等待安静期间用户接管；已发送的输入不撤回。
    TakenOver {
        /// 接管时的屏幕。
        screen: TerminalText,
    },
}

/// MCP 获取连接的方式（spec/mcp.md §2.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectMode {
    /// 只复用已有的已认证连接，没有就返回 [`McpConnectOutcome::NotConnected`]。只读级别用。
    ReuseOnly,
    /// 优先复用；没有时新建，但认证过程中**不向用户提问**，需要人参与就放弃并返回
    /// [`McpConnectOutcome::NeedsUser`]。完全级别用。
    NonInteractive,
}

/// 新建连接时需要人参与的原因（spec/mcp.md §2.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserNeed {
    /// keyboard-interactive（验证码、MFA，或只问密码的 keyboard-interactive）。
    KeyboardInteractive,
    /// 没有保存密码。
    Password,
    /// 私钥有口令且没有保存。
    KeyPassphrase,
    /// 首次见到的主机密钥。
    UnknownHostKey,
    /// 主机密钥已变更。
    ChangedHostKey,
}

/// [`Core::mcp_connect`](crate::Core::mcp_connect) 与
/// [`Core::mcp_open_session`](crate::Core::mcp_open_session) 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpConnectOutcome {
    /// 已连接。
    Connected {
        /// 无界面会话（`mcp_connect`）或新标签（`mcp_open_session`）。
        session: SessionId,
        /// 是否复用了用户建立的连接（借用）。借用连接在用户关闭该连接上最后一个标签时被 core 释放，
        /// 无界面会话随之关闭（spec/mcp.md §2.4）。
        borrowed: bool,
    },
    /// `ReuseOnly` 下没有可用连接。
    NotConnected,
    /// 新建连接需要人参与，已放弃，没有向用户提问。
    NeedsUser(UserNeed),
}

/// [`Core::mcp_exec`](crate::Core::mcp_exec) 的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecOutcome {
    /// 标准输出（已按会话编码转换并去掉 ANSI 控制序列）。
    pub stdout: CapturedText,
    /// 标准错误，处理同上。
    pub stderr: CapturedText,
    /// 退出码；超时或服务器没有报告时为 `None`。
    pub exit_status: Option<u32>,
    /// 是否超时。超时后通道已关闭，但远端进程可能仍在运行（spec/mcp.md §3.4）。
    pub timed_out: bool,
}

/// MCP 判断前置条件与识别客户端所在标签用的会话状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpSessionStatus {
    /// 与 `Core::session_info` 相同的摘要。
    pub info: SessionInfo,
    /// 当前活动（含 AI CLI）。
    pub activity: Activity,
    /// 终端是否已锁定。Kanso 在会话结束时锁定终端（terminal.md §13），所以为真时状态必为
    /// `Closed`，写操作按 `TerminalBusy::NotConnected` 拒绝；单列出来只为 `kanso_list_sessions` 展示。
    pub locked: bool,
    /// 是否在进行 ZMODEM / XMODEM 传输。
    pub terminal_transfer: bool,
    /// Sender 是否正在向它发送。
    pub sender_active: bool,
    /// 当前命令跟踪器拒绝执行命令的原因。
    pub run_blocker: Option<TerminalBusy>,
    /// 是否是 [`Core::mcp_connect`](crate::Core::mcp_connect) 建立的无界面会话。
    pub headless: bool,
    /// 有没有 OSC 133 shell 集成上报。
    pub osc133: bool,
    /// 本地会话 PTY 从设备的路径（例如 `/dev/ttys003`）；其他会话为 `None`。
    /// 用于按控制终端识别客户端所在标签（spec/mcp.md §2.5）。
    pub local_tty: Option<String>,
    /// 本地会话 shell 进程的 pid；其他会话为 `None`。用于按进程祖先链识别客户端所在标签。
    pub shell_pid: Option<u32>,
}

/// SFTP 写文件的模式（spec/mcp.md §3.5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SftpWriteMode {
    /// 仅新建：已存在则失败（`User(SftpFileExists)`），不会覆盖。
    CreateNew,
    /// 覆盖：写临时文件后用 `posix-rename@openssh.com` 替换；目标是符号链接时替换它指向的真实文件；
    /// 保留原文件权限位。服务器没有该扩展时返回 `NotSupported`，不退回到截断写入。
    Overwrite,
}

/// [`Core::mcp_terminal_run`](crate::Core::mcp_terminal_run) 等调用的等待参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitLimits {
    /// 超时。
    pub timeout: Duration,
    /// 单路文本的字节上限，超出保留末尾。
    pub max_bytes: usize,
}

impl Default for WaitLimits {
    fn default() -> Self {
        WaitLimits {
            timeout: Duration::from_secs(30),
            max_bytes: MCP_TEXT_LIMIT,
        }
    }
}

/// 按"保留末尾"的规则截取文本，截断点落在 UTF-8 字符边界上。
///
/// mock 与真实实现共用，保证两者对 `original_bytes`、`truncated` 的计算一致。
pub fn capture_tail(text: &str, max_bytes: usize) -> CapturedText {
    let original_bytes = text.len() as u64;
    if text.len() <= max_bytes {
        return CapturedText {
            text: text.to_owned(),
            original_bytes,
            truncated: false,
            cleared: false,
        };
    }
    let mut start = text.len() - max_bytes;
    // 向后找到字符边界：宁可少保留几个字节，也不能切出半个字符。
    while !text.is_char_boundary(start) {
        start += 1;
    }
    CapturedText {
        text: text[start..].to_owned(),
        original_bytes,
        truncated: true,
        cleared: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_tail_keeps_end_on_char_boundary() {
        let c = capture_tail("short", 10);
        assert!(!c.truncated);
        assert_eq!(c.text, "short");

        // "中" 占 3 字节；从中间切时向后挪到边界。
        let c = capture_tail("ab中文", 4);
        assert!(c.truncated);
        assert_eq!(c.original_bytes, 8);
        assert_eq!(c.text, "文");
    }
}
