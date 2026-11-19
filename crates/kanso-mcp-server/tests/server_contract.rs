//! `McpServer` 公开接口的契约测试：mock 服务的回调顺序，以及不支持的渠道 / 平台的表现。
//! 真实服务的测试在 `real_server.rs`。

use std::sync::{Arc, Mutex};

use kanso_core::{CollectingEventSink, Core, Platform};
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpServer, McpServerConfig, McpServerDelegate, McpServerError,
    McpServerStatus,
};
use kanso_types::SessionId;
use kanso_types::secret::SecretString;

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Status(McpServerStatus),
    Clients(Vec<String>),
    Opened(SessionId, String),
    Activity(SessionId, Option<String>),
    Released(SessionId),
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Call>>);

impl Recorder {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl McpServerDelegate for Recorder {
    fn on_status_changed(&self, status: McpServerStatus) {
        self.0.lock().unwrap().push(Call::Status(status));
    }
    fn on_clients_changed(&self, clients: Vec<McpClientInfo>) {
        self.0
            .lock()
            .unwrap()
            .push(Call::Clients(clients.into_iter().map(|c| c.name).collect()));
    }
    fn on_session_opened(&self, session: SessionId, client: McpClientInfo) {
        self.0
            .lock()
            .unwrap()
            .push(Call::Opened(session, client.name));
    }
    fn on_activity(&self, session: SessionId, activity: Option<McpActivity>) {
        self.0
            .lock()
            .unwrap()
            .push(Call::Activity(session, activity.map(|a| a.tool)));
    }
    fn on_session_released(&self, session: SessionId) {
        self.0.lock().unwrap().push(Call::Released(session));
    }
    fn on_audit_changed(&self) {}
}

fn config() -> McpServerConfig {
    McpServerConfig {
        bundle_id: "test.kanso".into(),
        token: SecretString::new("t0ken"),
    }
}

fn mock() -> (Arc<Core>, Arc<Recorder>, McpServer) {
    let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
        CollectingEventSink::new(),
    ))));
    let rec = Arc::new(Recorder::default());
    let server = McpServer::new_mock(core.clone(), rec.clone());
    (core, rec, server)
}

#[cfg(all(feature = "direct", any(unix, windows)))]
const MOCK_CLIENT: &str = "Claude Code (mock)";

#[cfg(all(feature = "direct", any(unix, windows)))]
#[test]
fn mock_start_stop_and_token_rotation() {
    let (_core, rec, server) = mock();
    assert_eq!(server.status(), McpServerStatus::Stopped);
    // 没运行时 debug 操作报 NotRunning。
    assert_eq!(
        server.debug_activity(SessionId::from_raw(1), None),
        Err(McpServerError::NotRunning)
    );

    server.start(config()).unwrap();
    assert!(matches!(server.status(), McpServerStatus::Running { .. }));
    let calls = rec.take();
    assert!(matches!(
        calls[0],
        Call::Status(McpServerStatus::Running { .. })
    ));
    assert_eq!(calls[1], Call::Clients(vec![MOCK_CLIENT.into()]));
    assert_eq!(server.clients().len(), 1);

    // 重新生成令牌：用旧令牌的连接全部断开，客户端换上新令牌后重新连上（新的客户端编号），服务仍在运行。
    let old_id = server.clients()[0].id;
    server.set_token(SecretString::new("new"));
    assert_eq!(rec.take(), vec![Call::Clients(vec![MOCK_CLIENT.into()])]);
    assert_ne!(server.clients()[0].id, old_id);
    assert!(matches!(server.status(), McpServerStatus::Running { .. }));

    server.stop();
    assert_eq!(
        rec.take(),
        vec![
            Call::Status(McpServerStatus::Stopped),
            Call::Clients(vec![])
        ]
    );
    // 重复 stop 不再回调。
    server.stop();
    assert!(rec.take().is_empty());
}

#[cfg(all(feature = "direct", any(unix, windows)))]
#[test]
fn mock_open_tab_activity_and_release_on_disconnect() {
    let (core, rec, server) = mock();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    server.start(config()).unwrap();
    rec.take();
    let web = core
        .profiles()
        .into_iter()
        .find(|p| p.name == "web-01")
        .unwrap()
        .id;

    let tab = rt.block_on(server.debug_open_tab(web)).unwrap();
    // MCP 打开的是普通标签，会出现在 sessions() 里。
    assert!(core.sessions().iter().any(|s| s.id == tab));
    // 先计调用次数，再回调打开标签。
    assert_eq!(
        rec.take(),
        vec![
            Call::Clients(vec![MOCK_CLIENT.into()]),
            Call::Opened(tab, MOCK_CLIENT.into())
        ]
    );
    assert_eq!(server.clients()[0].calls, 1);

    server
        .debug_activity(tab, Some("kanso_terminal_run".into()))
        .unwrap();
    server.debug_activity(tab, None).unwrap();
    // 活动标识只是某次调用的显示状态，不计调用次数。
    assert_eq!(server.clients()[0].calls, 1);
    assert_eq!(
        rec.take(),
        vec![
            Call::Activity(tab, Some("kanso_terminal_run".into())),
            Call::Activity(tab, None)
        ]
    );

    // 客户端断开：它打开的标签变成普通标签（去掉标记），标签本身不关闭。
    let id = server.clients()[0].id;
    server.disconnect(id);
    assert_eq!(rec.take(), vec![Call::Released(tab), Call::Clients(vec![])]);
    assert!(core.sessions().iter().any(|s| s.id == tab));
}

#[cfg(all(feature = "direct", any(unix, windows)))]
#[test]
fn refused_open_still_counts_as_a_call() {
    let (core, rec, server) = mock();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    server.start(config()).unwrap();
    rec.take();
    let mfa = core
        .profiles()
        .into_iter()
        .find(|p| p.name == "mfa-bastion")
        .unwrap()
        .id;
    // MFA 主机没有已认证连接：拒绝新建，但这次调用仍然计数（McpClientInfo::calls 含被拒绝的调用）。
    assert!(matches!(
        rt.block_on(server.debug_open_tab(mfa)),
        Err(McpServerError::Failed { .. })
    ));
    assert_eq!(server.clients()[0].calls, 1);
    assert!(!rec.take().iter().any(|c| matches!(c, Call::Opened(..))));
}

/// 非 Direct 渠道：两种构造都"不支持"，状态一开始就是 Unavailable，宿主不必调用 start 就能决定隐藏页面。
/// 运行：`cargo test -p kanso-mcp-server --no-default-features --features mas`。
#[cfg(not(all(feature = "direct", any(unix, windows))))]
#[test]
fn non_direct_channels_are_unsupported_from_the_start() {
    assert!(!kanso_mcp_server::supported());
    let unavailable = McpServerStatus::Unavailable(kanso_mcp_server::McpUnavailable::NotSupported);
    let (_core, rec, server) = mock();
    assert_eq!(server.status(), unavailable);
    assert_eq!(server.start(config()), Err(McpServerError::NotSupported));
    assert_eq!(
        server.debug_activity(SessionId::from_raw(1), None),
        Err(McpServerError::NotSupported)
    );
    assert!(server.clients().is_empty());
    // start 失败与其他渠道一样回调一次状态；stop 在不支持的渠道里什么也不做，状态仍是不支持。
    assert_eq!(rec.take(), vec![Call::Status(unavailable.clone())]);
    server.stop();
    assert_eq!(server.status(), unavailable);
    assert!(rec.take().is_empty());
}
