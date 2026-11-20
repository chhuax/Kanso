//! 本地 PTY 会话：启动、读写、调整大小、前台进程 cwd、结束（local-shell.md §6、§7）。

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;

use kanso_types::GridSize;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::command::CommandSpec;
use crate::cwd::process_cwd;
use crate::env::EnvSnapshot;
use crate::error::PtyError;

/// 启动一个本地 PTY 会话所需的全部输入。
#[derive(Clone, Debug)]
pub struct SpawnConfig {
    /// 要启动的程序与参数，通常来自 [`crate::resolve_command`]。
    pub command: CommandSpec,
    /// 子进程的**完整**环境（不再额外继承本进程的环境）：
    /// 调用方用 [`crate::build_env`] 的结果对 [`EnvSnapshot`] 做 `apply`。
    pub env: EnvSnapshot,
    /// 起始目录：配置里填的目录，否则用户主目录（由调用方决定）。
    /// 目录不存在时底层会静默退回主目录，调用方如需提示应事先自己检查。
    pub cwd: PathBuf,
    /// 初始终端尺寸（通常 80×24），之后用 [`PtySession::resize`] 跟随界面。
    pub size: GridSize,
}

/// 子进程的退出状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitStatus {
    code: Option<u32>,
    signal: Option<String>,
}

impl ExitStatus {
    /// 退出码；被信号终止时为 `None`（此时见 [`ExitStatus::signal`]）。
    pub fn code(&self) -> Option<u32> {
        self.code
    }

    /// 终止进程的信号描述（如 `Hangup: 1`）；正常退出为 `None`。
    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }

    /// 是否以退出码 0 正常结束。
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

impl From<portable_pty::ExitStatus> for ExitStatus {
    fn from(s: portable_pty::ExitStatus) -> Self {
        match s.signal() {
            // portable-pty 对信号终止的进程把 code 填成 1，这里不把它当真实退出码。
            Some(sig) => ExitStatus {
                code: None,
                signal: Some(sig.to_owned()),
            },
            None => ExitStatus {
                code: Some(s.exit_code()),
                signal: None,
            },
        }
    }
}

/// 一个运行中的本地 PTY 会话。
///
/// 读循环由会话层在专门的线程里跑 [`PtySession::reader`] 取得的读取端；其余方法在会话 actor 里调用。
/// 丢弃（drop）会话会关闭 PTY 主端；shell 还活着时请先调用 [`PtySession::kill`]。
pub struct PtySession {
    /// Windows 上子进程退出后会被 [`PtySession::detach_master`] 取走（关闭伪控制台）；其余时候总是 `Some`。
    master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    /// `take_writer` 已经把真正的写入端交走（`writer` 此后是丢弃一切的占位）。
    writer_taken: bool,
    pid: Option<u32>,
    /// PTY 从设备的路径（如 `/dev/ttys003`）。在 spawn 时取一次：之后主端可能被取走（Windows），
    /// 而且它不会变。只有 Unix 有这个概念，其余平台为 `None`。
    tty_path: Option<String>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

fn to_pty_size(size: GridSize, pixel_width: u16, pixel_height: u16) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width,
        pixel_height,
    }
}

fn spawn_err(e: impl std::fmt::Display) -> PtyError {
    PtyError::Spawn(e.to_string())
}

/// 串行化"打开 PTY + fork 子进程"。
///
/// macOS 上并发打开 PTY 会偶发 `openpty` 失败（`grantpt` / `unlockpt` 一类调用在多线程下互相干扰，
/// 错误码是莫名其妙的 -6）；并且 portable-pty 是先打开 fd、之后才设置 close-on-exec，
/// 另一个线程恰好在这两步之间 fork，就会把别的会话的 PTY fd 泄漏进自己的 shell。
/// 启动本来就不在热路径上（每次打开标签一次，毫秒级），全局串行最简单可靠。
static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl PtySession {
    /// 打开 PTY 并启动子进程。可从任意线程并发调用，内部会串行化（见 `SPAWN_LOCK`）。
    pub fn spawn(config: SpawnConfig) -> Result<PtySession, PtyError> {
        // 锁里没有会留下半更新状态的数据，中毒后继续使用即可。
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let pair = native_pty_system()
            .openpty(to_pty_size(config.size, 0, 0))
            .map_err(spawn_err)?;

        let mut argv: Vec<OsString> = Vec::with_capacity(1 + config.command.args.len());
        argv.push(OsString::from(&config.command.program));
        argv.extend(config.command.args.iter().map(OsString::from));
        let mut cmd = CommandBuilder::from_argv(argv);
        // `from_argv` 会先拷贝本进程的环境；清空后只放入调用方给定的完整环境，
        // 避免宿主应用（GUI 进程）自己的变量悄悄混进去，让三类清单（继承/删除/覆盖）失去意义。
        cmd.env_clear();
        for (k, v) in config.env.iter() {
            cmd.env(k, v);
        }
        cmd.cwd(&config.cwd);

        let child = pair.slave.spawn_command(cmd).map_err(spawn_err)?;
        // 父进程里必须关闭 PTY 的从端：从端的 fd 若一直开着，即使 shell 退出了，
        // 内核也认为还有进程持有从端，主端的读操作永远等不到 EOF（local-shell.md §7）。
        // portable-pty 的 spawn 只借用从端、不会替我们关闭，所以这里显式 drop；
        // 子进程自己拿到的是 fork 时复制的那份，不受影响。
        drop(pair.slave);

        let writer = pair.master.take_writer().map_err(spawn_err)?;
        let pid = child.process_id();
        #[cfg(unix)]
        let tty_path = pair
            .master
            .tty_name()
            .map(|p| p.to_string_lossy().into_owned());
        #[cfg(not(unix))]
        let tty_path = None;
        Ok(PtySession {
            master: Some(pair.master),
            child,
            writer,
            writer_taken: false,
            pid,
            tty_path,
        })
    }

    /// 取得一个新的读取端（阻塞读）。shell 退出且从端关闭后读到 EOF（`Ok(0)`）。
    ///
    /// portable-pty 把 Unix 上从端关闭时的 `EIO` 转成了 EOF，调用方不必再自己处理。
    /// 每次调用返回对同一个主端的独立句柄；通常只在会话层的读线程里调用一次。
    pub fn reader(&self) -> Result<Box<dyn Read + Send>, PtyError> {
        self.master()?.try_clone_reader().map_err(spawn_err)
    }

    /// 写入端（用户输入、终端应答等都从这里发给 shell）。
    ///
    /// 注意：写入端被丢弃时 portable-pty 会向 shell 补发一个换行和 EOF 字符，
    /// 所以会话存活期间不要丢弃会话，关闭时请先 [`PtySession::kill`]。
    /// 已经被 [`PtySession::take_writer`] 交走时，这里返回丢弃一切的占位。
    pub fn writer(&mut self) -> &mut (dyn Write + Send) {
        &mut *self.writer
    }

    /// 把写入端交给调用方独占（只能成功一次，之后返回 `None`）。
    ///
    /// 为什么需要：会话层要把"写 PTY"放进专门的写线程——PTY 缓冲区满（shell 不读输入）时
    /// `write` 会一直阻塞，如果写入端还留在 `PtySession` 里、靠 `&mut self` 访问，写线程就得
    /// 一直占着整个会话的锁，`resize` / `kill` 全被卡住。交走之后，本结构体里的 `writer()`
    /// 退化为丢弃一切的占位。
    ///
    /// 调用方注意：写入端被丢弃时 portable-pty 会向 shell 补发换行和 EOF，
    /// 所以要在 [`PtySession::kill`] 之后再丢弃它（或确认要让 shell 收到 EOF）。
    pub fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        if self.writer_taken {
            return None;
        }
        self.writer_taken = true;
        Some(std::mem::replace(
            &mut self.writer,
            Box::new(std::io::sink()),
        ))
    }

    /// 调整 PTY 尺寸。像素尺寸给 0 表示未知；shell 会收到 SIGWINCH。
    pub fn resize(
        &self,
        size: GridSize,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), PtyError> {
        // 主端已被取走（Windows 上 shell 已退出）时没有东西可调整。
        match &self.master {
            Some(master) => master
                .resize(to_pty_size(size, pixel_width, pixel_height))
                .map_err(spawn_err),
            None => Ok(()),
        }
    }

    fn master(&self) -> Result<&(dyn MasterPty + Send), PtyError> {
        self.master
            .as_deref()
            .ok_or_else(|| spawn_err("pty master already closed"))
    }

    /// 取走 PTY 主端，由调用方在**不持有会话锁**时丢弃它。
    ///
    /// 只用于 Windows：ConPTY 里子进程退出后，只要伪控制台没关闭，输出管道就不会 EOF；
    /// portable-pty 在主端被丢弃时才调用 `ClosePseudoConsole`（它会把残留输出冲给读端再关闭），
    /// 所以 shell 退出后由会话层调用本方法并丢弃返回值，读线程读完剩余输出后即得到 EOF。
    /// 丢弃可能阻塞到输出被读走，因此不能在持锁时丢弃。
    #[cfg(windows)]
    pub fn detach_master(&mut self) -> Option<Box<dyn Send>> {
        self.master.take().map(|m| Box::new(m) as Box<dyn Send>)
    }

    /// shell 进程的 pid。
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// PTY 从设备的路径；非 Unix 平台或系统没有给出时为 `None`。
    /// MCP 用它按控制终端识别客户端所在的标签（spec/mcp.md §2.5）。
    pub fn tty_path(&self) -> Option<&str> {
        self.tty_path.as_deref()
    }

    /// 结束 shell 进程并回收（避免僵尸进程）。
    ///
    /// 顺序是先结束进程、再回收资源（local-shell.md §7）：先发 SIGHUP 并给一小段宽限时间，
    /// 超时再 SIGKILL（由 portable-pty 完成），然后 `wait` 回收。shell 收到 SIGHUP 会把它
    /// 的作业一起结束；即使 shell 被 SIGKILL，会话首进程退出时内核也会向前台进程组发 SIGHUP。
    /// 进程已经退出时直接成功。会阻塞最多约 250 ms，请在后台线程或 `spawn_blocking` 里调用。
    pub fn kill(&mut self) -> Result<(), PtyError> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }

    /// 非阻塞地查询是否已退出。
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        Ok(self.child.try_wait()?.map(ExitStatus::from))
    }

    /// 阻塞等待退出。
    pub fn wait(&mut self) -> Result<ExitStatus, PtyError> {
        Ok(self.child.wait()?.into())
    }

    /// 前台进程的当前工作目录（local-shell.md §6）：嵌套 shell、`sudo -s`、从 shell 启动的程序都能查对。
    ///
    /// 用 PTY 主端的 `tcgetpgrp`（portable-pty 的 `process_group_leader`）取得前台进程组，
    /// 读组长进程的 cwd。组长可能已经退出（管道的第一段通常最先退出），此时读不到，
    /// 退回查 shell 进程本身。Windows 没有对应的系统查询，返回 `None`。
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        // `process_group_leader` 只在 portable-pty 的 Unix 实现里存在。
        #[cfg(unix)]
        if let Some(leader) = self.master.as_ref().and_then(|m| m.process_group_leader())
            && let Ok(pid) = u32::try_from(leader)
            && let Some(p) = process_cwd(pid)
        {
            return Some(p);
        }
        self.pid.and_then(process_cwd)
    }
}
