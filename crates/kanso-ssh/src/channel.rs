//! shell 与 exec 通道（ssh.md §6、§10）。
//!
//! 通道持有所属 [`Connection`] 的一份引用：只要还有通道在用，连接就不会被断开；
//! 最后一个使用者释放后连接才真正断开。

use std::sync::{Arc, Mutex};

use kanso_types::profile::Locale;
use russh::client::Msg;
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};

use bytes::Bytes;

use crate::connection::Connection;
use crate::error::SshError;

/// PTY 终端类型（ssh.md §6）。
pub const TERM: &str = "xterm-256color";

/// shell 通道的请求参数。终端类型与环境变量是规格固定的，不由调用方指定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtyRequest {
    /// 初始列数。规格里初始是 80×24，之后按界面实际大小发 window-change。
    pub cols: u16,
    /// 初始行数。
    pub rows: u16,
    /// 用户在配置里**明确填写**的 locale；`Some` 时才发送 `LANG`。
    ///
    /// 类型 [`Locale`] 构造时已校验字符集与长度（只允许字母、数字和 `_ - . @`，长度 ≤ 64），
    /// 不合规的值根本构造不出来，所以不会被发送。自动模式传 `None`，交给服务器自己的设置。
    pub lang: Option<Locale>,
}

impl Default for PtyRequest {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            lang: None,
        }
    }
}

impl PtyRequest {
    /// 要设置的环境变量，按发送顺序。
    fn env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![
            ("COLORTERM", "truecolor".to_owned()),
            ("TERM_PROGRAM", "Kanso".to_owned()),
        ];
        if let Some(l) = &self.lang {
            env.push(("LANG", l.as_str().to_owned()));
        }
        env
    }
}

/// 通道上收到的退出信息。
#[derive(Default)]
struct ExitInfo {
    code: Option<u32>,
    signal: Option<String>,
}

/// 读端与写端共享的状态。
#[derive(Default)]
struct State {
    exit: Mutex<ExitInfo>,
}

impl State {
    fn lock(&self) -> std::sync::MutexGuard<'_, ExitInfo> {
        self.exit.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 通道的读端。
///
/// 持有连接引用，读端没丢掉之前连接不会断开。
pub struct ChannelReader {
    read: ChannelReadHalf,
    state: Arc<State>,
    merge_stderr: bool,
    _conn: Connection,
}

impl ChannelReader {
    /// 读下一块输出；通道关闭（或连接断开）返回 `None`。
    ///
    /// 退出码在读取过程中被记录（见 [`ChannelWriter::exit_code`]）。
    /// 把通道**关闭**而不是 EOF 作为结束信号：有的服务器在 eof 之后才发退出码，
    /// 只有读到 close（这里的 `None`）才能保证退出码已经到手。
    pub async fn read(&mut self) -> Option<Bytes> {
        loop {
            match self.read.wait().await? {
                ChannelMsg::Data { data } => return Some(data),
                ChannelMsg::ExtendedData { data, .. } if self.merge_stderr => return Some(data),
                ChannelMsg::ExitStatus { exit_status } => {
                    self.state.lock().code = Some(exit_status);
                }
                ChannelMsg::ExitSignal { signal_name, .. } => {
                    self.state.lock().signal = Some(format!("{signal_name:?}"));
                }
                ChannelMsg::Close => return None,
                // Eof、WindowAdjusted、Success 等：不影响输出流。
                _ => {}
            }
        }
    }

    /// 与 [`ChannelReader::read`] 相同，但把 stdout 与 stderr 分开返回，且不受 `merge_stderr` 影响。
    ///
    /// 只认 stderr 这一种扩展数据类型（RFC 4254 §5.2 只定义了它），其他类型直接忽略。
    /// 结束信号与退出码的记录规则同 `read`：以通道关闭（返回 `None`）为准。
    pub async fn read_split(&mut self) -> Option<ChannelChunk> {
        loop {
            match self.read.wait().await? {
                ChannelMsg::Data { data } => return Some(ChannelChunk::Stdout(data)),
                ChannelMsg::ExtendedData { data, ext: 1 } => {
                    return Some(ChannelChunk::Stderr(data));
                }
                ChannelMsg::ExitStatus { exit_status } => {
                    self.state.lock().code = Some(exit_status);
                }
                ChannelMsg::ExitSignal { signal_name, .. } => {
                    self.state.lock().signal = Some(format!("{signal_name:?}"));
                }
                ChannelMsg::Close => return None,
                _ => {}
            }
        }
    }
}

/// [`ChannelReader::read_split`] 读到的一块输出。
#[derive(Debug)]
pub enum ChannelChunk {
    /// 标准输出。
    Stdout(Bytes),
    /// 标准错误（`SSH_EXTENDED_DATA_STDERR`，RFC 4254 §5.2）。
    Stderr(Bytes),
}

/// 通道的写端，可克隆（比如输入线程与 resize 各持一份）。
#[derive(Clone)]
pub struct ChannelWriter {
    write: Arc<ChannelWriteHalf<Msg>>,
    state: Arc<State>,
    conn: Connection,
}

impl ChannelWriter {
    /// 写入数据（shell 的键盘输入、exec 的 stdin）。
    ///
    /// 失败说明连接已断：调用方应立即结束会话（ssh.md §6），返回的错误区分 keepalive 超时与其他中断。
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.write
            .data_bytes(Bytes::copy_from_slice(data))
            .await
            .map_err(|_| self.conn.lost_error())
    }

    /// 通知服务器终端尺寸变化（window-change）。仅对 shell 通道有意义。
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.write
            .window_change(u32::from(cols), u32::from(rows), 0, 0)
            .await
            .map_err(|_| self.conn.lost_error())
    }

    /// 发送 EOF（exec 写完 stdin 后调用，让远端命令读到输入结束）。
    pub async fn eof(&self) -> Result<(), SshError> {
        self.write.eof().await.map_err(|_| self.conn.lost_error())
    }

    /// 关闭通道。连接本身是否断开取决于是否还有其他使用者。
    pub async fn close(&self) -> Result<(), SshError> {
        self.write.close().await.map_err(|_| self.conn.lost_error())
    }

    /// 远端进程的退出码；还没收到为 `None`。通道关闭（读端返回 `None`）之后再取才可靠。
    pub fn exit_code(&self) -> Option<u32> {
        self.state.lock().code
    }

    /// 远端进程被信号终止时的信号名（`exit-signal`）；否则为 `None`。
    pub fn exit_signal(&self) -> Option<String> {
        self.state.lock().signal.clone()
    }
}

/// 等一次通道请求的回复（`want_reply = true` 的 pty / shell / exec）。
async fn wait_reply(read: &mut ChannelReadHalf, what: &str) -> Result<(), String> {
    loop {
        match read.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) => return Err(format!("{what} request denied by server")),
            Some(ChannelMsg::Close) | None => {
                return Err(format!("channel closed while waiting for {what} reply"));
            }
            // 回复之前可能夹着 WindowAdjusted 等无关消息。
            Some(_) => {}
        }
    }
}

/// 打开 session 通道并拆成读写两半。
async fn open_session(
    conn: &Connection,
) -> Result<(ChannelReadHalf, ChannelWriteHalf<Msg>), SshError> {
    // 连接已断时直接报"共享连接已断"，不尝试重连（ssh.md §6）。
    if conn.is_closed() {
        return Err(conn.closed_error());
    }
    match conn.handle().channel_open_session().await {
        Ok(ch) => Ok(ch.split()),
        // 发送失败 = 会话任务已经没了；其余是服务器拒绝开通道。
        Err(russh::Error::SendError) | Err(russh::Error::Disconnect) => Err(conn.closed_error()),
        Err(e) => Err(SshError::ChannelFailed {
            endpoint: conn.endpoint().clone(),
            detail: e.to_string(),
        }),
    }
}

/// 打开 session 通道并请求子系统（如 `sftp`），返回可直接读写的字节流。
///
/// 子系统被服务器拒绝时返回 [`SshError::ChannelFailed`]；连接已断返回"共享连接已断"。
/// 返回的流持有通道，流被丢弃时通道随之关闭；调用方需自己持有 `conn` 以保证连接存活。
pub(crate) async fn open_subsystem(
    conn: &Connection,
    name: &str,
) -> Result<russh::ChannelStream<Msg>, SshError> {
    if conn.is_closed() {
        return Err(conn.closed_error());
    }
    let mut ch = match conn.handle().channel_open_session().await {
        Ok(ch) => ch,
        Err(russh::Error::SendError) | Err(russh::Error::Disconnect) => {
            return Err(conn.closed_error());
        }
        Err(e) => {
            return Err(SshError::ChannelFailed {
                endpoint: conn.endpoint().clone(),
                detail: e.to_string(),
            });
        }
    };
    ch.request_subsystem(true, name)
        .await
        .map_err(|_| conn.closed_error())?;
    // 回复之前可能夹着 WindowAdjusted 等无关消息，所以不能只看第一条。
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Success) => break,
            Some(ChannelMsg::Failure) => {
                return Err(channel_failed(
                    conn,
                    format!("{name} subsystem request denied by server"),
                ));
            }
            Some(ChannelMsg::Close) | None => {
                return Err(channel_failed(
                    conn,
                    format!("channel closed while waiting for {name} subsystem reply"),
                ));
            }
            Some(_) => {}
        }
    }
    Ok(ch.into_stream())
}

fn channel_failed(conn: &Connection, detail: String) -> SshError {
    if conn.is_closed() {
        return conn.closed_error();
    }
    SshError::ChannelFailed {
        endpoint: conn.endpoint().clone(),
        detail,
    }
}

fn assemble(
    conn: &Connection,
    read: ChannelReadHalf,
    write: ChannelWriteHalf<Msg>,
    merge_stderr: bool,
) -> (ChannelReader, ChannelWriter) {
    let state = Arc::new(State::default());
    (
        ChannelReader {
            read,
            state: state.clone(),
            merge_stderr,
            _conn: conn.clone(),
        },
        ChannelWriter {
            write: Arc::new(write),
            state,
            conn: conn.clone(),
        },
    )
}

/// 交互式 shell 通道：stdout 与 stderr 合并成一条字节流。
pub struct ShellChannel {
    reader: ChannelReader,
    writer: ChannelWriter,
}

impl ShellChannel {
    pub(crate) async fn open(conn: &Connection, pty: PtyRequest) -> Result<Self, SshError> {
        let (mut read, write) = open_session(conn).await?;
        let to_err = |_: russh::Error| conn.closed_error();

        write
            .request_pty(
                true,
                crate::channel::TERM,
                u32::from(pty.cols),
                u32::from(pty.rows),
                0,
                0,
                &[],
            )
            .await
            .map_err(to_err)?;
        wait_reply(&mut read, "pty")
            .await
            .map_err(|d| channel_failed(conn, d))?;

        // 环境变量服务器经常忽略，失败不报错（ssh.md §6）：不要回复，也不检查结果。
        // 必须在 shell 请求之前发，shell 一旦启动再设置环境变量对它无效。
        for (k, v) in pty.env() {
            let _ = write.set_env(false, k, v).await;
        }

        write.request_shell(true).await.map_err(to_err)?;
        wait_reply(&mut read, "shell")
            .await
            .map_err(|d| channel_failed(conn, d))?;

        let (reader, writer) = assemble(conn, read, write, true);
        Ok(Self { reader, writer })
    }

    /// 读下一块输出（stdout 与 stderr 合并）；通道关闭返回 `None`。
    pub async fn read(&mut self) -> Option<Bytes> {
        self.reader.read().await
    }

    /// 写入键盘输入。
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.writer.write(data).await
    }

    /// 终端尺寸变化。
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshError> {
        self.writer.resize(cols, rows).await
    }

    /// 远端 shell 的退出码；通道关闭后才可靠。
    pub fn exit_code(&self) -> Option<u32> {
        self.writer.exit_code()
    }

    /// 关闭通道。
    pub async fn close(&self) -> Result<(), SshError> {
        self.writer.close().await
    }

    /// 拆成读写两半，供读循环与输入线程并发使用。
    pub fn into_parts(self) -> (ChannelReader, ChannelWriter) {
        (self.reader, self.writer)
    }
}

/// exec 通道：stdin 写入、stdout 读取，以**通道关闭**作为结束信号。
///
/// stderr 被丢弃（与 shell 通道不同）：cwd 查询这类用途只关心 stdout，
/// 混进 stderr 会污染结果行。
pub struct ExecChannel {
    reader: ChannelReader,
    writer: ChannelWriter,
}

impl ExecChannel {
    pub(crate) async fn open(conn: &Connection, command: &str) -> Result<Self, SshError> {
        Self::open_with_env(conn, command, &[]).await
    }

    /// 与 `open` 相同，但在 exec 请求之前发送环境变量。
    ///
    /// 环境变量必须先于 exec 请求发出，命令一旦启动再设置对它无效；服务器经常忽略 `env` 请求
    /// （OpenSSH 默认只放行 `AcceptEnv` 里列出的变量），所以不要回复也不检查结果（同 shell 通道，ssh.md §6）。
    pub(crate) async fn open_with_env(
        conn: &Connection,
        command: &str,
        env: &[(&str, &str)],
    ) -> Result<Self, SshError> {
        let (mut read, write) = open_session(conn).await?;
        for (k, v) in env {
            let _ = write.set_env(false, *k, *v).await;
        }
        write
            .exec(true, command)
            .await
            .map_err(|_| conn.closed_error())?;
        wait_reply(&mut read, "exec")
            .await
            .map_err(|d| channel_failed(conn, d))?;
        let (reader, writer) = assemble(conn, read, write, false);
        Ok(Self { reader, writer })
    }

    /// 读下一块 stdout；通道关闭返回 `None`。
    pub async fn read(&mut self) -> Option<Bytes> {
        self.reader.read().await
    }

    /// 读到通道关闭为止，返回全部 stdout。
    ///
    /// 有的服务器在 eof / close 之后才发退出码，所以以 close 为准；返回后 [`ExecChannel::exit_code`] 可靠。
    pub async fn read_to_close(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(chunk) = self.reader.read().await {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// 写入 stdin。
    pub async fn write(&self, data: &[u8]) -> Result<(), SshError> {
        self.writer.write(data).await
    }

    /// 写完 stdin 后发送 EOF。
    pub async fn eof(&self) -> Result<(), SshError> {
        self.writer.eof().await
    }

    /// 命令的退出码；通道关闭后才可靠。
    pub fn exit_code(&self) -> Option<u32> {
        self.writer.exit_code()
    }

    /// 关闭通道。
    pub async fn close(&self) -> Result<(), SshError> {
        self.writer.close().await
    }

    /// 拆成读写两半。
    pub fn into_parts(self) -> (ChannelReader, ChannelWriter) {
        (self.reader, self.writer)
    }
}
