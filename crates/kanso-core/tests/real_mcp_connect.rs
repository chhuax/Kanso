//! MCP 的连接复用、非交互建连、借用释放与后台执行（F9 P3）的集成测试：
//! 进程内 russh 服务器 + 临时 known_hosts + `MemorySecretStore`。
//!
//! 隔离同 `real_ssh.rs`：只连 127.0.0.1，HOME、数据目录、known_hosts 都是临时目录。

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use kanso_core::mcp::{ConnectMode, McpConnectOutcome, UserNeed, WaitLimits};
use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, OpenTarget, Platform, SecretKind,
};
use kanso_secret::{MemorySecretStore, SecretRef, SecretStore};
use kanso_ssh::test_server::{
    KbdRoundSpec, Policy, ServerConfig, TestServer, ed25519_key, methods, public_key_blob,
};
use kanso_ssh::{HostKeyStore, KnownHostsFile};
use kanso_types::event::SessionState;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::{CoreEvent, GridSize, ProfileId, SessionId};
use russh::MethodKind;

const TIMEOUT: Duration = Duration::from_secs(20);
const GRID: GridSize = GridSize { cols: 80, rows: 24 };

struct Env {
    // 字段顺序即 drop 顺序：先关 core，再停服务器，最后停运行时。
    core: Core,
    sink: Arc<CollectingEventSink>,
    log: std::sync::Mutex<Vec<CoreEvent>>,
    known_hosts: Arc<KnownHostsFile>,
    secrets: Arc<MemorySecretStore>,
    server: TestServer,
    profile: Profile,
    rt: tokio::runtime::Runtime,
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

impl Env {
    fn new(cfg: ServerConfig) -> Env {
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
        let core = Core::new_with_base_env(
            CoreConfig::new(tmp.path().join("data"), "0.0.0-test", "test.kanso"),
            platform,
            vec![("HOME".into(), home.to_string_lossy().into_owned())],
        )
        .unwrap();
        let profile = Profile {
            id: ProfileId::new(),
            name: "dev box".into(),
            color: None,
            group: None,
            kind: ProfileKind::Ssh(SshProfile {
                target: SshTarget {
                    host: "127.0.0.1".into(),
                    port: server.port,
                    user: "tester".into(),
                    auth: AuthMethod::Password,
                    private_key_path: None,
                    jump: None,
                    proxy_command: None,
                    keepalive: kanso_types::profile::KeepaliveConfig::default(),
                },
                encoding: EncodingLabel::new(""),
                locale: None,
            }),
        };
        core.save_profile(profile.clone()).unwrap();
        Env {
            core,
            sink,
            log: Default::default(),
            known_hosts,
            secrets,
            server,
            profile,
            rt,
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

    /// 预置服务器的主机密钥（测试服务器默认用 `ed25519_key(1)`），模拟"已知主机"。
    fn trust_server(&self) {
        self.known_hosts
            .add(
                "127.0.0.1",
                self.server.port,
                "ssh-ed25519",
                &public_key_blob(&ed25519_key(1)),
            )
            .unwrap();
    }

    fn events(&self) -> Vec<CoreEvent> {
        let mut log = self.log.lock().unwrap();
        log.extend(self.sink.take());
        log.clone()
    }

    fn wait_until(&self, what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !cond() {
            assert!(Instant::now() < deadline, "等不到条件：{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn open_tab(&self) -> SessionId {
        let id = self
            .core
            .open_session(OpenTarget::Profile(self.profile.id), GRID)
            .unwrap();
        self.wait_until("tab connected", || {
            self.core.session_info(id).map(|i| i.state) == Some(SessionState::Connected)
        });
        id
    }

    fn connect(&self, mode: ConnectMode) -> Result<McpConnectOutcome, CoreError> {
        self.rt
            .block_on(self.core.mcp_connect(self.profile.id, mode))
    }

    fn connected(&self, mode: ConnectMode) -> (SessionId, bool) {
        match self.connect(mode).unwrap() {
            McpConnectOutcome::Connected { session, borrowed } => (session, borrowed),
            other => panic!("期望 Connected，得到 {other:?}"),
        }
    }

    fn exec(
        &self,
        session: SessionId,
        cmd: &str,
        timeout: Duration,
    ) -> kanso_core::mcp::ExecOutcome {
        self.rt
            .block_on(self.core.mcp_exec(
                session,
                cmd,
                WaitLimits {
                    timeout,
                    ..WaitLimits::default()
                },
            ))
            .unwrap()
    }

    fn assert_no_prompts(&self) {
        let events = self.events();
        assert!(
            !events.iter().any(|e| matches!(
                e,
                CoreEvent::Prompt { .. } | CoreEvent::PromptDismissed { .. }
            )),
            "非交互连接不得发提问事件：{events:#?}"
        );
    }

    fn server_accepted(&self) -> usize {
        self.server.stats.with(|o| o.accepted)
    }

    fn password_auths(&self) -> usize {
        self.server.stats.with(|o| {
            o.auth_log
                .iter()
                .filter(|l| l.starts_with("password:"))
                .count()
        })
    }

    fn known_hosts_entries(&self) -> usize {
        self.known_hosts
            .lookup("127.0.0.1", self.server.port)
            .unwrap()
            .len()
    }
}

fn limits(timeout: Duration) -> WaitLimits {
    WaitLimits {
        timeout,
        ..WaitLimits::default()
    }
}

#[test]
fn reuse_only_without_connection_is_not_connected_and_never_dials() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    assert_eq!(
        env.connect(ConnectMode::ReuseOnly).unwrap(),
        McpConnectOutcome::NotConnected
    );
    let r = env.rt.block_on(env.core.mcp_open_session(
        env.profile.id,
        ConnectMode::ReuseOnly,
        GRID,
    ));
    assert_eq!(r.unwrap(), McpConnectOutcome::NotConnected);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(env.server_accepted(), 0);
    assert!(env.core.sessions().is_empty());
}

#[test]
fn unknown_profile_and_session_errors() {
    let env = Env::new(password_server());
    let ghost = ProfileId::new();
    assert!(matches!(
        env.rt
            .block_on(env.core.mcp_connect(ghost, ConnectMode::ReuseOnly)),
        Err(CoreError::NoSuchProfile(p)) if p == ghost
    ));
    let r = env.rt.block_on(env.core.mcp_exec(
        SessionId::from_raw(9999),
        "echo hi",
        limits(Duration::from_secs(1)),
    ));
    assert!(matches!(r, Err(CoreError::NoSuchSession(_))), "{r:?}");
}

#[test]
fn reuse_borrows_user_connection_and_last_tab_close_releases_it() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let tab = env.open_tab();

    let (h, borrowed) = env.connected(ConnectMode::ReuseOnly);
    assert!(borrowed, "用户建立的连接应是借用");
    // 复用：同一次连接、同一次认证。
    assert_eq!(env.server_accepted(), 1);
    assert_eq!(env.password_auths(), 1);

    // 无界面会话不出现在列表里，但能查到状态，且标记为无界面。
    let listed: Vec<_> = env.core.sessions().iter().map(|s| s.id).collect();
    assert_eq!(listed, vec![tab]);
    let st = env.core.mcp_session_status(h).unwrap();
    assert!(st.headless);
    assert!(!env.core.mcp_session_status(tab).unwrap().headless);

    // 用户关闭最后一个可见标签：借用的无界面会话立即被关闭，连接断开。
    env.core.close_session(tab);
    assert!(env.core.mcp_session_status(h).is_none(), "借用会话应已关闭");
    env.wait_until("server sees disconnect", || {
        env.server.stats.with(|o| o.closed == 1)
    });
    // 再次复用：已没有连接。
    assert_eq!(
        env.connect(ConnectMode::ReuseOnly).unwrap(),
        McpConnectOutcome::NotConnected
    );
}

#[test]
fn borrowed_session_survives_while_another_visible_tab_remains() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let a = env.open_tab();
    let b = env
        .core
        .open_session(OpenTarget::Duplicate(a), GRID)
        .unwrap();
    env.wait_until("dup connected", || {
        env.core.session_info(b).map(|i| i.state) == Some(SessionState::Connected)
    });
    let (h, _) = env.connected(ConnectMode::ReuseOnly);
    env.core.close_session(a);
    assert!(env.core.mcp_session_status(h).is_some());
    env.core.close_session(b);
    assert!(env.core.mcp_session_status(h).is_none());
}

fn is_closed(env: &Env, id: SessionId) -> bool {
    matches!(
        env.core.session_info(id).map(|i| i.state),
        Some(SessionState::Closed(_))
    )
}

#[test]
fn visible_session_ending_by_itself_releases_borrowed_connection() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let tab = env.open_tab();
    let (h, borrowed) = env.connected(ConnectMode::ReuseOnly);
    assert!(borrowed);
    // 测试服务器收到 `exit\n` 时发退出码并关闭 shell 通道：标签自己结束，没有经过 close_session。
    env.core.text_input(tab, "exit\n").unwrap();
    env.wait_until("tab ended", || is_closed(&env, tab));
    // 释放由事件触发的后台任务完成。
    env.wait_until("borrowed session released", || {
        env.core.mcp_session_status(h).is_none()
    });
    env.wait_until("server sees disconnect", || {
        env.server.stats.with(|o| o.closed == 1)
    });
}

#[test]
fn borrowed_session_stays_while_another_visible_tab_is_alive_after_self_exit() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let a = env.open_tab();
    let b = env
        .core
        .open_session(OpenTarget::Duplicate(a), GRID)
        .unwrap();
    env.wait_until("dup connected", || {
        env.core.session_info(b).map(|i| i.state) == Some(SessionState::Connected)
    });
    let (h, _) = env.connected(ConnectMode::ReuseOnly);

    env.core.text_input(a, "exit\n").unwrap();
    env.wait_until("a ended", || is_closed(&env, a));
    // 给释放任务留出时间：b 还活着，借用会话必须保留。
    std::thread::sleep(Duration::from_millis(300));
    assert!(env.core.mcp_session_status(h).is_some());

    env.core.text_input(b, "exit\n").unwrap();
    env.wait_until("b ended", || is_closed(&env, b));
    env.wait_until("borrowed session released", || {
        env.core.mcp_session_status(h).is_none()
    });
}

#[test]
fn non_interactive_missing_password_needs_user_without_prompting() {
    let env = Env::new(password_server());
    env.trust_server();
    assert_eq!(
        env.connect(ConnectMode::NonInteractive).unwrap(),
        McpConnectOutcome::NeedsUser(UserNeed::Password)
    );
    env.assert_no_prompts();
    // 放弃的会话已关闭，不留孤儿。
    assert!(env.core.sessions().is_empty());
}

#[test]
fn non_interactive_keyboard_interactive_needs_user() {
    let env = Env::new(kbd_server());
    env.trust_server();
    assert_eq!(
        env.connect(ConnectMode::NonInteractive).unwrap(),
        McpConnectOutcome::NeedsUser(UserNeed::KeyboardInteractive)
    );
    env.assert_no_prompts();
}

#[test]
fn non_interactive_unknown_host_key_needs_user_and_is_not_recorded() {
    let env = Env::new(password_server());
    env.save_password("pw");
    assert_eq!(env.known_hosts_entries(), 0);
    assert_eq!(
        env.connect(ConnectMode::NonInteractive).unwrap(),
        McpConnectOutcome::NeedsUser(UserNeed::UnknownHostKey)
    );
    assert_eq!(env.known_hosts_entries(), 0, "未知主机密钥不得被自动记录");
    env.assert_no_prompts();
}

#[test]
fn non_interactive_changed_host_key_needs_user_and_keeps_old_record() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.known_hosts
        .add(
            "127.0.0.1",
            env.server.port,
            "ssh-ed25519",
            &public_key_blob(&ed25519_key(9)),
        )
        .unwrap();
    assert_eq!(
        env.connect(ConnectMode::NonInteractive).unwrap(),
        McpConnectOutcome::NeedsUser(UserNeed::ChangedHostKey)
    );
    let entries = env
        .known_hosts
        .lookup("127.0.0.1", env.server.port)
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, public_key_blob(&ed25519_key(9)));
    env.assert_no_prompts();
}

#[test]
fn non_interactive_open_session_needs_user_and_closes_the_tab() {
    let env = Env::new(password_server());
    env.trust_server();
    let r = env.rt.block_on(env.core.mcp_open_session(
        env.profile.id,
        ConnectMode::NonInteractive,
        GRID,
    ));
    assert_eq!(r.unwrap(), McpConnectOutcome::NeedsUser(UserNeed::Password));
    assert!(env.core.sessions().is_empty(), "失败的标签必须被关闭");
    env.assert_no_prompts();
}

#[test]
fn self_built_connection_is_shared_with_user_tab_and_outlives_it() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();

    let (h, borrowed) = env.connected(ConnectMode::NonInteractive);
    assert!(!borrowed);
    assert_eq!(env.server_accepted(), 1);
    assert!(env.core.sessions().is_empty(), "无界面会话不出现在列表里");
    assert!(env.core.mcp_session_status(h).unwrap().headless);

    // 用户打开同一 Profile：共享自建连接，不新建连接、不再认证。
    let tab = env.open_tab();
    assert_eq!(env.server_accepted(), 1);
    assert_eq!(env.password_auths(), 1);

    // 关闭标签：自建连接不受借用释放影响。
    env.core.close_session(tab);
    std::thread::sleep(Duration::from_millis(300));
    assert!(env.core.mcp_session_status(h).is_some());
    env.server.stats.with(|o| assert_eq!(o.closed, 0));

    // 自建会话由调用方（kanso-mcp-server）关闭，之后连接断开。
    env.core.close_session(h);
    env.wait_until("server sees disconnect", || {
        env.server.stats.with(|o| o.closed == 1)
    });
}

#[test]
fn second_connect_reuses_self_built_connection_as_not_borrowed() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let (_h1, b1) = env.connected(ConnectMode::NonInteractive);
    let (_h2, b2) = env.connected(ConnectMode::ReuseOnly);
    assert!(!b1 && !b2);
    assert_eq!(env.server_accepted(), 1);
}

#[test]
fn dropped_connect_future_leaves_no_orphan_session() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let r = env.rt.block_on(async {
        tokio::time::timeout(
            Duration::from_micros(1),
            env.core
                .mcp_connect(env.profile.id, ConnectMode::NonInteractive),
        )
        .await
    });
    assert!(r.is_err(), "1 微秒内不可能连上");
    // 孤儿会话若没被关闭，会让连接一直存活并被 ReuseOnly 复用。
    env.wait_until("no reusable connection", || {
        env.connect(ConnectMode::ReuseOnly).unwrap() == McpConnectOutcome::NotConnected
    });
}

#[test]
fn dropped_open_session_future_closes_the_pending_tab() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let r = env.rt.block_on(async {
        tokio::time::timeout(
            Duration::from_micros(1),
            env.core
                .mcp_open_session(env.profile.id, ConnectMode::NonInteractive, GRID),
        )
        .await
    });
    assert!(r.is_err(), "1 微秒内不可能连上");
    // guard 在 future 被丢弃时同步关闭标签：宿主从未摆放过它，不能留下看不见的标签。
    assert!(env.core.sessions().is_empty());
}

#[test]
fn open_session_reuses_connection_and_appears_in_sessions() {
    let env = Env::new(password_server());
    env.save_password("pw");
    let tab = env.open_tab();
    let r = env
        .rt
        .block_on(
            env.core
                .mcp_open_session(env.profile.id, ConnectMode::ReuseOnly, GRID),
        )
        .unwrap();
    let McpConnectOutcome::Connected { session, borrowed } = r else {
        panic!("{r:?}")
    };
    assert!(borrowed);
    assert_eq!(env.server_accepted(), 1);
    env.wait_until("mcp tab connected", || {
        env.core.session_info(session).map(|i| i.state) == Some(SessionState::Connected)
    });
    let listed: Vec<_> = env.core.sessions().iter().map(|s| s.id).collect();
    assert_eq!(listed, vec![tab, session]);
    assert!(!env.core.mcp_session_status(session).unwrap().headless);
    env.server.stats.with(|o| assert_eq!(o.shells, 2));
}

#[test]
fn exec_collects_stdout_stderr_and_exit_status() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let (h, _) = env.connected(ConnectMode::NonInteractive);
    let out = env.exec(h, "echo hi; echo err 1>&2; exit 3", Duration::from_secs(10));
    assert_eq!(out.stdout.text, "hi\n");
    assert_eq!(out.stderr.text, "err\n");
    assert_eq!(out.exit_status, Some(3));
    assert!(!out.timed_out);
    env.server.stats.with(|o| {
        // 不请求 PTY，也不发 LANG 之外的环境变量（Profile 没填 locale 时连 LANG 也不发）。
        assert!(o.ptys.is_empty());
        assert!(o.envs.is_empty(), "{:?}", o.envs);
    });
}

#[test]
fn exec_strips_ansi_sequences() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let (h, _) = env.connected(ConnectMode::NonInteractive);
    let out = env.exec(
        h,
        "echo \\e[31mred\\e[0m \\e]0;title\\x07ok",
        Duration::from_secs(10),
    );
    // 服务器脚本里 `\x07` 不会被翻译，这里只验证 CSI 被剥离；OSC 在单元测试里覆盖。
    assert!(out.stdout.text.starts_with("red "), "{:?}", out.stdout.text);
    assert!(!out.stdout.text.contains('\u{1b}'));
}

#[test]
fn exec_timeout_returns_partial_output_and_closes_channel() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let (h, _) = env.connected(ConnectMode::NonInteractive);
    let started = Instant::now();
    let out = env.exec(h, "echo partial; hang", Duration::from_millis(600));
    assert!(out.timed_out);
    assert_eq!(out.exit_status, None);
    assert_eq!(out.stdout.text, "partial\n");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn exec_runs_at_most_four_per_connection_and_queues_the_rest() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let (h, _) = env.connected(ConnectMode::NonInteractive);
    let started = Instant::now();
    let outs = env.rt.block_on(async {
        let calls = (0..5).map(|_| {
            env.core
                .mcp_exec(h, "sleep 0.6; echo done", limits(Duration::from_secs(20)))
        });
        futures_join(calls).await
    });
    let elapsed = started.elapsed();
    for o in outs {
        assert_eq!(o.unwrap().stdout.text, "done\n");
    }
    // 第 5 个要等前面释放名额：至少两轮。
    assert!(elapsed >= Duration::from_millis(1100), "{elapsed:?}");
    env.server
        .stats
        .with(|o| assert!(o.exec_peak <= 4, "{}", o.exec_peak));
}

#[test]
fn queueing_time_counts_against_the_timeout() {
    let env = Env::new(password_server());
    env.save_password("pw");
    env.trust_server();
    let (h, _) = env.connected(ConnectMode::NonInteractive);
    let outs = env.rt.block_on(async {
        // 4 个占满名额 1.5 秒；第 5 个超时 0.5 秒，应在排队中超时。
        let busy = (0..4).map(|_| {
            env.core
                .mcp_exec(h, "sleep 1.5", limits(Duration::from_secs(20)))
        });
        let queued = env
            .core
            .mcp_exec(h, "echo late", limits(Duration::from_millis(500)));
        // busy 必须排在前面：join 按顺序首次轮询，先占满 4 个名额。
        let (_, queued) = tokio::join!(futures_join(busy), queued);
        queued
    });
    let o = outs.unwrap();
    assert!(o.timed_out);
    assert_eq!(o.stdout.text, "");
}

#[test]
fn exec_on_local_session_is_not_supported() {
    let env = Env::new(password_server());
    let local = env
        .core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap();
    let r = env.rt.block_on(
        env.core
            .mcp_exec(local, "echo hi", limits(Duration::from_secs(1))),
    );
    assert!(matches!(r, Err(CoreError::NotSupported)), "{r:?}");
    env.core.close_session(local);
}

/// 并发等待一组 future（测试里不想额外引入 futures 依赖）。
async fn futures_join<F: std::future::Future>(futs: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut set = Vec::new();
    for f in futs {
        set.push(Box::pin(f));
    }
    let mut results: Vec<Option<F::Output>> = set.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, f) in set.iter_mut().enumerate() {
            if results[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => results[i] = Some(v),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    results.into_iter().map(|r| r.unwrap()).collect()
}
