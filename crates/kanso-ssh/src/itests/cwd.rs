//! 远端工作目录查询（ssh.md §10）：
//! - 探测脚本：用本机 `/bin/sh` + 伪造的 `/proc` 目录树直接运行（macOS 没有 /proc）；
//! - exec 通道、5 秒超时：进程内测试服务器。

use std::time::Duration;

use super::server::{Policy, ServerConfig, TestServer};
use super::support::{Scripted, connect_ok};
use crate::cwd::{PROBE_SCRIPT, parse_probe_output};
use crate::{AuthMethod, CwdError, PtyRequest, query_working_directory};

#[cfg(unix)]
mod script {
    use std::io::Write as _;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use super::*;

    const CONN: &str = "192.0.2.10 54321 198.51.100.7 22";

    /// 在伪造的 /proc 里放一个进程。`cwd = None` 表示读不到 cwd（别的用户的进程）。
    // 参数就是 /proc/<pid>/stat 里各字段的原样展开，拆成结构体只会让每个用例更啰嗦。
    #[allow(clippy::too_many_arguments)]
    fn add_proc(
        root: &Path,
        pid: u32,
        comm: &str,
        tty_nr: i64,
        pgrp: u32,
        tpgid: i64,
        env: &[&str],
        cwd: Option<&str>,
    ) {
        let d = root.join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("stat"),
            format!("{pid} ({comm}) S 1 {pgrp} {pid} {tty_nr} {tpgid} 4194304 100 0 0 0 1 1 0 0 20 0 1 0 5 0 0\n"),
        )
        .unwrap();
        std::fs::write(d.join("environ"), env.join("\0") + "\0").unwrap();
        if let Some(cwd) = cwd {
            std::os::unix::fs::symlink(cwd, d.join("cwd")).unwrap();
        }
    }

    fn ssh_env() -> String {
        format!("SSH_CONNECTION={CONN}")
    }

    /// 用 `/bin/sh` 从 stdin 读脚本运行，返回 stdout。`prelude` 模拟登录 rc 文件打印的杂讯。
    fn run(root: &Path, ssh_connection: Option<&str>, prelude: &str) -> Vec<u8> {
        // 外层 sh 先打印杂讯再 exec 成读 stdin 的 sh，与"登录 shell -c sh"的效果一致。
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!("{prelude}exec /bin/sh"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("KANSO_PROC_ROOT", root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(c) = ssh_connection {
            cmd.env("SSH_CONNECTION", c);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(PROBE_SCRIPT.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        out.stdout
    }

    fn probe(root: &Path) -> Result<String, CwdError> {
        parse_probe_output(&run(root, Some(CONN), ""))
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn finds_foreground_cwd_of_the_shell_channel() {
        let t = tmp();
        // 33 号是 exec 通道里的 sh 自己：同连接但没有控制终端。
        add_proc(t.path(), 33, "sh", 0, 33, -1, &[&ssh_env()], Some("/"));
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            100,
            &[&ssh_env()],
            Some("/home/u/proj"),
        );
        assert_eq!(probe(t.path()).unwrap(), "/home/u/proj");
    }

    #[test]
    fn only_exactly_equal_ssh_connection_matches() {
        let t = tmp();
        // 另一条连接（端口不同，且值互为前缀 / 后缀）的 shell：pid 字典序更靠前。
        add_proc(
            t.path(),
            10,
            "bash",
            34817,
            10,
            10,
            &[&format!("SSH_CONNECTION={CONN}0")],
            Some("/other/a"),
        );
        add_proc(
            t.path(),
            11,
            "bash",
            34818,
            11,
            11,
            &["SSH_CONNECTION=192.0.2.10 54321 198.51.100.7 2"],
            Some("/other/b"),
        );
        add_proc(
            t.path(),
            12,
            "bash",
            34819,
            12,
            12,
            &[&format!("XSSH_CONNECTION={CONN}")],
            Some("/other/c"),
        );
        // 正则元字符不能被当正则：点号匹配任意字符的话下面这条会误中。
        add_proc(
            t.path(),
            13,
            "bash",
            34820,
            13,
            13,
            &["SSH_CONNECTION=192x0x2x10 54321 198.51.100.7 22"],
            Some("/other/d"),
        );
        add_proc(
            t.path(),
            200,
            "bash",
            34830,
            200,
            200,
            &["HOME=/home/u", &ssh_env()],
            Some("/mine"),
        );
        assert_eq!(probe(t.path()).unwrap(), "/mine");
    }

    #[test]
    fn process_without_controlling_terminal_is_excluded() {
        let t = tmp();
        add_proc(
            t.path(),
            100,
            "sh",
            0,
            100,
            -1,
            &[&ssh_env()],
            Some("/not/a/shell"),
        );
        // 有终端号但没有前台进程组（tpgid = -1）同样不是 shell 通道。
        add_proc(
            t.path(),
            101,
            "x",
            34816,
            101,
            -1,
            &[&ssh_env()],
            Some("/nope"),
        );
        assert_eq!(probe(t.path()), Err(CwdError::NoShell));
    }

    #[test]
    fn nested_foreground_process_group_is_used() {
        let t = tmp();
        // shell(100) 的前台进程组是它启动的 vim(250)：要的是 vim 的 cwd，而不是 shell 的。
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            250,
            &[&ssh_env()],
            Some("/home/u"),
        );
        add_proc(
            t.path(),
            250,
            "vim",
            34816,
            250,
            250,
            &[&ssh_env()],
            Some("/srv/work"),
        );
        assert_eq!(probe(t.path()).unwrap(), "/srv/work");
    }

    #[test]
    fn foreground_process_of_another_user_is_reported() {
        let t = tmp();
        // `su -` 之后前台是 root 的 shell：它的目录在，但 cwd 链接读不到。
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            300,
            &[&ssh_env()],
            Some("/home/u"),
        );
        add_proc(t.path(), 300, "su", 34816, 300, 300, &[], None);
        assert_eq!(probe(t.path()), Err(CwdError::OtherUser));
    }

    #[test]
    fn vanished_foreground_leader_is_unavailable() {
        let t = tmp();
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            999,
            &[&ssh_env()],
            Some("/home/u"),
        );
        assert!(matches!(probe(t.path()), Err(CwdError::Unavailable { .. })));
    }

    #[test]
    fn missing_proc_is_reported() {
        let t = tmp();
        assert_eq!(probe(&t.path().join("no-such-proc")), Err(CwdError::NoProc));
    }

    #[test]
    fn missing_ssh_connection_is_reported() {
        let t = tmp();
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            100,
            &[&ssh_env()],
            Some("/x"),
        );
        assert_eq!(
            parse_probe_output(&run(t.path(), None, "")),
            Err(CwdError::NoSshConnection)
        );
    }

    #[test]
    fn no_matching_shell_is_reported() {
        let t = tmp();
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            100,
            &["SSH_CONNECTION=203.0.113.1 1 203.0.113.2 22"],
            Some("/x"),
        );
        assert_eq!(probe(t.path()), Err(CwdError::NoShell));
    }

    #[test]
    fn rc_file_noise_is_ignored() {
        let t = tmp();
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            100,
            &[&ssh_env()],
            Some("/home/u/real"),
        );
        // 杂讯里还有看着像路径、像结果的行：只认带前缀的那一行。
        let out = run(
            t.path(),
            Some(CONN),
            "echo 'Last login: Mon Jan  1 00:00:00'; echo '/tmp/fake'; echo 'cwd=/etc'; ",
        );
        assert!(String::from_utf8_lossy(&out).contains("Last login"));
        assert_eq!(parse_probe_output(&out).unwrap(), "/home/u/real");
    }

    #[test]
    fn comm_with_spaces_and_parentheses_is_parsed() {
        let t = tmp();
        add_proc(
            t.path(),
            100,
            "my (odd) sh",
            34816,
            100,
            100,
            &[&ssh_env()],
            Some("/p q/r"),
        );
        assert_eq!(probe(t.path()).unwrap(), "/p q/r");
    }

    #[test]
    fn unreadable_environ_of_other_processes_is_skipped() {
        let t = tmp();
        // 这个进程有终端但读不到 environ（别的用户的进程）：跳过而不是中断扫描。
        add_proc(t.path(), 10, "root-sh", 34815, 10, 10, &[], Some("/root"));
        std::fs::remove_file(t.path().join("10/environ")).unwrap();
        add_proc(
            t.path(),
            100,
            "bash",
            34816,
            100,
            100,
            &[&ssh_env()],
            Some("/home/u"),
        );
        assert_eq!(probe(t.path()).unwrap(), "/home/u");
    }
}

fn open_server(sh_reply: Option<&[u8]>) -> ServerConfig {
    ServerConfig {
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        sh_reply: sh_reply.map(<[u8]>::to_vec),
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn query_runs_script_on_a_separate_exec_channel_and_ignores_noise() {
    let server =
        TestServer::start(open_server(Some(b"Welcome back\n__KANSO_CWD__=/srv/app\n"))).await;
    let conn = connect_ok(&server, AuthMethod::Password, &Scripted::new())
        .await
        .connection;
    // 正在使用的 shell 通道：查询不能碰它。
    let mut sh = conn.open_shell(PtyRequest::default()).await.unwrap();
    let _ = sh.read().await;

    assert_eq!(query_working_directory(&conn).await.unwrap(), "/srv/app");
    server.stats.with(|o| {
        // exec 命令就是 `sh`，脚本整体走 stdin。
        assert_eq!(o.execs, ["sh"]);
        assert_eq!(o.stdin_seen, PROBE_SCRIPT.as_bytes());
        assert_eq!(o.shells, 1);
    });
    // shell 通道不受影响。
    sh.write(b"still-alive\n").await.unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(5), sh.read())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&echoed[..], b"still-alive\n");
}

#[tokio::test]
async fn query_maps_failure_codes() {
    for (reply, want) in [
        (&b"__KANSO_CWD__!noshell\n"[..], CwdError::NoShell),
        (b"__KANSO_CWD__!nosshconn\n", CwdError::NoSshConnection),
        (b"__KANSO_CWD__!otheruser\n", CwdError::OtherUser),
        (b"__KANSO_CWD__!noproc\n", CwdError::NoProc),
    ] {
        let server = TestServer::start(open_server(Some(reply))).await;
        let conn = connect_ok(&server, AuthMethod::Password, &Scripted::new())
            .await
            .connection;
        assert_eq!(query_working_directory(&conn).await, Err(want));
    }
}

#[tokio::test]
async fn query_times_out_after_five_seconds_and_connection_stays_usable() {
    // 服务器收到脚本后一直不回复。
    let server = TestServer::start(open_server(None)).await;
    let conn = connect_ok(&server, AuthMethod::Password, &Scripted::new())
        .await
        .connection;
    // 连接建好之后再暂停时钟：握手里的 spawn_blocking 在暂停时钟下会让虚拟时间乱跳。
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let r = query_working_directory(&conn).await;
    let elapsed = started.elapsed();
    assert_eq!(r, Err(CwdError::Timeout));
    assert!(
        elapsed >= Duration::from_secs(5) && elapsed < Duration::from_secs(6),
        "{elapsed:?}"
    );
    // 超时不影响连接：还能开新的通道。
    tokio::time::resume();
    conn.open_shell(PtyRequest::default()).await.unwrap();
}
