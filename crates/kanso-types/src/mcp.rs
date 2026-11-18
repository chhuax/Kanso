//! MCP 服务的授权级别与审计日志类型（spec/mcp.md §2、§6）。
//!
//! 放在 kanso-types 是因为 kanso-store（持久化）和 kanso-core（契约）都要用，
//! 而两者之间不能互相依赖。

use serde::{Deserialize, Serialize};

use crate::ids::{ProfileId, SessionId};

/// 已保存会话的 MCP 访问级别（spec/mcp.md §2.1）。
///
/// 顺序有意义：`Off < ReadOnly < Full`，调用方用比较判断"级别是否足够"，
/// 所以**不能调整变体顺序**。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum McpAccess {
    /// 关闭：MCP 看不到这个会话。没有授权记录的会话都是这个级别。
    #[default]
    Off,
    /// 只读：只复用已有连接，不新建连接（spec/mcp.md §2.4）。
    ReadOnly,
    /// 完全：允许写操作，并可在不向用户提问的前提下新建连接。
    Full,
}

/// 一次工具调用的审计记录（spec/mcp.md §6）。
///
/// **不得**放入终端输出、执行输出、读写的文件内容；`args` 只放命令文本、路径等参数摘要，
/// 由调用方（kanso-mcp-server）负责裁剪。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuditEntry {
    /// 调用开始时间，Unix 毫秒。
    pub at_ms: i64,
    /// 客户端自报的名称（`clientInfo`）；只用于显示，不可信。
    pub client: String,
    /// MCP 会话编号，区分同一客户端的多次连接。
    pub mcp_session: String,
    /// 工具名，例如 `kanso_exec`；连接阶段被拒绝（令牌无效）时为空串。
    pub tool: String,
    /// 目标会话配置；与具体会话无关的调用为 `None`。
    pub profile: Option<ProfileId>,
    /// 目标标签或无界面会话；没有时为 `None`。运行时 id，只用于同一次运行内的对照。
    pub session: Option<SessionId>,
    /// 参数摘要。
    pub args: String,
    /// 错误码（spec/mcp.md §9）；成功为 `None`。
    pub error_code: Option<String>,
    /// 耗时（毫秒）。
    pub duration_ms: u32,
    /// 返回给客户端的数据大小（字节）。
    pub bytes: u64,
}

/// 从数据库读出的审计记录：带自增 id，供分页。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuditRecord {
    /// 自增 id，越新越大。
    pub id: i64,
    /// 记录内容。
    pub entry: McpAuditEntry,
}

/// 审计日志查询条件。结果按 id 倒序（新的在前）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuditFilter {
    /// 只看某个客户端名称（精确匹配）。
    pub client: Option<String>,
    /// 只看某个会话配置。
    pub profile: Option<ProfileId>,
    /// 只看失败或被拒绝的调用。
    pub errors_only: bool,
    /// 分页：只返回 id 小于它的记录；第一页为 `None`。
    pub before_id: Option<i64>,
    /// 最多返回条数；0 按 [`MCP_AUDIT_DEFAULT_LIMIT`] 处理，超过 [`MCP_AUDIT_MAX_LIMIT`] 按上限处理。
    pub limit: u32,
}

impl McpAuditFilter {
    /// 实际生效的条数。真实存储与 mock 共用，保证两边分页行为一致。
    pub fn effective_limit(&self) -> u32 {
        match self.limit {
            0 => MCP_AUDIT_DEFAULT_LIMIT,
            n => n.min(MCP_AUDIT_MAX_LIMIT),
        }
    }
}

/// 审计查询 `limit` 为 0 时的默认条数。
pub const MCP_AUDIT_DEFAULT_LIMIT: u32 = 100;
/// 审计查询单次最多返回条数，防止设置页一次读出整张表。
pub const MCP_AUDIT_MAX_LIMIT: u32 = 1000;

/// 审计日志保留期：30 天（spec/mcp.md §6）。
pub const MCP_AUDIT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// 审计日志条数上限（spec/mcp.md §6）；与保留期先到者为准。
pub const MCP_AUDIT_MAX_ROWS: u32 = 10_000;
