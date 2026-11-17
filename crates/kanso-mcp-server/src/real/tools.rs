//! rmcp 处理器与工具（spec/mcp.md §3、§5、§9）。
//!
//! 每个连接一个 [`KansoHandler`]，持有共享上下文和自己的客户端编号。工具方法只写业务逻辑，
//! 开关检查、调用计数、审计、结果格式统一在 [`KansoHandler::invoke`] 里，新增工具时不要绕开它。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use kanso_core::Core;
use kanso_mcp::jsonrpc::MSG_DISABLED;
use kanso_types::event::{Activity, SessionState};
use kanso_types::mcp::{McpAccess, McpAuditEntry};
use kanso_types::profile::{ProfileKind, SessionKind};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Shared;
use crate::now_ms;

/// 审计里参数摘要的最大字符数：参数由 AI 给出，长度不可控，审计表不该被它撑大。
const ARGS_SUMMARY_MAX_CHARS: usize = 512;

/// 业务错误：作为工具结果（`isError`）返回给 AI，而不是协议错误，否则客户端看不到原因。
pub(super) struct ToolError {
    /// spec/mcp.md §9 的错误码。
    pub code: &'static str,
    /// 英文说明。
    pub message: String,
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
    tool_router: ToolRouter<Self>,
}

impl KansoHandler {
    pub(super) fn new(shared: Arc<Shared>, client_id: u64) -> KansoHandler {
        KansoHandler {
            shared,
            client_id,
            tool_router: Self::tool_router(),
        }
    }

    /// 每次工具调用的统一入口：检查总开关、累加调用次数、执行业务逻辑、写审计、组装结果。
    /// 被拒绝的调用（含开关关闭）同样计数、同样审计（spec/mcp.md §6）。
    async fn invoke<F>(&self, tool: &'static str, args: String, work: F) -> CallToolResult
    where
        F: Future<Output = Result<Value, ToolError>>,
    {
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
        let outcome = if enabled {
            work.await
        } else {
            Err(ToolError {
                code: "MCP_DISABLED",
                message: MSG_DISABLED.to_owned(),
            })
        };
        let (text, error_code) = match &outcome {
            Ok(value) => (value.to_string(), None),
            Err(e) => (
                json!({"error": {"code": e.code, "message": e.message}}).to_string(),
                Some(e.code),
            ),
        };
        self.shared.audit.push(McpAuditEntry {
            at_ms,
            client,
            mcp_session: self.client_id.to_string(),
            tool: tool.to_owned(),
            profile: None,
            session: None,
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

#[tool_router]
impl KansoHandler {
    #[tool(
        name = "kanso_list_sessions",
        description = "List the saved sessions Kanso lets AI tools see (access level is not Off), \
            with their open tabs. Each session has id, name, kind (local/ssh/sftp), host, port, user, \
            group and access (read-only/full). Each tab has id, title, state, opened_by_mcp and \
            is_client_tab (plus activity, locked and terminal_transfer when known). \
            opened_by_mcp is always false for now, and is_client_tab is currently always false: \
            Kanso cannot yet tell which tab the calling AI client runs in.",
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
        self.invoke("kanso_list_sessions", args, async move {
            // 同 `invoke`：整个枚举都是同步读库，放进阻塞线程池，避免占住只有 2 个线程的运行时。
            tokio::task::spawn_blocking(move || list_sessions(&core, &params))
                .await
                .map_err(|e| {
                    tracing::warn!("kanso_list_sessions task failed: {e}");
                    ToolError {
                        code: "INTERRUPTED",
                        message: "Listing sessions was interrupted by an internal error."
                            .to_owned(),
                    }
                })
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
fn list_sessions(core: &Core, params: &ListSessionsParams) -> Value {
    let accesses: HashMap<_, _> = core.mcp_accesses().into_iter().collect();
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
            .map(|s| tab_json(core, s))
            .collect();
        item["tabs"] = Value::Array(tab_items);
        out.push(item);
    }
    json!({ "sessions": out })
}

fn tab_json(core: &Core, info: &kanso_core::SessionInfo) -> Value {
    let mut tab = json!({
        "id": info.id.raw(),
        "title": info.name,
        "state": match info.state {
            SessionState::Connecting => "connecting",
            SessionState::Connected => "connected",
            SessionState::Closed(_) => "closed",
        },
        // TODO(F9 阶段 2)：阶段 1 没有 `kanso_open_tab`，不存在由 MCP 打开的标签，固定 false；
        // 客户端所在标签（控制终端与进程祖先链，spec/mcp.md §2.5）要阶段 2 才能判断，暂时固定 false。
        // 字段先留着，是为了让 AI 工具现在就能按 spec §3.1 的结构解析。
        "opened_by_mcp": false,
        "is_client_tab": false,
    });
    // 真实后端暂时没有这项状态（返回 None）：缺失就省略，不编造。
    if let Some(status) = core.mcp_session_status(info.id) {
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
