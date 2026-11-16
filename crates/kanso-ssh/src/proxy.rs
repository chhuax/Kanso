//! ProxyCommand 传输（ssh.md §5，仅 Direct 渠道）。
//!
//! 用外部命令的 stdin / stdout 作为 SSH 的字节流（cloudflared、AWS SSM 等场景）。
//! 命令经 `/bin/sh -c`（Windows 为 `cmd /C`）执行，**不继承我们的 PTY**：stdin / stdout / stderr 全是管道。
//!
//! # 子进程生命周期
//!
//! 子进程的所有权在一个监督任务里：字节流（[`ProxyStream`]）被丢弃——会话结束、握手失败都会——
//! 就通知监督任务，它先给命令一小段时间因 stdin 关闭而自行退出，超时再 kill，**并且一定 wait 回收**，
//! 避免僵尸进程。`kill_on_drop` 只是运行时已关闭时的兜底。
//!
//! 局限：kill 只作用于 `sh` 本身。`sh -c` 执行单条命令时 shell 会直接 `exec` 它（常见情形），
//! 复合命令里的孙进程靠管道关闭（EOF / SIGPIPE）自行退出。

use std::io;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use kanso_types::message::Reason;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, watch};

use crate::connection::{ConnectParams, Connected, connect_over};
use crate::error::SshError;
use crate::interaction::Interaction;

/// 保留 stderr 的最后多少字节，用于错误信息。
const STDERR_TAIL_BYTES: usize = 4096;
/// 关闭字节流后，等命令自行退出的宽限时间，超时才 kill。
const EXIT_GRACE: Duration = Duration::from_millis(300);
/// 等监督任务给出最终结果的上限（宽限 + 回收 + 读完 stderr 都有各自的上限，这里只是总兜底）。
const OUTCOME_WAIT: Duration = Duration::from_secs(3);
/// 命令退出后继续读 stderr 的最长时间（孙进程可能还握着管道）。
const STDERR_DRAIN: Duration = Duration::from_millis(500);

/// 按 OpenSSH 规则替换 ProxyCommand 里的 `%h`（目标主机）、`%p`（端口）、`%r`（远端用户）与 `%%`。
///
/// 其他 `%x` 一律报错而不是原样保留：OpenSSH 同样拒绝未知的替换符，原样保留会让命令悄悄带着
/// 字面量 `%x` 运行。替换值含 shell 元字符时也报错：值最终会进 `sh -c` 的命令行，
/// 主机名 / 用户名来自可导入的配置，不能让它们变成命令注入（OpenSSH 同样限制这些字符）。
/// 返回 `Err(技术细节)`，调用方包装成 [`SshError::ProxyCommandFailed`]。
pub fn expand_proxy_command(
    template: &str,
    host: &str,
    port: u16,
    user: &str,
) -> Result<String, String> {
    fn safe(value: &str) -> bool {
        // 主机名、IPv6 字面量（含 `:`、zone id 里的 `%`）与常见用户名字符；空白、引号、`;|&$` 等都排除。
        !value.is_empty()
            && value.chars().all(|c| {
                c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '@' | '+' | '%')
            })
    }
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('h') if safe(host) => out.push_str(host),
            Some('r') if safe(user) => out.push_str(user),
            Some('p') => out.push_str(&port.to_string()),
            Some(t @ ('h' | 'r')) => {
                return Err(format!("unsafe characters in value for %{t}"));
            }
            Some(other) => return Err(format!("unknown token %{other}")),
            None => return Err("trailing % in command".into()),
        }
    }
    Ok(out)
}

/// 监督任务给出的最终结果。
struct Outcome {
    /// 命令在我们要求它结束**之前**（含宽限期内）自己退出时的状态；被 kill 为 `None`。
    natural_exit: Option<ExitStatus>,
}

/// 丢弃时通知监督任务结束子进程。
struct KillOnDrop {
    _tx: oneshot::Sender<()>,
}

/// 子进程 stdin / stdout 拼成的双向字节流。
struct ProxyStream {
    stdin: ChildStdin,
    stdout: ChildStdout,
    /// stdout 已读到 EOF：握手失败时据此判断"是命令先退出了"，而不是服务器先断开。
    stdout_eof: Arc<AtomicBool>,
    _guard: KillOnDrop,
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.stdout).poll_read(cx, buf);
        if matches!(r, Poll::Ready(Ok(()))) && buf.filled().len() == before {
            self.stdout_eof.store(true, Ordering::SeqCst);
        }
        r
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

/// 子进程监督：等它自行退出，或在收到"结束"通知后给宽限期再 kill；两种情况都 wait 回收，最后发布结果。
async fn supervise(
    mut child: tokio::process::Child,
    stop: oneshot::Receiver<()>,
    stderr_task: tokio::task::JoinHandle<()>,
    outcome: watch::Sender<Option<Outcome>>,
) {
    let natural_exit = tokio::select! {
        status = child.wait() => status.ok(),
        // 发送端被丢弃（`Err`）也是"结束"通知，这正是 `KillOnDrop` 的用法。
        _ = stop => match tokio::time::timeout(EXIT_GRACE, child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                // kill() 会等待进程被回收。
                let _ = child.kill().await;
                None
            }
        },
    };
    // 让 stderr 读尽，错误信息才完整；孙进程握着管道时不能无限等。
    let _ = tokio::time::timeout(STDERR_DRAIN, stderr_task).await;
    let _ = outcome.send(Some(Outcome { natural_exit }));
}

/// 只保留 `buf` 的最后 [`STDERR_TAIL_BYTES`] 字节。
fn push_tail(buf: &mut Vec<u8>, chunk: &[u8]) {
    buf.extend_from_slice(chunk);
    if buf.len() > STDERR_TAIL_BYTES {
        let excess = buf.len() - STDERR_TAIL_BYTES;
        buf.drain(..excess);
    }
}

/// 把退出状态与 stderr 摘要写成失败原因（英文技术信息）。
fn describe_exit(status: Option<ExitStatus>, stderr: &[u8]) -> Reason {
    let mut detail = match status {
        Some(s) => match s.code() {
            Some(code) => format!("exited with code {code}"),
            None => "terminated by signal".to_owned(),
        },
        None => "exited".to_owned(),
    };
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if !text.is_empty() {
        detail.push_str(": ");
        detail.push_str(text);
    }
    Reason::Other { detail }
}

/// 启动 ProxyCommand 并在它的 stdin / stdout 上握手认证。
///
/// 命令在握手完成前退出（含启动失败）返回 [`SshError::ProxyCommandFailed`]，原因带退出码与 stderr 摘要；
/// 握手阶段的其他错误（主机密钥变更、认证失败等）原样返回。成功时子进程随返回的连接存活，
/// 连接释放时被结束并回收。
pub(crate) async fn connect_via_proxy_command<I: Interaction>(
    template: &str,
    params: ConnectParams,
    interaction: &I,
) -> Result<Connected, SshError> {
    let failed =
        |endpoint: &kanso_types::message::Endpoint, reason: Reason| SshError::ProxyCommandFailed {
            endpoint: endpoint.clone(),
            command: template.to_owned(),
            reason,
        };
    let endpoint = params.endpoint.clone();
    let expanded = expand_proxy_command(template, &endpoint.host, endpoint.port, &params.user)
        .map_err(|detail| failed(&endpoint, Reason::Other { detail }))?;

    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&expanded);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd.exe");
        // 命令行原样交给 cmd，不能让标准库按 argv 规则再转义一遍。
        c.raw_arg("/C").raw_arg(&expanded);
        // 不弹控制台窗口。
        c.creation_flags(0x0800_0000);
        c
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| failed(&endpoint, Reason::from(&e)))?;
    let (Some(stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(failed(
            &endpoint,
            Reason::Other {
                detail: "child stdio pipes missing".into(),
            },
        ));
    };

    let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let tail_w = Arc::clone(&tail);
    let stderr_task = tokio::spawn(async move {
        let mut chunk = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            push_tail(
                &mut tail_w.lock().unwrap_or_else(|e| e.into_inner()),
                &chunk[..n],
            );
        }
    });
    let (stop_tx, stop_rx) = oneshot::channel();
    let (outcome_tx, mut outcome_rx) = watch::channel(None);
    tokio::spawn(supervise(child, stop_rx, stderr_task, outcome_tx));

    let stdout_eof = Arc::new(AtomicBool::new(false));
    let stream = ProxyStream {
        stdin,
        stdout,
        stdout_eof: Arc::clone(&stdout_eof),
        _guard: KillOnDrop { _tx: stop_tx },
    };
    match connect_over(stream, params, interaction).await {
        Ok(connected) => Ok(connected),
        // 只有"传输层失败且是命令这一端先结束"才算命令失败；服务器先断开、协商失败等仍按原错误报。
        Err(e @ SshError::ConnectFailed { .. }) if stdout_eof.load(Ordering::SeqCst) => {
            // 流已随失败的握手被丢弃，监督任务正在收尾；等它给出结果。
            let outcome =
                tokio::time::timeout(OUTCOME_WAIT, outcome_rx.wait_for(Option::is_some)).await;
            let natural = match outcome {
                Ok(Ok(o)) => o.as_ref().and_then(|o| o.natural_exit),
                _ => None,
            };
            match natural {
                Some(status) => {
                    let stderr = tail.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    Err(failed(&endpoint, describe_exit(Some(status), &stderr)))
                }
                None => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_h_p_r_and_percent() {
        assert_eq!(
            expand_proxy_command("nc %h %p # %r 100%%", "example.com", 2222, "alice").unwrap(),
            "nc example.com 2222 # alice 100%"
        );
    }

    #[test]
    fn plain_command_is_unchanged() {
        assert_eq!(
            expand_proxy_command("cloudflared access ssh", "h", 22, "u").unwrap(),
            "cloudflared access ssh"
        );
    }

    #[test]
    fn ipv6_literal_is_allowed() {
        assert_eq!(
            expand_proxy_command("nc %h %p", "fe80::1%en0", 22, "u").unwrap(),
            "nc fe80::1%en0 22"
        );
    }

    #[test]
    fn unknown_and_trailing_tokens_are_rejected() {
        assert!(expand_proxy_command("nc %n", "h", 22, "u").is_err());
        assert!(expand_proxy_command("nc %", "h", 22, "u").is_err());
    }

    #[test]
    fn shell_metacharacters_in_values_are_rejected() {
        assert!(expand_proxy_command("nc %h %p", "h; rm -rf ~", 22, "u").is_err());
        assert!(expand_proxy_command("echo %r", "h", 22, "u`id`").is_err());
        assert!(expand_proxy_command("echo %r", "h", 22, "").is_err());
        // 与替换值无关的模板内容不受限制。
        assert!(expand_proxy_command("echo 'a b' | nc %h %p", "h", 22, "u").is_ok());
    }

    #[test]
    fn tail_keeps_last_bytes_only() {
        let mut b = Vec::new();
        push_tail(&mut b, &vec![b'a'; STDERR_TAIL_BYTES]);
        push_tail(&mut b, b"xyz");
        assert_eq!(b.len(), STDERR_TAIL_BYTES);
        assert!(b.ends_with(b"xyz"));
    }
}
