-- schema v3：MCP 服务的授权、本机开关与审计日志（spec/mcp.md §2.2、§6）。
--
-- 三张表都是**本机专用表**，不是同步候选表：没有 updated_at / deleted 墓碑，
-- 不进入 .kanso 导出，也不参与将来的同步。理由是 MCP 授权必须由用户在这台机器上亲手开启，
-- 导入一个文件或同步不能悄悄给 AI 开权限（spec/mcp.md §2.2）。

-- 会话的 MCP 访问级别。没有记录即为“关闭”，所以这里只存 read-only / full 两种取值；
-- 新建、导入、复制出来的会话天然是关闭。删除会话时由代码物理删除对应行。
CREATE TABLE mcp_grants (
    profile_id TEXT PRIMARY KEY,
    level      TEXT NOT NULL CHECK (level IN ('read-only', 'full'))
);

-- MCP 的本机键值设置（目前只有总开关 enabled）。不放进 settings 表：settings 会随 .kanso 导出。
CREATE TABLE mcp_local (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 工具调用审计。只记参数摘要，不记终端输出、执行输出和文件内容。
-- 保留 30 天且最多 10000 条，由代码定期清理。
CREATE TABLE mcp_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          INTEGER NOT NULL,        -- Unix 毫秒
    client      TEXT    NOT NULL,
    mcp_session TEXT    NOT NULL,
    tool        TEXT    NOT NULL,
    profile_id  TEXT    NULL,
    session_id  INTEGER NULL,            -- 运行时 SessionId，只在同一次运行内有意义
    args        TEXT    NOT NULL,
    error_code  TEXT    NULL,            -- NULL 表示成功
    duration_ms INTEGER NOT NULL,
    bytes       INTEGER NOT NULL
);
CREATE INDEX idx_mcp_audit_at ON mcp_audit (at);
CREATE INDEX idx_mcp_audit_profile ON mcp_audit (profile_id);
