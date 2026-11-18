//! 真实后端的 MCP 部分。
//!
//! 已实现：授权、总开关与审计日志的存取（schema v3）。
//! 尚未实现（**F9**）：终端读取与执行、无界面会话与连接复用、后台执行、SFTP 补充操作。
//! 按 `real` 模块的约定不 panic：返回 `NotSupported`，`mcp_session_status` 返回 `None`。

use std::time::Duration;

use kanso_types::mcp::{McpAccess, McpAuditEntry, McpAuditFilter, McpAuditRecord};
use kanso_types::{DirEntry, GridSize, ProfileId, SessionId};

use super::{RealCore, store_err};
use crate::api::{CoreError, Result};
use crate::mcp::{
    ConnectMode, ExecOutcome, McpConnectOutcome, McpInput, McpSessionStatus, SftpWriteMode,
    TerminalRunOutcome, TerminalSendOutcome, TerminalText, TextScope, WaitLimits,
};

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

    pub(crate) fn mcp_accesses(&self) -> Vec<(ProfileId, McpAccess)> {
        self.db().mcp_accesses().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read mcp accesses");
            Vec::new()
        })
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

    // ---- 以下由 F9 实现 ----

    pub(crate) fn mcp_session_status(&self, session: SessionId) -> Option<McpSessionStatus> {
        None
    }

    pub(crate) fn terminal_text(
        &self,
        session: SessionId,
        scope: TextScope,
        max_bytes: usize,
    ) -> Result<TerminalText> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn mcp_terminal_run(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<TerminalRunOutcome> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn mcp_terminal_send(
        &self,
        session: SessionId,
        inputs: Vec<McpInput>,
        settle: Option<Duration>,
        max_bytes: usize,
    ) -> Result<TerminalSendOutcome> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn mcp_connect(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
    ) -> Result<McpConnectOutcome> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn mcp_open_session(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
        size: GridSize,
    ) -> Result<McpConnectOutcome> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn mcp_exec(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<ExecOutcome> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn sftp_entry(
        &self,
        session: SessionId,
        path: &str,
        follow_symlinks: bool,
    ) -> Result<DirEntry> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn sftp_read(
        &self,
        session: SessionId,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn sftp_write(
        &self,
        session: SessionId,
        path: &str,
        data: Vec<u8>,
        mode: SftpWriteMode,
    ) -> Result<()> {
        Err(CoreError::NotSupported)
    }

    pub(crate) async fn sftp_rmdir(&self, session: SessionId, path: &str) -> Result<()> {
        Err(CoreError::NotSupported)
    }
}
