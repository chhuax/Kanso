//! 算法协商与旧算法记录（ssh.md §2、§12）。

use std::borrow::Cow;

use kanso_types::message::AlgorithmCategory;
use russh::keys::{Algorithm, HashAlg};
use russh::{Preferred, cipher, kex, mac};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::server::{Policy, ServerConfig, TestServer, ed25519_key, rsa_key};
use super::support::{Scripted, connect_ok, params, params_ep};
use crate::algorithms::{self, preferred};
use crate::connection::connect_with_preferred;
use crate::{AuthMethod, SshError};

/// 服务器侧偏好：给定的 kex 后面补上扩展标记，其余字段沿用 russh 默认。
fn server_pref(kex_list: &[kex::Name]) -> Preferred {
    let mut k = kex_list.to_vec();
    k.push(kex::EXTENSION_SUPPORT_AS_SERVER);
    k.push(kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER);
    Preferred {
        kex: Cow::Owned(k),
        ..Preferred::default()
    }
}

fn open_policy() -> Policy {
    Policy {
        none_ok: true,
        ..Policy::default()
    }
}

#[tokio::test]
async fn legacy_only_server_connects_and_records_legacy_algorithms() {
    let server = TestServer::start(ServerConfig {
        preferred: Preferred {
            key: Cow::Owned(vec![Algorithm::Rsa { hash: None }]),
            cipher: Cow::Owned(vec![cipher::AES_128_CBC]),
            mac: Cow::Owned(vec![mac::HMAC_SHA1]),
            ..server_pref(&[kex::DH_G14_SHA1])
        },
        keys: vec![rsa_key()],
        policy: open_policy(),
        ..ServerConfig::default()
    })
    .await;
    let c = connect_ok(&server, AuthMethod::Password, &Scripted::new()).await;
    let conn = c.connection;
    let n = conn.negotiated();
    assert_eq!(n.kex, "diffie-hellman-group14-sha1");
    assert_eq!(n.host_key, "ssh-rsa");
    assert_eq!(n.cipher, "aes128-cbc");
    assert_eq!(n.client_mac, "hmac-sha1");
    assert_eq!(n.server_mac, "hmac-sha1");
    assert_eq!(
        conn.legacy_algorithms(),
        [
            "diffie-hellman-group14-sha1",
            "ssh-rsa",
            "aes128-cbc",
            "hmac-sha1"
        ]
    );
    let hop = conn.legacy_hop().unwrap();
    assert_eq!(hop.endpoint, server.endpoint());
    assert_eq!(hop.algorithms.len(), 4);
}

#[tokio::test]
async fn nist_curve_preferred_over_sha1_when_both_supported() {
    // 服务器把 SHA-1 的 group14 排在前面：最终选择由客户端的顺序决定。
    let server = TestServer::start(ServerConfig {
        preferred: server_pref(&[kex::DH_G14_SHA1, kex::DH_GEX_SHA1, kex::ECDH_SHA2_NISTP256]),
        policy: open_policy(),
        ..ServerConfig::default()
    })
    .await;
    let c = connect_ok(&server, AuthMethod::Password, &Scripted::new()).await;
    assert_eq!(c.connection.negotiated().kex, "ecdh-sha2-nistp256");
    assert!(c.connection.legacy_algorithms().is_empty());
    assert!(c.connection.legacy_hop().is_none());
}

#[tokio::test]
async fn gex_with_2048_bit_group_connects() {
    let server = TestServer::start(ServerConfig {
        preferred: server_pref(&[kex::DH_GEX_SHA256]),
        gex_group_2048: true,
        policy: open_policy(),
        ..ServerConfig::default()
    })
    .await;
    let c = connect_ok(&server, AuthMethod::Password, &Scripted::new()).await;
    assert_eq!(
        c.connection.negotiated().kex,
        "diffie-hellman-group-exchange-sha256"
    );
}

#[tokio::test]
async fn modern_server_never_falls_to_legacy_even_if_it_lists_legacy_first() {
    // 服务器把我们的表整个倒过来（旧算法在最前）：客户端顺序仍然保证选到现代算法。
    let ours = preferred();
    fn rev<T: Clone>(v: &[T]) -> Vec<T> {
        v.iter().rev().cloned().collect()
    }
    let server = TestServer::start(ServerConfig {
        preferred: Preferred {
            kex: Cow::Owned(rev(&ours.kex)),
            key: Cow::Owned(rev(&ours.key)),
            cipher: Cow::Owned(rev(&ours.cipher)),
            mac: Cow::Owned(rev(&ours.mac)),
            ..Preferred::default()
        },
        keys: vec![ed25519_key(1), rsa_key()],
        policy: open_policy(),
        ..ServerConfig::default()
    })
    .await;
    let c = connect_ok(&server, AuthMethod::Password, &Scripted::new()).await;
    let conn = c.connection;
    assert!(
        conn.legacy_algorithms().is_empty(),
        "{:?}",
        conn.negotiated()
    );
    assert_eq!(conn.negotiated().kex, "mlkem768x25519-sha256");
    assert_eq!(conn.negotiated().host_key, "ssh-ed25519");
    assert_eq!(conn.negotiated().cipher, "chacha20-poly1305@openssh.com");
}

#[tokio::test]
async fn aead_cipher_does_not_report_sha1_mac_as_legacy() {
    let server = TestServer::start(ServerConfig {
        preferred: Preferred {
            cipher: Cow::Owned(vec![cipher::AES_256_GCM]),
            mac: Cow::Owned(vec![mac::HMAC_SHA1]),
            ..Preferred::default()
        },
        policy: open_policy(),
        ..ServerConfig::default()
    })
    .await;
    let c = connect_ok(&server, AuthMethod::Password, &Scripted::new()).await;
    let conn = c.connection;
    assert_eq!(conn.negotiated().cipher, "aes256-gcm@openssh.com");
    // russh 仍会把共同的 MAC 协商出来，但 AEAD 下它没有实际使用。
    assert!(
        conn.legacy_algorithms().is_empty(),
        "{:?}",
        conn.legacy_algorithms()
    );
}

/// 用受限的客户端偏好制造协商失败，并返回错误。
async fn negotiation_error(server_cfg: ServerConfig, client: Preferred) -> SshError {
    let server = TestServer::start(server_cfg).await;
    let stream = tokio::net::TcpStream::connect(server.addr()).await.unwrap();
    match connect_with_preferred(
        stream,
        params(&server, AuthMethod::Password),
        &Scripted::new(),
        client,
    )
    .await
    {
        Ok(_) => panic!("expected negotiation to fail"),
        Err(e) => e,
    }
}

fn client_pref(kex_list: &[kex::Name]) -> Preferred {
    let mut k = kex_list.to_vec();
    k.push(kex::EXTENSION_SUPPORT_AS_CLIENT);
    k.push(kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT);
    Preferred {
        kex: Cow::Owned(k),
        ..Preferred::default()
    }
}

#[tokio::test]
async fn kex_failure_lists_only_server_offers_without_pseudo_algorithms() {
    let e = negotiation_error(
        ServerConfig {
            preferred: server_pref(&[kex::DH_G14_SHA1]),
            ..ServerConfig::default()
        },
        client_pref(&[kex::CURVE25519]),
    )
    .await;
    match e {
        SshError::NegotiationFailed {
            category,
            server_offers,
            ..
        } => {
            assert_eq!(category, AlgorithmCategory::Kex);
            // 服务器的 ext-info-s / kex-strict-s-v00 是伪算法，不能出现在列表里。
            assert_eq!(server_offers, ["diffie-hellman-group14-sha1"]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn cipher_failure_names_cipher_category() {
    let e = negotiation_error(
        ServerConfig {
            preferred: Preferred {
                cipher: Cow::Owned(vec![cipher::AES_128_CTR]),
                ..Preferred::default()
            },
            ..ServerConfig::default()
        },
        Preferred {
            cipher: Cow::Owned(vec![cipher::AES_256_GCM]),
            ..preferred()
        },
    )
    .await;
    match e {
        SshError::NegotiationFailed {
            category,
            server_offers,
            ..
        } => {
            assert_eq!(category, AlgorithmCategory::Cipher);
            assert_eq!(server_offers, ["aes128-ctr"]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn mac_failure_names_mac_category() {
    let e = negotiation_error(
        ServerConfig {
            preferred: Preferred {
                cipher: Cow::Owned(vec![cipher::AES_128_CTR]),
                mac: Cow::Owned(vec![mac::HMAC_SHA1]),
                ..Preferred::default()
            },
            ..ServerConfig::default()
        },
        Preferred {
            cipher: Cow::Owned(vec![cipher::AES_128_CTR]),
            mac: Cow::Owned(vec![mac::HMAC_SHA256]),
            ..preferred()
        },
    )
    .await;
    match e {
        SshError::NegotiationFailed {
            category,
            server_offers,
            ..
        } => {
            assert_eq!(category, AlgorithmCategory::Mac);
            assert_eq!(server_offers, ["hmac-sha1"]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn host_key_failure_names_host_key_category() {
    let e = negotiation_error(
        ServerConfig::default(),
        Preferred {
            key: Cow::Owned(vec![Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            }]),
            ..preferred()
        },
    )
    .await;
    match e {
        SshError::NegotiationFailed {
            category,
            server_offers,
            ..
        } => {
            assert_eq!(category, AlgorithmCategory::HostKey);
            assert_eq!(server_offers, ["ssh-ed25519"]);
        }
        other => panic!("{other:?}"),
    }
    // 消息映射使用已有的 key。
}

#[tokio::test]
async fn negotiation_failure_maps_to_existing_message() {
    let e = negotiation_error(
        ServerConfig {
            preferred: server_pref(&[kex::DH_G14_SHA1]),
            ..ServerConfig::default()
        },
        client_pref(&[kex::CURVE25519]),
    )
    .await;
    assert_eq!(e.to_message().key(), "ssh-negotiation-failed");
}

/// 解析明文 KEXINIT 里的 10 个 name-list（RFC 4253 §7.1）。
fn parse_kexinit_lists(payload: &[u8]) -> Vec<Vec<String>> {
    assert_eq!(payload[0], 20, "不是 KEXINIT");
    let mut pos = 1 + 16; // 消息号 + cookie
    let mut lists = Vec::new();
    for _ in 0..10 {
        let len = u32::from_be_bytes(payload[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let s = std::str::from_utf8(&payload[pos..pos + len]).unwrap();
        lists.push(
            s.split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect(),
        );
        pos += len;
    }
    lists
}

/// 用一个只读的假服务器截取客户端实际发出的 KEXINIT。
async fn capture_client_kexinit() -> Vec<Vec<String>> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        sock.write_all(b"SSH-2.0-capture\r\n").await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).await.unwrap();
            assert!(n > 0, "客户端在发出 KEXINIT 前就断开了");
            buf.extend_from_slice(&tmp[..n]);
            // 身份串之后是 4 字节包长 + 1 字节填充长度 + payload。
            let Some(id_end) = buf.windows(2).position(|w| w == b"\r\n") else {
                continue;
            };
            let p = id_end + 2;
            if buf.len() < p + 4 {
                continue;
            }
            let len = u32::from_be_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
            if buf.len() < p + 4 + len {
                continue;
            }
            let pad = buf[p + 4] as usize;
            return buf[p + 5..p + 4 + len - pad].to_vec();
        }
    });
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    // 连接会因对端不再回应而失败，我们只关心它发出的第一个包。
    let endpoint = kanso_types::message::Endpoint {
        host: "127.0.0.1".into(),
        port,
        via: vec![],
    };
    let connecting = tokio::spawn(async move {
        let _ = connect_with_preferred(
            stream,
            params_ep(endpoint, AuthMethod::Password),
            &Scripted::new(),
            preferred(),
        )
        .await;
    });
    let payload = server.await.unwrap();
    connecting.abort();
    parse_kexinit_lists(&payload)
}

/// 断言最终发出的列表：顺序与我们的表一致，且带上 ext-info-c 与 strict-kex-c 标记（ssh.md §2.1）。
#[tokio::test]
async fn emitted_kexinit_matches_our_table_and_carries_markers() {
    let lists = capture_client_kexinit().await;
    let ours = preferred();

    let kex_sent = &lists[0];
    // russh 只发出本角色（客户端）的扩展标记，服务器角色的两个不会出现。
    let expect_kex: Vec<String> = ours
        .kex
        .iter()
        .map(|n| n.as_ref().to_owned())
        .filter(|n| n != "ext-info-s" && n != "kex-strict-s-v00@openssh.com")
        .collect();
    assert_eq!(kex_sent, &expect_kex);
    assert_eq!(
        &kex_sent[kex_sent.len() - 2..],
        ["ext-info-c", "kex-strict-c-v00@openssh.com"]
    );
    // 现代在前、旧在后。
    let pos = |s: &str| kex_sent.iter().position(|n| n == s).unwrap();
    assert!(pos("ecdh-sha2-nistp256") < pos("diffie-hellman-group14-sha1"));
    assert!(pos("diffie-hellman-group14-sha1") < pos("diffie-hellman-group-exchange-sha1"));

    let names = |v: &[cipher::Name]| v.iter().map(|n| n.as_ref().to_owned()).collect::<Vec<_>>();
    assert_eq!(
        lists[1],
        ours.key.iter().map(|a| a.to_string()).collect::<Vec<_>>()
    );
    assert_eq!(lists[2], names(&ours.cipher));
    assert_eq!(lists[3], names(&ours.cipher));
    let macs: Vec<String> = ours.mac.iter().map(|n| n.as_ref().to_owned()).collect();
    assert_eq!(lists[4], macs);
    assert_eq!(lists[5], macs);
    // 不支持的算法不会被发出。
    for l in &lists {
        for bad in ["3des-cbc", "hmac-md5", "ssh-dss"] {
            assert!(!l.iter().any(|n| n == bad));
        }
    }
    // 旧算法全部在列表里，且排在所有现代算法之后。
    assert!(algorithms::is_legacy(
        AlgorithmCategory::Cipher,
        lists[2].last().unwrap()
    ));
}
