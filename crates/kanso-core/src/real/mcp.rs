//! 真实后端的 MCP 部分。
//!
//! 已实现：授权、总开关与审计日志的存取（schema v3）；终端的状态、读取、命令执行与输入
//! （`mcp_session_status`、`terminal_text`、`mcp_terminal_run`、`mcp_terminal_send`，本地与 SSH 会话共用，
//! 逻辑在 `kanso-session` 的 `mcp` 模块，这里只做契约类型的映射）。
//! 无界面会话、连接复用与后台执行在 `connect.rs` / `exec.rs`。
//! SFTP 补充操作（`sftp_entry` / `sftp_read` / `sftp_write` / `sftp_rmdir`）复用 `real` 的 SFTP 客户端与错误映射。
//! 按 `real` 模块的约定不 panic：返回 `NotSupported`。

use std::time::Duration;

use kanso_session::{McpBusy, McpOutput, McpRun, McpScreen, McpSend, McpWrite, ReadScope};
use kanso_ssh::sftp::{OverwriteError, WriteFailure};
use kanso_types::event::Activity;
use kanso_types::mcp::{McpAccess, McpAuditEntry, McpAuditFilter, McpAuditRecord};
use kanso_types::message::{Message, Reason, SftpOp};
use kanso_types::profile::SessionKind;
use kanso_types::{DirEntry, GridSize, ProfileId, SessionId};

use super::{RealCore, sftp_err, store_err};
use crate::api::{CoreError, Result};
use crate::mcp::{
    CapturedText, ConnectMode, ExecOutcome, McpConnectOutcome, McpInput, McpSessionStatus,
    SftpWriteMode, TerminalBusy, TerminalRunOutcome, TerminalSendOutcome, TerminalText, TextScope,
    WaitLimits, capture_tail,
};

fn busy(b: McpBusy) -> TerminalBusy {
    match b {
        McpBusy::NotConnected => TerminalBusy::NotConnected,
        McpBusy::CommandRunning => TerminalBusy::CommandRunning,
        McpBusy::AgentRunning => TerminalBusy::AgentRunning,
        McpBusy::InputNotEmpty => TerminalBusy::InputNotEmpty,
        McpBusy::Transferring => TerminalBusy::Transferring,
    }
}

fn captured(output: McpOutput, max_bytes: usize) -> CapturedText {
    CapturedText {
        cleared: output.cleared,
        ..capture_tail(&output.text, max_bytes)
    }
}

fn terminal_text_of(screen: McpScreen, max_bytes: usize) -> TerminalText {
    TerminalText {
        content: capture_tail(&screen.text, max_bytes),
        cursor: screen.cursor,
        rows: screen.rows,
        cols: screen.cols,
        alternate_screen: screen.alternate_screen,
    }
}

impl RealCore {
    pub(crate) fn mcp_enabled(&self) -> bool {
        self.db().mcp_enabled().unwrap_or_else(|e| {
            // 读失败按关闭处理：授权判断宁可保守。
            tracing::warn!(error = %e, "failed to read mcp switch");
            false
        })
    }

    pub(crate) fn set_mcp_enabled(&self, enabled: bool) -> Result<()> {
        self.db().set_mcp_enabled(enabled).map_err(store_err)?;
        self.mcp_policy.bump();
        Ok(())
    }

    pub(crate) fn mcp_access(&self, profile: ProfileId) -> McpAccess {
        self.db().mcp_access(profile).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read mcp access");
            McpAccess::Off
        })
    }

    pub(crate) fn try_mcp_access(&self, profile: ProfileId) -> Result<McpAccess> {
        self.db().mcp_access(profile).map_err(store_err)
    }

    pub(crate) fn mcp_accesses(&self) -> Result<Vec<(ProfileId, McpAccess)>> {
        self.db().mcp_accesses().map_err(store_err)
    }

    pub(crate) fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> Result<()> {
        if self
            .db()
            .set_mcp_access(profile, level)
            .map_err(store_err)?
        {
            self.mcp_policy.bump();
            Ok(())
        } else {
            Err(CoreError::NoSuchProfile(profile))
        }
    }

    pub(crate) fn mcp_audit_append(&self, entries: Vec<McpAuditEntry>) -> Result<()> {
        self.db().append_mcp_audit(&entries).map_err(store_err)
    }

    pub(crate) fn mcp_audit(&self, filter: McpAuditFilter) -> Result<Vec<McpAuditRecord>> {
        self.db().mcp_audit(&filter).map_err(store_err)
    }

    pub(crate) fn mcp_audit_clear(&self) -> Result<()> {
        self.db().clear_mcp_audit().map_err(store_err)
    }

    pub(crate) fn mcp_audit_prune(&self, now_ms: i64) -> Result<u32> {
        self.db().prune_mcp_audit(now_ms).map_err(store_err)
    }

    // ---- 终端：状态、读取、命令与输入（本地与 SSH 会话共用）----

    pub(crate) fn mcp_session_status(&self, session: SessionId) -> Option<McpSessionStatus> {
        let s = self.sessions.session(session)?;
        let info = self.session_info(session)?;
        // 纯 SFTP 会话没有终端，也就没有命令跟踪与终端传输。
        let (activity, osc133, terminal_transfer, run_blocker) = if s.kind() == SessionKind::Sftp {
            (Activity::Idle, false, false, None)
        } else {
            let t = s.mcp_track_state();
            (t.activity, t.osc133, t.transfer, t.run_blocker.map(busy))
        };
        Some(McpSessionStatus {
            info,
            activity,
            locked: s.is_locked(),
            terminal_transfer,
            run_blocker,
            // 由 Core 层根据 Sender 状态覆盖。
            sender_active: false,
            headless: s.is_headless(),
            osc133,
            local_tty: s.tty_path(),
            shell_pid: s.shell_pid(),
        })
    }

    pub(crate) fn terminal_text(
        &self,
        session: SessionId,
        scope: TextScope,
        max_bytes: usize,
    ) -> Result<TerminalText> {
        let scope = match scope {
            TextScope::Screen => ReadScope::Screen,
            TextScope::Viewport => ReadScope::Viewport,
            TextScope::Recent { lines } => ReadScope::Recent(lines as usize),
        };
        let screen = self.terminal_session(session)?.mcp_read(scope);
        Ok(terminal_text_of(screen, max_bytes))
    }

    pub(crate) async fn mcp_terminal_run(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<TerminalRunOutcome> {
        // 先查会话再看命令：不存在的会话应当报 NoSuchSession，而不是被命令格式盖住。
        let s = self.terminal_session(session)?;
        // 空命令只会发出一个回车，shell 不会把它当命令，登记的命令永远等不到"返回"；
        // 与含换行一样属于调用方该拒绝的输入。
        if command.contains(['\n', '\r']) || command.trim().is_empty() {
            return Err(CoreError::NotSupported);
        }
        let max = limits.max_bytes;
        Ok(match s.mcp_run(command, limits.timeout).await {
            McpRun::Rejected(b) => TerminalRunOutcome::Rejected(busy(b)),
            McpRun::Completed { output, exit_code } => TerminalRunOutcome::Completed {
                output: captured(output, max),
                exit_code,
            },
            McpRun::Unconfirmed { output, osc133 } => TerminalRunOutcome::Unconfirmed {
                output: captured(output, max),
                osc133,
            },
            McpRun::TakenOver { output } => TerminalRunOutcome::TakenOver {
                output: captured(output, max),
            },
            // 契约里没有"中断"结果；服务侧对已开始的写调用把 NoSuchSession 统一报为 INTERRUPTED。
            McpRun::Ended => return Err(CoreError::NoSuchSession(session)),
        })
    }

    pub(crate) async fn mcp_terminal_send(
        &self,
        session: SessionId,
        inputs: Vec<McpInput>,
        settle: Option<Duration>,
        max_bytes: usize,
    ) -> Result<TerminalSendOutcome> {
        let s = self.terminal_session(session)?;
        let inputs = inputs
            .into_iter()
            .map(|input| match input {
                McpInput::Text(text) => McpWrite::Text(text),
                McpInput::Key(event) => McpWrite::Key(event),
            })
            .collect();
        Ok(match s.mcp_send(inputs, settle).await {
            McpSend::Rejected(b) => TerminalSendOutcome::Rejected(busy(b)),
            McpSend::Sent { screen } => TerminalSendOutcome::Sent {
                screen: screen.map(|screen| terminal_text_of(screen, max_bytes)),
            },
            McpSend::TakenOver { screen } => TerminalSendOutcome::TakenOver {
                screen: terminal_text_of(screen, max_bytes),
            },
            // 同 run：服务侧把写调用期间的 NoSuchSession 报为 INTERRUPTED。
            McpSend::Ended => return Err(CoreError::NoSuchSession(session)),
        })
    }

    // ---- 以下由 F9 实现 ----

    pub(crate) async fn mcp_connect(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
    ) -> Result<McpConnectOutcome> {
        self.mcp_connect_impl(profile, mode).await
    }

    pub(crate) async fn mcp_open_session(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
        size: GridSize,
    ) -> Result<McpConnectOutcome> {
        self.mcp_open_session_impl(profile, mode, size).await
    }

    pub(crate) async fn mcp_exec(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<ExecOutcome> {
        self.mcp_exec_impl(session, command, limits).await
    }

    pub(crate) async fn sftp_entry(
        &self,
        session: SessionId,
        path: &str,
        follow_symlinks: bool,
    ) -> Result<DirEntry> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move { client.entry(&path, follow_symlinks).await })
            .await?
            .map_err(sftp_err)
    }

    pub(crate) async fn sftp_read(
        &self,
        session: SessionId,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move { client.read_range(&path, offset, len).await })
            .await?
            .map_err(sftp_err)
    }

    pub(crate) async fn sftp_write(
        &self,
        session: SessionId,
        path: &str,
        data: Vec<u8>,
        mode: SftpWriteMode,
    ) -> Result<()> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move {
            match mode {
                SftpWriteMode::CreateNew => client
                    .write_new(&path, &data)
                    .await
                    .map_err(|f| write_failure(&path, mode, f)),
                SftpWriteMode::Overwrite => {
                    client
                        .overwrite_file(&path, &data)
                        .await
                        .map_err(|e| match e {
                            OverwriteError::Unsupported => CoreError::NotSupported,
                            OverwriteError::Failed(f) => write_failure(&path, mode, f),
                        })
                }
            }
        })
        .await?
    }

    pub(crate) async fn sftp_rmdir(&self, session: SessionId, path: &str) -> Result<()> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move { client.remove_dir(&path).await })
            .await?
            .map_err(sftp_err)
    }
}

/// 写失败 → 用户可见错误。有残留文件时，把路径写进 `SftpOpFailed` 的 detail
/// （spec/mcp.md §3.5：调用方要能据此手动清理，或知道目标只写了一半）；没有残留就沿用通用的错误映射。
///
/// 措辞按模式区分：`Overwrite` 残留的是临时文件，目标本身未被改动；
/// `CreateNew` 残留的就是目标路径，内容只写了一半。
fn write_failure(path: &str, mode: SftpWriteMode, f: WriteFailure) -> CoreError {
    let Some(left) = f.leftover else {
        return sftp_err(f.error);
    };
    let detail = match mode {
        SftpWriteMode::Overwrite => format!("{}; leftover temp file: {left}", f.error),
        SftpWriteMode::CreateNew => {
            format!("{}; partially written file left at {left}", f.error)
        }
    };
    CoreError::User(Message::SftpOpFailed {
        op: SftpOp::Open,
        path: path.to_owned(),
        reason: Reason::Other { detail },
    })
}
