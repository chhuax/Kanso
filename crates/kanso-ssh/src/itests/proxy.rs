//! ProxyCommand（ssh.md §5）：用 `nc %h %p` 连进程内测试服务器。
//!
//! 需要系统有 `nc`；没有时跳过（这些测试只在 Unix 上运行，`/bin/sh` 是前提）。

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use kanso_types::message::Reason;

use super::server::{Policy, ServerConfig, TestServer};
use super::support::{Scripted, eventually, params_ep};
use crate::{AuthMethod, HopParams, MemoryHostKeyStore, PtyRequest, SshError, connect_chain};

fn have_nc() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("nc").is_file()))
        .unwrap_or(false)
}

async fn open_server() -> TestServer {
    TestServer::start(ServerConfig {
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await
}

fn hop(name: &str, server: &TestServer, proxy: Option<&str>) -> HopParams {
    let mut p = params_ep(server.endpoint(), AuthMethod::Password);
    p.host_keys = Arc::new(MemoryHostKeyStore::new());
    HopParams {
        name: name.into(),
        connect: p,
        proxy_command: proxy.map(str::to_owned),
    }
}

fn pid_alive(pid: &str) -> bool {
    // `kill -0` 对僵尸进程也成功：它为 false 就说明进程已被回收，不只是被杀死。
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

async fn read_welcome(conn: &crate::Connection) {
    let mut sh = conn.open_shell(PtyRequest::default()).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), sh.read())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&got).contains("WELCOME"));
}

#[tokio::test]
async fn connects_through_nc_with_percent_substitution() {
    if !have_nc() {
        return;
    }
    let server = open_server().await;
    let got = connect_chain(vec![hop("t", &server, Some("nc %h %p"))], &Scripted::new())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    read_welcome(&got.connection).await;
    assert_eq!(server.stats.with(|o| o.accepted), 1);
}

#[tokio::test]
async fn child_is_killed_and_reaped_when_connection_is_released() {
    if !have_nc() {
        return;
    }
    let server = open_server().await;
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    // `exec` 让 sh 被 nc 替换，记录的 pid 就是真正持有管道的进程。
    let cmd = format!("echo $$ > {}; exec nc %h %p", pid_file.display());
    let got = connect_chain(vec![hop("t", &server, Some(&cmd))], &Scripted::new())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    assert!(pid_alive(&pid), "连接期间命令应在运行");
    drop(got);
    eventually(|| !pid_alive(&pid)).await;
    eventually(|| server.stats.with(|o| o.closed) == 1).await;
}

#[tokio::test]
async fn command_not_found_reports_exit_code_and_stderr() {
    let server = open_server().await;
    let err = match connect_chain(
        vec![hop("t", &server, Some("kanso-no-such-proxy-cmd %h"))],
        &Scripted::new(),
    )
    .await
    {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    let SshError::ProxyCommandFailed {
        command, reason, ..
    } = &err
    else {
        panic!("{err:?}");
    };
    assert_eq!(command, "kanso-no-such-proxy-cmd %h");
    let Reason::Other { detail } = reason else {
        panic!("{reason:?}");
    };
    assert!(detail.contains("127"), "{detail}");
    assert!(detail.contains("kanso-no-such-proxy-cmd"), "{detail}");
    assert_eq!(err.to_message().key(), "ssh-proxy-command-failed");
    // 命令根本没连上服务器。
    assert_eq!(server.stats.with(|o| o.accepted), 0);
}

#[tokio::test]
async fn immediate_exit_reports_stderr_tail() {
    let server = open_server().await;
    let err = match connect_chain(
        vec![hop("t", &server, Some("echo boom-from-proxy >&2; exit 3"))],
        &Scripted::new(),
    )
    .await
    {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    let SshError::ProxyCommandFailed { reason, .. } = &err else {
        panic!("{err:?}");
    };
    let Reason::Other { detail } = reason else {
        panic!("{reason:?}");
    };
    assert!(detail.contains("code 3"), "{detail}");
    assert!(detail.contains("boom-from-proxy"), "{detail}");
}

#[tokio::test]
async fn unsafe_host_is_refused_before_spawning_anything() {
    let server = open_server().await;
    let mut h = hop("t", &server, Some("nc %h %p"));
    h.connect.endpoint.host = "h; touch /tmp/kanso-should-not-exist".into();
    let err = match connect_chain(vec![h], &Scripted::new()).await {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SshError::ProxyCommandFailed { .. }),
        "{err:?}"
    );
    assert!(!Path::new("/tmp/kanso-should-not-exist").exists());
}

#[tokio::test]
async fn proxy_command_can_be_the_transport_of_the_first_jump() {
    if !have_nc() {
        return;
    }
    let a = open_server().await;
    let b = open_server().await;
    let got = connect_chain(
        vec![hop("a", &a, Some("nc %h %p")), hop("b", &b, None)],
        &Scripted::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got.connection.endpoint().via, ["a"]);
    read_welcome(&got.connection).await;
}
