//! ssh-agent 认证（ssh.md §4.3）。用进程内的 agent server，监听临时目录里的 unix socket，
//! 不会碰 `SSH_AUTH_SOCK` 指向的真实 agent。

use std::borrow::Cow;
use std::path::PathBuf;

use kanso_types::message::Endpoint;
use russh::MethodKind::{KeyboardInteractive, PublicKey};
use russh::keys::agent::client::AgentClient;
use russh::keys::agent::server::serve;
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

use super::server::{
    KbdRoundSpec, Policy, ServerConfig, TestServer, ed25519_key, methods, rsa_key,
};
use super::support::{Scripted, connect_err, params};
use crate::{AuthMethod, SshError, connect};

struct TestAgent {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    task: JoinHandle<()>,
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_agent(keys: &[PrivateKey]) -> TestAgent {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let incoming = Box::pin(futures::stream::unfold(listener, |l| async move {
        let accepted = l.accept().await.map(|(s, _)| s);
        Some((accepted, l))
    }));
    let task = tokio::spawn(async move {
        let _ = serve(incoming, ()).await;
    });
    let mut client = AgentClient::connect_uds(&socket).await.unwrap();
    for k in keys {
        client.add_identity(k, &[]).await.unwrap();
    }
    TestAgent {
        _dir: dir,
        socket,
        task,
    }
}

fn agent_params(server: &TestServer, agent: &TestAgent) -> crate::ConnectParams {
    let mut p = params(server, AuthMethod::Agent);
    p.agent_socket = Some(agent.socket.clone());
    p
}

fn count(server: &TestServer, prefix: &str) -> usize {
    server
        .stats
        .with(|o| o.auth_log.iter().filter(|l| l.starts_with(prefix)).count())
}

#[tokio::test]
async fn agent_without_accepted_identity_reports_no_identity_after_trying_all() {
    let server = TestServer::start(ServerConfig {
        policy: Policy {
            methods: methods(&[PublicKey]),
            pubkeys: vec![ed25519_key(99).public_key().clone()],
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let agent = start_agent(&[ed25519_key(60), ed25519_key(61)]).await;
    let e = connect_err(agent_params(&server, &agent), &Scripted::new()).await;
    assert!(
        matches!(&e, SshError::AuthAgentNoIdentity { user, .. } if user == "tester"),
        "{e:?}"
    );
    assert_eq!(e.to_message().key(), "auth-agent-no-identity");
    // 两个身份都被依次尝试过。
    assert_eq!(count(&server, "pk-offer:"), 2);
}

#[tokio::test]
async fn agent_tries_identities_until_one_is_accepted() {
    let accepted = ed25519_key(72);
    let server = TestServer::start(ServerConfig {
        policy: Policy {
            methods: methods(&[PublicKey]),
            pubkeys: vec![accepted.public_key().clone()],
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let agent = start_agent(&[ed25519_key(70), ed25519_key(71), accepted]).await;
    connect(agent_params(&server, &agent), &Scripted::new())
        .await
        .unwrap();
    assert_eq!(count(&server, "publickey:"), 1);
}

#[tokio::test]
async fn partial_success_stops_trying_further_identities() {
    let (k1, k2) = (ed25519_key(80), ed25519_key(81));
    let server = TestServer::start(ServerConfig {
        policy: Policy {
            methods: methods(&[PublicKey, KeyboardInteractive]),
            pubkeys: vec![k1.public_key().clone(), k2.public_key().clone()],
            pubkey_partial: true,
            after_partial: methods(&[KeyboardInteractive]),
            kbd: vec![KbdRoundSpec::question("Code: ", true)],
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let agent = start_agent(&[k1, k2]).await;
    let i = Scripted::new().answers(&["1"]);
    connect(agent_params(&server, &agent), &i).await.unwrap();
    // 第一个身份被 partial 接受后就停，不再去试第二个。
    assert_eq!(count(&server, "publickey:"), 1);
    assert_eq!(i.kbd_count(), 1);
}

#[tokio::test]
async fn agent_with_rsa_identity_uses_sha2_signature() {
    let key = rsa_key();
    let server = TestServer::start(ServerConfig {
        preferred: russh::Preferred {
            key: Cow::Owned(vec![Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            }]),
            ..russh::Preferred::default()
        },
        keys: vec![key.clone()],
        policy: Policy {
            methods: methods(&[PublicKey]),
            pubkeys: vec![key.public_key().clone()],
            reject_sha1_rsa: true,
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let agent = start_agent(&[key]).await;
    connect(agent_params(&server, &agent), &Scripted::new())
        .await
        .unwrap();
    let log = server.stats.with(|o| o.auth_log.clone());
    assert!(log.iter().any(|l| l == "publickey:rsa-sha2-256"), "{log:?}");
}

#[tokio::test]
async fn unreachable_agent_reports_unavailable_with_details() {
    let server = TestServer::start(ServerConfig::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut p = params(&server, AuthMethod::Agent);
    p.agent_socket = Some(dir.path().join("no-such.sock"));
    let e = connect_err(p, &Scripted::new()).await;
    match &e {
        SshError::AuthAgentUnavailable { details, endpoint } => {
            assert!(!details.is_empty());
            assert_eq!(
                endpoint,
                &Endpoint {
                    host: "127.0.0.1".into(),
                    port: server.port,
                    via: vec![]
                }
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(e.to_message().key(), "auth-agent-unavailable");

    // 调用方没有传 socket 路径（core 没读到 SSH_AUTH_SOCK）同样是"无法连上"。
    let e = connect_err(params(&server, AuthMethod::Agent), &Scripted::new()).await;
    assert!(matches!(e, SshError::AuthAgentUnavailable { .. }), "{e:?}");
}
