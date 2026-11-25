//! mock `Core` 后端：纯内存、确定性数据，不碰磁盘与网络。
//!
//! 用途是让 Swift UI 在内核完成前开发、截图测试（P0.4）。行为只模拟契约，不模拟真实内核的
//! 全部细节；与真实实现不同的地方在各方法注释里写明。
//!
//! 事件在持有状态锁时发出：这样同一会话的事件严格保序。这样做安全的前提是
//! [`EventSink::on_event`] 的契约（立即返回、不得同步回调 core），不满足该契约的接收端会死锁。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use kanso_frame::Frame;
use kanso_frame::mock::mock_frame;
use kanso_license::{Entitlement, EntitlementSource, PurchaseOutcome};
use kanso_secret::{SecretError, SecretKind, SecretRef, SecretStore};
use kanso_types::event::{
    Activity, CloseReason, LegacyHop, SessionState, TransferDirection, TransferOutcome,
};
use kanso_types::message::Endpoint;
use kanso_types::profile::{
    AuthMethod, DEFAULT_SSH_PORT, EncodingLabel, Group, LineEnding, LocalProfile, Profile,
    ProfileKind, SessionKind, Snippet, SnippetScope, SshProfile, SshTarget,
};
use kanso_types::prompt::{KbdQuestion, PromptRequest};
use kanso_types::secret::SecretString;
use kanso_types::transfer::{ConflictPolicy, XferProtocol};
use kanso_types::{
    CoreEvent, DirEntry, GridSize, GroupId, KeyEvent, Message, Modifiers, MouseEvent, PasteContent,
    ProfileId, PromptId, PromptResponse, SessionId, SnippetId, TerminalOptions, TimestampMode,
    TransferId, ViewportPoint,
};

use crate::api::{
    CoreError, DeleteImpact, DirStream, ImportReport, OpenTarget, Result, SearchQuery,
    SearchStatus, SessionInfo, SshConfigCandidate, SshConfigImportReport, SshConfigScan,
    XmodemBlock,
};
use crate::platform::{EventSink, Platform};
use crate::validate::validate_profile;

mod mcp;

/// mock 传输的总字节数（10 MB）。
const MOCK_TRANSFER_BYTES: u64 = 10 * 1024 * 1024;
/// mock 传输分成多少步。
const MOCK_TRANSFER_STEPS: u64 = 20;
/// mock 传输每步间隔。
const MOCK_TRANSFER_TICK: Duration = Duration::from_millis(50);
/// mock 搜索的固定匹配总数。
const MOCK_SEARCH_TOTAL: u32 = 3;

/// 一个 mock 会话。
struct MockSession {
    info: SessionInfo,
    /// SSH / SFTP 会话的连接对象；本地会话为 `None`。
    endpoint: Option<Endpoint>,
    /// 当前工作目录。
    cwd: String,
    size: GridSize,
    /// 帧序号，每次输入加 1。
    seq: u64,
    /// 终端自上次取帧后是否变脏。
    dirty: bool,
    selected: bool,
    /// 搜索当前匹配项（1..=总数）。
    search_current: u32,
    /// MCP 的无界面会话：不出现在 `sessions()` 里，也不发会话事件（见 `crate::mcp` 模块文档）。
    headless: bool,
    /// 无界面会话是否借用了用户的连接；借用的在该配置最后一个用户标签关闭时一并关闭。
    borrowed: bool,
}

/// 一个进行中的 mock 传输。
struct TransferEntry {
    session: SessionId,
    /// 是否终端内传输（XMODEM），`cancel_terminal_transfer` 只取消这类。
    terminal: bool,
    cancel: Arc<AtomicBool>,
}

struct MockState {
    profiles: Vec<Profile>,
    groups: Vec<Group>,
    snippets: Vec<Snippet>,
    sessions: Vec<MockSession>,
    /// 等待应答的提问 -> 所属会话。
    prompts: HashMap<PromptId, SessionId>,
    transfers: HashMap<TransferId, TransferEntry>,
    /// 设置表（键到 JSON 文本），语义同 kanso-store 的 `settings` 表。
    settings: HashMap<String, String>,
    /// 最近一次下发的终端显示选项；mock 没有真实终端，只记录以便测试断言。
    terminal_options: TerminalOptions,
    entitlement: Option<Entitlement>,
    /// MCP 总开关，语义同 kanso-store 的 `mcp_local`。
    mcp_enabled: bool,
    /// MCP 授权；没有记录即 `Off`，语义同 `mcp_grants`。
    mcp_grants: HashMap<ProfileId, kanso_types::mcp::McpAccess>,
    /// MCP 审计日志，按追加顺序。
    mcp_audit: Vec<kanso_types::mcp::McpAuditRecord>,
    /// 下一条审计记录的 id。单独计数而不是取"最后一条 + 1"：与 SQLite AUTOINCREMENT 一样，
    /// 清空或清理后 id 也不复用，否则旧的分页游标会把新记录当成更早的记录。
    next_audit_id: i64,
}

/// mock 后端。`Core::new_mock` 创建。
pub(crate) struct MockCore {
    events: Arc<dyn EventSink>,
    secrets: Arc<dyn SecretStore>,
    // 状态装箱，避免 `Backend` 枚举两个变体大小悬殊。
    state: Mutex<Box<MockState>>,
    next_session: AtomicU64,
    next_transfer: AtomicU64,
    next_prompt: AtomicU64,
    /// MCP 授权变化通知器，与 `Core` 共用。
    mcp_policy: crate::mcp::PolicyNotifier,
}

fn secret_err(e: SecretError) -> CoreError {
    CoreError::User(Message::SecretStoreFailed {
        detail: e.to_string(),
    })
}

fn ssh_target(p: &Profile) -> Option<&SshTarget> {
    match &p.kind {
        ProfileKind::Ssh(s) => Some(&s.target),
        ProfileKind::Sftp(t) => Some(t),
        ProfileKind::Local(_) => None,
    }
}

fn seed_target(host: &str, user: &str, jump: Option<ProfileId>) -> SshTarget {
    SshTarget {
        host: host.into(),
        port: DEFAULT_SSH_PORT,
        user: user.into(),
        auth: AuthMethod::Password,
        private_key_path: None,
        jump,
        proxy_command: None,
        keepalive: kanso_types::profile::KeepaliveConfig::default(),
    }
}

fn seed_ssh(name: &str, host: &str, user: &str, group: Option<GroupId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group,
        kind: ProfileKind::Ssh(SshProfile {
            target: seed_target(host, user, None),
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

fn seed_state() -> MockState {
    let prod = Group {
        id: GroupId::new(),
        name: "生产".into(),
    };
    let local = Profile {
        id: ProfileId::new(),
        name: "Local".into(),
        color: None,
        group: None,
        kind: ProfileKind::Local(LocalProfile {
            shell_command: String::new(),
            start_dir: None,
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    };
    let web = seed_ssh("web-01", "web-01.example.com", "deploy", Some(prod.id));
    let switch = seed_ssh("legacy-switch", "192.0.2.10", "admin", None);
    let bastion = seed_ssh("mfa-bastion", "bastion.example.com", "alice", None);
    let files = Profile {
        id: ProfileId::new(),
        name: "files".into(),
        color: None,
        group: None,
        kind: ProfileKind::Sftp(seed_target(
            "files.internal.example.com",
            "ops",
            Some(web.id),
        )),
    };
    let snippets = vec![
        Snippet {
            id: SnippetId::new(),
            name: "磁盘用量".into(),
            text: "df -h".into(),
            line_ending: LineEnding::Lf,
            scope: SnippetScope::Global,
        },
        Snippet {
            id: SnippetId::new(),
            name: "重启 nginx".into(),
            text: "sudo systemctl restart nginx".into(),
            line_ending: LineEnding::Lf,
            scope: SnippetScope::Profile(web.id),
        },
    ];
    MockState {
        profiles: vec![local, web, switch, bastion, files],
        groups: vec![prod],
        snippets,
        sessions: Vec::new(),
        prompts: HashMap::new(),
        transfers: HashMap::new(),
        settings: HashMap::new(),
        terminal_options: TerminalOptions::default(),
        entitlement: Entitlement::for_channel(),
        mcp_enabled: false,
        mcp_grants: HashMap::new(),
        mcp_audit: Vec::new(),
        next_audit_id: 1,
    }
}

/// 删除分组时会被连带删除的 Profile 与 Sender 命令（store-import.md §3）。
/// `group_delete_impact` 与 `delete_group` 共用，保证确认框里的数字与实际删除一致。
fn group_cascade(st: &MockState, gid: GroupId) -> (Vec<ProfileId>, Vec<SnippetId>) {
    let profiles: Vec<ProfileId> = st
        .profiles
        .iter()
        .filter(|p| p.group == Some(gid))
        .map(|p| p.id)
        .collect();
    let snippets = st
        .snippets
        .iter()
        .filter(|s| match &s.scope {
            SnippetScope::Group(g) => *g == gid,
            SnippetScope::Profile(p) => profiles.contains(p),
            _ => false,
        })
        .map(|s| s.id)
        .collect();
    (profiles, snippets)
}

/// 取终端类会话：不存在返回 `NoSuchSession`，纯 SFTP 返回 `NotSupported`。
fn terminal(st: &mut MockState, id: SessionId) -> Result<&mut MockSession> {
    let s = st
        .sessions
        .iter_mut()
        .find(|s| s.info.id == id)
        .ok_or(CoreError::NoSuchSession(id))?;
    if s.info.kind == SessionKind::Sftp {
        return Err(CoreError::NotSupported);
    }
    Ok(s)
}

/// 取 SSH / SFTP 会话：本地会话返回 `NotSupported`。
fn remote(st: &MockState, id: SessionId) -> Result<&MockSession> {
    let s = st
        .sessions
        .iter()
        .find(|s| s.info.id == id)
        .ok_or(CoreError::NoSuchSession(id))?;
    if s.info.kind == SessionKind::Local {
        return Err(CoreError::NotSupported);
    }
    Ok(s)
}

fn is_closed(s: &MockSession) -> bool {
    matches!(s.info.state, SessionState::Closed(_))
}

/// 后台线程：模拟一次传输。每步先睡眠再检查取消，所以取消最多延迟一步。
fn run_mock_transfer(
    events: Arc<dyn EventSink>,
    id: TransferId,
    cancel: Arc<AtomicBool>,
    waiter: Option<tokio::sync::oneshot::Sender<TransferOutcome>>,
) {
    // 结果同时交给等待者；等待者已放弃（future 被丢弃）时忽略。与真实后端一致：先发事件再交结果，
    // 等待者被唤醒时 `TransferFinished` 一定已经发出。
    let finish = |outcome: TransferOutcome| {
        events.on_event(CoreEvent::TransferFinished {
            transfer: id,
            outcome: outcome.clone(),
        });
        if let Some(tx) = waiter {
            let _ = tx.send(outcome);
        }
    };
    let step_bytes = MOCK_TRANSFER_BYTES / MOCK_TRANSFER_STEPS;
    for step in 1..=MOCK_TRANSFER_STEPS {
        thread::sleep(MOCK_TRANSFER_TICK);
        if cancel.load(Ordering::SeqCst) {
            finish(TransferOutcome::Cancelled);
            return;
        }
        events.on_event(CoreEvent::TransferProgress {
            transfer: id,
            done_bytes: step * step_bytes,
            total_bytes: Some(MOCK_TRANSFER_BYTES),
        });
    }
    finish(TransferOutcome::Completed { skipped: 0 });
}

fn file_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_owned()
}

impl MockCore {
    pub(crate) fn new(platform: Platform, mcp_policy: crate::mcp::PolicyNotifier) -> MockCore {
        MockCore {
            events: platform.events,
            secrets: platform.secrets,
            state: Mutex::new(Box::new(seed_state())),
            next_session: AtomicU64::new(1),
            next_transfer: AtomicU64::new(1),
            next_prompt: AtomicU64::new(1),
            mcp_policy,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Box<MockState>> {
        // mock 状态没有跨字段的不变式需要在 panic 后保护，中毒后继续使用。
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, event: CoreEvent) {
        self.events.on_event(event);
    }

    /// 置脏。只有从干净变脏时才发 `TerminalDirty`（唤醒合并，architecture §4）。
    fn mark_dirty(&self, s: &mut MockSession, bump_seq: bool) {
        if bump_seq {
            s.seq += 1;
        }
        if !s.dirty {
            s.dirty = true;
            self.emit(CoreEvent::TerminalDirty { session: s.info.id });
        }
    }

    /// 输入类方法的共同逻辑：会话已结束则丢弃并返回 `Ok`。
    fn input(&self, session: SessionId) -> Result<()> {
        let mut st = self.lock();
        let s = terminal(&mut st, session)?;
        if !is_closed(s) {
            self.mark_dirty(s, true);
        }
        Ok(())
    }

    /// 会话连接成功后的事件序列。
    fn finish_connect(&self, s: &mut MockSession) {
        let session = s.info.id;
        s.info.state = SessionState::Connected;
        self.emit(CoreEvent::SessionStateChanged {
            session,
            state: SessionState::Connected,
        });
        if s.info.name == "legacy-switch"
            && let Some(endpoint) = s.endpoint.clone()
        {
            self.emit(CoreEvent::LegacyAlgorithms {
                session,
                hops: vec![LegacyHop {
                    endpoint,
                    algorithms: vec![
                        "diffie-hellman-group14-sha1".into(),
                        "aes128-cbc".into(),
                        "hmac-sha1".into(),
                    ],
                }],
            });
        }
        let is_local = s.info.kind == SessionKind::Local;
        self.emit(CoreEvent::LocationChanged {
            session,
            cwd: Some(s.cwd.clone()),
            cwd_host: s.endpoint.as_ref().map(|e| e.host.clone()),
            git_branch: is_local.then(|| "main".to_owned()),
        });
        // 纯 SFTP 会话没有终端，不产生活动与帧。
        if s.info.kind != SessionKind::Sftp {
            self.emit(CoreEvent::ActivityChanged {
                session,
                activity: Activity::Idle,
            });
            self.mark_dirty(s, false);
        }
    }

    /// 启动一次 mock 传输：先发 `TransferStarted`，再起线程推进度。`waiter` 非空时结果同时交给它。
    // 参数都是独立的传输属性，打包成结构体只是把同样的字段换个地方写。
    #[allow(clippy::too_many_arguments)]
    fn start_transfer(
        &self,
        st: &mut MockState,
        session: SessionId,
        protocol: XferProtocol,
        direction: TransferDirection,
        path: &str,
        terminal: bool,
        waiter: Option<tokio::sync::oneshot::Sender<TransferOutcome>>,
    ) -> Result<TransferId> {
        let id = TransferId::from_raw(self.next_transfer.fetch_add(1, Ordering::SeqCst));
        let cancel = Arc::new(AtomicBool::new(false));
        st.transfers.insert(
            id,
            TransferEntry {
                session,
                terminal,
                cancel: cancel.clone(),
            },
        );
        self.emit(CoreEvent::TransferStarted {
            transfer: id,
            session,
            protocol,
            direction,
            name: file_name(path),
            total_bytes: Some(MOCK_TRANSFER_BYTES),
        });
        let events = self.events.clone();
        let spawned = thread::Builder::new()
            .name("mock-transfer".into())
            .spawn(move || run_mock_transfer(events, id, cancel, waiter));
        if let Err(e) = spawned {
            st.transfers.remove(&id);
            let detail = format!("failed to spawn mock transfer thread: {e}");
            self.emit(CoreEvent::TransferFinished {
                transfer: id,
                outcome: TransferOutcome::Failed {
                    message: Message::XferProtocolError {
                        protocol,
                        detail: detail.clone(),
                    },
                    already_exists: false,
                },
            });
            return Err(CoreError::Internal { detail });
        }
        Ok(id)
    }

    // ---- 生命周期 ----

    pub(crate) fn shutdown(&self) {
        let st = self.lock();
        for t in st.transfers.values() {
            t.cancel.store(true, Ordering::SeqCst);
        }
    }

    // ---- 数据 ----

    pub(crate) fn profiles(&self) -> Vec<Profile> {
        self.lock().profiles.clone()
    }

    pub(crate) fn save_profile(&self, profile: Profile) -> Result<()> {
        let mut st = self.lock();
        validate_profile(&profile, &st.profiles).map_err(CoreError::User)?;
        if let Some(g) = profile.group
            && !st.groups.iter().any(|x| x.id == g)
        {
            return Err(CoreError::NoSuchGroup(g));
        }
        match st.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => st.profiles.push(profile),
        }
        Ok(())
    }

    pub(crate) async fn delete_profile(&self, id: ProfileId) -> Result<()> {
        let mut st = self.lock();
        if !st.profiles.iter().any(|p| p.id == id) {
            return Err(CoreError::NoSuchProfile(id));
        }
        // 先删凭据：失败时不改动数据，用户可以重试。
        self.secrets.delete_all_for(id).map_err(secret_err)?;
        st.profiles.retain(|p| p.id != id);
        st.snippets.retain(|s| s.scope != SnippetScope::Profile(id));
        st.mcp_grants.remove(&id);
        self.mcp_policy.bump();
        Ok(())
    }

    pub(crate) fn groups(&self) -> Vec<Group> {
        self.lock().groups.clone()
    }

    pub(crate) fn save_group(&self, group: Group) -> Result<()> {
        let mut st = self.lock();
        match st.groups.iter_mut().find(|g| g.id == group.id) {
            Some(slot) => *slot = group,
            None => st.groups.push(group),
        }
        Ok(())
    }

    pub(crate) fn group_delete_impact(&self, id: GroupId) -> Result<DeleteImpact> {
        let st = self.lock();
        if !st.groups.iter().any(|g| g.id == id) {
            return Err(CoreError::NoSuchGroup(id));
        }
        let (profiles, snippets) = group_cascade(&st, id);
        Ok(DeleteImpact {
            profiles: profiles.len() as u32,
            snippets: snippets.len() as u32,
        })
    }

    pub(crate) async fn delete_group(&self, id: GroupId) -> Result<()> {
        let mut st = self.lock();
        if !st.groups.iter().any(|g| g.id == id) {
            return Err(CoreError::NoSuchGroup(id));
        }
        let (profiles, snippets) = group_cascade(&st, id);
        for p in &profiles {
            self.secrets.delete_all_for(*p).map_err(secret_err)?;
        }
        st.profiles.retain(|p| !profiles.contains(&p.id));
        st.snippets.retain(|s| !snippets.contains(&s.id));
        for p in &profiles {
            st.mcp_grants.remove(p);
        }
        self.mcp_policy.bump();
        st.groups.retain(|g| g.id != id);
        Ok(())
    }

    pub(crate) fn snippets(&self) -> Vec<Snippet> {
        self.lock().snippets.clone()
    }

    pub(crate) fn save_snippet(&self, snippet: Snippet) -> Result<()> {
        let mut st = self.lock();
        match st.snippets.iter_mut().find(|s| s.id == snippet.id) {
            Some(slot) => *slot = snippet,
            None => st.snippets.push(snippet),
        }
        Ok(())
    }

    pub(crate) fn delete_snippet(&self, id: SnippetId) -> Result<()> {
        self.lock().snippets.retain(|s| s.id != id);
        Ok(())
    }

    pub(crate) async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> Result<()> {
        let st = self.lock();
        if !st.profiles.iter().any(|p| p.id == profile) {
            return Err(CoreError::NoSuchProfile(profile));
        }
        self.secrets
            .set(&SecretRef { profile, kind }, &secret)
            .map_err(secret_err)
    }

    pub(crate) fn clear_history(&self) -> Result<()> {
        Ok(())
    }

    pub(crate) fn completion_state(&self, _session: SessionId) -> Option<crate::CompletionState> {
        None
    }

    pub(crate) fn complete(
        &self,
        _session: SessionId,
        _input: &str,
        _sources: crate::CompletionSources,
    ) -> crate::CompletionSuggestions {
        crate::CompletionSuggestions::default()
    }

    pub(crate) fn completion_plan(
        &self,
        _session: SessionId,
        _input: &str,
        _sources: &crate::CompletionSources,
    ) -> crate::CompletionSuggestions {
        crate::CompletionSuggestions::default()
    }

    // ---- 设置 ----

    pub(crate) fn setting(&self, key: &str) -> Option<String> {
        self.lock().settings.get(key).cloned()
    }

    pub(crate) fn set_setting(&self, key: &str, json: String) -> Result<()> {
        // 与真实存储一致：非法 JSON 被拒绝，测试才能覆盖"调用方写了坏数据"的路径。
        if serde_json::from_str::<serde_json::Value>(&json).is_err() {
            return Err(CoreError::Internal {
                detail: format!("setting {key:?} is not valid JSON"),
            });
        }
        self.lock().settings.insert(key.to_owned(), json);
        Ok(())
    }

    pub(crate) fn reset_settings(&self) -> Result<()> {
        self.lock().settings.clear();
        Ok(())
    }

    pub(crate) fn set_terminal_options(&self, options: TerminalOptions) {
        self.lock().terminal_options = options;
    }

    /// 测试用：最近一次下发的终端显示选项。
    #[allow(dead_code)]
    pub(crate) fn terminal_options(&self) -> TerminalOptions {
        self.lock().terminal_options
    }

    // ---- 会话 ----

    pub(crate) fn open_session(&self, target: OpenTarget, size: GridSize) -> Result<SessionId> {
        let mut st = self.lock();
        let (profile, kind, name) = match target {
            OpenTarget::Profile(id) => {
                let p = st
                    .profiles
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or(CoreError::NoSuchProfile(id))?;
                (Some(id), p.kind.kind(), p.name.clone())
            }
            OpenTarget::DefaultLocalShell | OpenTarget::LocalShell(_) => {
                (None, SessionKind::Local, "Local".to_owned())
            }
            OpenTarget::Duplicate(src) => {
                let s = st
                    .sessions
                    .iter()
                    .find(|s| s.info.id == src)
                    .ok_or(CoreError::NoSuchSession(src))?;
                (s.info.profile, s.info.kind, s.info.name.clone())
            }
        };
        // 连接对象与用户名取自 Profile；复制的源 Profile 若已被删除，按不存在处理。
        let remote_info = match profile {
            Some(id) => {
                let p = st
                    .profiles
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or(CoreError::NoSuchProfile(id))?;
                ssh_target(p).map(|t| (t.host.clone(), t.port, t.user.clone()))
            }
            None => None,
        };
        let (endpoint, cwd, user) = match remote_info {
            Some((host, port, user)) => (
                Some(Endpoint {
                    host,
                    port,
                    via: Vec::new(),
                }),
                format!("/home/{user}"),
                Some(user),
            ),
            None => (None, "~/workspace/kanso".to_owned(), None),
        };

        let id = SessionId::from_raw(self.next_session.fetch_add(1, Ordering::SeqCst));
        let mut session = MockSession {
            info: SessionInfo {
                id,
                profile,
                kind,
                name: name.clone(),
                custom_title: None,
                state: SessionState::Connecting,
            },
            endpoint: endpoint.clone(),
            cwd,
            size,
            seq: 0,
            dirty: false,
            selected: false,
            search_current: 1,
            headless: false,
            borrowed: false,
        };
        self.emit(CoreEvent::SessionStateChanged {
            session: id,
            state: SessionState::Connecting,
        });
        // 复制标签复用已认证的连接，不再认证（ssh.md §6），所以只有按配置新开时才模拟 MFA 提问。
        let fresh_connection = !matches!(target, OpenTarget::Duplicate(_));
        match (name.as_str(), endpoint, user) {
            ("mfa-bastion", Some(endpoint), Some(user)) if fresh_connection => {
                let prompt = PromptId::from_raw(self.next_prompt.fetch_add(1, Ordering::SeqCst));
                st.prompts.insert(prompt, id);
                self.emit(CoreEvent::Prompt {
                    prompt,
                    session: Some(id),
                    request: PromptRequest::KeyboardInteractive {
                        endpoint,
                        user,
                        name: String::new(),
                        instruction: String::new(),
                        preamble: Vec::new(),
                        questions: vec![KbdQuestion {
                            text: "Verification code: ".into(),
                            echo: false,
                        }],
                    },
                });
            }
            _ => self.finish_connect(&mut session),
        }
        st.sessions.push(session);
        Ok(id)
    }

    pub(crate) fn close_session(&self, session: SessionId) {
        let mut st = self.lock();
        let Some(pos) = st.sessions.iter().position(|s| s.info.id == session) else {
            return;
        };
        let closed = st.sessions.remove(pos);
        if closed.headless {
            // 无界面会话不向 UI 发会话事件；它发起的传输照常取消。
            for t in st.transfers.values().filter(|t| t.session == session) {
                t.cancel.store(true, Ordering::SeqCst);
            }
            return;
        }
        // 借用连接的无界面会话：该配置的最后一个用户标签关闭时一并关闭（spec/mcp.md §2.4）。
        if let Some(profile) = closed.info.profile
            && !st
                .sessions
                .iter()
                .any(|s| !s.headless && s.info.profile == Some(profile))
        {
            let released: Vec<SessionId> = st
                .sessions
                .iter()
                .filter(|s| s.headless && s.borrowed && s.info.profile == Some(profile))
                .map(|s| s.info.id)
                .collect();
            st.sessions.retain(|s| !released.contains(&s.info.id));
            // 与直接关闭无界面会话一致：它发起的传输随之取消。
            for t in st
                .transfers
                .values()
                .filter(|t| released.contains(&t.session))
            {
                t.cancel.store(true, Ordering::SeqCst);
            }
        }
        let dismissed: Vec<PromptId> = st
            .prompts
            .iter()
            .filter(|(_, s)| **s == session)
            .map(|(p, _)| *p)
            .collect();
        for p in dismissed {
            st.prompts.remove(&p);
            self.emit(CoreEvent::PromptDismissed { prompt: p });
        }
        for t in st.transfers.values().filter(|t| t.session == session) {
            t.cancel.store(true, Ordering::SeqCst);
        }
        self.emit(CoreEvent::SessionStateChanged {
            session,
            state: SessionState::Closed(CloseReason::ByUser),
        });
    }

    pub(crate) fn sessions(&self) -> Vec<SessionInfo> {
        self.lock()
            .sessions
            .iter()
            .filter(|s| !s.headless)
            .map(|s| s.info.clone())
            .collect()
    }

    pub(crate) fn session_info(&self, session: SessionId) -> Option<SessionInfo> {
        self.lock()
            .sessions
            .iter()
            .find(|s| s.info.id == session)
            .map(|s| s.info.clone())
    }

    pub(crate) fn set_session_title(&self, session: SessionId, title: Option<String>) {
        if let Some(s) = self
            .lock()
            .sessions
            .iter_mut()
            .find(|s| s.info.id == session)
        {
            s.info.custom_title = title;
        }
    }

    pub(crate) fn set_visible(&self, _session: SessionId, _visible: bool) {
        // mock 没有后台完成通知，可见性无需记录。
    }

    pub(crate) fn resize(
        &self,
        session: SessionId,
        size: GridSize,
        _cell_width_px: u16,
        _cell_height_px: u16,
    ) -> Result<()> {
        let mut st = self.lock();
        let s = terminal(&mut st, session)?;
        s.size = size;
        self.mark_dirty(s, false);
        Ok(())
    }

    // ---- 输入 ----

    pub(crate) fn key(&self, session: SessionId, _event: KeyEvent) -> Result<()> {
        self.input(session)
    }

    pub(crate) fn text_input(&self, session: SessionId, _text: &str) -> Result<()> {
        self.input(session)
    }

    pub(crate) fn paste(&self, session: SessionId, _content: PasteContent) -> Result<()> {
        self.input(session)
    }

    pub(crate) fn mouse(&self, session: SessionId, _event: MouseEvent) -> Result<()> {
        terminal(&mut self.lock(), session).map(|_| ())
    }

    pub(crate) fn scroll(
        &self,
        session: SessionId,
        _delta_lines: f32,
        _at: ViewportPoint,
        _mods: Modifiers,
    ) -> Result<()> {
        terminal(&mut self.lock(), session).map(|_| ())
    }

    pub(crate) fn focus(&self, session: SessionId, _focused: bool) -> Result<()> {
        terminal(&mut self.lock(), session).map(|_| ())
    }

    // ---- 选区与缓冲区 ----

    pub(crate) fn has_selection(&self, session: SessionId) -> bool {
        self.lock()
            .sessions
            .iter()
            .any(|s| s.info.id == session && s.selected)
    }

    pub(crate) fn command_block_text(
        &self,
        _session: SessionId,
        _id: u64,
        _part: u8,
    ) -> Option<String> {
        None
    }

    pub(crate) fn jump_command_block(
        &self,
        session: SessionId,
        _forward: bool,
    ) -> Result<Option<u64>> {
        terminal(&mut self.lock(), session).map(|_| None)
    }

    pub(crate) fn selection_text(&self, session: SessionId) -> Option<String> {
        self.lock()
            .sessions
            .iter()
            .find(|s| s.info.id == session && s.selected)
            .map(|_| "mock selection text".to_owned())
    }

    pub(crate) fn select_word_at(&self, session: SessionId, _at: ViewportPoint) -> Result<()> {
        terminal(&mut self.lock(), session)?.selected = true;
        Ok(())
    }

    pub(crate) fn select_all(&self, session: SessionId) -> Result<()> {
        terminal(&mut self.lock(), session)?.selected = true;
        Ok(())
    }

    pub(crate) fn clear_selection(&self, session: SessionId) -> Result<()> {
        terminal(&mut self.lock(), session)?.selected = false;
        Ok(())
    }

    pub(crate) fn clear_buffer(&self, session: SessionId) -> Result<()> {
        let mut st = self.lock();
        let s = terminal(&mut st, session)?;
        s.selected = false;
        self.mark_dirty(s, true);
        Ok(())
    }

    pub(crate) fn hyperlink_url(&self, session: SessionId, id: u32) -> Option<String> {
        let st = self.lock();
        // 约定 id 0 表示“没有链接”。
        (id != 0 && st.sessions.iter().any(|s| s.info.id == session))
            .then(|| format!("https://example.com/link/{id}"))
    }

    // ---- 搜索 ----

    pub(crate) fn search(&self, session: SessionId, _query: SearchQuery) -> Result<SearchStatus> {
        let mut st = self.lock();
        terminal(&mut st, session)?.search_current = 1;
        Ok(SearchStatus {
            invalid_regex: false,
            total: MOCK_SEARCH_TOTAL,
            current: Some(1),
        })
    }

    pub(crate) fn search_next(&self, session: SessionId, backwards: bool) -> Result<SearchStatus> {
        let mut st = self.lock();
        let s = terminal(&mut st, session)?;
        s.search_current = if backwards {
            if s.search_current <= 1 {
                MOCK_SEARCH_TOTAL
            } else {
                s.search_current - 1
            }
        } else {
            s.search_current % MOCK_SEARCH_TOTAL + 1
        };
        Ok(SearchStatus {
            invalid_regex: false,
            total: MOCK_SEARCH_TOTAL,
            current: Some(s.search_current),
        })
    }

    pub(crate) fn search_status(&self, session: SessionId) -> Result<SearchStatus> {
        let mut state = self.lock();
        let session = terminal(&mut state, session)?;
        Ok(SearchStatus {
            invalid_regex: false,
            total: MOCK_SEARCH_TOTAL,
            current: Some(session.search_current),
        })
    }

    pub(crate) fn search_close(&self, session: SessionId) {
        if let Some(s) = self
            .lock()
            .sessions
            .iter_mut()
            .find(|s| s.info.id == session)
        {
            s.search_current = 1;
        }
    }

    // ---- 显示 ----

    pub(crate) fn set_timestamp_mode(&self, _session: Option<SessionId>, _mode: TimestampMode) {
        // mock 帧的时间戳内容固定，显示模式由 UI 自己决定是否绘制，无需记录。
    }

    // ---- 帧 ----

    pub(crate) fn take_frame(
        &self,
        session: SessionId,
        reuse: &mut Option<Frame>,
    ) -> Option<Frame> {
        let (size, seq) = {
            let mut st = self.lock();
            let s = st.sessions.iter_mut().find(|s| s.info.id == session)?;
            if s.info.kind == SessionKind::Sftp || !s.dirty {
                return None;
            }
            s.dirty = false;
            (s.size, s.seq)
        };
        // 遵守契约：有新帧时取走 `reuse`。mock 帧生成器每次新建缓冲区，所以取走后直接丢弃；产帧放在锁外。
        reuse.take();
        Some(mock_frame(size.cols, size.rows, seq))
    }

    // ---- 提问 ----

    pub(crate) fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
        let mut st = self.lock();
        let Some(session) = st.prompts.remove(&prompt) else {
            return;
        };
        let Some(s) = st.sessions.iter_mut().find(|s| s.info.id == session) else {
            return;
        };
        match response {
            PromptResponse::Answers(_) => self.finish_connect(s),
            // 类型不匹配的应答按取消处理。
            _ => {
                let Some(endpoint) = s.endpoint.clone() else {
                    return;
                };
                let state =
                    SessionState::Closed(CloseReason::Error(Message::AuthCancelled { endpoint }));
                s.info.state = state.clone();
                self.emit(CoreEvent::SessionStateChanged { session, state });
            }
        }
    }

    // ---- 工作目录 ----

    pub(crate) async fn working_directory(&self, session: SessionId) -> Result<String> {
        let mut st = self.lock();
        let s = terminal(&mut st, session)?;
        if is_closed(s) {
            return Err(CoreError::User(Message::CwdNoShell {}));
        }
        Ok(s.cwd.clone())
    }

    // ---- SFTP ----

    pub(crate) async fn sftp_home(&self, session: SessionId) -> Result<String> {
        let st = self.lock();
        let s = remote(&st, session)?;
        // 会话的 cwd 会变，所以主目录重新由 Profile 推导。
        let user = s
            .info
            .profile
            .and_then(|id| st.profiles.iter().find(|p| p.id == id))
            .and_then(ssh_target)
            .map_or("user", |t| t.user.as_str());
        Ok(format!("/home/{user}"))
    }

    pub(crate) async fn sftp_stat(
        &self,
        session: SessionId,
        _path: &str,
    ) -> Result<kanso_types::fs::FileStat> {
        remote(&self.lock(), session)?;
        Ok(kanso_types::fs::FileStat {
            size: Some(2048),
            mtime_ms: Some(1_700_000_000_000),
        })
    }

    pub(crate) fn sftp_list(&self, session: SessionId, path: &str) -> Result<DirStream> {
        remote(&self.lock(), session)?;
        let base = path.trim_end_matches('/');
        let entry = |name: &str, is_dir: bool, is_symlink: bool, size: u64, meta: bool| DirEntry {
            name: name.into(),
            path: format!("{base}/{name}"),
            is_dir,
            is_symlink,
            size,
            mtime_ms: meta.then_some(1_700_000_000_000),
            permissions: meta.then_some(if is_dir { 0o755 } else { 0o644 }),
            owner: meta.then(|| "deploy".to_owned()),
            group: meta.then(|| "deploy".to_owned()),
        };
        Ok(DirStream::from_batches(vec![
            vec![
                entry("docs", true, false, 4096, true),
                entry("current", true, true, 11, true),
                entry("app.log", false, false, 1_048_576, true),
            ],
            vec![
                entry("README.md", false, false, 2048, true),
                // 服务器未提供元数据的条目，用来验证 UI 对 `None` 的处理。
                entry(".bashrc", false, false, 220, false),
            ],
        ]))
    }

    pub(crate) async fn sftp_mkdir(&self, session: SessionId, _path: &str) -> Result<()> {
        remote(&self.lock(), session).map(|_| ())
    }

    pub(crate) async fn sftp_remove(
        &self,
        session: SessionId,
        _path: &str,
        _is_dir: bool,
    ) -> Result<()> {
        remote(&self.lock(), session).map(|_| ())
    }

    pub(crate) async fn sftp_rename(
        &self,
        session: SessionId,
        _from: &str,
        _to: &str,
    ) -> Result<()> {
        remote(&self.lock(), session).map(|_| ())
    }

    pub(crate) async fn sftp_create_file(&self, session: SessionId, _path: &str) -> Result<()> {
        // mock 没有文件系统，无法判断“已存在”，一律成功。
        remote(&self.lock(), session).map(|_| ())
    }

    pub(crate) fn download(
        &self,
        session: SessionId,
        remote_path: &str,
        _local_dir: &str,
        // mock 没有真实的目标文件，不会出现"已存在"，冲突策略无意义。
        _conflict: ConflictPolicy,
    ) -> Result<TransferId> {
        let mut st = self.lock();
        remote(&st, session)?;
        self.start_transfer(
            &mut st,
            session,
            XferProtocol::Sftp,
            TransferDirection::Download,
            remote_path,
            false,
            None,
        )
    }

    /// 与 `download` 相同的模拟传输，额外把结果交给调用方；丢弃 future 不影响传输。
    pub(crate) async fn download_and_wait(
        &self,
        session: SessionId,
        remote_path: &str,
        _local_dir: &str,
        // 同 `download`：mock 不模拟"目标已存在"。
        _conflict: ConflictPolicy,
    ) -> Result<TransferOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut st = self.lock();
            remote(&st, session)?;
            self.start_transfer(
                &mut st,
                session,
                XferProtocol::Sftp,
                TransferDirection::Download,
                remote_path,
                false,
                Some(tx),
            )?;
        }
        rx.await.map_err(|_| CoreError::Internal {
            detail: "mock transfer ended without a result".to_owned(),
        })
    }

    pub(crate) fn upload(
        &self,
        session: SessionId,
        local_path: &str,
        _remote_dir: &str,
        // 同 `download`：mock 不模拟"目标已存在"。
        _conflict: ConflictPolicy,
    ) -> Result<TransferId> {
        let mut st = self.lock();
        remote(&st, session)?;
        self.start_transfer(
            &mut st,
            session,
            XferProtocol::Sftp,
            TransferDirection::Upload,
            local_path,
            false,
            None,
        )
    }

    /// 上传版的 `download_and_wait`。
    pub(crate) async fn upload_and_wait(
        &self,
        session: SessionId,
        local_path: &str,
        _remote_dir: &str,
        _conflict: ConflictPolicy,
    ) -> Result<TransferOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut st = self.lock();
            remote(&st, session)?;
            self.start_transfer(
                &mut st,
                session,
                XferProtocol::Sftp,
                TransferDirection::Upload,
                local_path,
                false,
                Some(tx),
            )?;
        }
        rx.await.map_err(|_| CoreError::Internal {
            detail: "mock transfer ended without a result".to_owned(),
        })
    }

    pub(crate) fn cancel_transfer(&self, transfer: TransferId) {
        if let Some(t) = self.lock().transfers.get(&transfer) {
            t.cancel.store(true, Ordering::SeqCst);
        }
    }

    // ---- 终端内传输 ----

    pub(crate) fn xmodem_send(
        &self,
        session: SessionId,
        path: &str,
        _block: XmodemBlock,
    ) -> Result<TransferId> {
        let mut st = self.lock();
        terminal(&mut st, session)?;
        self.start_transfer(
            &mut st,
            session,
            XferProtocol::Xmodem,
            TransferDirection::Upload,
            path,
            true,
            None,
        )
    }

    pub(crate) fn xmodem_receive(&self, session: SessionId, path: &str) -> Result<TransferId> {
        let mut st = self.lock();
        terminal(&mut st, session)?;
        self.start_transfer(
            &mut st,
            session,
            XferProtocol::Xmodem,
            TransferDirection::Download,
            path,
            true,
            None,
        )
    }

    pub(crate) fn xmodem_start(
        &self,
        session: SessionId,
        block: Option<XmodemBlock>,
    ) -> Result<TransferId> {
        if let Some(block) = block {
            self.xmodem_send(session, "", block)
        } else {
            self.xmodem_receive(session, "")
        }
    }

    pub(crate) fn cancel_terminal_transfer(&self, session: SessionId) {
        for t in self
            .lock()
            .transfers
            .values()
            .filter(|t| t.session == session && t.terminal)
        {
            t.cancel.store(true, Ordering::SeqCst);
        }
    }

    // ---- 导入导出（固定的示例结果）----

    pub(crate) async fn export_data(&self, _path: &str) -> Result<()> {
        Ok(())
    }

    pub(crate) async fn import_data(&self, _path: &str) -> Result<ImportReport> {
        Ok(ImportReport {
            added: 3,
            converted: Vec::new(),
            updated: 1,
            skipped: vec![Message::ImportSkippedUnsupportedKind {
                name: "old-serial".into(),
                kind: "serial".into(),
            }],
        })
    }

    pub(crate) async fn scan_ssh_config(&self, _path: Option<&str>) -> Result<SshConfigScan> {
        let existing = self
            .lock()
            .profiles
            .iter()
            .find(|p| p.name == "web-01")
            .map(|p| p.id);
        let candidates = vec![
            SshConfigCandidate {
                alias: "staging-db".into(),
                host: "10.0.0.21".into(),
                port: 22,
                user: "deploy".into(),
                identity_file: Some("~/.ssh/id_ed25519".into()),
                proxy_jump: Some("web-01".into()),
                existing: None,
                warnings: Vec::new(),
            },
            SshConfigCandidate {
                alias: "web-01".into(),
                host: "web-01.example.com".into(),
                port: 22,
                user: "deploy".into(),
                identity_file: None,
                proxy_jump: None,
                existing,
                warnings: Vec::new(),
            },
            SshConfigCandidate {
                alias: "deep-host".into(),
                host: "10.1.0.5".into(),
                port: 2222,
                user: "root".into(),
                identity_file: None,
                proxy_jump: Some("a,b".into()),
                existing: None,
                warnings: vec![Message::SshConfigMultiHopIgnored {
                    alias: "deep-host".into(),
                }],
            },
        ];
        Ok(SshConfigScan {
            candidates,
            warnings: Vec::new(),
        })
    }

    pub(crate) async fn import_ssh_config(
        &self,
        _path: Option<&str>,
        _aliases: &[String],
        _group: Option<GroupId>,
    ) -> Result<SshConfigImportReport> {
        Ok(SshConfigImportReport {
            added: 2,
            updated: 1,
            jumps_added: 1,
            multi_hop_ignored: 1,
            warnings: vec![Message::SshConfigMultiHopIgnored {
                alias: "deep-host".into(),
            }],
        })
    }

    // ---- 授权 ----

    pub(crate) fn entitlement(&self) -> Option<Entitlement> {
        self.lock().entitlement
    }

    /// 模拟激活成功，不执行真实 License Key 校验。
    pub(crate) async fn activate_license(&self, _key: &str) -> Result<Entitlement> {
        let entitlement = Entitlement {
            source: EntitlementSource::LicenseKey,
        };
        self.lock().entitlement = Some(entitlement);
        Ok(entitlement)
    }

    pub(crate) async fn purchase(&self) -> Result<PurchaseOutcome> {
        Ok(PurchaseOutcome::Purchased(Entitlement {
            source: EntitlementSource::AppStore,
        }))
    }

    pub(crate) async fn restore_purchases(&self) -> Result<Option<Entitlement>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Instant;

    use kanso_frame::FrameView;

    use super::*;
    use crate::api::Core;
    use crate::platform::CollectingEventSink;

    /// 最小的 `block_on`：mock 的 future 都立即就绪，不需要运行时。
    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    fn setup() -> (Core, Arc<CollectingEventSink>) {
        let sink = Arc::new(CollectingEventSink::new());
        let core = Core::new_mock(Platform::for_tests(sink.clone()));
        (core, sink)
    }

    #[test]
    fn settings_roundtrip_reset_and_json_validation() {
        let (core, _) = setup();
        assert_eq!(core.setting("display"), None);
        core.set_setting("display", r#"{"v":1}"#.into()).unwrap();
        assert_eq!(core.setting("display").as_deref(), Some(r#"{"v":1}"#));
        assert!(core.set_setting("display", "{oops".into()).is_err());
        // 写坏数据失败后旧值保留。
        assert_eq!(core.setting("display").as_deref(), Some(r#"{"v":1}"#));
        let before = core.profiles().len();
        core.reset_settings().unwrap();
        assert_eq!(core.setting("display"), None);
        assert_eq!(core.profiles().len(), before);
    }

    const SIZE: GridSize = GridSize { cols: 80, rows: 24 };

    fn profile_id(core: &Core, name: &str) -> ProfileId {
        core.profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("没有 profile {name}"))
            .id
    }

    fn open(core: &Core, name: &str) -> SessionId {
        core.open_session(OpenTarget::Profile(profile_id(core, name)), SIZE)
            .unwrap()
    }

    /// 轮询收集事件直到 `done` 成立，最多 10 秒（GitHub 的 macOS 机器较慢，2 秒不够完成模拟传输）。
    fn collect_until(
        sink: &CollectingEventSink,
        done: impl Fn(&[CoreEvent]) -> bool,
    ) -> Vec<CoreEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        while Instant::now() < deadline {
            all.extend(sink.take());
            if done(&all) {
                return all;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("等待事件超时，已收到：{all:?}");
    }

    fn dirty_count(events: &[CoreEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, CoreEvent::TerminalDirty { .. }))
            .count()
    }

    fn key_event() -> KeyEvent {
        KeyEvent {
            key: kanso_types::Key::Character("a".into()),
            mods: Modifiers::NONE,
            text: Some("a".into()),
            kind: kanso_types::KeyEventKind::Press,
        }
    }

    #[test]
    fn core_is_send_sync_and_seed_data() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Core>();
        let (core, _) = setup();
        let names: Vec<_> = core.profiles().into_iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            ["Local", "web-01", "legacy-switch", "mfa-bastion", "files"]
        );
        assert_eq!(core.groups().len(), 1);
        assert_eq!(core.snippets().len(), 2);
    }

    #[test]
    fn open_local_session_event_sequence() {
        let (core, sink) = setup();
        let id = open(&core, "Local");
        assert_eq!(
            sink.take(),
            vec![
                CoreEvent::SessionStateChanged {
                    session: id,
                    state: SessionState::Connecting
                },
                CoreEvent::SessionStateChanged {
                    session: id,
                    state: SessionState::Connected
                },
                CoreEvent::LocationChanged {
                    session: id,
                    cwd: Some("~/workspace/kanso".into()),
                    cwd_host: None,
                    git_branch: Some("main".into())
                },
                CoreEvent::ActivityChanged {
                    session: id,
                    activity: Activity::Idle
                },
                CoreEvent::TerminalDirty { session: id },
            ]
        );
        assert_eq!(
            core.session_info(id).unwrap().state,
            SessionState::Connected
        );
    }

    #[test]
    fn dirty_event_is_coalesced() {
        let (core, sink) = setup();
        let id = open(&core, "Local");
        sink.take();
        // 打开后终端是脏的：再输入不会发事件。
        core.key(id, key_event()).unwrap();
        assert_eq!(dirty_count(&sink.take()), 0);
        assert!(core.take_frame(id, &mut None).is_some());
        assert!(core.take_frame(id, &mut None).is_none());
        core.key(id, key_event()).unwrap();
        core.text_input(id, "x").unwrap();
        core.paste(id, PasteContent::Text("y".into())).unwrap();
        assert_eq!(dirty_count(&sink.take()), 1);
        assert!(core.take_frame(id, &mut None).is_some());
        core.key(id, key_event()).unwrap();
        assert_eq!(dirty_count(&sink.take()), 1);
    }

    #[test]
    fn take_frame_keeps_reuse_without_new_frame_and_takes_it_with_one() {
        let (core, _) = setup();
        let id = open(&core, "Local");
        let mut reuse = Some(mock_frame(80, 24, 1));
        // 刚打开的会话是脏的：有新帧，reuse 被取走。
        assert!(core.take_frame(id, &mut reuse).is_some());
        assert!(reuse.is_none());
        // 没有新帧：reuse 原样保留。
        let mut reuse = Some(mock_frame(80, 24, 2));
        let ptr = reuse.as_ref().unwrap().as_ptr();
        assert!(core.take_frame(id, &mut reuse).is_none());
        assert_eq!(reuse.as_ref().map(Frame::as_ptr), Some(ptr));
    }

    #[test]
    fn frame_parses_with_expected_size() {
        let (core, _) = setup();
        let id = open(&core, "Local");
        core.resize(
            id,
            GridSize {
                cols: 100,
                rows: 30,
            },
            8,
            16,
        )
        .unwrap();
        let frame = core.take_frame(id, &mut None).unwrap();
        let view = FrameView::parse(frame.as_bytes()).unwrap();
        assert_eq!((view.header().cols, view.header().rows), (100, 30));
    }

    #[test]
    fn mfa_bastion_answers_connects() {
        let (core, sink) = setup();
        let id = open(&core, "mfa-bastion");
        let events = sink.take();
        let prompt = events
            .iter()
            .find_map(|e| match e {
                CoreEvent::Prompt {
                    prompt, request, ..
                } => {
                    let PromptRequest::KeyboardInteractive { questions, .. } = request else {
                        panic!("提问类型不对");
                    };
                    assert_eq!(questions[0].text, "Verification code: ");
                    assert!(!questions[0].echo);
                    Some(*prompt)
                }
                _ => None,
            })
            .expect("应当先发 Prompt");
        assert!(!events.iter().any(|e| matches!(
            e,
            CoreEvent::SessionStateChanged {
                state: SessionState::Connected,
                ..
            }
        )));
        core.respond_prompt(prompt, PromptResponse::Answers(vec!["123456".into()]));
        let after = sink.take();
        assert!(after.contains(&CoreEvent::SessionStateChanged {
            session: id,
            state: SessionState::Connected
        }));
        // 应答过期后再次应答被忽略。
        core.respond_prompt(prompt, PromptResponse::Cancel);
        assert!(sink.take().is_empty());
        assert_eq!(
            core.session_info(id).unwrap().state,
            SessionState::Connected
        );
    }

    #[test]
    fn mfa_bastion_cancel_closes_with_auth_cancelled() {
        let (core, sink) = setup();
        let id = open(&core, "mfa-bastion");
        let prompt = sink
            .take()
            .into_iter()
            .find_map(|e| match e {
                CoreEvent::Prompt { prompt, .. } => Some(prompt),
                _ => None,
            })
            .unwrap();
        core.respond_prompt(prompt, PromptResponse::Cancel);
        let events = sink.take();
        let [
            CoreEvent::SessionStateChanged {
                session,
                state: SessionState::Closed(CloseReason::Error(Message::AuthCancelled { endpoint })),
            },
        ] = events.as_slice()
        else {
            panic!("事件不符：{events:?}");
        };
        assert_eq!(*session, id);
        assert_eq!(endpoint.host, "bastion.example.com");
    }

    #[test]
    fn legacy_switch_reports_algorithms() {
        let (core, sink) = setup();
        let id = open(&core, "legacy-switch");
        let events = sink.take();
        let hops = events
            .iter()
            .find_map(|e| match e {
                CoreEvent::LegacyAlgorithms { session, hops } if *session == id => Some(hops),
                _ => None,
            })
            .expect("应当发 LegacyAlgorithms");
        assert_eq!(hops.len(), 1);
        assert_eq!(
            hops[0].algorithms,
            ["diffie-hellman-group14-sha1", "aes128-cbc", "hmac-sha1"]
        );
    }

    #[test]
    fn duplicate_copies_profile() {
        let (core, _) = setup();
        let a = open(&core, "web-01");
        let b = core.open_session(OpenTarget::Duplicate(a), SIZE).unwrap();
        assert_ne!(a, b);
        let (ia, ib) = (core.session_info(a).unwrap(), core.session_info(b).unwrap());
        assert_eq!(
            (ia.profile, ia.kind, ia.name),
            (ib.profile, ib.kind, ib.name)
        );
        assert!(matches!(
            core.open_session(OpenTarget::Duplicate(SessionId::from_raw(999)), SIZE),
            Err(CoreError::NoSuchSession(_))
        ));
    }

    #[test]
    fn sftp_session_rejects_terminal_ops_and_local_rejects_sftp() {
        let (core, _) = setup();
        let sftp = open(&core, "files");
        assert_eq!(core.key(sftp, key_event()), Err(CoreError::NotSupported));
        assert!(core.take_frame(sftp, &mut None).is_none());
        let local = open(&core, "Local");
        assert_eq!(
            block_on(core.sftp_home(local)),
            Err(CoreError::NotSupported)
        );
    }

    #[test]
    fn sftp_home_list_and_cwd() {
        let (core, _) = setup();
        let id = open(&core, "web-01");
        assert_eq!(block_on(core.sftp_home(id)).unwrap(), "/home/deploy");
        assert_eq!(
            block_on(core.working_directory(id)).unwrap(),
            "/home/deploy"
        );
        let mut stream = core.sftp_list(id, "/var/www/").unwrap();
        let mut sizes = Vec::new();
        while let Some(batch) = block_on(stream.next_batch()).unwrap() {
            sizes.push(batch.len());
        }
        assert_eq!(sizes, [3, 2]);
    }

    #[test]
    fn save_profile_validates() {
        let (core, _) = setup();
        let mut p = core
            .profiles()
            .into_iter()
            .find(|p| p.name == "web-01")
            .unwrap();
        if let ProfileKind::Ssh(s) = &mut p.kind {
            s.target.host.clear();
        }
        assert_eq!(
            core.save_profile(p),
            Err(CoreError::User(Message::ProfileHostEmpty {}))
        );
    }

    #[test]
    fn group_delete_cascade_matches_impact() {
        let (core, _) = setup();
        let gid = core.groups()[0].id;
        let web = profile_id(&core, "web-01");
        block_on(core.set_credential(web, SecretKind::Password, SecretString::new("pw"))).unwrap();
        // 加一条作用域为分组的命令：影响应为 1 个会话 + 2 条命令（分组命令与 web-01 专属命令）。
        core.save_snippet(Snippet {
            id: SnippetId::new(),
            name: "g".into(),
            text: "ls".into(),
            line_ending: LineEnding::Lf,
            scope: SnippetScope::Group(gid),
        })
        .unwrap();
        let impact = core.group_delete_impact(gid).unwrap();
        assert_eq!(
            impact,
            DeleteImpact {
                profiles: 1,
                snippets: 2
            }
        );
        let (p_before, s_before) = (core.profiles().len(), core.snippets().len());
        block_on(core.delete_group(gid)).unwrap();
        assert_eq!(core.profiles().len(), p_before - impact.profiles as usize);
        assert_eq!(core.snippets().len(), s_before - impact.snippets as usize);
        assert!(core.groups().is_empty());
        // 全局命令保留。
        assert_eq!(core.snippets().len(), 1);
        assert_eq!(
            core.group_delete_impact(gid),
            Err(CoreError::NoSuchGroup(gid))
        );
    }

    #[test]
    fn delete_profile_removes_credentials_and_own_snippets() {
        let sink = Arc::new(CollectingEventSink::new());
        let platform = Platform::for_tests(sink);
        let secrets = platform.secrets.clone();
        let core = Core::new_mock(platform);
        let web = profile_id(&core, "web-01");
        block_on(core.set_credential(web, SecretKind::Password, SecretString::new("pw"))).unwrap();
        let r = SecretRef {
            profile: web,
            kind: SecretKind::Password,
        };
        assert!(secrets.get(&r).unwrap().is_some());
        block_on(core.delete_profile(web)).unwrap();
        assert!(secrets.get(&r).unwrap().is_none());
        assert_eq!(core.snippets().len(), 1);
        assert_eq!(
            block_on(core.delete_profile(web)),
            Err(CoreError::NoSuchProfile(web))
        );
    }

    #[test]
    fn selection_and_search() {
        let (core, _) = setup();
        let id = open(&core, "Local");
        assert!(!core.has_selection(id));
        assert_eq!(core.selection_text(id), None);
        core.select_all(id).unwrap();
        assert!(core.has_selection(id));
        assert!(core.selection_text(id).is_some());
        core.clear_selection(id).unwrap();
        assert!(!core.has_selection(id));

        let q = SearchQuery {
            text: "x".into(),
            case_sensitive: false,
            regex: false,
        };
        let st = |current| SearchStatus {
            invalid_regex: false,
            total: 3,
            current: Some(current),
        };
        assert_eq!(core.search(id, q).unwrap(), st(1));
        assert_eq!(core.search_next(id, false).unwrap(), st(2));
        assert_eq!(core.search_next(id, false).unwrap(), st(3));
        assert_eq!(core.search_next(id, false).unwrap(), st(1));
        assert_eq!(core.search_next(id, true).unwrap(), st(3));
    }

    #[test]
    fn transfer_completes() {
        let (core, sink) = setup();
        let id = open(&core, "web-01");
        sink.take();
        let t = core
            .download(id, "/var/log/app.log", "/tmp", ConflictPolicy::Fail)
            .unwrap();
        let events = collect_until(&sink, |ev| {
            ev.iter()
                .any(|e| matches!(e, CoreEvent::TransferFinished { .. }))
        });
        assert!(matches!(
            &events[0],
            CoreEvent::TransferStarted { transfer, name, .. } if *transfer == t && name == "app.log"
        ));
        let progress = events
            .iter()
            .filter(|e| matches!(e, CoreEvent::TransferProgress { .. }))
            .count();
        assert_eq!(progress, 20);
        assert_eq!(
            events.last().unwrap(),
            &CoreEvent::TransferFinished {
                transfer: t,
                outcome: TransferOutcome::Completed { skipped: 0 }
            }
        );
    }

    #[test]
    fn cancel_transfer_reports_cancelled() {
        let (core, sink) = setup();
        let id = open(&core, "web-01");
        sink.take();
        let t = core
            .upload(id, "/tmp/a.bin", "/srv", ConflictPolicy::Fail)
            .unwrap();
        core.cancel_transfer(t);
        let events = collect_until(&sink, |ev| {
            ev.iter()
                .any(|e| matches!(e, CoreEvent::TransferFinished { .. }))
        });
        assert_eq!(
            events.last().unwrap(),
            &CoreEvent::TransferFinished {
                transfer: t,
                outcome: TransferOutcome::Cancelled
            }
        );
    }

    #[test]
    fn close_session_emits_closed_by_user() {
        let (core, sink) = setup();
        let id = open(&core, "Local");
        sink.take();
        core.close_session(id);
        assert_eq!(
            sink.take(),
            vec![CoreEvent::SessionStateChanged {
                session: id,
                state: SessionState::Closed(CloseReason::ByUser)
            }]
        );
        assert!(core.session_info(id).is_none());
        assert_eq!(core.key(id, key_event()), Err(CoreError::NoSuchSession(id)));
    }

    #[test]
    fn entitlement_follows_channel() {
        let (core, _) = setup();
        assert_eq!(core.entitlement(), Entitlement::for_channel());
    }

    #[cfg(feature = "direct")]
    #[test]
    fn activation_updates_mock_entitlement() {
        let (core, _) = setup();
        assert_eq!(core.entitlement(), None);
        let entitlement = block_on(core.activate_license("mock-key")).unwrap();
        assert_eq!(entitlement.source, EntitlementSource::LicenseKey);
        assert_eq!(core.entitlement(), Some(entitlement));
    }

    #[test]
    fn import_export_return_fixed_results() {
        let (core, _) = setup();
        let r = block_on(core.import_data("/tmp/x.kanso")).unwrap();
        assert_eq!((r.added, r.updated, r.skipped.len()), (3, 1, 1));
        assert!(block_on(core.export_data("/tmp/x.kanso")).is_ok());
        assert_eq!(
            block_on(core.scan_ssh_config(None))
                .unwrap()
                .candidates
                .len(),
            3
        );
        assert_eq!(block_on(core.restore_purchases()).unwrap(), None);
    }

    #[test]
    fn set_session_title_trims_and_clears() {
        let (core, _) = setup();
        let id = open(&core, "Local");
        assert_eq!(core.session_info(id).unwrap().custom_title, None);
        core.set_session_title(id, Some("  构建  ".into()));
        assert_eq!(
            core.session_info(id).unwrap().custom_title.as_deref(),
            Some("构建")
        );
        core.set_session_title(id, Some("   ".into()));
        assert_eq!(core.session_info(id).unwrap().custom_title, None);
        // 不存在的会话静默忽略。
        core.set_session_title(SessionId::from_raw(999_999), Some("x".into()));
    }
}
