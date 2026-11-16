//! core 推送给 UI 的事件，以及提问（Prompt）的请求与应答。

use kanso_types::CoreEvent;
use kanso_types::agent::AgentKind;
use kanso_types::event::{
    Activity, CloseReason, LegacyHop, NoticeLevel, SessionState, TransferDirection, TransferOutcome,
};
use kanso_types::prompt::{CredentialKind, KbdQuestion, PromptRequest};
use kanso_types::secret::SecretString;
use kanso_types::transfer::{ConflictPolicy, XferProtocol};
use kanso_types::{PromptId, PromptResponse, SessionId, TransferId};

use super::message::{FfiEndpoint, FfiMessage};
use super::mirror_enum;

mirror_enum! {
    /// AI 命令行 agent 种类。
    FfiAgentKind <=> AgentKind {
        /// Claude Code。
        Claude,
        /// Codex CLI。
        Codex,
        /// Gemini CLI。
        Gemini,
        /// Aider。
        Aider,
        /// Cursor Agent。
        CursorAgent,
        /// GitHub Copilot CLI。
        Copilot,
        /// Amp。
        Amp,
        /// OpenCode。
        OpenCode,
        /// Crush。
        Crush,
        /// Goose。
        Goose,
        /// Qwen Code。
        Qwen,
        /// Droid。
        Droid,
        /// OpenHands。
        OpenHands,
        /// Grok CLI。
        Grok,
    }
}

mirror_enum! {
    /// 传输协议种类。
    FfiXferProtocol <=> XferProtocol {
        /// SFTP。
        Sftp,
        /// ZMODEM。
        Zmodem,
        /// XMODEM。
        Xmodem,
    }
}

mirror_enum! {
    /// 传输遇到目标已存在时的处理策略。
    FfiConflictPolicy <=> ConflictPolicy {
        /// 已存在即失败。
        Fail,
        /// 覆盖。
        Overwrite,
        /// 跳过已存在的文件。
        Skip,
    }
}

mirror_enum! {
    /// 传输方向。
    FfiTransferDirection <=> TransferDirection {
        /// 上传。
        Upload,
        /// 下载。
        Download,
    }
}

mirror_enum! {
    /// 通知级别。
    FfiNoticeLevel <=> NoticeLevel {
        /// 信息。
        Info,
        /// 警告。
        Warning,
        /// 错误。
        Error,
    }
}

/// 会话关闭原因。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCloseReason {
    /// 用户主动关闭。
    ByUser,
    /// 进程 / 远端 shell 退出。
    Exited {
        /// 退出码；未知为 `None`。
        code: Option<i32>,
    },
    /// 出错关闭。
    Error {
        /// 可展示给用户的消息。
        message: FfiMessage,
    },
}

/// 会话状态。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSessionState {
    /// 连接中。
    Connecting,
    /// 已连接。
    Connected,
    /// 已关闭。
    Closed {
        /// 关闭原因。
        reason: FfiCloseReason,
    },
}

impl From<&CloseReason> for FfiCloseReason {
    fn from(r: &CloseReason) -> Self {
        match r {
            CloseReason::ByUser => FfiCloseReason::ByUser,
            CloseReason::Exited { code } => FfiCloseReason::Exited { code: *code },
            CloseReason::Error(m) => FfiCloseReason::Error { message: m.into() },
        }
    }
}

impl From<&SessionState> for FfiSessionState {
    fn from(s: &SessionState) -> Self {
        match s {
            SessionState::Connecting => FfiSessionState::Connecting,
            SessionState::Connected => FfiSessionState::Connected,
            SessionState::Closed(r) => FfiSessionState::Closed { reason: r.into() },
        }
    }
}

/// 会话活动状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiActivity {
    /// 空闲。
    Idle,
    /// 有命令在运行。
    CommandRunning,
    /// agent 在等待输入。
    AgentIdle {
        /// agent 种类。
        agent: FfiAgentKind,
    },
    /// agent 在工作。
    AgentWorking {
        /// agent 种类。
        agent: FfiAgentKind,
    },
}

impl From<Activity> for FfiActivity {
    fn from(a: Activity) -> Self {
        match a {
            Activity::Idle => FfiActivity::Idle,
            Activity::CommandRunning => FfiActivity::CommandRunning,
            Activity::AgentIdle(agent) => FfiActivity::AgentIdle {
                agent: agent.into(),
            },
            Activity::AgentWorking(agent) => FfiActivity::AgentWorking {
                agent: agent.into(),
            },
        }
    }
}

/// 使用了过时算法的一跳连接。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiLegacyHop {
    /// 连接对象。
    pub endpoint: FfiEndpoint,
    /// 协商用到的过时算法名。
    pub algorithms: Vec<String>,
}

impl From<&LegacyHop> for FfiLegacyHop {
    fn from(h: &LegacyHop) -> Self {
        FfiLegacyHop {
            endpoint: (&h.endpoint).into(),
            algorithms: h.algorithms.clone(),
        }
    }
}

/// 传输结果。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTransferOutcome {
    /// 完成。
    Completed {
        /// 因目标已存在且策略为 `Skip` 而跳过的文件数。
        skipped: u32,
    },
    /// 已取消。
    Cancelled,
    /// 失败。
    Failed {
        /// 可展示给用户的消息。
        message: FfiMessage,
        /// 失败原因是"目标已存在"；UI 据此询问覆盖 / 跳过 / 取消，确认后带 `Overwrite` 或 `Skip` 重试。
        already_exists: bool,
    },
}

impl From<&TransferOutcome> for FfiTransferOutcome {
    fn from(o: &TransferOutcome) -> Self {
        match o {
            TransferOutcome::Completed { skipped } => {
                FfiTransferOutcome::Completed { skipped: *skipped }
            }
            TransferOutcome::Cancelled => FfiTransferOutcome::Cancelled,
            TransferOutcome::Failed {
                message,
                already_exists,
            } => FfiTransferOutcome::Failed {
                message: message.into(),
                already_exists: *already_exists,
            },
        }
    }
}

/// keyboard-interactive 的一个问题。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiKbdQuestion {
    /// 服务器给的提示文本（原样，不本地化）。
    pub text: String,
    /// 输入时是否回显。
    pub echo: bool,
}

/// 需要用户提供的凭据种类。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCredentialKind {
    /// 登录密码。
    Password,
    /// 私钥口令。
    KeyPassphrase {
        /// 私钥文件路径。
        path: String,
    },
}

/// 需要用户回答的请求。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPromptRequest {
    /// 主机密钥与 known_hosts 记录不符。
    HostKeyChanged {
        /// 连接对象。
        endpoint: FfiEndpoint,
        /// 密钥类型。
        key_type: String,
        /// SHA256 指纹。
        fingerprint_sha256: String,
        /// known_hosts 文件路径。
        known_hosts_path: String,
        /// 旧记录所在行号。
        line: u32,
    },
    /// keyboard-interactive 认证（含 MFA 验证码）。
    KeyboardInteractive {
        /// 连接对象。
        endpoint: FfiEndpoint,
        /// 用户名。
        user: String,
        /// 服务器给的名称。
        name: String,
        /// 服务器给的说明。
        instruction: String,
        /// 问题之前要显示的文本行。
        preamble: Vec<String>,
        /// 问题列表。
        questions: Vec<FfiKbdQuestion>,
    },
    /// 需要密码或私钥口令。
    Credential {
        /// 连接对象。
        endpoint: FfiEndpoint,
        /// 用户名。
        user: String,
        /// 凭据种类。
        kind: FfiCredentialKind,
    },
    /// ZMODEM 收到文件，询问保存位置。
    ZmodemSaveFile {
        /// 对端建议的文件名。
        suggested_name: String,
        /// 文件大小；未知为 `None`。
        size: Option<u64>,
    },
    /// ZMODEM 对端要接收文件，询问选哪些。
    ZmodemPickFiles,
    /// XMODEM 发送时选择单个文件。
    XmodemPickFile,
    /// XMODEM 接收时选择保存位置。
    XmodemSaveFile,
}

impl From<&PromptRequest> for FfiPromptRequest {
    fn from(r: &PromptRequest) -> Self {
        match r {
            PromptRequest::HostKeyChanged {
                endpoint,
                key_type,
                fingerprint_sha256,
                known_hosts_path,
                line,
            } => FfiPromptRequest::HostKeyChanged {
                endpoint: endpoint.into(),
                key_type: key_type.clone(),
                fingerprint_sha256: fingerprint_sha256.clone(),
                known_hosts_path: known_hosts_path.clone(),
                line: *line,
            },
            PromptRequest::KeyboardInteractive {
                endpoint,
                user,
                name,
                instruction,
                preamble,
                questions,
            } => FfiPromptRequest::KeyboardInteractive {
                endpoint: endpoint.into(),
                user: user.clone(),
                name: name.clone(),
                instruction: instruction.clone(),
                preamble: preamble.clone(),
                questions: questions
                    .iter()
                    .map(|q: &KbdQuestion| FfiKbdQuestion {
                        text: q.text.clone(),
                        echo: q.echo,
                    })
                    .collect(),
            },
            PromptRequest::Credential {
                endpoint,
                user,
                kind,
            } => FfiPromptRequest::Credential {
                endpoint: endpoint.into(),
                user: user.clone(),
                kind: match kind {
                    CredentialKind::Password => FfiCredentialKind::Password,
                    CredentialKind::KeyPassphrase { path } => {
                        FfiCredentialKind::KeyPassphrase { path: path.clone() }
                    }
                },
            },
            PromptRequest::ZmodemSaveFile {
                suggested_name,
                size,
            } => FfiPromptRequest::ZmodemSaveFile {
                suggested_name: suggested_name.clone(),
                size: *size,
            },
            PromptRequest::ZmodemPickFiles => FfiPromptRequest::ZmodemPickFiles,
            PromptRequest::XmodemPickFile => FfiPromptRequest::XmodemPickFile,
            PromptRequest::XmodemSaveFile => FfiPromptRequest::XmodemSaveFile,
        }
    }
}

/// 用户对 [`FfiPromptRequest`] 的应答。
///
/// 不派生 `Debug`：`Credential` 含明文口令，避免被 `{:?}` 误写进日志。
#[derive(Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPromptResponse {
    /// 取消 / 拒绝。
    Cancel,
    /// 接受新的主机密钥。
    AcceptHostKey,
    /// keyboard-interactive 的答案，与问题一一对应。
    Answers {
        /// 答案。
        answers: Vec<String>,
    },
    /// 凭据。
    Credential {
        /// 明文凭据；只经 core 的 `SecretString` 处理，不写日志。
        secret: String,
        /// 是否记住到钥匙串。
        remember: bool,
    },
    /// ZMODEM 保存路径。
    SavePath {
        /// 保存路径。
        path: String,
    },
    /// ZMODEM 选中的文件。
    Files {
        /// 文件路径。
        paths: Vec<String>,
    },
}

impl From<FfiPromptResponse> for PromptResponse {
    fn from(r: FfiPromptResponse) -> Self {
        match r {
            FfiPromptResponse::Cancel => PromptResponse::Cancel,
            FfiPromptResponse::AcceptHostKey => PromptResponse::AcceptHostKey,
            FfiPromptResponse::Answers { answers } => PromptResponse::Answers(answers),
            FfiPromptResponse::Credential { secret, remember } => PromptResponse::Credential {
                secret: SecretString::new(secret),
                remember,
            },
            FfiPromptResponse::SavePath { path } => PromptResponse::SavePath(path),
            FfiPromptResponse::Files { paths } => PromptResponse::Files(paths),
        }
    }
}

/// core 推送给 UI 的事件。运行时 id（会话、传输、提问）都是 `u64`。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCoreEvent {
    /// 会话状态变化。
    SessionStateChanged {
        /// 会话。
        session: u64,
        /// 新状态。
        state: FfiSessionState,
    },
    /// 终端由干净变脏：UI 应在渲染线程取帧。
    TerminalDirty {
        /// 会话。
        session: u64,
    },
    /// 工作目录、主机或 git 分支变化。
    LocationChanged {
        /// 会话。
        session: u64,
        /// 当前工作目录。
        cwd: Option<String>,
        /// 工作目录所在主机。
        cwd_host: Option<String>,
        /// git 分支。
        git_branch: Option<String>,
    },
    /// 会话活动状态变化。
    ActivityChanged {
        /// 会话。
        session: u64,
        /// 新活动状态。
        activity: FfiActivity,
    },
    /// 连接用到了过时算法，需要提示用户。
    LegacyAlgorithms {
        /// 会话。
        session: u64,
        /// 各跳使用的过时算法。
        hops: Vec<FfiLegacyHop>,
    },
    /// 需要提示用户的通知。
    Notice {
        /// 关联会话；全局通知为 `None`。
        session: Option<u64>,
        /// 级别。
        level: FfiNoticeLevel,
        /// 可展示给用户的消息。
        message: FfiMessage,
    },
    /// 需要用户回答的请求；用 `respond_prompt` 应答。
    Prompt {
        /// 提问 id。
        prompt: u64,
        /// 关联会话。
        session: Option<u64>,
        /// 请求内容。
        request: FfiPromptRequest,
    },
    /// 提问已失效（会话关闭等），UI 应关闭对应对话框。
    PromptDismissed {
        /// 提问 id。
        prompt: u64,
    },
    /// 传输开始。
    TransferStarted {
        /// 传输 id。
        transfer: u64,
        /// 会话。
        session: u64,
        /// 协议。
        protocol: FfiXferProtocol,
        /// 方向。
        direction: FfiTransferDirection,
        /// 文件名或目录名。
        name: String,
        /// 总字节数；未知为 `None`。
        total_bytes: Option<u64>,
    },
    /// 传输进度。
    TransferProgress {
        /// 传输 id。
        transfer: u64,
        /// 已完成字节数。
        done_bytes: u64,
        /// 总字节数；未知为 `None`。
        total_bytes: Option<u64>,
    },
    /// 传输结束。
    TransferFinished {
        /// 传输 id。
        transfer: u64,
        /// 结果。
        outcome: FfiTransferOutcome,
    },
}

impl From<&CoreEvent> for FfiCoreEvent {
    fn from(e: &CoreEvent) -> Self {
        let sid = |s: &SessionId| s.raw();
        let tid = |t: &TransferId| t.raw();
        let pid = |p: &PromptId| p.raw();
        match e {
            CoreEvent::SessionStateChanged { session, state } => {
                FfiCoreEvent::SessionStateChanged {
                    session: sid(session),
                    state: state.into(),
                }
            }
            CoreEvent::TerminalDirty { session } => FfiCoreEvent::TerminalDirty {
                session: sid(session),
            },
            CoreEvent::LocationChanged {
                session,
                cwd,
                cwd_host,
                git_branch,
            } => FfiCoreEvent::LocationChanged {
                session: sid(session),
                cwd: cwd.clone(),
                cwd_host: cwd_host.clone(),
                git_branch: git_branch.clone(),
            },
            CoreEvent::ActivityChanged { session, activity } => FfiCoreEvent::ActivityChanged {
                session: sid(session),
                activity: (*activity).into(),
            },
            CoreEvent::LegacyAlgorithms { session, hops } => FfiCoreEvent::LegacyAlgorithms {
                session: sid(session),
                hops: hops.iter().map(Into::into).collect(),
            },
            CoreEvent::Notice {
                session,
                level,
                message,
            } => FfiCoreEvent::Notice {
                session: session.as_ref().map(sid),
                level: (*level).into(),
                message: message.into(),
            },
            CoreEvent::Prompt {
                prompt,
                session,
                request,
            } => FfiCoreEvent::Prompt {
                prompt: pid(prompt),
                session: session.as_ref().map(sid),
                request: request.into(),
            },
            CoreEvent::PromptDismissed { prompt } => FfiCoreEvent::PromptDismissed {
                prompt: pid(prompt),
            },
            CoreEvent::TransferStarted {
                transfer,
                session,
                protocol,
                direction,
                name,
                total_bytes,
            } => FfiCoreEvent::TransferStarted {
                transfer: tid(transfer),
                session: sid(session),
                protocol: (*protocol).into(),
                direction: (*direction).into(),
                name: name.clone(),
                total_bytes: *total_bytes,
            },
            CoreEvent::TransferProgress {
                transfer,
                done_bytes,
                total_bytes,
            } => FfiCoreEvent::TransferProgress {
                transfer: tid(transfer),
                done_bytes: *done_bytes,
                total_bytes: *total_bytes,
            },
            CoreEvent::TransferFinished { transfer, outcome } => FfiCoreEvent::TransferFinished {
                transfer: tid(transfer),
                outcome: outcome.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::Message;

    #[test]
    fn events_map_ids_and_payloads() {
        let s = SessionId::from_raw(7);
        assert_eq!(
            FfiCoreEvent::from(&CoreEvent::TerminalDirty { session: s }),
            FfiCoreEvent::TerminalDirty { session: 7 }
        );
        let ev = CoreEvent::SessionStateChanged {
            session: s,
            state: SessionState::Closed(CloseReason::Error(Message::CwdNoShell {})),
        };
        match FfiCoreEvent::from(&ev) {
            FfiCoreEvent::SessionStateChanged {
                session: 7,
                state:
                    FfiSessionState::Closed {
                        reason: FfiCloseReason::Error { message },
                    },
            } => assert_eq!(message.key, "cwd-no-shell"),
            other => panic!("非预期：{other:?}"),
        }
        let ev = CoreEvent::TransferFinished {
            transfer: TransferId::from_raw(3),
            outcome: TransferOutcome::Cancelled,
        };
        assert_eq!(
            FfiCoreEvent::from(&ev),
            FfiCoreEvent::TransferFinished {
                transfer: 3,
                outcome: FfiTransferOutcome::Cancelled
            }
        );
        let ev = CoreEvent::ActivityChanged {
            session: s,
            activity: Activity::AgentWorking(AgentKind::OpenHands),
        };
        assert_eq!(
            FfiCoreEvent::from(&ev),
            FfiCoreEvent::ActivityChanged {
                session: 7,
                activity: FfiActivity::AgentWorking {
                    agent: FfiAgentKind::OpenHands
                }
            }
        );
    }

    #[test]
    fn prompt_roundtrip_pieces() {
        let req = PromptRequest::KeyboardInteractive {
            endpoint: kanso_types::message::Endpoint {
                host: "h".into(),
                port: 22,
                via: vec![],
            },
            user: "u".into(),
            name: String::new(),
            instruction: String::new(),
            preamble: vec![],
            questions: vec![KbdQuestion {
                text: "code: ".into(),
                echo: false,
            }],
        };
        match FfiPromptRequest::from(&req) {
            FfiPromptRequest::KeyboardInteractive { questions, .. } => {
                assert_eq!(
                    questions,
                    vec![FfiKbdQuestion {
                        text: "code: ".into(),
                        echo: false
                    }]
                );
            }
            other => panic!("非预期：{other:?}"),
        }
        match PromptResponse::from(FfiPromptResponse::Credential {
            secret: "pw".into(),
            remember: true,
        }) {
            PromptResponse::Credential { secret, remember } => {
                assert_eq!(secret.expose(), "pw");
                assert!(remember);
            }
            other => panic!("非预期：{other:?}"),
        }
    }
}
