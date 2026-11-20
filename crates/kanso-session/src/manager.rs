//! 会话管理器：持有全部会话，对外只暴露 `SessionId`（architecture §7）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kanso_encoding::TextEncoding;
use kanso_frame::Frame;
use kanso_pty::EnvSnapshot;
use kanso_ssh::Connection;
use kanso_term::{SearchQuery, SearchStatus};
use kanso_types::event::SessionState;
use kanso_types::profile::{LocalProfile, SessionKind};
use kanso_types::{
    GridSize, KeyEvent, Modifiers, MouseEvent, PasteContent, SessionId, TerminalOptions,
    TimestampMode, ViewportPoint,
};

use crate::host::{HeadlessHost, SessionHost};
use crate::local::{LocalLaunch, LocalSession, StartError, join_with_timeout};
use crate::ssh::{Source, SshLaunch, SshSession};

/// 会话管理器的配置，由 `Core::new` 的 `CoreConfig` 转换而来。
#[derive(Clone, Debug)]
pub struct ManagerConfig {
    /// 数据目录。zsh shim 写在它下面的 `shell-integration/zsh/`。
    pub data_dir: PathBuf,
    /// 应用版本号，写入 `TERM_PROGRAM_VERSION`。
    pub app_version: String,
    /// 系统首选语言（如 `zh-Hans-CN`），推算 locale 用（local-shell.md §4）。
    pub preferred_language: String,
    /// 系统地区（如 `CN`），推算 locale 用。
    pub region: Option<String>,
    /// 默认 shell 的命令行；`None` 取 `$SHELL`（没有则 `/bin/bash`）。
    pub default_shell: Option<String>,
    /// 是否启用 zsh 提示符集成（local-shell.md §5.1 的开关）。
    pub shell_integration: bool,
    /// 子进程环境的基线；`None` 用本进程的环境。
    ///
    /// 存在的唯一理由是**测试隔离**：测试要把 `HOME`、`ZDOTDIR` 等指向临时目录，而修改本进程的环境变量
    /// 既不安全（`set_var` 在多线程下是 unsafe）也会影响并行的其他测试。
    pub base_env: Option<EnvSnapshot>,
}

/// 打开会话的请求。按 Profile 打开需要存储，由 core 在更上层查出 Profile 后转成
/// [`OpenRequest::Local`] / [`OpenRequest::Ssh`]。
///
/// 不再实现 `PartialEq`：SSH 变体带着建立连接的能力（trait 对象），没有有意义的相等性。
#[derive(Clone, Debug)]
pub enum OpenRequest {
    /// 默认配置的本地 shell。
    DefaultLocalShell,
    /// 按本地 Profile 的配置打开：shell 命令行、起始目录、编码、locale。
    /// 空的 `shell_command` 等同默认 shell；编码标签在这里解析一次（未知标签回退 UTF-8）。
    Local(LocalProfile),
    /// 新建 SSH 会话：解析好的连接能力与终端参数（编码、locale）。
    /// 连接在后台进行，`open` 立即返回 id，结果通过事件通知（`Closed(Error(..))` 等）。
    Ssh(SshLaunch),
    /// 复制一个会话：本地会话用同一份启动参数重新启动；
    /// SSH 会话在**同一个已认证连接**上再开一个 shell 通道，不重新握手认证（ssh.md §6）。
    Duplicate(SessionId),
    /// 新建**可见**的 SSH / 纯 SFTP 会话，但连接用调用方给的这一条（不新建、不认证），
    /// 相当于"在别处已认证的连接上再开一个标签"（core 用它做按 Profile 的连接复用）。
    /// `launch` 里的连接器此时不会被调用；连接已断时新会话以 `SshSharedConnectionClosed` 关闭。
    SshShared {
        /// 启动参数（决定开 shell 还是纯 SFTP）。
        launch: SshLaunch,
        /// 要共享的连接。
        connection: Connection,
    },
    /// 新建**无界面**会话（MCP 的 `mcp_connect`）：机制同纯 SFTP 会话（不开 shell、不产生终端帧），
    /// 不出现在 [`SessionManager::sessions`] 里，也不向 UI 发会话类事件，只放行传输事件。
    /// `launch` 会被强制改成纯 SFTP；`connection` 为 `Some` 时共享它，否则用 `launch` 的连接器新建。
    /// 无界面会话不重连：连接断了就以 `Closed` 结束。
    Headless {
        /// 启动参数。
        launch: SshLaunch,
        /// 共享的连接；`None` 表示新建。
        connection: Option<Connection>,
    },
}

/// 打开会话失败。
#[derive(Debug)]
pub enum OpenError {
    /// 被复制的会话不存在（或已被关闭）。
    NoSuchSession(SessionId),
    /// 命令行无法解析、PTY 无法打开或 shell 无法启动。
    Spawn {
        /// 要启动的程序（尽力而为：命令行都无法解析时是原始命令行）。
        command: String,
        /// 英文技术细节，只进日志。
        detail: String,
    },
    /// 管理器已经 `shutdown`。
    ShuttingDown,
}

/// 会话摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    /// 会话种类（本地、SSH 终端或纯 SFTP）。
    pub kind: SessionKind,
    /// 会话 id。
    pub id: SessionId,
    /// 显示名。
    pub name: String,
    /// 当前状态。
    pub state: SessionState,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 一个会话：本地或 SSH。管理器内部与 core 用它统一处理两类会话，克隆只复制 `Arc`。
#[derive(Clone)]
pub enum Session {
    /// 本地 shell。
    Local(Arc<LocalSession>),
    /// SSH 终端。
    Ssh(Arc<SshSession>),
}

/// 把两类会话上签名相同的方法转发出去，避免手写两遍分发。
macro_rules! forward {
    ($($(#[$m:meta])* fn $name:ident(&self $(, $a:ident : $t:ty)*) $(-> $r:ty)?;)*) => {
        impl Session {
            $(
                $(#[$m])*
                pub fn $name(&self $(, $a: $t)*) $(-> $r)? {
                    match self {
                        Session::Local(s) => s.$name($($a),*),
                        Session::Ssh(s) => s.$name($($a),*),
                    }
                }
            )*
        }
    };
}

forward! {
    /// 会话 id。
    fn id(&self) -> SessionId;
    /// 当前状态。
    fn state(&self) -> SessionState;
    /// 一次按键。
    fn key(&self, event: &KeyEvent);
    /// 直接文本输入。
    fn text_input(&self, text: &str);
    /// 检查 Sender 输入状态和命令完成状态。
    fn sender_ready(&self) -> Result<bool, kanso_types::Message>;
    /// 写入一行 Sender 内容，与终端内传输互斥。
    fn sender_line(&self, text: &str, ending: &str) -> Result<bool, kanso_types::Message>;
    /// 粘贴。
    fn paste(&self, content: &PasteContent);
    /// 鼠标事件。
    fn mouse(&self, event: &MouseEvent);
    /// 滚动。
    fn scroll(&self, delta_lines: f32, at: ViewportPoint, mods: Modifiers);
    /// 键盘焦点变化。
    fn focus(&self, focused: bool);
    /// 取消当前终端传输，或取消匹配 id 的传输。
    fn cancel_terminal_transfer(&self, id: Option<kanso_types::TransferId>);
    /// 手动启动 XMODEM，未指定路径时异步询问并暂存字节。
    fn xmodem(&self, sending: bool, one_k: bool, path: Option<&str>) -> Result<kanso_types::TransferId, kanso_types::Message>;
    /// 调整尺寸。
    fn resize(&self, size: GridSize, cell_width_px: u16, cell_height_px: u16);
    /// 标签可见性。
    fn set_visible(&self, visible: bool);
    /// 设置本会话的时间戳显示模式。
    fn set_timestamp_mode(&self, mode: TimestampMode);
    /// 应用显示选项（回滚上限、默认光标样式）。
    fn set_terminal_options(&self, options: TerminalOptions);
    /// 是否有选区。
    fn has_selection(&self) -> bool;
    /// 选区文本。
    fn selection_text(&self) -> Option<String>;
    /// 提取命令块文本。
    fn command_block_text(&self, id: u64, part: u8) -> Option<String>;
    /// 滚动到相邻命令块。
    fn jump_command_block(&self, forward: bool) -> Option<u64>;
    /// 选中单词。
    fn select_word_at(&self, at: ViewportPoint);
    /// 全选。
    fn select_all(&self);
    /// 清除选区。
    fn clear_selection(&self);
    /// 清空缓冲区。
    fn clear_buffer(&self);
    /// 超链接 URL。
    fn hyperlink_url(&self, id: u32) -> Option<String>;
    /// 搜索。
    fn search(&self, query: &SearchQuery) -> SearchStatus;
    /// 只读取已维护的搜索状态。
    fn search_status(&self) -> SearchStatus;
    /// 下一个匹配。
    fn search_next(&self, backwards: bool) -> SearchStatus;
    /// 关闭搜索。
    fn search_close(&self);
    /// 工作目录。本地可能查系统（阻塞），调用方按需放进 `spawn_blocking`。
    fn working_directory(&self) -> Option<String>;
    /// 命令行输入起点与光标前文字。
    fn completion_input(&self) -> Option<(String, u16)>;
    /// 最新工作目录上报及主机。
    fn reported_directory(&self) -> Option<kanso_term::CwdReport>;
    /// 取最新帧。
    fn take_frame(&self, reuse: &mut Option<Frame>) -> Option<Frame>;
}

impl Session {
    fn name(&self) -> &str {
        match self {
            Session::Local(s) => s.name(),
            Session::Ssh(s) => s.name(),
        }
    }

    /// 是否是无界面会话（MCP 的 `mcp_connect`）。本地会话恒为 `false`。
    pub fn is_headless(&self) -> bool {
        match self {
            Session::Local(_) => false,
            Session::Ssh(s) => s.is_headless(),
        }
    }

    /// 会话用过的 SSH 连接的 id（[`Connection::id`]），会话结束后仍保留；本地会话与尚无连接的会话为 `None`。
    pub fn connection_id(&self) -> Option<u64> {
        match self {
            Session::Local(_) => None,
            Session::Ssh(s) => s.connection_id(),
        }
    }

    /// 会话**当前**持有的 SSH 连接（克隆共享）；本地会话、没连上或已结束为 `None`。
    /// 调用方只能在一次短操作内持有，理由见 [`SshSession::connection`]。
    pub fn connection(&self) -> Option<Connection> {
        match self {
            Session::Local(_) => None,
            Session::Ssh(s) => s.connection(),
        }
    }

    /// 会话种类。纯 SFTP 会话没有终端：core 据此拒绝终端类操作。
    pub fn kind(&self) -> SessionKind {
        match self {
            Session::Local(_) => SessionKind::Local,
            Session::Ssh(s) => s.kind(),
        }
    }

    /// 本地会话 PTY 从设备的路径；其他会话为 `None`。
    pub fn tty_path(&self) -> Option<String> {
        match self {
            Session::Local(s) => s.tty_path(),
            Session::Ssh(_) => None,
        }
    }

    /// 本地会话 shell 进程的 pid；其他会话为 `None`。
    pub fn shell_pid(&self) -> Option<u32> {
        match self {
            Session::Local(s) => s.pid(),
            Session::Ssh(_) => None,
        }
    }

    /// 命令跟踪与终端传输的状态。纯 SFTP 会话没有终端，调用方须先排除。
    pub fn mcp_track_state(&self) -> crate::mcp::McpTrackState {
        match self {
            Session::Local(s) => s.mcp_track_state(),
            Session::Ssh(s) => s.mcp_track_state(),
        }
    }

    /// 终端是否已锁定（会话已结束，terminal.md §13）。
    pub fn is_locked(&self) -> bool {
        match self {
            Session::Local(s) => s.is_locked(),
            Session::Ssh(s) => s.is_locked(),
        }
    }

    /// 读取终端文本（MCP）。纯 SFTP 会话没有终端，调用方须先排除。
    pub fn mcp_read(&self, scope: kanso_term::ReadScope) -> crate::mcp::McpScreen {
        match self {
            Session::Local(s) => s.mcp_read(scope),
            Session::Ssh(s) => s.mcp_read(scope),
        }
    }

    /// 输入一行命令并等它返回提示符（MCP）。取消安全，见 `mcp` 模块文档。
    pub async fn mcp_run(&self, command: &str, timeout: Duration) -> crate::mcp::McpRun {
        match self {
            Session::Local(s) => s.mcp_run(command, timeout).await,
            Session::Ssh(s) => s.mcp_run(command, timeout).await,
        }
    }

    /// 输入文字与按键，可选等输出安静（MCP）。取消安全，见 `mcp` 模块文档。
    pub async fn mcp_send(
        &self,
        inputs: Vec<crate::mcp::McpWrite>,
        settle: Option<Duration>,
    ) -> crate::mcp::McpSend {
        match self {
            Session::Local(s) => s.mcp_send(inputs, settle).await,
            Session::Ssh(s) => s.mcp_send(inputs, settle).await,
        }
    }

    fn close_by_user(&self) -> Option<JoinHandle<()>> {
        match self {
            Session::Local(s) => s.close_by_user(),
            Session::Ssh(s) => s.close_by_user(),
        }
    }
}

/// 会话管理器。线程安全，所有方法取 `&self`。
pub struct SessionManager {
    cfg: ManagerConfig,
    host: Arc<dyn SessionHost>,
    next_id: AtomicU64,
    /// 有序表：`sessions()` 按打开顺序返回。已经因 shell 退出而结束的会话**保留**在表里
    /// （UI 还要显示最后的画面、读选区），直到用户 `close`。
    sessions: Mutex<BTreeMap<SessionId, Session>>,
    /// 用户关闭会话时的收尾线程，`shutdown` 要等它们。
    closers: Mutex<Vec<JoinHandle<()>>>,
    shutting_down: AtomicBool,
    /// SSH 会话的任务跑在这个运行时上；`None`（没有注入）时打开 SSH 会话会失败。
    runtime: Option<tokio::runtime::Handle>,
    /// 全局的时间戳显示模式。TODO(T10)：渲染侧读取方式待定，目前只存储。
    timestamp_mode: Mutex<TimestampMode>,
    /// 当前显示选项：新开的会话在激活前应用它，已有会话由 `set_terminal_options` 逐个更新。
    terminal_options: Mutex<TerminalOptions>,
}

impl SessionManager {
    /// 创建管理器。不会启动任何东西。
    pub fn new(cfg: ManagerConfig, host: Arc<dyn SessionHost>) -> SessionManager {
        SessionManager {
            cfg,
            host,
            next_id: AtomicU64::new(1),
            sessions: Mutex::new(BTreeMap::new()),
            closers: Mutex::new(Vec::new()),
            shutting_down: AtomicBool::new(false),
            runtime: None,
            timestamp_mode: Mutex::new(TimestampMode::default()),
            terminal_options: Mutex::new(TerminalOptions::default()),
        }
    }

    /// 注入 SSH 会话使用的 tokio 运行时（core 的进程内唯一运行时）。创建后立即调用，之后不再更换。
    pub fn with_runtime(mut self, runtime: tokio::runtime::Handle) -> SessionManager {
        self.runtime = Some(runtime);
        self
    }

    /// 打开会话，立即返回 id。依次发出 `Connecting`、`Connected`、`TerminalDirty`。
    ///
    /// 阻塞：本地会话只做 fork + 创建两个线程，毫秒级；SSH 会话只登记并启动后台任务，连接在后台进行。
    /// 本地启动失败返回 [`OpenError`]，此时没有发出任何事件；SSH 的连接失败不在这里返回，而是
    /// 通过 `Closed(Error(..))` 事件通知（契约：`Core::open_session`）。
    pub fn open(&self, request: OpenRequest, size: GridSize) -> Result<SessionId, OpenError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(OpenError::ShuttingDown);
        }
        let launch = match request {
            OpenRequest::Ssh(launch) => {
                return self.open_ssh(launch, Source::Connect, size, false);
            }
            OpenRequest::SshShared { launch, connection } => {
                return self.open_ssh(launch, Source::Shared(Some(connection)), size, false);
            }
            OpenRequest::Headless { launch, connection } => {
                let source = connection.map_or(Source::Connect, |c| Source::Shared(Some(c)));
                return self.open_ssh(launch.sftp_only(), source, size, true);
            }
            OpenRequest::DefaultLocalShell => LocalLaunch {
                command_line: self.cfg.default_shell.clone().unwrap_or_default(),
                ..LocalLaunch::default()
            },
            OpenRequest::Local(p) => LocalLaunch {
                command_line: if p.shell_command.trim().is_empty() {
                    self.cfg.default_shell.clone().unwrap_or_default()
                } else {
                    p.shell_command
                },
                start_dir: p.start_dir.filter(|d| !d.is_empty()).map(PathBuf::from),
                encoding: TextEncoding::resolve(&p.encoding),
                locale: p.locale.map(|l| l.as_str().to_owned()),
            },
            OpenRequest::Duplicate(src) => {
                match self.session(src).ok_or(OpenError::NoSuchSession(src))? {
                    Session::Local(source) => {
                        // 复制出来的本地 shell 从原标签当前所在的目录启动，而不是原配置的起始目录。
                        let mut launch = source.launch().clone();
                        if let Some(dir) = source.duplicate_start_dir() {
                            launch.start_dir = Some(dir);
                        }
                        launch
                    }
                    // 无界面会话对外不存在：UI 拿不到它的 id，这里防御性地按"不存在"处理，
                    // 否则会把它的非交互连接器与纯 SFTP 设置复制成一个可见标签。
                    Session::Ssh(source) if source.is_headless() => {
                        return Err(OpenError::NoSuchSession(src));
                    }
                    Session::Ssh(source) => {
                        // 只在这里短暂取一份连接引用，立刻交给新会话；源会话已结束则没有连接。
                        let connection = source.connection();
                        return self.open_ssh(
                            source.launch().clone(),
                            Source::Shared(connection),
                            size,
                            false,
                        );
                    }
                }
            }
        };
        let command_line = launch.command_line.clone();
        let id = SessionId::from_raw(self.next_id.fetch_add(1, Ordering::Relaxed));
        let session = LocalSession::start(id, Arc::clone(&self.host), launch, size, &self.cfg)
            .map_err(|e| {
                let (command, detail) = match e {
                    StartError::Spawn { command, error } => (command, error.to_string()),
                    StartError::Command(error) => (command_line, error.to_string()),
                    StartError::Thread(error) => (command_line, error.to_string()),
                };
                OpenError::Spawn { command, detail }
            })?;
        // 先登记再发事件：UI 收到 `Connected` 后立刻调用 core 的任何方法都要找得到会话。
        lock(&self.sessions).insert(id, Session::Local(Arc::clone(&session)));
        // 在 activate 之前应用：读线程尚未开始处理输出，新会话从第一帧起就是当前的显示设置。
        session.init_terminal_options(self.terminal_options());
        session.activate();
        Ok(id)
    }

    fn open_ssh(
        &self,
        launch: SshLaunch,
        source: Source,
        size: GridSize,
        headless: bool,
    ) -> Result<SessionId, OpenError> {
        let Some(runtime) = self.runtime.clone() else {
            return Err(OpenError::Spawn {
                command: "ssh".to_owned(),
                detail: "no tokio runtime injected for ssh sessions".to_owned(),
            });
        };
        let id = SessionId::from_raw(self.next_id.fetch_add(1, Ordering::Relaxed));
        let host: Arc<dyn SessionHost> = if headless {
            Arc::new(HeadlessHost(Arc::clone(&self.host)))
        } else {
            Arc::clone(&self.host)
        };
        let session = SshSession::new(id, host, launch, size, runtime, source, headless);
        // 先登记再启动任务：任务发出的第一个事件一到 UI，UI 就会调用 core，会话必须已经查得到。
        lock(&self.sessions).insert(id, Session::Ssh(Arc::clone(&session)));
        session.init_terminal_options(self.terminal_options());
        session.activate();
        Ok(id)
    }

    /// 用户关闭会话；会话不存在时忽略。不阻塞（杀进程在收尾线程里）。
    pub fn close(&self, id: SessionId) {
        let Some(session) = lock(&self.sessions).remove(&id) else {
            return;
        };
        if let Some(handle) = session.close_by_user() {
            let mut closers = lock(&self.closers);
            closers.retain(|h| !h.is_finished());
            closers.push(handle);
        }
    }

    /// 取**本地**会话（SSH 会话返回 `None`）。返回 `Arc`，调用方在管理器的锁之外使用它
    /// （管理器的表锁只保护查表）。需要不区分种类时用 [`SessionManager::session`]。
    pub fn get(&self, id: SessionId) -> Option<Arc<LocalSession>> {
        match lock(&self.sessions).get(&id) {
            Some(Session::Local(s)) => Some(Arc::clone(s)),
            _ => None,
        }
    }

    /// 取会话（本地或 SSH）。
    pub fn session(&self, id: SessionId) -> Option<Session> {
        lock(&self.sessions).get(&id).cloned()
    }

    /// 当前全部**有界面**会话，按打开顺序。无界面会话不在其中（UI 不该看到它们）；
    /// 需要包含无界面会话时用 [`SessionManager::all_sessions`]。
    pub fn sessions(&self) -> Vec<SessionSummary> {
        let all: Vec<_> = lock(&self.sessions).values().cloned().collect();
        all.iter()
            .filter(|s| !s.is_headless())
            .map(summarize)
            .collect()
    }

    /// 全部会话（含无界面会话），按打开顺序。core 内部做连接查找用。
    pub fn all_sessions(&self) -> Vec<Session> {
        lock(&self.sessions).values().cloned().collect()
    }

    /// 单个会话的摘要。
    pub fn summary(&self, id: SessionId) -> Option<SessionSummary> {
        self.session(id).map(|s| summarize(&s))
    }

    /// 设置全局时间戳显示模式（对没有单独设置的会话生效）。TODO(T10)：渲染侧读取方式待定。
    pub fn set_timestamp_mode(&self, mode: TimestampMode) {
        *lock(&self.timestamp_mode) = mode;
    }

    /// 全局时间戳显示模式。
    pub fn timestamp_mode(&self) -> TimestampMode {
        *lock(&self.timestamp_mode)
    }

    /// 设置显示选项并应用到所有现有会话；之后新开的会话也使用它（terminal.md §15）。
    ///
    /// 先更新记录再遍历：与并发的 `open` 竞争时，新会话要么读到新值，要么已在表里被遍历到。
    pub fn set_terminal_options(&self, options: TerminalOptions) {
        *lock(&self.terminal_options) = options;
        let all: Vec<_> = lock(&self.sessions).values().cloned().collect();
        for session in all {
            session.set_terminal_options(options);
        }
    }

    /// 当前显示选项。
    pub fn terminal_options(&self) -> TerminalOptions {
        *lock(&self.terminal_options)
    }

    /// 关闭所有会话并等待收尾线程（杀进程、读写线程退出），总共最多等 `timeout`，
    /// 超时就放弃等待——不能让应用退出卡住。之后 `open` 返回 `ShuttingDown`。
    pub fn shutdown(&self, timeout: Duration) {
        self.shutting_down.store(true, Ordering::Release);
        let sessions = std::mem::take(&mut *lock(&self.sessions));
        let mut handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.closers));
        // 先让所有会话同时开始收尾（每个会话一个线程），再统一等待，总耗时不随会话数相加。
        for session in sessions.into_values() {
            handles.extend(session.close_by_user());
        }
        let deadline = Instant::now() + timeout;
        for handle in handles {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !join_with_timeout(handle, remaining) {
                tracing::warn!("session closer did not finish before shutdown deadline");
            }
        }
    }
}

impl Drop for SessionManager {
    /// 没有调用 `shutdown` 就丢弃时也要结束 shell 进程，不留孤儿。
    fn drop(&mut self) {
        self.shutdown(Duration::from_secs(2));
    }
}

fn summarize(s: &Session) -> SessionSummary {
    SessionSummary {
        kind: s.kind(),
        id: s.id(),
        name: s.name().to_owned(),
        state: s.state(),
    }
}
