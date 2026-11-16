//! 本地会话端到端测试：真实 PTY + `/bin/sh -i`，不碰用户的真实环境（见 `common`）。

#![cfg(unix)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::*;
use kanso_frame::Frame;
use kanso_types::event::{Activity, CloseReason, SessionState};
use kanso_types::{
    CoreEvent, GridSize, Key, KeyEvent, KeyEventKind, Modifiers, PasteContent, SessionId,
};

fn is_dirty(e: &CoreEvent) -> bool {
    matches!(e, CoreEvent::TerminalDirty { .. })
}

fn ctrl(c: &str) -> KeyEvent {
    KeyEvent {
        key: Key::Character(c.to_owned()),
        mods: Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        text: None,
        kind: KeyEventKind::Press,
    }
}

fn plain(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        mods: Modifiers::NONE,
        text: None,
        kind: KeyEventKind::Press,
    }
}

fn closed_events(fx: &Fixture, id: SessionId) -> Vec<CloseReason> {
    fx.rec
        .all()
        .into_iter()
        .filter_map(|e| match e {
            CoreEvent::SessionStateChanged {
                session,
                state: SessionState::Closed(r),
            } if session == id => Some(r),
            _ => None,
        })
        .collect()
}

#[test]
fn open_emits_connecting_connected_then_dirty_in_order() {
    let fx = sh_fixture();
    let (id, _s) = fx.open();
    let events = fx.rec.all();
    assert_eq!(
        events[0],
        CoreEvent::SessionStateChanged {
            session: id,
            state: SessionState::Connecting
        }
    );
    assert_eq!(
        events[1],
        CoreEvent::SessionStateChanged {
            session: id,
            state: SessionState::Connected
        }
    );
    assert_eq!(events[2], CoreEvent::TerminalDirty { session: id });
    // 摘要里能查到会话，名字是 shell 程序名。
    let info = fx.mgr.summary(id).unwrap();
    assert_eq!(info.name, "sh");
    assert_eq!(info.state, SessionState::Connected);
}

#[test]
fn text_input_reaches_shell_and_frame() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    // 用算术展开让"输出"与"回显的输入"不同：只有 shell 真正执行了才会出现 he2llo。
    s.text_input("echo he$((1+1))llo\r");
    assert!(screen.wait_contains("he2llo"), "{}", screen.text);
    s.text_input("echo hello\r");
    assert!(screen.wait_contains("hello"));
}

#[test]
fn shell_exit_emits_exited_once_and_input_is_dropped_afterwards() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    s.text_input("exit 3\r");
    assert!(fx.rec.wait_for(TIMEOUT, is_closed));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        closed_events(&fx, id),
        vec![CloseReason::Exited { code: Some(3) }]
    );
    assert_eq!(
        s.state(),
        SessionState::Closed(CloseReason::Exited { code: Some(3) })
    );

    // 之后的输入全部丢弃：不 panic、不产生任何新事件、写通道已关闭。
    let before = fx.rec.all().len();
    let reads = s.read_stats();
    s.text_input("echo x\r");
    s.key(&plain(Key::Enter));
    s.paste(&PasteContent::Text("y".into()));
    s.paste(&PasteContent::Files(vec!["/tmp/a b".into()]));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fx.rec.all().len(), before);
    assert_eq!(s.read_stats(), reads);
    // 会话保留在管理器里（UI 还要显示最后的画面）。
    assert!(fx.mgr.get(id).is_some());
}

#[test]
fn signal_killed_shell_reports_code_none() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    s.text_input("kill -9 $$\r");
    assert!(fx.rec.wait_for(TIMEOUT, is_closed));
    assert_eq!(
        closed_events(&fx, id),
        vec![CloseReason::Exited { code: None }]
    );
}

#[test]
fn close_by_user_kills_process_and_emits_by_user_only() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let pid = s.pid().expect("pid");
    assert!(process_alive(pid));
    fx.mgr.close(id);
    assert!(fx.rec.wait_for(TIMEOUT, is_closed));
    // 事件在进程确实结束之后才发。
    assert!(
        !process_alive(pid),
        "收到 Closed(ByUser) 时 shell 必须已经结束"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(closed_events(&fx, id), vec![CloseReason::ByUser]);
    // 已从管理器移除；输入不再有任何效果。
    assert!(fx.mgr.get(id).is_none());
    s.text_input("echo x\r");
    // 重复关闭无事发生。
    fx.mgr.close(id);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(closed_events(&fx, id).len(), 1);
}

/// 回归：关闭时读线程必须继续排空 PTY，否则正在输出的 shell 退出时会卡在内核里，kill 永远不返回。
#[test]
fn close_while_output_is_flooding_does_not_hang() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let pid = s.pid().unwrap();
    s.text_input("yes\r");
    let deadline = Instant::now() + TIMEOUT;
    while s.read_stats().bytes < 200_000 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    fx.mgr.close(id);
    assert!(fx.rec.wait_for(TIMEOUT, is_closed));
    assert!(!process_alive(pid));
    assert_eq!(closed_events(&fx, id), vec![CloseReason::ByUser]);
}

#[test]
fn close_after_self_exit_does_not_emit_again() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    s.text_input("exit 0\r");
    assert!(fx.rec.wait_for(TIMEOUT, is_closed));
    fx.mgr.close(id);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        closed_events(&fx, id),
        vec![CloseReason::Exited { code: Some(0) }]
    );
}

/// 用户关闭与 shell 自己退出并发：无论谁赢，结束事件恰好一条，进程都已结束。
#[test]
fn close_racing_with_self_exit_emits_exactly_one_closed_event() {
    let fx = sh_fixture();
    for i in 0..40u64 {
        let (id, s) = fx.open();
        let pid = s.pid().unwrap();
        s.text_input("exit 5\r");
        // 0..~12 ms 的错开，覆盖"close 先到 / exit 先到 / 几乎同时"。
        std::thread::sleep(Duration::from_micros((i % 8) * 1500));
        fx.mgr.close(id);
        let deadline = Instant::now() + TIMEOUT;
        while closed_events(&fx, id).is_empty() {
            assert!(
                Instant::now() < deadline,
                "第 {i} 轮：没有任何结束事件 state={:?} alive={} events={:?}",
                s.state(),
                process_alive(pid),
                fx.rec.all().iter().rev().take(5).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // 等一段时间，确认没有第二条。
        std::thread::sleep(Duration::from_millis(60));
        let closed = closed_events(&fx, id);
        assert_eq!(closed.len(), 1, "第 {i} 轮：{closed:?}");
        assert!(
            matches!(
                closed[0],
                CloseReason::ByUser | CloseReason::Exited { code: Some(5) }
            ),
            "第 {i} 轮：{closed:?}"
        );
        assert!(!process_alive(pid), "第 {i} 轮：shell 没有结束");
    }
}

#[test]
fn shutdown_closes_every_session_within_timeout() {
    let fx = sh_fixture();
    let sessions: Vec<_> = (0..3).map(|_| fx.open()).collect();
    let pids: Vec<_> = sessions.iter().map(|(_, s)| s.pid().unwrap()).collect();
    let started = Instant::now();
    fx.mgr.shutdown(Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(5));
    for (id, _) in &sessions {
        assert_eq!(closed_events(&fx, *id), vec![CloseReason::ByUser]);
    }
    for pid in pids {
        assert!(!process_alive(pid));
    }
    // shutdown 之后不能再打开会话。
    assert!(
        fx.mgr
            .open(kanso_session::OpenRequest::DefaultLocalShell, GRID)
            .is_err()
    );
}

#[test]
fn wakeups_are_coalesced_and_rearmed_by_take_frame() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(fx.rec.wait_for(TIMEOUT, is_dirty));
    // 等启动输出（提示符）稳定后清掉脏标记。
    std::thread::sleep(Duration::from_millis(500));
    screen.refresh();
    let dirty_before = fx.rec.count(is_dirty);
    let stats_before = s.read_stats();

    // 大量输出期间不取帧：脏标记一直是真，事件只会在第一次变脏时发一次。
    s.text_input("yes hello | head -n 1000000; echo don$((1))e\r");
    let expected = stats_before.bytes + 6_000_000;
    let deadline = Instant::now() + TIMEOUT;
    while s.read_stats().bytes < expected {
        assert!(Instant::now() < deadline, "输出没有读完");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    let chunks = s.read_stats().chunks - stats_before.chunks;
    let events = fx.rec.count(is_dirty) - dirty_before;
    println!("读到 {chunks} 块，TerminalDirty {events} 次");
    assert!(events >= 1);
    assert!(
        chunks >= 20 * events as u64,
        "唤醒没有合并：{chunks} 块 / {events} 次事件"
    );

    // 取帧之后，新的输出会再次触发。
    assert!(screen.wait_contains("don1e"));
    let baseline = fx.rec.count(is_dirty);
    s.text_input("echo again$((1))\r");
    let deadline = Instant::now() + TIMEOUT;
    while fx.rec.count(is_dirty) <= baseline {
        assert!(
            Instant::now() < deadline,
            "取帧后没有再次收到 TerminalDirty"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(screen.wait_contains("again1"));
}

#[test]
fn dirty_events_never_exceed_frames_taken_plus_one_under_flood() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    std::thread::sleep(Duration::from_millis(300));
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let taker = {
        let (s, stop, frames) = (Arc::clone(&s), Arc::clone(&stop), Arc::clone(&frames));
        std::thread::spawn(move || {
            let mut reuse = None;
            while !stop.load(Ordering::Relaxed) {
                if let Some(f) = s.take_frame(&mut reuse) {
                    frames.fetch_add(1, Ordering::Relaxed);
                    reuse = Some(f);
                }
                std::thread::sleep(Duration::from_micros(8333)); // 120 Hz
            }
        })
    };
    let bytes_before = s.read_stats().bytes;
    s.text_input("yes hello | head -n 500000; echo fin$((1))ish\r");
    let deadline = Instant::now() + TIMEOUT;
    while s.read_stats().bytes < bytes_before + 3_000_000 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    taker.join().unwrap();
    let events = fx.rec.count(is_dirty) as u64;
    let frames = frames.load(Ordering::Relaxed);
    // 每个事件之前都必须被取帧清除过一次标志（首个事件除外）。
    assert!(events <= frames + 1, "{events} 次事件 / {frames} 帧");
}

#[test]
fn take_frame_keeps_reuse_when_there_is_no_new_frame() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(fx.rec.wait_for(TIMEOUT, is_dirty));
    std::thread::sleep(Duration::from_millis(500));
    screen.refresh();
    screen.refresh();

    let mut reuse = s.take_frame(&mut None);
    // 等启动输出静止：此后没有新输出，也就没有新帧。
    if reuse.is_none() {
        reuse = Some(kanso_frame::mock::mock_frame(80, 24, 1));
    }
    let before = reuse.as_ref().unwrap().as_ptr();
    assert!(s.take_frame(&mut reuse).is_none(), "没有变化时不应有新帧");
    assert_eq!(
        reuse.as_ref().map(Frame::as_ptr),
        Some(before),
        "没有新帧时 reuse 必须原样保留"
    );

    s.text_input("echo x$((1))\r");
    assert!(fx.rec.wait_for(TIMEOUT, |_| s.read_stats().bytes > 0));
    let deadline = Instant::now() + TIMEOUT;
    let frame = loop {
        if let Some(f) = s.take_frame(&mut reuse) {
            break f;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(reuse.is_none(), "有新帧时 reuse 必须被取走");
    drop(frame);
}

#[test]
fn resize_is_visible_to_the_shell() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    s.resize(
        GridSize {
            cols: 100,
            rows: 30,
        },
        8,
        16,
    );
    s.text_input("stty size\r");
    assert!(screen.wait_contains("30 100"), "{}", screen.text);
}

#[test]
fn ctrl_c_interrupts_foreground_command() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    s.text_input("echo ready$((1)); sleep 100\r");
    assert!(screen.wait_contains("ready1"));
    std::thread::sleep(Duration::from_millis(300));
    s.key(&ctrl("c"));
    s.text_input("echo st$?\r");
    assert!(screen.wait_contains("st130"), "{}", screen.text);
}

#[test]
fn arrow_key_is_encoded_and_reaches_the_program() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    s.text_input("cat -v\r");
    std::thread::sleep(Duration::from_millis(300));
    s.key(&plain(Key::ArrowUp));
    s.key(&plain(Key::Enter));
    // 默认（非 DECCKM）方向键是 ESC [ A，cat -v 把它显示成 ^[[A。
    assert!(screen.wait_contains("^[[A"), "{}", screen.text);
}

#[test]
fn terminal_reply_is_written_back_to_the_pty() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    // DA1 查询：终端内核回应 `ESC [ ? … c`，会话原样写回 PTY。tty 的行规程把收到的 ESC 回显成 `^[`，
    // 所以屏幕上出现 `^[[?` 就说明回应确实进了 PTY 的输入端。
    s.text_input("printf '\\033[c'; sleep 1\r");
    assert!(screen.wait_contains("^[[?"), "{}", screen.text);
}

#[test]
fn paste_text_and_files_are_quoted_for_posix_shell() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    let mut screen = Screen::new(&s);
    s.paste(&PasteContent::Files(vec![
        "/tmp/a b".into(),
        "/tmp/c".into(),
    ]));
    // 反斜杠转义空格，路径之间一个空格。
    assert!(screen.wait_contains("/tmp/a\\ b /tmp/c"), "{}", screen.text);
    s.paste(&PasteContent::ImageOnly); // 本 WP 不发送任何东西，不 panic。
}

#[test]
fn working_directory_follows_foreground_process() {
    let fx = sh_fixture();
    let (_id, s) = fx.open();
    s.text_input("cd /tmp\r");
    let want = std::fs::canonicalize("/tmp").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(p) = s.working_directory()
            && std::fs::canonicalize(&p).ok().as_deref() == Some(want.as_path())
        {
            break;
        }
        assert!(Instant::now() < deadline, "cwd 一直没变成 /tmp");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn duplicate_reuses_launch_and_unknown_source_is_rejected() {
    use kanso_session::{OpenError, OpenRequest};
    let fx = sh_fixture();
    let (id, _s) = fx.open();
    let dup = fx.mgr.open(OpenRequest::Duplicate(id), GRID).unwrap();
    assert_ne!(dup, id);
    assert_eq!(fx.mgr.get(dup).unwrap().launch().command_line, "/bin/sh -i");
    let missing = SessionId::from_raw(9999);
    assert!(matches!(
        fx.mgr.open(OpenRequest::Duplicate(missing), GRID),
        Err(OpenError::NoSuchSession(_))
    ));
    assert_eq!(fx.mgr.sessions().len(), 2);
}

/// 复制本地标签时，新会话从原会话当前所在的目录启动（ui-shell.md §3.3）。
#[test]
fn duplicate_starts_in_the_source_sessions_current_directory() {
    use kanso_session::OpenRequest;
    let fx = sh_fixture();
    let (id, s) = fx.open();
    s.text_input("cd /tmp\r");
    let want = std::fs::canonicalize("/tmp").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while s
        .working_directory()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .as_deref()
        != Some(want.as_path())
    {
        assert!(Instant::now() < deadline, "cwd 一直没变成 /tmp");
        std::thread::sleep(Duration::from_millis(20));
    }
    let dup = fx.mgr.open(OpenRequest::Duplicate(id), GRID).unwrap();
    let start = fx.mgr.get(dup).unwrap().launch().start_dir.clone().unwrap();
    assert_eq!(std::fs::canonicalize(start).unwrap(), want);
}

#[test]
fn spawn_failure_returns_error_without_events() {
    use kanso_session::{OpenError, OpenRequest};
    let fx = fixture("/nonexistent/shell-for-kanso-test", false);
    let r = fx.mgr.open(OpenRequest::DefaultLocalShell, GRID);
    assert!(matches!(r, Err(OpenError::Spawn { .. })), "{r:?}");
    assert!(fx.rec.all().is_empty());
    assert!(fx.mgr.sessions().is_empty());
}

// ---- zsh 端到端：临时 HOME，开启 shell 集成 ----

/// 等到满足条件的 `LocationChanged`。
fn wait_location(fx: &Fixture, pred: impl Fn(&Option<String>, &Option<String>) -> bool) -> bool {
    fx.rec.wait_for(TIMEOUT, |e| {
        matches!(e, CoreEvent::LocationChanged { cwd, git_branch, .. } if pred(cwd, git_branch))
    })
}

#[test]
fn zsh_integration_reports_location_activity_and_git_branch() {
    if !std::path::Path::new("/bin/zsh").exists() {
        eprintln!("没有 /bin/zsh，跳过");
        return;
    }
    let fx = fixture("/bin/zsh", true);
    let (id, s) = fx.open();
    let home = std::fs::canonicalize(&fx.home).unwrap();
    // 首个 LocationChanged：cwd 是临时 HOME（路径可能经过 /private 规范化，所以比较规范化之后的结果）。
    assert!(
        wait_location(&fx, |cwd, _| {
            cwd.as_deref()
                .and_then(|c| std::fs::canonicalize(c).ok())
                .as_deref()
                == Some(home.as_path())
        }),
        "{:?}",
        fx.rec.all()
    );

    // 假的 git 目录：.git/HEAD 指向一个分支。
    let proj = fx.home.join("proj");
    std::fs::create_dir_all(proj.join(".git")).unwrap();
    std::fs::write(proj.join(".git/HEAD"), "ref: refs/heads/fake-branch\n").unwrap();

    s.text_input("sleep 1\r");
    let running = CoreEvent::ActivityChanged {
        session: id,
        activity: Activity::CommandRunning,
    };
    assert!(fx.rec.wait_for(TIMEOUT, |e| *e == running));
    let idle_after_running = || {
        let events = fx.rec.all();
        let pos = events.iter().position(|e| *e == running);
        pos.is_some_and(|p| {
            events[p..].iter().any(|e| {
                *e == CoreEvent::ActivityChanged {
                    session: id,
                    activity: Activity::Idle,
                }
            })
        })
    };
    let deadline = Instant::now() + TIMEOUT;
    while !idle_after_running() {
        assert!(Instant::now() < deadline, "命令结束后没有回到 Idle");
        std::thread::sleep(Duration::from_millis(20));
    }

    s.text_input("cd proj\r");
    assert!(
        wait_location(&fx, |_, branch| branch.as_deref() == Some("fake-branch")),
        "{:?}",
        fx.rec.all()
    );
    // 可见时命令结束不发系统通知；不可见时发。
    assert!(fx.rec.finished.lock().unwrap().is_empty());
    s.set_visible(false);
    s.text_input("true\r");
    let deadline = Instant::now() + TIMEOUT;
    while fx.rec.finished.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "不可见时命令结束应当发通知");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fx.rec.finished.lock().unwrap()[0], (id, Some(0)));
}

// ---- 吞吐（手动运行：cargo test --release -p kanso-session -- --ignored --nocapture）----

#[test]
#[ignore = "吞吐测试，手动在 release 下运行"]
fn throughput_cat_200mb_with_120hz_frame_taker() {
    use std::io::Write;
    let fx = sh_fixture();
    let file = fx.tmp.path().join("big.txt");
    {
        let line =
            b"the quick brown fox jumps over the lazy dog 0123456789 \xe4\xb8\xad\xe6\x96\x87\n";
        let mut chunk = Vec::new();
        while chunk.len() < 1 << 20 {
            chunk.extend_from_slice(line);
        }
        let mut f = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
        let mut written = 0usize;
        while written < 200 * 1024 * 1024 {
            f.write_all(&chunk).unwrap();
            written += chunk.len();
        }
    }
    let size = std::fs::metadata(&file).unwrap().len();
    let (_id, s) = fx.open();
    std::thread::sleep(Duration::from_millis(500));

    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicU64::new(0));
    let seen_end = Arc::new(AtomicBool::new(false));
    let taker = {
        let (s, stop, frames, seen_end) = (
            Arc::clone(&s),
            Arc::clone(&stop),
            Arc::clone(&frames),
            Arc::clone(&seen_end),
        );
        std::thread::spawn(move || {
            let mut reuse: Option<Frame> = None;
            let mut next = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                if let Some(f) = s.take_frame(&mut reuse) {
                    frames.fetch_add(1, Ordering::Relaxed);
                    if frame_text(&f).contains("EN1D") {
                        seen_end.store(true, Ordering::Relaxed);
                    }
                    reuse = Some(f);
                }
                next += Duration::from_micros(8333);
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
        })
    };

    let bytes0 = s.read_stats().bytes;
    let t0 = Instant::now();
    s.text_input(&format!("cat '{}'; echo EN$((1))D\r", file.display()));
    while !seen_end.load(Ordering::Relaxed) {
        assert!(t0.elapsed() < Duration::from_secs(300), "超时");
        std::thread::sleep(Duration::from_millis(5));
    }
    let secs = t0.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    taker.join().unwrap();
    let st = s.read_stats();
    let mb = (st.bytes - bytes0) as f64 / (1024.0 * 1024.0);
    println!(
        "吞吐：文件 {} MiB，读到 {mb:.1} MiB，用时 {secs:.2} s，{:.1} MiB/s，读 {} 块（平均 {:.0} B/块），\
         120 Hz 取帧器共取到 {} 帧，TerminalDirty {} 次",
        size / (1024 * 1024),
        mb / secs,
        st.chunks,
        st.bytes as f64 / st.chunks.max(1) as f64,
        frames.load(Ordering::Relaxed),
        fx.rec.count(is_dirty),
    );
}
