//! 跳板链（ssh.md §5、§12）：2 跳 / 3 跳、每跳独立认证与主机密钥策略、不可达、密钥变更、
//! 断开顺序、复制标签共享整条链、旧算法汇总。

use std::sync::Arc;
use std::time::Duration;

use kanso_types::message::Reason;
use kanso_types::secret::SecretString;
use russh::MethodKind::{KeyboardInteractive, Password, PublicKey};
use russh::keys::ssh_key::LineEnding;

use super::server::{
    KbdRoundSpec, Policy, ServerConfig, TestServer, ed25519_key, legacy_only_config, methods,
    public_key_blob,
};
use super::support::{Scripted, eventually, params_ep};
use crate::connection::DISCONNECT_LOG;
use crate::{
    AuthMethod, ConnectParams, HopParams, MemoryHostKeyStore, PtyRequest, SshError,
    accept_changed_key, connect_chain,
};

fn password_server() -> TestServer_ {
    TestServer_(ServerConfig {
        policy: Policy {
            methods: methods(&[Password]),
            password: Some("pw-a".into()),
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
}

/// 仅为了把配置延迟到 `.start()` 才建服务器的小包装。
struct TestServer_(ServerConfig);
impl TestServer_ {
    async fn start(self) -> TestServer {
        TestServer::start(self.0).await
    }
}

fn kbd_server() -> ServerConfig {
    ServerConfig {
        policy: Policy {
            methods: methods(&[KeyboardInteractive]),
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

fn hop(
    name: &str,
    server: &TestServer,
    auth: AuthMethod,
    store: &Arc<MemoryHostKeyStore>,
) -> HopParams {
    let mut p: ConnectParams = params_ep(server.endpoint(), auth);
    p.host_keys = store.clone();
    HopParams {
        name: name.into(),
        connect: p,
        proxy_command: None,
    }
}

fn password_hop(
    name: &str,
    server: &TestServer,
    pw: &str,
    store: &Arc<MemoryHostKeyStore>,
) -> HopParams {
    let mut h = hop(name, server, AuthMethod::Password, store);
    h.connect.saved_password = Some(SecretString::new(pw));
    h
}

async fn read_until(sh: &mut crate::ShellChannel, want: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    while !buf.windows(want.len()).any(|w| w == want) {
        match tokio::time::timeout(Duration::from_secs(5), sh.read()).await {
            Ok(Some(b)) => buf.extend_from_slice(&b),
            _ => break,
        }
    }
    buf
}

#[tokio::test]
async fn three_hops_with_different_auth_per_hop_and_via_in_prompts() {
    let a = password_server().start().await;
    let key = ed25519_key(60);
    let b = TestServer::start(ServerConfig {
        policy: Policy {
            methods: methods(&[PublicKey]),
            pubkeys: vec![key.public_key().clone()],
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let c = TestServer::start(kbd_server()).await;
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("id_b");
    std::fs::write(
        &key_path,
        key.to_openssh(LineEnding::LF).unwrap().as_bytes(),
    )
    .unwrap();
    let store = Arc::new(MemoryHostKeyStore::new());

    let interaction = Scripted::new().default_answers(&["123456"]);
    let got = connect_chain(
        vec![
            password_hop("hop-a", &a, "pw-a", &store),
            hop(
                "hop-b",
                &b,
                AuthMethod::PublicKey {
                    path: key_path.to_str().unwrap().to_owned(),
                },
                &store,
            ),
            hop("target", &c, AuthMethod::Password, &store),
        ],
        &interaction,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    // 每一跳认证方式不同：A 密码、B 私钥、C keyboard-interactive。
    a.stats.with(|o| {
        assert!(
            o.auth_log.iter().any(|l| l == "password:pw-a"),
            "{:?}",
            o.auth_log
        )
    });
    b.stats.with(|o| {
        assert!(
            o.auth_log.iter().any(|l| l.starts_with("publickey:")),
            "{:?}",
            o.auth_log
        )
    });
    c.stats.with(|o| {
        assert!(
            o.auth_log.iter().any(|l| l == "kbd-start"),
            "{:?}",
            o.auth_log
        )
    });

    // 提问来自目标，endpoint 带完整的 via（从外到内）。
    {
        let rounds = interaction.kbd_rounds.lock().unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].endpoint.port, c.port);
        assert_eq!(rounds[0].endpoint.via, ["hop-a", "hop-b"]);
    }

    // 转发请求逐跳指向下一跳。
    a.stats
        .with(|o| assert_eq!(o.forwards, [("127.0.0.1".to_owned(), u32::from(b.port))]));
    b.stats
        .with(|o| assert_eq!(o.forwards, [("127.0.0.1".to_owned(), u32::from(c.port))]));
    c.stats.with(|o| assert!(o.forwards.is_empty()));

    assert_eq!(got.remember.len(), 3);
    assert_eq!(got.connection.endpoint().via, ["hop-a", "hop-b"]);
    // 目标上能正常开 shell。
    let mut sh = got
        .connection
        .open_shell(PtyRequest::default())
        .await
        .unwrap();
    assert!(!read_until(&mut sh, b"WELCOME").await.is_empty());
}

#[tokio::test]
async fn two_hops_each_use_their_own_host_key_record() {
    let a = TestServer::start(open_server()).await;
    let b = TestServer::start(ServerConfig {
        keys: vec![ed25519_key(2)],
        ..open_server()
    })
    .await;
    let store = Arc::new(MemoryHostKeyStore::new());
    connect_chain(
        vec![
            hop("a", &a, AuthMethod::Password, &store),
            hop("b", &b, AuthMethod::Password, &store),
        ],
        &Scripted::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    // 两跳各自按 host:port 记录自己的密钥（同为 127.0.0.1，端口区分）。
    use crate::HostKeyStore as _;
    assert_eq!(
        store.lookup("127.0.0.1", a.port).unwrap()[0].key,
        public_key_blob(&ed25519_key(1))
    );
    assert_eq!(
        store.lookup("127.0.0.1", b.port).unwrap()[0].key,
        public_key_blob(&ed25519_key(2))
    );
}

fn unused_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn unreachable_next_hop_reports_jump_unreachable() {
    let a = TestServer::start(open_server()).await;
    let store = Arc::new(MemoryHostKeyStore::new());
    let mut dead = hop("dead", &a, AuthMethod::Password, &store);
    dead.connect.endpoint.port = unused_port();
    let err = match connect_chain(
        vec![hop("a", &a, AuthMethod::Password, &store), dead],
        &Scripted::new(),
    )
    .await
    {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    let SshError::JumpUnreachable {
        jump,
        target,
        reason,
    } = &err
    else {
        panic!("{err:?}");
    };
    assert_eq!(jump, "a");
    assert_eq!(target.via, ["a"]);
    assert_eq!(
        reason,
        &Reason::Other {
            detail: "connect failed".into()
        }
    );
    assert_eq!(err.to_message().key(), "jump-unreachable");
}

#[tokio::test]
async fn forwarding_prohibited_reports_permission_denied() {
    let a = TestServer::start(ServerConfig {
        forwarding: false,
        ..open_server()
    })
    .await;
    let b = TestServer::start(open_server()).await;
    let store = Arc::new(MemoryHostKeyStore::new());
    let err = match connect_chain(
        vec![
            hop("a", &a, AuthMethod::Password, &store),
            hop("b", &b, AuthMethod::Password, &store),
        ],
        &Scripted::new(),
    )
    .await
    {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    assert!(
        matches!(
            &err,
            SshError::JumpUnreachable {
                reason: Reason::PermissionDenied,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn host_key_change_on_second_hop_stops_there_and_accepting_reconnects_whole_chain() {
    let a = TestServer::start(open_server()).await;
    let b = TestServer::start(ServerConfig {
        keys: vec![ed25519_key(2)],
        ..open_server()
    })
    .await;
    let c = TestServer::start(open_server()).await;
    let store = Arc::new(MemoryHostKeyStore::new());
    {
        use crate::HostKeyStore as _;
        store
            .add(
                "127.0.0.1",
                b.port,
                "ssh-ed25519",
                &public_key_blob(&ed25519_key(9)),
            )
            .unwrap();
    }
    let hops = |store: &Arc<MemoryHostKeyStore>| {
        vec![
            hop("a", &a, AuthMethod::Password, store),
            hop("b", &b, AuthMethod::Password, store),
            hop("c", &c, AuthMethod::Password, store),
        ]
    };

    let err = match connect_chain(hops(&store), &Scripted::new()).await {
        Ok(_) => panic!("expected host key change"),
        Err(e) => e,
    };
    let SshError::HostKeyChanged { endpoint, details } = err else {
        panic!("{err:?}");
    };
    // 停在第 2 跳：错误指明是 b（含 via = [a]），目标 c 完全没被碰过。
    assert_eq!(endpoint.port, b.port);
    assert_eq!(endpoint.via, ["a"]);
    assert_eq!(c.stats.with(|o| o.accepted), 0);
    assert_eq!(b.stats.with(|o| o.shells), 0);

    accept_changed_key(
        store.clone(),
        &endpoint.host,
        endpoint.port,
        &details.key_type,
        &details.key,
    )
    .await
    .unwrap();
    // 整条链重连：第 1 跳被重新连接（第一次失败时已随错误释放）。
    let got = connect_chain(hops(&store), &Scripted::new())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(a.stats.with(|o| o.accepted), 2);
    assert_eq!(got.connection.endpoint().port, c.port);
    // 第一次尝试留下的那条 a 连接已断开，不残留半条链。
    eventually(|| a.stats.with(|o| o.closed) == 1).await;
}

#[tokio::test]
async fn disconnect_goes_innermost_first_and_only_after_last_user_releases() {
    let a = TestServer::start(open_server()).await;
    let b = TestServer::start(open_server()).await;
    let c = TestServer::start(open_server()).await;
    let store = Arc::new(MemoryHostKeyStore::new());
    let got = connect_chain(
        vec![
            hop("a", &a, AuthMethod::Password, &store),
            hop("b", &b, AuthMethod::Password, &store),
            hop("c", &c, AuthMethod::Password, &store),
        ],
        &Scripted::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    // 复制标签：同一条目标连接上再开 shell，整条链各只认证一次。
    let conn2 = got.connection.clone();
    let sh1 = got
        .connection
        .open_shell(PtyRequest::default())
        .await
        .unwrap();
    let sh2 = conn2.open_shell(PtyRequest::default()).await.unwrap();
    for s in [&a, &b, &c] {
        assert_eq!(s.stats.with(|o| o.accepted), 1);
        assert_eq!(
            s.stats
                .with(|o| o.auth_log.iter().filter(|l| *l == "none").count()),
            1
        );
    }
    c.stats.with(|o| assert_eq!(o.shells, 2));

    // 还有使用者时整条链保持连接。
    drop(got);
    drop(sh1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    for s in [&a, &b, &c] {
        assert_eq!(s.stats.with(|o| o.closed), 0);
    }

    // 最后一个使用者释放：每个服务器恰好断开一次，且 disconnect 的发出顺序是内层先、外层后。
    drop(sh2);
    drop(conn2);
    for s in [&a, &b, &c] {
        eventually(|| s.stats.with(|o| o.closed) == 1).await;
    }
    let log = DISCONNECT_LOG.lock().unwrap().clone();
    let pos = |port: u16| {
        log.iter()
            .position(|p| *p == port)
            .unwrap_or_else(|| panic!("{log:?}"))
    };
    assert!(
        pos(c.port) < pos(b.port) && pos(b.port) < pos(a.port),
        "{log:?}"
    );
}

#[tokio::test]
async fn legacy_algorithms_are_summarized_per_hop_across_the_chain() {
    let a = TestServer::start(legacy_only_config()).await;
    let b = TestServer::start(open_server()).await;
    let c = TestServer::start(legacy_only_config()).await;
    let store = Arc::new(MemoryHostKeyStore::new());
    let got = connect_chain(
        vec![
            hop("a", &a, AuthMethod::Password, &store),
            hop("b", &b, AuthMethod::Password, &store),
            hop("c", &c, AuthMethod::Password, &store),
        ],
        &Scripted::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let hops = got.connection.legacy_hops();
    assert_eq!(hops.len(), 2, "{hops:?}");
    assert_eq!(hops[0].endpoint.port, a.port);
    assert!(hops[0].endpoint.via.is_empty());
    assert_eq!(hops[1].endpoint.port, c.port);
    assert_eq!(hops[1].endpoint.via, ["a", "b"]);
    assert!(hops.iter().all(|h| !h.algorithms.is_empty()));
}

#[tokio::test]
async fn proxy_command_on_inner_hop_and_empty_chain_are_rejected() {
    let a = TestServer::start(open_server()).await;
    let store = Arc::new(MemoryHostKeyStore::new());
    let mut inner = hop("b", &a, AuthMethod::Password, &store);
    inner.proxy_command = Some("nc %h %p".into());
    let err = match connect_chain(
        vec![hop("a", &a, AuthMethod::Password, &store), inner],
        &Scripted::new(),
    )
    .await
    {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    assert!(matches!(err, SshError::ConnectFailed { .. }), "{err:?}");
    assert!(matches!(
        connect_chain(vec![], &Scripted::new()).await.err(),
        Some(SshError::ConnectFailed { .. })
    ));
}
