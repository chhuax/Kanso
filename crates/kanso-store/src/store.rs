//! 仓库层：Profile、分组、Sender 命令、设置的读写。命令历史见 `history.rs`，导入导出见 `exchange.rs`。
//!
//! # 墓碑与读取时修正
//!
//! 同步候选表只做墓碑删除（`deleted = 1`），所以所有读取都过滤 `deleted = 0`。
//! 引用一致性不靠外键（见 `docs/schema.md` §1），而是**读取时修正**：
//!
//! - Profile 引用的分组不存在或已删除 → 返回 `group: None`（放回顶层，store-import.md §3）；
//! - Snippet 作用域指向不存在 / 已删除的 Profile 或分组 → 返回 `Global`（store-import.md §4）。
//!
//! 只在读取结果里修正、不回写数据库：将来 iCloud 同步时记录可能乱序到达（Profile 比分组先到），
//! 回写会把"暂时还没到"的引用永久改掉；读取时修正则等分组到达后自动恢复。
//! 跳板引用（`jump`）不在读取时修正：失效必须在连接时报错，不能悄悄改成直连（store-import.md §3）。

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use kanso_types::geometry::Rgb;
use kanso_types::profile::{
    Group, LineEnding, Profile, ProfileKind, SessionKind, Snippet, SnippetKindScope, SnippetScope,
};
use kanso_types::{GroupId, ProfileId, SnippetId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::schema::{self, StoreError};

/// Sender 命令总数上限（store-import.md §4）。
pub const MAX_SNIPPETS: usize = 1000;

/// 取当前标签可见的作用域并集，按名称忽略大小写排序；快速连接不继承配置或分组范围。
pub fn visible_snippets(
    mut snippets: Vec<Snippet>,
    profiles: &[Profile],
    profile: Option<ProfileId>,
    kind: Option<SessionKind>,
) -> Vec<Snippet> {
    let saved = profile.and_then(|id| profiles.iter().find(|item| item.id == id));
    snippets.retain(|snippet| match snippet.scope {
        SnippetScope::Global => true,
        SnippetScope::Profile(id) => saved.is_some_and(|item| item.id == id),
        SnippetScope::Group(id) => saved.is_some_and(|item| item.group == Some(id)),
        SnippetScope::Kind(SnippetKindScope::Local) => kind == Some(SessionKind::Local),
        SnippetScope::Kind(SnippetKindScope::Ssh) => kind == Some(SessionKind::Ssh),
    });
    snippets.sort_by_key(|snippet| snippet.name.to_lowercase());
    snippets
}

/// 删除分组会连带删除的数量；与 [`Store::delete_group`] 实际删除的集合由同一个查询得出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupDeleteImpact {
    /// 将被删除的 Profile 数。
    pub profiles: u32,
    /// 将被删除的 Sender 命令数。
    pub snippets: u32,
}

type CompletionHistorySnapshot = Vec<(crate::HistoryEntry, Vec<String>)>;

/// 存储仓库：持有一个 SQLite 连接，同步 API。
///
/// 线程模型：`Connection` 不是 `Sync`，调用方（core）把 `Store` 放进 `Mutex`；
/// 每个方法都很短（单条或少量 SQL），可在调用线程上同步执行。
pub struct Store {
    pub(crate) conn: Connection,
    /// 命令历史条数上限。生产用 5000（store-import.md §5）；做成字段只为让测试能用小数字验证淘汰。
    pub(crate) history_cap: usize,
    pub(crate) completion_history_cache: RefCell<Option<CompletionHistorySnapshot>>,
}

/// 当前 Unix 毫秒。
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Store {
    /// 打开（必要时创建并迁移）位于 `path` 的数据库。文件权限、WAL 等见 [`schema::open`]。
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        Ok(Store::from_connection(schema::open(path)?))
    }

    /// 打开内存数据库，供测试使用。
    pub fn open_in_memory() -> Result<Store, StoreError> {
        Ok(Store::from_connection(schema::open_in_memory()?))
    }

    fn from_connection(conn: Connection) -> Store {
        Store {
            conn,
            history_cap: crate::history::HISTORY_CAPACITY,
            completion_history_cache: RefCell::new(None),
        }
    }

    /// 改写命令历史的条数上限，仅供测试用小数字验证淘汰；产品代码不要调用。
    #[doc(hidden)]
    pub fn with_history_capacity(mut self, cap: usize) -> Store {
        self.history_cap = cap;
        self
    }

    // ---- Profile ----

    /// 全部未删除的 Profile，按 `sort_key`、名称、id 稳定排序；失效的分组引用已修正为顶层。
    pub fn profiles(&self) -> Result<Vec<Profile>, StoreError> {
        profiles_on(&self.conn)
    }

    /// 按 id 取一个未删除的 Profile。
    pub fn profile(&self, id: ProfileId) -> Result<Option<Profile>, StoreError> {
        Ok(profiles_on(&self.conn)?.into_iter().find(|p| p.id == id))
    }

    /// 新增或更新（按 id）。新记录排在最后；更新保留原排序位置；已被墓碑的同 id 记录会复活。
    ///
    /// 不做业务校验（跳板链等由 core 的 `validate_profile` 负责），也不检查分组是否存在。
    pub fn upsert_profile(&mut self, profile: &Profile) -> Result<(), StoreError> {
        upsert_profile_on(&self.conn, profile, now_ms())
    }

    /// 在同一个事务里新增或更新一批 Profile（语义同 [`Store::upsert_profile`]，按给定顺序）。
    ///
    /// 任何一条失败都整体回滚：ssh_config 导入要么全部生效、要么都不生效，不留下半截状态。
    /// 同样不做业务校验，由调用方（core）先行校验。
    pub fn upsert_profiles(&mut self, profiles: &[Profile]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        let now = now_ms();
        for p in profiles {
            upsert_profile_on(&tx, p, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 墓碑删除 Profile，**同时**墓碑掉作用域只针对它的 Sender 命令。
    /// 返回 `false` 表示不存在（或已删除）。凭据由调用方（core）另行清理。
    pub fn delete_profile(&mut self, id: ProfileId) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let now = now_ms();
        let key = id.to_string();
        let n = tx.execute(
            "UPDATE profiles SET deleted = 1, updated_at = ?2 WHERE id = ?1 AND deleted = 0",
            params![key, now],
        )?;
        if n == 0 {
            return Ok(false);
        }
        tx.execute(
            "UPDATE snippets SET deleted = 1, updated_at = ?2
             WHERE scope_kind = 'profile' AND scope_ref = ?1 AND deleted = 0",
            params![key, now],
        )?;
        // MCP 授权是本机专用表，没有墓碑，直接物理删除（spec/mcp.md §2.2）。
        tx.execute("DELETE FROM mcp_grants WHERE profile_id = ?1", [&key])?;
        tx.commit()?;
        Ok(true)
    }

    // ---- 分组 ----

    /// 全部未删除的分组，按 `sort_key`、名称、id 排序。
    pub fn groups(&self) -> Result<Vec<Group>, StoreError> {
        groups_on(&self.conn)
    }

    /// 按 id 取一个未删除的分组。
    pub fn group(&self, id: GroupId) -> Result<Option<Group>, StoreError> {
        Ok(groups_on(&self.conn)?.into_iter().find(|g| g.id == id))
    }

    /// 新增或更新（按 id）分组。
    pub fn upsert_group(&mut self, group: &Group) -> Result<(), StoreError> {
        upsert_group_on(&self.conn, group, now_ms())
    }

    /// 删除该分组会连带删除多少 Profile 与 Sender 命令；分组不存在返回 `None`。
    ///
    /// 与 [`Store::delete_group`] 共用 [`group_cascade`]，所以数字必然一致。
    pub fn delete_impact(&self, id: GroupId) -> Result<Option<GroupDeleteImpact>, StoreError> {
        if !group_live(&self.conn, id)? {
            return Ok(None);
        }
        let (profiles, snippets) = group_cascade(&self.conn, id)?;
        Ok(Some(GroupDeleteImpact {
            profiles: profiles.len() as u32,
            snippets: snippets.len() as u32,
        }))
    }

    /// 墓碑删除分组、组内 Profile、以及作用域是该分组或组内 Profile 的 Sender 命令。
    ///
    /// 返回被删除的 Profile id（供 core 删除凭据）；分组不存在返回 `None`。
    /// 删除的集合直接取自 [`group_cascade`]，不再另写条件，保证与 `delete_impact` 一致。
    pub fn delete_group(&mut self, id: GroupId) -> Result<Option<Vec<ProfileId>>, StoreError> {
        let tx = self.conn.transaction()?;
        if !group_live(&tx, id)? {
            return Ok(None);
        }
        let (profiles, snippets) = group_cascade(&tx, id)?;
        let now = now_ms();
        for p in &profiles {
            tx.execute(
                "UPDATE profiles SET deleted = 1, updated_at = ?2 WHERE id = ?1",
                params![p, now],
            )?;
            // 同 delete_profile：MCP 授权直接物理删除。
            tx.execute("DELETE FROM mcp_grants WHERE profile_id = ?1", [p])?;
        }
        for s in &snippets {
            tx.execute(
                "UPDATE snippets SET deleted = 1, updated_at = ?2 WHERE id = ?1",
                params![s, now],
            )?;
        }
        tx.execute(
            "UPDATE groups SET deleted = 1, updated_at = ?2 WHERE id = ?1",
            params![id.to_string(), now],
        )?;
        tx.commit()?;
        Ok(Some(
            profiles
                .iter()
                .filter_map(|p| ProfileId::from_str(p).ok())
                .collect(),
        ))
    }

    // ---- Sender 命令 ----

    /// 全部未删除的 Sender 命令；作用域指向不存在的 Profile / 分组时按全局返回。
    pub fn snippets(&self) -> Result<Vec<Snippet>, StoreError> {
        snippets_on(&self.conn)
    }

    /// 新增或更新（按 id）。已有 [`MAX_SNIPPETS`] 条未删除命令时拒绝**新增**
    /// （返回 [`StoreError::SnippetLimitReached`]）；更新已有的不受限。
    pub fn upsert_snippet(&mut self, snippet: &Snippet) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        // 被墓碑的同 id 记录复活也算新增：此时它不在未删除集合里。
        let exists = snippet_live(&tx, snippet.id)?;
        if !exists && snippet_count(&tx)? >= MAX_SNIPPETS {
            return Err(StoreError::SnippetLimitReached);
        }
        upsert_snippet_on(&tx, snippet, now_ms())?;
        tx.commit()?;
        Ok(())
    }

    /// 墓碑删除；不存在时视为成功（幂等）。
    pub fn delete_snippet(&mut self, id: SnippetId) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE snippets SET deleted = 1, updated_at = ?2 WHERE id = ?1 AND deleted = 0",
            params![id.to_string(), now_ms()],
        )?;
        Ok(())
    }

    // ---- 设置 ----

    /// 读设置值（JSON 文本）；不存在返回 `None`。设置的结构由上层（F7）定义，这里只存取。
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// 写设置值。`json` 必须是合法 JSON（表结构约定 value 是 JSON，写入时挡住坏数据，
    /// 否则导出与读取方都要各自处理）。
    pub fn set_setting(&mut self, key: &str, json: &str) -> Result<(), StoreError> {
        serde_json::from_str::<serde_json::Value>(json).map_err(|e| StoreError::InvalidData {
            detail: format!("setting {key:?} is not valid JSON: {e}"),
        })?;
        upsert_setting_on(&self.conn, key, json, now_ms())
    }

    /// 清空全部设置（"恢复默认设置"，store-import.md §9）。只动 `settings` 表，
    /// 不动会话、分组、Sender 命令与历史。
    pub fn reset_settings(&mut self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM settings", [])?;
        Ok(())
    }

    /// 全部设置，按键排序。导出用。
    pub(crate) fn settings_all(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

// ---- 以下函数接收 `&Connection`：`Transaction` 可解引用成它，导入时在同一事务里复用 ----

pub(crate) fn upsert_setting_on(
    conn: &Connection,
    key: &str,
    json: &str,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, json, now],
    )?;
    Ok(())
}

/// Rgb 转 `#rrggbb`。
pub(crate) fn color_to_hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// 解析严格的 `#rrggbb`；其他格式返回 `None`。
pub(crate) fn color_from_hex(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    })
}

fn kind_column(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Local => "local",
        SessionKind::Ssh => "ssh",
        SessionKind::Sftp => "sftp",
    }
}

fn line_ending_column(l: LineEnding) -> &'static str {
    match l {
        LineEnding::None => "none",
        LineEnding::Lf => "lf",
        LineEnding::CrLf => "crlf",
    }
}

fn line_ending_from_column(s: &str) -> Option<LineEnding> {
    match s {
        "none" => Some(LineEnding::None),
        "lf" => Some(LineEnding::Lf),
        "crlf" => Some(LineEnding::CrLf),
        _ => None,
    }
}

/// 作用域拆成 `(scope_kind, scope_ref)` 两列。
fn scope_columns(scope: &SnippetScope) -> (&'static str, Option<String>) {
    match scope {
        SnippetScope::Profile(id) => ("profile", Some(id.to_string())),
        SnippetScope::Group(id) => ("group", Some(id.to_string())),
        SnippetScope::Kind(SnippetKindScope::Local) => ("kind", Some("local".to_owned())),
        SnippetScope::Kind(SnippetKindScope::Ssh) => ("kind", Some("ssh".to_owned())),
        SnippetScope::Global => ("global", None),
    }
}

pub(crate) fn live_group_ids(conn: &Connection) -> Result<HashSet<GroupId>, StoreError> {
    Ok(groups_on(conn)?.into_iter().map(|g| g.id).collect())
}

pub(crate) fn live_profile_ids(conn: &Connection) -> Result<HashSet<ProfileId>, StoreError> {
    let mut stmt = conn.prepare("SELECT id FROM profiles WHERE deleted = 0")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids
        .iter()
        .filter_map(|s| ProfileId::from_str(s).ok())
        .collect())
}

pub(crate) fn groups_on(conn: &Connection) -> Result<Vec<Group>, StoreError> {
    let mut stmt =
        conn.prepare("SELECT id, name FROM groups WHERE deleted = 0 ORDER BY sort_key, name, id")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, name)| {
            Some(Group {
                id: GroupId::from_str(&id).ok()?,
                name,
            })
        })
        .collect())
}

pub(crate) fn upsert_group_on(conn: &Connection, g: &Group, now: i64) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO groups (id, name, sort_key, updated_at, deleted)
         VALUES (?1, ?2, (SELECT COALESCE(MAX(sort_key), 0) + 1 FROM groups), ?3, 0)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name, updated_at = excluded.updated_at, deleted = 0",
        params![g.id.to_string(), g.name, now],
    )?;
    Ok(())
}

pub(crate) fn profiles_on(conn: &Connection) -> Result<Vec<Profile>, StoreError> {
    let groups = live_group_ids(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, group_id, color, theme, config FROM profiles
         WHERE deleted = 0 ORDER BY sort_key, name, id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, name, group_id, color, theme, config) in rows {
        // 解析不了的行（id 损坏，或 config 来自更新版本程序的不认识的类型）不能让整个列表失败，
        // 也不能静默丢弃而不留痕迹：跳过并记日志，数据仍留在库里，升级后可恢复。
        let Ok(pid) = ProfileId::from_str(&id) else {
            tracing::warn!(%id, "skipping profile row with invalid id");
            continue;
        };
        let kind = match serde_json::from_str::<ProfileKind>(&config) {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(%id, error = %e, "skipping profile row with unreadable config");
                continue;
            }
        };
        // 读取时修正失效的分组引用，见模块文档。
        let group = group_id
            .and_then(|g| GroupId::from_str(&g).ok())
            .filter(|g| groups.contains(g));
        out.push(Profile {
            id: pid,
            name,
            color: color.as_deref().and_then(color_from_hex),
            group,
            theme,
            kind,
        });
    }
    Ok(out)
}

pub(crate) fn upsert_profile_on(
    conn: &Connection,
    p: &Profile,
    now: i64,
) -> Result<(), StoreError> {
    let config = serde_json::to_string(&p.kind).map_err(|e| StoreError::InvalidData {
        detail: format!("cannot serialize profile config: {e}"),
    })?;
    // kind 列必须与 config 的类型标签一致（schema.md §2.2），两者都由同一个 `p.kind` 派生。
    conn.execute(
        "INSERT INTO profiles
            (id, name, kind, group_id, color, theme, sort_key, config, updated_at, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                 (SELECT COALESCE(MAX(sort_key), 0) + 1 FROM profiles), ?7, ?8, 0)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name, kind = excluded.kind, group_id = excluded.group_id,
            color = excluded.color, theme = excluded.theme, config = excluded.config,
            updated_at = excluded.updated_at, deleted = 0",
        params![
            p.id.to_string(),
            p.name,
            kind_column(p.kind.kind()),
            p.group.map(|g| g.to_string()),
            p.color.map(color_to_hex),
            p.theme,
            config,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn snippets_on(conn: &Connection) -> Result<Vec<Snippet>, StoreError> {
    let groups = live_group_ids(conn)?;
    let profiles = live_profile_ids(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, text, line_ending, scope_kind, scope_ref FROM snippets
         WHERE deleted = 0 ORDER BY sort_key, name, id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, name, text, le, scope_kind, scope_ref) in rows {
        let (Ok(sid), Some(line_ending)) = (SnippetId::from_str(&id), line_ending_from_column(&le))
        else {
            tracing::warn!(%id, "skipping snippet row with invalid id or line ending");
            continue;
        };
        // 读取时修正失效的作用域引用：退回全局（store-import.md §4）。
        let scope = match (scope_kind.as_str(), scope_ref.as_deref()) {
            ("profile", Some(r)) => ProfileId::from_str(r)
                .ok()
                .filter(|p| profiles.contains(p))
                .map_or(SnippetScope::Global, SnippetScope::Profile),
            ("group", Some(r)) => GroupId::from_str(r)
                .ok()
                .filter(|g| groups.contains(g))
                .map_or(SnippetScope::Global, SnippetScope::Group),
            ("kind", Some("local")) => SnippetScope::Kind(SnippetKindScope::Local),
            ("kind", Some("ssh")) => SnippetScope::Kind(SnippetKindScope::Ssh),
            _ => SnippetScope::Global,
        };
        out.push(Snippet {
            id: sid,
            name,
            text,
            line_ending,
            scope,
        });
    }
    Ok(out)
}

pub(crate) fn upsert_snippet_on(
    conn: &Connection,
    s: &Snippet,
    now: i64,
) -> Result<(), StoreError> {
    let (scope_kind, scope_ref) = scope_columns(&s.scope);
    conn.execute(
        "INSERT INTO snippets
            (id, name, text, line_ending, scope_kind, scope_ref, sort_key, updated_at, deleted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                 (SELECT COALESCE(MAX(sort_key), 0) + 1 FROM snippets), ?7, 0)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name, text = excluded.text, line_ending = excluded.line_ending,
            scope_kind = excluded.scope_kind, scope_ref = excluded.scope_ref,
            updated_at = excluded.updated_at, deleted = 0",
        params![
            s.id.to_string(),
            s.name,
            s.text,
            line_ending_column(s.line_ending),
            scope_kind,
            scope_ref,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn snippet_live(conn: &Connection, id: SnippetId) -> Result<bool, StoreError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM snippets WHERE id = ?1 AND deleted = 0",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub(crate) fn snippet_count(conn: &Connection) -> Result<usize, StoreError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM snippets WHERE deleted = 0", [], |r| {
        r.get(0)
    })?;
    Ok(usize::try_from(n).unwrap_or(0))
}

fn group_live(conn: &Connection, id: GroupId) -> Result<bool, StoreError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM groups WHERE id = ?1 AND deleted = 0",
            [id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// 删除分组会波及的 Profile id 与 Sender 命令 id。
///
/// `delete_impact` 与 `delete_group` **都**调用它，而不是各写一份条件：
/// 契约要求预览的数字与实际删除的完全一致，共用一个查询是最不容易漂移的做法。
/// 波及的命令：作用域是该分组，或作用域是组内（未删除）Profile。
fn group_cascade(
    conn: &Connection,
    group: GroupId,
) -> Result<(Vec<String>, Vec<String>), StoreError> {
    let gid = group.to_string();
    let mut stmt = conn.prepare("SELECT id FROM profiles WHERE group_id = ?1 AND deleted = 0")?;
    let profiles = stmt
        .query_map([&gid], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut stmt = conn.prepare(
        "SELECT id FROM snippets WHERE deleted = 0 AND (
            (scope_kind = 'group' AND scope_ref = ?1)
            OR (scope_kind = 'profile' AND scope_ref IN
                (SELECT id FROM profiles WHERE group_id = ?1 AND deleted = 0)))",
    )?;
    let snippets = stmt
        .query_map([&gid], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((profiles, snippets))
}
