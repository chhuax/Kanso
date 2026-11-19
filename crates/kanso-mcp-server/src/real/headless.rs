//! 无界面连接管理器（spec/mcp.md §2.4）：按 profile 缓存 core 的无界面会话，供 `kanso_exec` 与 SFTP 工具复用。
//!
//! 范围是整个服务（跨客户端共享），每次 `start` 重建，`stop` 时 [`HeadlessManager::close_all`] 全部关闭。
//!
//! # 并发与顺序
//!
//! - 同一 profile 的 `acquire` 由一把 per-profile 的异步互斥串行化：否则两个并发调用都看到"没有条目"，
//!   各自 `mcp_connect`，会建出两条连接，其中一条没人管。互斥在整个"查缓存 → 建连接 → 登记"期间持有。
//!   不同 profile 之间互不阻塞。
//! - 条目表（`entries`）是叶子锁，只在内存里改字段，从不在持锁时 `await`，也不在持锁时调用 core。"进行中调用数"与"到期判断"
//!   在同一把锁里读写，所以 reaper 不会关掉一个刚被复用的连接（复用先加计数，reaper 再看到就不到期）。
//! - [`Lease`] 的 `Drop` 才减计数并刷新 `last_used`：调用被取消（future 被丢弃）、连接断开、panic 都会走到，
//!   不会让条目永远处于"进行中"而无法释放。
//!
//! # 释放
//!
//! 自建连接（`borrowed == false`）在没有进行中调用、且距最后一次使用满 [`IDLE_RELEASE`] 时由 reaper 关闭。
//! 借用的连接不计时：用户关掉该连接上最后一个标签时，由 core 关闭这个无界面会话，下次 `acquire` 发现条目失效。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kanso_core::mcp::{ConnectMode, McpConnectOutcome, UserNeed};
use kanso_core::{Core, CoreError};
use kanso_types::event::SessionState;
use kanso_types::{ProfileId, SessionId};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::gate::ClockFn;

/// 自建连接最后一次使用后保留多久（spec/mcp.md §2.4）。
pub(super) const IDLE_RELEASE: Duration = Duration::from_secs(5 * 60);

struct Entry {
    session: SessionId,
    /// 是否借用了用户建立的连接；借用的不计时。
    borrowed: bool,
    last_used: Instant,
    /// 持有 [`Lease`] 的调用数。
    in_flight: usize,
}

/// [`HeadlessManager::acquire`] 的结果。
pub(super) enum Acquired {
    Lease(Lease),
    /// `ReuseOnly` 下没有可用连接。
    NotConnected,
    /// 新建连接需要人参与。
    NeedsUser(UserNeed),
}

pub(super) struct HeadlessManager {
    core: Arc<Core>,
    clock: ClockFn,
    /// 自建连接的空闲时限；生产用 [`IDLE_RELEASE`]，测试可以缩短。
    idle: Duration,
    entries: Mutex<HashMap<ProfileId, Entry>>,
    /// 每个 profile 一把互斥，见模块文档。表只增不减：profile 数量有限，条目很小。
    profile_locks: Mutex<HashMap<ProfileId, Arc<tokio::sync::Mutex<()>>>>,
    /// 条目变化（新建、释放租约）时唤醒 reaper 重算最早到期时刻。用 `notify_one`：没有等待者时
    /// 会存一个许可，所以"算完到期时刻、开始等待之前"发生的变化不会丢。
    wake: Notify,
}

/// 一次调用对无界面会话的占用。持有期间条目不会被空闲释放；`Drop` 时释放并刷新 `last_used`。
pub(super) struct Lease {
    manager: Arc<HeadlessManager>,
    profile: ProfileId,
    session: SessionId,
}

impl Lease {
    pub(super) fn session(&self) -> SessionId {
        self.session
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.manager.release(self.profile, self.session);
    }
}

impl HeadlessManager {
    pub(super) fn new(core: Arc<Core>, clock: ClockFn, idle: Duration) -> Arc<HeadlessManager> {
        Arc::new(HeadlessManager {
            core,
            clock,
            idle,
            entries: Mutex::new(HashMap::new()),
            profile_locks: Mutex::new(HashMap::new()),
            wake: Notify::new(),
        })
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<ProfileId, Entry>> {
        // 条目只是独立字段，没有需要在 panic 后保护的跨字段不变式，中毒后继续使用。
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn profile_lock(&self, profile: ProfileId) -> Arc<tokio::sync::Mutex<()>> {
        self.profile_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(profile)
            .or_default()
            .clone()
    }

    /// 取得 `profile` 的无界面会话：有可用条目就复用，否则按 `mode` 向 core 建立。
    ///
    /// 调用方按**会话当前的级别**选 `mode`（只读 → `ReuseOnly`，完全 → `NonInteractive`，spec/mcp.md §2.4），
    /// 并负责在调用期间持有返回的 [`Lease`]。future 取消安全：丢弃它不会留下计数。
    pub(super) async fn acquire(
        self: &Arc<Self>,
        profile: ProfileId,
        mode: ConnectMode,
    ) -> Result<Acquired, CoreError> {
        let gate = self.profile_lock(profile);
        let _serial = gate.lock().await;
        if let Some(lease) = self.reuse(profile) {
            return Ok(Acquired::Lease(lease));
        }
        match self.core.mcp_connect(profile, mode).await? {
            McpConnectOutcome::Connected { session, borrowed } => {
                self.entries().insert(
                    profile,
                    Entry {
                        session,
                        borrowed,
                        last_used: (self.clock)(),
                        in_flight: 1,
                    },
                );
                self.wake.notify_one();
                Ok(Acquired::Lease(Lease {
                    manager: self.clone(),
                    profile,
                    session,
                }))
            }
            McpConnectOutcome::NotConnected => Ok(Acquired::NotConnected),
            McpConnectOutcome::NeedsUser(need) => Ok(Acquired::NeedsUser(need)),
        }
    }

    /// 条目仍可用就占用它；已失效（会话没了或已不是已连接状态）就移除并关闭，返回 `None`。
    ///
    /// 失效的例子：借用的连接被用户关掉，或自建连接被服务器断开。失效条目也调用 `close_session`：
    /// 会话已不存在时它什么也不做；还以"已关闭"状态留在 core 里时借此清掉，避免残留。
    ///
    /// 查询状态进入 core，所以不在持 `entries` 锁时做：先取出 session id、放锁、查询，再重新加锁，
    /// 核对条目仍是同一个 session 才加计数或移除。调用方持有 profile 锁，条目只可能被 reaper 移除；
    /// 核对不通过就当作没有可复用的条目返回 `None`，由调用方新建连接。
    fn reuse(self: &Arc<Self>, profile: ProfileId) -> Option<Lease> {
        let session = self.entries().get(&profile)?.session;
        let usable = self
            .core
            .mcp_session_status(session)
            .is_some_and(|s| s.info.state == SessionState::Connected);
        let stale = {
            let mut entries = self.entries();
            let entry = entries.get_mut(&profile)?;
            if entry.session != session {
                return None;
            }
            if usable {
                entry.in_flight += 1;
                return Some(Lease {
                    manager: self.clone(),
                    profile,
                    session,
                });
            }
            entries.remove(&profile)
        };
        if let Some(stale) = stale {
            self.core.close_session(stale.session);
        }
        None
    }

    fn release(&self, profile: ProfileId, session: SessionId) {
        {
            let mut entries = self.entries();
            // 按会话 id 核对：条目在调用期间可能已失效并被新连接取代，不能把新条目的计数减掉。
            if let Some(entry) = entries.get_mut(&profile)
                && entry.session == session
            {
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.last_used = (self.clock)();
            }
        }
        self.wake.notify_one();
    }

    /// 关闭已到期的自建连接，返回下一个到期时刻（没有需要计时的条目为 `None`）。
    ///
    /// 到期 = 非借用、没有进行中调用、距 `last_used` 满 `idle`。判断与移除在同一把锁内完成，
    /// 与 [`reuse`](Self::reuse) 的"加计数"互斥。`close_session` 放到锁外：它进入 core。
    pub(super) fn reap_expired(&self) -> Option<Instant> {
        let now = (self.clock)();
        let mut next: Option<Instant> = None;
        let mut expired = Vec::new();
        {
            let mut entries = self.entries();
            entries.retain(|_, entry| {
                if entry.borrowed || entry.in_flight > 0 {
                    return true;
                }
                let due = entry.last_used + self.idle;
                if due <= now {
                    expired.push(entry.session);
                    false
                } else {
                    next = Some(next.map_or(due, |n| n.min(due)));
                    true
                }
            });
        }
        for session in expired {
            self.core.close_session(session);
        }
        next
    }

    /// 后台任务：按最早到期时刻睡眠，条目变化时被唤醒重算，不做固定频率的空转。
    /// `stop` 触发后退出；退出前不关连接，那是 [`close_all`](Self::close_all) 的事。
    pub(super) async fn run_reaper(self: Arc<Self>, stop: CancellationToken) {
        loop {
            let next = self.reap_expired();
            let sleep = async {
                match next {
                    // 睡眠时长按注入的时钟算；生产里就是真实的单调时钟。
                    Some(due) => {
                        tokio::time::sleep(due.saturating_duration_since((self.clock)())).await
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = stop.cancelled() => return,
                () = self.wake.notified() => {}
                () = sleep => {}
            }
        }
    }

    /// 服务停止时关闭全部无界面会话并清空表，借用的与自建的都关。
    /// 对借用的，`close_session` 只关 MCP 自己的那个无界面会话，不影响用户的标签。
    pub(super) fn close_all(&self) {
        let drained: Vec<Entry> = self.entries().drain().map(|(_, e)| e).collect();
        for entry in drained {
            self.core.close_session(entry.session);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries().len()
    }
}

#[cfg(test)]
mod tests {
    use kanso_core::{CollectingEventSink, OpenTarget, Platform};
    use kanso_types::GridSize;

    use super::*;

    struct Env {
        core: Arc<Core>,
        manager: Arc<HeadlessManager>,
        now: Arc<Mutex<Instant>>,
        web: ProfileId,
    }

    fn env() -> Env {
        let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
            CollectingEventSink::new(),
        ))));
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock: ClockFn = {
            let now = now.clone();
            Arc::new(move || *now.lock().unwrap())
        };
        let web = core
            .profiles()
            .into_iter()
            .find(|p| p.name == "web-01")
            .unwrap()
            .id;
        Env {
            manager: HeadlessManager::new(core.clone(), clock, IDLE_RELEASE),
            core,
            now,
            web,
        }
    }

    impl Env {
        fn advance(&self, by: Duration) {
            *self.now.lock().unwrap() += by;
        }

        async fn lease(&self, mode: ConnectMode) -> Lease {
            match self.manager.acquire(self.web, mode).await.unwrap() {
                Acquired::Lease(l) => l,
                _ => panic!("expected a lease"),
            }
        }

        fn alive(&self, session: SessionId) -> bool {
            self.core.mcp_session_status(session).is_some()
        }

        fn open_user_tab(&self) -> SessionId {
            self.core
                .open_session(
                    OpenTarget::Profile(self.web),
                    GridSize { cols: 80, rows: 24 },
                )
                .unwrap()
        }
    }

    #[tokio::test]
    async fn reuses_the_cached_session() {
        let e = env();
        let a = e.lease(ConnectMode::NonInteractive).await;
        let first = a.session();
        drop(a);
        // 只读级别（ReuseOnly）也能复用 MCP 自己建的连接。
        let b = e.lease(ConnectMode::ReuseOnly).await;
        assert_eq!(b.session(), first);
        assert_eq!(e.manager.len(), 1);
    }

    #[tokio::test]
    async fn reuse_only_without_a_connection_is_not_connected() {
        let e = env();
        let r = e.manager.acquire(e.web, ConnectMode::ReuseOnly).await;
        assert!(matches!(r, Ok(Acquired::NotConnected)));
        assert_eq!(e.manager.len(), 0);
    }

    #[tokio::test]
    async fn rebuilds_after_a_self_built_session_disappears() {
        let e = env();
        let first = e.lease(ConnectMode::NonInteractive).await.session();
        e.core.close_session(first);
        let second = e.lease(ConnectMode::NonInteractive).await.session();
        assert_ne!(first, second);
        assert!(e.alive(second));
    }

    #[tokio::test]
    async fn borrowed_session_closed_by_the_user_is_not_reused() {
        let e = env();
        let tab = e.open_user_tab();
        let lease = e.lease(ConnectMode::ReuseOnly).await;
        let borrowed = lease.session();
        drop(lease);
        // 用户关闭最后一个标签：core 一并关闭借用的无界面会话。
        e.core.close_session(tab);
        assert!(!e.alive(borrowed));
        let r = e.manager.acquire(e.web, ConnectMode::ReuseOnly).await;
        assert!(matches!(r, Ok(Acquired::NotConnected)));
        assert_eq!(e.manager.len(), 0, "失效条目应被移除");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_acquires_connect_only_once() {
        // mock 的连接是瞬时的，只能靠多线程并发撞窗口：没有 per-profile 互斥时，
        // 两个任务可能同时看到"没有条目"而各建一条。
        for _ in 0..50 {
            let e = env();
            let barrier = Arc::new(tokio::sync::Barrier::new(8));
            let tasks: Vec<_> = (0..8)
                .map(|_| {
                    let (manager, web, barrier) = (e.manager.clone(), e.web, barrier.clone());
                    tokio::spawn(async move {
                        barrier.wait().await;
                        match manager.acquire(web, ConnectMode::NonInteractive).await {
                            Ok(Acquired::Lease(l)) => l,
                            _ => panic!("expected a lease"),
                        }
                    })
                })
                .collect();
            let mut sessions = Vec::new();
            let mut leases = Vec::new();
            for t in tasks {
                let lease = t.await.unwrap();
                sessions.push(lease.session());
                leases.push(lease);
            }
            sessions.dedup();
            assert_eq!(sessions.len(), 1, "并发 acquire 建出了多条连接");
        }
    }

    #[tokio::test]
    async fn self_built_session_is_closed_after_five_idle_minutes() {
        let e = env();
        let session = e.lease(ConnectMode::NonInteractive).await.session();
        e.advance(IDLE_RELEASE - Duration::from_secs(1));
        assert!(e.manager.reap_expired().is_some());
        assert!(e.alive(session));
        e.advance(Duration::from_secs(1));
        assert_eq!(e.manager.reap_expired(), None);
        assert!(!e.alive(session));
        assert_eq!(e.manager.len(), 0);
    }

    #[tokio::test]
    async fn borrowed_session_is_never_timed() {
        let e = env();
        let _tab = e.open_user_tab();
        let session = e.lease(ConnectMode::ReuseOnly).await.session();
        e.advance(IDLE_RELEASE * 10);
        assert_eq!(e.manager.reap_expired(), None, "借用的条目没有到期时刻");
        assert!(e.alive(session));
        assert_eq!(e.manager.len(), 1);
    }

    #[tokio::test]
    async fn in_flight_call_blocks_release_and_release_restarts_the_clock() {
        let e = env();
        let lease = e.lease(ConnectMode::NonInteractive).await;
        let session = lease.session();
        e.advance(IDLE_RELEASE * 2);
        e.manager.reap_expired();
        assert!(e.alive(session), "进行中的调用不能被释放");
        drop(lease);
        // 计时从调用结束时重新算起。
        e.advance(IDLE_RELEASE - Duration::from_secs(1));
        e.manager.reap_expired();
        assert!(e.alive(session));
        e.advance(Duration::from_secs(1));
        e.manager.reap_expired();
        assert!(!e.alive(session));
    }

    #[tokio::test]
    async fn close_all_closes_self_built_and_borrowed_sessions() {
        let e = env();
        let tab = e.open_user_tab();
        let borrowed = e.lease(ConnectMode::ReuseOnly).await.session();
        e.manager.close_all();
        assert!(!e.alive(borrowed));
        assert!(e.alive(tab), "用户的标签不受影响");
        assert_eq!(e.manager.len(), 0);
    }

    #[tokio::test]
    async fn reaper_task_closes_idle_sessions_using_the_real_clock() {
        let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
            CollectingEventSink::new(),
        ))));
        let web = core
            .profiles()
            .into_iter()
            .find(|p| p.name == "web-01")
            .unwrap()
            .id;
        let clock: ClockFn = Arc::new(Instant::now);
        let manager = HeadlessManager::new(core.clone(), clock, Duration::from_millis(50));
        let stop = CancellationToken::new();
        let reaper = tokio::spawn(manager.clone().run_reaper(stop.clone()));
        let session = match manager.acquire(web, ConnectMode::NonInteractive).await {
            Ok(Acquired::Lease(l)) => l.session(),
            _ => panic!("expected a lease"),
        };
        for _ in 0..200 {
            if core.mcp_session_status(session).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(core.mcp_session_status(session).is_none());
        stop.cancel();
        reaper.await.unwrap();
    }
}
