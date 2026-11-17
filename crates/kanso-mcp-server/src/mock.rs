//! mock 服务：不监听 socket、不跑 MCP 协议，只模拟设置页和标签栏能看到的状态变化，
//! 供 Swift 界面在真实服务（F9）完成前开发与截图测试。
//!
//! - `start` 后状态为 `Running`，并模拟一个名为 "Claude Code (mock)" 的已连接客户端；
//! - `set_token` 模拟"用旧令牌的连接全部断开，客户端换上新令牌后重新连上"（得到新的客户端编号）；
//! - `debug_open_tab` / `debug_activity` 让界面能触发对应回调。
//!
//! 所有回调都在**持有状态锁时**发出：这样回调的先后顺序与状态变化的先后顺序严格一致
//! （否则两个线程各自放锁后再回调，宿主可能先收到"标签已释放"再收到"标签已打开"，或者旧的客户端列表
//! 盖掉新的）。这样做安全的前提是回调契约：实现必须立即返回，且不得在回调里同步调用 `McpServer`。
//! 内核的 mock 出于同样的理由也是持锁发事件。

use std::sync::{Arc, Mutex, MutexGuard};

use kanso_core::Core;
use kanso_core::mcp::{ConnectMode, McpConnectOutcome};
use kanso_types::secret::SecretString;
use kanso_types::{GridSize, ProfileId, SessionId};

use crate::{
    McpActivity, McpClientInfo, McpServerConfig, McpServerDelegate, McpServerError,
    McpServerStatus, Result, now_ms,
};

/// mock 客户端的名称。
const MOCK_CLIENT: &str = "Claude Code (mock)";
/// `debug_open_tab` 打开标签用的初始尺寸；宿主摆放后会按实际大小 resize。
const MOCK_GRID: GridSize = GridSize { cols: 80, rows: 24 };

#[derive(Default)]
struct State {
    socket_path: Option<String>,
    clients: Vec<McpClientInfo>,
    next_client: u64,
    /// 各客户端打开的标签，客户端断开时逐个回调 `on_session_released`。
    opened: Vec<(u64, SessionId)>,
}

impl State {
    /// 模拟一个客户端连上，返回它的信息。
    fn connect_client(&mut self) -> McpClientInfo {
        let client = McpClientInfo {
            id: self.next_client,
            name: MOCK_CLIENT.to_owned(),
            connected_at_ms: now_ms(),
            calls: 0,
            own_session: None,
        };
        self.next_client += 1;
        self.clients.push(client.clone());
        client
    }

    /// 断开满足条件的客户端，返回它们打开过、需要回调 `on_session_released` 的标签。
    /// 调用方要在**仍持有**状态锁时回调（经 `notify_clients`），见模块文档。
    fn drop_clients(&mut self, which: impl Fn(&McpClientInfo) -> bool) -> Vec<SessionId> {
        let ids: Vec<u64> = self
            .clients
            .iter()
            .filter(|c| which(c))
            .map(|c| c.id)
            .collect();
        self.clients.retain(|c| !ids.contains(&c.id));
        let released = self
            .opened
            .iter()
            .filter(|(c, _)| ids.contains(c))
            .map(|(_, s)| *s)
            .collect();
        self.opened.retain(|(c, _)| !ids.contains(c));
        released
    }

    fn client(&self, id: u64) -> Option<&McpClientInfo> {
        self.clients.iter().find(|c| c.id == id)
    }
}

pub(crate) struct MockServer {
    core: Arc<Core>,
    delegate: Arc<dyn McpServerDelegate>,
    state: Mutex<State>,
}

impl MockServer {
    pub(crate) fn new(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> MockServer {
        MockServer {
            core,
            delegate,
            state: Mutex::new(State {
                next_client: 1,
                ..State::default()
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // mock 状态没有跨字段的不变式需要在 panic 后保护，中毒后继续使用。
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 回调已释放的标签与新的客户端列表。调用方必须**仍持有**状态锁（见模块文档）。
    fn notify_clients(&self, st: &State, released: Vec<SessionId>) {
        for s in released {
            self.delegate.on_session_released(s);
        }
        self.delegate.on_clients_changed(st.clients.clone());
    }

    pub(crate) fn start(&self, config: McpServerConfig) -> Result<()> {
        let mut st = self.lock();
        // 重启：旧连接全部断开，再模拟一个客户端连上。
        let released = st.drop_clients(|_| true);
        let socket_path = format!("/tmp/{}.mcp.sock (mock)", config.bundle_id);
        st.socket_path = Some(socket_path.clone());
        st.connect_client();
        self.delegate
            .on_status_changed(McpServerStatus::Running { socket_path });
        self.notify_clients(&st, released);
        Ok(())
    }

    pub(crate) fn stop(&self) {
        let mut st = self.lock();
        if st.socket_path.take().is_none() {
            return;
        }
        let released = st.drop_clients(|_| true);
        self.delegate.on_status_changed(McpServerStatus::Stopped);
        self.notify_clients(&st, released);
    }

    pub(crate) fn set_token(&self, token: SecretString) {
        let _ = token;
        let mut st = self.lock();
        if st.socket_path.is_none() {
            return;
        }
        // 用旧令牌建立的连接全部断开；mock 里所有连接都算旧令牌。随后模拟客户端换上新令牌重新连上，
        // 否则令牌轮换之后 `debug_*` 再也找不到客户端，界面没法继续走后面的流程。
        let released = st.drop_clients(|_| true);
        st.connect_client();
        self.notify_clients(&st, released);
    }

    pub(crate) fn status(&self) -> McpServerStatus {
        match &self.lock().socket_path {
            Some(path) => McpServerStatus::Running {
                socket_path: path.clone(),
            },
            None => McpServerStatus::Stopped,
        }
    }

    pub(crate) fn clients(&self) -> Vec<McpClientInfo> {
        self.lock().clients.clone()
    }

    pub(crate) fn disconnect(&self, client_id: u64) {
        let mut st = self.lock();
        if st.client(client_id).is_none() {
            return;
        }
        let released = st.drop_clients(|c| c.id == client_id);
        self.notify_clients(&st, released);
    }

    /// 以第一个已连接的客户端名义记一次调用（含随后被拒绝的调用，与 `McpClientInfo::calls` 的定义一致），
    /// 返回计数后的客户端信息。没有运行或没有客户端时报错。
    fn begin_call(&self) -> Result<McpClientInfo> {
        let mut st = self.lock();
        if st.socket_path.is_none() {
            return Err(McpServerError::NotRunning);
        }
        let client = st.clients.first_mut().ok_or(McpServerError::Failed {
            detail: "no mock client connected".into(),
        })?;
        client.calls += 1;
        let client = client.clone();
        self.delegate.on_clients_changed(st.clients.clone());
        Ok(client)
    }

    pub(crate) async fn debug_open_tab(&self, profile: ProfileId) -> Result<SessionId> {
        let client = self.begin_call()?;
        let outcome = self
            .core
            .mcp_open_session(profile, ConnectMode::NonInteractive, MOCK_GRID)
            .await
            .map_err(|e| McpServerError::Failed {
                detail: e.to_string(),
            })?;
        let McpConnectOutcome::Connected { session, .. } = outcome else {
            return Err(McpServerError::Failed {
                detail: format!("open tab refused: {outcome:?}"),
            });
        };
        // 打开标签要 await，期间宿主可能已经 stop / disconnect / set_token。客户端已经不在了，
        // 就不能再把标签记到它名下：它的 on_session_released 永远不会再发，"由 MCP 打开"标记会一直留着。
        // 这时关掉刚打开的标签，当作这次调用失败（真实服务对断开的客户端同样不再回复）。
        // 记录与回调在同一把锁内：并发的 disconnect 要么在此之前（客户端已不在，走上面的失败分支），
        // 要么在此之后（它看到 opened 里的这个标签，`on_session_released` 必然排在 `on_session_opened` 之后）。
        let mut st = self.lock();
        let Some(current) = st.client(client.id).cloned() else {
            drop(st);
            // 关标签放在锁外：core 的 close_session 会发核心事件，不需要和本服务的状态一起保序。
            self.core.close_session(session);
            return Err(McpServerError::NotRunning);
        };
        st.opened.push((client.id, session));
        self.delegate.on_session_opened(session, current);
        Ok(session)
    }

    /// 第一个已连接的客户端，不计调用次数。没有运行或没有客户端时报错。
    fn first_client(st: &State) -> Result<McpClientInfo> {
        if st.socket_path.is_none() {
            return Err(McpServerError::NotRunning);
        }
        st.clients.first().cloned().ok_or(McpServerError::Failed {
            detail: "no mock client connected".into(),
        })
    }

    /// 模拟一次操作的开始或结束；它只是某次工具调用的显示状态，不是单独的调用，所以不计次数。
    pub(crate) fn debug_activity(&self, session: SessionId, tool: Option<String>) -> Result<()> {
        let st = self.lock();
        let client = Self::first_client(&st)?;
        let activity = tool.map(|tool| McpActivity {
            client_id: client.id,
            client_name: client.name,
            tool,
        });
        self.delegate.on_activity(session, activity);
        Ok(())
    }
}
