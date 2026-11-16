//! 真实进程测试：启动 shell / 命令，验证读写、退出码、从端关闭、调整大小、kill、前台进程 cwd。
//! 全部只用 `/bin/sh` 与 `sleep`，不读取任何用户配置文件。

#![cfg(unix)]

use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kanso_pty::{CommandSpec, EnvSnapshot, PtySession, SpawnConfig};
use kanso_types::GridSize;

fn spawn(program: &str, args: &[&str]) -> PtySession {
    let env = EnvSnapshot::from_pairs([("PATH", "/usr/bin:/bin"), ("TERM", "xterm-256color")]);
    PtySession::spawn(SpawnConfig {
        command: CommandSpec {
            program: program.to_owned(),
            args: args.iter().map(|s| s.to_string()).collect(),
        },
        env,
        cwd: std::env::temp_dir(),
        size: GridSize { cols: 80, rows: 24 },
    })
    .unwrap()
}

/// 在线程里读到 EOF，把全部输出发回来；超时说明 EOF 没有到达（从端没关闭）。
fn read_to_eof(session: &PtySession) -> mpsc::Receiver<Vec<u8>> {
    let mut reader = session.reader().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        let _ = tx.send(all);
    });
    rx
}

#[test]
fn output_exit_code_and_eof() {
    let mut s = spawn("/bin/sh", &["-c", "printf hi; exit 3"]);
    let rx = read_to_eof(&s);
    // 能在超时前收到输出，同时证明从端已关闭、读循环拿到了 EOF。
    let out = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("读循环没有等到 EOF");
    assert!(String::from_utf8_lossy(&out).contains("hi"));
    let st = s.wait().unwrap();
    assert_eq!(st.code(), Some(3));
    assert!(!st.success());
    assert!(st.signal().is_none());
    assert!(s.pid().is_some());
}

#[test]
fn resize_is_visible_to_the_child() {
    let mut s = spawn("/bin/sh", &["-c", "read x; stty size"]);
    s.resize(
        GridSize {
            cols: 100,
            rows: 30,
        },
        0,
        0,
    )
    .unwrap();
    let rx = read_to_eof(&s);
    s.writer().write_all(b"go\n").unwrap();
    let out = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        String::from_utf8_lossy(&out).contains("30 100"),
        "{:?}",
        String::from_utf8_lossy(&out)
    );
    s.wait().unwrap();
}

#[test]
fn kill_terminates_sleep() {
    let mut s = spawn("sleep", &["100"]);
    assert!(s.try_wait().unwrap().is_none());
    let start = Instant::now();
    s.kill().unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    let st = s.try_wait().unwrap();
    // kill 已经回收过进程，再次查询要么仍能给出状态、要么是已回收；都不得显示"仍在运行"。
    assert!(st.is_none_or(|st| !st.success()));
    // 重复 kill 不报错。
    s.kill().unwrap();
}

#[test]
fn try_wait_reports_running_then_exited() {
    let mut s = spawn("/bin/sh", &["-c", "sleep 0.3; exit 0"]);
    assert!(s.try_wait().unwrap().is_none());
    let st = s.wait().unwrap();
    assert!(st.success());
}

fn canon(p: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap()
}

#[test]
fn foreground_cwd_follows_foreground_process() {
    // 前台进程是 sleep（cwd 为 /tmp），不是 shell 自己（cwd 为 temp_dir 之外的起始目录）。
    let s = spawn("/bin/sh", &["-c", "cd /tmp && sleep 5"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let want = canon(Path::new("/tmp"));
    loop {
        // 进程刚启动时前台进程组可能还是 sh 本身，轮询到 sleep 接管前台为止。
        if s.foreground_cwd().map(|p| canon(&p)) == Some(want.clone()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "前台 cwd 没有变成 /tmp：{:?}",
            s.foreground_cwd()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // macOS 上 /tmp 是符号链接，系统查询返回真实路径。
    if cfg!(target_os = "macos") {
        assert_eq!(s.foreground_cwd().unwrap(), Path::new("/private/tmp"));
    }
    let mut s = s;
    s.kill().unwrap();
}

#[test]
fn foreground_cwd_of_shell_itself() {
    // 直接启动的进程就是前台进程组组长：读到的是它自己的 cwd，也就是 spawn 时给的起始目录。
    let mut s = spawn("sleep", &["5"]);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        s.foreground_cwd().map(|p| canon(&p)),
        Some(canon(&std::env::temp_dir()))
    );
    s.kill().unwrap();
}
