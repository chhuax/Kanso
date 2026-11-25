//! 标签写操作的互斥、用户接管冷却与活动标识（spec/mcp.md §4.1、§4.2 最后一条、§4.3）。
//!
//! 三样东西都按标签（`SessionId`）记录，**跨客户端共享**：同一标签同一时刻只允许一个 MCP 写操作，
//! 不管调用来自哪个客户端；冷却同理（用户在标签里接管后，任何客户端都要等）。
//! 状态只在内存里：标签 id 在一次 App 运行期间不复用，所以不需要随标签关闭清理，
//! 过期的冷却记录在下一次获取写权时顺带清掉。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kanso_types::SessionId;

use crate::{McpActivity, McpServerDelegate};

/// 用户接管后拒绝新写操作的时长（spec/mcp.md §4.3）。
pub(super) const TAKEOVER_COOLDOWN: Duration = Duration::from_secs(10);

/// 时间来源。生产用 `Instant::now`；测试注入假时钟，免得为等 10 秒冷却真去 sleep。
/// 只需要单调时间，所以用 `Instant` 而不是墙上时间：系统改时间不应让冷却提前或延后结束。
pub type ClockFn = Arc<dyn Fn() -> Instant + Send + Sync>;

/// 获取写权被拒的原因。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GateDenied {
    /// 另一个 MCP 调用正在操作这个标签。
    Busy,
    /// 用户刚接管过这个标签，冷却还没结束。
    Cooldown { remaining: Duration },
}

#[derive(Default)]
struct State {
    busy: HashSet<SessionId>,
    /// 标签 -> 冷却结束的时刻。
    cooldown: HashMap<SessionId, Instant>,
}

/// 全部标签共用的写权登记处，随服务的一次 `start` 创建。
pub(super) struct TabGate {
    clock: ClockFn,
    state: Mutex<State>,
}

impl TabGate {
    pub(super) fn new(clock: ClockFn) -> Arc<TabGate> {
        Arc::new(TabGate {
            clock,
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // 状态只是两个集合，没有跨字段不变式；中毒后继续使用。
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 取得标签的写权。返回的租约在 `Drop` 时释放：调用被取消（future 被丢弃）、连接断开、
    /// 任务 panic 都会走到 `Drop`，不会让标签永远处于"被占用"。
    ///
    /// 先判断冷却再判断占用：冷却只在写调用返回之后才设置，此时锁已释放，两者不会同时成立；
    /// 但若同时成立，告诉 AI "用户接管了" 比 "别人在操作" 更有用。
    pub(super) fn acquire(self: &Arc<Self>, session: SessionId) -> Result<WriteLease, GateDenied> {
        let now = (self.clock)();
        let mut st = self.lock();
        st.cooldown.retain(|_, until| *until > now);
        if let Some(reason) = write_blocker(&st, session, now) {
            return Err(reason);
        }
        st.busy.insert(session);
        Ok(WriteLease {
            gate: self.clone(),
            session,
        })
    }

    /// 查询当前写入阻塞原因，不取得写锁。
    pub(super) fn blocker(&self, session: SessionId) -> Option<GateDenied> {
        write_blocker(&self.lock(), session, (self.clock)())
    }

    /// 记录用户接管：此后 [`TAKEOVER_COOLDOWN`] 内拒绝这个标签的新写操作。
    pub(super) fn took_over(&self, session: SessionId) {
        let until = (self.clock)() + TAKEOVER_COOLDOWN;
        self.lock().cooldown.insert(session, until);
    }
}

fn write_blocker(st: &State, session: SessionId, now: Instant) -> Option<GateDenied> {
    if let Some(until) = st.cooldown.get(&session).filter(|until| **until > now) {
        return Some(GateDenied::Cooldown {
            remaining: *until - now,
        });
    }
    st.busy.contains(&session).then_some(GateDenied::Busy)
}

/// 一个标签的写权；`Drop` 时释放。
pub(super) struct WriteLease {
    gate: Arc<TabGate>,
    session: SessionId,
}

impl Drop for WriteLease {
    fn drop(&mut self) {
        self.gate.lock().busy.remove(&self.session);
    }
}

/// 标签上的 MCP 活动标识：创建时回调 `Some`，`Drop` 时回调 `None`（spec/mcp.md §4.1）。
///
/// 用 `Drop` 是为了调用被取消、连接断开时标识也能撤掉，否则标签上会一直显示 MCP 在操作。
/// 使用方必须让它**先于** [`WriteLease`] 释放（先声明租约、后声明它），这样同一标签上
/// 前一次的 `None` 一定先于下一次的 `Some` 发出，宿主看到的顺序不会颠倒。
pub(super) struct ActivityGuard {
    delegate: Arc<dyn McpServerDelegate>,
    session: SessionId,
}

impl ActivityGuard {
    pub(super) fn start(
        delegate: Arc<dyn McpServerDelegate>,
        session: SessionId,
        activity: McpActivity,
    ) -> ActivityGuard {
        delegate.on_activity(session, Some(activity));
        ActivityGuard { delegate, session }
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.delegate.on_activity(self.session, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 可手动拨动的假时钟。
    fn fake_clock() -> (ClockFn, Arc<Mutex<Instant>>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let handle = now.clone();
        (Arc::new(move || *handle.lock().unwrap()), now)
    }

    fn tab(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    #[test]
    fn second_acquire_is_busy_and_drop_releases() {
        let (clock, _) = fake_clock();
        let gate = TabGate::new(clock);
        let lease = gate.acquire(tab(1)).unwrap();
        assert_eq!(gate.blocker(tab(1)), Some(GateDenied::Busy));
        assert_eq!(gate.acquire(tab(1)).err(), Some(GateDenied::Busy));
        // 别的标签不受影响。
        let _other = gate.acquire(tab(2)).unwrap();
        drop(lease);
        assert!(gate.acquire(tab(1)).is_ok());
    }

    #[test]
    fn takeover_cooldown_expires_with_the_clock() {
        let (clock, now) = fake_clock();
        let gate = TabGate::new(clock);
        gate.took_over(tab(1));
        *now.lock().unwrap() += Duration::from_secs(4);
        match gate.acquire(tab(1)) {
            Err(GateDenied::Cooldown { remaining }) => {
                assert_eq!(remaining, Duration::from_secs(6));
            }
            other => panic!("expected cooldown, got {:?}", other.err()),
        }
        // 其他标签不受冷却影响。
        assert!(gate.acquire(tab(2)).is_ok());
        *now.lock().unwrap() += Duration::from_secs(6);
        assert_eq!(gate.blocker(tab(1)), None);
        assert!(gate.acquire(tab(1)).is_ok());
    }
}
