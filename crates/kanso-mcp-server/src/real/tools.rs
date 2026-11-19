//! rmcp 处理器与工具（spec/mcp.md §3、§5、§9）。
//!
//! 每个连接一个 [`KansoHandler`]，持有共享上下文和自己的客户端编号。工具方法只写业务逻辑，
//! 开关检查、调用计数、审计、结果格式统一在 [`KansoHandler::invoke`] 里，新增工具时不要绕开它。
//!
//! 终端写操作（run、send）统一走 [`KansoHandler::write_op`]：级别判断、客户端所在标签、写锁与冷却、
//! 活动标识、中断都在那里，工具方法只提供"怎么调 core"和"怎么解释结果"。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_core::mcp::{MCP_TEXT_LIMIT, McpSessionStatus, TextScope, WaitLimits};
use kanso_core::{Core, CoreError};
use kanso_mcp::jsonrpc::MSG_DISABLED;
use kanso_types::event::{Activity, SessionState};
use kanso_types::mcp::{McpAccess, McpAuditEntry};
use kanso_types::profile::{ProfileKind, SessionKind};
use kanso_types::{ProfileId, SessionId};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::Shared;
use super::gate::{ActivityGuard, GateDenied};
use super::identity::ClientIdentity;
use super::terminal::{
    self, READ_LINES_DEFAULT, READ_LINES_MAX, RUN_TIMEOUT_DEFAULT, RUN_TIMEOUT_MAX, SEND_MAX_ITEMS,
    SEND_MAX_TEXT_BYTES, SETTLE_MAX_MS,
};
use crate::{McpActivity, now_ms};

/// 审计里参数摘要的最大字符数：参数由 AI 给出，长度不可控，审计表不该被它撑大。
const ARGS_SUMMARY_MAX_CHARS: usize = 512;

/// 业务错误：作为工具结果（`isError`）返回给 AI，而不是协议错误，否则客户端看不到原因。
#[derive(Debug)]
pub(super) struct ToolError {
    /// spec/mcp.md §9 的错误码。
    pub code: &'static str,
    /// 英文说明。
    pub message: String,
    /// 错误时仍要带给 AI 的数据（例如超时时已有的输出），合并进结果 JSON 的顶层，与 `error` 并列。
    /// 审计只记结果的大小，不记这里的内容（spec/mcp.md §6：不记录终端输出）。
    pub details: Option<Map<String, Value>>,
}

impl ToolError {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> ToolError {
        ToolError {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub(super) fn with_details(
        code: &'static str,
        message: impl Into<String>,
        details: Map<String, Value>,
    ) -> ToolError {
        ToolError {
            code,
            message: message.into(),
            details: Some(details),
        }
    }
}

/// 一次调用的审计目标：由工具在解析出目标后填写，`invoke` 写审计时读取。
/// 用 `Mutex` 是因为工具的 future 与 `invoke` 同时持有它；锁只在赋值和读取的瞬间持有。
#[derive(Default)]
pub(super) struct Target(Mutex<(Option<ProfileId>, Option<SessionId>)>);

impl Target {
    fn set_session(&self, session: SessionId) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(session);
    }

    fn set_profile(&self, profile: ProfileId) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(profile);
    }

    fn get(&self) -> (Option<ProfileId>, Option<SessionId>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 解析标签时被拒的原因。
enum Denied {
    Disabled,
    /// 标签不存在、不是已保存会话的标签、或会话级别为 Off；三者对 AI 必须无法区分（spec/mcp.md §2.1）。
    NotFound,
    Insufficient,
}

/// 解析出的标签。
struct Resolved {
    status: McpSessionStatus,
    /// 是不是客户端所在的标签（spec/mcp.md §2.5）。
    own: bool,
}

/// 按**当前**的总开关与级别解析标签。同步读 core（会访问存储），调用方放进阻塞线程池。
/// 首次判断与中断时的复查共用它，保证两处的规则不会各改各的。
fn resolve_tab(
    core: &Core,
    identity: &ClientIdentity,
    session: SessionId,
    need: McpAccess,
    target: &Target,
) -> Result<Resolved, Denied> {
    let status = core.mcp_session_status(session).ok_or(Denied::NotFound)?;
    // 无界面会话不是标签；没有 profile 的是未保存的临时会话，不能授权（spec/mcp.md D3）。
    let profile = match (status.headless, status.info.profile) {
        (false, Some(profile)) => profile,
        _ => return Err(Denied::NotFound),
    };
    target.set_profile(profile);
    if !core.mcp_enabled() {
        return Err(Denied::Disabled);
    }
    let access = core.mcp_access(profile);
    if access == McpAccess::Off {
        return Err(Denied::NotFound);
    }
    if access < need {
        return Err(Denied::Insufficient);
    }
    let own = identity.is_own(&status);
    Ok(Resolved { status, own })
}

/// `kanso_get_cwd` 与 `kanso_terminal_read` 共用的参数：标签 id。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct TabParams {
    #[schemars(description = "Tab id, as returned by kanso_list_sessions.")]
    tab: u64,
}

/// 读取范围。
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ReadScope {
    Screen,
    Recent,
}

/// `kanso_terminal_read` 的参数。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ReadParams {
    #[schemars(description = "Tab id, as returned by kanso_list_sessions.")]
    tab: u64,
    #[schemars(
        description = "\"screen\" (default) returns the visible screen; \"recent\" returns the \
            last `lines` lines of scrollback plus screen."
    )]
    scope: Option<ReadScope>,
    #[schemars(
        description = "Number of lines for scope \"recent\". Default 200, clamped to 1-2000."
    )]
    lines: Option<u32>,
}

/// `kanso_terminal_run` 的参数。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct RunParams {
    #[schemars(description = "Tab id, as returned by kanso_list_sessions.")]
    tab: u64,
    #[schemars(
        description = "One command line, without a trailing newline. Line breaks are rejected."
    )]
    command: String,
    #[schemars(
        description = "Seconds to wait for the prompt to return. Default 30, clamped to 1-600."
    )]
    timeout_secs: Option<u64>,
}

/// `kanso_terminal_send` 的一项输入：`text` 与 `key` 二选一。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct SendItem {
    #[schemars(description = "Literal text to type (not a paste). Use either text or key.")]
    text: Option<String>,
    #[schemars(
        description = "One key: Enter, Tab, Esc, Backspace, Up, Down, Left, Right, Home, End, \
            PageUp, PageDown, or Ctrl+<letter> such as Ctrl+C. Use either text or key."
    )]
    key: Option<String>,
}

/// `kanso_terminal_send` 的参数。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct SendParams {
    #[schemars(description = "Tab id, as returned by kanso_list_sessions.")]
    tab: u64,
    #[schemars(description = "Inputs, sent in order.")]
    inputs: Vec<SendItem>,
    #[schemars(
        description = "If set, wait until the terminal output has been quiet for this many \
            milliseconds (clamped to 0-10000) and return the screen."
    )]
    settle_ms: Option<u64>,
}

/// `kanso_list_sessions` 的参数。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(super) struct ListSessionsParams {
    #[schemars(description = "Only sessions whose name contains this text (case-insensitive).")]
    name: Option<String>,
    #[schemars(description = "Only sessions in the group with exactly this name.")]
    group: Option<String>,
}

/// 一个连接对应的 MCP 处理器。
pub(super) struct KansoHandler {
    shared: Arc<Shared>,
    client_id: u64,
    /// 连接被取消（客户端断开、被用户断开、令牌更换、服务停止）时触发，等待中的写调用据此中断。
    cancel: CancellationToken,
    /// 客户端所在标签的识别线索（spec/mcp.md §2.5）。
    identity: Arc<ClientIdentity>,
    tool_router: ToolRouter<Self>,
}

impl KansoHandler {
    pub(super) fn new(
        shared: Arc<Shared>,
        client_id: u64,
        cancel: CancellationToken,
        identity: Arc<ClientIdentity>,
    ) -> KansoHandler {
        KansoHandler {
            shared,
            client_id,
            cancel,
            identity,
            tool_router: Self::tool_router(),
        }
    }

    /// 每次工具调用的统一入口：检查总开关、累加调用次数、执行业务逻辑、写审计、组装结果。
    /// 被拒绝的调用（含开关关闭）同样计数、同样审计（spec/mcp.md §6）。
    ///
    /// `work` 收到本次调用的 [`Target`]，解析出目标会话 / 标签后要填进去，审计的 `profile`、`session`
    /// 字段就取自它；被拒绝的调用也要填（spec/mcp.md §6：未授权的调用要能看出尝试的是哪个会话）。
    async fn invoke<F, Fut>(&self, tool: &'static str, args: String, work: F) -> CallToolResult
    where
        F: FnOnce(Arc<Target>) -> Fut,
        Fut: Future<Output = Result<Value, ToolError>>,
    {
        // 停止时 teardown 会等 `conns` 归零后才关闭审计队列。rmcp 把每个请求放在它自己 spawn 的任务里执行，
        // 不在连接任务之内，所以只等连接任务不够：这里登记一个令牌，调用结束（含写审计）后才释放。
        let _inflight = self.shared.conns.token();
        let started = Instant::now();
        let at_ms = now_ms();
        let client = self.shared.registry.add_call(self.client_id);
        // core 的查询会读 SQLite（同步阻塞）；MCP 运行时只有 2 个工作线程，直接在这里调用，
        // 几个并发连接就能占满线程，连接握手与取消都会被拖住，所以放进阻塞线程池。
        // 任务异常（panic）时按"开关关闭"处理：宁可拒绝也不放行。
        let core = self.shared.core.clone();
        let enabled = tokio::task::spawn_blocking(move || core.mcp_enabled())
            .await
            .unwrap_or(false);
        let target = Arc::new(Target::default());
        let outcome = if enabled {
            work(target.clone()).await
        } else {
            Err(ToolError::new("MCP_DISABLED", MSG_DISABLED))
        };
        let (text, error_code) = match &outcome {
            Ok(value) => (value.to_string(), None),
            Err(e) => {
                let mut body = e.details.clone().unwrap_or_default();
                body.insert(
                    "error".to_owned(),
                    json!({"code": e.code, "message": e.message}),
                );
                (Value::Object(body).to_string(), Some(e.code))
            }
        };
        let (profile, session) = target.get();
        self.shared.audit.push(McpAuditEntry {
            at_ms,
            client,
            mcp_session: self.client_id.to_string(),
            tool: tool.to_owned(),
            profile,
            session,
            args: args.chars().take(ARGS_SUMMARY_MAX_CHARS).collect(),
            error_code: error_code.map(str::to_owned),
            duration_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            bytes: text.len() as u64,
        });
        let content = vec![ContentBlock::text(text)];
        if error_code.is_some() {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
    }
}

impl KansoHandler {
    /// 把标签 id 解析成可用的标签：判断总开关与级别（每次调用都判断，spec/mcp.md §8），
    /// 并把目标记进审计。读 core 会访问存储，放进阻塞线程池。
    async fn resolve(
        &self,
        session: SessionId,
        need: McpAccess,
        target: &Arc<Target>,
    ) -> Result<Resolved, ToolError> {
        target.set_session(session);
        let core = self.shared.core.clone();
        let identity = self.identity.clone();
        let t = target.clone();
        let resolved =
            tokio::task::spawn_blocking(move || resolve_tab(&core, &identity, session, need, &t))
                .await
                .map_err(|e| {
                    tracing::warn!("resolving a tab failed: {e}");
                    internal_error()
                })?;
        let resolved = resolved.map_err(|denied| match denied {
            Denied::Disabled => ToolError::new("MCP_DISABLED", MSG_DISABLED),
            Denied::NotFound => ToolError::new(
                "NOT_FOUND",
                format!(
                    "No tab with id {}. Use kanso_list_sessions to see the tabs you can use.",
                    session.raw()
                ),
            ),
            Denied::Insufficient => ToolError::new(
                "ACCESS_DENIED",
                if need == McpAccess::Full {
                    "This operation needs full access, but the session is read-only. Ask the \
                     user to raise its MCP access in Kanso."
                } else {
                    "The session's MCP access level does not allow this operation."
                },
            ),
        })?;
        // 纯 SFTP 会话没有终端；级别通过之后才告诉 AI 类型，避免对没权限的会话泄露信息。
        if resolved.status.info.kind == SessionKind::Sftp {
            return Err(ToolError::new(
                "UNSUPPORTED",
                "This tab is an SFTP-only session and has no terminal.",
            ));
        }
        Ok(resolved)
    }

    /// 终端写操作的统一流程（spec/mcp.md §2.5、§4）：
    /// 级别 → 客户端所在标签 → 冷却与写锁 → 活动标识 → 等待 core，等待期间可被中断。
    ///
    /// - `call` 返回 core 的调用 future；它是取消安全的，被中断时直接丢弃即停止等待。
    /// - `took_over` 判断 core 的结果是不是"用户接管"；是的话在这里记冷却，调用方不用再记，
    ///   以免新增写工具时漏掉。
    /// - 先订阅授权变化、再做首次级别判断：反过来的话，两步之间的降级既没被首次判断看到、
    ///   也没被订阅收到，调用会照常执行。
    async fn write_op<T, Fut>(
        &self,
        session: SessionId,
        target: &Arc<Target>,
        request_ct: &CancellationToken,
        tool: &'static str,
        call: impl FnOnce() -> Fut,
        took_over: impl Fn(&T) -> bool,
    ) -> Result<T, ToolError>
    where
        Fut: Future<Output = Result<T, CoreError>>,
    {
        let mut policy = self.shared.core.mcp_policy_watch();
        let resolved = self.resolve(session, McpAccess::Full, target).await?;
        if resolved.own {
            return Err(ToolError::new(
                "TAB_SELF",
                "This tab is the one your own client runs in. Writing to it would disturb your own \
                 interface, so write operations are refused; reading it is allowed.",
            ));
        }
        // 声明顺序有意义：局部变量逆序释放，活动标识要先于写锁撤掉（见 `ActivityGuard`）。
        let _lease = self
            .shared
            .gate
            .acquire(session)
            .map_err(|denied| match denied {
                GateDenied::Busy => ToolError::new(
                    "TAB_BUSY",
                    "Another MCP call is operating this tab. Try again when it finishes.",
                ),
                GateDenied::Cooldown { remaining } => ToolError::new(
                    "USER_TOOK_OVER",
                    format!(
                        "The user recently took over this tab. Write operations are refused for \
                     another {} seconds.",
                        remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
                    ),
                ),
            })?;
        let _activity = ActivityGuard::start(
            self.shared.delegate.clone(),
            session,
            McpActivity {
                client_id: self.client_id,
                client_name: self.shared.registry.client_name(self.client_id),
                tool: tool.to_owned(),
            },
        );
        let outcome = self
            .interruptible(call(), &mut policy, session, request_ct)
            .await?
            .map_err(|e| core_error(&e, "INTERRUPTED"))?;
        if took_over(&outcome) {
            self.shared.gate.took_over(session);
        }
        Ok(outcome)
    }

    /// 等待 `fut`，同时监视会让调用失去意义的事件（spec/mcp.md §4.4）：
    /// 连接被取消（客户端断开、服务停止）、客户端发出取消通知、总开关或级别变化。
    /// 前两种是 `select!` 的直接分支；授权变化要重新读 core 判断是否仍被允许，不是每次变化都中断
    /// （别的会话的级别变了与这次调用无关）。
    async fn interruptible<T>(
        &self,
        fut: impl Future<Output = T>,
        policy: &mut watch::Receiver<u64>,
        session: SessionId,
        request_ct: &CancellationToken,
    ) -> Result<T, ToolError> {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                out = &mut fut => return Ok(out),
                () = self.cancel.cancelled() => {
                    return Err(ToolError::new("INTERRUPTED", "The MCP client disconnected."));
                }
                () = request_ct.cancelled() => {
                    return Err(ToolError::new("CANCELLED", "The call was cancelled by the client."));
                }
                changed = policy.changed() => {
                    if changed.is_err() {
                        return Err(ToolError::new("INTERRUPTED", "Kanso is shutting down."));
                    }
                    // 复查期间 `fut` 暂停被轮询；复查只读本地存储，很短，且 core 的等待以截止时间计，
                    // 不会因此多等。
                    if let Some(reason) = self.policy_violation(session).await {
                        return Err(ToolError::new("INTERRUPTED", reason));
                    }
                }
            }
        }
    }

    /// 授权变化后，这次写调用是否已不被允许；是则返回给 AI 的原因。
    /// 复查失败（任务 panic）按"不允许"处理：宁可中断也不放行。
    async fn policy_violation(&self, session: SessionId) -> Option<&'static str> {
        let core = self.shared.core.clone();
        let identity = self.identity.clone();
        let denied = tokio::task::spawn_blocking(move || {
            resolve_tab(
                &core,
                &identity,
                session,
                McpAccess::Full,
                &Target::default(),
            )
            .err()
        })
        .await;
        match denied {
            Ok(None) => None,
            Ok(Some(Denied::Disabled)) => Some("MCP was turned off in Kanso settings."),
            Ok(Some(Denied::NotFound)) => {
                Some("The tab was closed, or its session is no longer available to MCP.")
            }
            Ok(Some(Denied::Insufficient)) => {
                Some("The session's MCP access was lowered below full access.")
            }
            Err(_) => Some("The access check failed."),
        }
    }
}

fn internal_error() -> ToolError {
    ToolError::new(
        "INTERRUPTED",
        "The call was interrupted by an internal error.",
    )
}

/// 把 core 的错误映射成错误码。`gone_code` 是"标签已不存在"时用的码：读取类调用用 `NOT_FOUND`，
/// 已经开始执行的写调用用 `INTERRUPTED`（标签在调用期间被关闭，spec/mcp.md §4.4）。
fn core_error(e: &CoreError, gone_code: &'static str) -> ToolError {
    match e {
        CoreError::NoSuchSession(_) => {
            ToolError::new(gone_code, "The tab no longer exists (it was closed).")
        }
        CoreError::NotSupported => {
            ToolError::new("UNSUPPORTED", "This tab does not support the operation.")
        }
        CoreError::User(m) => match m.key() {
            "cwd-timeout" => ToolError::new(
                "TIMEOUT",
                "Looking up the working directory timed out. Shells that report OSC 7 always work.",
            ),
            "session-not-terminal" => ToolError::new(
                "UNSUPPORTED",
                "This tab is an SFTP-only session and has no terminal.",
            ),
            "cwd-no-shell" => ToolError::new(
                "UNSUPPORTED",
                "No shell process was found for this tab, so its working directory cannot be \
                 determined. Shells that report OSC 7 always work.",
            ),
            "cwd-no-ssh-connection" | "cwd-no-proc" | "cwd-other-user" | "cwd-unavailable" => {
                ToolError::new(
                    "UNSUPPORTED",
                    format!(
                        "The working directory could not be determined ({}). Shells that report \
                         OSC 7 always work.",
                        m.key()
                    ),
                )
            }
            key => ToolError::new(
                "INTERRUPTED",
                format!("The operation failed or the session ended ({key})."),
            ),
        },
        other => {
            tracing::warn!("mcp core call failed: {other}");
            internal_error()
        }
    }
}

#[tool_router]
impl KansoHandler {
    #[tool(
        name = "kanso_list_sessions",
        description = "List the saved sessions Kanso lets AI tools see (access level is not Off), \
            with their open tabs. Each session has id, name, kind (local/ssh/sftp), host, port, user, \
            group and access (read-only/full). Each tab has id, title, state, opened_by_mcp and \
            is_client_tab (plus activity, locked and terminal_transfer when known). \
            is_client_tab marks the tab your own client runs in: terminal writes to it are \
            refused (TAB_SELF), reads are allowed. opened_by_mcp is always false for now.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn kanso_list_sessions(
        &self,
        Parameters(params): Parameters<ListSessionsParams>,
    ) -> CallToolResult {
        let args = format!(
            "name={}, group={}",
            params.name.as_deref().unwrap_or(""),
            params.group.as_deref().unwrap_or("")
        );
        let core = self.shared.core.clone();
        let identity = self.identity.clone();
        self.invoke("kanso_list_sessions", args, |_target| async move {
            // 同 `invoke`：整个枚举都是同步读库，放进阻塞线程池，避免占住只有 2 个线程的运行时。
            tokio::task::spawn_blocking(move || list_sessions(&core, &params, &identity))
                .await
                .map_err(|e| {
                    tracing::warn!("kanso_list_sessions task failed: {e}");
                    internal_error()
                })?
        })
        .await
    }

    #[tool(
        name = "kanso_get_cwd",
        description = "Get the current working directory of a tab (uses OSC 7 when the shell \
            reports it, otherwise asks the server or the OS). Needs at least read-only access.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn kanso_get_cwd(&self, Parameters(params): Parameters<TabParams>) -> CallToolResult {
        let session = SessionId::from_raw(params.tab);
        self.invoke(
            "kanso_get_cwd",
            format!("tab={}", params.tab),
            |target| async move {
                self.resolve(session, McpAccess::ReadOnly, &target).await?;
                let cwd = self
                    .shared
                    .core
                    .working_directory(session)
                    .await
                    .map_err(|e| core_error(&e, "NOT_FOUND"))?;
                Ok(json!({ "cwd": cwd }))
            },
        )
        .await
    }

    #[tool(
        name = "kanso_terminal_read",
        description = "Read a tab's terminal text: the visible screen, or the last N lines \
            including scrollback. Returns text, cursor position, screen size and whether the \
            alternate screen (a full-screen TUI) is active. Text is capped at 256 KiB (the tail \
            is kept) and the result says so when truncated. Needs at least read-only access; \
            also allowed on the tab your own client runs in.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn kanso_terminal_read(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> CallToolResult {
        let session = SessionId::from_raw(params.tab);
        let scope = match (params.scope, params.lines) {
            (Some(ReadScope::Screen), _) | (None, None) => TextScope::Screen,
            (Some(ReadScope::Recent) | None, lines) => TextScope::Recent {
                lines: lines.unwrap_or(READ_LINES_DEFAULT).clamp(1, READ_LINES_MAX),
            },
        };
        let args = format!("tab={}, scope={scope:?}", params.tab);
        self.invoke("kanso_terminal_read", args, |target| async move {
            self.resolve(session, McpAccess::ReadOnly, &target).await?;
            let core = self.shared.core.clone();
            // 读终端要持终端锁、拷贝最多 2000 行，放进阻塞线程池。
            let text = tokio::task::spawn_blocking(move || {
                core.terminal_text(session, scope, MCP_TEXT_LIMIT)
            })
            .await
            .map_err(|e| {
                tracing::warn!("kanso_terminal_read task failed: {e}");
                internal_error()
            })?
            .map_err(|e| core_error(&e, "NOT_FOUND"))?;
            Ok(terminal::text_json(&text))
        })
        .await
    }

    #[tool(
        name = "kanso_terminal_run",
        description = "Type one command line into a tab, press Enter, wait for the prompt to \
            return and return the command's output and exit code (exit_code is \"unknown\" when \
            the shell has no OSC 133 integration). The tab must be idle at an empty shell prompt \
            (otherwise TAB_BUSY). On timeout (TIMEOUT) the command is left running, no Ctrl+C \
            is sent, and the output so far is still returned. If the user types in the tab the \
            call ends with USER_TOOK_OVER. Needs full access; refused on the tab your own client \
            runs in (TAB_SELF).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn kanso_terminal_run(
        &self,
        Parameters(params): Parameters<RunParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let session = SessionId::from_raw(params.tab);
        let timeout_secs =
            terminal::clamp_or(params.timeout_secs, RUN_TIMEOUT_DEFAULT, 1, RUN_TIMEOUT_MAX);
        // 命令文本进审计（spec/mcp.md §6）。
        let args = format!(
            "tab={}, timeout_secs={timeout_secs}, command={}",
            params.tab, params.command
        );
        self.invoke("kanso_terminal_run", args, |target| async move {
            if params.command.contains(['\n', '\r']) {
                target.set_session(session);
                return Err(terminal::invalid(
                    "The command contains a line break. kanso_terminal_run takes a single line; \
                     run multi-line scripts one command at a time.",
                ));
            }
            let limits = WaitLimits {
                timeout: Duration::from_secs(timeout_secs),
                max_bytes: MCP_TEXT_LIMIT,
            };
            let core = self.shared.core.clone();
            let outcome = self
                .write_op(
                    session,
                    &target,
                    &context.ct,
                    "kanso_terminal_run",
                    || core.mcp_terminal_run(session, &params.command, limits),
                    |o| matches!(o, kanso_core::mcp::TerminalRunOutcome::TakenOver { .. }),
                )
                .await?;
            terminal::run_result(outcome, timeout_secs)
        })
        .await
    }

    #[tool(
        name = "kanso_terminal_send",
        description = "Send text and special keys to a tab, for REPLs, full-screen TUIs and \
            confirmation prompts. Each input is {\"text\": ...} or {\"key\": ...} (Enter, Tab, \
            Esc, Backspace, Up, Down, Left, Right, Home, End, PageUp, PageDown, Ctrl+<letter>). \
            Keys are encoded like the user's own key presses. With settle_ms the call waits \
            until output has been quiet that long and returns the screen. Needs full access; \
            refused on the tab your own client runs in (TAB_SELF).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn kanso_terminal_send(
        &self,
        Parameters(params): Parameters<SendParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let session = SessionId::from_raw(params.tab);
        let settle = params
            .settle_ms
            .map(|ms| Duration::from_millis(ms.min(SETTLE_MAX_MS)));
        // 文字原样进审计，与命令文本同级别敏感（spec/mcp.md §6）。
        let summary: Vec<String> = params
            .inputs
            .iter()
            .map(|i| match (&i.text, &i.key) {
                (Some(t), None) => format!("text:{t:?}"),
                (None, Some(k)) => format!("key:{k}"),
                _ => "invalid".to_owned(),
            })
            .collect();
        let args = format!(
            "tab={}, settle_ms={}, inputs=[{}]",
            params.tab,
            settle.map_or_else(|| "none".to_owned(), |d| d.as_millis().to_string()),
            summary.join(", ")
        );
        self.invoke("kanso_terminal_send", args, |target| async move {
            target.set_session(session);
            if params.inputs.is_empty() {
                return Err(terminal::invalid("inputs must not be empty."));
            }
            let text_bytes: usize = params
                .inputs
                .iter()
                .filter_map(|i| i.text.as_deref())
                .map(str::len)
                .sum();
            if params.inputs.len() > SEND_MAX_ITEMS || text_bytes > SEND_MAX_TEXT_BYTES {
                return Err(ToolError::new(
                    "TOO_LARGE",
                    format!(
                        "At most {SEND_MAX_ITEMS} inputs and {SEND_MAX_TEXT_BYTES} bytes of text \
                         per call."
                    ),
                ));
            }
            let inputs = params
                .inputs
                .iter()
                .map(|i| terminal::parse_input(i.text.as_deref(), i.key.as_deref()))
                .collect::<Result<Vec<_>, _>>()?;
            let core = self.shared.core.clone();
            let outcome = self
                .write_op(
                    session,
                    &target,
                    &context.ct,
                    "kanso_terminal_send",
                    || core.mcp_terminal_send(session, inputs, settle, MCP_TEXT_LIMIT),
                    |o| matches!(o, kanso_core::mcp::TerminalSendOutcome::TakenOver { .. }),
                )
                .await?;
            terminal::send_result(outcome)
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KansoHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("kanso", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Kanso is a terminal app. These tools let you see and (where the user allows) \
                 use the user's saved terminal sessions. Every session needs the user's \
                 authorization in Kanso: sessions whose access is Off are invisible. When a \
                 call returns NEEDS_USER, ask the user to open that session in Kanso first.",
            )
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // 名称只用于设置页显示与审计，客户端自报、不可信。
        self.shared
            .registry
            .set_name(self.client_id, request.client_info.name.clone());
        // 与默认实现一致：先记下对端信息，再协商协议版本。
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }
}

/// `kanso_list_sessions`：只列访问级别不是 Off 的已保存会话，绝不带任何凭据相关字段（spec/mcp.md §2.4）。
///
/// 读不到授权时返回错误而不是空列表：把"未知"当成"全部 Off"会让 AI 以为用户没有授权任何会话。
/// spec §9 没有"授权存储不可读"的专用错误码，沿用 `INTERRUPTED`（其他中断），说明里写明原因。
fn list_sessions(
    core: &Core,
    params: &ListSessionsParams,
    identity: &ClientIdentity,
) -> Result<Value, ToolError> {
    let accesses: HashMap<_, _> = core
        .mcp_accesses()
        .map_err(|e| {
            tracing::warn!("kanso_list_sessions: failed to read access levels: {e}");
            ToolError::new(
                "INTERRUPTED",
                "Could not read session access levels; no sessions are listed.",
            )
        })?
        .into_iter()
        .collect();
    let groups: HashMap<_, _> = core.groups().into_iter().map(|g| (g.id, g.name)).collect();
    let tabs = core.sessions();
    let name_filter = params.name.as_deref().map(str::to_lowercase);

    let mut out = Vec::new();
    for profile in core.profiles() {
        let access = accesses.get(&profile.id).copied().unwrap_or(McpAccess::Off);
        if access == McpAccess::Off {
            continue;
        }
        let group = profile.group.and_then(|g| groups.get(&g)).cloned();
        if let Some(filter) = &name_filter
            && !profile.name.to_lowercase().contains(filter)
        {
            continue;
        }
        if let Some(wanted) = &params.group
            && group.as_deref() != Some(wanted.as_str())
        {
            continue;
        }
        let mut item = json!({
            "id": profile.id.to_string(),
            "name": profile.name,
            "kind": match profile.kind.kind() {
                SessionKind::Local => "local",
                SessionKind::Ssh => "ssh",
                SessionKind::Sftp => "sftp",
            },
            "group": group,
            "access": if access == McpAccess::Full { "full" } else { "read-only" },
        });
        let target = match &profile.kind {
            ProfileKind::Ssh(p) => Some(&p.target),
            ProfileKind::Sftp(t) => Some(t),
            ProfileKind::Local(_) => None,
        };
        if let Some(t) = target {
            item["host"] = json!(t.host);
            item["port"] = json!(t.port);
            item["user"] = json!(t.user);
        }
        let tab_items: Vec<Value> = tabs
            .iter()
            .filter(|s| s.profile == Some(profile.id))
            .map(|s| tab_json(core, s, identity))
            .collect();
        item["tabs"] = Value::Array(tab_items);
        out.push(item);
    }
    Ok(json!({ "sessions": out }))
}

fn tab_json(core: &Core, info: &kanso_core::SessionInfo, identity: &ClientIdentity) -> Value {
    let mut tab = json!({
        "id": info.id.raw(),
        "title": info.name,
        "state": match info.state {
            SessionState::Connecting => "connecting",
            SessionState::Connected => "connected",
            SessionState::Closed(_) => "closed",
        },
        // TODO(F9 阶段 3)：还没有 `kanso_open_tab`，不存在由 MCP 打开的标签，固定 false。
        "opened_by_mcp": false,
        "is_client_tab": false,
    });
    // 真实后端暂时没有这项状态（返回 None）：缺失就省略，`is_client_tab` 保持 false，不编造。
    if let Some(status) = core.mcp_session_status(info.id) {
        tab["is_client_tab"] = json!(identity.is_own(&status));
        tab["activity"] = json!(match status.activity {
            Activity::Idle => "idle",
            Activity::CommandRunning => "command-running",
            Activity::AgentIdle(_) | Activity::AgentWorking(_) => "agent-running",
        });
        tab["locked"] = json!(status.locked);
        tab["terminal_transfer"] = json!(status.terminal_transfer);
    }
    tab
}

#[cfg(test)]
mod tests {
    //! 写调用的流程测试：写锁、冷却、客户端所在标签、活动标识成对、各种中断。
    //!
    //! mock core 的 `mcp_terminal_run` 立即返回，无法构造"一直在等"的场景，所以这里直接驱动
    //! `write_op` 并传入永不完成的 future；经 socket 的端到端用例在 `tests/real_server.rs`。

    use std::future::pending;
    use std::sync::Mutex;
    use std::time::Instant;

    use kanso_core::mcp::TerminalRunOutcome;
    use kanso_core::{CollectingEventSink, OpenTarget, Platform};
    use kanso_types::GridSize;

    use super::*;
    use crate::real::audit;
    use crate::real::gate::{ClockFn, TabGate};
    use crate::real::registry::Registry;
    use crate::{McpClientInfo, McpServerDelegate, McpServerStatus};

    /// 记录 `on_activity` 的 delegate。
    #[derive(Default)]
    struct Activities(Mutex<Vec<(u64, bool)>>);

    impl McpServerDelegate for Activities {
        fn on_status_changed(&self, _: McpServerStatus) {}
        fn on_clients_changed(&self, _: Vec<McpClientInfo>) {}
        fn on_session_opened(&self, _: SessionId, _: McpClientInfo) {}
        fn on_activity(&self, session: SessionId, activity: Option<McpActivity>) {
            self.0
                .lock()
                .unwrap()
                .push((session.raw(), activity.is_some()));
        }
        fn on_session_released(&self, _: SessionId) {}
        fn on_audit_changed(&self) {}
    }

    struct Env {
        core: Arc<Core>,
        activities: Arc<Activities>,
        handler: Arc<KansoHandler>,
        cancel: CancellationToken,
        now: Arc<Mutex<Instant>>,
        web: ProfileId,
        tab: SessionId,
    }

    fn profile(core: &Core, name: &str) -> ProfileId {
        core.profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
            .id
    }

    fn env(identity: ClientIdentity) -> Env {
        let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
            CollectingEventSink::new(),
        ))));
        core.set_mcp_enabled(true).unwrap();
        let web = profile(&core, "web-01");
        core.set_mcp_access(web, McpAccess::Full).unwrap();
        let tab = core
            .open_session(OpenTarget::Profile(web), GridSize { cols: 80, rows: 24 })
            .unwrap();
        let activities = Arc::new(Activities::default());
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock: ClockFn = {
            let now = now.clone();
            Arc::new(move || *now.lock().unwrap())
        };
        let (audit, _rx) = audit::channel();
        let shared = Arc::new(Shared {
            core: core.clone(),
            delegate: activities.clone(),
            registry: Arc::new(Registry::new(activities.clone())),
            gate: TabGate::new(clock),
            ancestors: Arc::new(|_| Vec::new()),
            audit,
            conns: tokio_util::task::TaskTracker::new(),
        });
        let cancel = CancellationToken::new();
        let handler = Arc::new(KansoHandler::new(
            shared,
            1,
            cancel.clone(),
            Arc::new(identity),
        ));
        Env {
            core,
            activities,
            handler,
            cancel,
            now,
            web,
            tab,
        }
    }

    type Op = Result<(), CoreError>;

    impl Env {
        /// 执行一次写调用，`fut` 为 core 调用的替身。
        async fn write<T, Fut>(
            &self,
            request_ct: &CancellationToken,
            call: impl FnOnce() -> Fut,
            took_over: impl Fn(&T) -> bool,
        ) -> Result<T, ToolError>
        where
            Fut: Future<Output = Result<T, CoreError>>,
        {
            let target = Arc::new(Target::default());
            self.handler
                .write_op(
                    self.tab,
                    &target,
                    request_ct,
                    "kanso_terminal_run",
                    call,
                    took_over,
                )
                .await
        }

        /// 在后台启动一个一直等待的写调用，等到它登记了活动标识（说明已拿到写锁、进入等待）才返回。
        async fn start_hanging(&self) -> tokio::task::JoinHandle<Result<(), ToolError>> {
            let handler = self.handler.clone();
            let tab = self.tab;
            let task = tokio::spawn(async move {
                let target = Arc::new(Target::default());
                handler
                    .write_op(
                        tab,
                        &target,
                        &CancellationToken::new(),
                        "kanso_terminal_run",
                        pending::<Op>,
                        |()| false,
                    )
                    .await
            });
            self.wait_activity_count(1).await;
            task
        }

        async fn wait_activity_count(&self, n: usize) {
            for _ in 0..500 {
                if self.activities.0.lock().unwrap().len() >= n {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("timeout waiting for activity callbacks");
        }

        fn activity_log(&self) -> Vec<bool> {
            self.activities
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.1)
                .collect()
        }
    }

    fn ready_ok() -> impl FnOnce() -> std::future::Ready<Op> {
        || std::future::ready(Ok(()))
    }

    fn no_takeover(_: &()) -> bool {
        false
    }

    fn code<T>(r: Result<T, ToolError>) -> &'static str {
        match r {
            Ok(_) => "ok",
            Err(e) => e.code,
        }
    }

    #[tokio::test]
    async fn success_reports_activity_start_and_end() {
        let e = env(ClientIdentity::default());
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "ok");
        assert_eq!(e.activity_log(), [true, false]);
    }

    #[tokio::test]
    async fn second_writer_is_busy_and_lock_is_released_on_cancel() {
        let e = env(ClientIdentity::default());
        let first = e.start_hanging().await;
        // 另一个客户端对同一标签写：立即 TAB_BUSY，且不应产生活动回调。
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "TAB_BUSY");
        assert_eq!(e.activity_log(), [true]);
        // 取消第一个调用（等同客户端断开 / 请求被丢弃）：锁与活动标识都要释放。
        first.abort();
        let _ = first.await;
        assert_eq!(e.activity_log(), [true, false]);
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "ok");
        assert_eq!(e.activity_log(), [true, false, true, false]);
    }

    #[tokio::test]
    async fn takeover_starts_cooldown_that_expires() {
        let e = env(ClientIdentity::default());
        let r = e
            .write(
                &CancellationToken::new(),
                || {
                    std::future::ready(Ok::<_, CoreError>(TerminalRunOutcome::TakenOver {
                        output: Default::default(),
                    }))
                },
                |o| matches!(o, TerminalRunOutcome::TakenOver { .. }),
            )
            .await;
        assert_eq!(code(r), "ok", "write_op itself returns the outcome");
        *e.now.lock().unwrap() += std::time::Duration::from_secs(9);
        let err = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await
            .err()
            .unwrap();
        assert_eq!(err.code, "USER_TOOK_OVER");
        assert!(err.message.contains("1 seconds"), "{}", err.message);
        // 冷却被拒的调用没有开始操作，不应产生活动回调。
        assert_eq!(e.activity_log(), [true, false]);
        *e.now.lock().unwrap() += std::time::Duration::from_secs(1);
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "ok");
    }

    #[tokio::test]
    async fn level_checks() {
        let e = env(ClientIdentity::default());
        e.core.set_mcp_access(e.web, McpAccess::ReadOnly).unwrap();
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "ACCESS_DENIED");
        // 关闭级别必须与"不存在"无法区分。
        e.core.set_mcp_access(e.web, McpAccess::Off).unwrap();
        let off = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await
            .err()
            .unwrap();
        assert_eq!(off.code, "NOT_FOUND");
        let target = Arc::new(Target::default());
        let missing = e
            .handler
            .write_op(
                SessionId::from_raw(987_654),
                &target,
                &CancellationToken::new(),
                "kanso_terminal_run",
                ready_ok(),
                no_takeover,
            )
            .await
            .err()
            .unwrap();
        assert_eq!(missing.code, "NOT_FOUND");
        assert_eq!(
            off.message.replace(&e.tab.raw().to_string(), "N"),
            missing.message.replace("987654", "N")
        );
        assert_eq!(e.activity_log(), Vec::<bool>::new());
    }

    #[tokio::test]
    async fn own_tab_is_refused() {
        let e = env(ClientIdentity::default());
        // 用 mock 的 Local 配置开一个本地标签，再让客户端身份指向它的 shell。
        let local = profile(&e.core, "Local");
        e.core.set_mcp_access(local, McpAccess::Full).unwrap();
        let tab = e
            .core
            .open_session(OpenTarget::Profile(local), GridSize { cols: 80, rows: 24 })
            .unwrap();
        let status = e.core.mcp_session_status(tab).unwrap();
        let own = ClientIdentity {
            tty: None,
            ancestors: vec![1234, status.shell_pid.unwrap()],
        };
        let e2 = env_with_core(&e, own);
        let target = Arc::new(Target::default());
        let r = e2
            .write_op(
                tab,
                &target,
                &CancellationToken::new(),
                "kanso_terminal_run",
                ready_ok(),
                no_takeover,
            )
            .await;
        assert_eq!(code(r), "TAB_SELF");
        // 别的标签不受影响。
        let r = e2
            .write_op(
                e.tab,
                &target,
                &CancellationToken::new(),
                "kanso_terminal_run",
                ready_ok(),
                no_takeover,
            )
            .await;
        assert_eq!(code(r), "ok");
    }

    /// 复用 `e` 的 core，换一个客户端身份重建 handler。
    fn env_with_core(e: &Env, identity: ClientIdentity) -> Arc<KansoHandler> {
        let shared = e.handler.shared.clone();
        Arc::new(KansoHandler::new(
            shared,
            2,
            CancellationToken::new(),
            Arc::new(identity),
        ))
    }

    #[tokio::test]
    async fn switch_off_interrupts_a_waiting_call() {
        let e = env(ClientIdentity::default());
        let task = e.start_hanging().await;
        e.core.set_mcp_enabled(false).unwrap();
        let err = task.await.unwrap().err().unwrap();
        assert_eq!(err.code, "INTERRUPTED");
        assert!(err.message.contains("turned off"));
        assert_eq!(e.activity_log(), [true, false]);
    }

    #[tokio::test]
    async fn downgrade_interrupts_and_later_writes_are_denied() {
        let e = env(ClientIdentity::default());
        let task = e.start_hanging().await;
        // 别的会话的级别变化与这次调用无关，不能中断它。
        let other = profile(&e.core, "legacy-switch");
        e.core.set_mcp_access(other, McpAccess::Full).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!task.is_finished());
        e.core.set_mcp_access(e.web, McpAccess::ReadOnly).unwrap();
        let err = task.await.unwrap().err().unwrap();
        assert_eq!(err.code, "INTERRUPTED");
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "ACCESS_DENIED");
        assert_eq!(e.activity_log(), [true, false]);
    }

    #[tokio::test]
    async fn client_disconnect_and_request_cancel_end_the_wait() {
        let e = env(ClientIdentity::default());
        let task = e.start_hanging().await;
        e.cancel.cancel();
        assert_eq!(task.await.unwrap().err().unwrap().code, "INTERRUPTED");

        let e = env(ClientIdentity::default());
        let ct = CancellationToken::new();
        let ct2 = ct.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            ct2.cancel();
        });
        let r = e.write(&ct, pending::<Op>, no_takeover).await;
        assert_eq!(code(r), "CANCELLED");
        canceller.await.unwrap();
        // 中断后锁已释放。
        let r = e
            .write(&CancellationToken::new(), ready_ok(), no_takeover)
            .await;
        assert_eq!(code(r), "ok");
    }

    #[tokio::test]
    async fn core_error_during_the_call_is_an_interruption() {
        let e = env(ClientIdentity::default());
        let r = e
            .write(
                &CancellationToken::new(),
                || std::future::ready(Err::<(), _>(CoreError::NoSuchSession(e.tab))),
                no_takeover,
            )
            .await;
        assert_eq!(code(r), "INTERRUPTED");
        assert_eq!(e.activity_log(), [true, false]);
    }
}
