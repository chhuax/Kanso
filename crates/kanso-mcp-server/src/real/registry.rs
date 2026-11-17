//! 已连接客户端的登记、令牌与回调。
//!
//! 令牌校验与登记在**同一把锁内**完成：否则"校验通过 → 令牌被轮换 → 才登记"会让一个用旧令牌的连接
//! 溜过 `set_token` 的断开。所有回调都在持锁时发出，保证宿主看到的列表顺序与状态变化一致
//! （理由同 `mock.rs` 模块文档；回调契约禁止重入，所以持锁回调是安全的）。

use std::sync::{Arc, Mutex, MutexGuard};

use kanso_types::secret::SecretString;
use tokio_util::sync::CancellationToken;

use crate::{McpClientInfo, McpServerDelegate, now_ms};

/// 同时连接的客户端上限（spec/mcp.md §1）。
pub(super) const MAX_CLIENTS: usize = 16;

struct Entry {
    info: McpClientInfo,
    /// 取消后该客户端的连接任务结束并关闭 socket。
    cancel: CancellationToken,
}

struct Inner {
    /// 服务是否在接受连接；停止后晚到的连接不得再登记。
    open: bool,
    token: SecretString,
    next_id: u64,
    clients: Vec<Entry>,
}

/// 登记结果。
pub(super) enum Admission {
    Admitted {
        id: u64,
        cancel: CancellationToken,
    },
    InvalidToken,
    TooMany,
    /// 服务已停止。
    Closed,
}

pub(super) struct Registry {
    delegate: Arc<dyn McpServerDelegate>,
    inner: Mutex<Inner>,
}

impl Registry {
    pub(super) fn new(delegate: Arc<dyn McpServerDelegate>) -> Registry {
        Registry {
            delegate,
            inner: Mutex::new(Inner {
                open: false,
                token: SecretString::new(""),
                next_id: 1,
                clients: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 状态里没有需要在 panic 后保护的跨字段不变式，中毒后继续使用。
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self, g: &Inner) {
        self.delegate
            .on_clients_changed(g.clients.iter().map(|c| c.info.clone()).collect());
    }

    /// 开始接受连接，使用给定令牌。
    pub(super) fn open(&self, token: SecretString) {
        let mut g = self.lock();
        g.open = true;
        g.token = token;
    }

    /// 停止接受连接并断开全部客户端；有客户端被断开时回调一次。
    pub(super) fn close(&self) {
        let mut g = self.lock();
        g.open = false;
        // 没有客户端时列表没有变化，不回调。
        if Self::drop_all(&mut g) {
            self.notify(&g);
        }
    }

    fn drop_all(g: &mut Inner) -> bool {
        let had = !g.clients.is_empty();
        for c in g.clients.drain(..) {
            c.cancel.cancel();
        }
        had
    }

    /// 更换令牌并断开全部现有客户端（它们用的都是旧令牌）。没有在运行时什么也不做。
    pub(super) fn set_token(&self, token: SecretString) {
        let mut g = self.lock();
        if !g.open {
            return;
        }
        g.token = token;
        if Self::drop_all(&mut g) {
            self.notify(&g);
        }
    }

    /// 校验令牌并登记客户端；通过时回调新的客户端列表。
    pub(super) fn admit(&self, presented: &str) -> Admission {
        let mut g = self.lock();
        if !g.open {
            return Admission::Closed;
        }
        // 空令牌一律拒绝：否则两边都为空时 constant_time_eq 会放行，等于没有令牌。
        if presented.is_empty()
            || g.token.expose().is_empty()
            || !constant_time_eq(presented.as_bytes(), g.token.expose().as_bytes())
        {
            return Admission::InvalidToken;
        }
        if g.clients.len() >= MAX_CLIENTS {
            return Admission::TooMany;
        }
        let id = g.next_id;
        g.next_id += 1;
        let cancel = CancellationToken::new();
        g.clients.push(Entry {
            info: McpClientInfo {
                id,
                name: String::new(),
                connected_at_ms: now_ms(),
                calls: 0,
                own_session: None,
            },
            cancel: cancel.clone(),
        });
        self.notify(&g);
        Admission::Admitted { id, cancel }
    }

    /// 取消并移除客户端；它存在时回调。连接任务结束与 `disconnect` 都走这里，
    /// 不存在（已被别处移除）时静默，所以不会重复回调。
    pub(super) fn remove(&self, id: u64) {
        let mut g = self.lock();
        let Some(pos) = g.clients.iter().position(|c| c.info.id == id) else {
            return;
        };
        g.clients.remove(pos).cancel.cancel();
        self.notify(&g);
    }

    /// 记录客户端自报的名称（`clientInfo.name`）。
    pub(super) fn set_name(&self, id: u64, name: String) {
        self.update(id, |c| c.name = name);
    }

    /// 工具调用计数加一，返回客户端当前的名称（审计要用）；客户端已不存在时为空串。
    pub(super) fn add_call(&self, id: u64) -> String {
        let mut g = self.lock();
        let Some(c) = g.clients.iter_mut().find(|c| c.info.id == id) else {
            return String::new();
        };
        c.info.calls += 1;
        let name = c.info.name.clone();
        self.notify(&g);
        name
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut McpClientInfo)) {
        let mut g = self.lock();
        if let Some(c) = g.clients.iter_mut().find(|c| c.info.id == id) {
            f(&mut c.info);
            self.notify(&g);
        }
    }

    pub(super) fn list(&self) -> Vec<McpClientInfo> {
        self.lock().clients.iter().map(|c| c.info.clone()).collect()
    }
}

/// 常量时间比较：长度不同直接判不等（令牌长度不是秘密），长度相同时逐字节异或累积，
/// 不因第一个不同字节提前返回，避免通过响应时间逐字节猜令牌。不要换成 `==`。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn compares_tokens() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }
}
