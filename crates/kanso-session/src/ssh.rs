//! SSH 会话：一个 SSH shell 通道，加上它的编码转换与终端状态（ssh.md §6）。
//!
//! 与 [`crate::LocalSession`] 并列，终端侧的约定（锁、脏标记合并、绕过编码器的写入、
//! 会话结束只发一次 `Closed`）完全一致，见 crate 文档；这里只写 SSH 特有的部分。
//!
//! # 任务模型
//!
//! SSH 是 async，所以没有 std 线程，全部跑在 core 的 tokio 运行时上：
//!
//! - **主任务**（`run`）：连接（含主机密钥变更确认后的自动重连）→ 开 shell 通道 → 读循环 → 会话结束流程。
//!   读循环里的 `Terminal::feed` 是 1 ms 量级的纯计算，直接在任务里做；临界区约定同本地会话。
//! - **写任务**（`write_loop`）：输入、终端回应、窗口尺寸变化都先放进一个无界通道，由它按顺序写通道。
//!   调用方（主线程）因此永不阻塞；读循环发回应也走这个通道，不会因写而卡住读
//!   （否则会出现"我们阻塞在写、对端阻塞在写输出"的互相等待）。
//! - **收尾线程**（`close_by_user`）：用户关闭时需要 `await` 关通道，而 `SessionManager::close`
//!   是同步的，所以用一个 std 线程 `block_on`，句柄交给管理器等待（`shutdown` 用）。
//!
//! # 连接的所有权
//!
//! 会话持有一份 `Connection`（只用来给"复制标签"提供共享连接），会话结束（任何原因）时立刻释放。
//! `Connection` 内部是 `Arc`：通道的读写端也各持一份，最后一个持有者释放才真正断开。
//! 所以**不得在别处额外持有 `Connection`**，否则关闭全部标签后连接不会断。

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kanso_encoding::{StreamDecoder, TextEncoding, encode_text};
use kanso_frame::Frame;
use kanso_ssh::{ChannelWriter, Connection, PtyRequest, SftpClient, SshError};
use kanso_term::{
    InputKind, MouseOutcome, Registered, ScrollAction, SearchQuery, SearchStatus, ShellFlavor,
    TermConfig, TermEvent, Terminal, encode_key, encode_paste, encode_scroll, quote_paths,
};
use kanso_types::agent::AgentKind;
use kanso_types::event::{Activity, CloseReason, SessionState};
use kanso_types::message::Endpoint;
use kanso_types::profile::{EncodingLabel, Locale, SessionKind};
use kanso_types::{
    CoreEvent, GridSize, KeyEvent, Modifiers, MouseEvent, PasteContent, SessionId, TerminalOptions,
    TimestampMode, ViewportPoint,
};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;

use crate::activity::{ActivityHost, ActivityHub};
use crate::host::SessionHost;
use crate::local::{DEFAULT_SCROLLBACK_LINES, key_options, lock, to_session_encoding, unix_ms};

/// 关闭会话时等待远端通道关闭的上限；对端无响应不能卡住收尾（应用退出路径也会走这里）。
const CHANNEL_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// 通道读到结束、但没有收到退出码时，等连接断开原因落定的时间。
///
/// 对端掉线时通道结束与连接记录"断开原因"之间有一小段间隔；shell 自己退出但没带退出码（被信号杀死）时
/// 连接并不会断，等满这个时间后按"退出码未知"结束。
const CLOSE_REASON_WAIT: Duration = Duration::from_millis(300);

/// 装箱的 `Send` future，用于 trait 对象里的异步方法。
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// 建立 SSH 连接的能力，由 `kanso-core` 实现并随 [`SshLaunch`] 注入。
///
/// 凭据读取、向用户提问（keyboard-interactive / 缺失凭据 / 主机密钥变更）、记住凭据、
/// known_hosts 都属于 core 的职责（会话层不认识 Profile、钥匙串与提示通道），
/// 会话层只负责"连上之后的一切"与重连流程的编排。
///
/// - 调用者：SSH 会话的主任务（tokio 工作线程）；两个方法都可能等很久（等用户输入）。
/// - 取消：会话被用户关闭时主任务会被 abort，返回的 future 随之被丢弃；实现必须在被丢弃时
///   清理自己挂起的提问（core 靠 Drop guard 发 `PromptDismissed`）。
pub trait SshConnector: Send + Sync + 'static {
    /// 连接并认证，成功返回已认证的连接。
    ///
    /// 每次调用都是一次完整的新连接（重连时会再次调用）。
    fn connect(&self, session: SessionId) -> BoxFuture<Result<Connection, SshError>>;

    /// 主机密钥变更：`connect` 失败且错误是 [`SshError::HostKeyChanged`] 时调用。
    ///
    /// 实现向用户确认；用户接受则**已经**写入新密钥（`accept_changed_key`），返回 `Ok(true)`，
    /// 会话层随即重新调用 `connect`；用户拒绝、取消或超时返回 `Ok(false)`；
    /// 写入 known_hosts 失败返回 `Err`，会话以该错误关闭。
    fn confirm_host_key_change(
        &self,
        session: SessionId,
        error: SshError,
    ) -> BoxFuture<Result<bool, SshError>>;
}

/// 空闲保活参数：由 core 从 Profile 解析好（含转义展开）后传给会话层。
#[derive(Clone, Debug)]
pub struct IdleKeepalive {
    /// 网络空闲多久后发送一次。
    pub interval: Duration,
    /// 空闲时写进 shell 的字节。
    pub bytes: Vec<u8>,
}

/// SSH 会话的启动参数。`Duplicate` 复用源会话的这份参数。
#[derive(Clone)]
pub struct SshLaunch {
    /// 显示名（没有 Profile 名时用，通常是 `user@host`）。
    pub name: String,
    /// 连接对象，用于"共享连接已断开"这类没有现成 `SshError` 的错误。
    pub endpoint: Endpoint,
    /// 会话编码。输入与输出两个方向共用这一个值（`kanso-encoding` 模块文档要求）。
    pub encoding: TextEncoding,
    /// 用户明确填写的 locale；`Some` 时才随 PTY 请求发送 `LANG`。
    pub locale: Option<Locale>,
    /// 建立连接的能力。
    pub connector: Arc<dyn SshConnector>,
    /// 纯 SFTP 会话（ssh.md §7）：连接、认证、跳板与 SSH 会话完全相同，只是不开 shell、不产生终端帧。
    /// 复制会话时随启动参数一起复制，所以"复制 SFTP 会话"得到的仍是 SFTP 会话。
    pub sftp_only: bool,
    /// 空闲保活（按 session 配置）；`None` 表示不启用。仅终端会话会真正启动定时器。
    pub idle_keepalive: Option<IdleKeepalive>,
}

impl SshLaunch {
    /// 构造启动参数。编码标签在这里解析一次（未知标签回退 UTF-8，`kanso-encoding` 的约定），
    /// 之后输入与输出两个方向都用解析出的同一个值。
    pub fn new(
        name: String,
        endpoint: Endpoint,
        encoding: &EncodingLabel,
        locale: Option<Locale>,
        connector: Arc<dyn SshConnector>,
    ) -> SshLaunch {
        SshLaunch {
            name,
            endpoint,
            encoding: TextEncoding::resolve(encoding),
            locale,
            connector,
            sftp_only: false,
            idle_keepalive: None,
        }
    }

    /// 改成纯 SFTP 会话（不开 shell）。
    pub fn sftp_only(mut self) -> SshLaunch {
        self.sftp_only = true;
        self
    }

    /// 启用空闲保活：网络空闲达到配置的间隔时，向 shell 写入指定字节。
    pub fn idle_keepalive(mut self, cfg: IdleKeepalive) -> Self {
        self.idle_keepalive = Some(cfg);
        self
    }
}

impl std::fmt::Debug for SshLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshLaunch")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("encoding", &self.encoding)
            .field("locale", &self.locale)
            .field("sftp_only", &self.sftp_only)
            .field("idle_keepalive", &self.idle_keepalive)
            .finish_non_exhaustive()
    }
}

/// 会话的连接来源。
pub(crate) enum Source {
    /// 新建连接（含认证）。
    Connect,
    /// 复用另一个会话的已认证连接；`None` 表示源会话已经没有可用连接。
    Shared(Option<Connection>),
}

/// 写任务的指令。
enum WriteCmd {
    /// 原样写通道的字节（已经按会话编码处理过，或是不该被编码器改写的协议字节）。
    Data(Vec<u8>),
    /// 终端尺寸变化（window-change）。
    Resize(u16, u16),
}

/// 读循环与调用线程共享的会话状态。
struct Shared {
    history_host: String,
    transfer: Arc<crate::transfer::TerminalTransfer>,
    id: SessionId,
    host: Arc<dyn SessionHost>,
    encoding: TextEncoding,
    /// 终端状态。临界区只放 `Terminal` 的方法，见 crate 文档"锁"一节。
    term: Mutex<Terminal>,
    /// 当前终端尺寸。开 shell 通道时用它，所以连接期间的 resize 不会丢。
    size: Mutex<GridSize>,
    /// 唤醒合并标志：`false → true` 时发一次 `TerminalDirty`，`take_frame` 清除。
    dirty: AtomicBool,
    visible: AtomicBool,
    /// 用户已经请求关闭：读循环据此放弃自己的结束流程。
    user_closing: AtomicBool,
    ended: AtomicBool,
    /// 会话状态；**发事件时持有它**，保证事件保序且 `Closed` 之后不再有事件。
    lifecycle: Mutex<SessionState>,
    /// 写任务的发送端；会话结束后取走（`None`），写任务随之退出。
    write_tx: Mutex<Option<mpsc::UnboundedSender<WriteCmd>>>,
    scroll_remainder: Mutex<f32>,
    timestamp_mode: Mutex<Option<TimestampMode>>,
    /// 命令跟踪与 AI 轮次（terminal.md §5、§6）。SSH 会话没有 OSC 133，全靠提示符推断。
    activity: Arc<ActivityHub>,
    /// 本会话持有的连接，只给"复制标签"与 SFTP 用；会话结束即释放。
    connection: Mutex<Option<Connection>>,
    /// 本会话用过的连接的 [`Connection::id`]；0 表示还没有连接（id 从 1 开始分配）。
    ///
    /// 与 `connection` 不同，会话结束、释放连接之后**仍然保留**：core 判断"刚结束的会话用的是哪条连接"
    /// （借用连接的释放）时，那时连接引用早已放掉。
    connection_id: AtomicU64,
    /// 会话是否已"尘埃落定"：到过 `Connected` 或已经 `Closed`。只增不减，供 [`SshSession::wait_settled`] 异步等待。
    /// 用 watch 而不是轮询状态：状态变化点（`set_connected`、`finish`）都在这个结构里，顺手通知即可。
    settled: watch::Sender<bool>,
    /// 本会话的 SFTP 客户端，首次请求时才创建（见 [`SshSession::sftp_client`]）；会话结束即释放。
    sftp: Mutex<Option<SftpClient>>,
    /// 通道写端的一份克隆，只用来在用户关闭时发 close。
    channel: Mutex<Option<ChannelWriter>>,
    /// 主任务与写任务，关闭时 abort。
    tasks: Mutex<Vec<AbortHandle>>,
    /// 最近一次活动（用户输入或远端输出）的毫秒时间戳；空闲保活定时器据此判断。
    last_activity: AtomicU64,
}

impl Shared {
    fn term(&self) -> MutexGuard<'_, Terminal> {
        lock(&self.term)
    }

    /// 发事件。会话已经 `Closed` 时丢弃（`finish` 自己发的那一个除外）。
    fn emit(&self, event: CoreEvent) {
        let state = lock(&self.lifecycle);
        if matches!(*state, SessionState::Closed(_)) {
            return;
        }
        self.host.emit(event);
    }

    /// 置脏；只有从干净变脏时才发 `TerminalDirty`（唤醒合并，理由同本地会话）。
    fn mark_dirty(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            self.emit(CoreEvent::TerminalDirty { session: self.id });
        }
    }

    /// 在终端锁内执行 `f`，然后（释放锁之后）转发终端因此产生的回应并按需置脏。
    fn with_term<R>(&self, f: impl FnOnce(&mut Terminal) -> R) -> R {
        let (result, replies, dirty) = {
            let mut t = self.term();
            let r = f(&mut t);
            (r, t.take_replies(), t.is_dirty())
        };
        self.send(replies);
        if dirty {
            self.mark_dirty();
        }
        result
    }

    /// 把字节交给写任务。还没连上或已结束（没有发送端）时丢弃。永不阻塞。
    fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        // 有字节要写说明有活动（用户输入或终端回应），空闲计时重新开始。
        self.last_activity
            .store(unix_ms() as u64, Ordering::Release);
        if let Some(tx) = lock(&self.write_tx).as_ref() {
            // 写任务已退出时 `send` 返回 Err，说明会话已经结束，丢弃即可。
            let _ = tx.send(WriteCmd::Data(bytes));
        }
    }

    fn input_blocked(&self) -> bool {
        self.ended.load(Ordering::Acquire)
            || self.user_closing.load(Ordering::Acquire)
            || self.transfer.active()
    }

    /// 改成 `Closed(reason)` 并发事件。**只有第一个调用者返回 `true`**（裁决点，理由同本地会话）。
    fn finish(&self, reason: CloseReason) -> bool {
        self.transfer.close(match &reason {
            CloseReason::Error(message) => message.clone(),
            _ => kanso_types::Message::XferSessionEnded {},
        });
        let first = {
            let mut state = lock(&self.lifecycle);
            if matches!(*state, SessionState::Closed(_)) {
                return false;
            }
            let new_state = SessionState::Closed(reason);
            *state = new_state.clone();
            self.ended.store(true, Ordering::Release);
            self.host.emit(CoreEvent::SessionStateChanged {
                session: self.id,
                state: new_state,
            });
            // 状态已写入后才通知：等待方被唤醒后读到的一定是 `Closed`。
            self.settled.send_replace(true);
            true
        };
        // 必须在释放生命周期锁之后（理由见 `local.rs` 的 `Shared::finish`）：
        // 重置跟踪状态并唤醒等待命令返回的调用方。
        self.activity.close();
        first
    }

    /// `Connecting → Connected`；状态已不是 `Connecting`（用户已关闭）时返回 `false`。
    fn set_connected(&self) -> bool {
        let mut state = lock(&self.lifecycle);
        if *state != SessionState::Connecting {
            return false;
        }
        *state = SessionState::Connected;
        self.host.emit(CoreEvent::SessionStateChanged {
            session: self.id,
            state: SessionState::Connected,
        });
        self.settled.send_replace(true);
        true
    }

    /// 主机密钥变更被接受后重新连接：状态仍是 `Connecting`，但再发一次事件，
    /// 让 UI 把"等待确认"的界面切回"连接中"。
    fn announce_reconnecting(&self) {
        let state = lock(&self.lifecycle);
        if *state == SessionState::Connecting {
            self.host.emit(CoreEvent::SessionStateChanged {
                session: self.id,
                state: SessionState::Connecting,
            });
        }
    }

    /// 会话结束后释放对连接的全部引用：写通道、连接、通道写端。
    ///
    /// 必须在 `finish` 之后调用；"最后一个使用者关闭才断开"靠各会话及时放手。
    fn release(&self) {
        lock(&self.write_tx).take();
        lock(&self.channel).take();
        lock(&self.connection).take();
        lock(&self.sftp).take();
    }
}

impl crate::mcp::McpHost for Shared {
    fn hub(&self) -> &ActivityHub {
        &self.activity
    }

    fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    fn transfer(&self) -> &crate::transfer::TerminalTransfer {
        &self.transfer
    }

    fn connected(&self) -> bool {
        *lock(&self.lifecycle) == SessionState::Connected
            && !self.ended.load(Ordering::Acquire)
            && !self.user_closing.load(Ordering::Acquire)
    }

    fn write(&self, bytes: Vec<u8>) -> bool {
        lock(&self.write_tx)
            .as_ref()
            .is_some_and(|writer| writer.send(WriteCmd::Data(bytes)).is_ok())
    }

    fn scroll_to_bottom(&self) {
        self.with_term(Terminal::scroll_to_bottom);
    }
}

impl ActivityHost for Shared {
    fn command_submitted(&self, text: &str) {
        self.host.record_command(text, &self.history_host);
    }

    fn term(&self) -> &Mutex<Terminal> {
        &self.term
    }

    fn emit_activity(&self, activity: Activity) {
        self.emit(CoreEvent::ActivityChanged {
            session: self.id,
            activity,
        });
    }

    fn command_completed(&self, exit_code: Option<i32>, agent: Option<AgentKind>) {
        // 会话不可见时才通知；可见时 UI 自己看得到。
        if self.visible.load(Ordering::Acquire) {
            return;
        }
        match agent {
            Some(kind) => self.host.agent_turn_done_in_background(self.id, kind),
            None => self.host.command_finished_in_background(self.id, exit_code),
        }
    }
}

/// 一个 SSH 会话。由 [`crate::SessionManager`] 持有；UI 只拿 `SessionId`。
pub struct SshSession {
    shared: Arc<Shared>,
    launch: SshLaunch,
    runtime: Handle,
    /// 连接来源，`activate` 时取走交给主任务。
    source: Mutex<Option<Source>>,
    /// 无界面会话（MCP 的 `mcp_connect`）：不出现在会话列表，事件由管理器注入的过滤 host 处理。
    headless: bool,
}

impl SshSession {
    /// 创建会话，但还不发事件、不启动任务；随后必须调用 [`SshSession::activate`]。
    pub(crate) fn new(
        id: SessionId,
        host: Arc<dyn SessionHost>,
        launch: SshLaunch,
        size: GridSize,
        runtime: Handle,
        source: Source,
        headless: bool,
    ) -> Arc<SshSession> {
        // 终端至少 2 列 1 行。
        let size = GridSize {
            cols: size.cols.max(2),
            rows: size.rows.max(1),
        };
        let shared = Arc::new(Shared {
            history_host: launch.endpoint.host.clone(),
            transfer: crate::transfer::TerminalTransfer::new(id, Arc::clone(&host)),
            id,
            host,
            encoding: launch.encoding,
            term: Mutex::new(Terminal::new(TermConfig {
                size,
                scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            })),
            size: Mutex::new(size),
            dirty: AtomicBool::new(false),
            visible: AtomicBool::new(true),
            user_closing: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            lifecycle: Mutex::new(SessionState::Connecting),
            write_tx: Mutex::new(None),
            scroll_remainder: Mutex::new(0.0),
            timestamp_mode: Mutex::new(None),
            activity: ActivityHub::new(),
            connection: Mutex::new(None),
            connection_id: AtomicU64::new(0),
            settled: watch::Sender::new(false),
            sftp: Mutex::new(None),
            channel: Mutex::new(None),
            tasks: Mutex::new(Vec::new()),
            last_activity: AtomicU64::new(unix_ms() as u64),
        });
        // 共享来源的连接在这里就已确定：同步记下 id，不等主任务被调度，
        // 这样"开了就立刻关"的标签也能被 core 认出用的是哪条连接。
        if let Source::Shared(Some(c)) = &source {
            shared.connection_id.store(c.id(), Ordering::Release);
        }
        let weak = Arc::downgrade(&shared);
        shared.transfer.set_writer(Arc::new(move |bytes| {
            let shared = weak.upgrade().ok_or("session ended")?;
            let writer = lock(&shared.write_tx);
            writer
                .as_ref()
                .ok_or_else(|| "session ended".to_string())?
                .send(WriteCmd::Data(bytes))
                .map_err(|error| error.to_string())
        }));
        Arc::new(SshSession {
            shared,
            launch,
            runtime,
            source: Mutex::new(Some(source)),
            headless,
        })
    }

    /// 发出 `Connecting` 与首个 `TerminalDirty`，然后启动主任务。
    ///
    /// 必须在管理器登记会话**之后**调用：任务一启动就可能发事件，UI 收到后会立刻调用 core。
    pub(crate) fn activate(&self) {
        {
            let state = lock(&self.shared.lifecycle);
            self.shared.host.emit(CoreEvent::SessionStateChanged {
                session: self.shared.id,
                state: state.clone(),
            });
        }
        // 纯 SFTP 会话没有终端，不产生帧，也就不该唤醒 UI 去取帧。
        if !self.launch.sftp_only {
            self.shared.mark_dirty();
            // 定时线程负责"2 秒无输出"与"延迟读命令文本"；会话结束时 `Shared::finish` 会让它退出。
            self.shared.activity.spawn_timer(
                Arc::downgrade(&self.shared),
                format!("kanso-activity-{}", self.shared.id),
            );
        }
        let Some(source) = lock(&self.source).take() else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        let launch = self.launch.clone();
        let task = self.runtime.spawn(run(shared, launch, source));
        lock(&self.shared.tasks).push(task.abort_handle());
    }

    /// 会话 id。
    pub fn id(&self) -> SessionId {
        self.shared.id
    }

    /// 显示名。
    pub fn name(&self) -> &str {
        &self.launch.name
    }

    /// 启动参数，`Duplicate` 用。
    pub fn launch(&self) -> &SshLaunch {
        &self.launch
    }

    /// 当前状态。
    pub fn state(&self) -> SessionState {
        lock(&self.shared.lifecycle).clone()
    }

    /// 是否是无界面会话。
    pub fn is_headless(&self) -> bool {
        self.headless
    }

    /// 本会话用过的连接的 id（[`Connection::id`]）；连接还没建立（或共享来源已断）时为 `None`。
    ///
    /// 连接建立后一直保留，**会话结束、释放连接之后也不清除**：借用连接的释放要靠它识别
    /// "刚结束的标签用的是哪条连接"。
    pub fn connection_id(&self) -> Option<u64> {
        match self.shared.connection_id.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }

    /// 等到会话连上（`Connected`）或已经结束（`Closed`），返回此刻的状态。
    ///
    /// 取消安全（只是等待一个 watch）。返回 `Connected` 只代表"返回那一刻之前连上过"，
    /// 调用方若要长期依赖，仍需自己处理之后的 `Closed`。
    pub async fn wait_settled(&self) -> SessionState {
        let mut rx = self.shared.settled.subscribe();
        // 发送端在 `Shared` 里，与 `self` 同生共死，所以这里不会因发送端关闭而出错。
        let _ = rx.wait_for(|settled| *settled).await;
        self.state()
    }

    /// 会话种类：SSH 终端或纯 SFTP。
    pub fn kind(&self) -> SessionKind {
        if self.launch.sftp_only {
            SessionKind::Sftp
        } else {
            SessionKind::Ssh
        }
    }

    /// 本会话的 SFTP 客户端（克隆共享同一条子系统通道）。
    ///
    /// 每个会话一个客户端，首次调用时才基于会话持有的连接创建（引擎再在第一次请求时才开子系统）。
    /// 复制标签得到的会话共用同一条连接，但各有自己的客户端与子系统通道：
    /// 传输的取消、会话关闭时的清理都以会话为单位，共享客户端要额外处理"最后一个使用者"的生命周期，
    /// 而一条连接上多开一个 sftp 通道的代价可以忽略。
    ///
    /// 还没连上或会话已结束（没有连接）返回 `SharedConnectionClosed`：不偷偷重连（ssh.md §7）。
    /// 调用方持有返回的克隆会让连接多活一段时间，因此长期持有者（目录流、传输）必须在会话关闭时放手。
    pub fn sftp_client(&self) -> Result<SftpClient, SshError> {
        let mut slot = lock(&self.shared.sftp);
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        let Some(conn) = lock(&self.shared.connection).clone() else {
            return Err(SshError::SharedConnectionClosed {
                endpoint: self.launch.endpoint.clone(),
            });
        };
        let client = SftpClient::new(conn);
        *slot = Some(client.clone());
        Ok(client)
    }

    /// 本会话持有的已认证连接（克隆共享）；没连上或已结束为 `None`。
    ///
    /// 给 `Duplicate`、core 的连接复用与 MCP 的 exec 用：调用方拿到后要么立刻交给新会话，
    /// 要么只在一次短操作内持有，不得长期持有（见模块文档，否则关闭全部标签后连接不会断）。
    pub fn connection(&self) -> Option<Connection> {
        lock(&self.shared.connection).clone()
    }

    // ---- 输入（约定同本地会话：调用线程编码、丢进写通道、永不阻塞，会话结束后丢弃）----

    /// 一次按键。
    pub fn key(&self, event: &KeyEvent) {
        if self.shared.input_blocked() {
            return;
        }
        let opts = key_options();
        let bytes = self.shared.with_term(|t| {
            let bytes = encode_key(event, &t.modes(), &opts);
            if bytes.is_some() {
                t.scroll_to_bottom();
            }
            bytes
        });
        if let Some(b) = bytes {
            // 先登记用户输入、再写出：登记要读写出之前的光标位置，见 `ActivityHub::on_input`。
            self.shared
                .activity
                .on_input(&*self.shared, InputKind::of_key_bytes(&b));
            self.shared
                .send(to_session_encoding(self.shared.encoding, b));
        }
    }

    /// 取消终端传输；id 为 None 时取消本会话当前传输。
    pub fn cancel_terminal_transfer(&self, id: Option<kanso_types::TransferId>) {
        self.shared.transfer.cancel(id);
    }

    /// 通过当前终端启动 XMODEM。
    pub fn xmodem(
        &self,
        sending: bool,
        one_k: bool,
        path: Option<&str>,
    ) -> Result<kanso_types::TransferId, kanso_types::Message> {
        self.shared.transfer.xmodem(sending, one_k, path)
    }

    /// IME 上屏等直接文本输入：不经过按键编码，只做会话编码。
    pub fn text_input(&self, text: &str) {
        if text.is_empty() || self.shared.input_blocked() {
            return;
        }
        self.shared.with_term(Terminal::scroll_to_bottom);
        let kinds = InputKind::of_text(text);
        if kinds.is_empty() {
            // 理由同本地会话：目录选择器的私有序列仍是用户的操作。
            self.shared.activity.note_user_input();
        }
        for kind in kinds {
            self.shared.activity.on_input(&*self.shared, kind);
        }
        self.shared
            .send(encode_text(self.shared.encoding, text).into_owned());
    }

    /// 粘贴。
    pub fn paste(&self, content: &PasteContent) {
        if self.shared.input_blocked() {
            return;
        }
        match content {
            PasteContent::Text(text) => self.paste_text(text),
            // 规则同本地会话：AI CLI 会话原样粘贴路径，远端普通 shell 按 POSIX 转义（terminal.md §8.3）。
            PasteContent::Files(paths) => {
                let text = if self.shared.activity.agent().is_some() {
                    paths.join(" ")
                } else {
                    quote_paths(paths, ShellFlavor::Posix)
                };
                self.paste_text(&text);
            }
            PasteContent::ImageOnly => self.paste_image_key(),
        }
    }

    fn paste_image_key(&self) {
        if self.shared.activity.agent().is_none() {
            return;
        }
        self.shared.with_term(Terminal::scroll_to_bottom);
        self.shared
            .activity
            .on_input(&*self.shared, InputKind::Typing);
        self.shared.send(vec![0x16]);
    }

    fn paste_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = self.shared.with_term(|t| {
            let bytes = encode_paste(text, &t.modes());
            t.scroll_to_bottom();
            bytes
        });
        self.shared
            .activity
            .on_input(&*self.shared, InputKind::of_paste(text));
        self.shared
            .send(to_session_encoding(self.shared.encoding, bytes));
    }

    // ---- 命令跟踪（Sender 用；目前没有调用方，接 UI 是 F2 的事）----

    /// 发送前登记"即将发送某条命令"，以当前提示符作为完成特征（terminal.md §5.3）。
    ///
    /// 调用方保证：先登记、后写会话；写失败时必须调用 [`SshSession::cancel_send`]。
    /// 返回 [`Registered::Busy`] 说明上一条命令还没返回提示符。
    pub fn register_send(&self, text: &str) -> Registered {
        self.shared.activity.register_send(&*self.shared, text)
    }

    /// 撤销 [`SshSession::register_send`] 的登记。
    pub fn cancel_send(&self) {
        self.shared.activity.cancel_send(&*self.shared);
    }

    /// 检查 Sender 输入是否可用，并返回当前命令是否结束。
    pub fn sender_ready(&self) -> Result<bool, kanso_types::Message> {
        if self.kind() == SessionKind::Sftp
            || self.shared.input_blocked()
            || self.state() != SessionState::Connected
        {
            return Err(if self.shared.transfer.active() {
                kanso_types::Message::SenderTerminalTransferring {}
            } else {
                kanso_types::Message::SenderTerminalUnavailable {}
            });
        }
        Ok(self.wait_command_idle(Duration::ZERO))
    }

    /// 与终端传输互斥地写入一行，返回命令能否跟踪。
    pub fn sender_line(&self, text: &str, ending: &str) -> Result<bool, kanso_types::Message> {
        self.shared.transfer.sender_input(|| {
            if self.shared.ended.load(Ordering::Acquire)
                || self.shared.user_closing.load(Ordering::Acquire)
            {
                return Err(kanso_types::Message::SenderTerminalUnavailable {});
            }
            let tracked = !ending.is_empty() && self.register_send(text) == Registered::Command;
            self.shared.with_term(Terminal::scroll_to_bottom);
            let bytes = encode_text(self.shared.encoding, &format!("{text}{ending}")).into_owned();
            let sent = lock(&self.shared.write_tx)
                .as_ref()
                .is_some_and(|writer| writer.send(WriteCmd::Data(bytes)).is_ok());
            if !sent {
                self.cancel_send();
                return Err(kanso_types::Message::SenderWriteFailed {});
            }
            Ok(tracked)
        })
    }

    /// 等待当前命令返回提示符，最多 `timeout`；返回等待结束时是否已空闲。
    /// 没有命令在运行时立即返回 `true`；会话结束会立即唤醒并返回 `true`。
    pub fn wait_command_idle(&self, timeout: Duration) -> bool {
        self.shared.activity.wait_idle(timeout)
    }

    /// 命令跟踪与终端传输的状态（MCP 判断前置条件用）。
    pub fn mcp_track_state(&self) -> crate::mcp::McpTrackState {
        crate::mcp::track_state(&*self.shared)
    }

    /// 会话是否已结束（含用户正在关闭）；终端此时被锁定（terminal.md §13）。
    pub fn is_locked(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire)
            || self.shared.user_closing.load(Ordering::Acquire)
    }

    /// 读取终端文本，规则见 `kanso_term::Terminal::read_text`。纯 SFTP 会话没有终端，调用方须先排除。
    pub fn mcp_read(&self, scope: kanso_term::ReadScope) -> crate::mcp::McpScreen {
        crate::mcp::read_screen(&*self.shared, scope)
    }

    /// 输入一行命令并等它返回提示符，见 `mcp` 模块文档。取消安全。
    pub async fn mcp_run(&self, command: &str, timeout: Duration) -> crate::mcp::McpRun {
        crate::mcp::run(&*self.shared, command, timeout).await
    }

    /// 输入文字与按键，可选等输出安静，见 `mcp` 模块文档。取消安全。
    pub async fn mcp_send(
        &self,
        inputs: Vec<crate::mcp::McpWrite>,
        settle: Option<Duration>,
    ) -> crate::mcp::McpSend {
        crate::mcp::send(&*self.shared, inputs, settle).await
    }

    /// 鼠标事件：程序开启了鼠标上报时转成上报序列写回，否则由终端处理选区。
    pub fn mouse(&self, event: &MouseEvent) {
        let outcome = self.shared.with_term(|t| t.handle_mouse(event));
        match outcome {
            MouseOutcome::Report(bytes) => {
                if !self.shared.input_blocked() {
                    self.shared.send(bytes);
                }
            }
            MouseOutcome::SelectionFinished { .. } | MouseOutcome::None => {}
        }
    }

    /// 滚轮 / 触摸板滚动，小数行累积到下一次（同本地会话）。
    pub fn scroll(&self, delta_lines: f32, at: ViewportPoint, mods: Modifiers) {
        if !delta_lines.is_finite() {
            return;
        }
        let lines = {
            let mut rem = lock(&self.shared.scroll_remainder);
            *rem += delta_lines;
            let whole = rem.trunc();
            *rem -= whole;
            whole.clamp(i32::MIN as f32, i32::MAX as f32) as i32
        };
        if lines == 0 {
            return;
        }
        let action = self.shared.with_term(|t| {
            let action = encode_scroll(lines, at, mods, &t.modes());
            if let ScrollAction::ScrollViewport(n) = &action {
                t.scroll_display(*n);
            }
            action
        });
        match action {
            ScrollAction::Report(bytes) | ScrollAction::ArrowKeys(bytes) => {
                if !self.shared.input_blocked() {
                    self.shared.send(bytes);
                }
            }
            ScrollAction::ScrollViewport(_) => {}
        }
    }

    /// 键盘焦点变化。
    pub fn focus(&self, focused: bool) {
        self.shared.with_term(|t| t.set_focused(focused));
    }

    // ---- 尺寸、可见性、显示 ----

    /// 调整终端尺寸，并通知服务器（window-change）。
    ///
    /// 像素尺寸只用于终端内核；`kanso-ssh` 的 window-change 不带像素。
    /// 还没连上时只记录：开通道时用最新的尺寸。
    pub fn resize(&self, size: GridSize, cell_width_px: u16, cell_height_px: u16) {
        let size = GridSize {
            cols: size.cols.max(2),
            rows: size.rows.max(1),
        };
        self.shared.with_term(|t| {
            t.set_cell_pixel_size(cell_width_px, cell_height_px);
            t.resize(size);
        });
        *lock(&self.shared.size) = size;
        if self.shared.user_closing.load(Ordering::Acquire) {
            return;
        }
        if let Some(tx) = lock(&self.shared.write_tx).as_ref() {
            let _ = tx.send(WriteCmd::Resize(size.cols, size.rows));
        }
    }

    /// 标签是否可见。
    pub fn set_visible(&self, visible: bool) {
        self.shared.visible.store(visible, Ordering::Release);
    }

    /// 设置本会话的时间戳显示模式（目前只存储）。
    pub fn set_timestamp_mode(&self, mode: TimestampMode) {
        *lock(&self.shared.timestamp_mode) = Some(mode);
    }

    /// 本会话单独设置过的时间戳显示模式。
    pub fn timestamp_mode(&self) -> Option<TimestampMode> {
        *lock(&self.shared.timestamp_mode)
    }

    // ---- 选区、搜索、缓冲区 ----

    /// 是否有选区。
    pub fn has_selection(&self) -> bool {
        self.shared.term().has_selection()
    }

    /// 提取指定命令块的文本。
    pub fn command_block_text(&self, id: u64, part: u8) -> Option<String> {
        self.shared.term().command_block_text(id, part)
    }

    /// 滚动到相邻命令块，不改变选区。
    pub fn jump_command_block(&self, forward: bool) -> Option<u64> {
        self.shared
            .with_term(|term| term.jump_command_block(forward))
    }

    /// 选区文本。
    pub fn selection_text(&self) -> Option<String> {
        self.shared.term().selection_text()
    }

    /// 选中视口内某处的单词。
    pub fn select_word_at(&self, at: ViewportPoint) {
        self.shared.with_term(|t| t.select_word_at(at));
    }

    /// 全选。
    pub fn select_all(&self) {
        self.shared.with_term(Terminal::select_all);
    }

    /// 清除选区。
    pub fn clear_selection(&self) {
        self.shared.with_term(Terminal::clear_selection);
    }

    /// 新会话在 `activate` 之前应用显示选项。与 `set_terminal_options` 的区别：不发 `TerminalDirty`——
    /// 事件必须从 `Connecting` 开始（契约：`open_session`），此时终端本来就是脏的，首帧会被正常取走。
    pub(crate) fn init_terminal_options(&self, options: TerminalOptions) {
        let mut t = self.shared.term();
        t.set_scrollback_lines(options.scrollback_lines as usize);
        t.set_default_cursor(options.cursor_shape, options.cursor_blink);
        t.set_semantic_coloring(options.semantic_coloring);
    }

    /// 应用显示选项：回滚上限（调小立即丢弃超出的行）与默认光标样式（terminal.md §15）。
    pub fn set_terminal_options(&self, options: TerminalOptions) {
        self.shared.with_term(|t| {
            t.set_scrollback_lines(options.scrollback_lines as usize);
            t.set_default_cursor(options.cursor_shape, options.cursor_blink);
            t.set_semantic_coloring(options.semantic_coloring);
        });
    }

    /// 清空屏幕、回滚与行元数据。
    pub fn clear_buffer(&self) {
        self.shared.with_term(Terminal::clear_buffer);
    }

    /// 按帧里的超链接 id 查 URL。
    pub fn hyperlink_url(&self, id: u32) -> Option<String> {
        self.shared.term().hyperlink_url(id).map(str::to_owned)
    }

    /// 开始或更新搜索。
    pub fn search(&self, query: &SearchQuery) -> SearchStatus {
        self.shared.with_term(|t| t.search(query))
    }

    /// 只读取已维护的搜索状态，不发送置脏事件。
    pub fn search_status(&self) -> SearchStatus {
        self.shared.term().search_status()
    }

    /// 跳到下一个 / 上一个匹配。
    pub fn search_next(&self, backwards: bool) -> SearchStatus {
        self.shared.with_term(|t| t.search_next(backwards))
    }

    /// 关闭搜索并清除高亮。
    pub fn search_close(&self) {
        self.shared.with_term(Terminal::search_close);
    }

    /// 远端当前目录：只用终端记录的 OSC 7 / 1337 / 9;9 上报。
    /// 没有上报时为 `None`；此时由调用方再用 [`SshSession::query_remote_working_directory`] 到服务器上查。
    pub fn working_directory(&self) -> Option<String> {
        self.shared.term().cwd().map(|c| c.path.clone())
    }

    /// 当前命令行的光标前输入；传输或终端锁定时不可用。
    pub fn completion_input(&self) -> Option<(String, u16)> {
        if self.shared.input_blocked() {
            return None;
        }
        self.shared.activity.completion_input(&*self.shared)
    }

    /// 当前 SSH 连接的目标主机，供历史排序使用。
    pub fn endpoint_host(&self) -> &str {
        &self.shared.history_host
    }

    /// 最新工作目录上报，随会话结束与重连清空。
    pub fn reported_directory(&self) -> Option<kanso_term::CwdReport> {
        self.shared.term().cwd().cloned()
    }

    /// 没有 OSC 7 上报时，在**同一连接**上另开 exec 通道查询 shell 的当前目录（ssh.md §10）。
    ///
    /// 只在用户主动请求时调用（只查一次）；内部有 5 秒超时。查询期间临时持有一份连接克隆，
    /// 因此最后一个标签恰好在查询中途关闭时，断开最多推迟到查询结束。
    /// 会话还没连上或已结束（没有连接）时返回 `Unavailable`。
    pub async fn query_remote_working_directory(&self) -> Result<String, kanso_ssh::CwdError> {
        let Some(connection) = self.connection() else {
            return Err(kanso_ssh::CwdError::Unavailable {
                detail: "session has no connection".into(),
            });
        };
        kanso_ssh::query_working_directory(&connection).await
    }

    /// 取最新帧；契约与清标志、产帧的顺序理由同 [`crate::LocalSession::take_frame`]。
    pub fn take_frame(&self, reuse: &mut Option<Frame>) -> Option<Frame> {
        if !self.shared.dirty.swap(false, Ordering::AcqRel) {
            return None;
        }
        let old = reuse.take();
        Some(self.shared.term().snapshot(old))
    }

    // ---- 关闭 ----

    /// 用户关闭会话：先标记"用户关闭"、发 `Closed(ByUser)`，再在收尾线程里关通道、停任务、放开连接。
    /// 不阻塞调用线程，返回收尾线程的句柄（`shutdown` 用它等待）。
    ///
    /// 先发事件再收尾：这里没有"进程必须先死"的要求（对端 shell 的生死不归我们管），
    /// 立即告知 UI 比等网络往返更好；收尾慢（对端无响应）也不影响 UI。
    pub(crate) fn close_by_user(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        // 必须先于任何收尾动作：读循环据此放弃自己的"通道结束"流程，避免把用户关闭误报成 shell 退出。
        self.shared.user_closing.store(true, Ordering::Release);
        self.shared.finish(CloseReason::ByUser);
        lock(&self.source).take();
        let shared = Arc::clone(&self.shared);
        let runtime = self.runtime.clone();
        let work = move || teardown(&shared, &runtime);
        let shared = Arc::clone(&self.shared);
        match thread::Builder::new()
            .name(format!("kanso-ssh-close-{}", self.shared.id))
            .spawn(work)
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::error!(session = %shared.id, error = %e, "failed to spawn ssh closer thread, aborting tasks inline");
                abort_tasks(&shared);
                shared.release();
                None
            }
        }
    }
}

fn abort_tasks(shared: &Shared) {
    for t in lock(&shared.tasks).drain(..) {
        t.abort();
    }
}

/// 收尾：关通道（有超时）→ abort 任务 → 释放连接。
///
/// 在 std 线程里用 `block_on`：管理器的 `close` 是同步的，而关通道要 `await`。
/// 最后一次释放连接放在运行时上下文里做：`Connection` 的 `Drop` 需要运行时来发 disconnect。
fn teardown(shared: &Arc<Shared>, runtime: &Handle) {
    let channel = lock(&shared.channel).take();
    runtime.block_on(async {
        if let Some(ch) = channel {
            // 连接已断时 close 会失败，不需要处理。
            let _ = tokio::time::timeout(CHANNEL_CLOSE_TIMEOUT, ch.close()).await;
        }
        abort_tasks(shared);
        shared.release();
    });
}

/// 主任务：连接 → 开 shell → 读循环 → 结束流程。
async fn run(shared: Arc<Shared>, launch: SshLaunch, source: Source) {
    let connection = match source {
        Source::Shared(Some(c)) => c,
        Source::Shared(None) => {
            // 源会话已经结束或还没连上：不偷偷重连（ssh.md §6）。
            let err = SshError::SharedConnectionClosed {
                endpoint: launch.endpoint.clone(),
            };
            shared.finish(CloseReason::Error(err.to_message()));
            return;
        }
        Source::Connect => match connect_with_retry(&shared, &launch).await {
            Some(c) => c,
            None => return,
        },
    };
    shared
        .connection_id
        .store(connection.id(), Ordering::Release);

    if launch.sftp_only {
        run_sftp(&shared, &launch, connection).await;
        return;
    }

    let size = *lock(&shared.size);
    let pty = PtyRequest {
        cols: size.cols,
        rows: size.rows,
        lang: launch.locale.clone(),
    };
    let shell = match connection.open_shell(pty).await {
        Ok(s) => s,
        Err(e) => {
            shared.finish(CloseReason::Error(e.to_message()));
            return;
        }
    };
    let (mut reader, writer) = shell.into_parts();

    let (tx, rx) = mpsc::unbounded_channel();
    *lock(&shared.write_tx) = Some(tx);
    *lock(&shared.channel) = Some(writer.clone());
    *lock(&shared.connection) = Some(connection.clone());
    // 安装之后再检查一次：用户关闭可能发生在"开通道"期间，那时收尾线程看不到这些引用。
    if shared.user_closing.load(Ordering::Acquire) {
        let _ = writer.close().await;
        shared.release();
        return;
    }
    let writer_task = tokio::spawn(write_loop(Arc::clone(&shared), writer.clone(), rx));
    lock(&shared.tasks).push(writer_task.abort_handle());

    // 空闲保活：每最小粒度 tick 检查一次，达到配置的空闲间隔就往 shell 写保活字节。
    // 定时器持有 write_tx 的克隆，句柄放进 shared.tasks，会话关闭时随之 abort。
    // 纯 SFTP 分支已经在上面 return（没有 shell channel），即便配置了也不会走到这里。
    if let Some(idle) = &launch.idle_keepalive {
        shared
            .last_activity
            .store(unix_ms() as u64, Ordering::Release);
        let tx = lock(&shared.write_tx)
            .as_ref()
            .cloned()
            .expect("write_tx 已在上面安装");
        let bytes = idle.bytes.clone();
        let interval_ms = idle.interval.as_millis() as u64;
        let tick = idle.interval.min(Duration::from_secs(1));
        let timer_shared = Arc::clone(&shared);
        let timer_task = tokio::spawn(async move {
            let mut timer = tokio::time::interval(tick);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                if timer_shared.ended.load(Ordering::Acquire)
                    || timer_shared.user_closing.load(Ordering::Acquire)
                {
                    break;
                }
                let now = unix_ms() as u64;
                if now.saturating_sub(timer_shared.last_activity.load(Ordering::Acquire))
                    >= interval_ms
                {
                    // 先刷新时间戳：保活字节的远端回显也是输出，不能让它触发连发。
                    timer_shared.last_activity.store(now, Ordering::Release);
                    if tx.send(WriteCmd::Data(bytes.clone())).is_err() {
                        break;
                    }
                }
            }
        });
        lock(&shared.tasks).push(timer_task.abort_handle());
    }

    if !shared.set_connected() {
        shared.release();
        return;
    }
    // 连接期间用户可能已经改过窗口大小（开通道用的是当时的值），补发一次最新的。
    let latest = *lock(&shared.size);
    if latest != size {
        let _ = lock(&shared.write_tx)
            .as_ref()
            .map(|tx| tx.send(WriteCmd::Resize(latest.cols, latest.rows)));
    }
    // 整条跳板链上每一跳都单独协商算法，用到旧算法的都要列出（ssh.md §2.2、§5）。
    let legacy_hops = connection.legacy_hops();
    if !legacy_hops.is_empty() {
        shared.emit(CoreEvent::LegacyAlgorithms {
            session: shared.id,
            hops: legacy_hops,
        });
    }
    shared.mark_dirty();

    read_loop(&shared, &mut reader).await;
    on_channel_end(&shared, &connection, &writer, &launch.endpoint).await;
}

/// 纯 SFTP 会话的主任务：不开 shell，登记连接、置 `Connected`，然后一直等到连接断开。
///
/// 存活检测直接等 `Connection::wait_closed`（事件驱动：对端关闭或 keepalive 超时都会触发），
/// 比每 5 秒轮询 `is_closed` 更及时，也不需要额外的定时器。
/// 会话持有的连接引用与终端会话一样只用于 SFTP 与复制；任务自己也持有一份，任务结束（或被 abort）即放手。
async fn run_sftp(shared: &Shared, launch: &SshLaunch, connection: Connection) {
    *lock(&shared.connection) = Some(connection.clone());
    // 安装之后再检查：用户关闭可能发生在连接期间，那时收尾线程看不到这份引用。
    if shared.user_closing.load(Ordering::Acquire) {
        shared.release();
        return;
    }
    if !shared.set_connected() {
        shared.release();
        return;
    }
    let legacy_hops = connection.legacy_hops();
    if !legacy_hops.is_empty() {
        shared.emit(CoreEvent::LegacyAlgorithms {
            session: shared.id,
            hops: legacy_hops,
        });
    }
    let why = connection.wait_closed().await;
    if shared.user_closing.load(Ordering::Acquire) {
        // 用户关闭路径负责发事件和收尾。
        return;
    }
    // 与 `close_by_user` 并发时由 `finish` 裁决，只有一个会发事件。
    shared.finish(CloseReason::Error(
        why.to_error(&launch.endpoint).to_message(),
    ));
    shared.release();
}

/// 连接；主机密钥变更时问用户，接受后在同一个会话上重连。
///
/// 返回 `None` 表示会话已经以对应原因关闭（或被用户关闭）。
async fn connect_with_retry(shared: &Shared, launch: &SshLaunch) -> Option<Connection> {
    loop {
        match launch.connector.connect(shared.id).await {
            Ok(c) => return Some(c),
            Err(err @ SshError::HostKeyChanged { .. }) => {
                match launch
                    .connector
                    .confirm_host_key_change(shared.id, err.clone())
                    .await
                {
                    Ok(true) => shared.announce_reconnecting(),
                    Ok(false) => {
                        shared.finish(CloseReason::Error(err.to_message()));
                        return None;
                    }
                    Err(e) => {
                        shared.finish(CloseReason::Error(e.to_message()));
                        return None;
                    }
                }
            }
            Err(e) => {
                shared.finish(CloseReason::Error(e.to_message()));
                return None;
            }
        }
    }
}

/// 写任务：把通道里的指令按顺序写给服务器。
///
/// 写失败说明连接已断：立即结束会话（ssh.md §6），错误区分 keepalive 超时与其他中断。
async fn write_loop(
    shared: Arc<Shared>,
    writer: ChannelWriter,
    mut rx: mpsc::UnboundedReceiver<WriteCmd>,
) {
    while let Some(cmd) = rx.recv().await {
        let result = match cmd {
            WriteCmd::Data(bytes) => writer.write(&bytes).await,
            WriteCmd::Resize(cols, rows) => writer.resize(cols, rows).await,
        };
        if let Err(e) = result {
            if !shared.user_closing.load(Ordering::Acquire) {
                shared.finish(CloseReason::Error(e.to_message()));
                shared.release();
                // 读循环会因连接断开自行结束，但写失败不一定伴随读结束：主动停掉它。
                if let Some(main) = lock(&shared.tasks).first() {
                    main.abort();
                }
            }
            return;
        }
    }
}

/// 终端输出出口串行维护的状态，读任务与文件面板恢复输出共用。
struct ReaderState {
    /// 上一次发出的位置信息 `(cwd, host)`，用来去重：shell 每个提示符都会上报 OSC 7。
    last_location: Option<(String, Option<String>)>,
}

fn terminal_output(shared: &Arc<Shared>) -> Arc<dyn Fn(Vec<u8>) + Send + Sync> {
    let weak = Arc::downgrade(shared);
    let output_state = Mutex::new((
        StreamDecoder::new(shared.encoding),
        String::new(),
        ReaderState {
            last_location: None,
        },
        0i64,
    ));
    Arc::new(move |bytes| {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let mut output_state = lock(&output_state);
        let (decoder, text, state, last_ms) = &mut *output_state;
        text.clear();
        decoder.decode(&bytes, text);
        if text.is_empty() {
            // 只读到了半个多字节字符，等下一块。
            return;
        }
        // 时间戳不能随系统时间回拨而倒退（kanso-term 不检查）。
        let now = unix_ms().max(*last_ms);
        *last_ms = now;

        // 内核的缺陷（panic）不能让会话悄悄卡死：捕获后按"异常结束"处理，与本地会话一致。
        let processed = catch_unwind(AssertUnwindSafe(|| {
            let (replies, events) = {
                let mut t = shared.term();
                t.feed(text.as_bytes(), now);
                (t.take_replies(), t.take_events())
            };
            // 以下在释放终端锁之后：回应原样写回（不经过编码器）、置脏唤醒、处理事件。
            shared.send(replies);
            shared.mark_dirty();
            for event in &events {
                handle_term_event(&shared, state, event);
            }
            // 命令跟踪与 AI 轮次：每批输出都要通知，不论有没有事件（理由见本地会话的读循环）。
            shared.activity.on_output(&*shared, &events);
        }));
        if processed.is_err() {
            tracing::error!(session = %shared.id, "ssh terminal reader panicked");
            if !shared.user_closing.load(Ordering::Acquire) {
                shared.finish(CloseReason::Exited { code: None });
                shared.release();
            }
        }
    })
}

async fn read_loop(shared: &Arc<Shared>, reader: &mut kanso_ssh::ChannelReader) {
    let output = terminal_output(shared);
    shared.transfer.set_terminal_output(Arc::clone(&output));
    while let Some(chunk) = reader.read().await {
        // 用户已经请求关闭：丢弃数据，不再喂终端、不再发事件。
        if shared.user_closing.load(Ordering::Acquire) {
            continue;
        }
        // 远端有输出也算活动：空闲保活不应打断正在工作的会话。
        shared
            .last_activity
            .store(unix_ms() as u64, Ordering::Release);
        shared.transfer.feed(&chunk);
    }
}

/// `TermEvent` → `CoreEvent`。与本地会话的差别：远端目录不在本机，不查 git 分支。
/// 活动（运行 / 空闲 / AI 轮次）由 `ActivityHub::on_output` 处理：SSH 会话没有 OSC 133 时靠提示符推断。
fn handle_term_event(shared: &Shared, state: &mut ReaderState, event: &TermEvent) {
    match event {
        TermEvent::CwdChanged(report) => {
            let location = (report.path.clone(), report.host.clone());
            if state.last_location.as_ref() != Some(&location) {
                state.last_location = Some(location.clone());
                shared.emit(CoreEvent::LocationChanged {
                    session: shared.id,
                    cwd: Some(location.0),
                    cwd_host: location.1,
                    git_branch: None,
                });
            }
        }
        TermEvent::CommandStart { .. }
        | TermEvent::PromptStart { .. }
        | TermEvent::InputStart { .. }
        | TermEvent::CommandFinished { .. } => {}
        TermEvent::TitleChanged | TermEvent::Bell => {}
        // OSC 52：默认不放行，理由同本地会话（任何能往终端输出文字的程序都能触发）。
        TermEvent::ClipboardStore(_) => {}
    }
}

/// 通道读到结束之后的会话结束流程：判断是"shell 退出"还是"连接出错"。
///
/// 有退出码一定是 shell 自己退出（有的服务器退出后紧接着断开连接，不能因此报成连接错误）；
/// 没有退出码时看连接是否随之断开：断开按断开原因报错，没断按"退出码未知"结束。
async fn on_channel_end(
    shared: &Shared,
    connection: &Connection,
    writer: &ChannelWriter,
    endpoint: &Endpoint,
) {
    if shared.user_closing.load(Ordering::Acquire) {
        // 用户关闭路径负责发事件和收尾。
        return;
    }
    let reason = match writer.exit_code() {
        Some(code) => CloseReason::Exited {
            code: i32::try_from(code).ok(),
        },
        None => match tokio::time::timeout(CLOSE_REASON_WAIT, connection.wait_closed()).await {
            Ok(why) => CloseReason::Error(why.to_error(endpoint).to_message()),
            Err(_) => CloseReason::Exited { code: None },
        },
    };
    // 与 `close_by_user` 并发时由 `finish` 裁决，只有一个会发事件。
    shared.finish(reason);
    shared.release();
}
