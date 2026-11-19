//! MCP 的连接复用、非交互建连与借用连接的释放（spec/mcp.md §2.4，architecture §16.3）。
//!
//! # 概念
//!
//! - **无界面会话**：`mcp_connect` 返回的会话，机制同纯 SFTP 会话，不在 `sessions()` 里，登记在
//!   `RealCore::headless` 表里，值为 `borrowed`。
//! - **借用**（`true`）：无界面会话共享的是用户建立的连接。用户关闭该连接上最后一个**可见**会话时，
//!   core 立即关闭这些借用会话，连接随之断开。
//! - **自建**（`false`）：MCP 自己新建的连接（或共享自建连接的会话）。不受上面的释放影响，
//!   由 `kanso-mcp-server` 按 5 分钟空闲关闭。
//! - **可复用连接**：按 Profile 找，取 `Connection::id` 最大（最近建立）且仍存活的那条。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use kanso_session::{OpenRequest, Session, SessionManager};
use kanso_types::event::{CloseReason, SessionState};
use kanso_types::{GridSize, ProfileId, SessionId, TransferId};
use tokio::sync::mpsc::UnboundedReceiver;

use super::{ProfileLaunch, RealCore, TransferEntry, open_error};
use crate::api::{CoreError, OpenTarget, Result};
use crate::mcp::{ConnectMode, McpConnectOutcome, UserNeed};
use crate::ssh::NeedSlot;

/// 无界面会话没有界面，给个固定的网格即可（纯 SFTP 机制，不会真的用到）。
const HEADLESS_SIZE: GridSize = GridSize { cols: 80, rows: 24 };

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // 临界区只有 HashMap 的增删，不会留下半更新状态。
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 借用连接释放所需的状态，克隆给后台任务用。
///
/// 管理器只持弱引用：管理器持有的 host 里有发送端，任务又在等接收端，强引用会形成环，
/// 导致管理器永远无法释放。
pub(crate) struct ReleaseCtx {
    pub sessions: Weak<SessionManager>,
    pub profile_sessions: Arc<Mutex<HashMap<SessionId, (ProfileId, String)>>>,
    pub headless: Arc<Mutex<HashMap<SessionId, bool>>>,
    pub transfers: Arc<Mutex<HashMap<TransferId, TransferEntry>>>,
}

/// 启动后台任务：可见会话结束（远端 shell 退出、断线）时检查是否要释放借用连接。
///
/// 事件回调（`HostAdapter::emit`）在会话的生命周期锁内执行，不能直接拿管理器锁，
/// 所以只往通道里投一个会话 id，由这个任务在运行时上处理。
pub(crate) fn spawn_release_task(
    handle: &tokio::runtime::Handle,
    mut closed: UnboundedReceiver<SessionId>,
    ctx: ReleaseCtx,
) {
    handle.spawn(async move {
        while let Some(id) = closed.recv().await {
            let Some(mgr) = ctx.sessions.upgrade() else {
                return;
            };
            // 会话已被用户关闭（已从表里移除）的情况由 `close_session` 自己处理。
            let Some(session) = mgr.session(id) else {
                continue;
            };
            if session.is_headless() {
                continue;
            }
            if let Some(connection_id) = session.connection_id() {
                release_borrowed(
                    &mgr,
                    &ctx.headless,
                    &ctx.profile_sessions,
                    &ctx.transfers,
                    connection_id,
                );
            }
        }
    });
}

/// 这条连接上若已没有未结束的可见 SSH / SFTP 会话，就关闭它上面所有借用的无界面会话。
///
/// 可见会话"未结束"指状态不是 `Closed`（含还在连接中的）。自建的无界面会话不动。
fn release_borrowed(
    mgr: &SessionManager,
    headless: &Mutex<HashMap<SessionId, bool>>,
    profile_sessions: &Mutex<HashMap<SessionId, (ProfileId, String)>>,
    transfers: &Mutex<HashMap<TransferId, TransferEntry>>,
    connection_id: u64,
) {
    let all = mgr.all_sessions();
    let same_connection =
        |s: &Session| matches!(s, Session::Ssh(_)) && s.connection_id() == Some(connection_id);
    let visible_alive = all.iter().any(|s| {
        same_connection(s) && !s.is_headless() && !matches!(s.state(), SessionState::Closed(_))
    });
    if visible_alive {
        return;
    }
    for s in all.iter().filter(|s| same_connection(s) && s.is_headless()) {
        let id = s.id();
        if lock(headless).get(&id) != Some(&true) {
            continue;
        }
        mgr.close(id);
        // 与 `close_session` 一致：关闭会话取消它的传输。
        for t in lock(transfers).values() {
            if t.session == id {
                t.cancel.cancel();
            }
        }
        lock(profile_sessions).remove(&id);
        lock(headless).remove(&id);
    }
}

/// 打开中的会话的 Drop guard：future 被丢弃（调用方取消）或建连失败时关闭它，不留孤儿会话。
struct OpenGuard<'a> {
    core: &'a RealCore,
    id: SessionId,
    armed: bool,
}

impl OpenGuard<'_> {
    /// 连接成功，交给调用方管理。
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for OpenGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.core.close_session(self.id);
        }
    }
}

/// 等待连接建立的失败结果。
enum Failure {
    Need(UserNeed),
    Error(CoreError),
}

impl RealCore {
    /// 立即检查并释放某条连接上借用的无界面会话，规则见 [`release_borrowed`]。
    pub(crate) fn release_borrowed(&self, connection_id: u64) {
        release_borrowed(
            &self.sessions,
            &self.headless,
            &self.profile_sessions,
            &self.transfers,
            connection_id,
        );
    }

    /// 某 Profile 下仍存活的 SSH / 纯 SFTP 会话（含无界面）及其连接，附带是否无界面与是否自建。
    fn live_connections(&self, profile: ProfileId) -> Vec<(kanso_ssh::Connection, bool, bool)> {
        let table = lock(&self.headless).clone();
        self.sessions
            .all_sessions()
            .into_iter()
            .filter(|s| matches!(s, Session::Ssh(_)))
            .filter(|s| self.profile_of(s.id()).is_some_and(|(p, _)| p == profile))
            .filter_map(|s| {
                // 连接引用只在这里短暂克隆，用完即放。
                let conn = s.connection().filter(|c| !c.is_closed())?;
                let headless = s.is_headless();
                let self_built = headless && table.get(&s.id()) == Some(&false);
                Some((conn, headless, self_built))
            })
            .collect()
    }

    /// 可复用的连接：取最近建立（`Connection::id` 最大）且存活的那条。
    /// 返回值的 `bool` 为 `borrowed`：这条连接上没有自建的无界面会话，即它是用户建立的。
    fn find_reusable(&self, profile: ProfileId) -> Option<(kanso_ssh::Connection, bool)> {
        let live = self.live_connections(profile);
        let (conn, _, _) = live.iter().max_by_key(|(c, _, _)| c.id())?;
        let conn = conn.clone();
        let borrowed = !live
            .iter()
            .any(|(c, _, self_built)| *self_built && c.id() == conn.id());
        Some((conn, borrowed))
    }

    /// 这个 Profile 上 MCP 自建且存活的连接（最近建立的那条）。用户打开会话时用它做共享。
    pub(crate) fn find_self_built(&self, profile: ProfileId) -> Option<kanso_ssh::Connection> {
        self.live_connections(profile)
            .into_iter()
            .filter(|(_, _, self_built)| *self_built)
            .map(|(c, _, _)| c)
            .max_by_key(kanso_ssh::Connection::id)
    }

    /// 开一个无界面会话并登记 Profile 与借用标记。
    fn open_headless(
        &self,
        profile: ProfileId,
        name: String,
        launch: kanso_session::SshLaunch,
        connection: Option<kanso_ssh::Connection>,
        borrowed: bool,
    ) -> Result<SessionId> {
        let id = self
            .sessions
            .open(OpenRequest::Headless { launch, connection }, HEADLESS_SIZE)
            .map_err(open_error)?;
        lock(&self.profile_sessions).insert(id, (profile, name));
        lock(&self.headless).insert(id, borrowed);
        Ok(id)
    }

    /// 等会话连上或结束；失败时先看非交互槽，有原因就是 `Need`。
    async fn await_connected(
        &self,
        id: SessionId,
        slot: &NeedSlot,
    ) -> std::result::Result<(), Failure> {
        let Some(Session::Ssh(session)) = self.sessions.session(id) else {
            return Err(Failure::Error(CoreError::NoSuchSession(id)));
        };
        match session.wait_settled().await {
            SessionState::Connected => Ok(()),
            SessionState::Closed(reason) => {
                if let Some(need) = slot.get() {
                    return Err(Failure::Need(need));
                }
                Err(Failure::Error(match reason {
                    CloseReason::Error(message) => CoreError::User(message),
                    // 等待期间被别处关闭（用户或释放逻辑）：会话已经不存在。
                    _ => CoreError::NoSuchSession(id),
                }))
            }
            SessionState::Connecting => Err(Failure::Error(CoreError::Internal {
                detail: "session settled while still connecting".to_owned(),
            })),
        }
    }

    pub(crate) async fn mcp_connect_impl(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
    ) -> Result<McpConnectOutcome> {
        let slot = Arc::new(NeedSlot::default());
        let non_interactive =
            matches!(mode, ConnectMode::NonInteractive).then(|| Arc::clone(&slot));
        let (profile_rec, launch) = self.profile_launch(profile, non_interactive)?;
        let ProfileLaunch::Ssh(launch) = launch else {
            return Err(CoreError::NotSupported);
        };
        let name = profile_rec.name;

        if let Some((connection, borrowed)) = self.find_reusable(profile) {
            let session = self.open_headless(profile, name, launch, Some(connection), borrowed)?;
            return Ok(McpConnectOutcome::Connected { session, borrowed });
        }
        if mode == ConnectMode::ReuseOnly {
            return Ok(McpConnectOutcome::NotConnected);
        }
        let session = self.open_headless(profile, name, launch, None, false)?;
        // 在 await 之前建立：调用方丢弃 future 时 guard 关闭这个正在建立的会话。
        let guard = OpenGuard {
            core: self,
            id: session,
            armed: true,
        };
        match self.await_connected(session, &slot).await {
            Ok(()) => {
                guard.disarm();
                Ok(McpConnectOutcome::Connected {
                    session,
                    borrowed: false,
                })
            }
            // guard 在返回时关闭会话。
            Err(Failure::Need(need)) => Ok(McpConnectOutcome::NeedsUser(need)),
            Err(Failure::Error(e)) => Err(e),
        }
    }

    pub(crate) async fn mcp_open_session_impl(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
        size: GridSize,
    ) -> Result<McpConnectOutcome> {
        let slot = Arc::new(NeedSlot::default());
        let non_interactive =
            matches!(mode, ConnectMode::NonInteractive).then(|| Arc::clone(&slot));
        let (profile_rec, launch) = self.profile_launch(profile, non_interactive)?;
        let launch = match launch {
            // 本地 Profile 不涉及认证，`mode` 忽略，与普通方式打开完全相同。
            ProfileLaunch::Local(_) => {
                let session = self.open_session(OpenTarget::Profile(profile), size)?;
                return Ok(McpConnectOutcome::Connected {
                    session,
                    borrowed: false,
                });
            }
            ProfileLaunch::Ssh(launch) => launch,
        };
        let name = profile_rec.name;
        let register = |id: SessionId| {
            lock(&self.profile_sessions).insert(id, (profile, name.clone()));
        };

        if let Some((connection, borrowed)) = self.find_reusable(profile) {
            let session = self
                .sessions
                .open(OpenRequest::SshShared { launch, connection }, size)
                .map_err(open_error)?;
            register(session);
            return Ok(McpConnectOutcome::Connected { session, borrowed });
        }
        if mode == ConnectMode::ReuseOnly {
            return Ok(McpConnectOutcome::NotConnected);
        }
        let session = self
            .sessions
            .open(OpenRequest::Ssh(launch), size)
            .map_err(open_error)?;
        register(session);
        let guard = OpenGuard {
            core: self,
            id: session,
            armed: true,
        };
        match self.await_connected(session, &slot).await {
            Ok(()) => {
                guard.disarm();
                Ok(McpConnectOutcome::Connected {
                    session,
                    borrowed: false,
                })
            }
            Err(Failure::Need(need)) => Ok(McpConnectOutcome::NeedsUser(need)),
            Err(Failure::Error(e)) => Err(e),
        }
    }
}
