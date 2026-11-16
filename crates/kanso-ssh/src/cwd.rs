//! 远端工作目录查询（ssh.md §10）：没有 OSC 7 上报时，在同一连接上另开 exec 通道执行探测脚本。
//!
//! 脚本在 `cwd_probe.sh`（`include_str!`），通过 stdin 交给 `sh`，不碰正在使用的 shell 通道。
//! 本模块负责：跑脚本、5 秒超时、解析带前缀的结果行、把失败码映射成 [`CwdError`]。

use std::time::Duration;

use kanso_types::message::Message;

use crate::connection::Connection;

/// 探测脚本（POSIX sh）。测试里可以直接用本机 `/bin/sh` 配合伪造的 `/proc` 目录运行它。
pub const PROBE_SCRIPT: &str = include_str!("cwd_probe.sh");

/// 结果行的前缀。登录 rc 文件可能往 stdout 打印杂讯，只认带这个前缀的行。
pub const RESULT_PREFIX: &str = "__KANSO_CWD__";

/// 整个查询（开通道、写脚本、读结果）的超时（ssh.md §10）。
pub const CWD_TIMEOUT: Duration = Duration::from_secs(5);

/// 远端 cwd 查询失败的原因；各项对应 `kanso-types` 里已有的 `cwd-*` 消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CwdError {
    /// 没找到这个连接的 shell。
    NoShell,
    /// 服务器没有设置 `SSH_CONNECTION`。
    NoSshConnection,
    /// 前台进程属于别的用户（sudo、su），读不到它的 cwd。
    OtherUser,
    /// 服务器没有 `/proc`。
    NoProc,
    /// 查询超过 [`CWD_TIMEOUT`]。
    Timeout,
    /// 其他情形：开通道失败、连接已断、输出里没有结果行。`detail` 为英文技术信息，只进日志。
    Unavailable {
        /// 技术细节。
        detail: String,
    },
}

impl CwdError {
    /// 映射成面向用户的消息（只使用已有的 `cwd-*` key）。
    pub fn to_message(&self) -> Message {
        match self {
            CwdError::NoShell => Message::CwdNoShell {},
            CwdError::NoSshConnection => Message::CwdNoSshConnection {},
            CwdError::OtherUser => Message::CwdOtherUser {},
            CwdError::NoProc => Message::CwdNoProc {},
            CwdError::Timeout => Message::CwdTimeout {},
            CwdError::Unavailable { .. } => Message::CwdUnavailable {},
        }
    }
}

impl std::fmt::Display for CwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CwdError::NoShell => write!(f, "no shell found for this connection"),
            CwdError::NoSshConnection => write!(f, "SSH_CONNECTION is not set on the server"),
            CwdError::OtherUser => write!(f, "foreground process belongs to another user"),
            CwdError::NoProc => write!(f, "no /proc on the server"),
            CwdError::Timeout => write!(f, "remote cwd query timed out"),
            CwdError::Unavailable { detail } => write!(f, "remote cwd unavailable: {detail}"),
        }
    }
}

impl std::error::Error for CwdError {}

/// 解析探测脚本的 stdout。
///
/// 只认以 [`RESULT_PREFIX`] 开头的行，取**最后一个**（杂讯行不会带这个前缀，但万一重复以最新为准）。
/// 路径不是合法 UTF-8 时按有损方式转换：路径只用于让文件面板跳转，宁可显示近似值也不报错。
pub fn parse_probe_output(out: &[u8]) -> Result<String, CwdError> {
    let text = String::from_utf8_lossy(out);
    let Some(payload) = text
        .lines()
        .rev()
        .find_map(|l| l.trim_end_matches('\r').strip_prefix(RESULT_PREFIX))
    else {
        return Err(CwdError::Unavailable {
            detail: "probe printed no result line".into(),
        });
    };
    if let Some(path) = payload.strip_prefix('=') {
        return if path.is_empty() {
            Err(CwdError::Unavailable {
                detail: "probe returned an empty path".into(),
            })
        } else {
            Ok(path.to_owned())
        };
    }
    match payload.strip_prefix('!') {
        Some("noproc") => Err(CwdError::NoProc),
        Some("nosshconn") => Err(CwdError::NoSshConnection),
        Some("noshell") => Err(CwdError::NoShell),
        Some("otheruser") => Err(CwdError::OtherUser),
        Some(code) => Err(CwdError::Unavailable {
            detail: format!("probe failed: {code}"),
        }),
        None => Err(CwdError::Unavailable {
            detail: "malformed probe result".into(),
        }),
    }
}

/// 在 `conn` 上另开 exec 通道查询 shell 的当前目录。
///
/// 调用约束：只在没有 OSC 7 上报时、用户主动请求时调用（只查一次，不持续跟踪）。
/// 总时长不超过 [`CWD_TIMEOUT`]；超时会尽力关掉远端通道。
pub async fn query_working_directory(conn: &Connection) -> Result<String, CwdError> {
    let started = tokio::time::Instant::now();
    let unavailable = |e: crate::SshError| CwdError::Unavailable {
        detail: e.to_string(),
    };
    // 命令就是 `sh`，脚本走 stdin：避免用户的登录 shell（可能是 csh / fish）解析脚本。
    let channel = tokio::time::timeout(CWD_TIMEOUT, conn.open_exec("sh"))
        .await
        .map_err(|_| CwdError::Timeout)?
        .map_err(unavailable)?;
    let (mut reader, writer) = channel.into_parts();
    let remaining = CWD_TIMEOUT.saturating_sub(started.elapsed());
    let run = async {
        writer.write(PROBE_SCRIPT.as_bytes()).await?;
        writer.eof().await?;
        // 以通道关闭为结束信号：有的服务器 eof 之后才发退出码、最后才 close。
        let mut out = Vec::new();
        while let Some(chunk) = reader.read().await {
            out.extend_from_slice(&chunk);
        }
        Ok::<_, crate::SshError>(out)
    };
    match tokio::time::timeout(remaining, run).await {
        Ok(Ok(out)) => parse_probe_output(&out),
        Ok(Err(e)) => Err(unavailable(e)),
        Err(_) => {
            // 超时后远端的 sh 还可能在跑：关掉通道，别留在连接上。关闭本身也设上限。
            let _ = tokio::time::timeout(Duration::from_secs(1), writer.close()).await;
            Err(CwdError::Timeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_prefixed_lines_count() {
        let out = b"Welcome!\n/home/x\n__KANSO_CWD__=/srv/app\ntrailing noise\n";
        assert_eq!(parse_probe_output(out).unwrap(), "/srv/app");
    }

    #[test]
    fn failure_codes_map_to_errors() {
        let cases = [
            ("noproc", CwdError::NoProc),
            ("nosshconn", CwdError::NoSshConnection),
            ("noshell", CwdError::NoShell),
            ("otheruser", CwdError::OtherUser),
        ];
        for (code, want) in cases {
            let out = format!("__KANSO_CWD__!{code}\n");
            assert_eq!(parse_probe_output(out.as_bytes()), Err(want));
        }
        assert!(matches!(
            parse_probe_output(b"__KANSO_CWD__!whatever\n"),
            Err(CwdError::Unavailable { .. })
        ));
    }

    #[test]
    fn missing_or_malformed_result_is_unavailable() {
        for out in [
            &b""[..],
            b"noise only\n",
            b"__KANSO_CWD__=\n",
            b"__KANSO_CWD__x\n",
        ] {
            assert!(matches!(
                parse_probe_output(out),
                Err(CwdError::Unavailable { .. })
            ));
        }
    }

    #[test]
    fn error_messages_use_existing_keys() {
        assert_eq!(CwdError::NoShell.to_message().key(), "cwd-no-shell");
        assert_eq!(
            CwdError::NoSshConnection.to_message().key(),
            "cwd-no-ssh-connection"
        );
        assert_eq!(CwdError::OtherUser.to_message().key(), "cwd-other-user");
        assert_eq!(CwdError::NoProc.to_message().key(), "cwd-no-proc");
        assert_eq!(CwdError::Timeout.to_message().key(), "cwd-timeout");
        assert_eq!(
            CwdError::Unavailable {
                detail: String::new()
            }
            .to_message()
            .key(),
            "cwd-unavailable"
        );
    }
}
