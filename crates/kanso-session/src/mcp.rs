//! MCP 对终端的读取、执行与输入（spec/mcp.md §3.2、§3.3、§4.2、§4.3、§5）。本地与 SSH 会话共用。
//!
//! 本 crate 不依赖 `kanso-core`，所以这里用自己的结果类型（`Mcp*`），由 `kanso-core` 转成它的契约类型。
//!
//! # 完成与接管怎么判断
//!
//! 全部靠 [`ActivityHub`] 的状态机，不另起一套：
//! - **完成**：登记命令时记下"已结束命令数"，之后它变大就是这条命令返回了提示符（有 OSC 133 D 用其退出码）；
//! - **接管**：状态机在每次用户来源的输入（`on_input`）上累计计数，登记时记下基线，变大即用户接管。
//!   终端自己的回应、鼠标上报不经过 `on_input`，MCP 自己的输入走 [`ActivityHub::on_mcp_input`]，都不会计数；
//! - **安静**：`on_output` 记录最后一批输出的时刻。
//!
//! 等待用 `watch` 通知唤醒，没有轮询；每次唤醒都重新读状态，所以多唤醒一次无害。
//!
//! # 锁与取消安全
//!
//! - 登记、检查前置条件与写入在同一个临界区里完成（先拿终端传输的锁，再拿终端锁与 tracker 锁，
//!   顺序与 Sender 的 `sender_line` 一致），用户输入在 tracker 锁上与它们串行。
//! - **任何 `await` 点都不持有锁**；async 函数里只有局部变量与 `watch::Receiver`。
//! - future 被丢弃时**不撤销**命令的登记：命令已经发给 shell，撤销只会让状态机以为 shell 停在提示符上，
//!   下一次 `run` 就会把命令塞进还在运行的程序的输入里，并被前一条命令的结束误判为自己完成。
//!   登记本身不会"残留"：它就是普通的"命令运行中"，提示符返回时与用户亲手输入的命令一样被清除；
//!   写入失败这种命令确实没发出去的情形，在登记的同一个临界区里就已经撤销了。

use std::time::Duration;

use kanso_encoding::{TextEncoding, encode_text};
use kanso_term::{InputKind, ReadScope, RunBlock, Terminal, encode_key};
use kanso_types::event::Activity;
use kanso_types::geometry::CellPoint;
use kanso_types::{KeyEvent, Message};
use tokio::time::Instant;

use crate::activity::{ActivityHost, ActivityHub};
use crate::local::{key_options, lock, to_session_encoding};
use crate::transfer::TerminalTransfer;

/// `kanso_terminal_send` 等待输出安静时，在 `settle` 之外最多再多等的时间：
/// 终端持续输出（`yes`、日志）时永远等不到安静，不能让调用一直挂着。
const SETTLE_EXTRA: Duration = Duration::from_secs(10);

/// MCP 需要会话提供的能力。由两类会话的内部共享状态实现。
pub(crate) trait McpHost: ActivityHost {
    /// 命令跟踪。
    fn hub(&self) -> &ActivityHub;
    /// 会话编码。
    fn encoding(&self) -> TextEncoding;
    /// 终端内传输。
    fn transfer(&self) -> &TerminalTransfer;
    /// 会话已连接、没有结束也没有正在被用户关闭。写操作的前提。
    fn connected(&self) -> bool;
    /// 把字节交给写通道；通道已经不在（会话结束）返回 `false`。永不阻塞。
    fn write(&self, bytes: Vec<u8>) -> bool;
    /// 把视口滚回底部（与用户输入一致）。
    fn scroll_to_bottom(&self);
}

/// 标签不能接受 MCP 写操作的原因（不含 Sender，Sender 由 core 判断）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpBusy {
    /// 未连接或已结束（含锁定）。
    NotConnected,
    /// 有命令在运行。
    CommandRunning,
    /// 正在运行 AI CLI。
    AgentRunning,
    /// 输入行不为空。
    InputNotEmpty,
    /// 正在进行终端内传输。
    Transferring,
}

impl From<RunBlock> for McpBusy {
    fn from(b: RunBlock) -> McpBusy {
        match b {
            RunBlock::CommandRunning => McpBusy::CommandRunning,
            RunBlock::AgentRunning => McpBusy::AgentRunning,
            RunBlock::InputNotEmpty => McpBusy::InputNotEmpty,
        }
    }
}

/// 终端屏幕文本与光标等信息。`text` 的规则见 [`Terminal::read_text`]，尚未按大小截取。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpScreen {
    /// 文本。
    pub text: String,
    /// 光标坐标（行、列，从 0 开始）；滚出视口时为空。
    pub cursor: Option<(u16, u16)>,
    /// 屏幕行数。
    pub rows: u16,
    /// 屏幕列数。
    pub cols: u16,
    /// 是否在备用屏幕。
    pub alternate_screen: bool,
}

/// 命令输出，尚未按大小截取。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpOutput {
    /// 文本（规则同选区复制）。
    pub text: String,
    /// 期间清过屏，只保留了清屏之后的部分。
    pub cleared: bool,
}

/// [`McpHost`] 的 `run` 结果，见 `kanso_core::mcp::TerminalRunOutcome`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpRun {
    /// 前置条件不满足，什么也没发送。
    Rejected(McpBusy),
    /// 提示符返回。
    Completed {
        /// 输出。
        output: McpOutput,
        /// 有 OSC 133 D 时的退出码。
        exit_code: Option<i32>,
    },
    /// 超时仍未确认完成。
    Unconfirmed {
        /// 到那时为止的输出。
        output: McpOutput,
        /// 有没有 OSC 133。
        osc133: bool,
    },
    /// 等待期间用户输入。
    TakenOver {
        /// 到那时为止的输出。
        output: McpOutput,
    },
    /// 等待期间会话结束（标签关闭、断开或锁定，spec/mcp.md §4.4），命令可能已经发出。
    Ended,
}

/// MCP 发给终端的一段输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpWrite {
    /// 文字，原样输入（只做会话编码）。
    Text(String),
    /// 一个按键，按终端当前模式编码。
    Key(KeyEvent),
}

/// `send` 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpSend {
    /// 不能输入。
    Rejected(McpBusy),
    /// 已发送；请求了等待安静时附带屏幕。
    Sent {
        /// 屏幕。
        screen: Option<McpScreen>,
    },
    /// 等待期间用户输入。
    TakenOver {
        /// 接管时的屏幕。
        screen: McpScreen,
    },
    /// 等待期间会话结束（标签关闭、断开或锁定，spec/mcp.md §4.4），输入可能已经发出。
    Ended,
}

/// MCP 判断前置条件用的、来自命令跟踪的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpTrackState {
    /// 当前活动。
    pub activity: Activity,
    /// 是否见过 OSC 133。
    pub osc133: bool,
    /// 是否在进行终端内传输。
    pub transfer: bool,
    /// 当前命令跟踪器拒绝执行命令的原因。
    pub run_blocker: Option<McpBusy>,
}

/// 读取屏幕文本；持终端锁的时间与范围成正比。
pub(crate) fn read_screen<H: McpHost>(host: &H, scope: ReadScope) -> McpScreen {
    let term = lock(host.term());
    screen_of(&term, scope)
}

fn screen_of(term: &Terminal, scope: ReadScope) -> McpScreen {
    let cursor = match scope {
        ReadScope::Viewport => term.viewport_cursor(),
        _ => Some(term.screen_cursor()),
    };
    let size = term.size();
    McpScreen {
        text: term.read_text(scope),
        cursor,
        rows: size.rows,
        cols: size.cols,
        alternate_screen: term.modes().alt_screen,
    }
}

/// 命令跟踪相关的状态。
pub(crate) fn track_state<H: McpHost>(host: &H) -> McpTrackState {
    let p = host.hub().progress();
    McpTrackState {
        activity: p.activity,
        osc133: p.osc133,
        transfer: host.transfer().active(),
        run_blocker: host.hub().run_blocker(host).map(McpBusy::from),
    }
}

/// 一次登记成功的命令，等待阶段据此判断完成、接管与输出范围。
struct Ticket {
    /// 登记时的"已结束命令数"。
    finished: u64,
    /// 登记时的用户输入计数。
    user_inputs: u64,
    /// 登记时的整屏清除计数。
    clears: u64,
    /// 登记时光标所在位置（提示符加命令回显的那一行）；全屏期间读不到为 `None`。
    start: Option<CellPoint>,
    /// 能否跟踪到完成：提示符特征为空且没有 OSC 133 时无法判断命令何时返回，只能等到超时。
    tracked: bool,
}

/// 把 `Message` 形式的"现在不能输入"转成 [`McpBusy`]。
fn busy_of(message: &Message) -> McpBusy {
    match message {
        Message::SenderTerminalTransferring {} => McpBusy::Transferring,
        _ => McpBusy::NotConnected,
    }
}

/// 在同一个临界区里检查前置条件、登记并写出命令。
///
/// 临界区 = 终端传输锁（与终端内传输互斥）→ 终端锁 + tracker 锁。用户输入在 tracker 锁上串行：
/// 要么在检查之前（于是被前置条件看到，拒绝），要么在登记之后（属于这条命令，会被判为接管）。
fn begin_run<H: McpHost>(host: &H, command: &str) -> Result<Ticket, McpBusy> {
    let hub = host.hub();
    let started = host.transfer().sender_input(|| {
        if !host.connected() {
            return Ok(Err(McpBusy::NotConnected));
        }
        host.scroll_to_bottom();
        let result = hub.with_tracker(host, |t, term, now| {
            if let Some(block) = t.run_blocker(term) {
                return (Err(McpBusy::from(block)), Vec::new());
            }
            let finished = t.finished();
            let user_inputs = t.user_inputs();
            let clears = term.clear_count();
            let start = term.cursor_point();
            let (registered, mut fx) = t.register_run(command, term, now);
            let tracked = match registered {
                kanso_term::Registered::Command => true,
                kanso_term::Registered::Untracked => false,
                // 前置条件已经保证没有命令和 AI CLI，不会走到这里；万一，当作命令在运行，什么也不发。
                kanso_term::Registered::Busy | kanso_term::Registered::Turn => {
                    return (Err(McpBusy::CommandRunning), fx);
                }
            };
            // 行尾用回车：与用户按 Enter 发出的字节一致（terminal.md §9）。
            let bytes = encode_text(host.encoding(), &format!("{command}\r")).into_owned();
            if !host.write(bytes) {
                // 没发出去，登记必须撤销，否则状态机会一直等一条不存在的命令返回。
                fx.extend(t.cancel_send());
                return (Err(McpBusy::NotConnected), fx);
            }
            (
                Ok(Ticket {
                    finished,
                    user_inputs,
                    clears,
                    start,
                    tracked,
                }),
                fx,
            )
        });
        Ok(result)
    });
    match started {
        Ok(result) => result,
        Err(message) => Err(busy_of(&message)),
    }
}

/// 读取命令输出：登记位置所在逻辑行之后，到 `end`（`None` 时到光标）之前。
fn read_output<H: McpHost>(host: &H, ticket: &Ticket, end: Option<CellPoint>) -> McpOutput {
    let mut term = lock(host.term());
    // 期间清过屏：清屏之前的内容（含命令回显）都没了，现存的全部内容都在清屏之后。
    let cleared = term.clear_count() != ticket.clears;
    let from = if cleared {
        Some(CellPoint {
            line: term.first_line(),
            col: 0,
        })
    } else {
        ticket.start.map(|s| CellPoint {
            line: term.line_after_logical(s.line),
            col: 0,
        })
    };
    let end = end.or_else(|| term.cursor_text_end());
    let text = match (from, end) {
        (Some(from), Some(end)) => term.text_between(from, end),
        // 全屏程序占着屏幕：网格里是它的画面，不是命令输出；让调用方用读取工具看。
        _ => String::new(),
    };
    McpOutput { text, cleared }
}

/// 执行一条命令并等它返回提示符。`command` 必须是一行（由调用方保证）。
///
/// 取消安全，见模块文档。`timeout` 到期返回 [`McpRun::Unconfirmed`]，不向终端发任何东西。
pub(crate) async fn run<H: McpHost>(host: &H, command: &str, timeout: Duration) -> McpRun {
    let hub = host.hub();
    // 先订阅再登记：登记之后的任何变化都能被 `changed()` 看到。
    let mut rx = hub.subscribe();
    let ticket = match begin_run(host, command) {
        Ok(t) => t,
        Err(busy) => return McpRun::Rejected(busy),
    };
    let deadline = Instant::now() + timeout;
    loop {
        let p = hub.progress();
        // 接管优先于完成：用户在等待期间按过键，输出里就可能混着他的输入，要让调用方知道。
        if !p.closed && p.user_inputs > ticket.user_inputs {
            return McpRun::TakenOver {
                output: read_output(host, &ticket, None),
            };
        }
        if !p.closed && ticket.tracked && p.finished > ticket.finished {
            return McpRun::Completed {
                output: read_output(host, &ticket, p.last_finish.end),
                exit_code: p.last_finish.exit_code,
            };
        }
        // 会话结束必须与超时区分：超时意味着命令可能仍在跑，结束意味着调用被中断。
        if p.closed {
            return McpRun::Ended;
        }
        if Instant::now() >= deadline {
            return McpRun::Unconfirmed {
                output: read_output(host, &ticket, None),
                osc133: p.osc133,
            };
        }
        // 到时或被唤醒都回到循环顶重新判断。发送端不会先于接收端消失（hub 由会话持有）；
        // 万一出错只可能是会话没了，按结束处理，避免空转。
        if let Ok(Err(_)) = tokio::time::timeout_at(deadline, rx.changed()).await {
            return McpRun::Ended;
        }
    }
}

/// 向终端输入文字与按键；`settle` 为 `Some` 时等输出安静这么久再返回屏幕。
///
/// 取消安全：写入是同步完成的，等待阶段不持有任何状态。
pub(crate) async fn send<H: McpHost>(
    host: &H,
    inputs: Vec<McpWrite>,
    settle: Option<Duration>,
) -> McpSend {
    let hub = host.hub();
    let mut rx = hub.subscribe();
    let sent = host.transfer().sender_input(|| {
        if !host.connected() {
            return Ok(Err(McpBusy::NotConnected));
        }
        host.scroll_to_bottom();
        let user_inputs = hub.progress().user_inputs;
        for input in &inputs {
            let bytes = match input {
                McpWrite::Text(text) => {
                    for kind in InputKind::of_text(text) {
                        hub.on_mcp_input(host, kind);
                    }
                    encode_text(host.encoding(), text).into_owned()
                }
                McpWrite::Key(event) => {
                    // modes 必须取自发送这一刻（DECCKM 与 kitty 标志随程序随时变化）。
                    let encoded = {
                        let term = lock(host.term());
                        encode_key(event, &term.modes(), &key_options())
                    };
                    let Some(bytes) = encoded else { continue };
                    hub.on_mcp_input(host, InputKind::of_key_bytes(&bytes));
                    to_session_encoding(host.encoding(), bytes)
                }
            };
            if !bytes.is_empty() && !host.write(bytes) {
                return Ok(Err(McpBusy::NotConnected));
            }
        }
        Ok(Ok(user_inputs))
    });
    let user_inputs = match sent {
        Ok(Ok(n)) => n,
        Ok(Err(busy)) => return McpSend::Rejected(busy),
        Err(message) => return McpSend::Rejected(busy_of(&message)),
    };
    let Some(settle) = settle else {
        return McpSend::Sent { screen: None };
    };

    let settle_ms = u64::try_from(settle.as_millis()).unwrap_or(u64::MAX);
    // 安静从**发送完成**起算：之前的沉默不能算，否则回显还没到就会被判为"已经安静"。
    let sent_ms = hub.now();
    let deadline = Instant::now() + settle + SETTLE_EXTRA;
    loop {
        let p = hub.progress();
        if !p.closed && p.user_inputs > user_inputs {
            return McpSend::TakenOver {
                screen: read_screen(host, ReadScope::Screen),
            };
        }
        if p.closed {
            return McpSend::Ended;
        }
        let quiet = hub.now().saturating_sub(p.last_output_ms.max(sent_ms));
        let now = Instant::now();
        if quiet >= settle_ms || now >= deadline {
            return McpSend::Sent {
                screen: Some(read_screen(host, ReadScope::Screen)),
            };
        }
        let wait = Duration::from_millis(settle_ms - quiet).min(deadline - now);
        if let Ok(Err(_)) = tokio::time::timeout(wait, rx.changed()).await {
            return McpSend::Ended;
        }
    }
}
