//! 真实 core 的跳板链、ProxyCommand 与远端工作目录测试（T6b）：进程内 russh 服务器（`kanso_ssh::test_server`）。
//!
//! 隔离：只连 127.0.0.1 上的测试服务器；HOME、数据目录、known_hosts 都是临时目录，
//! 不读写真实的 `~/.ssh`，不连 ssh-agent，不碰真实钥匙串。

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, OpenTarget, Platform, SecretKind,
};
use kanso_frame::{CELL_WIDE_SPACER, Frame};
use kanso_secret::{MemorySecretStore, SecretRef, SecretStore};
use kanso_ssh::test_server::{
    KbdRoundSpec, Policy, ServerConfig, TestServer, ed25519_key, legacy_only_config, methods,
    public_key_blob,
};
use kanso_ssh::{HostKeyStore, KnownHostsFile};
use kanso_types::event::{CloseReason, SessionState};
use kanso_types::message::Message;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::{
    CoreEvent, GridSize, ProfileId, PromptId, PromptRequest, PromptResponse, SessionId,
};
use russh::MethodKind;

const TIMEOUT: Duration = Duration::from_secs(20);
const GRID: GridSize = GridSize { cols: 80, rows: 24 };

struct Env {
    // 字段顺序即 drop 顺序：先关 core（断开会话），再停服务器与运行时。
    core: Core,
    sink: Arc<CollectingEventSink>,
    log: std::sync::Mutex<Vec<CoreEvent>>,
    secrets: Arc<MemorySecretStore>,
    known_hosts: Arc<KnownHostsFile>,
    servers: Vec<TestServer>,
    rt: tokio::runtime::Runtime,
    _tmp: tempfile::TempDir,
}

fn password_server(pw: &str) -> ServerConfig {
    ServerConfig {
        policy: Policy {
            password: Some(pw.into()),
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

fn ssh_profile(name: &str, t: SshTarget) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group: None,
        kind: ProfileKind::Ssh(SshProfile {
            target: t,
            encoding: EncodingLabel::new(""),
            locale: None,
        }),
    }
}

fn via(server: &TestServer, jump: Option<&Profile>) -> SshTarget {
    SshTarget {
        jump: jump.map(|j| j.id),
        ..target(server.port)
    }
}

impl Env {
    fn new() -> Env {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let sink = Arc::new(CollectingEventSink::new());
        let secrets = Arc::new(MemorySecretStore::new());
        let known_hosts = Arc::new(KnownHostsFile::new(tmp.path().join("known_hosts")));
        let mut platform = Platform::for_tests(sink.clone());
        platform.secrets = secrets.clone();
        platform.host_keys = Some(known_hosts.clone());
        let core = Core::new_with_base_env(
            CoreConfig::new(tmp.path().join("data"), "0.0.0-test", "test.kanso"),
            platform,
            vec![("HOME".into(), home.to_string_lossy().into_owned())],
        )
        .unwrap();
        Env {
            core,
            sink,
            log: Default::default(),
            secrets,
            known_hosts,
            servers: Vec::new(),
            rt,
            _tmp: tmp,
        }
    }

    /// 启动一个测试服务器，返回它在 `servers` 里的下标。
    fn start(&mut self, cfg: ServerConfig) -> usize {
        let s = self.rt.block_on(TestServer::start(cfg));
        self.servers.push(s);
        self.servers.len() - 1
    }

    fn save(&self, p: &Profile) {
        self.core.save_profile(p.clone()).unwrap();
    }

    fn save_password(&self, p: &Profile, pw: &str) {
        self.secrets
            .set(
                &SecretRef {
                    profile: p.id,
                    kind: SecretKind::Password,
                },
                &SecretString::new(pw),
            )
            .unwrap();
    }

    fn saved_password(&self, p: &Profile) -> Option<String> {
        self.secrets
            .get(&SecretRef {
                profile: p.id,
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

    fn open(&self, p: &Profile) -> SessionId {
        self.core
            .open_session(OpenTarget::Profile(p.id), GRID)
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

    fn wait_prompt(&self, id: SessionId, nth: usize) -> (PromptId, PromptRequest) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let found = self
                .events()
                .iter()
                .filter_map(|e| match e {
                    CoreEvent::Prompt {
                        prompt,
                        session: Some(s),
                        request,
                    } if *s == id => Some((*prompt, request.clone())),
                    _ => None,
                })
                .nth(nth);
            if let Some(p) = found {
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

    fn working_directory(&self, id: SessionId) -> Result<String, CoreError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(self.core.working_directory(id))
    }
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

// ---- 跳板链 ----

#[test]
fn jump_chain_opens_with_each_hops_own_saved_credentials() {
    let mut env = Env::new();
    let j = env.start(password_server("pw-jump"));
    let t = env.start(password_server("pw-target"));
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    env.save_password(&jump, "pw-jump");
    env.save_password(&dest, "pw-target");

    let id = env.open(&dest);
    env.wait_connected(id);
    env.wait_screen(id, "WELCOME");
    // 每一跳用自己 Profile 里的凭据。
    env.servers[j].stats.with(|o| {
        assert!(o.auth_log.iter().any(|l| l == "password:pw-jump"));
        assert_eq!(
            o.forwards,
            [("127.0.0.1".to_owned(), u32::from(env.servers[t].port))]
        );
        assert_eq!(o.shells, 0, "跳板机上不开 shell");
    });
    env.servers[t].stats.with(|o| {
        assert!(o.auth_log.iter().any(|l| l == "password:pw-target"));
        assert!(!o.auth_log.iter().any(|l| l == "password:pw-jump"));
        assert_eq!(o.shells, 1);
    });
}

#[test]
fn prompt_for_target_hop_names_the_jump_in_via_and_jump_prompt_has_empty_via() {
    let mut env = Env::new();
    let j = env.start(kbd_server());
    let t = env.start(kbd_server());
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);

    let id = env.open(&dest);
    // 第一个问题来自跳板机：直连它，没有 via。
    let (p0, r0) = env.wait_prompt(id, 0);
    let PromptRequest::KeyboardInteractive { endpoint, .. } = r0 else {
        panic!("{r0:?}")
    };
    assert_eq!(endpoint.port, env.servers[j].port);
    assert!(endpoint.via.is_empty());
    env.core
        .respond_prompt(p0, PromptResponse::Answers(vec!["123456".into()]));
    // 第二个问题来自目标：经由 bastion。
    let (p1, r1) = env.wait_prompt(id, 1);
    let PromptRequest::KeyboardInteractive { endpoint, .. } = r1 else {
        panic!("{r1:?}")
    };
    assert_eq!(endpoint.port, env.servers[t].port);
    assert_eq!(endpoint.via, ["bastion"]);
    env.core
        .respond_prompt(p1, PromptResponse::Answers(vec!["123456".into()]));
    env.wait_connected(id);
}

#[test]
fn remembered_credential_is_saved_under_the_hops_own_profile() {
    let mut env = Env::new();
    let j = env.start(password_server("pw-jump"));
    let t = env.start(open_server());
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);

    let id = env.open(&dest);
    // 跳板机缺密码：提问带"跳板机"的 endpoint；用户选择记住。
    let (p, req) = env.wait_prompt(id, 0);
    let PromptRequest::Credential { endpoint, .. } = req else {
        panic!("{req:?}")
    };
    assert_eq!(endpoint.port, env.servers[j].port);
    env.core.respond_prompt(
        p,
        PromptResponse::Credential {
            secret: SecretString::new("pw-jump"),
            remember: true,
        },
    );
    env.wait_connected(id);
    assert_eq!(env.saved_password(&jump).as_deref(), Some("pw-jump"));
    assert_eq!(env.saved_password(&dest), None);
}

#[test]
fn host_key_change_on_target_hop_names_the_hop_and_reconnects_whole_chain() {
    let mut env = Env::new();
    let j = env.start(open_server());
    let t = env.start(open_server());
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    env.known_hosts
        .add(
            "127.0.0.1",
            env.servers[t].port,
            "ssh-ed25519",
            &public_key_blob(&ed25519_key(9)),
        )
        .unwrap();

    let id = env.open(&dest);
    let (p, req) = env.wait_prompt(id, 0);
    let PromptRequest::HostKeyChanged { endpoint, .. } = req else {
        panic!("{req:?}")
    };
    // 指明是哪一跳：目标主机，经由 bastion。
    assert_eq!(endpoint.port, env.servers[t].port);
    assert_eq!(endpoint.via, ["bastion"]);
    env.core.respond_prompt(p, PromptResponse::AcceptHostKey);
    env.wait_connected(id);
    // 整条链重连：跳板机被连了两次（第一次随错误释放）。
    env.servers[j].stats.with(|o| assert_eq!(o.accepted, 2));
    let now = env
        .known_hosts
        .lookup("127.0.0.1", env.servers[t].port)
        .unwrap();
    assert_eq!(now[0].key, public_key_blob(&ed25519_key(1)));
}

#[test]
fn host_key_change_on_jump_hop_is_attributed_to_the_jump() {
    let mut env = Env::new();
    let j = env.start(open_server());
    let t = env.start(open_server());
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    env.known_hosts
        .add(
            "127.0.0.1",
            env.servers[j].port,
            "ssh-ed25519",
            &public_key_blob(&ed25519_key(9)),
        )
        .unwrap();
    let id = env.open(&dest);
    let (p, req) = env.wait_prompt(id, 0);
    let PromptRequest::HostKeyChanged { endpoint, .. } = req else {
        panic!("{req:?}")
    };
    assert_eq!(endpoint.port, env.servers[j].port);
    assert!(endpoint.via.is_empty());
    env.core.respond_prompt(p, PromptResponse::Cancel);
    assert_eq!(error_key(&env.wait_closed(id)), "hostkey-changed");
    // 目标主机完全没被碰过。
    env.servers[t].stats.with(|o| assert_eq!(o.accepted, 0));
}

#[test]
fn unreachable_target_closes_with_jump_unreachable() {
    let mut env = Env::new();
    let j = env.start(open_server());
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    // 目标端口没有任何服务在监听。
    let dead = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let dest = ssh_profile("prod", target(dead_port));
    let dest = Profile {
        kind: match dest.kind {
            ProfileKind::Ssh(mut s) => {
                s.target.jump = Some(jump.id);
                ProfileKind::Ssh(s)
            }
            k => k,
        },
        ..dest
    };
    env.save(&jump);
    env.save(&dest);
    let id = env.open(&dest);
    assert_eq!(error_key(&env.wait_closed(id)), "jump-unreachable");
}

#[test]
fn duplicate_tab_reuses_the_whole_chain_and_last_close_disconnects_every_hop() {
    let mut env = Env::new();
    let j = env.start(password_server("pw-jump"));
    let t = env.start(password_server("pw-target"));
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    env.save_password(&jump, "pw-jump");
    env.save_password(&dest, "pw-target");

    let a = env.open(&dest);
    env.wait_connected(a);
    let b = env
        .core
        .open_session(OpenTarget::Duplicate(a), GRID)
        .unwrap();
    env.wait_connected(b);
    for s in [j, t] {
        env.servers[s].stats.with(|o| {
            assert_eq!(o.accepted, 1);
            assert_eq!(
                o.auth_log
                    .iter()
                    .filter(|l| l.starts_with("password:"))
                    .count(),
                1
            );
        });
    }
    env.servers[t].stats.with(|o| assert_eq!(o.shells, 2));

    env.core.close_session(a);
    env.wait_closed(a);
    env.servers[j].stats.with(|o| assert_eq!(o.closed, 0));
    env.core.close_session(b);
    env.wait_until("both hops disconnect", || {
        env.servers.iter().all(|s| s.stats.with(|o| o.closed == 1))
    });
}

#[test]
fn legacy_algorithms_event_lists_every_legacy_hop_with_via() {
    let mut env = Env::new();
    let j = env.start(legacy_only_config());
    let t = env.start(legacy_only_config());
    let jump = ssh_profile("old-bastion", target(env.servers[j].port));
    let dest = ssh_profile("old-box", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    let id = env.open(&dest);
    env.wait_connected(id);
    env.wait_event("LegacyAlgorithms", |e| {
        matches!(e, CoreEvent::LegacyAlgorithms { session, hops } if *session == id && hops.len() == 2)
    });
    let hops = env
        .events()
        .into_iter()
        .find_map(|e| match e {
            CoreEvent::LegacyAlgorithms { hops, .. } => Some(hops),
            _ => None,
        })
        .unwrap();
    assert_eq!(hops[0].endpoint.port, env.servers[j].port);
    assert!(hops[0].endpoint.via.is_empty());
    assert_eq!(hops[1].endpoint.port, env.servers[t].port);
    assert_eq!(hops[1].endpoint.via, ["old-bastion"]);
}

// ---- Jump* 校验：不得悄悄直连 ----

fn assert_not_connected(env: &Env, server: usize) {
    env.servers[server]
        .stats
        .with(|o| assert_eq!(o.accepted, 0, "校验失败不得发起任何连接（含直连）"));
    assert!(env.core.sessions().is_empty());
}

#[test]
fn missing_jump_profile_is_an_error_not_a_direct_connection() {
    let mut env = Env::new();
    let t = env.start(open_server());
    let jump = ssh_profile("bastion", target(1));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    env.rt.block_on(env.core.delete_profile(jump.id)).unwrap();
    let err = env
        .core
        .open_session(OpenTarget::Profile(dest.id), GRID)
        .unwrap_err();
    assert!(
        matches!(err, CoreError::User(Message::JumpMissing { .. })),
        "{err:?}"
    );
    assert_not_connected(&env, t);
}

#[test]
fn non_ssh_jump_profile_is_an_error() {
    let mut env = Env::new();
    let t = env.start(open_server());
    let jump = ssh_profile("bastion", target(1));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    // 之后把被引用的 Profile 改成 SFTP 类型：引用方不会被改写，打开时报 JumpNotSsh。
    env.save(&Profile {
        kind: ProfileKind::Sftp(target(1)),
        ..jump.clone()
    });
    let err = env
        .core
        .open_session(OpenTarget::Profile(dest.id), GRID)
        .unwrap_err();
    assert_eq!(
        err,
        CoreError::User(Message::JumpNotSsh {
            profile: "bastion".into()
        })
    );
    assert_not_connected(&env, t);
}

#[test]
fn chain_longer_than_eight_jumps_is_an_error() {
    let mut env = Env::new();
    let t = env.start(open_server());
    // p0 ← p1 ← … ← p8（p8 经 8 个跳板，刚好合法），再让 p0 也经由一个跳板：p8 的链变成 9 跳。
    let mut chain: Vec<Profile> = Vec::new();
    for i in 0..9 {
        let t = SshTarget {
            jump: chain.last().map(|p| p.id),
            ..target(1)
        };
        let p = ssh_profile(&format!("p{i}"), t);
        env.save(&p);
        chain.push(p);
    }
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&chain[7])));
    env.save(&dest);
    let extra = ssh_profile("extra", target(1));
    env.save(&extra);
    env.save(&Profile {
        kind: ProfileKind::Ssh(SshProfile {
            target: SshTarget {
                jump: Some(extra.id),
                ..target(1)
            },
            encoding: EncodingLabel::new(""),
            locale: None,
        }),
        ..chain[0].clone()
    });
    let err = env
        .core
        .open_session(OpenTarget::Profile(dest.id), GRID)
        .unwrap_err();
    assert_eq!(err, CoreError::User(Message::JumpTooLong { max_hops: 8 }));
    assert_not_connected(&env, t);
}

#[test]
fn jump_and_proxy_command_together_is_rejected_at_save_time() {
    let env = Env::new();
    let jump = ssh_profile("bastion", target(1));
    env.save(&jump);
    let mut t = target(1);
    t.jump = Some(jump.id);
    t.proxy_command = Some("nc %h %p".into());
    let both = ssh_profile("both", t);
    assert_eq!(
        env.core.save_profile(both),
        Err(CoreError::User(Message::JumpProxyCommandConflict {}))
    );
}

// ---- ProxyCommand ----

fn have_nc() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("nc").is_file()))
        .unwrap_or(false)
}

#[cfg(not(feature = "mas"))]
#[test]
fn proxy_command_profile_connects_through_the_command() {
    if !have_nc() {
        return;
    }
    let mut env = Env::new();
    let t = env.start(password_server("pw"));
    let mut tg = target(env.servers[t].port);
    tg.proxy_command = Some("nc %h %p".into());
    let p = ssh_profile("via nc", tg);
    env.save(&p);
    env.save_password(&p, "pw");
    let id = env.open(&p);
    env.wait_connected(id);
    env.wait_screen(id, "WELCOME");
    env.servers[t].stats.with(|o| assert_eq!(o.accepted, 1));
}

#[cfg(not(feature = "mas"))]
#[test]
fn failing_proxy_command_closes_with_proxy_command_failed() {
    let env = Env::new();
    let mut tg = target(1);
    tg.proxy_command = Some("echo no-route >&2; exit 5".into());
    let p = ssh_profile("bad proxy", tg);
    env.save(&p);
    let id = env.open(&p);
    let reason = env.wait_closed(id);
    assert_eq!(error_key(&reason), "ssh-proxy-command-failed");
}

#[cfg(feature = "mas")]
#[test]
fn proxy_command_is_not_supported_in_the_mas_channel() {
    let mut env = Env::new();
    let t = env.start(open_server());
    let mut tg = target(env.servers[t].port);
    tg.proxy_command = Some("nc %h %p".into());
    let p = ssh_profile("via nc", tg);
    env.save(&p);
    assert_eq!(
        env.core.open_session(OpenTarget::Profile(p.id), GRID),
        Err(CoreError::NotSupported)
    );
    assert_not_connected(&env, t);
}

// ---- 远端工作目录 ----

#[test]
fn working_directory_queries_the_server_when_no_osc7_and_prefers_osc7_when_present() {
    let mut env = Env::new();
    let t = env.start(ServerConfig {
        sh_reply: Some(b"motd\n__KANSO_CWD__=/srv/from-exec\n".to_vec()),
        ..open_server()
    });
    let p = ssh_profile("box", target(env.servers[t].port));
    env.save(&p);
    let id = env.open(&p);
    env.wait_connected(id);
    env.wait_screen(id, "WELCOME");

    // 没有 OSC 7：走 exec 通道，同一连接、不新增 shell。
    assert_eq!(env.working_directory(id).as_deref(), Ok("/srv/from-exec"));
    env.servers[t].stats.with(|o| {
        assert_eq!(o.execs, ["sh"]);
        assert_eq!(o.accepted, 1);
        assert_eq!(o.shells, 1);
    });

    // 有 OSC 7：直接用上报值，不再 exec。
    core_type(&env, id, "\x1b]7;file://host/home/me\x07");
    env.wait_until("osc7", || {
        env.working_directory(id).as_deref() == Ok("/home/me")
    });
    // 等待期间 OSC 7 尚未被处理的那几次轮询可能多出 exec 查询；之后不应再增长。
    let before = env.servers[t].stats.with(|o| o.execs.len());
    assert_eq!(env.working_directory(id).as_deref(), Ok("/home/me"));
    assert_eq!(env.servers[t].stats.with(|o| o.execs.len()), before);
}

fn core_type(env: &Env, id: SessionId, text: &str) {
    env.core.text_input(id, text).unwrap();
}

#[test]
fn working_directory_failure_codes_map_to_cwd_messages() {
    let cases: [(&[u8], Message); 4] = [
        (b"__KANSO_CWD__!noshell\n", Message::CwdNoShell {}),
        (b"__KANSO_CWD__!nosshconn\n", Message::CwdNoSshConnection {}),
        (b"__KANSO_CWD__!otheruser\n", Message::CwdOtherUser {}),
        (b"__KANSO_CWD__!noproc\n", Message::CwdNoProc {}),
    ];
    for (reply, want) in cases {
        let mut env = Env::new();
        let t = env.start(ServerConfig {
            sh_reply: Some(reply.to_vec()),
            ..open_server()
        });
        let p = ssh_profile("box", target(env.servers[t].port));
        env.save(&p);
        let id = env.open(&p);
        env.wait_connected(id);
        assert_eq!(env.working_directory(id), Err(CoreError::User(want)));
    }
}

#[test]
fn working_directory_through_a_jump_chain_queries_the_target_connection() {
    let mut env = Env::new();
    let j = env.start(open_server());
    let t = env.start(ServerConfig {
        sh_reply: Some(b"__KANSO_CWD__=/srv/deep\n".to_vec()),
        ..open_server()
    });
    let jump = ssh_profile("bastion", target(env.servers[j].port));
    let dest = ssh_profile("prod", via(&env.servers[t], Some(&jump)));
    env.save(&jump);
    env.save(&dest);
    let id = env.open(&dest);
    env.wait_connected(id);
    assert_eq!(env.working_directory(id).as_deref(), Ok("/srv/deep"));
    env.servers[j].stats.with(|o| assert!(o.execs.is_empty()));
    env.servers[t].stats.with(|o| assert_eq!(o.execs, ["sh"]));
}
