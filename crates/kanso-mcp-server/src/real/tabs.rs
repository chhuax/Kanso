//! MCP 打开的标签登记表（spec/mcp.md §3.3）：`标签 -> 打开它的客户端编号`，跨客户端共享。
//!
//! 只有这张表里的标签才算"由 MCP 打开"，`kanso_close_tab` 据此限制只能关自己打开的标签。
//! 客户端断开时它的条目全部移除（标签保留，变成普通标签），之后任何客户端都不能再关闭它。
//!
//! # 锁与回调
//!
//! 与 `Registry` 一样，delegate 回调在持表锁时发出，保证宿主看到的"打开 / 释放"顺序与表的变化一致
//! （回调契约禁止重入）。加锁顺序：`McpTabs` → `Registry`（登记时要读客户端信息）；`Registry` 从不反向取本表的锁。
//! 本表在每次 `start` 重建，不跨 stop/start 保留：停止时所有客户端都已断开，条目本来就要清掉。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use kanso_types::SessionId;

use super::registry::Registry;
use crate::McpServerDelegate;

#[derive(Default)]
pub(super) struct McpTabs {
    owners: Mutex<HashMap<SessionId, u64>>,
}

impl McpTabs {
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionId, u64>> {
        // 只是一张映射表，没有跨字段不变式，中毒后继续使用。
        self.owners.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 登记 `client_id` 打开了 `tab`，并回调 `on_session_opened`。返回 `false` 表示该客户端已经断开：
    /// 不登记也不回调，标签留作普通标签。
    ///
    /// 存在性检查、登记、回调在同一把表锁内完成：连接任务结束时先从 `Registry` 移除客户端、再取本表的锁清理
    /// （见 `conn::handle`）。若检查通过则登记一定先于清理，清理能看到它；若检查未通过则不会留下孤儿条目。
    /// 没有这层保证，"打开标签的调用恰好在客户端断开时完成"会让标签永远带着"由 MCP 打开"标记。
    pub(super) fn register(
        &self,
        registry: &Registry,
        delegate: &Arc<dyn McpServerDelegate>,
        tab: SessionId,
        client_id: u64,
    ) -> bool {
        let mut owners = self.lock();
        let Some(client) = registry.client(client_id) else {
            return false;
        };
        owners.insert(tab, client_id);
        delegate.on_session_opened(tab, client);
        true
    }

    /// 标签是不是由 MCP 打开的（任一客户端）。
    pub(super) fn contains(&self, tab: SessionId) -> bool {
        self.lock().contains_key(&tab)
    }

    /// 仅当标签是 `client_id` 打开的才移除并返回 `true`。检查与移除原子完成，
    /// 两个并发的 `kanso_close_tab` 只会有一个成功去关闭标签。
    pub(super) fn remove_if_owner(&self, tab: SessionId, client_id: u64) -> bool {
        let mut owners = self.lock();
        if owners.get(&tab) == Some(&client_id) {
            owners.remove(&tab);
            true
        } else {
            false
        }
    }

    /// 清掉已被用户关闭的标签的条目；`exists` 判断标签是否还在。
    pub(super) fn prune(&self, exists: impl Fn(SessionId) -> bool) {
        self.lock().retain(|tab, _| exists(*tab));
    }

    /// 客户端断开：移除它打开的全部条目，并对仍存在的标签回调 `on_session_released`
    /// （标签保留，变成普通标签）。已被用户关掉的标签没有可去标记的对象，不回调。
    pub(super) fn release_client(
        &self,
        delegate: &Arc<dyn McpServerDelegate>,
        client_id: u64,
        exists: impl Fn(SessionId) -> bool,
    ) {
        let mut owners = self.lock();
        let mut released: Vec<SessionId> = owners
            .iter()
            .filter(|(_, owner)| **owner == client_id)
            .map(|(tab, _)| *tab)
            .collect();
        // HashMap 无序；按标签 id 排序，让回调顺序稳定。
        released.sort_by_key(|tab| tab.raw());
        for tab in released {
            owners.remove(&tab);
            if exists(tab) {
                delegate.on_session_released(tab);
            }
        }
    }
}
