//! SSH 会话在 core 一侧的接线：提问表、`Interaction` 实现与连接器（ssh.md §3、§4、§6）。
//!
//! 会话层（`kanso-session`）只负责"连上之后的一切"，不认识 Profile、钥匙串与提示通道；
//! 这些都在这里：
//!
//! - [`PromptTable`]：`PromptId → oneshot` 的登记表。提问 future 被丢弃的任何路径
//!   （180 秒超时、会话被关闭时任务被 abort、用户取消）都靠 Drop guard 清表并发 `PromptDismissed`，
//!   所以不会泄漏，也不会漏发。
//! - [`CoreSshConnector`]：实现会话层的 `SshConnector`——读凭据、调用 `kanso_ssh::connect`、
//!   保存用户选择"记住"的凭据，并处理主机密钥变更的确认。带跳板机的 Profile 由它按链逐跳读凭据、
//!   调用 `kanso_ssh::connect_chain`（ssh.md §5）；第一跳可以用 ProxyCommand 作为传输。

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use kanso_secret::{SecretKind, SecretRef, SecretStore};
use kanso_session::{BoxFuture, SshConnector};
use kanso_ssh::interaction::{credential_from_response, credential_prompt_request};
use kanso_ssh::{
    AuthMethod, ConnectParams, Connection, HopParams, HostKeyStore, Interaction, KbdRound,
    PROMPT_TIMEOUT, RememberedCredential, RevokedKeys, SshError, accept_changed_key, connect_chain,
};
use kanso_ssh::{HostKeyStoreError, KnownHostKey};
use kanso_types::event::NoticeLevel;
use kanso_types::message::Endpoint;
use kanso_types::profile::KeepaliveConfig;
use kanso_types::prompt::{CredentialKind, PromptRequest, PromptResponse};
use kanso_types::secret::SecretString;
use kanso_types::{CoreEvent, Message, ProfileId, PromptId, SessionId};

use crate::mcp::UserNeed;
use crate::platform::EventSink;

/// 提问期望的应答类型；类型不匹配的应答按 `Cancel` 处理（`respond_prompt` 契约）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expects {
    Kbd,
    Credential,
    HostKey,
    SavePath,
    Files,
}

impl Expects {
    fn accepts(self, r: &PromptResponse) -> bool {
        matches!(
            (self, r),
            (Expects::Kbd, PromptResponse::Answers(_))
                | (Expects::Credential, PromptResponse::Credential { .. })
                | (Expects::HostKey, PromptResponse::AcceptHostKey)
                | (Expects::SavePath, PromptResponse::SavePath(_))
                | (Expects::Files, PromptResponse::Files(_))
        )
    }
}

struct Pending {
    session: Option<SessionId>,
    expects: Expects,
    tx: tokio::sync::oneshot::Sender<PromptResponse>,
}

/// 未决提问表。
pub(crate) struct PromptTable {
    events: Arc<dyn EventSink>,
    next: AtomicU64,
    pending: Mutex<HashMap<PromptId, Pending>>,
}

/// future 被丢弃（超时、abort、会话关闭）时清理提问：表里还有这一项就移除并通知 UI 关闭界面。
///
/// 已经被应答或被 `cancel_*` 移除的项不会重复通知。
struct DismissGuard {
    table: Arc<PromptTable>,
    id: PromptId,
}

impl Drop for DismissGuard {
    fn drop(&mut self) {
        if self.table.lock().remove(&self.id).is_some() {
            self.table
                .events
                .on_event(CoreEvent::PromptDismissed { prompt: self.id });
        }
    }
}

impl PromptTable {
    pub(crate) fn new(events: Arc<dyn EventSink>) -> Arc<PromptTable> {
        Arc::new(PromptTable {
            events,
            next: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<PromptId, Pending>> {
        // 临界区只有 HashMap 的增删，不会留下半更新状态。
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 发出提问并等待应答。`None` 表示被取消（会话关闭、core 关闭）；
    /// future 被丢弃时由 guard 清理。
    pub(crate) fn ask(
        self: &Arc<Self>,
        session: Option<SessionId>,
        request: PromptRequest,
        expects: Expects,
    ) -> impl Future<Output = Option<PromptResponse>> + Send + 'static {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = PromptId::from_raw(self.next.fetch_add(1, Ordering::Relaxed));
        // 先登记再发事件：UI 可能在事件回调返回之前就调用 `respond_prompt`。
        self.lock().insert(
            id,
            Pending {
                session,
                expects,
                tx,
            },
        );
        let guard = DismissGuard {
            table: Arc::clone(self),
            id,
        };
        self.events.on_event(CoreEvent::Prompt {
            prompt: id,
            session,
            request,
        });
        async move {
            let _guard = guard;
            rx.await.ok()
        }
    }

    /// UI 的应答。未知或已过期的 id 静默忽略；类型与请求不匹配按 `Cancel`。
    pub(crate) fn respond(&self, id: PromptId, response: PromptResponse) {
        let Some(pending) = self.lock().remove(&id) else {
            return;
        };
        let response = if pending.expects.accepts(&response) {
            response
        } else {
            PromptResponse::Cancel
        };
        // 接收端已被丢弃说明提问刚好超时，忽略。
        let _ = pending.tx.send(response);
    }

    /// 取消挂在某会话上的全部提问并通知 UI；`None` 表示全部（core 关闭）。
    ///
    /// 先从表里移除再发事件：被唤醒的提问 future 里的 guard 因此不会重复通知。
    pub(crate) fn cancel(&self, session: Option<SessionId>) {
        let removed: Vec<PromptId> = {
            let mut map = self.lock();
            let ids: Vec<PromptId> = map
                .iter()
                .filter(|(_, p)| session.is_none() || p.session == session)
                .map(|(id, _)| *id)
                .collect();
            // 丢弃 Pending 即丢弃 oneshot 发送端，提问方收到 `None` 并按取消处理。
            for id in &ids {
                map.remove(id);
            }
            ids
        };
        for prompt in removed {
            self.events.on_event(CoreEvent::PromptDismissed { prompt });
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

/// `kanso_ssh::Interaction` 的 core 实现：把提问转成 `CoreEvent::Prompt` 并等 UI 应答。
///
/// 180 秒超时由 kanso-ssh 用 `timeout` 包住调用实现，超时丢弃 future，guard 发 `PromptDismissed`。
pub(crate) struct CoreInteraction {
    pub prompts: Arc<PromptTable>,
    pub session: SessionId,
}

impl Interaction for CoreInteraction {
    fn keyboard_interactive(
        &self,
        round: KbdRound,
    ) -> impl Future<Output = Option<Vec<String>>> + Send {
        let prompts = Arc::clone(&self.prompts);
        let session = self.session;
        async move {
            let r = prompts
                .ask(Some(session), round.into_prompt_request(), Expects::Kbd)
                .await?;
            kanso_ssh::interaction::kbd_answers_from_response(r)
        }
    }

    fn credential(
        &self,
        endpoint: &Endpoint,
        user: &str,
        kind: CredentialKind,
    ) -> impl Future<Output = Option<(SecretString, bool)>> + Send {
        let prompts = Arc::clone(&self.prompts);
        let session = self.session;
        let request = credential_prompt_request(endpoint, user, kind);
        async move {
            let r = prompts
                .ask(Some(session), request, Expects::Credential)
                .await?;
            credential_from_response(r)
        }
    }
}

/// 非交互连接（MCP）里"需要人参与"的原因槽，一次连接尝试一个。
///
/// 非交互的 `Interaction` 与主机密钥存储包装在发现需要人参与时写入它；连接失败后由调用方读取，
/// 有值就报 `NeedsUser(原因)`，没有才按普通连接失败处理。
/// 只保留**第一个**原因：后面的失败多半是它的连锁反应（例如私钥口令缺失后回退到密码）。
#[derive(Default)]
pub(crate) struct NeedSlot(Mutex<Option<UserNeed>>);

impl NeedSlot {
    pub(crate) fn record(&self, need: UserNeed) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(need);
    }

    pub(crate) fn get(&self) -> Option<UserNeed> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 非交互的 `Interaction`：任何需要用户输入的场合都**立即**返回 `None`（按取消处理）并记下原因。
///
/// 刻意不碰提问表：不登记提问、不发 `CoreEvent::Prompt`，MCP 不能让界面弹出任何对话框
/// （spec/mcp.md §2.4），也不能与用户同时打开同一会话时的提问混在一起。
pub(crate) struct NonInteractive {
    pub slot: Arc<NeedSlot>,
}

impl Interaction for NonInteractive {
    fn keyboard_interactive(
        &self,
        _round: KbdRound,
    ) -> impl Future<Output = Option<Vec<String>>> + Send {
        self.slot.record(UserNeed::KeyboardInteractive);
        std::future::ready(None)
    }

    fn credential(
        &self,
        _endpoint: &Endpoint,
        _user: &str,
        kind: CredentialKind,
    ) -> impl Future<Output = Option<(SecretString, bool)>> + Send {
        self.slot.record(match kind {
            CredentialKind::Password => UserNeed::Password,
            CredentialKind::KeyPassphrase { .. } => UserNeed::KeyPassphrase,
        });
        std::future::ready(None)
    }
}

/// 非交互连接用的主机密钥存储包装：只读，**不写入**。
///
/// kanso-ssh 对未知主机、以及已知主机的新算法密钥都会调用 `add`（ssh.md §3 的 accept-new）；
/// MCP 不能替用户信任新密钥（spec/mcp.md §2.4），所以 `add` 拒绝写入、返回错误让握手失败，
/// 并记下 `UnknownHostKey`。`lookup` 照常，已记录且匹配的主机密钥不受影响。
pub(crate) struct NonInteractiveHostKeys {
    pub inner: Arc<dyn HostKeyStore>,
    pub slot: Arc<NeedSlot>,
}

impl HostKeyStore for NonInteractiveHostKeys {
    fn lookup(&self, host: &str, port: u16) -> Result<Vec<KnownHostKey>, HostKeyStoreError> {
        self.inner.lookup(host, port)
    }

    fn add(
        &self,
        _host: &str,
        _port: u16,
        _key_type: &str,
        _key: &[u8],
    ) -> Result<(), HostKeyStoreError> {
        self.slot.record(UserNeed::UnknownHostKey);
        Err(HostKeyStoreError::Other {
            detail: "unknown host key is not recorded in non-interactive mode".to_owned(),
        })
    }

    fn replace_all(
        &self,
        _host: &str,
        _port: u16,
        _key_type: &str,
        _key: &[u8],
    ) -> Result<(), HostKeyStoreError> {
        // 非交互模式下 `confirm_host_key_change` 直接拒绝，正常不会走到这里；防御性地同样不写入。
        self.slot.record(UserNeed::ChangedHostKey);
        Err(HostKeyStoreError::Other {
            detail: "host key replacement is not allowed in non-interactive mode".to_owned(),
        })
    }
}

/// 从 core 的基础环境快照取出的 SSH 相关值（`new_with_base_env` 能覆盖，所以不直接读进程环境）。
#[derive(Clone, Debug, Default)]
pub(crate) struct SshEnv {
    /// `SSH_AUTH_SOCK`。
    pub agent_socket: Option<PathBuf>,
    /// `HOME`。
    pub home_dir: Option<PathBuf>,
}

/// 一个 SSH Profile（链上的一跳）解析出的连接目标。
#[derive(Clone, Debug)]
pub(crate) struct ConnectSpec {
    pub profile: ProfileId,
    /// Profile 名：作为这一跳的显示名，内层各跳的"经由 X"用它。
    pub name: String,
    /// 不含 `via`：链上的位置由 `connect_chain` 填。
    pub endpoint: Endpoint,
    pub user: String,
    pub auth: AuthMethod,
    /// 要从凭据存储读取的凭据种类；agent 认证不读。
    pub secret_kind: Option<SecretKind>,
    /// 这一跳的保活配置（每跳用它自己 Profile 的配置）。
    pub keepalive: KeepaliveConfig,
}

/// 主机密钥存储与吊销查询（Direct 版是同一个 `KnownHostsFile`）。
#[derive(Clone)]
pub(crate) struct HostKeys {
    pub store: Arc<dyn HostKeyStore>,
    pub revoked: Option<Arc<dyn RevokedKeys>>,
}

/// 会话层 `SshConnector` 的 core 实现。
pub(crate) struct CoreSshConnector {
    pub prompts: Arc<PromptTable>,
    pub events: Arc<dyn EventSink>,
    pub secrets: Arc<dyn SecretStore>,
    pub host_keys: HostKeys,
    pub env: SshEnv,
    /// 从外到内的各跳，最后一个是目标；无跳板时只有目标一项。
    /// 每一跳读**自己** Profile 的凭据（钥匙串）、用自己的认证方式。
    pub chain: Vec<ConnectSpec>,
    /// 第一跳的 ProxyCommand（与跳板机互斥，只能挂在最外层）。
    pub proxy_command: Option<String>,
    /// `Some` 表示非交互连接（MCP）：不提问、不自动信任未知主机密钥，需要人参与的原因记入这个槽。
    /// 跳板链的每一跳共用同一个槽与同一套规则。
    pub non_interactive: Option<Arc<NeedSlot>>,
}

impl CoreSshConnector {
    /// 读某一跳已保存的凭据。读失败当作没保存过（`SecretStore` 的约定、store-import.md §2）：
    /// 之后认证流程会向用户提问。
    async fn load_saved(&self, spec: &ConnectSpec) -> Option<SecretString> {
        let kind = spec.secret_kind?;
        let secrets = Arc::clone(&self.secrets);
        let r = SecretRef {
            profile: spec.profile,
            kind,
        };
        // 钥匙串可能弹授权框而阻塞，必须在阻塞线程里访问。
        match tokio::task::spawn_blocking(move || secrets.get(&r)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "failed to read saved credential, treating as absent");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "credential read task failed, treating as absent");
                None
            }
        }
    }

    /// 保存用户在某一跳选择"记住"的凭据（存到那一跳自己的 Profile 下）；
    /// 失败只记日志并发 `Notice`，不影响会话。
    async fn save_remembered(
        &self,
        spec: &ConnectSpec,
        session: SessionId,
        items: Vec<RememberedCredential>,
    ) {
        for item in items {
            let kind = match item.kind {
                CredentialKind::Password => SecretKind::Password,
                CredentialKind::KeyPassphrase { .. } => SecretKind::KeyPassphrase,
            };
            let secrets = Arc::clone(&self.secrets);
            let r = SecretRef {
                profile: spec.profile,
                kind,
            };
            let secret = item.secret;
            let result = tokio::task::spawn_blocking(move || secrets.set(&r, &secret)).await;
            let detail = match result {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => e.to_string(),
                Err(e) => e.to_string(),
            };
            tracing::warn!(%detail, "failed to save remembered credential");
            self.events.on_event(CoreEvent::Notice {
                session: Some(session),
                level: NoticeLevel::Warning,
                message: Message::SecretStoreFailed { detail },
            });
        }
    }
}

impl SshConnector for CoreSshConnector {
    fn connect(&self, session: SessionId) -> BoxFuture<Result<Connection, SshError>> {
        // 需要 'static：把用到的东西克隆进 async 块，而不是借 `&self`。
        let me = CoreSshConnector {
            prompts: Arc::clone(&self.prompts),
            events: Arc::clone(&self.events),
            secrets: Arc::clone(&self.secrets),
            host_keys: self.host_keys.clone(),
            env: self.env.clone(),
            chain: self.chain.clone(),
            proxy_command: self.proxy_command.clone(),
            non_interactive: self.non_interactive.clone(),
        };
        Box::pin(async move {
            // 非交互时换成"不写入"的主机密钥存储；吊销查询不涉及写入，照常。
            let host_store: Arc<dyn HostKeyStore> = match &me.non_interactive {
                Some(slot) => Arc::new(NonInteractiveHostKeys {
                    inner: Arc::clone(&me.host_keys.store),
                    slot: Arc::clone(slot),
                }),
                None => Arc::clone(&me.host_keys.store),
            };
            let mut hops = Vec::with_capacity(me.chain.len());
            for (i, spec) in me.chain.iter().enumerate() {
                let saved = me.load_saved(spec).await;
                let (saved_password, saved_passphrase) = match spec.secret_kind {
                    Some(SecretKind::Password) => (saved, None),
                    Some(SecretKind::KeyPassphrase) => (None, saved),
                    None => (None, None),
                };
                hops.push(HopParams {
                    name: spec.name.clone(),
                    connect: ConnectParams {
                        endpoint: spec.endpoint.clone(),
                        user: spec.user.clone(),
                        auth: spec.auth.clone(),
                        saved_password,
                        saved_passphrase,
                        agent_socket: me.env.agent_socket.clone(),
                        home_dir: me.env.home_dir.clone(),
                        host_keys: Arc::clone(&host_store),
                        revoked_keys: me.host_keys.revoked.clone(),
                        keepalive: spec.keepalive.clone(),
                    },
                    proxy_command: if i == 0 {
                        me.proxy_command.clone()
                    } else {
                        None
                    },
                });
            }
            let chained = match &me.non_interactive {
                Some(slot) => {
                    let interaction = NonInteractive {
                        slot: Arc::clone(slot),
                    };
                    connect_chain(hops, &interaction).await?
                }
                None => {
                    let interaction = CoreInteraction {
                        prompts: Arc::clone(&me.prompts),
                        session,
                    };
                    connect_chain(hops, &interaction).await?
                }
            };
            // `remember` 与 `chain` 逐跳对应：每一跳的"记住"存回那一跳自己的 Profile。
            for (spec, items) in me.chain.iter().zip(chained.remember) {
                me.save_remembered(spec, session, items).await;
            }
            Ok(chained.connection)
        })
    }

    fn confirm_host_key_change(
        &self,
        session: SessionId,
        error: SshError,
    ) -> BoxFuture<Result<bool, SshError>> {
        let prompts = Arc::clone(&self.prompts);
        let store = Arc::clone(&self.host_keys.store);
        let non_interactive = self.non_interactive.clone();
        Box::pin(async move {
            let SshError::HostKeyChanged { endpoint, details } = error else {
                return Ok(false);
            };
            // 非交互：不提问、不替换记录，只记下原因并拒绝。
            if let Some(slot) = non_interactive {
                slot.record(UserNeed::ChangedHostKey);
                return Ok(false);
            }
            // 没有文件概念的存储（MAS 的数据库）没有位置信息：路径留空、行号 0。
            let (path, line) = details
                .known_hosts
                .as_ref()
                .map_or((String::new(), 0), |l| (l.path.clone(), l.line));
            let request = PromptRequest::HostKeyChanged {
                endpoint: endpoint.clone(),
                key_type: details.key_type.clone(),
                fingerprint_sha256: details.fingerprint.clone(),
                known_hosts_path: path,
                line,
            };
            // 主机密钥变更不经 kanso-ssh 的 `Interaction`，所以 180 秒超时要自己加（ssh.md §4.2 同理：
            // 窗口可能已关闭，不能让会话一直挂着）。超时丢弃 future，guard 发 `PromptDismissed`。
            let answer = tokio::time::timeout(
                PROMPT_TIMEOUT,
                prompts.ask(Some(session), request, Expects::HostKey),
            )
            .await;
            if !matches!(answer, Ok(Some(PromptResponse::AcceptHostKey))) {
                return Ok(false);
            }
            // 删除该主机所有旧记录再写入新密钥（ssh.md §3），成功后调用方自动重连。
            accept_changed_key(
                store,
                &endpoint.host,
                endpoint.port,
                &details.key_type,
                &details.key,
            )
            .await?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::platform::CollectingEventSink;

    fn table() -> (Arc<PromptTable>, Arc<CollectingEventSink>) {
        let sink = Arc::new(CollectingEventSink::new());
        (PromptTable::new(sink.clone()), sink)
    }

    fn request() -> PromptRequest {
        PromptRequest::ZmodemPickFiles
    }

    #[tokio::test]
    async fn respond_delivers_and_clears_without_dismissed() {
        let (t, sink) = table();
        let t2 = Arc::clone(&t);
        let task = tokio::spawn(async move { t2.ask(None, request(), Expects::Kbd).await });
        while t.len() == 0 {
            tokio::task::yield_now().await;
        }
        let id = match sink.take().remove(0) {
            CoreEvent::Prompt { prompt, .. } => prompt,
            e => panic!("{e:?}"),
        };
        t.respond(id, PromptResponse::Answers(vec!["x".into()]));
        assert_eq!(
            task.await.unwrap(),
            Some(PromptResponse::Answers(vec!["x".into()]))
        );
        assert_eq!(t.len(), 0);
        assert!(
            sink.take().is_empty(),
            "已应答的提问不应再发 PromptDismissed"
        );
        // 过期 id 再应答：静默忽略。
        t.respond(id, PromptResponse::Cancel);
        assert!(sink.take().is_empty());
    }

    #[tokio::test]
    async fn mismatched_response_becomes_cancel() {
        let (t, sink) = table();
        let t2 = Arc::clone(&t);
        let task = tokio::spawn(async move { t2.ask(None, request(), Expects::Credential).await });
        while t.len() == 0 {
            tokio::task::yield_now().await;
        }
        let CoreEvent::Prompt { prompt, .. } = sink.take().remove(0) else {
            panic!()
        };
        t.respond(prompt, PromptResponse::AcceptHostKey);
        assert_eq!(task.await.unwrap(), Some(PromptResponse::Cancel));
    }

    #[tokio::test]
    async fn dropped_future_is_dismissed_and_removed() {
        let (t, sink) = table();
        // 模拟超时：future 被 timeout 丢弃。
        let r = tokio::time::timeout(
            Duration::from_millis(20),
            t.ask(None, request(), Expects::Kbd),
        )
        .await;
        assert!(r.is_err());
        assert_eq!(t.len(), 0);
        let events = sink.take();
        assert_eq!(events.len(), 2, "{events:?}");
        let (CoreEvent::Prompt { prompt: a, .. }, CoreEvent::PromptDismissed { prompt: b }) =
            (&events[0], &events[1])
        else {
            panic!("{events:?}")
        };
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn cancel_session_dismisses_only_that_session() {
        let (t, sink) = table();
        let (s1, s2) = (SessionId::from_raw(1), SessionId::from_raw(2));
        let (ta, tb) = (Arc::clone(&t), Arc::clone(&t));
        let a = tokio::spawn(async move { ta.ask(Some(s1), request(), Expects::Kbd).await });
        let b = tokio::spawn(async move { tb.ask(Some(s2), request(), Expects::Kbd).await });
        while t.len() < 2 {
            tokio::task::yield_now().await;
        }
        sink.take();
        t.cancel(Some(s1));
        assert_eq!(a.await.unwrap(), None);
        assert_eq!(t.len(), 1);
        let events = sink.take();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(matches!(events[0], CoreEvent::PromptDismissed { .. }));
        t.cancel(None);
        assert_eq!(b.await.unwrap(), None);
        assert_eq!(t.len(), 0);
    }
}
