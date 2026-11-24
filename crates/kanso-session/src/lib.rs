//! 会话管理器与每会话的 IO 与状态：串联 PTY/SSH、编码、传输、终端状态，并负责 Sender 与传输队列。
//!
//! 目前实现的是**本地会话**（I1）与 **SSH 会话**（I2）：[`LocalSession`] / [`SshSession`] 各自把
//! 数据源（本地 PTY / SSH shell 通道）、编码转换、终端内核（`kanso-term`）串成一条完整链路；
//! [`SessionManager`] 持有全部会话，UI 只拿 `SessionId`（architecture §7）。
//! SSH 会话的任务模型见 `ssh.rs` 的模块文档（tokio 任务，不是 std 线程）；
//! Sender 与传输队列由后续工作包加入。
//!
//! 下文"线程划分"一节描述本地会话。
//!
//! # 本 crate 不依赖 `kanso-core`
//!
//! 依赖方向只能自上而下（architecture §2.2），而事件出口、通知发送端这些平台 trait 定义在
//! `kanso-core`。所以本 crate 只定义自己需要的最小接口 [`SessionHost`]，由 `kanso-core` 用
//! `Platform` 实现它。
//!
//! # 数据流
//!
//! ```text
//!   输出方向（读线程）
//!   PTY ──read──▶ StreamDecoder ──▶ lock Terminal::feed ──▶ unlock ──▶ 事件 / 唤醒
//!                                          │                              │
//!                                          └─ take_replies ──▶ 写通道     └─ TerminalDirty（合并）
//!
//!   输入方向（调用线程，通常是主线程）
//!   key / text_input / paste ──▶ 编码（encode_key / encode_paste → encode_text）──▶ 写通道 ──▶ 写线程 ──▶ PTY
//!   mouse / scroll / focus   ──▶ Terminal 处理（Report / ArrowKeys / 回应）─────────┘
//! ```
//!
//! # 命令跟踪与 AI 轮次
//!
//! 两类会话共用 [`activity`] 模块的 `ActivityHub`，它包着 `kanso_term::CommandTracker` 状态机
//! （terminal.md §5、§6）：
//!
//! ```text
//!   用户输入（key / text_input / paste，写出之前） ──▶ hub.on_input ──┐
//!   每批输出（feed 之后，含 OSC 133 事件）          ──▶ hub.on_output ─┼─▶ CommandTracker ─▶ Effect
//!   定时线程（2 秒无输出、延迟读命令文本）          ──▶ hub.on_tick ───┘        │
//!                                    ActivityChanged / 后台完成通知（命令或 AI 轮次）◀──┘
//! ```
//!
//! 终端回应与鼠标上报绕过编码器直接写出，不经过 `on_input`，所以不算用户输入。
//! 有 OSC 133 时以它为准，没有（SSH、非 zsh）时按提示符特征推断。
//!
//! # 线程划分
//!
//! 每个本地会话有两个 **std 线程**，没有用 tokio 任务：
//!
//! - **读线程**（local-shell.md §7）：阻塞读 PTY → 解码 → 加锁 `Terminal::feed` → 取出
//!   `take_replies` 与 `take_events` → **释放锁** → 把回应写回 PTY、处理事件、必要时唤醒 UI。
//!   阻塞式的 PTY 读放在 tokio 里要么占用 `spawn_blocking` 线程池、要么引入 PTY 的异步封装，
//!   二者都不比一个专用线程简单；每个会话一个线程在标签数量（几十）下完全可以接受。
//!   读到 EOF 后进入"会话结束"流程。
//! - **写线程**：输入与终端回应都先放进一个通道，由它负责 `write`。目的是**调用方线程
//!   （主线程）调用 `key` / `text_input` 时永不阻塞**：shell 不读输入、PTY 缓冲区写满时，
//!   阻塞的只是写线程。读线程发回应也走这个通道，所以读线程永远不会因为写 PTY 而阻塞
//!   （否则会出现"我们阻塞在写、shell 阻塞在写输出"的互相等待）。
//!
//! # 锁
//!
//! - `Mutex<Terminal>`：临界区**只包含** `feed`、`snapshot`、选区 / 搜索 / 滚动等 `Terminal` 方法
//!   （都是 CPU 计算，1 ms 量级，architecture §4）；绝不在持锁时做 IO、发事件、调用平台回调。
//! - `Mutex<PtySession>`：只保护 `resize` / `kill` / `try_wait` / `foreground_cwd` 这些很快的调用。
//!   写入端在启动时就用 `PtySession::take_writer` 交给了写线程，所以写阻塞不会占着这把锁。
//! - 生命周期锁（`Shared::lifecycle`）：保护会话状态，并且**发事件时持有它**，用来保证
//!   "`Closed` 之后不会再有别的事件"。持锁期间只调用 `SessionHost::emit`，其契约要求立即返回、
//!   不得回调 core，所以不会死锁。
//! - 加锁顺序：生命周期锁 → 其他锁从不嵌套；`Terminal` 锁与 `PtySession` 锁从不同时持有。
//!
//! # 唤醒合并
//!
//! `dirty: AtomicBool`：feed / resize / 选区变化后，只有它由 `false` 变 `true` 时才发一次
//! `TerminalDirty`；`take_frame` 把它清回 `false`。洪水输出时读线程一直在 feed，但 UI 每取一次帧
//! 才会产生一次事件。
//!
//! # 绕过编码器的写入
//!
//! 终端回应（DA、CPR、焦点通知、颜色查询应答）和鼠标 / 滚轮上报是协议字节，经过编码器会被改写
//! （`kanso-encoding` 的模块文档同理），所以它们**原样**写 PTY；并且它们不是用户输入，
//! 不触发"滚回底部"（terminal.md §5.2）。只有用户的按键、IME 上屏、粘贴才经过 `encode_text`。
//!
//! # 会话结束的两种方式
//!
//! shell 自己退出（读线程读到 EOF → 取退出码 → `Closed(Exited)`）与用户关闭（`close_session` →
//! 先标记"用户关闭"→ 杀进程 → `Closed(ByUser)`）可能并发。两条路径最终都经过
//! `Shared::finish`，它在生命周期锁内检查状态：只有第一个调用者能把状态改成 `Closed` 并发事件，
//! 另一个什么也不发——保证不重复也不遗漏。用户关闭路径额外有一个 `user_closing` 标志，
//! 在 kill 之前置位，读线程看到它就放弃自己的结束流程（否则 kill 引起的 EOF 会抢先发出
//! `Exited`，把用户关闭误报成进程退出）。置位之后读线程**继续读到 EOF 但丢弃数据**：macOS 上 shell
//! 退出时关闭 tty 要等已写出的输出被主端读走，没人读就会卡在内核里，SIGKILL 也唤不醒，kill 的 `wait`
//! 永远不返回。

mod activity;
mod git;
mod host;
mod local;
mod manager;
mod mcp;
mod ssh;
mod transfer;

pub use git::git_branch;
pub use host::SessionHost;
pub use kanso_pty::EnvSnapshot;
pub use kanso_term::{ReadScope, Registered, SearchQuery, SearchStatus};
pub use local::ReadStats;
pub use local::{LocalLaunch, LocalSession};
pub use manager::{ManagerConfig, OpenError, OpenRequest, Session, SessionManager, SessionSummary};
pub use mcp::{McpBusy, McpOutput, McpRun, McpScreen, McpSend, McpTrackState, McpWrite};
pub use ssh::{BoxFuture, IdleKeepalive, SshConnector, SshLaunch, SshSession};
