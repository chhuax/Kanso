//! 真实 core 的 SSH 集成测试（I2）：进程内 russh 服务器（`kanso_ssh::test_server`）+ 临时 known_hosts +
//! `MemorySecretStore` + 记录事件的 `EventSink`。
//!
//! 隔离：只连 127.0.0.1 上的测试服务器；HOME、数据目录、known_hosts 都是临时目录，
//! 不读写真实的 `~/.ssh`，不连 ssh-agent，不碰真实钥匙串。

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, Notification, Notifier, OpenTarget, Platform,
    SecretKind,
};
use kanso_frame::{CELL_WIDE_SPACER, Frame};
use kanso_secret::{MemorySecretStore, SecretRef, SecretStore};
use kanso_ssh::test_server::{
    KbdRoundSpec, Policy, ServerConfig, TestServer, ed25519_key, legacy_only_config, methods,
    public_key_blob,
};
use kanso_ssh::{HostKeyStore, KnownHostsFile};
use kanso_types::agent::AgentKind;
use kanso_types::event::{Activity, CloseReason, SessionState};
use kanso_types::message::Message;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::{
    CoreEvent, GridSize, PasteContent, ProfileId, PromptId, PromptRequest, PromptResponse,
    SessionId,
};
use russh::MethodKind;

const TIMEOUT: Duration = Duration::from_secs(20);
const GRID: GridSize = GridSize { cols: 80, rows: 24 };

/// 记录后台通知。
#[derive(Default)]
struct RecNotifier(std::sync::Mutex<Vec<Notification>>);

impl Notifier for RecNotifier {
    fn notify(&self, n: Notification) {
        self.0.lock().unwrap().push(n);
    }
}

struct Env {
    // 字段顺序即 drop 顺序：先关 core（断开会话），再停服务器，最后停运行时。
    core: Core,
    notes: Arc<RecNotifier>,
    sink: Arc<CollectingEventSink>,
    log: std::sync::Mutex<Vec<CoreEvent>>,
    secrets: Arc<MemorySecretStore>,
    known_hosts: Arc<KnownHostsFile>,
    server: TestServer,
    profile: Profile,
    _rt: tokio::runtime::Runtime,
    _tmp: tempfile::TempDir,
}

fn password_server() -> ServerConfig {
    ServerConfig {
        policy: Policy {
            password: Some("pw".into()),
            ..Policy::default()
        },
        ..ServerConfig::default()
    }
}

fn kbd_server() -> ServerConfig {
    ServerConfig {
        policy: Policy {
            methods: methods(&[MethodKind::KeyboardInteractive]),
            kbd: vec![KbdRoundSpec::question("Verification code: ", false)],
            kbd_expect: vec![Some(vec!["123456".into()])],
            ..Policy::default()
        },
        ..ServerConfig::default()
    }
}

fn open_server() -> ServerConfig {
    ServerConfig {
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        ..ServerConfig::default()
    }
}

fn target(port: u16) -> SshTarget {
    SshTarget {
        host: "127.0.0.1".into(),
        port,
        user: "tester".into(),
        auth: AuthMethod::Password,
        private_key_path: None,
        jump: None,
        proxy_command: None,
        keepalive: kanso_types::profile::KeepaliveConfig::default(),
    }
}

fn ssh_profile(name: &str, t: SshTarget, encoding: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group: None,
        kind: ProfileKind::Ssh(SshProfile {
            target: t,
            encoding: EncodingLabel::new(encoding),
            locale: None,
        }),
    }
}

impl Env {
    fn new(cfg: ServerConfig) -> Env {
        Env::with_encoding(cfg, "")
    }

    fn with_encoding(cfg: ServerConfig, encoding: &str) -> Env {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(TestServer::start(cfg));
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let sink = Arc::new(CollectingEventSink::new());
        let secrets = Arc::new(MemorySecretStore::new());
        let known_hosts = Arc::new(KnownHostsFile::new(tmp.path().join("known_hosts")));
        let mut platform = Platform::for_tests(sink.clone());
        platform.secrets = secrets.clone();
        platform.host_keys = Some(known_hosts.clone());
        let notes = Arc::new(RecNotifier::default());
        platform.notifier = notes.clone();
        let core = Core::new_with_base_env(
            CoreConfig::new(tmp.path().join("data"), "0.0.0-test", "test.kanso"),
            platform,
            vec![("HOME".into(), home.to_string_lossy().into_owned())],
        )
        .unwrap();
        let profile = ssh_profile("dev box", target(server.port), encoding);
        core.save_profile(profile.clone()).unwrap();
        Env {
            core,
            notes,
            sink,
            log: Default::default(),
            secrets,
            known_hosts,
            server,
            profile,
            _rt: rt,
            _tmp: tmp,
        }
    }

    fn save_password(&self, pw: &str) {
        self.secrets
            .set(
                &SecretRef {
                    profile: self.profile.id,
                    kind: SecretKind::Password,
                },
                &SecretString::new(pw),
            )
            .unwrap();
    }

    fn saved_password(&self) -> Option<String> {
        self.secrets
            .get(&SecretRef {
                profile: self.profile.id,
                kind: SecretKind::Password,
            })
            .unwrap()
            .map(|s| s.expose().to_owned())
    }

    fn events(&self) -> Vec<CoreEvent> {
        let mut log = self.log.lock().unwrap();
        log.extend(self.sink.take());
        log.clone()
    }

    fn wait_event(&self, what: &str, pred: impl Fn(&CoreEvent) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if self.events().iter().any(&pred) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "等不到事件：{what}\n{:#?}",
                self.events()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_until(&self, what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !cond() {
            assert!(Instant::now() < deadline, "等不到条件：{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn open(&self) -> SessionId {
        self.core
            .open_session(OpenTarget::Profile(self.profile.id), GRID)
            .unwrap()
    }

    fn wait_connected(&self, id: SessionId) {
        self.wait_event("Connected", |e| {
            state_of(e, id) == Some(SessionState::Connected)
        });
    }

    fn wait_closed(&self, id: SessionId) -> CloseReason {
        self.wait_event("Closed", |e| closed_of(e, id).is_some());
        self.events()
            .iter()
            .find_map(|e| closed_of(e, id).cloned())
            .unwrap()
    }

    /// 第 `nth`（从 0 起）个关联该会话的 Prompt。
    fn wait_prompt(&self, id: SessionId, nth: usize) -> (PromptId, PromptRequest) {
        let pick = |events: &[CoreEvent]| {
            events
                .iter()
                .filter_map(|e| match e {
                    CoreEvent::Prompt {
                        prompt,
                        session: Some(s),
                        request,
                    } if *s == id => Some((*prompt, request.clone())),
                    _ => None,
                })
                .nth(nth)
        };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(p) = pick(&self.events()) {
                return p;
            }
            assert!(Instant::now() < deadline, "等不到第 {nth} 个 Prompt");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_screen(&self, id: SessionId, needle: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        let mut reuse: Option<Frame> = None;
        let mut text = String::new();
        loop {
            if let Some(f) = self.core.take_frame(id, &mut reuse) {
                text = frame_text(&f);
                reuse = Some(f);
            }
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "屏幕上一直没有 {needle:?}：\n{text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn type_text(&self, id: SessionId, text: &str) {
        self.core.text_input(id, text).unwrap();
    }
}

fn activities(env: &Env, id: SessionId) -> Vec<Activity> {
    env.events()
        .into_iter()
        .filter_map(|e| match e {
            CoreEvent::ActivityChanged { session, activity } if session == id => Some(activity),
            _ => None,
        })
        .collect()
}

fn state_of(e: &CoreEvent, id: SessionId) -> Option<SessionState> {
    match e {
        CoreEvent::SessionStateChanged { session, state } if *session == id => Some(state.clone()),
        _ => None,
    }
}

fn closed_of(e: &CoreEvent, id: SessionId) -> Option<&CloseReason> {
    match e {
        CoreEvent::SessionStateChanged {
            session,
            state: SessionState::Closed(r),
        } if *session == id => Some(r),
        _ => None,
    }
}

fn frame_text(frame: &Frame) -> String {
    let view = frame.view().unwrap();
    let mut out = String::new();
    for r in 0..view.header().rows {
        for cell in view.row(r) {
            if cell.flags & CELL_WIDE_SPACER != 0 {
                continue;
            }
            out.push(if cell.ch == 0 {
                ' '
            } else {
                char::from_u32(cell.ch).unwrap_or('?')
            });
        }
        out.push('\n');
    }
    out
}

fn error_key(r: &CloseReason) -> &'static str {
    match r {
        CloseReason::Error(m) => m.key(),
        other => panic!("期望 Error，实际 {other:?}"),
    }
}

fn count_connecting(env: &Env, id: SessionId) -> usize {
    env.events()
        .iter()
        .filter(|e| state_of(e, id) == Some(SessionState::Connecting))
        .count()
}

// ---- 打开、输入输出、编码 ----

#[test]
fn open_connects_with_saved_password_and_io_roundtrips() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let id = env.open();
    // 契约：立即返回 id，状态先是 Connecting，随后 Connected。
    let first = env.events();
    assert_eq!(
        state_of(&first[0], id),
        Some(SessionState::Connecting),
        "{first:?}"
    );
    env.wait_connected(id);
    env.wait_screen(id, "WELCOME");

    env.type_text(id, "hello\r");
    // 服务器回显：输入到达了服务器，输出回到了终端。
    env.wait_screen(id, "hello");
    env.server
        .stats
        .with(|o| assert!(o.stdin_seen.starts_with(b"hello\r"), "{:?}", o.stdin_seen));

    // 保存的密码自动使用：没有任何提问。
    assert!(
        !env.events()
            .iter()
            .any(|e| matches!(e, CoreEvent::Prompt { .. }))
    );
    env.server.stats.with(|o| {
        assert_eq!(
            o.auth_log
                .iter()
                .filter(|l| l.starts_with("password:"))
                .count(),
            1
        )
    });
    // 初始 PTY 尺寸取自 open_session 的 size；之后的 resize 变成 window-change。
    env.server.stats.with(|o| {
        assert_eq!(o.ptys[0], ("xterm-256color".to_owned(), 80, 24));
    });
    env.core
        .resize(
            id,
            GridSize {
                cols: 100,
                rows: 30,
            },
            8,
            16,
        )
        .unwrap();
    env.wait_until("window-change", || {
        env.server
            .stats
            .with(|o| o.window_changes.contains(&(100, 30)))
    });

    let info = env.core.session_info(id).unwrap();
    assert_eq!(info.kind, kanso_types::profile::SessionKind::Ssh);
    assert_eq!(info.name, "dev box");
    assert_eq!(info.profile, Some(env.profile.id));
    assert_eq!(info.state, SessionState::Connected);
    // 主机密钥按 accept-new 写进了临时 known_hosts（不是真实的 ~/.ssh）。
    assert_eq!(
        env.known_hosts
            .lookup("127.0.0.1", env.server.port)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn paste_reaches_server() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.core
        .paste(id, PasteContent::Text("pasted-text".into()))
        .unwrap();
    env.wait_screen(id, "pasted-text");
    env.server
        .stats
        .with(|o| assert!(o.stdin_seen.ends_with(b"pasted-text"), "{:?}", o.stdin_seen));
}

#[test]
fn gbk_session_decodes_output_and_encodes_input() {
    let env = Env::with_encoding(password_server(), "gbk");
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.type_text(id, "你好\r");
    // 服务器收到的是 GBK 字节，不是 UTF-8。
    env.wait_until("stdin", || {
        env.server
            .stats
            .with(|o| o.stdin_seen == [0xC4, 0xE3, 0xBA, 0xC3, b'\r'])
    });
    // 服务器回显的 GBK 字节在终端里解码回汉字。
    env.wait_screen(id, "你好");
}

// ---- 凭据与提问 ----

#[test]
fn missing_password_prompts_and_remembers_when_asked() {
    let env = Env::new(password_server());
    assert_eq!(env.saved_password(), None);
    let id = env.open();
    let (pid, req) = env.wait_prompt(id, 0);
    let PromptRequest::Credential {
        endpoint,
        user,
        kind,
    } = req
    else {
        panic!("{req:?}")
    };
    assert_eq!(endpoint, env.server.endpoint());
    assert_eq!(user, "tester");
    assert_eq!(kind, kanso_types::prompt::CredentialKind::Password);
    // 还没回答：密码没存。
    assert_eq!(env.saved_password(), None);
    env.core.respond_prompt(
        pid,
        PromptResponse::Credential {
            secret: SecretString::new("pw"),
            remember: true,
        },
    );
    env.wait_connected(id);
    // 认证成功后才保存，且是用户输入的那个密码。
    env.wait_until("credential saved", || env.saved_password().is_some());
    assert_eq!(env.saved_password().as_deref(), Some("pw"));
    // 应答后提问已从表里移除：再次应答静默忽略，也不会有 PromptDismissed。
    env.core.respond_prompt(pid, PromptResponse::Cancel);
    assert!(
        !env.events()
            .iter()
            .any(|e| matches!(e, CoreEvent::PromptDismissed { .. }))
    );
}

#[test]
fn credential_not_saved_without_remember() {
    let env = Env::new(password_server());
    let id = env.open();
    let (pid, _) = env.wait_prompt(id, 0);
    env.core.respond_prompt(
        pid,
        PromptResponse::Credential {
            secret: SecretString::new("pw"),
            remember: false,
        },
    );
    env.wait_connected(id);
    assert_eq!(env.saved_password(), None);
}

#[test]
fn mismatched_prompt_response_cancels_login() {
    let env = Env::new(password_server());
    let id = env.open();
    let (pid, _) = env.wait_prompt(id, 0);
    // 凭据提问收到 AcceptHostKey：类型不匹配，按 Cancel。
    env.core.respond_prompt(pid, PromptResponse::AcceptHostKey);
    let reason = env.wait_closed(id);
    assert_eq!(error_key(&reason), "auth-cancelled");
}

#[test]
fn keyboard_interactive_roundtrip() {
    let env = Env::new(kbd_server());
    let id = env.open();
    let (pid, req) = env.wait_prompt(id, 0);
    let PromptRequest::KeyboardInteractive {
        questions, user, ..
    } = req
    else {
        panic!("{req:?}")
    };
    assert_eq!(user, "tester");
    assert_eq!(questions[0].text, "Verification code: ");
    assert!(!questions[0].echo);
    env.core
        .respond_prompt(pid, PromptResponse::Answers(vec!["123456".into()]));
    env.wait_connected(id);
    env.server
        .stats
        .with(|o| assert_eq!(o.kbd_answers, [vec!["123456".to_owned()]]));
}

#[test]
fn keyboard_interactive_cancel_closes_with_auth_cancelled() {
    let env = Env::new(kbd_server());
    let id = env.open();
    let (pid, _) = env.wait_prompt(id, 0);
    env.core.respond_prompt(pid, PromptResponse::Cancel);
    let reason = env.wait_closed(id);
    assert_eq!(error_key(&reason), "auth-cancelled");
    // 取消是用户的应答，不是失效：没有 PromptDismissed。
    assert!(
        !env.events()
            .iter()
            .any(|e| matches!(e, CoreEvent::PromptDismissed { .. }))
    );
}

#[test]
fn closing_session_dismisses_pending_prompt_and_ignores_late_answers() {
    let env = Env::new(kbd_server());
    let id = env.open();
    let (pid, _) = env.wait_prompt(id, 0);
    env.core.close_session(id);
    assert_eq!(env.wait_closed(id), CloseReason::ByUser);
    env.wait_event("PromptDismissed", |e| {
        *e == CoreEvent::PromptDismissed { prompt: pid }
    });
    // 只通知一次。
    let dismissed = env
        .events()
        .iter()
        .filter(|e| matches!(e, CoreEvent::PromptDismissed { .. }))
        .count();
    assert_eq!(dismissed, 1);
    // 过期 id 的应答：什么也不发生（服务器没有收到任何答案，会话也不会复活）。
    env.core
        .respond_prompt(pid, PromptResponse::Answers(vec!["123456".into()]));
    std::thread::sleep(Duration::from_millis(200));
    env.server.stats.with(|o| assert!(o.kbd_answers.is_empty()));
}

#[test]
fn respond_prompt_with_unknown_id_is_noop() {
    let env = Env::new(password_server());
    let before = env.events().len();
    env.core
        .respond_prompt(PromptId::from_raw(424_242), PromptResponse::Cancel);
    env.core
        .respond_prompt(PromptId::from_raw(0), PromptResponse::AcceptHostKey);
    assert_eq!(env.events().len(), before);
}

// ---- 主机密钥变更 ----

fn seed_old_key(env: &Env) {
    env.known_hosts
        .add(
            "127.0.0.1",
            env.server.port,
            "ssh-ed25519",
            &public_key_blob(&ed25519_key(9)),
        )
        .unwrap();
}

#[test]
fn host_key_change_accept_replaces_record_and_reconnects_same_session() {
    let env = Env::new(open_server());
    seed_old_key(&env);
    let id = env.open();
    let (pid, req) = env.wait_prompt(id, 0);
    let PromptRequest::HostKeyChanged {
        endpoint,
        key_type,
        fingerprint_sha256,
        known_hosts_path,
        line,
    } = req
    else {
        panic!("{req:?}")
    };
    assert_eq!(endpoint.port, env.server.port);
    assert_eq!(key_type, "ssh-ed25519");
    assert!(
        fingerprint_sha256.starts_with("SHA256:"),
        "{fingerprint_sha256}"
    );
    assert_eq!(
        known_hosts_path,
        env.known_hosts.path().display().to_string()
    );
    assert_eq!(line, 1);
    // 拒绝发生在认证之前：服务器没有看到任何认证尝试，更没有 shell。
    env.server.stats.with(|o| {
        assert!(o.auth_log.is_empty());
        assert_eq!(o.shells, 0);
    });

    env.core.respond_prompt(pid, PromptResponse::AcceptHostKey);
    env.wait_connected(id);
    // 同一个会话 id 重连：状态回到过 Connecting（共两次），没有新会话。
    assert_eq!(count_connecting(&env, id), 2);
    assert_eq!(env.core.sessions().len(), 1);
    // 旧记录被整体替换为服务器当前的密钥。
    let now = env
        .known_hosts
        .lookup("127.0.0.1", env.server.port)
        .unwrap();
    assert_eq!(now.len(), 1);
    assert_eq!(now[0].key, public_key_blob(&ed25519_key(1)));
    env.wait_screen(id, "WELCOME");
}

#[test]
fn host_key_change_cancel_closes_with_hostkey_changed_message() {
    let env = Env::new(open_server());
    seed_old_key(&env);
    let id = env.open();
    let (pid, _) = env.wait_prompt(id, 0);
    env.core.respond_prompt(pid, PromptResponse::Cancel);
    let reason = env.wait_closed(id);
    assert_eq!(error_key(&reason), "hostkey-changed");
    // 记录原样保留。
    let now = env
        .known_hosts
        .lookup("127.0.0.1", env.server.port)
        .unwrap();
    assert_eq!(now[0].key, public_key_blob(&ed25519_key(9)));
    env.server.stats.with(|o| assert_eq!(o.shells, 0));
}

// ---- 复制标签与共享连接 ----

#[test]
fn duplicate_reuses_connection_without_reauth_and_last_close_disconnects() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let a = env.open();
    env.wait_connected(a);
    let b = env
        .core
        .open_session(OpenTarget::Duplicate(a), GRID)
        .unwrap();
    env.wait_connected(b);
    // 同一条连接、同一次认证，两个 shell。
    env.server.stats.with(|o| {
        assert_eq!(o.accepted, 1);
        assert_eq!(o.shells, 2);
        assert_eq!(
            o.auth_log
                .iter()
                .filter(|l| l.starts_with("password:"))
                .count(),
            1
        );
    });
    assert_eq!(
        env.core.session_info(b).unwrap().profile,
        Some(env.profile.id)
    );

    // 关闭一个不影响另一个。
    env.core.close_session(a);
    assert_eq!(env.wait_closed(a), CloseReason::ByUser);
    env.type_text(b, "still-alive\r");
    env.wait_screen(b, "still-alive");
    env.server.stats.with(|o| assert_eq!(o.closed, 0));

    // 都关了，连接才断开。
    env.core.close_session(b);
    env.wait_until("server sees disconnect", || {
        env.server.stats.with(|o| o.closed == 1)
    });
}

#[test]
fn duplicate_of_session_that_lost_its_connection_reports_shared_connection_closed() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let a = env.open();
    env.wait_connected(a);
    env.type_text(a, "exit\n");
    env.wait_closed(a);
    // 源会话已结束并放开了连接：复制不会偷偷重连。
    let c = env
        .core
        .open_session(OpenTarget::Duplicate(a), GRID)
        .unwrap();
    let reason = env.wait_closed(c);
    assert_eq!(error_key(&reason), "ssh-shared-connection-closed");
    env.server.stats.with(|o| assert_eq!(o.accepted, 1));
}

// ---- 结束 ----

#[test]
fn shell_exit_code_becomes_exited_and_connection_is_released() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.type_text(id, "exit\n");
    assert_eq!(env.wait_closed(id), CloseReason::Exited { code: Some(3) });
    // 会话结束后不再持有连接：服务器看到断开。
    env.wait_until("connection released", || {
        env.server.stats.with(|o| o.closed == 1)
    });
    // 已结束的会话仍在表里（UI 还要显示最后的画面）。
    assert!(env.core.session_info(id).is_some());
}

#[test]
fn server_disconnect_closes_with_connection_lost() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.type_text(id, "disconnect\n");
    let reason = env.wait_closed(id);
    assert_eq!(error_key(&reason), "ssh-connection-lost");
}

#[test]
fn user_close_is_by_user_without_error() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.core.close_session(id);
    assert_eq!(env.wait_closed(id), CloseReason::ByUser);
    assert!(env.core.session_info(id).is_none());
    env.wait_until("server sees disconnect", || {
        env.server.stats.with(|o| o.closed == 1)
    });
}

// ---- 其他 ----

#[test]
fn legacy_algorithms_event_is_emitted() {
    let env = Env::new(legacy_only_config());
    let id = env.open();
    env.wait_connected(id);
    env.wait_event("LegacyAlgorithms", |e| {
        matches!(e, CoreEvent::LegacyAlgorithms { session, hops }
            if *session == id && hops.len() == 1 && hops[0].algorithms.len() == 4)
    });
}

#[test]
fn modern_server_emits_no_legacy_event() {
    let env = Env::new(open_server());
    let id = env.open();
    env.wait_connected(id);
    assert!(
        !env.events()
            .iter()
            .any(|e| matches!(e, CoreEvent::LegacyAlgorithms { .. }))
    );
}

#[test]
fn working_directory_uses_osc7_and_reports_unavailable_otherwise() {
    // exec 查询的回复里没有带前缀的结果行：按"查不到"处理。
    let env = Env::new(ServerConfig {
        sh_reply: Some(b"motd noise only\n".to_vec()),
        ..password_server()
    });
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    // 还没有 OSC 7：会到服务器上 exec 查询（回复里没有结果行，所以报"查不到"；
    // 查询成功与各失败码的映射见 real_ssh_jump.rs）。
    let err = rt.block_on(env.core.working_directory(id)).unwrap_err();
    assert_eq!(err, CoreError::User(Message::CwdUnavailable {}), "{err:?}");
    // 服务器回显 OSC 7：终端记录后就能取到。
    env.type_text(id, "\x1b]7;file://host/home/me\x07");
    env.wait_until("osc7", || {
        rt.block_on(env.core.working_directory(id)).as_deref() == Ok("/home/me")
    });
}

// ---- 命令跟踪推断与 AI 轮次（F5）：SSH 没有 OSC 133，全靠提示符推断 ----

fn fake_shell_server() -> ServerConfig {
    ServerConfig {
        fake_shell_prompt: Some(b"remote$ ".to_vec()),
        ..password_server()
    }
}

#[test]
fn zmodem_ssh_detects_handshake_blocks_input_and_cancels() {
    let env = Env::new(ServerConfig {
        fake_shell_prompt: Some(b"**\x18B0100000023be50\r\n\x11".to_vec()),
        ..password_server()
    });
    env.save_password("pw");
    let session = env.open();
    env.wait_connected(session);
    let (prompt, request) = env.wait_prompt(session, 0);
    assert_eq!(request, PromptRequest::ZmodemPickFiles);
    env.core.text_input(session, "KEYBOARD").unwrap();
    env.core
        .paste(session, PasteContent::Text("PASTE".into()))
        .unwrap();
    env.core.cancel_terminal_transfer(session);
    env.wait_event("Cancelled", |event| {
        matches!(
            event,
            CoreEvent::TransferFinished {
                outcome: kanso_types::event::TransferOutcome::Cancelled,
                ..
            }
        )
    });
    env.wait_event(
        "PromptDismissed",
        |event| matches!(event, CoreEvent::PromptDismissed { prompt: id } if *id == prompt),
    );
    env.wait_until("取消序列", || {
        !env.server.stats.with(|stats| stats.stdin_seen.is_empty())
    });
    assert_eq!(
        env.server.stats.with(|stats| stats.stdin_seen.clone()),
        [vec![0x18; 8], vec![8; 10]].concat()
    );
    let screen = env.wait_screen(session, "WELCOME");
    assert!(!screen.contains("**B0100"), "{screen}");
    env.core.shutdown();
}

#[test]
fn prompt_inference_tracks_remote_commands_and_agent_turns() {
    let env = Env::new(fake_shell_server());
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.wait_screen(id, "remote$");
    // 不可见：命令 / 轮次结束时才会发后台通知。
    env.core.set_visible(id, false);

    // 长命令：运行中，期间的输入属于这条命令（`done` 不是新命令行）。
    env.type_text(id, "hold\r");
    env.wait_until("CommandRunning", || {
        activities(&env, id) == [Activity::CommandRunning]
    });
    env.type_text(id, "done\r");
    env.wait_until("回到 Idle", || {
        activities(&env, id) == [Activity::CommandRunning, Activity::Idle]
    });
    assert_eq!(
        *env.notes.0.lock().unwrap(),
        vec![Notification::CommandFinished {
            session: id,
            exit_code: None
        }]
    );

    // 立即返回的命令。
    env.type_text(id, "ls\r");
    env.wait_until("ls 的 Running/Idle", || activities(&env, id).len() == 4);

    // AI 工具：启动是 AgentIdle；用户按键开启轮次；2 秒无输出结束。
    let k = AgentKind::Claude;
    env.type_text(id, "claude\r");
    env.wait_until("AgentIdle", || {
        activities(&env, id)[4..] == [Activity::AgentIdle(k)]
    });
    env.type_text(id, "hi\r");
    env.wait_until("AgentWorking", || {
        activities(&env, id)[4..] == [Activity::AgentIdle(k), Activity::AgentWorking(k)]
    });
    env.wait_until("轮次结束", || {
        activities(&env, id)[4..]
            == [
                Activity::AgentIdle(k),
                Activity::AgentWorking(k),
                Activity::AgentIdle(k),
            ]
    });
    assert_eq!(
        env.notes.0.lock().unwrap().last(),
        Some(&Notification::AgentTurnDone {
            session: id,
            agent: k
        }),
        "轮次结束发 agent 完成通知"
    );
    assert_eq!(env.notes.0.lock().unwrap().len(), 3, "hold、ls 与一个轮次");

    // AI 会话里粘贴：路径原样，不转义。
    env.core
        .paste(id, PasteContent::Files(vec!["/tmp/a b.png".into()]))
        .unwrap();
    env.wait_screen(id, "/tmp/a b.png");
    // 模拟 shell 按行缓冲：先回车清掉这一行，下面的 `exit` 才是完整的一行。
    env.type_text(id, "\r");

    // 工具退出（提示符返回）：撤掉 agent 标识。
    env.type_text(id, "exit\r");
    env.wait_until("Idle", || {
        activities(&env, id).last() == Some(&Activity::Idle)
    });
}

#[test]
fn ssh_selection_reports_has_selection_and_text() {
    let env = Env::new(fake_shell_server());
    env.save_password("pw");
    let id = env.open();
    env.wait_connected(id);
    env.wait_screen(id, "remote$");

    assert!(!env.core.has_selection(id));
    env.core.select_all(id).unwrap();
    assert!(
        env.core.has_selection(id),
        "SSH 会话选中后 has_selection 应为真"
    );
    assert!(
        env.core
            .selection_text(id)
            .as_deref()
            .is_some_and(|text| text.contains("remote$")),
        "SSH 会话应能取出选区文本"
    );
    env.core.clear_selection(id).unwrap();
    assert!(!env.core.has_selection(id));
    env.core.shutdown();
}
