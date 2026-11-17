//! 本地会话：一个 PTY 里的 shell，加上它的编码转换与终端状态（local-shell.md、terminal.md）。
//!
//! 线程、锁、唤醒合并与两种会话结束方式的总体设计见 crate 文档（`lib.rs`），这里只补充实现细节。

use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kanso_encoding::{StreamDecoder, TextEncoding, encode_text};
use kanso_frame::Frame;
use kanso_pty::{
    EnvOptions, EnvSnapshot, Platform as PtyPlatform, PtyError, PtySession, SpawnConfig,
    SystemLocaleInfo, ZshIntegration, build_env, decide_locale, install_zsh_shim, resolve_command,
    system_locale_exists,
};
use kanso_term::{
    InputKind, KeyOptions, MouseOutcome, OptionAsMeta, Platform as KeyPlatform, Registered,
    ScrollAction, SearchQuery, SearchStatus, ShellFlavor, TermConfig, TermEvent, Terminal,
    encode_key, encode_paste, encode_scroll, quote_paths,
};
use kanso_types::agent::AgentKind;
use kanso_types::event::{Activity, CloseReason, SessionState};
use kanso_types::{
    CoreEvent, GridSize, KeyEvent, Modifiers, MouseEvent, PasteContent, SessionId, TerminalOptions,
    TimestampMode, ViewportPoint,
};

use crate::activity::{ActivityHost, ActivityHub};
use crate::git::git_branch;
use crate::host::SessionHost;
use crate::manager::ManagerConfig;

/// 每次读 PTY 的缓冲区大小。不小于 64 KiB：洪水输出时单次读得越多，加锁与唤醒的次数越少。
const READ_BUF_SIZE: usize = 64 * 1024;

/// 默认回滚行数。TODO(F7)：改为读设置项（terminal.md §15）。
pub(crate) const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

/// EOF 之后等待 shell 退出码的上限。EOF 说明所有持有从端的进程都已关闭它，
/// 正常情况下 shell 此刻已经退出或马上退出；超过上限仍未退出（shell 关掉了自己的 tty 却继续运行）
/// 就按"退出码未知"结束会话，进程留给 `close_session` 去 kill。
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// 关闭会话时等待读写线程退出的上限。超时就放弃等待（线程自己会在读到 EOF / 写失败后退出）。
const THREAD_JOIN_TIMEOUT: Duration = Duration::from_millis(1500);

/// 本地会话的启动参数。`Duplicate` 复用源会话的这份参数（ui-shell.md §3.3）。
#[derive(Clone, Debug)]
pub struct LocalLaunch {
    /// 启动命令行（local-shell.md §1）；空串表示默认 shell（配置的 `default_shell`，再没有取 `$SHELL`）。
    pub command_line: String,
    /// 起始目录；`None` 为用户主目录。
    pub start_dir: Option<PathBuf>,
    /// 会话编码。输入与输出两个方向共用这一个值（`kanso-encoding` 模块文档要求）。
    pub encoding: TextEncoding,
    /// 显式 locale（Profile 里填写的）；`None` 走自动推算（local-shell.md §4）。
    /// 非法值由 `decide_locale` 当作没填处理，不会写进环境变量。
    pub locale: Option<String>,
}

impl Default for LocalLaunch {
    /// 默认 shell、主目录、UTF-8。
    fn default() -> Self {
        LocalLaunch {
            command_line: String::new(),
            start_dir: None,
            encoding: TextEncoding::utf8(),
            locale: None,
        }
    }
}

/// 读线程统计，只用于测试与诊断（例如验证唤醒合并：事件数应远少于读到的块数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// 读到数据的次数（`read` 返回正数的次数）。
    pub chunks: u64,
    /// 读到的总字节数。
    pub bytes: u64,
}

/// 从 `Mutex` 取锁。持锁的代码（`Terminal` 的方法、发事件的回调）一旦 panic 就会使锁中毒，
/// 但这些临界区不会留下需要靠中毒来发现的"半更新"状态，继续使用比让整个会话报废更好。
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 当前 Unix 毫秒。
pub(crate) fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// 把按键 / 粘贴编码出来的字节转成会话编码。
///
/// `encode_key` / `encode_paste` 产生的是 UTF-8（控制序列是 ASCII，文本部分是 UTF-8），
/// 非 UTF-8 会话要把文本部分转成会话编码；UTF-8 会话原样返回，不拷贝。
/// 万一出现不是合法 UTF-8 的字节（目前不会），当作二进制原样发送，不能丢。
pub(crate) fn to_session_encoding(enc: TextEncoding, bytes: Vec<u8>) -> Vec<u8> {
    if enc.is_utf8() {
        return bytes;
    }
    match String::from_utf8(bytes) {
        Ok(s) => encode_text(enc, &s).into_owned(),
        Err(e) => e.into_bytes(),
    }
}

/// 读线程与调用线程共享的会话状态。
struct Shared {
    transfer: Arc<crate::transfer::TerminalTransfer>,
    id: SessionId,
    host: Arc<dyn SessionHost>,
    encoding: TextEncoding,
    /// 终端状态。临界区只放 `Terminal` 的方法，见 crate 文档"锁"一节。
    term: Mutex<Terminal>,
    /// PTY 句柄（写入端已交给写线程）。
    pty: Mutex<PtySession>,
    /// 唤醒合并标志：`false → true` 时发一次 `TerminalDirty`，`take_frame` 清除。
    dirty: AtomicBool,
    /// 标签是否可见，决定命令结束时是否发系统通知。
    visible: AtomicBool,
    /// 用户已经请求关闭。在 kill 之前置位，读线程据此放弃自己的"shell 退出"流程。
    user_closing: AtomicBool,
    /// 会话已经 `Closed`（任何原因）。为输入路径提供无锁的快速判断，真正的状态在 `lifecycle`。
    ended: AtomicBool,
    /// 会话状态；**发事件时持有它**，保证事件保序且 `Closed` 之后不再有事件。
    lifecycle: Mutex<SessionState>,
    /// 写通道发送端；会话结束后取走（`None`），写线程随之退出。
    write_tx: Mutex<Option<Sender<Vec<u8>>>>,
    /// 滚轮的小数行累积（`scroll` 的 `delta_lines` 带小数，core 负责累积，api.rs）。
    scroll_remainder: Mutex<f32>,
    /// 本会话的时间戳列显示模式。TODO(T10)：渲染侧如何读取待定，目前只存储。
    timestamp_mode: Mutex<Option<TimestampMode>>,
    /// 命令跟踪与 AI 轮次（terminal.md §5、§6），见 `activity` 模块。
    activity: Arc<ActivityHub>,
    read_chunks: AtomicU64,
    read_bytes: AtomicU64,
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

    /// 置脏；只有从干净变脏时才发 `TerminalDirty`（唤醒合并）。
    ///
    /// `AcqRel`：`take_frame` 的 `swap(false)` 与这里的 `swap(true)` 构成同一个原子变量上的全序，
    /// 内存序只需保证它们之间不被重排。
    fn mark_dirty(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            self.emit(CoreEvent::TerminalDirty { session: self.id });
        }
    }

    /// 在终端锁内执行 `f`，然后（**释放锁之后**）转发终端因此产生的回应并按需置脏。
    ///
    /// 回应原样写回 PTY，不经过编码器，也不算用户输入（terminal.md §5.2）。
    /// 事件（`take_events`）不在这里取：保持由读线程独占，事件顺序才与输出顺序一致。
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

    /// 把字节交给写线程。通道已关闭（会话结束）时丢弃。永不阻塞。
    fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(tx) = lock(&self.write_tx).as_ref() {
            // 写线程已退出时 `send` 返回 Err，说明会话已经结束，丢弃即可。
            let _ = tx.send(bytes);
        }
    }

    /// 关闭写通道，写线程把已排队的数据写完后退出。
    fn close_writer(&self) {
        lock(&self.write_tx).take();
    }

    /// 会话已结束或正在被用户关闭：用户输入一律丢弃（terminal.md §1、§13）。
    fn input_blocked(&self) -> bool {
        self.ended.load(Ordering::Acquire)
            || self.user_closing.load(Ordering::Acquire)
            || self.transfer.active()
    }

    /// 把会话状态改成 `Closed(reason)` 并发事件。**只有第一个调用者返回 `true`**，其余什么也不做：
    /// 这是"shell 自己退出"和"用户关闭"两条并发路径不重复发、也不遗漏结束事件的唯一裁决点。
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
            true
        };
        // 必须在释放生命周期锁之后：命令跟踪发事件时先持 tracker 锁再取生命周期锁，反过来会死锁。
        // 会话结束重置全部跟踪状态，并唤醒正在等待命令返回的调用方（terminal.md §5.2）。
        self.activity.close();
        first
    }

    /// 结束 shell 进程并回收（先结束进程、再回收资源，local-shell.md §7）。失败只记日志。
    fn kill_process(&self) {
        if let Err(e) = lock(&self.pty).kill() {
            tracing::warn!(session = %self.id, error = %e, "failed to kill shell process");
        }
    }
}

impl ActivityHost for Shared {
    fn command_submitted(&self, text: &str) {
        self.host.record_command(text, "local");
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

/// 一个本地会话。由 [`crate::SessionManager`] 持有；UI 只拿 `SessionId`。
pub struct LocalSession {
    shared: Arc<Shared>,
    name: String,
    launch: LocalLaunch,
    /// 读线程的启动闸门：`activate` 发出 `Connecting` / `Connected` 之后才放行读线程，
    /// 保证这两个事件一定排在任何输出相关事件之前。发送端被丢弃（启动失败）时读线程直接退出。
    gate: Mutex<Option<Sender<()>>>,
    threads: Mutex<Threads>,
}

#[derive(Default)]
struct Threads {
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

/// 启动失败。
#[derive(Debug)]
pub(crate) enum StartError {
    /// 打开 PTY 或启动 shell 失败；`command` 为最终启动的程序（用于提示用户）。
    Spawn { command: String, error: PtyError },
    /// 命令行无法解析。
    Command(PtyError),
    /// 创建线程失败。
    Thread(std::io::Error),
}

impl LocalSession {
    /// 启动 shell 并创建会话，但还不开始读输出、不发任何事件；随后必须调用 [`LocalSession::activate`]。
    ///
    /// 启动流程（local-shell.md）：`resolve_command` → `install_zsh_shim`（仅在开启 shell 集成时，
    /// 失败则不设 `ZDOTDIR`、照常启动）→ `decide_locale` → `build_env` → `PtySession::spawn`。
    pub(crate) fn start(
        id: SessionId,
        host: Arc<dyn SessionHost>,
        launch: LocalLaunch,
        size: GridSize,
        cfg: &ManagerConfig,
    ) -> Result<Arc<LocalSession>, StartError> {
        let platform = PtyPlatform::current();
        let base = cfg
            .base_env
            .clone()
            .unwrap_or_else(EnvSnapshot::from_current);

        let command =
            resolve_command(&launch.command_line, &base, platform).map_err(StartError::Command)?;

        // 只有开启了集成才生成 shim；失败不是致命错误：不设 ZDOTDIR，shell 按原样启动，只是没有提示符集成。
        let zsh = if cfg.shell_integration {
            match install_zsh_shim(&cfg.data_dir) {
                Ok(dir) => Some(ZshIntegration {
                    shim_dir: dir,
                    prompt_enabled: true,
                }),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to install zsh shim, starting shell without integration");
                    None
                }
            }
        } else {
            None
        };

        let locale = decide_locale(
            launch.locale.as_deref(),
            launch.encoding.is_utf8(),
            &base,
            platform,
            &SystemLocaleInfo {
                preferred_language: cfg.preferred_language.clone(),
                region: cfg.region.clone(),
            },
            system_locale_exists,
        );
        let changes = build_env(
            &base,
            &EnvOptions {
                term_program_version: cfg.app_version.clone(),
                locale,
                zsh,
            },
        );
        let env = base.apply(&changes);

        let home = env.get_non_empty("HOME").map(PathBuf::from);
        // 会话配置里的起始目录常写成 `~/code`：shell 不会替我们展开（cwd 在 exec 之前就要设好），
        // 所以这里按子进程的 HOME 展开 `~` 与 `~/…`；`~user` 写法不支持，原样交给系统（会启动失败并报错）。
        let cwd = launch
            .start_dir
            .clone()
            .map(|dir| expand_home(dir, home.as_deref()))
            .or_else(|| home.clone())
            .unwrap_or_else(|| PathBuf::from("/"));
        // PTY 与终端用同一份夹紧后的尺寸：终端至少 2 列 1 行，PTY 也不能是 0。
        let size = GridSize {
            cols: size.cols.max(2),
            rows: size.rows.max(1),
        };
        let program = command.program.clone();
        let name = Path::new(&program)
            .file_name()
            .map_or_else(|| program.clone(), |n| n.to_string_lossy().into_owned());

        let mut pty = PtySession::spawn(SpawnConfig {
            command,
            env,
            cwd,
            size,
        })
        .map_err(|error| StartError::Spawn {
            command: program.clone(),
            error,
        })?;
        let spawn_err = |error| StartError::Spawn {
            command: program.clone(),
            error,
        };
        let reader = pty.reader().map_err(spawn_err)?;
        let writer = pty.take_writer();

        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
        let shared = Arc::new(Shared {
            transfer: crate::transfer::TerminalTransfer::new(id, Arc::clone(&host)),
            id,
            host,
            encoding: launch.encoding,
            term: Mutex::new(Terminal::new(TermConfig {
                size,
                scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            })),
            pty: Mutex::new(pty),
            dirty: AtomicBool::new(false),
            visible: AtomicBool::new(true),
            user_closing: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            lifecycle: Mutex::new(SessionState::Connecting),
            write_tx: Mutex::new(Some(write_tx)),
            scroll_remainder: Mutex::new(0.0),
            timestamp_mode: Mutex::new(None),
            activity: ActivityHub::new(),
            read_chunks: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
        });

        let weak = Arc::downgrade(&shared);
        shared.transfer.set_writer(Arc::new(move |bytes| {
            let shared = weak.upgrade().ok_or("session ended")?;
            let writer = lock(&shared.write_tx);
            writer
                .as_ref()
                .ok_or_else(|| "session ended".to_string())?
                .send(bytes)
                .map_err(|error| error.to_string())
        }));
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let spawned = (|| -> std::io::Result<Threads> {
            let reader_shared = Arc::clone(&shared);
            let reader_thread = thread::Builder::new()
                .name(format!("kanso-pty-read-{id}"))
                .spawn(move || reader_main(reader_shared, reader, gate_rx))?;
            let writer_thread = match writer {
                Some(w) => Some(
                    thread::Builder::new()
                        .name(format!("kanso-pty-write-{id}"))
                        .spawn(move || write_loop(w, write_rx))?,
                ),
                None => None,
            };
            Ok(Threads {
                reader: Some(reader_thread),
                writer: writer_thread,
            })
        })();
        let threads = match spawned {
            Ok(t) => t,
            Err(e) => {
                // 闸门随 `gate_tx` 一起丢弃，已经启动的读线程会直接退出；进程要自己结束。
                shared.close_writer();
                shared.kill_process();
                return Err(StartError::Thread(e));
            }
        };

        #[cfg(windows)]
        spawn_exit_watcher(&shared);

        // 定时线程放在读写线程之后：前面任何一步失败都会提前返回，不会留下没人关闭的定时线程。
        // 会话结束时 `Shared::finish` 会关闭 hub 让它退出。
        shared
            .activity
            .spawn_timer(Arc::downgrade(&shared), format!("kanso-activity-{id}"));

        Ok(Arc::new(LocalSession {
            shared,
            name,
            launch,
            gate: Mutex::new(Some(gate_tx)),
            threads: Mutex::new(threads),
        }))
    }

    /// 发出 `Connecting`、`Connected` 与首个 `TerminalDirty`，然后放行读线程。
    ///
    /// 这个顺序是对外契约：UI 在收到 `Connected` 之前不会看到该会话的任何别的事件。
    pub(crate) fn activate(&self) {
        {
            // 不经过 `emit`：这两个事件必须在 `Connecting` 状态下发出，且本函数是唯一的发送者。
            let mut state = lock(&self.shared.lifecycle);
            self.shared.host.emit(CoreEvent::SessionStateChanged {
                session: self.shared.id,
                state: SessionState::Connecting,
            });
            *state = SessionState::Connected;
            self.shared.host.emit(CoreEvent::SessionStateChanged {
                session: self.shared.id,
                state: SessionState::Connected,
            });
        }
        // 终端刚创建就是"脏"的（空屏幕也是一帧）：告诉 UI 可以取第一帧了。
        self.shared.mark_dirty();
        if let Some(gate) = lock(&self.gate).take() {
            let _ = gate.send(());
        }
    }

    /// 会话 id。
    pub fn id(&self) -> SessionId {
        self.shared.id
    }

    /// 显示名（shell 程序名）。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 启动参数，`Duplicate` 用。
    pub fn launch(&self) -> &LocalLaunch {
        &self.launch
    }

    /// 当前状态。
    pub fn state(&self) -> SessionState {
        lock(&self.shared.lifecycle).clone()
    }

    /// shell 进程的 pid（诊断与测试用）。
    pub fn pid(&self) -> Option<u32> {
        lock(&self.shared.pty).pid()
    }

    /// 读线程统计，见 [`ReadStats`]。
    pub fn read_stats(&self) -> ReadStats {
        ReadStats {
            chunks: self.shared.read_chunks.load(Ordering::Relaxed),
            bytes: self.shared.read_bytes.load(Ordering::Relaxed),
        }
    }

    // ---- 输入 ----
    //
    // 共同约定：在调用线程完成编码，把结果丢进写通道，永不阻塞；会话已结束或正在关闭时丢弃。

    /// 一次按键。
    ///
    /// 编码选项目前固定为 mac + Option 不作 Meta。TODO(F7)：`option_as_meta` 改为读设置项。
    pub fn key(&self, event: &KeyEvent) {
        if self.shared.input_blocked() {
            return;
        }
        let opts = KeyOptions {
            platform: KeyPlatform::Mac,
            option_as_meta: OptionAsMeta::Off,
        };
        // `encode_key` 是纯计算，放在锁内可以让"读 modes"与"滚回底部"是同一个临界区；
        // modes 必须取自发送这一刻（DECCKM 与 kitty 标志随程序随时变化）。
        let bytes = self.shared.with_term(|t| {
            let bytes = encode_key(event, &t.modes(), &opts);
            if bytes.is_some() {
                // 发送任何用户输入时把视口滚回底部。
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
        for kind in InputKind::of_text(text) {
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
            // AI CLI 会话原样粘贴路径，让工具把它识别成附件；普通 shell 按 POSIX 转义（terminal.md §8.3）。
            PasteContent::Files(paths) => {
                let text = if self.shared.activity.agent().is_some() {
                    paths.join(" ")
                } else {
                    quote_paths(paths, ShellFlavor::Posix)
                };
                self.paste_text(&text);
            }
            // 只有图片：AI CLI 会话里不粘贴文字，而是发送工具自己的图片粘贴键（macOS 为 Ctrl+V），
            // 由工具自己读剪贴板；普通 shell 没有对应行为，什么也不发。
            PasteContent::ImageOnly => self.paste_image_key(),
        }
    }

    fn paste_image_key(&self) {
        if self.shared.activity.agent().is_none() {
            return;
        }
        self.shared.with_term(Terminal::scroll_to_bottom);
        // 对工具来说这是一次用户按键，会开启一个轮次。
        self.shared
            .activity
            .on_input(&*self.shared, InputKind::Typing);
        // 控制字符，不经过会话编码转换。
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
    /// 调用方保证：先登记、后写会话；写失败时必须调用 [`LocalSession::cancel_send`]。
    /// 返回 [`Registered::Busy`] 说明上一条命令还没返回提示符。
    pub fn register_send(&self, text: &str) -> Registered {
        self.shared.activity.register_send(&*self.shared, text)
    }

    /// 撤销 [`LocalSession::register_send`] 的登记。
    pub fn cancel_send(&self) {
        self.shared.activity.cancel_send(&*self.shared);
    }

    /// 检查 Sender 输入是否可用，并返回当前命令是否结束。
    pub fn sender_ready(&self) -> Result<bool, kanso_types::Message> {
        if self.shared.input_blocked() {
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
                .is_some_and(|writer| writer.send(bytes).is_ok());
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

    /// 鼠标事件：程序开启了鼠标上报时转成上报序列写回，否则由终端处理选区。
    pub fn mouse(&self, event: &MouseEvent) {
        let outcome = self.shared.with_term(|t| t.handle_mouse(event));
        match outcome {
            // 上报是协议字节，原样写；会话已结束时丢弃。
            MouseOutcome::Report(bytes) => {
                if !self.shared.input_blocked() {
                    self.shared.send(bytes);
                }
            }
            // 选中即复制在 UI 层（terminal.md §8.2），core 这边无事可做；选区变化已经由 with_term 置脏。
            MouseOutcome::SelectionFinished { .. } | MouseOutcome::None => {}
        }
    }

    /// 滚轮 / 触摸板滚动。`delta_lines` 正数向上（看更早的内容），小数部分累积到下一次。
    pub fn scroll(&self, delta_lines: f32, at: ViewportPoint, mods: Modifiers) {
        if !delta_lines.is_finite() {
            return;
        }
        let lines = {
            let mut rem = lock(&self.shared.scroll_remainder);
            *rem += delta_lines;
            let whole = rem.trunc();
            *rem -= whole;
            // f32 转 i32 在 Rust 里本来就是饱和转换；先 clamp 只是让意图明确。
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
            // 上报与方向键是发给程序的输入，会话结束后丢弃；滚动视口不是（结束后仍可回看输出）。
            ScrollAction::Report(bytes) | ScrollAction::ArrowKeys(bytes) => {
                if !self.shared.input_blocked() {
                    self.shared.send(bytes);
                }
            }
            ScrollAction::ScrollViewport(_) => {}
        }
    }

    /// 键盘焦点变化；程序开启了焦点上报时产生的回应由 `with_term` 写回。
    pub fn focus(&self, focused: bool) {
        self.shared.with_term(|t| t.set_focused(focused));
    }

    // ---- 尺寸、可见性、显示 ----

    /// 调整终端与 PTY 的尺寸。像素尺寸是整个终端区域的像素（单元格像素 × 行列数）。
    pub fn resize(&self, size: GridSize, cell_width_px: u16, cell_height_px: u16) {
        let size = GridSize {
            cols: size.cols.max(2),
            rows: size.rows.max(1),
        };
        self.shared.with_term(|t| {
            t.set_cell_pixel_size(cell_width_px, cell_height_px);
            t.resize(size);
        });
        // 正在被用户关闭时 kill 持有 PTY 锁（最多约 250 ms），不要让调用线程等它。
        if self.shared.user_closing.load(Ordering::Acquire) {
            return;
        }
        let result = lock(&self.shared.pty).resize(
            size,
            cell_width_px.saturating_mul(size.cols),
            cell_height_px.saturating_mul(size.rows),
        );
        if let Err(e) = result {
            // 会话已结束时 PTY 调整一定会失败，属于正常情况。
            tracing::debug!(session = %self.shared.id, error = %e, "pty resize failed");
        }
    }

    /// 标签是否可见。不可见时命令结束会发系统通知。
    pub fn set_visible(&self, visible: bool) {
        self.shared.visible.store(visible, Ordering::Release);
    }

    /// 设置本会话的时间戳显示模式。TODO(T10)：目前只存储，渲染侧读取方式待定。
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

    /// 当前工作目录：优先用终端记录的 OSC 7 / 1337 / 9;9 上报，没有就查前台进程的 cwd
    /// （local-shell.md §6）；都拿不到为 `None`。
    pub fn working_directory(&self) -> Option<String> {
        let reported = self.shared.term().cwd().map(|c| c.path.clone());
        reported.or_else(|| {
            lock(&self.shared.pty)
                .foreground_cwd()
                .map(|p| p.to_string_lossy().into_owned())
        })
    }

    /// 当前命令行的光标前输入；传输或终端锁定时不可用。
    pub fn completion_input(&self) -> Option<(String, u16)> {
        if self.shared.input_blocked() {
            return None;
        }
        self.shared.activity.completion_input(&*self.shared)
    }

    /// 最新上报连同主机归属，供补全与目录定位判断。
    pub fn reported_directory(&self) -> Option<kanso_term::CwdReport> {
        self.shared.term().cwd().cloned()
    }

    /// 复制标签时新会话的起始目录：原会话此刻所在的目录（ui-shell.md §3.3）。
    ///
    /// 优先取前台进程的 cwd：它一定是本机路径。OSC 7 上报的目录可能来自用户在这个本地 shell 里
    /// `ssh` 登录的远端主机，拿它当本机起始目录会进错地方；所以只在查不到前台进程时才退回它，
    /// 并且要求它在本机确实是一个目录。都拿不到返回 `None`，由调用方沿用原会话的启动配置。
    pub fn duplicate_start_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = lock(&self.shared.pty).foreground_cwd() {
            return Some(dir);
        }
        let reported = self.shared.term().cwd().map(|c| PathBuf::from(&c.path))?;
        reported.is_dir().then_some(reported)
    }

    // ---- 帧 ----

    /// 取最新帧；契约见 `kanso-core` 的 `Core::take_frame`。
    ///
    /// 只有 `dirty` 为真才产帧并清除它；**没有新帧时 `reuse` 原样保留**（调用方可能还要重绘它）。
    /// 先清标志、再加锁产帧：清标志之后到加锁之前到达的新输出会再次置脏并再发一次事件，
    /// 最坏多取一帧；反过来（先产帧再清标志）会吞掉这期间的唤醒，画面停在旧状态。
    pub fn take_frame(&self, reuse: &mut Option<Frame>) -> Option<Frame> {
        if !self.shared.dirty.swap(false, Ordering::AcqRel) {
            return None;
        }
        let old = reuse.take();
        Some(self.shared.term().snapshot(old))
    }

    // ---- 关闭 ----

    /// 用户关闭会话：先标记"用户关闭"，再 kill，然后发 `Closed(ByUser)`。不阻塞调用线程，
    /// 返回收尾线程的句柄（`shutdown` 用它等待）。
    ///
    /// 会话已经因为 shell 退出而结束时不会再发事件（`finish` 只放行第一个），这里只做回收。
    pub(crate) fn close_by_user(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        // 必须在 kill 之前：kill 引起的 EOF 会让读线程以为 shell 自己退出了。
        self.shared.user_closing.store(true, Ordering::Release);
        self.shared
            .transfer
            .close(kanso_types::Message::XferSessionEnded {});
        // 放走还没激活的读线程（不会发生，但这样关闭路径不依赖调用顺序）。
        lock(&self.gate).take();
        let me = Arc::clone(self);
        let work = move || me.finish_close();
        match thread::Builder::new()
            .name(format!("kanso-pty-close-{}", self.shared.id))
            .spawn(work)
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::error!(session = %self.shared.id, error = %e, "failed to spawn closer thread, closing inline");
                self.finish_close();
                None
            }
        }
    }

    /// 收尾：杀进程 → 发 `Closed(ByUser)` → 关写通道 → 等读写线程退出（有上限）。
    ///
    /// 顺序有讲究：事件必须在进程确实结束之后发（UI 收到它就可以认为 shell 没了）；
    /// 写通道要在 kill 之后才关，因为写入端被丢弃时 portable-pty 会向 shell 补发换行与 EOF，
    /// 对还活着的 shell 是一次多余的输入。
    fn finish_close(&self) {
        self.shared.kill_process();
        self.shared.finish(CloseReason::ByUser);
        self.shared.close_writer();
        let (reader, writer) = {
            let mut t = lock(&self.threads);
            (t.reader.take(), t.writer.take())
        };
        for handle in [reader, writer].into_iter().flatten() {
            join_with_timeout(handle, THREAD_JOIN_TIMEOUT);
        }
    }
}

/// 等线程退出，最多 `timeout`。超时就放弃（线程句柄被丢弃，线程自行收尾）。
pub(crate) fn join_with_timeout(handle: JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = handle.join();
    true
}

/// 写线程：把通道里的字节按顺序写进 PTY。
///
/// 写阻塞（PTY 缓冲区满）只会阻塞本线程。写失败说明 shell 已经没了：丢弃剩余数据并退出，
/// 之后 `Shared::send` 发现通道断开也会直接丢弃。
fn write_loop(mut writer: Box<dyn Write + Send>, rx: Receiver<Vec<u8>>) {
    while let Ok(bytes) = rx.recv() {
        if writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .is_err()
        {
            break;
        }
    }
}

/// 读线程入口：等闸门 → 读循环 → 会话结束流程。
fn reader_main(shared: Arc<Shared>, mut reader: Box<dyn Read + Send>, gate: Receiver<()>) {
    if gate.recv().is_err() {
        return;
    }
    // 读循环里的 panic（比如终端内核的缺陷）不能让会话悄悄卡死：捕获后按"异常结束"处理。
    let outcome = catch_unwind(AssertUnwindSafe(|| read_loop(&shared, &mut *reader)));
    match outcome {
        Ok(()) => on_eof(&shared),
        Err(_) => {
            tracing::error!(session = %shared.id, "terminal reader thread panicked");
            if !shared.user_closing.load(Ordering::Acquire) {
                shared.kill_process();
                shared.finish(CloseReason::Exited { code: None });
                shared.close_writer();
            }
        }
    }
}

/// 终端输出出口串行维护的状态，读线程与文件面板恢复输出共用。
struct ReaderState {
    /// 上一次发出的位置信息 `(cwd, host, git_branch)`，用来去重：shell 每个提示符都会上报 OSC 7。
    last_location: Option<(Option<String>, Option<String>, Option<String>)>,
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

        let (replies, events) = {
            let mut t = shared.term();
            t.feed(text.as_bytes(), now);
            (t.take_replies(), t.take_events())
        };
        // 以下都在释放终端锁之后：回应写回 PTY（原样，不经过编码器，不算用户输入）、
        // 置脏唤醒、处理事件（会读文件、发回调）。
        shared.send(replies);
        shared.mark_dirty();
        for event in &events {
            handle_term_event(&shared, state, event);
        }
        // 命令跟踪与 AI 轮次：每批输出都要通知（轮次靠输出续命，推断靠屏幕判断提示符返回），
        // 不论这一批有没有事件。
        shared.activity.on_output(&*shared, &events);
    })
}

fn read_loop(shared: &Arc<Shared>, reader: &mut dyn Read) {
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let output = terminal_output(shared);
    shared.transfer.set_terminal_output(Arc::clone(&output));
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                // portable-pty 已把 Unix 上从端关闭的 EIO 转成 EOF，走到这里是别的错误：同样结束读取。
                tracing::debug!(session = %shared.id, error = %e, "pty read failed");
                return;
            }
        };
        // 用户已经请求关闭：**继续读直到 EOF，但丢弃数据**，不再喂终端、不再发事件。
        // 不能直接退出读循环：macOS 上 shell 退出时关闭 tty 要等已写出的输出被主端读走，
        // 没人读就会卡在内核里（SIGKILL 也唤不醒），`kill` 里的 `wait` 就永远不返回。
        if shared.user_closing.load(Ordering::Acquire) {
            continue;
        }
        shared.read_chunks.fetch_add(1, Ordering::Relaxed);
        shared.read_bytes.fetch_add(n as u64, Ordering::Relaxed);

        // 每次读到的数据都立刻交给终端，不攒批（local-shell.md §7：避免最后一点输出卡在缓冲里）；
        // 一次读取就是终端的一个"批次"（kanso-term 的 clear 语义按批次判断）。
        shared.transfer.feed(&buf[..n]);
    }
}

/// `TermEvent` → `CoreEvent` 的映射。活动（运行 / 空闲 / AI 轮次）不在这里：
/// 它由 `ActivityHub::on_output` 统一处理（OSC 133 与提示符推断共用一套状态机）。
fn handle_term_event(shared: &Shared, state: &mut ReaderState, event: &TermEvent) {
    match event {
        TermEvent::CwdChanged(report) => {
            // 读几个很小的文件，不启动 git 进程；放在读线程里、终端锁之外。
            let branch = git_branch(Path::new(&report.path));
            let location = (Some(report.path.clone()), report.host.clone(), branch);
            if state.last_location.as_ref() != Some(&location) {
                state.last_location = Some(location.clone());
                shared.emit(CoreEvent::LocationChanged {
                    session: shared.id,
                    cwd: location.0,
                    cwd_host: location.1,
                    git_branch: location.2,
                });
            }
        }
        // OSC 133 的几个事件交给命令跟踪。
        TermEvent::CommandStart { .. }
        | TermEvent::PromptStart { .. }
        | TermEvent::InputStart { .. }
        | TermEvent::CommandFinished { .. } => {}
        // 标题与响铃本 WP 只在内核里记录，UI 事件等 T10 再定。
        TermEvent::TitleChanged | TermEvent::Bell => {}
        // OSC 52（程序请求写系统剪贴板）：先忽略。任何能往终端输出文字的程序（包括 `cat` 一个恶意文件）
        // 都能触发它，默认放行就等于让远端随意改写用户的剪贴板；需要先定授权策略（询问 / 白名单）。
        TermEvent::ClipboardStore(_) => {}
    }
}

/// Windows 专用：等 shell 进程退出，然后关闭伪控制台。
///
/// ConPTY 的已知行为：子进程退出后只要 `ClosePseudoConsole` 没被调用，输出管道就不会 EOF，
/// 读线程会一直阻塞、会话永远不结束。关闭伪控制台会先把残留输出送到读端再关管道，
/// 所以 shell 退出前的最后输出不会丢。用轮询 `try_wait` 而不是阻塞 `wait`，理由同 `on_eof`
/// （不长时间占着 PTY 锁）；主端在锁外丢弃，因为关闭可能阻塞到输出被读走。
/// 用户主动关闭时 `user_closing` 已置位，这里直接退出，由 `kill` 流程负责收尾。
#[cfg(windows)]
fn spawn_exit_watcher(shared: &Arc<Shared>) {
    let shared = Arc::clone(shared);
    let name = format!("kanso-pty-exit-{}", shared.id);
    let work = move || {
        loop {
            if shared.user_closing.load(Ordering::Acquire) || shared.ended.load(Ordering::Acquire) {
                return;
            }
            let waited = lock(&shared.pty).try_wait();
            let exited = matches!(waited, Ok(Some(_)));
            if exited {
                tracing::debug!(session = %shared.id, status = ?waited, "exit watcher: shell exited, closing pseudo console");
                let master = lock(&shared.pty).detach_master();
                drop(master);
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    };
    if let Err(e) = thread::Builder::new().name(name).spawn(work) {
        tracing::error!(error = %e, "failed to spawn exit watcher thread");
    }
}

/// 读到 EOF 之后的"shell 自己退出"流程：取退出码 → 发 `Closed(Exited)`。
///
/// 用 `try_wait` 轮询而不是阻塞的 `wait`：`wait` 会一直占着 PTY 锁，而 shell 可能关闭了 tty
/// 却继续运行（此时 `wait` 等很久），用户的 `close_session` 就会被卡在 `kill` 的拿锁上。
fn on_eof(shared: &Shared) {
    if shared.user_closing.load(Ordering::Acquire) {
        // 用户关闭路径负责发事件。
        return;
    }
    let deadline = Instant::now() + EXIT_WAIT;
    let mut delay = Duration::from_millis(5);
    let code = loop {
        let waited = lock(&shared.pty).try_wait();
        match waited {
            Ok(Some(status)) => break status.code().and_then(|c| i32::try_from(c).ok()),
            Ok(None) if Instant::now() < deadline => {}
            Ok(None) => break None,
            Err(e) => {
                tracing::warn!(session = %shared.id, error = %e, "try_wait failed after pty eof");
                break None;
            }
        }
        if shared.user_closing.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(100));
    };
    // 与 `close_by_user` 并发时由 `finish` 裁决，只有一个会发事件。
    shared.finish(CloseReason::Exited { code });
    shared.close_writer();
}

/// 把 `~` 或 `~/…` 开头的路径按 `home` 展开；没有 home 或不是这两种写法时原样返回。
fn expand_home(dir: PathBuf, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else { return dir };
    match dir.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => dir,
    }
}

#[cfg(test)]
mod expand_home_tests {
    use super::*;

    #[test]
    fn tilde_forms() {
        let home = Path::new("/Users/u");
        assert_eq!(
            expand_home("~".into(), Some(home)),
            PathBuf::from("/Users/u")
        );
        assert_eq!(
            expand_home("~/code".into(), Some(home)),
            PathBuf::from("/Users/u/code")
        );
        // `~user` 不是 `~` 组件，Path::strip_prefix 按组件比较，不会误展开。
        assert_eq!(
            expand_home("~bob/x".into(), Some(home)),
            PathBuf::from("~bob/x")
        );
        assert_eq!(
            expand_home("/abs".into(), Some(home)),
            PathBuf::from("/abs")
        );
        assert_eq!(expand_home("~/x".into(), None), PathBuf::from("~/x"));
    }
}
