//! MCP 服务的本机数据：会话授权、总开关、审计日志（schema v3，spec/mcp.md §2.2、§6）。
//!
//! 这几张表是本机专用表，不进入 `.kanso` 导出，也不是同步候选表；见迁移 `0003_mcp.sql` 的说明。

use std::str::FromStr;

use kanso_types::ProfileId;
use kanso_types::SessionId;
use kanso_types::mcp::{
    MCP_AUDIT_MAX_ROWS, MCP_AUDIT_RETENTION_MS, McpAccess, McpAuditEntry, McpAuditFilter,
    McpAuditRecord,
};
use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};

use crate::schema::StoreError;
use crate::store::Store;

/// `mcp_local` 里总开关的键。
const KEY_ENABLED: &str = "enabled";

fn level_text(level: McpAccess) -> Option<&'static str> {
    match level {
        McpAccess::Off => None,
        McpAccess::ReadOnly => Some("read-only"),
        McpAccess::Full => Some("full"),
    }
}

fn parse_level(text: &str) -> Result<McpAccess, StoreError> {
    match text {
        "read-only" => Ok(McpAccess::ReadOnly),
        "full" => Ok(McpAccess::Full),
        other => Err(StoreError::InvalidData {
            detail: format!("unknown mcp access level {other:?}"),
        }),
    }
}

impl Store {
    /// MCP 总开关；从未设置过为关闭（spec/mcp.md §1 默认关闭）。
    pub fn mcp_enabled(&self) -> Result<bool, StoreError> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM mcp_local WHERE key = ?1",
                [KEY_ENABLED],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("1"))
    }

    /// 设置 MCP 总开关。只改开关本身，不动各会话的授权（spec/mcp.md §2.1）。
    pub fn set_mcp_enabled(&mut self, enabled: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO mcp_local (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![KEY_ENABLED, if enabled { "1" } else { "0" }],
        )?;
        Ok(())
    }

    /// 某个会话配置的访问级别；没有记录或会话已删除都是 `Off`。
    ///
    /// 已删除的会话按 `Off` 处理，不依赖删除时是否清理干净：授权判断宁可保守。
    pub fn mcp_access(&self, profile: ProfileId) -> Result<McpAccess, StoreError> {
        let level: Option<String> = self
            .conn
            .query_row(
                "SELECT g.level FROM mcp_grants g
                 JOIN profiles p ON p.id = g.profile_id AND p.deleted = 0
                 WHERE g.profile_id = ?1",
                [profile.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        level.map_or(Ok(McpAccess::Off), |l| parse_level(&l))
    }

    /// 全部不是 `Off` 的授权，只含未删除的会话；顺序不保证。
    pub fn mcp_accesses(&self) -> Result<Vec<(ProfileId, McpAccess)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT g.profile_id, g.level FROM mcp_grants g
             JOIN profiles p ON p.id = g.profile_id AND p.deleted = 0",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, level) = row?;
            // 主键是代码写入的 UUID 文本，解析失败说明数据被外部改坏；跳过这一行，不让整个列表失败。
            let Ok(id) = ProfileId::from_str(&id) else {
                tracing::warn!(profile = %id, "skipping mcp grant with invalid profile id");
                continue;
            };
            out.push((id, parse_level(&level)?));
        }
        Ok(out)
    }

    /// 设置访问级别；`Off` 即删除记录。会话不存在（含已删除）返回 `Ok(false)`，不写入。
    pub fn set_mcp_access(
        &mut self,
        profile: ProfileId,
        level: McpAccess,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let key = profile.to_string();
        let live: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM profiles WHERE id = ?1 AND deleted = 0)",
            [&key],
            |r| r.get(0),
        )?;
        if !live {
            return Ok(false);
        }
        match level_text(level) {
            None => {
                tx.execute("DELETE FROM mcp_grants WHERE profile_id = ?1", [&key])?;
            }
            Some(text) => {
                tx.execute(
                    "INSERT INTO mcp_grants (profile_id, level) VALUES (?1, ?2)
                     ON CONFLICT (profile_id) DO UPDATE SET level = excluded.level",
                    params![key, text],
                )?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// 追加审计记录，全部在一个事务里写入（调用方按批提交，避免每次调用都单独落盘）。
    pub fn append_mcp_audit(&mut self, entries: &[McpAuditEntry]) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO mcp_audit
                 (at, client, mcp_session, tool, profile_id, session_id, args, error_code,
                  duration_ms, bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for e in entries {
                stmt.execute(params![
                    e.at_ms,
                    e.client,
                    e.mcp_session,
                    e.tool,
                    e.profile.map(|p| p.to_string()),
                    // SQLite 的 INTEGER 是 i64；运行时 id 超过 i64 的情况不会出现，真出现时按 NULL 记录。
                    e.session.and_then(|s| i64::try_from(s.raw()).ok()),
                    e.args,
                    e.error_code,
                    e.duration_ms,
                    // 同上：超过 i64 的字节数只会来自错误数据，夹到上限。
                    i64::try_from(e.bytes).unwrap_or(i64::MAX),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 查询审计记录，按 id 倒序。
    pub fn mcp_audit(&self, filter: &McpAuditFilter) -> Result<Vec<McpAuditRecord>, StoreError> {
        let mut sql = String::from(
            "SELECT id, at, client, mcp_session, tool, profile_id, session_id, args, error_code,
                    duration_ms, bytes
             FROM mcp_audit WHERE 1 = 1",
        );
        let mut args: Vec<Value> = Vec::new();
        if let Some(profile) = filter.profile {
            sql.push_str(" AND profile_id = ?");
            args.push(Value::Text(profile.to_string()));
        }
        if filter.errors_only {
            sql.push_str(" AND error_code IS NOT NULL");
        }
        if let Some(before) = filter.before_id {
            sql.push_str(" AND id < ?");
            args.push(Value::Integer(before));
        }
        sql.push_str(" ORDER BY id DESC");
        let client_filter = filter.client.as_deref().map(str::to_lowercase);
        let limit = filter.effective_limit() as usize;

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, at, client, mcp_session, tool, profile, session, args, error_code, ms, bytes) =
                row?;
            if client_filter
                .as_ref()
                .is_some_and(|query| !client.to_lowercase().contains(query))
            {
                continue;
            }
            out.push(McpAuditRecord {
                id,
                entry: McpAuditEntry {
                    at_ms: at,
                    client,
                    mcp_session,
                    tool,
                    // 会话配置可能已被删除，id 文本仍然保留；解析失败只丢掉这个字段，不丢记录。
                    profile: profile.and_then(|p| ProfileId::from_str(&p).ok()),
                    session: session
                        .and_then(|s| u64::try_from(s).ok())
                        .map(SessionId::from_raw),
                    args,
                    error_code,
                    duration_ms: u32::try_from(ms).unwrap_or(u32::MAX),
                    bytes: u64::try_from(bytes).unwrap_or(0),
                },
            });
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }

    /// 清空审计日志（设置页"清空"）。
    pub fn clear_mcp_audit(&mut self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM mcp_audit", [])?;
        Ok(())
    }

    /// 按保留规则清理：删除早于 `now_ms - 30 天` 的记录，再只保留最新的 10000 条。
    /// 返回删除的条数。由 kanso-mcp-server 在启动时和每小时调用。
    pub fn prune_mcp_audit(&mut self, now_ms: i64) -> Result<u32, StoreError> {
        let tx = self.conn.transaction()?;
        let by_age = tx.execute(
            "DELETE FROM mcp_audit WHERE at < ?1",
            [now_ms.saturating_sub(MCP_AUDIT_RETENTION_MS)],
        )?;
        // 按 id 而不是 at 保留最新的若干条：id 单调递增，而 at 来自调用方的时钟，可能回拨。
        // 用"第 N+1 新的 id"做上界的范围删除，走主键，不必构造 N 个 id 的集合；
        // 不足 N+1 条时子查询为 NULL，比较结果为假，什么也不删。
        let by_count = tx.execute(
            "DELETE FROM mcp_audit WHERE id <=
             (SELECT id FROM mcp_audit ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            [MCP_AUDIT_MAX_ROWS],
        )?;
        tx.commit()?;
        Ok(u32::try_from(by_age + by_count).unwrap_or(u32::MAX))
    }
}
