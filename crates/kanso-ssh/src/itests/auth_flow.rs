//! 认证流程（ssh.md §4.1、§4.2、§12）。

use std::sync::Arc;
use std::time::Duration;

use kanso_types::profile::AuthMethod as ProfileAuthMethod;
use kanso_types::prompt::CredentialKind;
use kanso_types::secret::SecretString;
use russh::MethodKind::{HostBased, KeyboardInteractive, Password, PublicKey};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg};

use super::server::{
    ENCRYPTED_KEY_PASSPHRASE, ENCRYPTED_KEY_PEM, KbdRoundSpec, Policy, ServerConfig, TestServer,
    ed25519_key, methods, rsa_key,
};
use super::support::{KbdReply, Scripted, connect_err, params};
use crate::{AuthMethod, RememberedCredential, SshError, connect};

fn server_with(policy: Policy) -> ServerConfig {
    ServerConfig {
        policy,
        ..ServerConfig::default()
    }
}

/// 把私钥写成临时文件，返回（目录守卫，路径字符串）。
fn key_file(dir: &tempfile::TempDir, name: &str, key: &PrivateKey) -> String {
    let p = dir.path().join(name);
    std::fs::write(&p, key.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    p.to_str().unwrap().to_owned()
}

fn with_password(mut p: crate::ConnectParams, pw: &str) -> crate::ConnectParams {
    p.saved_password = Some(SecretString::new(pw));
    p
}

#[tokio::test]
async fn server_that_accepts_none_logs_in_without_asking_anything() {
    let server = TestServer::start(server_with(Policy {
        none_ok: true,
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new();
    let c = connect(params(&server, AuthMethod::Password), &i)
        .await
        .unwrap();
    assert!(c.remember.is_empty());
    assert_eq!(server.stats.with(|o| o.auth_log.clone()), ["none"]);
    assert!(i.cred_requests.lock().unwrap().is_empty());
    assert_eq!(i.kbd_count(), 0);
}

#[tokio::test]
async fn saved_password_is_sent_after_none() {
    let server = TestServer::start(server_with(Policy {
        password: Some("pw".into()),
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new();
    connect(
        with_password(params(&server, AuthMethod::Password), "pw"),
        &i,
    )
    .await
    .unwrap();
    assert_eq!(
        server.stats.with(|o| o.auth_log.clone()),
        ["none", "password:pw"]
    );
    assert!(i.cred_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_password_is_requested_and_remember_flag_returned() {
    let server = TestServer::start(server_with(Policy {
        password: Some("pw".into()),
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().cred(Some(("pw", true)));
    let c = connect(params(&server, AuthMethod::Password), &i)
        .await
        .unwrap();
    assert_eq!(*i.cred_requests.lock().unwrap(), [CredentialKind::Password]);
    assert_eq!(
        c.remember,
        [RememberedCredential {
            kind: CredentialKind::Password,
            secret: SecretString::new("pw")
        }]
    );
}

#[tokio::test]
async fn credential_not_remembered_when_user_declines() {
    let server = TestServer::start(server_with(Policy {
        password: Some("pw".into()),
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().cred(Some(("pw", false)));
    let c = connect(params(&server, AuthMethod::Password), &i)
        .await
        .unwrap();
    assert!(c.remember.is_empty());
}

#[tokio::test]
async fn cancelled_credential_prompt_reports_auth_cancelled() {
    let server = TestServer::start(server_with(Policy {
        password: Some("pw".into()),
        ..Policy::default()
    }))
    .await;
    let e = connect_err(
        params(&server, AuthMethod::Password),
        &Scripted::new().cred(None),
    )
    .await;
    assert!(matches!(e, SshError::AuthCancelled { .. }), "{e:?}");
}

#[tokio::test]
async fn wrong_password_reports_auth_failed_for_password_method() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[Password]),
        password: Some("pw".into()),
        ..Policy::default()
    }))
    .await;
    let e = connect_err(
        with_password(params(&server, AuthMethod::Password), "bad"),
        &Scripted::new(),
    )
    .await;
    match &e {
        SshError::AuthFailed {
            user,
            method,
            endpoint,
        } => {
            assert_eq!(user, "tester");
            assert_eq!(*method, ProfileAuthMethod::Password);
            assert_eq!(endpoint, &server.endpoint());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(e.to_message().key(), "auth-failed");
}

#[tokio::test]
async fn rejected_public_key_reports_key_rejected() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[PublicKey]),
        pubkeys: vec![ed25519_key(50).public_key().clone()],
        ..Policy::default()
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "id", &ed25519_key(51));
    let e = connect_err(
        params(&server, AuthMethod::PublicKey { path: path.clone() }),
        &Scripted::new(),
    )
    .await;
    match &e {
        SshError::AuthKeyRejected { path: p, .. } => assert_eq!(p, &path),
        other => panic!("{other:?}"),
    }
    assert_eq!(e.to_message().key(), "auth-key-rejected");
}

#[tokio::test]
async fn public_key_auth_succeeds_and_tilde_path_expands_with_injected_home() {
    let key = ed25519_key(52);
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[PublicKey]),
        pubkeys: vec![key.public_key().clone()],
        ..Policy::default()
    }))
    .await;
    let home = tempfile::tempdir().unwrap();
    key_file(&home, "id_test", &key);
    let mut p = params(
        &server,
        AuthMethod::PublicKey {
            path: "~/id_test".into(),
        },
    );
    p.home_dir = Some(home.path().to_path_buf());
    connect(p, &Scripted::new()).await.unwrap();
}

#[tokio::test]
async fn unreadable_key_file_reports_path_and_reason() {
    let server = TestServer::start(server_with(Policy::default())).await;
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope").to_str().unwrap().to_owned();
    let e = connect_err(
        params(
            &server,
            AuthMethod::PublicKey {
                path: missing.clone(),
            },
        ),
        &Scripted::new(),
    )
    .await;
    match &e {
        SshError::KeyFileUnreadable { path, reason, .. } => {
            assert_eq!(path, &missing);
            assert_eq!(*reason, kanso_types::message::Reason::NotFound);
        }
        other => panic!("{other:?}"),
    }
    // 内容不是私钥：同样是"无法读取"，不是"口令错误"。
    let junk = dir.path().join("junk");
    std::fs::write(&junk, "not a key").unwrap();
    let e = connect_err(
        params(
            &server,
            AuthMethod::PublicKey {
                path: junk.to_str().unwrap().into(),
            },
        ),
        &Scripted::new(),
    )
    .await;
    assert!(matches!(e, SshError::KeyFileUnreadable { .. }), "{e:?}");
    assert_eq!(e.to_message().key(), "ssh-key-file-unreadable");
}

fn encrypted_key_server() -> (ServerConfig, tempfile::TempDir, String) {
    let key = PrivateKey::from_openssh(ENCRYPTED_KEY_PEM)
        .unwrap()
        .decrypt(ENCRYPTED_KEY_PASSPHRASE)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enc");
    std::fs::write(&path, ENCRYPTED_KEY_PEM).unwrap();
    (
        server_with(Policy {
            methods: methods(&[PublicKey]),
            pubkeys: vec![key.public_key().clone()],
            ..Policy::default()
        }),
        dir,
        path.to_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn encrypted_key_uses_saved_passphrase() {
    let (cfg, _d, path) = encrypted_key_server();
    let server = TestServer::start(cfg).await;
    let mut p = params(&server, AuthMethod::PublicKey { path });
    p.saved_passphrase = Some(SecretString::new(ENCRYPTED_KEY_PASSPHRASE));
    let i = Scripted::new();
    connect(p, &i).await.unwrap();
    assert!(i.cred_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn encrypted_key_without_passphrase_asks_user_and_returns_remember() {
    let (cfg, _d, path) = encrypted_key_server();
    let server = TestServer::start(cfg).await;
    let i = Scripted::new().cred(Some((ENCRYPTED_KEY_PASSPHRASE, true)));
    let c = connect(
        params(&server, AuthMethod::PublicKey { path: path.clone() }),
        &i,
    )
    .await
    .unwrap();
    assert_eq!(
        *i.cred_requests.lock().unwrap(),
        [CredentialKind::KeyPassphrase { path }]
    );
    assert_eq!(c.remember.len(), 1);
}

#[tokio::test]
async fn wrong_saved_passphrase_falls_back_to_prompt_then_decrypt_failure() {
    let (cfg, _d, path) = encrypted_key_server();
    let server = TestServer::start(cfg).await;
    let mut p = params(&server, AuthMethod::PublicKey { path: path.clone() });
    p.saved_passphrase = Some(SecretString::new("wrong"));
    // 用户给了正确口令 → 成功。
    connect(
        p,
        &Scripted::new().cred(Some((ENCRYPTED_KEY_PASSPHRASE, false))),
    )
    .await
    .unwrap();
    // 用户也给错 → 解密失败，而不是"密钥被拒"。
    let e = connect_err(
        params(&server, AuthMethod::PublicKey { path: path.clone() }),
        &Scripted::new().cred(Some(("wrong", false))),
    )
    .await;
    assert!(
        matches!(&e, SshError::KeyDecryptFailed { path: p, .. } if *p == path),
        "{e:?}"
    );
    assert_eq!(e.to_message().key(), "ssh-key-decrypt-failed");
}

#[tokio::test]
async fn public_key_then_verification_code_completes_login() {
    let key = ed25519_key(53);
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[PublicKey, KeyboardInteractive]),
        pubkeys: vec![key.public_key().clone()],
        pubkey_partial: true,
        after_partial: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Verification code: ", false)],
        kbd_expect: vec![Some(vec!["123456".into()])],
        ..Policy::default()
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "id", &key);
    let i = Scripted::new().answers(&["123456"]);
    // 认证方式是私钥，保存的密码不得拿来回答验证码。
    let mut p = params(&server, AuthMethod::PublicKey { path });
    p.saved_password = Some(SecretString::new("account-pw"));
    connect(p, &i).await.unwrap();
    let rounds = i.kbd_rounds.lock().unwrap();
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].questions[0].text, "Verification code: ");
    assert!(!rounds[0].questions[0].echo);
    assert_eq!(rounds[0].user, "tester");
    assert_eq!(rounds[0].endpoint, server.endpoint());
    assert_eq!(
        server.stats.with(|o| o.kbd_answers.clone()),
        [vec!["123456".to_owned()]]
    );
}

#[tokio::test]
async fn partial_success_without_usable_method_does_not_say_wrong_password() {
    let key = ed25519_key(54);
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[PublicKey, Password]),
        pubkeys: vec![key.public_key().clone()],
        pubkey_partial: true,
        // 服务器接着只愿意接受本客户端不支持的 hostbased。
        after_partial: methods(&[HostBased]),
        ..Policy::default()
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "id", &key);
    let e = connect_err(
        params(&server, AuthMethod::PublicKey { path }),
        &Scripted::new(),
    )
    .await;
    match &e {
        SshError::AuthPartialSuccess { methods, user, .. } => {
            assert_eq!(methods, &["hostbased".to_owned()]);
            assert_eq!(user, "tester");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(e.to_message().key(), "auth-partial-success");
}

/// 只开 password + publickey 的堡垒机：私钥不在授权列表里时，像 OpenSSH 一样改问密码。
fn bastion_without_kbd() -> Policy {
    Policy {
        methods: methods(&[PublicKey, Password]),
        pubkeys: vec![ed25519_key(60).public_key().clone()],
        password: Some("pw".into()),
        ..Policy::default()
    }
}

#[tokio::test]
async fn rejected_key_falls_back_to_password_prompt() {
    let server = TestServer::start(server_with(bastion_without_kbd())).await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "id", &ed25519_key(61));
    let i = Scripted::new().cred(Some(("pw", true)));
    let c = connect(params(&server, AuthMethod::PublicKey { path }), &i)
        .await
        .unwrap();
    assert_eq!(*i.cred_requests.lock().unwrap(), [CredentialKind::Password]);
    assert_eq!(
        c.remember,
        [RememberedCredential {
            kind: CredentialKind::Password,
            secret: SecretString::new("pw")
        }]
    );
}

#[tokio::test]
async fn rejected_key_then_wrong_password_reports_password_failure() {
    let server = TestServer::start(server_with(bastion_without_kbd())).await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "id", &ed25519_key(62));
    let e = connect_err(
        params(&server, AuthMethod::PublicKey { path }),
        &Scripted::new().cred(Some(("bad", false))),
    )
    .await;
    match &e {
        SshError::AuthFailed { method, .. } => assert_eq!(*method, ProfileAuthMethod::Password),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn rejected_key_password_then_verification_code() {
    let server = TestServer::start(server_with(Policy {
        password_partial: true,
        after_partial: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Verification code: ", false)],
        kbd_expect: vec![Some(vec!["123456".into()])],
        ..bastion_without_kbd()
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "id", &ed25519_key(63));
    let i = Scripted::new()
        .cred(Some(("pw", false)))
        .answers(&["123456"]);
    connect(params(&server, AuthMethod::PublicKey { path }), &i)
        .await
        .unwrap();
    assert_eq!(
        server.stats.with(|o| o.kbd_answers.clone()),
        [vec!["123456".to_owned()]]
    );
}

#[tokio::test]
async fn kbd_password_round_is_answered_with_saved_password() {
    // 设备只提供 keyboard-interactive（没有 password 方法）。
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Password: ", false)],
        kbd_expect: vec![Some(vec!["pw".into()])],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new();
    connect(
        with_password(params(&server, AuthMethod::Password), "pw"),
        &i,
    )
    .await
    .unwrap();
    assert_eq!(i.kbd_count(), 0, "密码轮应自动回答，不弹窗");
    // 服务器没有 password 方法：不应白白发一次 password 认证。
    assert_eq!(
        server.stats.with(|o| o.auth_log.clone()),
        ["none", "kbd-start"]
    );
}

#[tokio::test]
async fn kbd_verification_code_round_is_never_auto_answered() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Verification code: ", false)],
        kbd_expect: vec![Some(vec!["654321".into()])],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().answers(&["654321"]);
    connect(
        with_password(params(&server, AuthMethod::Password), "pw"),
        &i,
    )
    .await
    .unwrap();
    assert_eq!(i.kbd_count(), 1);
    // 账户密码没有出现在任何答案里。
    assert_eq!(
        server.stats.with(|o| o.kbd_answers.clone()),
        [vec!["654321".to_owned()]]
    );
}

#[tokio::test]
async fn saved_password_is_auto_filled_at_most_once() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![
            KbdRoundSpec::question("Password: ", false),
            KbdRoundSpec::question("Password: ", false),
        ],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().answers(&["typed-by-user"]);
    connect(
        with_password(params(&server, AuthMethod::Password), "pw"),
        &i,
    )
    .await
    .unwrap();
    assert_eq!(i.kbd_count(), 1, "第二次问密码要交给用户");
    assert_eq!(
        server.stats.with(|o| o.kbd_answers.clone()),
        [vec!["pw".to_owned()], vec!["typed-by-user".to_owned()]]
    );
}

#[tokio::test]
async fn saved_password_rejected_by_password_method_is_not_reused_in_kbd() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[Password, KeyboardInteractive]),
        password: Some("right".into()),
        kbd: vec![KbdRoundSpec::question("Password: ", false)],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().answers(&["right"]);
    connect(
        with_password(params(&server, AuthMethod::Password), "stale"),
        &i,
    )
    .await
    .unwrap();
    // 已被拒绝的密码不再自动重发（避免账户锁定），改问用户。
    assert_eq!(i.kbd_count(), 1);
    assert_eq!(
        server.stats.with(|o| o.kbd_answers.clone()),
        [vec!["right".to_owned()]]
    );
}

#[tokio::test]
async fn password_question_conditions_are_strict() {
    // 回显的问题、多个问题、不含 password 的问题都不能自动回答。
    for round in [
        KbdRoundSpec::question("Password: ", true),
        KbdRoundSpec::question("Enter PIN: ", false),
        KbdRoundSpec {
            name: String::new(),
            instruction: String::new(),
            prompts: vec![("Password: ".into(), false), ("Code: ".into(), false)],
        },
    ] {
        let n = round.prompts.len();
        let server = TestServer::start(server_with(Policy {
            methods: methods(&[KeyboardInteractive]),
            kbd: vec![round],
            ..Policy::default()
        }))
        .await;
        let i = Scripted::new().default_answers(&["a", "b"]);
        connect(
            with_password(params(&server, AuthMethod::Password), "pw"),
            &i,
        )
        .await
        .unwrap();
        assert_eq!(i.kbd_count(), 1);
        let answers = server.stats.with(|o| o.kbd_answers.clone());
        assert_eq!(answers[0].len(), n, "答案数量会被补齐 / 截断到与问题数一致");
        assert!(!answers[0].iter().any(|a| a == "pw"));
    }
}

#[tokio::test]
async fn password_matching_is_case_insensitive() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Enter YOUR PASSWORD:", false)],
        kbd_expect: vec![Some(vec!["pw".into()])],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new();
    connect(
        with_password(params(&server, AuthMethod::Password), "pw"),
        &i,
    )
    .await
    .unwrap();
    assert_eq!(i.kbd_count(), 0);
}

#[tokio::test]
async fn text_only_round_is_answered_empty_and_carried_to_next_prompt() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![
            KbdRoundSpec::text("Push sent to your phone"),
            KbdRoundSpec::question("Code: ", true),
        ],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().answers(&["1"]);
    connect(params(&server, AuthMethod::Password), &i)
        .await
        .unwrap();
    let rounds = i.kbd_rounds.lock().unwrap();
    assert_eq!(rounds.len(), 1, "只有文字的轮次不弹窗");
    assert_eq!(rounds[0].preamble, ["Push sent to your phone"]);
    drop(rounds);
    assert_eq!(
        server.stats.with(|o| o.kbd_answers.clone()),
        [Vec::<String>::new(), vec!["1".to_owned()]]
    );
}

#[tokio::test]
async fn kbd_stops_after_sixteen_rounds() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Code: ", true)],
        kbd_endless: true,
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().default_answers(&["x"]);
    let e = connect_err(params(&server, AuthMethod::Password), &i).await;
    assert!(
        matches!(&e, SshError::AuthTooManyRounds { max_rounds: 16, .. }),
        "{e:?}"
    );
    assert_eq!(i.kbd_count(), 16);
    assert_eq!(e.to_message().key(), "auth-too-many-rounds");
}

#[tokio::test]
async fn user_cancel_reports_auth_cancelled() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Code: ", true)],
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().kbd([KbdReply::Cancel]);
    let e = connect_err(params(&server, AuthMethod::Password), &i).await;
    assert!(matches!(e, SshError::AuthCancelled { .. }), "{e:?}");
}

#[tokio::test]
async fn server_rejecting_responses_reports_response_rejected() {
    let server = TestServer::start(server_with(Policy {
        methods: methods(&[KeyboardInteractive]),
        kbd: vec![KbdRoundSpec::question("Code: ", true)],
        kbd_accept: false,
        ..Policy::default()
    }))
    .await;
    let i = Scripted::new().answers(&["wrong"]);
    let e = connect_err(params(&server, AuthMethod::Password), &i).await;
    assert!(matches!(&e, SshError::AuthResponseRejected { .. }), "{e:?}");
    assert_eq!(e.to_message().key(), "auth-response-rejected");
}

/// 180 秒无应答按取消处理。用暂停的时钟推进，不真的等。
#[tokio::test(start_paused = true)]
async fn prompt_times_out_after_180_seconds_as_cancel() {
    let server = TestServer::start(ServerConfig {
        inactivity: None,
        policy: Policy {
            methods: methods(&[KeyboardInteractive]),
            kbd: vec![KbdRoundSpec::question("Code: ", true)],
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let i = Scripted::new().kbd([KbdReply::Hang]);
    let e = connect_err(params(&server, AuthMethod::Password), &i).await;
    assert!(matches!(e, SshError::AuthCancelled { .. }), "{e:?}");
    // 只量"提问开始 → 超时取消"这一段：握手阶段的 spawn_blocking 会让暂停的时钟提前自动推进，
    // 把它算进来会得到不稳定的总时长。
    let waited = i.kbd_started.lock().unwrap().unwrap().elapsed();
    assert!(waited >= Duration::from_secs(180), "{waited:?}");
    assert!(waited < Duration::from_secs(181), "{waited:?}");
}

#[tokio::test]
async fn rsa_key_signs_with_sha2_for_server_that_refuses_sha1() {
    // 服务器只愿意接受 rsa-sha2-256，并通过 server-sig-algs 宣告。
    let key = rsa_key();
    let cfg = ServerConfig {
        preferred: russh::Preferred {
            key: std::borrow::Cow::Owned(vec![Algorithm::Rsa {
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
    };
    let server = TestServer::start(cfg).await;

    // 对照组：直接用 SHA-1 签名确实会被这台服务器拒绝，否则下面的断言没有意义。
    {
        use super::support::USER;
        struct AcceptAll;
        impl russh::client::Handler for AcceptAll {
            type Error = russh::Error;
            async fn check_server_key(
                &mut self,
                _: &russh::keys::PublicKeyOrCertificate,
            ) -> Result<bool, Self::Error> {
                Ok(true)
            }
        }
        let stream = tokio::net::TcpStream::connect(server.addr()).await.unwrap();
        let mut h = russh::client::connect_stream(
            Arc::new(russh::client::Config::default()),
            stream,
            AcceptAll,
        )
        .await
        .unwrap();
        let sha1 = h
            .authenticate_publickey(
                USER,
                PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None),
            )
            .await
            .unwrap();
        assert!(!sha1.success(), "对照组：SHA-1 签名应被拒绝");
    }
    server.stats.with(|o| o.auth_log.clear());

    let dir = tempfile::tempdir().unwrap();
    let path = key_file(&dir, "rsa", &key);
    connect(
        params(&server, AuthMethod::PublicKey { path }),
        &Scripted::new(),
    )
    .await
    .unwrap();
    let log = server.stats.with(|o| o.auth_log.clone());
    assert!(log.iter().any(|l| l == "publickey:rsa-sha2-256"), "{log:?}");
    assert!(!log.iter().any(|l| l.contains("ssh-rsa")), "{log:?}");
}
