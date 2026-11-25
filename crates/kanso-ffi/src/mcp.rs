//! MCP 的 FFI 导出（spec/mcp.md，architecture §16）。
//!
//! - `KansoCore` 上的授权、总开关、审计读写：设置 → MCP 页面与会话编辑对话框使用。
//! - [`KansoMcpServer`]：服务对象。宿主在总开关打开时 `start`、关闭时 `stop`，
//!   通过 [`FfiMcpServerDelegate`] 接收状态、客户端、标签摆放与 MCP 标识的回调。
//! - [`mcp_client_config`]：生成"复制 Claude Code / Codex 配置"的命令。
//!
//! **三个渠道都导出这些接口**：Swift 绑定只生成一份、三个渠道共用，按渠道裁剪导出会让 MAS 版
//! 链接失败。非 Direct 渠道里 [`mcp_supported`] 为假、服务恒为"不支持"。界面**只按 `mcp_supported`**
//! 决定是否显示 MCP 页面（spec/mcp.md §11），不看 `start` 的结果或状态。
//!
//! 服务的终端、执行、SFTP 等 MCP 工具只在 Rust 内部使用（`kanso-mcp-server` 直接调 `kanso_core::Core`），
//! 不导出到 Swift。

use std::sync::Arc;

use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpClientKind, McpServer, McpServerConfig, McpServerDelegate,
    McpServerError, McpServerStatus, McpUnavailable,
};
use kanso_types::mcp::{McpAccess, McpAuditDispatch, McpAuditFilter, McpAuditRecord};
use kanso_types::secret::SecretString;
use kanso_types::{ProfileId, SessionId};

use crate::core_object::KansoCore;
use crate::ffi_types::{FfiCoreError, mirror_enum, parse_id};

// ---- 授权与审计 ----

mirror_enum! {
    /// 会话的 MCP 访问级别（镜像 `kanso_types::mcp::McpAccess`）。
    FfiMcpAccess <=> McpAccess {
        /// 关闭：MCP 看不到这个会话。
        Off,
        /// 只读：只复用已有连接，不新建连接。
        ReadOnly,
        /// 完全：允许写操作，可在不提问的前提下新建连接。
        Full,
    }
}

/// 一条不是"关闭"的授权（设置页列表）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMcpGrant {
    /// 会话配置 id（小写连字符 UUID）。
    pub profile_id: String,
    /// 访问级别。
    pub level: FfiMcpAccess,
}

/// 审计日志查询条件（镜像 `McpAuditFilter`）。结果按 id 倒序。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMcpAuditFilter {
    /// 只看名称包含此文本的客户端，忽略大小写。
    pub client: Option<String>,
    /// 只看某个会话配置。
    pub profile_id: Option<String>,
    /// 只看失败或被拒绝的调用。
    pub errors_only: bool,
    /// 分页：只返回 id 小于它的记录；第一页为空。
    pub before_id: Option<i64>,
    /// 最多返回条数；0 按默认 100，上限 1000。
    pub limit: u32,
}

impl TryFrom<FfiMcpAuditFilter> for McpAuditFilter {
    type Error = FfiCoreError;

    fn try_from(f: FfiMcpAuditFilter) -> Result<Self, FfiCoreError> {
        Ok(McpAuditFilter {
            client: f.client,
            profile: f
                .profile_id
                .map(|p| parse_id::<ProfileId>(&p, "profile"))
                .transpose()?,
            errors_only: f.errors_only,
            before_id: f.before_id,
            limit: f.limit,
        })
    }
}

mirror_enum! {
    /// 审计记录中输入的发送状态。
    FfiMcpAuditDispatch <=> McpAuditDispatch {
        /// 在发送前被拒绝。
        Rejected,
        /// 未能确认是否发出。
        Unconfirmed,
        /// 输入已发出。
        Submitted,
    }
}

/// 一条审计记录（扁平化的 `McpAuditRecord`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMcpAuditRecord {
    /// 自增 id，分页游标。
    pub id: i64,
    /// 调用开始时间，Unix 毫秒。
    pub at_ms: i64,
    /// 客户端自报名称。
    pub client: String,
    /// MCP 会话编号。
    pub mcp_session: String,
    /// 工具名；连接阶段被拒绝时为空串。
    pub tool: String,
    /// 目标会话配置；会话已删除时 id 仍保留，界面查不到名称时显示"已删除的会话"。
    pub profile_id: Option<String>,
    /// 目标标签或无界面会话的运行时 id。
    pub session_id: Option<u64>,
    /// 参数摘要，如超时和路径。
    pub args: String,
    /// 命令或按键摘要。
    pub input: Option<String>,
    /// 输入的发送状态，旧记录可能没有。
    pub dispatch: Option<FfiMcpAuditDispatch>,
    /// 错误码；成功为空。
    pub error_code: Option<String>,
    /// 耗时（毫秒）。
    pub duration_ms: u32,
    /// 返回的数据大小（字节）。
    pub bytes: u64,
}

impl From<McpAuditRecord> for FfiMcpAuditRecord {
    fn from(r: McpAuditRecord) -> Self {
        let e = r.entry;
        let (args, input, dispatch) = match e.input() {
            Some(input) => (
                input.options.to_owned(),
                Some(input.text.to_owned()),
                input.dispatch.map(Into::into),
            ),
            None => (e.args, None, None),
        };
        FfiMcpAuditRecord {
            id: r.id,
            at_ms: e.at_ms,
            client: e.client,
            mcp_session: e.mcp_session,
            tool: e.tool,
            profile_id: e.profile.map(|p| p.to_string()),
            session_id: e.session.map(|s| s.raw()),
            args,
            input,
            dispatch,
            error_code: e.error_code,
            duration_ms: e.duration_ms,
            bytes: e.bytes,
        }
    }
}

#[cfg(test)]
mod audit_tests {
    use kanso_types::mcp::McpAuditEntry;

    use super::*;

    #[test]
    fn audit_input_keeps_tab_and_supports_previous_records() {
        let mut record = McpAuditRecord {
            id: 1,
            entry: McpAuditEntry {
                at_ms: 0,
                client: "Codex".into(),
                mcp_session: "client-1".into(),
                tool: "kanso_terminal_run".into(),
                profile: None,
                session: Some(SessionId::from_raw(3)),
                args: "command=echo hello\ndispatch=submitted\ntimeout_secs=2".into(),
                error_code: Some("TIMEOUT".into()),
                duration_ms: 2_000,
                bytes: 0,
            },
        };
        let current = FfiMcpAuditRecord::from(record.clone());
        assert_eq!(current.session_id, Some(3));
        assert_eq!(current.input.as_deref(), Some("echo hello"));
        assert_eq!(current.dispatch, Some(FfiMcpAuditDispatch::Submitted));
        assert_eq!(current.args, "timeout_secs=2");

        record.entry.args = "tab=3, timeout_secs=2, command=echo hello".into();
        let legacy = FfiMcpAuditRecord::from(record);
        assert_eq!(legacy.session_id, Some(3));
        assert_eq!(legacy.input.as_deref(), Some("echo hello"));
        assert_eq!(legacy.dispatch, None);
        assert_eq!(legacy.args, "tab=3, timeout_secs=2");
    }
}

#[uniffi::export]
impl KansoCore {
    /// MCP 总开关；从未设置过为关闭。宿主据此决定启动时是否调用 `KansoMcpServer.start`。
    pub fn mcp_enabled(&self) -> bool {
        self.inner.mcp_enabled()
    }

    /// 设置 MCP 总开关。宿主随后自行调用 `KansoMcpServer.start` / `stop`。
    pub fn set_mcp_enabled(&self, enabled: bool) -> Result<(), FfiCoreError> {
        Ok(self.inner.set_mcp_enabled(enabled)?)
    }

    /// 会话配置的访问级别；不含总开关。读库失败时抛错（不按"关闭"返回）。
    pub fn mcp_access(&self, profile_id: String) -> Result<FfiMcpAccess, FfiCoreError> {
        let id: ProfileId = parse_id(&profile_id, "profile")?;
        // 读库失败要抛给宿主，不能按 Off 返回：编辑对话框据此显示读取失败并禁用字段。
        Ok(self.inner.try_mcp_access(id)?.into())
    }

    /// 全部不是"关闭"的授权。读库失败时抛错：宿主不得把失败当成"全部关闭"，
    /// 应显示读取失败并禁用依赖授权状态的操作。
    pub fn mcp_accesses(&self) -> Result<Vec<FfiMcpGrant>, FfiCoreError> {
        Ok(self
            .inner
            .mcp_accesses()?
            .into_iter()
            .map(|(p, level)| FfiMcpGrant {
                profile_id: p.to_string(),
                level: level.into(),
            })
            .collect())
    }

    /// 设置访问级别，`Off` 即撤销。会话配置不存在返回 `NoSuchProfile`。
    pub fn set_mcp_access(
        &self,
        profile_id: String,
        level: FfiMcpAccess,
    ) -> Result<(), FfiCoreError> {
        let id: ProfileId = parse_id(&profile_id, "profile")?;
        Ok(self.inner.set_mcp_access(id, level.into())?)
    }

    /// 查询审计日志。
    pub fn mcp_audit(
        &self,
        filter: FfiMcpAuditFilter,
    ) -> Result<Vec<FfiMcpAuditRecord>, FfiCoreError> {
        Ok(self
            .inner
            .mcp_audit(filter.try_into()?)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// 清空审计日志。
    pub fn mcp_audit_clear(&self) -> Result<(), FfiCoreError> {
        Ok(self.inner.mcp_audit_clear()?)
    }
}

// ---- 服务 ----

/// 无法提供服务的原因（镜像 `McpUnavailable`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMcpUnavailable {
    /// 当前渠道不提供 MCP，或真实服务尚未实现。**不要据此隐藏 MCP 页面**：是否显示页面只看
    /// [`mcp_supported`]。Direct 构建里出现这个状态时在页面上显示错误，保证用户还能把开关关掉。
    NotSupported,
    /// 另一个 Kanso 实例（例如同时打开的 Debug 版）已在提供服务。
    AnotherInstance,
    /// 启动失败；`detail` 只进日志。
    Failed {
        /// 英文技术细节。
        detail: String,
    },
}

/// 服务状态（镜像 `McpServerStatus`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMcpServerStatus {
    /// 没有运行。
    Stopped,
    /// 正在监听。
    Running {
        /// socket 路径，设置页可显示以便排查。
        socket_path: String,
    },
    /// 无法提供服务。
    Unavailable {
        /// 原因。
        reason: FfiMcpUnavailable,
    },
}

impl From<McpServerStatus> for FfiMcpServerStatus {
    fn from(s: McpServerStatus) -> Self {
        match s {
            McpServerStatus::Stopped => FfiMcpServerStatus::Stopped,
            McpServerStatus::Running { socket_path } => FfiMcpServerStatus::Running { socket_path },
            McpServerStatus::Unavailable(reason) => FfiMcpServerStatus::Unavailable {
                reason: match reason {
                    McpUnavailable::NotSupported => FfiMcpUnavailable::NotSupported,
                    McpUnavailable::AnotherInstance => FfiMcpUnavailable::AnotherInstance,
                    McpUnavailable::Failed { detail } => FfiMcpUnavailable::Failed { detail },
                },
            },
        }
    }
}

/// 已连接的客户端（镜像 `McpClientInfo`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMcpClientInfo {
    /// 本次运行内唯一的客户端编号。
    pub id: u64,
    /// 客户端自报名称；空串时界面显示"未知客户端"。
    pub name: String,
    /// 连接时间，Unix 毫秒。
    pub connected_at_ms: i64,
    /// 已处理的工具调用次数。
    pub calls: u64,
    /// 客户端所在标签；没有识别出为空。
    pub own_session_id: Option<u64>,
}

impl From<McpClientInfo> for FfiMcpClientInfo {
    fn from(c: McpClientInfo) -> Self {
        FfiMcpClientInfo {
            id: c.id,
            name: c.name,
            connected_at_ms: c.connected_at_ms,
            calls: c.calls,
            own_session_id: c.own_session.map(|s| s.raw()),
        }
    }
}

/// 标签上正在进行的 MCP 操作（镜像 `McpActivity`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMcpActivity {
    /// 客户端编号。
    pub client_id: u64,
    /// 客户端名称。
    pub client_name: String,
    /// 工具名。
    pub tool: String,
}

/// 服务操作失败（镜像 `McpServerError`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum FfiMcpServerError {
    /// 当前渠道或当前实现不支持。
    NotSupported,
    /// 另一个 Kanso 实例已在提供服务。
    AnotherInstance,
    /// 服务没有运行。
    NotRunning,
    /// 其他失败；`detail` 只进日志。
    Failed {
        /// 英文技术细节。
        detail: String,
    },
}

impl std::fmt::Display for FfiMcpServerError {
    /// 英文，供日志使用；不得直接展示给用户。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiMcpServerError::NotSupported => f.write_str("mcp server not supported"),
            FfiMcpServerError::AnotherInstance => {
                f.write_str("another kanso instance is serving mcp")
            }
            FfiMcpServerError::NotRunning => f.write_str("mcp server is not running"),
            FfiMcpServerError::Failed { detail } => write!(f, "mcp server failed: {detail}"),
        }
    }
}

impl std::error::Error for FfiMcpServerError {}

impl From<McpServerError> for FfiMcpServerError {
    fn from(e: McpServerError) -> Self {
        match e {
            McpServerError::NotSupported => FfiMcpServerError::NotSupported,
            McpServerError::AnotherInstance => FfiMcpServerError::AnotherInstance,
            McpServerError::NotRunning => FfiMcpServerError::NotRunning,
            McpServerError::Failed { detail } => FfiMcpServerError::Failed { detail },
        }
    }
}

/// 服务回调，由 Swift 实现。
///
/// - **线程**：在任意工作线程上调用，不保证是主线程；
/// - **阻塞**：必须立即返回，只做"投递到主线程"；
/// - **重入**：不得在回调里同步调用 `KansoMcpServer` 或 `KansoCore` 的方法（可能死锁）。
#[uniffi::export(with_foreign)]
pub trait FfiMcpServerDelegate: Send + Sync {
    /// 服务状态变化。
    fn on_status_changed(&self, status: FfiMcpServerStatus);
    /// 已连接的客户端列表变化（完整的新列表）；调用次数变化可能很频繁，应合并刷新。
    fn on_clients_changed(&self, clients: Vec<FfiMcpClientInfo>);
    /// MCP 打开了新标签：放进最前面窗口的当前分屏，**不切换当前标签、不抢焦点**，加"由 MCP 打开"标记。
    fn on_session_opened(&self, session_id: u64, client: FfiMcpClientInfo);
    /// 标签上的 MCP 活动：有值时显示标识；为空表示操作结束，2 秒后撤掉标识。
    fn on_activity(&self, session_id: u64, activity: Option<FfiMcpActivity>);
    /// 客户端断开后，它打开的标签变成普通标签：去掉"由 MCP 打开"标记。
    fn on_session_released(&self, session_id: u64);
    /// 审计库内容变了（真实服务才会触发）：一批记录已写入，或定期清理删除了旧记录；宿主据此刷新
    /// 活动记录窗口，不必轮询。一批只回调一次；写入失败或清理没删到记录时不回调。
    ///
    /// 回调期间持有服务的拆除同步锁，而销毁 `KansoMcpServer` 的线程（通常是主线程）要拿同一把锁：
    /// 实现绝不能在回调里同步等待主线程（如 `DispatchQueue.main.sync`），只能异步投递后立即返回。
    fn on_audit_changed(&self);
}

/// 把 Swift 的 [`FfiMcpServerDelegate`] 适配成服务的 [`McpServerDelegate`]：只做类型转换后转发。
struct DelegateAdapter(Arc<dyn FfiMcpServerDelegate>);

impl McpServerDelegate for DelegateAdapter {
    fn on_status_changed(&self, status: McpServerStatus) {
        self.0.on_status_changed(status.into());
    }
    fn on_clients_changed(&self, clients: Vec<McpClientInfo>) {
        self.0
            .on_clients_changed(clients.into_iter().map(Into::into).collect());
    }
    fn on_session_opened(&self, session: SessionId, client: McpClientInfo) {
        self.0.on_session_opened(session.raw(), client.into());
    }
    fn on_activity(&self, session: SessionId, activity: Option<McpActivity>) {
        self.0.on_activity(
            session.raw(),
            activity.map(|a| FfiMcpActivity {
                client_id: a.client_id,
                client_name: a.client_name,
                tool: a.tool,
            }),
        );
    }
    fn on_session_released(&self, session: SessionId) {
        self.0.on_session_released(session.raw());
    }
    fn on_audit_changed(&self) {
        self.0.on_audit_changed();
    }
}

/// MCP 服务对象。宿主创建一个，在整个进程生命周期内持有；线程安全。
#[derive(uniffi::Object)]
pub struct KansoMcpServer {
    inner: McpServer,
}

#[uniffi::export]
impl KansoMcpServer {
    /// 创建真实服务，与宿主共用 `core`。构造时就取单实例锁；
    /// `start` 传入的 bundle id 必须与 `core` 的一致。非 Direct 渠道（及非 Unix 平台）`start` 永远返回 `NotSupported`。
    #[uniffi::constructor]
    pub fn new(core: Arc<KansoCore>, delegate: Arc<dyn FfiMcpServerDelegate>) -> Arc<Self> {
        Arc::new(KansoMcpServer {
            inner: McpServer::new(core.inner.clone(), Arc::new(DelegateAdapter(delegate))),
        })
    }

    /// 创建 mock 服务（界面开发与截图测试）：`start` 后模拟一个已连接的客户端，
    /// `debug_open_tab` / `debug_activity` 可触发对应回调。通常与 `KansoCore.newMock` 配套。
    #[uniffi::constructor]
    pub fn new_mock(core: Arc<KansoCore>, delegate: Arc<dyn FfiMcpServerDelegate>) -> Arc<Self> {
        Arc::new(KansoMcpServer {
            inner: McpServer::new_mock(core.inner.clone(), Arc::new(DelegateAdapter(delegate))),
        })
    }

    /// 开始监听；已在运行时用新参数重启。`bundle_id` 传 App 的 bundle id，`token` 传钥匙串里的客户端令牌。
    pub fn start(&self, bundle_id: String, token: String) -> Result<(), FfiMcpServerError> {
        Ok(self.inner.start(McpServerConfig {
            bundle_id,
            token: SecretString::new(token),
        })?)
    }

    /// 停止监听并断开全部客户端。
    pub fn stop(&self) {
        self.inner.stop()
    }

    /// 更换客户端令牌（"重新生成令牌"后调用）：用旧令牌的连接立即断开。
    pub fn set_token(&self, token: String) {
        self.inner.set_token(SecretString::new(token))
    }

    /// 当前状态。
    pub fn status(&self) -> FfiMcpServerStatus {
        self.inner.status().into()
    }

    /// 当前已连接的客户端。
    pub fn clients(&self) -> Vec<FfiMcpClientInfo> {
        self.inner.clients().into_iter().map(Into::into).collect()
    }

    /// 断开指定客户端；不存在时忽略。
    pub fn disconnect(&self, client_id: u64) {
        self.inner.disconnect(client_id)
    }

    /// **仅 mock**：模拟客户端打开 `profile_id` 的标签，返回新标签 id。真实服务返回 `NotSupported`。
    pub async fn debug_open_tab(&self, profile_id: String) -> Result<u64, FfiMcpServerError> {
        let id: ProfileId =
            parse_id(&profile_id, "profile").map_err(|e| FfiMcpServerError::Failed {
                detail: e.to_string(),
            })?;
        Ok(self.inner.debug_open_tab(id).await?.raw())
    }

    /// **仅 mock**：模拟客户端在 `session_id` 上开始（有 `tool`）或结束（`tool` 为空）一次操作。
    pub fn debug_activity(
        &self,
        session_id: u64,
        tool: Option<String>,
    ) -> Result<(), FfiMcpServerError> {
        Ok(self
            .inner
            .debug_activity(SessionId::from_raw(session_id), tool)?)
    }
}

/// 当前构建是否提供 MCP 服务（只有 Direct 渠道提供）。宿主据此决定是否显示设置 → MCP 页面，
/// 不要靠调用 `start` 试探：`start` 只在总开关打开时才调用，而开关就在那个页面上（spec/mcp.md §11）。
#[uniffi::export]
pub fn mcp_supported() -> bool {
    kanso_mcp_server::supported()
}

mirror_enum! {
    /// 生成配置命令的客户端种类（镜像 `kanso_mcp_server::McpClientKind`）。
    FfiMcpClientKind <=> McpClientKind {
        /// Claude Code。
        ClaudeCode,
        /// Codex CLI。
        Codex,
    }
}

/// 生成"复制 Claude Code / Codex 配置"的终端命令。`helper_path` 为 App 包内 `kanso-mcp` 的绝对路径。
///
/// 命令以一个空格开头，使令牌不进入 **Kanso 自己**的命令历史；界面原样复制，不要去掉。
/// 它**挡不住 shell 自己的历史**：macOS 默认的 zsh 不开 `HIST_IGNORE_SPACE`，令牌仍会写进
/// `~/.zsh_history`。设置页必须在复制按钮旁说明这一点。
#[uniffi::export]
pub fn mcp_client_config(kind: FfiMcpClientKind, helper_path: String, token: String) -> String {
    kanso_mcp_server::client_config(kind.into(), &helper_path, &token)
}
