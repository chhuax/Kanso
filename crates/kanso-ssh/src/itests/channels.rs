//! shell / exec 通道、连接共享与 keepalive（ssh.md §2.4、§6、§10、§12）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kanso_types::profile::Locale;

use super::server::{Policy, ServerConfig, TestServer};
use super::support::{Blackhole, Scripted, connect_ok, eventually, params};
use crate::{AuthMethod, CloseReason, Connection, PtyRequest, SshError, connect_over};

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

async fn conn(server: &TestServer) -> Connection {
    connect_ok(server, AuthMethod::Password, &Scripted::new())
        .await
        .connection
}

/// 读到至少 `want` 字节（或通道关闭）。
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
async fn shell_requests_pty_and_env_in_order_and_supports_read_write_resize() {
    let server = open_server().await;
    let c = conn(&server).await;
    let mut sh = c
        .open_shell(PtyRequest {
            cols: 100,
            rows: 30,
            lang: Locale::parse("zh_CN.UTF-8"),
        })
        .await
        .unwrap();
    server.stats.with(|o| {
        assert_eq!(o.ptys, [("xterm-256color".to_owned(), 100, 30)]);
        assert_eq!(
            o.envs,
            [
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
                ("TERM_PROGRAM".to_owned(), "Kanso".to_owned()),
                ("LANG".to_owned(), "zh_CN.UTF-8".to_owned()),
            ]
        );
        assert_eq!(o.shells, 1);
    });
    assert_eq!(read_until(&mut sh, b"WELCOME").await, b"WELCOME\r\n");

    sh.write(b"hello\n").await.unwrap();
    assert_eq!(read_until(&mut sh, b"hello").await, b"hello\n");

    sh.resize(120, 40).await.unwrap();
    eventually(|| server.stats.with(|o| o.window_changes == [(120, 40)])).await;

    // stderr 与 stdout 合并成同一条字节流。
    sh.write(b"err\n").await.unwrap();
    assert_eq!(read_until(&mut sh, b"to-stderr").await, b"to-stderr\n");

    // 退出码：服务器先发 exit-status 再关闭通道。
    sh.write(b"exit\n").await.unwrap();
    while sh.read().await.is_some() {}
    assert_eq!(sh.exit_code(), Some(3));
}

#[tokio::test]
async fn lang_is_not_sent_unless_caller_gives_a_valid_locale() {
    let server = open_server().await;
    let c = conn(&server).await;
    let _sh = c.open_shell(PtyRequest::default()).await.unwrap();
    server.stats.with(|o| {
        assert_eq!(o.ptys, [("xterm-256color".to_owned(), 80, 24)]);
        assert!(o.envs.iter().all(|(k, _)| k != "LANG"), "{:?}", o.envs);
    });
    // 不合规的 locale（含注入字符）在 Locale::parse 就被挡掉，调用方只能传 None。
    assert!(Locale::parse("en_US; rm -rf /").is_none());
}

#[tokio::test]
async fn second_shell_on_same_connection_does_not_reauthenticate() {
    let server = open_server().await;
    let c = conn(&server).await;
    let mut a = c.open_shell(PtyRequest::default()).await.unwrap();
    let mut b = c.clone().open_shell(PtyRequest::default()).await.unwrap();
    assert_eq!(read_until(&mut a, b"WELCOME").await, b"WELCOME\r\n");
    assert_eq!(read_until(&mut b, b"WELCOME").await, b"WELCOME\r\n");
    server.stats.with(|o| {
        assert_eq!(o.accepted, 1, "只有一次 TCP 连接 / 握手");
        assert_eq!(o.auth_log, ["none"], "只认证了一次");
        assert_eq!(o.shells, 2);
    });
}

#[tokio::test]
async fn connection_closes_only_after_last_user_is_released() {
    let server = open_server().await;
    let c = conn(&server).await;
    let sh1 = c.open_shell(PtyRequest::default()).await.unwrap();
    let sh2 = c.open_shell(PtyRequest::default()).await.unwrap();

    drop(c);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.stats.with(|o| o.closed), 0, "还有通道在用");
    drop(sh1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(server.stats.with(|o| o.closed), 0, "还有一个通道在用");
    drop(sh2);
    eventually(|| server.stats.with(|o| o.closed == 1)).await;
}

#[tokio::test]
async fn concurrent_release_disconnects_exactly_once() {
    let server = open_server().await;
    let c = conn(&server).await;
    let mut shells = Vec::new();
    for _ in 0..4 {
        shells.push(c.open_shell(PtyRequest::default()).await.unwrap());
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = Vec::new();
    // 12 个连接克隆 + 4 个通道，在同一时刻并发释放。
    for _ in 0..12 {
        let (c, b) = (c.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            b.wait().await;
            drop(c);
        }));
    }
    for sh in shells {
        let b = barrier.clone();
        tasks.push(tokio::spawn(async move {
            b.wait().await;
            drop(sh);
        }));
    }
    // 最后一份由本任务持有：其他全部释放后连接必须仍然可用。
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(server.stats.with(|o| o.closed), 0);
    let _extra = c.open_shell(PtyRequest::default()).await.unwrap();
    drop(_extra);
    drop(c);
    eventually(|| server.stats.with(|o| o.closed == 1)).await;
    // 不会重复断开：多等一会儿计数仍然是 1。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.stats.with(|o| (o.accepted, o.closed)), (1, 1));
}

#[tokio::test]
async fn open_shell_after_disconnect_reports_shared_connection_closed_without_reconnecting() {
    let server = open_server().await;
    let c = conn(&server).await;
    let mut sh = c.open_shell(PtyRequest::default()).await.unwrap();
    // 服务器掉线：所有通道都得到结束通知。
    sh.write(b"disconnect\n").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), c.wait_closed())
            .await
            .unwrap(),
        CloseReason::Lost
    );
    assert!(sh.read().await.is_none() || sh.read().await.is_none());
    assert!(c.is_closed());

    let e = c.open_shell(PtyRequest::default()).await.err().unwrap();
    assert!(
        matches!(&e, SshError::SharedConnectionClosed { endpoint } if *endpoint == server.endpoint()),
        "{e:?}"
    );
    assert_eq!(e.to_message().key(), "ssh-shared-connection-closed");
    // 没有偷偷重连。
    assert_eq!(server.stats.with(|o| o.accepted), 1);
    // 写失败立即报错。
    assert!(sh.write(b"x").await.is_err());
    // exec 同样。
    assert!(matches!(
        c.open_exec("cat").await.err().unwrap(),
        SshError::SharedConnectionClosed { .. }
    ));
}

#[tokio::test]
async fn exec_channel_reads_stdout_until_close_and_gets_late_exit_status() {
    let server = open_server().await;
    let c = conn(&server).await;
    // 服务器先 eof、再发退出码、最后才 close：必须以 close 为结束信号才拿得到退出码。
    let mut ex = c.open_exec("late-status").await.unwrap();
    assert_eq!(ex.read_to_close().await, b"out\n", "stderr 不混进 stdout");
    assert_eq!(ex.exit_code(), Some(7));
}

#[tokio::test]
async fn exec_channel_writes_stdin_then_eof() {
    let server = open_server().await;
    let c = conn(&server).await;
    let mut ex = c.open_exec("cat").await.unwrap();
    ex.write(b"echo-this").await.unwrap();
    ex.eof().await.unwrap();
    assert_eq!(ex.read_to_close().await, b"echo-this");
    assert_eq!(ex.exit_code(), Some(0));
    // exec 与 shell 互不影响：同一连接上还能开 shell。
    let mut sh = c.open_shell(PtyRequest::default()).await.unwrap();
    assert_eq!(read_until(&mut sh, b"WELCOME").await, b"WELCOME\r\n");
}

/// 通过可黑洞化的字节流连接：认证后切断网络。
async fn connect_blackholed(server: &TestServer) -> (Connection, Arc<AtomicBool>) {
    let block = Arc::new(AtomicBool::new(false));
    let stream = Blackhole {
        inner: tokio::net::TcpStream::connect(server.addr()).await.unwrap(),
        block: block.clone(),
    };
    let c = connect_over(
        stream,
        params(server, AuthMethod::Password),
        &Scripted::new(),
    )
    .await
    .unwrap();
    (c.connection, block)
}

/// keepalive 断线检测：对端不再回应，约 `(3 + 1) * 30` 秒后判定断线。用暂停的时钟推进。
#[tokio::test(start_paused = true)]
async fn keepalive_timeout_detects_dead_connection() {
    let server = open_server().await;
    let (c, block) = connect_blackholed(&server).await;
    let mut sh = c.open_shell(PtyRequest::default()).await.unwrap();

    block.store(true, Ordering::SeqCst);
    let start = tokio::time::Instant::now();
    let reason = tokio::time::timeout(Duration::from_secs(300), c.wait_closed())
        .await
        .expect("应在 keepalive 超时后判定断线");
    assert_eq!(reason, CloseReason::KeepaliveTimeout);
    let waited = start.elapsed();
    // 连续 3 次 keepalive 无回应（间隔 30 秒）；russh 在第 4 个间隔到期时才断开。
    assert!(waited >= Duration::from_secs(90), "{waited:?}");
    assert!(waited <= Duration::from_secs(125), "{waited:?}");
    // 先把缓冲里的欢迎信息读掉，之后必须收到结束通知。
    while sh.read().await.is_some() {}
    let e = sh.write(b"x").await.err().unwrap();
    assert!(matches!(e, SshError::KeepaliveTimeout { .. }), "{e:?}");
    assert_eq!(e.to_message().key(), "ssh-keepalive-timeout");
}

/// 对端正常回应 keepalive 时，不设空闲超时：很久没有输入输出也不会断。
#[tokio::test(start_paused = true)]
async fn idle_connection_with_responsive_server_stays_open() {
    let server = open_server().await;
    let c = conn(&server).await;
    let _sh = c.open_shell(PtyRequest::default()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(400)).await;
    assert!(!c.is_closed());
    assert_eq!(c.close_reason(), None);
}
