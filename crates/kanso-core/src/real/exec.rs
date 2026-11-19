//! MCP 的后台命令执行（`mcp_exec`，spec/mcp.md §3.4、§5）：在已认证连接上另开 exec 通道，
//! 不请求 PTY，分别收集 stdout 与 stderr。

use std::sync::{Arc, Weak};
use std::time::Duration;

use kanso_encoding::{StreamDecoder, TextEncoding};
use kanso_session::Session;
use kanso_ssh::{ChannelChunk, ChannelWriter, Connection, SshError};
use kanso_types::SessionId;
use kanso_types::message::Endpoint;
use tokio::sync::Semaphore;

use super::RealCore;
use crate::api::{CoreError, Result};
use crate::mcp::{CapturedText, ExecOutcome, WaitLimits};

/// 每条连接同时进行的 exec 上限（spec/mcp.md §3.4），超出的排队。
pub(crate) const EXEC_CONCURRENCY: usize = 4;

/// 超时或被取消后等待远端确认关闭通道的上限：对端无响应不能卡住调用。
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// ANSI 控制序列剥离器：流式状态机，序列可以跨数据块。
///
/// 处理 CSI（`ESC [ ... 终结字节`）、OSC / DCS / SOS / PM / APC（以 BEL 或 `ESC \` 结束的字符串）与
/// 其他 ESC 序列（`ESC` + 可选中间字节 + 终结字节）。普通字符（含换行、制表符）原样保留。
/// 输入已经是解码后的 UTF-8，所以 C1 控制符只会以 U+009B / U+009D 这类码点出现。
#[derive(Default)]
pub(crate) struct AnsiStripper {
    state: AnsiState,
    /// 字符串类序列已吞掉的字符数，用于给"没有终结符"的序列设上限。
    string_len: usize,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum AnsiState {
    #[default]
    Ground,
    Esc,
    /// `ESC` + 中间字节（0x20-0x2F）之后，等终结字节。
    EscIntermediate,
    Csi,
    /// OSC / DCS / SOS / PM / APC 的字符串体。
    Str,
    /// 字符串体里刚读到 `ESC`，下一个 `\` 才算 ST。
    StrEsc,
}

/// 无终结符的字符串序列最多吞这么多字符，之后放弃剥离：防止一个未闭合的 OSC 吃掉后面全部输出。
const MAX_STRING_SEQUENCE: usize = 4096;

impl AnsiStripper {
    /// 处理一段文本，把非控制序列的字符追加到 `out`。
    pub(crate) fn feed(&mut self, text: &str, out: &mut String) {
        for ch in text.chars() {
            self.step(ch, out);
        }
    }

    fn step(&mut self, ch: char, out: &mut String) {
        match self.state {
            AnsiState::Ground => match ch {
                '\u{1b}' => self.state = AnsiState::Esc,
                '\u{9b}' => self.state = AnsiState::Csi,
                '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                    self.state = AnsiState::Str;
                    self.string_len = 0;
                }
                _ => out.push(ch),
            },
            AnsiState::Esc => match ch {
                '[' => self.state = AnsiState::Csi,
                // OSC、DCS、SOS、PM、APC
                ']' | 'P' | 'X' | '^' | '_' => {
                    self.state = AnsiState::Str;
                    self.string_len = 0;
                }
                '\u{20}'..='\u{2f}' => self.state = AnsiState::EscIntermediate,
                // 连续的 ESC：仍在等序列体。
                '\u{1b}' => {}
                // 单字符 ESC 序列的终结字节（含 `ESC \` 单独出现的 ST）。
                '\u{30}'..='\u{7e}' => self.state = AnsiState::Ground,
                // 其他（控制字符等）：不是合法序列，序列作废，字符按普通输出处理。
                _ => {
                    self.state = AnsiState::Ground;
                    self.step(ch, out);
                }
            },
            AnsiState::EscIntermediate => match ch {
                '\u{20}'..='\u{2f}' => {}
                '\u{30}'..='\u{7e}' => self.state = AnsiState::Ground,
                _ => {
                    self.state = AnsiState::Ground;
                    self.step(ch, out);
                }
            },
            AnsiState::Csi => match ch {
                // 参数字节与中间字节。
                '\u{20}'..='\u{3f}' => {}
                '\u{40}'..='\u{7e}' => self.state = AnsiState::Ground,
                // 序列中夹了控制字符：作废并按普通字符处理，避免吞掉换行。
                _ => {
                    self.state = AnsiState::Ground;
                    self.step(ch, out);
                }
            },
            AnsiState::Str => match ch {
                '\u{07}' | '\u{9c}' => self.state = AnsiState::Ground,
                '\u{1b}' => self.state = AnsiState::StrEsc,
                _ => {
                    self.string_len += 1;
                    if self.string_len > MAX_STRING_SEQUENCE {
                        self.state = AnsiState::Ground;
                    }
                }
            },
            AnsiState::StrEsc => match ch {
                '\\' => self.state = AnsiState::Ground,
                // ESC 后不是 `\`：字符串被打断，这个 ESC 开启了新的序列。
                _ => {
                    self.state = AnsiState::Esc;
                    self.step(ch, out);
                }
            },
        }
    }
}

/// 只保留末尾的文本缓冲，并统计总字节数；内存有界。
struct TailBuf {
    max: usize,
    buf: String,
    total: u64,
}

impl TailBuf {
    fn push(&mut self, text: &str) {
        self.total += text.len() as u64;
        self.buf.push_str(text);
        // 攒到两倍上限才整理一次，摊还代价是 O(1)；最终结果由 `finish` 再精确截到 `max`。
        if self.buf.len() > self.max.saturating_mul(2).max(self.max + 1) {
            self.trim();
        }
    }

    fn trim(&mut self) {
        let mut start = self.buf.len().saturating_sub(self.max);
        while !self.buf.is_char_boundary(start) {
            start += 1;
        }
        self.buf.drain(..start);
    }
}

/// 一路输出（stdout 或 stderr）的收集器：解码 → 去 ANSI → 保留末尾。
pub(crate) struct Collector {
    decoder: StreamDecoder,
    strip: AnsiStripper,
    tail: TailBuf,
    decoded: String,
    clean: String,
}

impl Collector {
    pub(crate) fn new(encoding: TextEncoding, max_bytes: usize) -> Collector {
        Collector {
            decoder: StreamDecoder::new(encoding),
            strip: AnsiStripper::default(),
            tail: TailBuf {
                max: max_bytes,
                buf: String::new(),
                total: 0,
            },
            decoded: String::new(),
            clean: String::new(),
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.decoded.clear();
        self.decoder.decode(bytes, &mut self.decoded);
        self.clean.clear();
        self.strip.feed(&self.decoded, &mut self.clean);
        self.tail.push(&self.clean);
    }

    /// 结果。`original_bytes` 是去掉控制序列之后、截断之前的 UTF-8 字节总数。
    pub(crate) fn finish(mut self) -> CapturedText {
        self.tail.trim();
        let text = std::mem::take(&mut self.tail.buf);
        CapturedText {
            truncated: self.tail.total > text.len() as u64,
            original_bytes: self.tail.total,
            text,
            cleared: false,
        }
    }
}

/// 通道的 Drop guard：执行 future 被丢弃（客户端断开、调用被取消，spec/mcp.md §4.4）时关闭通道。
///
/// 不能省：russh 丢弃通道的读写端并不保证向服务器发 close，远端命令会继续占着通道。
/// Drop 是同步的，关闭要 await，所以交给运行时；没有运行时（进程退出阶段）时放弃，连接随之断开。
#[derive(Default)]
struct ChannelGuard(Option<ChannelWriter>);

impl Drop for ChannelGuard {
    fn drop(&mut self) {
        if let Some(writer) = self.0.take()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            rt.spawn(async move {
                let _ = tokio::time::timeout(CLOSE_WAIT, writer.close()).await;
            });
        }
    }
}

fn ssh_err(e: &SshError) -> CoreError {
    CoreError::User(e.to_message())
}

/// 在 `conn` 上执行一条命令。排队、开通道、收集全部计入 `limits.timeout`。
///
/// - `slots`：这条连接的并发限额，调用方保证整个调用期间持有它的 `Arc`（见 `RealCore::exec_slots`）。
/// - 以**通道关闭**为结束信号（有的服务器在 eof 之后才发退出码，ssh.md §10）。
/// - 发送 EOF：命令没有 stdin，若不关闭输入，读 stdin 的命令（`cat`、`sudo` 提示等）会一直挂到超时。
pub(crate) async fn run_exec(
    conn: &Connection,
    command: &str,
    lang: Option<&str>,
    encoding: TextEncoding,
    slots: &Semaphore,
    limits: WaitLimits,
) -> Result<ExecOutcome> {
    let mut out = Collector::new(encoding, limits.max_bytes);
    let mut err = Collector::new(encoding, limits.max_bytes);
    let mut exit: Option<u32> = None;
    // 声明在 `run` 之前、与 future 同生命周期：整个调用被丢弃时 guard 随之 Drop 关闭通道。
    let mut guard = ChannelGuard::default();

    let run = async {
        let _permit = slots.acquire().await.map_err(|_| CoreError::Internal {
            detail: "exec semaphore closed".to_owned(),
        })?;
        let env: Vec<(&str, &str)> = lang.map(|l| ("LANG", l)).into_iter().collect();
        let channel = conn
            .open_exec_with_env(command, &env)
            .await
            .map_err(|e| ssh_err(&e))?;
        let (mut reader, writer) = channel.into_parts();
        guard.0 = Some(writer.clone());
        // 失败说明连接已断，读端马上会结束，下面统一处理。
        let _ = writer.eof().await;
        while let Some(chunk) = reader.read_split().await {
            match chunk {
                ChannelChunk::Stdout(bytes) => out.push(&bytes),
                ChannelChunk::Stderr(bytes) => err.push(&bytes),
            }
        }
        exit = writer.exit_code();
        // 通道已由服务器关闭，guard 不必再发 close。
        guard.0 = None;
        Ok::<(), CoreError>(())
    };

    let timed_out = match tokio::time::timeout(limits.timeout, run).await {
        Ok(result) => {
            result?;
            false
        }
        Err(_) => {
            // 超时：关闭通道（远端进程可能仍在运行，很多服务器不处理 signal）。
            if let Some(writer) = guard.0.take() {
                let _ = tokio::time::timeout(CLOSE_WAIT, writer.close()).await;
            }
            true
        }
    };

    // 通道结束但没有退出码、且连接已断：这是断线而不是命令结束。
    if !timed_out && exit.is_none() && conn.is_closed() {
        let why = conn.close_reason().unwrap_or(kanso_ssh::CloseReason::Lost);
        return Err(ssh_err(&why.to_error(conn.endpoint())));
    }
    Ok(ExecOutcome {
        stdout: out.finish(),
        stderr: err.finish(),
        exit_status: if timed_out { None } else { exit },
        timed_out,
    })
}

impl RealCore {
    /// 这条连接的并发限额。表里只存弱引用：所有进行中（含排队）的调用结束后信号量随之释放，
    /// 下次访问时顺带清掉失效条目，所以连接关闭后表不会无限增长，也不需要连接关闭的通知。
    fn exec_slots_for(&self, connection_id: u64) -> Arc<Semaphore> {
        let mut table = self.exec_slots.lock().unwrap_or_else(|e| e.into_inner());
        table.retain(|_, weak| weak.strong_count() > 0);
        if let Some(slots) = table.get(&connection_id).and_then(Weak::upgrade) {
            return slots;
        }
        let slots = Arc::new(Semaphore::new(EXEC_CONCURRENCY));
        table.insert(connection_id, Arc::downgrade(&slots));
        slots
    }

    pub(crate) async fn mcp_exec_impl(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<ExecOutcome> {
        let Session::Ssh(ssh) = self.session(session)? else {
            return Err(CoreError::NotSupported);
        };
        let closed = |endpoint: &Endpoint| {
            CoreError::User(
                SshError::SharedConnectionClosed {
                    endpoint: endpoint.clone(),
                }
                .to_message(),
            )
        };
        // 连接引用在整个执行期间持有：最后一个标签恰好关闭时，断开最多推迟到这次执行结束。
        let Some(conn) = ssh.connection().filter(|c| !c.is_closed()) else {
            return Err(closed(&ssh.launch().endpoint));
        };
        let slots = self.exec_slots_for(conn.id());
        let launch = ssh.launch();
        run_exec(
            &conn,
            command,
            launch.locale.as_ref().map(|l| l.as_str()),
            launch.encoding,
            &slots,
            limits,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_chunks(chunks: &[&str]) -> String {
        let mut s = AnsiStripper::default();
        let mut out = String::new();
        for c in chunks {
            s.feed(c, &mut out);
        }
        out
    }

    #[test]
    fn strips_csi_osc_and_other_escapes() {
        assert_eq!(strip_chunks(&["\x1b[31mred\x1b[0m ok\n"]), "red ok\n");
        assert_eq!(strip_chunks(&["a\x1b[2K\x1b[1;32mb\x1b[m"]), "ab");
        assert_eq!(strip_chunks(&["\x1b]0;title\x07text"]), "text");
        assert_eq!(
            strip_chunks(&["\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"]),
            "link"
        );
        assert_eq!(strip_chunks(&["\x1b(Bplain\x1b=\x1b>"]), "plain");
        assert_eq!(strip_chunks(&["\x1bPdcs data\x1b\\after"]), "after");
    }

    #[test]
    fn sequences_may_span_chunks() {
        assert_eq!(
            strip_chunks(&["x\x1b", "[3", "1mred\x1b]0;ti", "tle\x07", "!"]),
            "xred!"
        );
    }

    #[test]
    fn keeps_plain_controls_and_unicode() {
        assert_eq!(strip_chunks(&["a\tb\r\n中文\n"]), "a\tb\r\n中文\n");
    }

    #[test]
    fn aborted_csi_does_not_swallow_newline() {
        assert_eq!(strip_chunks(&["\x1b[12\nnext"]), "\nnext");
    }

    #[test]
    fn unterminated_osc_is_bounded() {
        let long = format!("\x1b]0;{}", "a".repeat(MAX_STRING_SEQUENCE + 10));
        let out = strip_chunks(&[&long, "tail"]);
        assert!(out.ends_with("tail"), "{out:?}");
    }

    #[test]
    fn collector_keeps_tail_with_true_total() {
        let mut c = Collector::new(TextEncoding::utf8(), 8);
        for _ in 0..100 {
            c.push("0123456789".as_bytes());
        }
        let r = c.finish();
        assert_eq!(r.original_bytes, 1000);
        assert!(r.truncated);
        assert_eq!(r.text.len(), 8);
        assert_eq!(r.text, "23456789");
    }

    #[test]
    fn collector_cuts_on_char_boundary_and_strips_ansi() {
        let mut c = Collector::new(TextEncoding::utf8(), 4);
        c.push("\x1b[1mab中文\x1b[0m".as_bytes());
        let r = c.finish();
        // "ab中文" 共 8 字节；保留末尾 4 字节时从字符边界起，只剩 "文"。
        assert_eq!(r.text, "文");
        assert_eq!(r.original_bytes, 8);
        assert!(r.truncated);
    }

    #[test]
    fn collector_decodes_split_multibyte() {
        let bytes = "中".as_bytes();
        let mut c = Collector::new(TextEncoding::utf8(), 100);
        c.push(&bytes[..1]);
        c.push(&bytes[1..]);
        assert_eq!(c.finish().text, "中");
    }
}
