//! 命令历史（补全用，store-import.md §5）。

use rusqlite::params;

use crate::schema::StoreError;
use crate::store::Store;

/// 历史条数上限。
pub const HISTORY_CAPACITY: usize = 5000;

/// 单条命令的最大字符数（按字符，不是字节）；超过的多半是误捕获的输出，不记录。
pub const HISTORY_MAX_CHARS: usize = 500;

/// 一条历史记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    /// 命令文本。
    pub command: String,
    /// 最近使用时间（Unix 毫秒）。
    pub last_used_at: i64,
    /// 使用次数。
    pub use_count: i64,
}

impl Store {
    /// 记录一次命令执行；返回是否真的记录了。
    ///
    /// 不记录：空白串、以空格开头（用户用它显式表示"别记"，terminal.md §5.2）、
    /// 超过 [`HISTORY_MAX_CHARS`] 个字符。
    ///
    /// 写入很小（每执行一条命令都会调用）：已有则只更新一行；新命令插入一行，超过上限时
    /// 删掉 `last_used_at` 最小的若干行（最久没用的，不是最早记录的），绝不重写全表。
    /// 先 UPDATE 再按需 INSERT，是为了只在"新增"时才付出计数与淘汰的开销。
    pub fn record_history(&mut self, command: &str, now_ms: i64) -> Result<bool, StoreError> {
        self.record_history_on_host(command, "", now_ms)
    }

    /// 记录命令及使用过的主机，同文本共用一条历史。
    pub fn record_history_on_host(
        &mut self,
        command: &str,
        host: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        if command.trim().is_empty()
            || command.starts_with(' ')
            || command.chars().count() > HISTORY_MAX_CHARS
        {
            return Ok(false);
        }
        let tx = self.conn.transaction()?;
        let updated = tx.execute(
            "UPDATE history SET last_used_at = ?2, use_count = use_count + 1 WHERE command = ?1",
            params![command, now_ms],
        )?;
        if updated == 0 {
            tx.execute(
                "INSERT INTO history (command, last_used_at, use_count) VALUES (?1, ?2, 1)",
                params![command, now_ms],
            )?;
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?;
            let excess = count - i64::try_from(self.history_cap).unwrap_or(i64::MAX);
            if excess > 0 {
                // 同一毫秒的并列项用 command 打破平局，保证淘汰结果确定。
                tx.execute(
                    "DELETE FROM history WHERE command IN
                       (SELECT command FROM history ORDER BY last_used_at, command LIMIT ?1)",
                    [excess],
                )?;
            }
        }
        if !host.is_empty() {
            tx.execute(
                "UPDATE history SET hosts = json_insert(hosts, '$[#]', ?2)
                 WHERE command = ?1 AND NOT EXISTS (SELECT 1 FROM json_each(hosts) WHERE value = ?2)",
                params![command, host],
            )?;
        }
        tx.commit()?;
        *self.completion_history_cache.get_mut() = None;
        Ok(true)
    }

    /// 按前缀查询历史（区分大小写），最近使用的在前，最多 `limit` 条；`prefix` 为空返回全部。
    ///
    /// 不用 `LIKE`：它对 ASCII 不区分大小写，且前缀里的 `%` `_` 要转义；
    /// `substr` 按字符截取比较，既精确又没有转义问题。
    pub fn history(&self, prefix: &str, limit: usize) -> Result<Vec<HistoryEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT command, last_used_at, use_count FROM history
             WHERE substr(command, 1, ?2) = ?1
             ORDER BY last_used_at DESC, use_count DESC, command
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![
                prefix,
                i64::try_from(prefix.chars().count()).unwrap_or(i64::MAX),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |r| {
                Ok(HistoryEntry {
                    command: r.get(0)?,
                    last_used_at: r.get(1)?,
                    use_count: r.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 清空全部历史（"编辑 → 清除命令历史"）。
    pub fn clear_history(&mut self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM history", [])?;
        *self.completion_history_cache.get_mut() = None;
        Ok(())
    }

    /// 读取补全历史，并标记是否在指定主机使用过。
    pub fn completion_history(&self, host: &str) -> Result<Vec<(HistoryEntry, bool)>, StoreError> {
        let mut cache = self.completion_history_cache.borrow_mut();
        if cache.is_none() {
            let mut statement = self
                .conn
                .prepare("SELECT command, last_used_at, use_count, hosts FROM history")?;
            let rows = statement.query_map([], |row| {
                let hosts: String = row.get(3)?;
                let hosts: Vec<String> = serde_json::from_str(&hosts).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok((
                    HistoryEntry {
                        command: row.get(0)?,
                        last_used_at: row.get(1)?,
                        use_count: row.get(2)?,
                    },
                    hosts,
                ))
            })?;
            *cache = Some(rows.collect::<Result<_, _>>()?);
        }
        Ok(cache
            .as_ref()
            .expect("补全历史快照已载入")
            .iter()
            .map(|(entry, hosts)| (entry.clone(), hosts.iter().any(|name| name == host)))
            .collect())
    }
}
