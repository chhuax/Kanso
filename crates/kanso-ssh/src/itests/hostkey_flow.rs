//! 主机密钥策略（ssh.md §3、§12）。

use std::fs;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::PublicKeyBase64;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use super::server::{Policy, ServerConfig, TestServer, ed25519_key, rsa_key};
use super::support::{Scripted, connect_err, params_with_store};
use crate::{
    AuthMethod, HostKeyChange, HostKeyStore, HostKeyStoreError, KnownHostKey, KnownHostsFile,
    SshError, accept_changed_key, connect, fingerprint_sha256,
};

fn open_server(keys: Vec<russh::keys::PrivateKey>) -> ServerConfig {
    ServerConfig {
        keys,
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        ..ServerConfig::default()
    }
}

fn blob(k: &russh::keys::PrivateKey) -> Vec<u8> {
    k.public_key().public_key_bytes()
}

fn known_hosts(dir: &tempfile::TempDir) -> Arc<KnownHostsFile> {
    Arc::new(KnownHostsFile::new(dir.path().join("known_hosts")))
}

async fn connect_with(
    server: &TestServer,
    store: Arc<dyn HostKeyStore>,
) -> Result<crate::Connected, SshError> {
    connect(
        params_with_store(server, AuthMethod::Password, store),
        &Scripted::new(),
    )
    .await
}

#[tokio::test]
async fn unknown_host_is_recorded_then_recognized() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);

    connect_with(&server, kh.clone()).await.unwrap();
    let text = fs::read_to_string(kh.path()).unwrap();
    // 非 22 端口记录成 [host]:port（ssh.md §3）。
    assert!(
        text.starts_with(&format!("[127.0.0.1]:{} ssh-ed25519 ", server.port)),
        "{text}"
    );
    assert_eq!(text.lines().count(), 1);

    // 再次连接：识别为已知，不重复写入。
    connect_with(&server, kh.clone()).await.unwrap();
    assert_eq!(fs::read_to_string(kh.path()).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn changed_key_is_rejected_before_any_auth_or_channel() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    // 注释行与空行也计行号：旧记录在第 4 行。
    let old = ed25519_key(9);
    fs::write(
        kh.path(),
        format!(
            "# my hosts\n\nother ssh-ed25519 {}\n[127.0.0.1]:{} ssh-ed25519 {}\n",
            STANDARD.encode(blob(&ed25519_key(7))),
            server.port,
            STANDARD.encode(blob(&old)),
        ),
    )
    .unwrap();

    let err = connect_with(&server, kh.clone()).await.err().unwrap();
    let SshError::HostKeyChanged { endpoint, details } = err.clone() else {
        panic!("{err:?}");
    };
    let HostKeyChange {
        key_type,
        key,
        fingerprint,
        known_hosts,
    } = *details;
    assert_eq!(endpoint, server.endpoint());
    assert_eq!(key_type, "ssh-ed25519");
    assert_eq!(key, blob(&ed25519_key(1)));
    // 指纹格式与 `ssh-keygen -lf` 一致：SHA256: + 无填充 base64。
    let expect = format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&key)));
    assert_eq!(fingerprint, expect);
    assert_eq!(fingerprint_sha256(ed25519_key(1).public_key()), expect);
    let loc = known_hosts.unwrap();
    assert_eq!(loc.line, 4);
    assert_eq!(loc.path, kh.path().display().to_string());
    assert_eq!(err.to_message().key(), "hostkey-changed");

    // 拒绝发生在密钥交换之后、认证之前：没有 none 探测，更没有通道。
    server.stats.with(|o| {
        assert!(o.auth_log.is_empty(), "{:?}", o.auth_log);
        assert_eq!(o.shells, 0);
    });
    // 旧记录没有被改动。
    assert!(
        fs::read_to_string(kh.path())
            .unwrap()
            .contains(&STANDARD.encode(blob(&old)))
    );
}

#[tokio::test]
async fn new_algorithm_for_known_host_is_recorded() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    // 该主机只记录过 RSA 密钥：出现 ed25519 当作新算法，记录并继续。
    kh.add("127.0.0.1", server.port, "ssh-rsa", &blob(&rsa_key()))
        .unwrap();
    connect_with(&server, kh.clone()).await.unwrap();
    let got = kh.lookup("127.0.0.1", server.port).unwrap();
    let types: Vec<_> = got.iter().map(|k| k.key_type.as_str()).collect();
    assert_eq!(types, ["ssh-rsa", "ssh-ed25519"]);
}

#[tokio::test]
async fn accepting_changed_key_replaces_all_old_records_and_reconnect_succeeds() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    kh.add("other", 22, "ssh-ed25519", b"keep-me").unwrap();
    kh.add(
        "127.0.0.1",
        server.port,
        "ssh-ed25519",
        &blob(&ed25519_key(9)),
    )
    .unwrap();
    kh.add("127.0.0.1", server.port, "ssh-rsa", &blob(&rsa_key()))
        .unwrap();

    let SshError::HostKeyChanged { details, .. } =
        connect_with(&server, kh.clone()).await.err().unwrap()
    else {
        panic!("expected HostKeyChanged");
    };
    let (key_type, key) = (details.key_type, details.key);
    // 用户接受后：所有旧记录（含其他算法的）被替换。
    accept_changed_key(kh.clone(), "127.0.0.1", server.port, &key_type, &key)
        .await
        .unwrap();
    let got = kh.lookup("127.0.0.1", server.port).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key, blob(&ed25519_key(1)));
    assert_eq!(kh.lookup("other", 22).unwrap().len(), 1);

    connect_with(&server, kh.clone()).await.unwrap();
}

#[tokio::test]
async fn hashed_hostname_record_is_recognized_without_adding() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    let token = format!("[127.0.0.1]:{}", server.port);
    let salt = b"0123456789abcdef0123";
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(salt).unwrap();
    mac.update(token.as_bytes());
    let hashed = format!(
        "|1|{}|{}",
        STANDARD.encode(salt),
        STANDARD.encode(mac.finalize().into_bytes())
    );
    fs::write(
        kh.path(),
        format!(
            "{hashed} ssh-ed25519 {}\n",
            STANDARD.encode(blob(&ed25519_key(1)))
        ),
    )
    .unwrap();
    connect_with(&server, kh.clone()).await.unwrap();
    assert_eq!(fs::read_to_string(kh.path()).unwrap().lines().count(), 1);
}

#[tokio::test]
async fn revoked_key_is_refused() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    fs::write(
        kh.path(),
        format!(
            "@revoked [127.0.0.1]:{} ssh-ed25519 {}\n",
            server.port,
            STANDARD.encode(blob(&ed25519_key(1)))
        ),
    )
    .unwrap();
    let mut p = params_with_store(&server, AuthMethod::Password, kh.clone());
    p.revoked_keys = Some(kh.clone());
    let err = connect_err(p, &Scripted::new()).await;
    assert!(matches!(err, SshError::HostKeyRevoked { .. }), "{err:?}");
    server.stats.with(|o| assert!(o.auth_log.is_empty()));
    // 被吊销的密钥不会被写进普通记录。
    assert!(kh.lookup("127.0.0.1", server.port).unwrap().is_empty());
}

/// 记录调用线程、并可注入失败的存储。
#[derive(Default)]
struct ProbeStore {
    fail_lookup: bool,
    fail_add: bool,
    threads: Mutex<Vec<std::thread::ThreadId>>,
}

impl HostKeyStore for ProbeStore {
    fn lookup(&self, _h: &str, _p: u16) -> Result<Vec<KnownHostKey>, HostKeyStoreError> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        if self.fail_lookup {
            return Err(HostKeyStoreError::Other {
                detail: "boom".into(),
            });
        }
        Ok(vec![])
    }
    fn add(&self, _h: &str, _p: u16, _t: &str, _k: &[u8]) -> Result<(), HostKeyStoreError> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        if self.fail_add {
            return Err(HostKeyStoreError::Io {
                path: "/x/known_hosts".into(),
                detail: "read-only".into(),
            });
        }
        Ok(())
    }
    fn replace_all(&self, _h: &str, _p: u16, _t: &str, _k: &[u8]) -> Result<(), HostKeyStoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn store_calls_run_on_blocking_threads() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let store = Arc::new(ProbeStore::default());
    connect_with(&server, store.clone()).await.unwrap();
    let here = std::thread::current().id();
    let threads = store.threads.lock().unwrap().clone();
    assert_eq!(threads.len(), 2, "lookup + add");
    // 默认的 current_thread 测试运行时里 async 代码就跑在本线程；store 必须不在它上面。
    assert!(threads.iter().all(|t| *t != here));
}

#[tokio::test]
async fn lookup_failure_fails_the_connection() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let store = Arc::new(ProbeStore {
        fail_lookup: true,
        ..Default::default()
    });
    let err = connect_with(&server, store).await.err().unwrap();
    assert!(
        matches!(err, SshError::HostKeyStore { write: false, .. }),
        "{err:?}"
    );
    server.stats.with(|o| assert!(o.auth_log.is_empty()));
}

#[tokio::test]
async fn add_failure_fails_the_connection_instead_of_silently_proceeding() {
    let server = TestServer::start(open_server(vec![ed25519_key(1)])).await;
    let store = Arc::new(ProbeStore {
        fail_add: true,
        ..Default::default()
    });
    let err = connect_with(&server, store).await.err().unwrap();
    assert!(
        matches!(err, SshError::HostKeyStore { write: true, .. }),
        "{err:?}"
    );
    assert_eq!(err.to_message().key(), "hostkey-store-write-failed");
    server.stats.with(|o| assert!(o.auth_log.is_empty()));
}

#[tokio::test]
async fn rsa_host_key_recorded_as_ssh_rsa_regardless_of_signature_hash() {
    let server = TestServer::start(open_server(vec![rsa_key()])).await;
    let dir = tempfile::tempdir().unwrap();
    let kh = known_hosts(&dir);
    let c = connect_ok_with(&server, kh.clone()).await;
    // 服务器与客户端会协商 rsa-sha2-512 签名，但记录里的密钥类型仍是 ssh-rsa。
    assert_eq!(c.negotiated().host_key, "rsa-sha2-512");
    let got = kh.lookup("127.0.0.1", server.port).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key_type, "ssh-rsa");
    // 再连一次仍然匹配（不会因为签名哈希不同被当成新算法或变更）。
    connect_with(&server, kh.clone()).await.unwrap();
    assert_eq!(kh.lookup("127.0.0.1", server.port).unwrap().len(), 1);
}

async fn connect_ok_with(server: &TestServer, kh: Arc<KnownHostsFile>) -> crate::Connection {
    connect_with(server, kh).await.unwrap().connection
}
