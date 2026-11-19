//! Kanso 的 MCP 服务（spec/mcp.md，architecture §16）。
//!
//! 运行在 App 进程里：`kanso-mcp` 转发程序把 AI 工具的 stdio 转到本机通道，
//! 这里负责 MCP 协议、授权、审计，并通过 [`kanso_core::Core`] 执行工具调用。
//!
//! # 本 crate 的现状
//!
//! 公开接口（[`McpServer`]、[`McpServerDelegate`] 及相关类型）已定，Swift 界面对着它开发。
//! 支持的平台有两种实现：
//! - **mock**（[`McpServer::new_mock`]）：不监听 socket，`start` 后模拟一个已连接的客户端；
//!   另有 `debug_*` 方法让界面能触发"打开标签""MCP 活动"等回调。
//! - **真实**（[`McpServer::new`]）：监听本机 Unix socket 或 Windows 命名管道，实现 MCP 协议、令牌校验与审计（architecture §16）。
//!   构造时就拿单实例锁；目前只提供 `kanso_list_sessions` 一个工具（F9 阶段 1）。
//!
//! 非 Direct 渠道和不支持的平台（[`supported`] 为假）构造出的服务状态恒为
//! `Unavailable(NotSupported)`，`start` 返回 `NotSupported`（spec/mcp.md §11）。
//! 这一限制在编译期生效：此时 mock 与真实实现的代码都不参与编译。
//!
//! # 回调约定
//!
//! [`McpServerDelegate`] 的方法与 core 的事件回调一样：在任意工作线程上调用，实现必须立即返回、
//! 只做"投递到主线程"，且不得在回调里同步调用 [`McpServer`] 或 `Core` 的方法（可能死锁）。

mod client_config;
#[cfg(all(feature = "direct", any(unix, windows)))]
mod mock;
#[cfg(all(feature = "direct", any(unix, windows)))]
mod real;

use std::sync::Arc;

use kanso_core::Core;
use kanso_types::secret::SecretString;
use kanso_types::{ProfileId, SessionId};

pub use client_config::{McpClientKind, client_config, powershell_client_config};

/// 服务状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpServerStatus {
    /// 没有运行（总开关关闭，或尚未启动）。
    Stopped,
    /// 正在监听。
    Running {
        /// socket 路径，设置页可以显示，便于排查。
        socket_path: String,
    },
    /// 无法提供服务。
    Unavailable(McpUnavailable),
}

/// 无法提供服务的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpUnavailable {
    /// 当前渠道不提供 MCP（MAS / Setapp，spec/mcp.md §11），或真实服务尚未实现。
    NotSupported,
    /// 另一个 Kanso 实例已经持有单实例锁并在提供服务（architecture §16.1），
    /// 例如同时打开了 Debug 版与 Release 版（二者 bundle id 相同）。
    AnotherInstance,
    /// 启动失败，`detail` 为英文技术细节，只进日志。
    Failed {
        /// 技术细节。
        detail: String,
    },
}

/// 一个已连接的客户端（设置页"已连接的客户端"列表）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpClientInfo {
    /// 本次运行内唯一的客户端编号，断开指定客户端时用。
    pub id: u64,
    /// 客户端自报名称（`clientInfo.name`）；没有时为空串，界面显示"未知客户端"。只用于显示，不可信。
    pub name: String,
    /// 连接时间，Unix 毫秒。
    pub connected_at_ms: i64,
    /// 已处理的工具调用次数（含被拒绝的）。
    pub calls: u64,
    /// 识别出的"客户端所在标签"（spec/mcp.md §2.5）；没有识别出为 `None`。
    pub own_session: Option<SessionId>,
}

/// 某个标签上正在进行的 MCP 操作（标签行的 MCP 标识与悬停提示，spec/mcp.md §4.1）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpActivity {
    /// 发起操作的客户端编号。
    pub client_id: u64,
    /// 客户端名称（同 [`McpClientInfo::name`]）。
    pub client_name: String,
    /// 工具名，例如 `kanso_terminal_run`。
    pub tool: String,
}

/// 服务回调，由前端宿主实现。约定见模块文档"回调约定"。
pub trait McpServerDelegate: Send + Sync {
    /// 服务状态变化。
    fn on_status_changed(&self, status: McpServerStatus);

    /// 已连接的客户端列表变化（连接、断开、调用次数变化）；给的是完整的新列表。
    /// 调用次数变化可能很频繁，实现方应合并刷新。
    fn on_clients_changed(&self, clients: Vec<McpClientInfo>);

    /// MCP 打开了一个新标签（`kanso_open_tab`）。宿主把它放进最前面窗口的当前分屏，没有窗口时新建窗口；
    /// **不切换当前标签、不抢键盘焦点**，并给它加上"由 MCP 打开"标记（spec/mcp.md §3.3、§4.1）。
    fn on_session_opened(&self, session: SessionId, client: McpClientInfo);

    /// 标签上的 MCP 活动变化。`Some` 表示开始显示标识；`None` 表示操作已结束，宿主在 2 秒后撤掉标识
    /// （spec/mcp.md §4.1）。
    fn on_activity(&self, session: SessionId, activity: Option<McpActivity>);

    /// 客户端断开后，它打开的标签变成普通标签：宿主去掉"由 MCP 打开"标记（spec/mcp.md §3.3）。
    fn on_session_released(&self, session: SessionId);

    /// 一批审计记录已**成功写入数据库**（包括令牌错误被拒的连接产生的记录）。宿主据此刷新审计摘要，
    /// 不必轮询数据库。
    ///
    /// 在审计任务的线程上调用，且**回调期间持有服务的拆除同步锁**（`audit_quiet`）：`McpServer` 的 `Drop`
    /// 要拿同一把锁才能置位"静默"，所以回调没返回时，销毁服务的线程会被挡住。因此实现必须立即返回：
    /// 不得同步调用 `McpServer`，也绝不能同步等待另一个可能正在销毁 `McpServer` 的线程（例如同步派发到
    /// 主线程并等待，而主线程恰好在 `Drop`），否则两边互相等待形成死锁。只能异步投递后立即返回。
    /// 一批只回调一次（不是每条记录一次）。写入失败不回调；定期清理（prune）实际删除了记录时也回调
    /// 一次，没删到记录不回调。服务在 `Drop` 中拆除后不再回调。真实服务才会触发，mock 服务不写审计。
    fn on_audit_changed(&self);
}

/// 启动参数。
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// App 的 bundle id；socket 与单实例锁的路径由它推出，`kanso-mcp` 用同一规则（architecture §16.1）。
    /// 真实服务构造时就按 `Core::bundle_id()` 取锁，所以这里必须与之一致，不一致时 `start` 返回 `Failed`。
    pub bundle_id: String,
    /// 客户端令牌（spec/mcp.md §1.1）。由宿主保存在钥匙串里，启动时传入。
    pub token: SecretString,
}

/// 服务操作失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpServerError {
    /// 当前渠道或当前实现不支持。
    NotSupported,
    /// 另一个 Kanso 实例已在提供服务。
    AnotherInstance,
    /// 服务没有运行（`debug_*` 等需要运行中的服务）。
    NotRunning,
    /// 其他失败，`detail` 为英文技术细节，只进日志。
    Failed {
        /// 技术细节。
        detail: String,
    },
}

impl std::fmt::Display for McpServerError {
    /// 英文，供日志使用；不得直接展示给用户。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpServerError::NotSupported => f.write_str("mcp server not supported"),
            McpServerError::AnotherInstance => f.write_str("another kanso instance is serving mcp"),
            McpServerError::NotRunning => f.write_str("mcp server is not running"),
            McpServerError::Failed { detail } => write!(f, "mcp server failed: {detail}"),
        }
    }
}

impl std::error::Error for McpServerError {}

/// 结果简写。
pub type Result<T> = std::result::Result<T, McpServerError>;

/// 当前构建是否提供 MCP 服务：只有 Direct 渠道、且在 Unix 或 Windows 平台上提供（spec/mcp.md §11）。
///
/// 宿主**只用它**决定是否显示设置 → MCP 页面，不能靠 `start` 的结果或状态判断：
/// `start` 只在总开关打开时才调用，而开关就在那个页面上；Direct 构建里 `start` 失败
/// （例如拿不到单实例锁）应当在页面上显示错误，而不是把页面藏起来，否则用户再也关不掉开关。
pub const fn supported() -> bool {
    cfg!(all(feature = "direct", any(unix, windows)))
}

/// 渠道限制在**编译期**生效：非 Direct（或不支持的平台）构建里 `mock` 与 `real` 两个模块根本不参与编译，
/// socket 监听、rmcp 等依赖不会被链接进 MAS / Setapp（architecture §16）。
enum Backend {
    #[cfg(all(feature = "direct", any(unix, windows)))]
    Mock(mock::MockServer),
    #[cfg(all(feature = "direct", any(unix, windows)))]
    Real(real::RealServer),
    /// 非 Direct 渠道（或不支持的平台）：状态恒为 `Unavailable(NotSupported)`，所有操作返回 `NotSupported` 或空。
    /// 持有回调，是为了让 `start` 失败时也按契约回调一次状态。
    #[cfg_attr(all(feature = "direct", any(unix, windows)), allow(dead_code))]
    Unsupported(Arc<dyn McpServerDelegate>),
}

const UNSUPPORTED: McpServerStatus = McpServerStatus::Unavailable(McpUnavailable::NotSupported);

/// MCP 服务。宿主创建一个，在整个进程生命周期内持有；所有方法可从任意线程调用。
///
/// 总开关由宿主驱动：开关打开时调用 [`start`](Self::start)，关闭时调用 [`stop`](Self::stop)。
/// 开关本身存在 core 里（`Core::set_mcp_enabled`），本对象不读写它。
pub struct McpServer {
    backend: Backend,
}

impl McpServer {
    /// 创建真实服务。`core` 与宿主使用的是同一个 core。
    ///
    /// 构造时就取得单实例锁，并一直持有到服务对象销毁（architecture §16.1：转发程序靠它区分
    /// "App 未运行"与"开关关闭"）。拿不到锁时状态恒为 `Unavailable(AnotherInstance)`。
    pub fn new(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> McpServer {
        #[cfg(all(feature = "direct", any(unix, windows)))]
        let backend = Backend::Real(real::RealServer::new(core, delegate));
        #[cfg(not(all(feature = "direct", any(unix, windows))))]
        let backend = {
            let _ = core;
            Backend::Unsupported(delegate)
        };
        McpServer { backend }
    }

    /// 同 [`McpServer::new`]，但目录由调用方指定，仅供测试使用（避免碰真实的用户目录）。
    #[doc(hidden)]
    #[cfg(all(feature = "direct", any(unix, windows)))]
    pub fn new_in_dir(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        dir: std::path::PathBuf,
    ) -> McpServer {
        McpServer {
            backend: Backend::Real(real::RealServer::in_dir(core, delegate, dir)),
        }
    }

    /// 创建 mock 服务，供 Swift 界面开发与截图测试。通常与 `Core::new_mock` 配套使用。
    /// 渠道规则与真实服务相同：非 Direct 构建同样"不支持"，界面在各渠道下的表现与真实情况一致。
    pub fn new_mock(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> McpServer {
        #[cfg(all(feature = "direct", any(unix, windows)))]
        let backend = Backend::Mock(mock::MockServer::new(core, delegate));
        #[cfg(not(all(feature = "direct", any(unix, windows))))]
        let backend = {
            let _ = core;
            Backend::Unsupported(delegate)
        };
        McpServer { backend }
    }

    /// 开始监听。已经在运行时用新配置重启（旧连接全部断开）。
    ///
    /// - 线程：任意。阻塞：同步，创建目录、加锁、绑定 socket，毫秒级。
    /// - 出错：渠道不支持返回 `NotSupported`；拿不到单实例锁返回 `AnotherInstance`；其他返回 `Failed`。
    ///   失败时状态同步变为对应的 `Unavailable`，并回调 `on_status_changed`（各渠道一致）。
    pub fn start(&self, config: McpServerConfig) -> Result<()> {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.start(config),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(b) => b.start(config),
            Backend::Unsupported(delegate) => {
                let _ = config;
                delegate.on_status_changed(UNSUPPORTED);
                Err(McpServerError::NotSupported)
            }
        }
    }

    /// 停止监听并断开全部客户端；正在执行的调用按 spec/mcp.md §4.4 中断。
    /// 之后状态为 `Stopped`（此前是 `Unavailable` 时也回到 `Stopped`，非 Direct 渠道除外），
    /// 状态有变化时回调 `on_status_changed`。
    pub fn stop(&self) {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.stop(),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(b) => b.stop(),
            Backend::Unsupported(_) => {}
        }
    }

    /// 更换客户端令牌（"重新生成令牌"）：之后只接受新令牌，用旧令牌建立的连接立即断开（spec/mcp.md §1.1）。
    /// 没有运行时什么也不做：下次 `start` 以传入的配置为准，宿主应传入钥匙串里的最新令牌。
    pub fn set_token(&self, token: SecretString) {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.set_token(token),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(b) => b.set_token(token),
            Backend::Unsupported(_) => {
                let _ = token;
            }
        }
    }

    /// 当前状态。
    pub fn status(&self) -> McpServerStatus {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.status(),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(b) => b.status(),
            Backend::Unsupported(_) => UNSUPPORTED,
        }
    }

    /// 当前已连接的客户端。
    pub fn clients(&self) -> Vec<McpClientInfo> {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.clients(),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(b) => b.clients(),
            Backend::Unsupported(_) => Vec::new(),
        }
    }

    /// 断开指定客户端；不存在时忽略。
    pub fn disconnect(&self, client_id: u64) {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.disconnect(client_id),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(b) => b.disconnect(client_id),
            Backend::Unsupported(_) => {
                let _ = client_id;
            }
        }
    }

    /// **仅 mock**：模拟第一个已连接的客户端调用 `kanso_open_tab` 打开 `profile`，
    /// 走 `Core::mcp_open_session` 并回调 `on_session_opened`，返回新标签。
    /// 用于界面开发时检验"不抢焦点"的标签摆放。真实服务返回 `NotSupported`。
    pub async fn debug_open_tab(&self, profile: ProfileId) -> Result<SessionId> {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.debug_open_tab(profile).await,
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(_) => Err(McpServerError::NotSupported),
            Backend::Unsupported(_) => {
                let _ = profile;
                Err(McpServerError::NotSupported)
            }
        }
    }

    /// **仅 mock**：模拟第一个已连接的客户端在 `session` 上开始（`Some(tool)`）或结束（`None`）一次操作，
    /// 回调 `on_activity`。真实服务返回 `NotSupported`。
    pub fn debug_activity(&self, session: SessionId, tool: Option<String>) -> Result<()> {
        match &self.backend {
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Mock(b) => b.debug_activity(session, tool),
            #[cfg(all(feature = "direct", any(unix, windows)))]
            Backend::Real(_) => Err(McpServerError::NotSupported),
            Backend::Unsupported(_) => {
                let _ = (session, tool);
                Err(McpServerError::NotSupported)
            }
        }
    }
}

/// 当前 Unix 毫秒。
#[cfg(all(feature = "direct", any(unix, windows)))]
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
