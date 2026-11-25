//! Windows / Linux MCP 宿主：令牌保存在系统凭据库中，服务回调经通道投递到 GPUI 线程。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{Context, EventEmitter, Task};
use kanso_core::Core;
use kanso_gpui::app::launcher::render_message;
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpClientKind, McpServer, McpServerConfig, McpServerDelegate,
    McpServerStatus, McpUnavailable, client_config, powershell_client_config,
};
use kanso_secret::PlatformSecretStore;
use kanso_types::event::SessionState;
use kanso_types::message::Message;
use kanso_types::secret::SecretString;
use kanso_types::{ProfileId, SessionId};

enum Event {
    Status,
    Clients(Vec<McpClientInfo>),
    Opened(SessionId, McpClientInfo),
    Activity(SessionId, Option<McpActivity>),
    Released(SessionId),
    AuditChanged,
}

struct Delegate(UnboundedSender<Event>);

impl McpServerDelegate for Delegate {
    fn on_status_changed(&self, _: McpServerStatus) {
        let _ = self.0.unbounded_send(Event::Status);
    }

    fn on_clients_changed(&self, clients: Vec<McpClientInfo>) {
        let _ = self.0.unbounded_send(Event::Clients(clients));
    }

    fn on_session_opened(&self, session: SessionId, client: McpClientInfo) {
        let _ = self.0.unbounded_send(Event::Opened(session, client));
    }

    fn on_activity(&self, session: SessionId, activity: Option<McpActivity>) {
        let _ = self.0.unbounded_send(Event::Activity(session, activity));
    }

    fn on_session_released(&self, session: SessionId) {
        let _ = self.0.unbounded_send(Event::Released(session));
    }

    fn on_audit_changed(&self) {
        let _ = self.0.unbounded_send(Event::AuditChanged);
    }
}

pub(super) struct TabOpened(pub(super) SessionId);
pub(super) struct AuditChanged;

#[derive(Default)]
struct TabMark {
    opened_by: Option<String>,
    activity: Option<McpActivity>,
    clear: Option<Task<()>>,
}

pub(super) struct McpModel {
    core: Arc<Core>,
    server: McpServer,
    tokens: PlatformSecretStore,
    token: Option<SecretString>,
    status: McpServerStatus,
    clients: Vec<McpClientInfo>,
    helper: PathBuf,
    marks: HashMap<SessionId, TabMark>,
    accessible_profiles: HashSet<ProfileId>,
}

impl EventEmitter<TabOpened> for McpModel {}
impl EventEmitter<AuditChanged> for McpModel {}

impl McpModel {
    pub(super) fn new(core: Arc<Core>, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded();
        let (server, tokens) = open_server(&core, Arc::new(Delegate(tx)));
        let mut policy = core.mcp_policy_watch();
        let mut model = Self {
            tokens,
            status: server.status(),
            accessible_profiles: accessible_profiles(&core),
            core,
            server,
            token: None,
            clients: Vec::new(),
            helper: std::env::current_exe()
                .map(|exe| {
                    exe.with_file_name(if cfg!(windows) {
                        "kanso-mcp.exe"
                    } else {
                        "kanso-mcp"
                    })
                })
                .unwrap_or_default(),
            marks: HashMap::new(),
        };
        if model.enabled()
            && let Err(error) = model.start(false)
        {
            // 系统凭据失败时服务还未开始，无服务回调，仍要给设置页一个状态。
            if matches!(model.status, McpServerStatus::Stopped) {
                model.status =
                    McpServerStatus::Unavailable(McpUnavailable::Failed { detail: error });
            }
        }
        cx.spawn(async move |entity, cx| {
            while let Some(first) = rx.next().await {
                let mut events = vec![first];
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                    if events.len() == 64 {
                        break;
                    }
                }
                if entity
                    .update(cx, |m, cx| {
                        for event in events {
                            m.apply(event, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |entity, cx| {
            while policy.changed().await.is_ok() {
                if entity
                    .update(cx, |model, cx| {
                        model.accessible_profiles = accessible_profiles(&model.core);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.on_app_quit(|model, _| {
            model.server.stop();
            std::future::ready(())
        })
        .detach();
        model
    }

    pub(super) fn core(&self) -> &Core {
        &self.core
    }

    pub(super) fn enabled(&self) -> bool {
        self.core.mcp_enabled()
    }

    pub(super) fn set_enabled(&mut self, on: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let result = if on {
            self.start(true)
        } else {
            self.check_instance()?;
            self.core.set_mcp_enabled(false).map_err(|e| {
                super::log(&format!("无法关闭 MCP：{e}"));
                e.user_message().map_or_else(
                    || render_message(&Message::WinMcpStopFailed {}),
                    render_message,
                )
            })?;
            self.server.stop();
            self.status = self.server.status();
            Ok(())
        };
        cx.notify();
        result
    }

    pub(super) fn status(&self) -> &McpServerStatus {
        &self.status
    }

    pub(super) fn can_address_tab(&self, profile: ProfileId) -> bool {
        matches!(self.status, McpServerStatus::Running { .. })
            && self.accessible_profiles.contains(&profile)
    }

    pub(super) fn clients(&self) -> &[McpClientInfo] {
        &self.clients
    }

    pub(super) fn disconnect(&self, id: u64) {
        self.server.disconnect(id);
    }

    pub(super) fn helper_path(&self) -> &Path {
        &self.helper
    }

    pub(super) fn command(&self, kind: McpClientKind) -> Option<String> {
        let path = self.helper.to_str().filter(|path| !path.is_empty())?;
        let token = self.token.as_ref()?.expose();
        Some(if cfg!(windows) {
            powershell_client_config(kind, path, token)
        } else {
            client_config(kind, path, token)
        })
    }

    pub(super) fn refresh_token(&mut self, cx: &mut Context<Self>) {
        if self.token.is_none() {
            match self.tokens.mcp_token() {
                Ok(Some(token)) => {
                    self.token = Some(token);
                    cx.notify();
                }
                Ok(None) => {}
                Err(error) => super::log(&format!("MCP 凭据读取失败：{error}")),
            }
        }
    }

    pub(super) fn regenerate_token(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        self.check_instance()?;
        let token = self.create_token()?;
        self.server.set_token(token.clone());
        self.token = Some(token);
        cx.notify();
        Ok(())
    }

    pub(super) fn forget(&mut self, session: SessionId) {
        self.marks.remove(&session);
    }

    pub(super) fn tab_mark(&self, session: SessionId) -> Option<(bool, Vec<String>)> {
        let mark = self.marks.get(&session)?;
        let mut lines = Vec::new();
        if let Some(client) = &mark.opened_by {
            lines.push(render_message(&Message::WinMcpTabOpened {
                client: client_name(client),
            }));
        }
        if let Some(activity) = &mark.activity {
            lines.push(render_message(&Message::WinMcpTabActivity {
                client: client_name(&activity.client_name),
                tool: activity.tool.clone(),
            }));
        }
        (!lines.is_empty()).then_some((mark.activity.is_some(), lines))
    }

    fn ensure_token(&mut self) -> Result<SecretString, String> {
        if let Some(token) = &self.token {
            return Ok(token.clone());
        }
        let token = match self.tokens.mcp_token().map_err(token_error)? {
            Some(token) => token,
            None => self.create_token()?,
        };
        self.token = Some(token.clone());
        Ok(token)
    }

    fn create_token(&self) -> Result<SecretString, String> {
        let token = generate_token().map_err(token_error)?;
        self.tokens.set_mcp_token(&token).map_err(token_error)?;
        Ok(token)
    }

    fn start(&mut self, persist: bool) -> Result<(), String> {
        self.check_instance()?;
        let token = self.ensure_token()?;
        if persist {
            self.core.set_mcp_enabled(true).map_err(|e| {
                super::log(&format!("无法启用 MCP：{e}"));
                e.user_message().map_or_else(
                    || render_message(&Message::WinMcpStartFailed {}),
                    render_message,
                )
            })?;
        }
        let result = self.server.start(McpServerConfig {
            bundle_id: self.core.bundle_id().to_owned(),
            token,
        });
        self.status = self.server.status();
        result.map_err(|error| {
            super::log(&format!("MCP 服务启动失败：{error}"));
            render_message(&Message::WinMcpStartFailed {})
        })
    }

    fn check_instance(&self) -> Result<(), String> {
        match self.server.status() {
            McpServerStatus::Unavailable(McpUnavailable::AnotherInstance) => {
                Err(render_message(&Message::WinMcpAnotherInstance {}))
            }
            McpServerStatus::Unavailable(McpUnavailable::NotSupported) => {
                Err(render_message(&Message::WinMcpNotSupported {}))
            }
            _ => Ok(()),
        }
    }

    fn apply(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::AuditChanged => cx.emit(AuditChanged),
            Event::Status => {
                self.status = self.server.status();
                if !matches!(self.status, McpServerStatus::Running { .. }) {
                    self.marks.clear();
                }
            }
            Event::Clients(clients) => {
                self.marks.retain(|_, mark| {
                    if mark
                        .activity
                        .as_ref()
                        .is_some_and(|a| !clients.iter().any(|c| c.id == a.client_id))
                    {
                        mark.activity = None;
                        mark.clear = None;
                    }
                    mark.opened_by.is_some() || mark.activity.is_some()
                });
                self.clients = clients;
            }
            Event::Opened(session, client) if self.live_session(session) => {
                self.marks.entry(session).or_default().opened_by = Some(client.name);
                cx.emit(TabOpened(session));
            }
            Event::Activity(session, Some(activity)) if self.live_session(session) => {
                let mark = self.marks.entry(session).or_default();
                mark.clear = None;
                mark.activity = Some(activity);
            }
            Event::Activity(session, None) => {
                if let Some(mark) = self.marks.get_mut(&session)
                    && mark.activity.is_some()
                {
                    mark.clear = Some(cx.spawn(async move |entity, cx| {
                        cx.background_executor().timer(Duration::from_secs(2)).await;
                        let _ = entity.update(cx, |m, cx| {
                            if let Some(mark) = m.marks.get_mut(&session) {
                                mark.activity = None;
                                if mark.opened_by.is_none() {
                                    m.marks.remove(&session);
                                }
                                cx.notify();
                            }
                        });
                    }));
                }
            }
            Event::Released(session) => {
                if let Some(mark) = self.marks.get_mut(&session) {
                    mark.opened_by = None;
                    if mark.activity.is_none() {
                        self.marks.remove(&session);
                    }
                }
            }
            Event::Opened(..) | Event::Activity(..) => {}
        }
    }

    fn live_session(&self, session: SessionId) -> bool {
        self.core
            .session_info(session)
            .is_some_and(|i| !matches!(i.state, SessionState::Closed(_)))
    }
}

fn accessible_profiles(core: &Core) -> HashSet<ProfileId> {
    match core.mcp_accesses() {
        Ok(accesses) => accesses.into_iter().map(|(profile, _)| profile).collect(),
        Err(error) => {
            super::log(&format!("读取标签的 MCP 授权失败：{error}"));
            HashSet::new()
        }
    }
}

fn open_server(
    core: &Arc<Core>,
    delegate: Arc<dyn McpServerDelegate>,
) -> (McpServer, PlatformSecretStore) {
    #[cfg(all(debug_assertions, feature = "direct"))]
    if let Some(path) = std::env::var_os("KANSO_MCP_DIR") {
        // 界面实测与转发程序使用同一测试目录。测试令牌也使用独立的凭据名称。
        let dir = PathBuf::from(path);
        let tokens =
            PlatformSecretStore::new(format!("{}/mcp-test/{}", core.bundle_id(), dir.display()));
        return (McpServer::new_in_dir(core.clone(), delegate, dir), tokens);
    }
    (
        McpServer::new(core.clone(), delegate),
        PlatformSecretStore::new(core.bundle_id()),
    )
}

fn client_name(name: &str) -> String {
    if name.trim().is_empty() {
        render_message(&Message::WinMcpUnknownClient {})
    } else {
        name.to_owned()
    }
}

fn token_error(error: impl std::fmt::Display) -> String {
    super::log(&format!("MCP 令牌存取失败：{error}"));
    render_message(&Message::WinMcpTokenStoreFailed {})
}

fn generate_token() -> Result<SecretString, getrandom::Error> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut token = String::with_capacity(22);
    let mut bytes = [0; 32];
    while token.len() < 22 {
        getrandom::fill(&mut bytes)?;
        // 248 是 62 的倍数，拒绝剩下的字节，避免取模偏差。
        let missing = 22 - token.len();
        token.extend(
            bytes
                .iter()
                .copied()
                .filter(|b| *b < 248)
                .map(|b| char::from(ALPHABET[usize::from(b) % ALPHABET.len()]))
                .take(missing),
        );
    }
    Ok(SecretString::new(token))
}
