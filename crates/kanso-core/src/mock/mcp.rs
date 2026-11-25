//! mock 后端的 MCP 部分：纯内存、确定性，供 `kanso-mcp-server` 在没有真实网络时测试
//! （architecture §16.6）。
//!
//! 与真实实现的差异：
//! - 终端文本、命令输出、执行输出、文件内容都是固定格式的假数据；
//! - 命令以 `mock-hang` 开头时立即返回"未确认完成"，模拟超时而不真的等待；
//! - 名为 `mfa-bastion` 的配置新建连接时返回 `NeedsUser(KeyboardInteractive)`，模拟 MFA 主机；
//! - 写文件时路径以 `exists` 结尾视为已存在，`CreateNew` 返回 `SftpFileExists`；
//! - 不模拟用户接管，也不模拟 posix-rename 扩展缺失。

use std::sync::atomic::Ordering;
use std::time::Duration;

use kanso_types::event::{Activity, SessionState};
use kanso_types::mcp::{McpAccess, McpAuditEntry, McpAuditFilter, McpAuditRecord};
use kanso_types::message::Endpoint;
use kanso_types::profile::SessionKind;
use kanso_types::{DirEntry, GridSize, Message, ProfileId, SessionId};

use super::{MockCore, MockSession, MockState, is_closed, remote, terminal};
use crate::api::{CoreError, OpenTarget, Result, SessionInfo};
use crate::mcp::{
    ConnectMode, ExecOutcome, McpConnectOutcome, McpInput, McpSessionStatus, SftpWriteMode,
    TerminalBusy, TerminalRunOutcome, TerminalSendOutcome, TerminalText, TextScope, UserNeed,
    WaitLimits, capture_tail,
};

/// 模拟 MFA 主机的配置名，与 mock 种子数据和 `open_session` 的提问模拟一致。
const MFA_PROFILE: &str = "mfa-bastion";
/// 模拟超时的命令前缀。
const HANG_PREFIX: &str = "mock-hang";
/// `sftp_read` 返回的固定内容。
const MOCK_FILE: &[u8] = b"mock file content\n";

/// [`MockCore::mcp_target`] 的结果。
struct McpTarget {
    name: String,
    /// SSH / SFTP 配置的 (主机, 端口, 用户)；本地配置为 `None`。
    remote: Option<(String, u16, String)>,
    /// 可借用的已连接用户会话（最近打开的那个）。
    live: Option<SessionId>,
}

/// 会话是否在进行终端内传输（ZMODEM / XMODEM）。
fn terminal_transfer(st: &MockState, session: SessionId) -> bool {
    st.transfers
        .values()
        .any(|t| t.terminal && t.session == session)
}

/// 由 mock 会话生成终端文本：一行标题加提示符，光标停在提示符末尾。
fn mock_text(s: &MockSession, max_bytes: usize) -> TerminalText {
    let prompt = format!("{}$ ", s.cwd);
    let text = format!("mock terminal: {}\n{prompt}", s.info.name);
    TerminalText {
        content: capture_tail(&text, max_bytes),
        cursor: Some((1, u16::try_from(prompt.chars().count()).unwrap_or(u16::MAX))),
        rows: s.size.rows,
        cols: s.size.cols,
        alternate_screen: false,
    }
}

impl MockCore {
    // ---- 授权与审计 ----

    pub(crate) fn mcp_enabled(&self) -> bool {
        self.lock().mcp_enabled
    }

    pub(crate) fn set_mcp_enabled(&self, enabled: bool) -> Result<()> {
        self.lock().mcp_enabled = enabled;
        self.mcp_policy.bump();
        Ok(())
    }

    pub(crate) fn mcp_access(&self, profile: ProfileId) -> McpAccess {
        let st = self.lock();
        // 与真实存储一致：配置不存在时一律 Off，不看残留记录。
        if !st.profiles.iter().any(|p| p.id == profile) {
            return McpAccess::Off;
        }
        st.mcp_grants.get(&profile).copied().unwrap_or_default()
    }

    /// mock 的内存状态读不会失败。
    pub(crate) fn try_mcp_access(&self, profile: ProfileId) -> Result<McpAccess> {
        Ok(self.mcp_access(profile))
    }

    pub(crate) fn mcp_accesses(&self) -> Result<Vec<(ProfileId, McpAccess)>> {
        let st = self.lock();
        // mock 的内存状态读不会失败；真实实现的失败路径由 real 侧负责。
        Ok(st
            .mcp_grants
            .iter()
            .filter(|(id, _)| st.profiles.iter().any(|p| p.id == **id))
            .map(|(id, level)| (*id, *level))
            .collect())
    }

    pub(crate) fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> Result<()> {
        let mut st = self.lock();
        if !st.profiles.iter().any(|p| p.id == profile) {
            return Err(CoreError::NoSuchProfile(profile));
        }
        if level == McpAccess::Off {
            st.mcp_grants.remove(&profile);
        } else {
            st.mcp_grants.insert(profile, level);
        }
        self.mcp_policy.bump();
        Ok(())
    }

    pub(crate) fn mcp_audit_append(&self, entries: Vec<McpAuditEntry>) -> Result<()> {
        let mut st = self.lock();
        for entry in entries {
            let id = st.next_audit_id;
            st.next_audit_id += 1;
            st.mcp_audit.push(McpAuditRecord { id, entry });
        }
        Ok(())
    }

    pub(crate) fn mcp_audit(&self, filter: McpAuditFilter) -> Result<Vec<McpAuditRecord>> {
        let limit = filter.effective_limit() as usize;
        let client_filter = filter.client.as_deref().map(str::to_lowercase);
        Ok(self
            .lock()
            .mcp_audit
            .iter()
            .rev()
            .filter(|r| {
                client_filter
                    .as_ref()
                    .is_none_or(|query| r.entry.client.to_lowercase().contains(query))
            })
            .filter(|r| filter.profile.is_none_or(|p| r.entry.profile == Some(p)))
            .filter(|r| !filter.errors_only || r.entry.error_code.is_some())
            .filter(|r| filter.before_id.is_none_or(|b| r.id < b))
            .take(limit)
            .cloned()
            .collect())
    }

    pub(crate) fn mcp_audit_clear(&self) -> Result<()> {
        self.lock().mcp_audit.clear();
        Ok(())
    }

    pub(crate) fn mcp_audit_prune(&self, now_ms: i64) -> Result<u32> {
        let mut st = self.lock();
        let before = st.mcp_audit.len();
        let cutoff = now_ms.saturating_sub(kanso_types::mcp::MCP_AUDIT_RETENTION_MS);
        st.mcp_audit.retain(|r| r.entry.at_ms >= cutoff);
        let max = kanso_types::mcp::MCP_AUDIT_MAX_ROWS as usize;
        if st.mcp_audit.len() > max {
            let excess = st.mcp_audit.len() - max;
            st.mcp_audit.drain(..excess);
        }
        Ok(u32::try_from(before - st.mcp_audit.len()).unwrap_or(u32::MAX))
    }

    // ---- 终端 ----

    pub(crate) fn mcp_session_status(&self, session: SessionId) -> Option<McpSessionStatus> {
        let st = self.lock();
        let s = st.sessions.iter().find(|s| s.info.id == session)?;
        let local = s.info.kind == SessionKind::Local;
        Some(McpSessionStatus {
            info: s.info.clone(),
            activity: Activity::Idle,
            locked: is_closed(s),
            terminal_transfer: terminal_transfer(&st, session),
            // 由 Core 层根据 Sender 状态覆盖。
            sender_active: false,
            run_blocker: None,
            headless: s.headless,
            osc133: local,
            local_tty: local.then(|| format!("/dev/ttys{:03}", session.raw() % 1000)),
            shell_pid: local.then(|| 10_000 + u32::try_from(session.raw() % 10_000).unwrap_or(0)),
        })
    }

    pub(crate) fn terminal_text(
        &self,
        session: SessionId,
        scope: TextScope,
        max_bytes: usize,
    ) -> Result<TerminalText> {
        let mut st = self.lock();
        let s = terminal(&mut st, session)?;
        // mock 只有两行，Screen 与 Recent 返回相同内容。
        let _ = scope;
        Ok(mock_text(s, max_bytes))
    }

    pub(crate) async fn mcp_terminal_run(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<TerminalRunOutcome> {
        if command.contains(['\n', '\r']) {
            return Err(CoreError::NotSupported);
        }
        let mut st = self.lock();
        let transferring = terminal_transfer(&st, session);
        let s = terminal(&mut st, session)?;
        if s.info.state != SessionState::Connected {
            return Ok(TerminalRunOutcome::Rejected(TerminalBusy::NotConnected));
        }
        if transferring {
            return Ok(TerminalRunOutcome::Rejected(TerminalBusy::Transferring));
        }
        s.seq += 1;
        s.dirty = true;
        let local = s.info.kind == SessionKind::Local;
        if command.starts_with(HANG_PREFIX) {
            return Ok(TerminalRunOutcome::Unconfirmed {
                output: capture_tail("", limits.max_bytes),
                osc133: local,
            });
        }
        Ok(TerminalRunOutcome::Completed {
            output: capture_tail(&format!("mock output: {command}\n"), limits.max_bytes),
            // 只有带 OSC 133 的会话（mock 里是本地会话）才有退出码。
            exit_code: local.then_some(0),
        })
    }

    pub(crate) async fn mcp_terminal_send(
        &self,
        session: SessionId,
        inputs: Vec<McpInput>,
        settle: Option<Duration>,
        max_bytes: usize,
    ) -> Result<TerminalSendOutcome> {
        let mut st = self.lock();
        let transferring = terminal_transfer(&st, session);
        let s = terminal(&mut st, session)?;
        if s.info.state != SessionState::Connected {
            return Ok(TerminalSendOutcome::Rejected(TerminalBusy::NotConnected));
        }
        if transferring {
            return Ok(TerminalSendOutcome::Rejected(TerminalBusy::Transferring));
        }
        let _ = inputs;
        s.seq += 1;
        s.dirty = true;
        Ok(TerminalSendOutcome::Sent {
            screen: settle.map(|_| mock_text(s, max_bytes)),
        })
    }

    // ---- 连接 ----

    /// 在**同一把锁内**解析配置并找出可借用的用户会话，返回解析结果；
    /// 调用方据此决定是否新建，避免两次加锁之间配置被删除。
    fn mcp_target(st: &MockState, profile: ProfileId) -> Result<McpTarget> {
        let p = st
            .profiles
            .iter()
            .find(|p| p.id == profile)
            .ok_or(CoreError::NoSuchProfile(profile))?;
        let live = st
            .sessions
            .iter()
            .rev()
            .find(|s| {
                !s.headless
                    && s.info.profile == Some(profile)
                    && s.info.state == SessionState::Connected
            })
            .map(|s| s.info.id);
        Ok(McpTarget {
            name: p.name.clone(),
            remote: super::ssh_target(p).map(|t| (t.host.clone(), t.port, t.user.clone())),
            live,
        })
    }

    /// 没有可借用的连接时，按模式决定能否新建；能新建返回 `None`。
    fn refuse_new(target: &McpTarget, mode: ConnectMode) -> Option<McpConnectOutcome> {
        match mode {
            ConnectMode::ReuseOnly => Some(McpConnectOutcome::NotConnected),
            ConnectMode::NonInteractive if target.name == MFA_PROFILE => {
                Some(McpConnectOutcome::NeedsUser(UserNeed::KeyboardInteractive))
            }
            ConnectMode::NonInteractive => None,
        }
    }

    pub(crate) async fn mcp_connect(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
    ) -> Result<McpConnectOutcome> {
        let mut st = self.lock();
        let target = Self::mcp_target(&st, profile)?;
        let Some((host, port, user)) = target.remote.clone() else {
            return Err(CoreError::NotSupported);
        };
        let borrowed = target.live.is_some();
        if !borrowed && let Some(refused) = Self::refuse_new(&target, mode) {
            return Ok(refused);
        }
        let id = SessionId::from_raw(self.next_session.fetch_add(1, Ordering::SeqCst));
        // 无界面会话不发任何会话事件，直接处于已连接状态。
        st.sessions.push(MockSession {
            info: SessionInfo {
                id,
                profile: Some(profile),
                kind: SessionKind::Sftp,
                name: target.name,
                custom_title: None,
                state: SessionState::Connected,
            },
            endpoint: Some(Endpoint {
                host,
                port,
                via: Vec::new(),
            }),
            cwd: format!("/home/{user}"),
            size: GridSize { cols: 80, rows: 24 },
            seq: 0,
            dirty: false,
            selected: false,
            search_current: 1,
            headless: true,
            borrowed,
        });
        Ok(McpConnectOutcome::Connected {
            session: id,
            borrowed,
        })
    }

    pub(crate) async fn mcp_open_session(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
        size: GridSize,
    ) -> Result<McpConnectOutcome> {
        // open_session 自己加锁，这里先在锁内算好目标再放锁；两步之间配置被删除时，
        // open_session 会返回 NoSuchProfile / NoSuchSession，结果仍然正确。
        let (open, borrowed) = {
            let st = self.lock();
            let target = Self::mcp_target(&st, profile)?;
            match (&target.remote, target.live) {
                (None, _) => (OpenTarget::Profile(profile), false),
                // 有已连接的标签：复制它，不再认证。
                (Some(_), Some(src)) => (OpenTarget::Duplicate(src), true),
                (Some(_), None) => match Self::refuse_new(&target, mode) {
                    Some(refused) => return Ok(refused),
                    None => (OpenTarget::Profile(profile), false),
                },
            }
        };
        let session = self.open_session(open, size)?;
        Ok(McpConnectOutcome::Connected { session, borrowed })
    }

    pub(crate) async fn mcp_exec(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<ExecOutcome> {
        let st = self.lock();
        let s = remote(&st, session)?;
        if is_closed(s) {
            return Err(CoreError::User(Message::SshConnectionLost {
                endpoint: s.endpoint.clone().unwrap_or_else(|| Endpoint {
                    host: String::new(),
                    port: 0,
                    via: Vec::new(),
                }),
            }));
        }
        Ok(ExecOutcome {
            stdout: capture_tail(&format!("mock exec: {command}\n"), limits.max_bytes),
            stderr: capture_tail("", limits.max_bytes),
            exit_status: Some(0),
            timed_out: false,
        })
    }

    // ---- SFTP 补充 ----

    pub(crate) async fn sftp_entry(
        &self,
        session: SessionId,
        path: &str,
        follow_symlinks: bool,
    ) -> Result<DirEntry> {
        let st = self.lock();
        remote(&st, session)?;
        let _ = follow_symlinks;
        let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        Ok(DirEntry {
            name: name.to_owned(),
            path: path.to_owned(),
            is_dir: path.ends_with('/'),
            is_symlink: false,
            size: MOCK_FILE.len() as u64,
            mtime_ms: None,
            permissions: Some(0o644),
            owner: None,
            group: None,
        })
    }

    pub(crate) async fn sftp_read(
        &self,
        session: SessionId,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>> {
        let st = self.lock();
        remote(&st, session)?;
        let _ = path;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(MOCK_FILE.len());
        let end = start.saturating_add(len as usize).min(MOCK_FILE.len());
        Ok(MOCK_FILE[start..end].to_vec())
    }

    pub(crate) async fn sftp_write(
        &self,
        session: SessionId,
        path: &str,
        data: Vec<u8>,
        mode: SftpWriteMode,
    ) -> Result<()> {
        let st = self.lock();
        remote(&st, session)?;
        let _ = data;
        if mode == SftpWriteMode::CreateNew && path.ends_with("exists") {
            return Err(CoreError::User(Message::SftpFileExists {
                path: path.to_owned(),
            }));
        }
        Ok(())
    }

    pub(crate) async fn sftp_rmdir(&self, session: SessionId, path: &str) -> Result<()> {
        let st = self.lock();
        remote(&st, session)?;
        let _ = path;
        Ok(())
    }
}
