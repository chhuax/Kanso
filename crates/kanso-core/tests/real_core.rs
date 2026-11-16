//! 真实 core 的集成测试：`Core::new_with_base_env` + 真实 PTY + `/bin/sh -i`。
//!
//! 隔离：数据目录、HOME 都是临时目录，子进程环境显式给定，不读用户真实的配置文件。
//! 本地会话的细节（并发结束、唤醒合并、zsh 集成、吞吐）在 `kanso-session` 的测试里；
//! 这里验证 `Core` 这一层的接线、契约与"未实现方法不 panic"。

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, Entitlement, OpenTarget, Platform,
    SearchQuery,
};
use kanso_frame::{CELL_WIDE_SPACER, Frame};
use kanso_types::event::{CloseReason, SessionState};
use kanso_types::transfer::ConflictPolicy;
use kanso_types::{
    CoreEvent, GridSize, KeyEvent, KeyEventKind, Message, Modifiers, PasteContent, ProfileId,
    SessionId, TimestampMode,
};

const TIMEOUT: Duration = Duration::from_secs(20);
const GRID: GridSize = GridSize { cols: 80, rows: 24 };

struct Fixture {
    _tmp: tempfile::TempDir,
    core: Core,
    sink: Arc<CollectingEventSink>,
    log: std::sync::Mutex<Vec<CoreEvent>>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let sink = Arc::new(CollectingEventSink::new());
    let mut config = CoreConfig::new(tmp.path().join("data/nested"), "0.0.0-test", "test.kanso");
    // 带参数的命令行不会被加 `-l`，所以不读 /etc/profile 与 ~/.profile。
    config.default_shell = Some("/bin/sh -i".to_owned());
    let core = Core::new_with_base_env(
        config,
        Platform::for_tests(sink.clone()),
        vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("HOME".into(), home.to_string_lossy().into_owned()),
        ],
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        core,
        sink,
        log: Default::default(),
    }
}

impl Fixture {
    fn events(&self) -> Vec<CoreEvent> {
        let mut log = self.log.lock().unwrap();
        log.extend(self.sink.take());
        log.clone()
    }

    fn wait_event(&self, pred: impl Fn(&CoreEvent) -> bool) -> bool {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if self.events().iter().any(&pred) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn open(&self) -> SessionId {
        self.core
            .open_session(OpenTarget::DefaultLocalShell, GRID)
            .unwrap()
    }

    /// 轮询取帧直到屏幕文本满足条件。
    fn wait_screen(&self, id: SessionId, needle: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        let mut reuse: Option<Frame> = None;
        let mut text = String::new();
        loop {
            if let Some(f) = self.core.take_frame(id, &mut reuse) {
                text = frame_text(&f);
                reuse = Some(f);
            }
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "屏幕上一直没有 {needle:?}：\n{text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn frame_text(frame: &Frame) -> String {
    let view = frame.view().unwrap();
    let mut out = String::new();
    for r in 0..view.header().rows {
        for cell in view.row(r) {
            if cell.flags & CELL_WIDE_SPACER != 0 {
                continue;
            }
            out.push(if cell.ch == 0 {
                ' '
            } else {
                char::from_u32(cell.ch).unwrap_or('?')
            });
        }
        out.push('\n');
    }
    out
}

#[test]
fn completion_switch_controls_live_history_collection() {
    let fixture = fixture();
    let session = fixture.open();
    fixture.wait_screen(session, "$");
    let database =
        kanso_store::Store::open(&fixture._tmp.path().join("data/nested/kanso.db")).unwrap();
    fixture
        .core
        .text_input(session, "echo F3_DISABLED\r")
        .unwrap();
    fixture.wait_screen(session, "F3_DISABLED");
    std::thread::sleep(Duration::from_millis(300));
    assert!(database.history("", 5000).unwrap().is_empty());
    assert!(fixture.core.completion_state(session).is_none());
    fixture
        .core
        .set_setting("completion", r#"{"enabled":true}"#.into())
        .unwrap();
    fixture
        .core
        .text_input(session, "echo F3_ENABLED\r")
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while database.history("", 5000).unwrap().is_empty() {
        assert!(Instant::now() < deadline, "开启后没有采集历史");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        database.history("", 5000).unwrap()[0].command,
        "echo F3_ENABLED"
    );
    fixture
        .core
        .set_setting("completion", r#"{"enabled":false}"#.into())
        .unwrap();
    fixture
        .core
        .text_input(session, "echo F3_OFF_AGAIN\r")
        .unwrap();
    fixture.wait_screen(session, "F3_OFF_AGAIN");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(database.history("", 5000).unwrap().len(), 1);
    fixture.core.clear_history().unwrap();
    assert!(database.history("", 5000).unwrap().is_empty());
    fixture.core.close_session(session);
}

fn zmodem_program(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("KANSO_LRZSZ_BIN")
        .into_iter()
        .map(std::path::PathBuf::from)
        .chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ))
        .collect::<Vec<_>>();
    for directory in paths {
        for candidate in [name.to_string(), format!("l{name}")] {
            let path = directory.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    eprintln!("跳过 core ZMODEM 端到端测试：找不到 {name}，可用 KANSO_LRZSZ_BIN 指定目录");
    None
}

#[test]
fn zmodem_local_shell_receive_three_files_skip_second() {
    receive_three_files(false);
}

#[test]
fn xmodem_local_shell_dialog_input_exclusion_file_and_prompt() {
    use kanso_types::{PromptRequest, PromptResponse};
    let fixture = fixture();
    let id = fixture.open();
    fixture.wait_screen(id, "$ ");
    fixture.core.text_input(id, "stty raw -echo; printf '\\130\\115\\117\\104\\105\\115\\137\\122\\105\\101\\104\\131'; request=$(dd bs=1 count=1 2>/dev/null); if [ \"$request\" = C ]; then printf '\\001\\001\\376'; dd if=/dev/zero bs=128 count=1 2>/dev/null; printf '\\000\\000'; dd bs=1 count=1 of=/dev/null 2>/dev/null; printf '\\004'; dd bs=1 count=1 of=/dev/null 2>/dev/null; fi; stty sane; printf '\\130\\115\\117\\104\\105\\115\\137\\104\\117\\116\\105\\n'\n").unwrap();
    fixture.wait_screen(id, "XMODEM_READY");
    let transfer = fixture.core.xmodem_start(id, None).unwrap();
    assert!(fixture.wait_event(|event| matches!(
        event,
        CoreEvent::Prompt {
            request: PromptRequest::XmodemSaveFile,
            ..
        }
    )));
    assert!(fixture.core.completion_state(id).is_none());
    fixture.core.text_input(id, "MUST_NOT_SEND").unwrap();
    fixture
        .core
        .paste(id, PasteContent::Text("MUST_NOT_PASTE".into()))
        .unwrap();
    let prompt = fixture
        .events()
        .iter()
        .find_map(|event| match event {
            CoreEvent::Prompt {
                prompt,
                request: PromptRequest::XmodemSaveFile,
                ..
            } => Some(*prompt),
            _ => None,
        })
        .unwrap();
    let path = fixture._tmp.path().join("xmodem.bin");
    fixture.core.respond_prompt(
        prompt,
        PromptResponse::SavePath(path.to_string_lossy().into_owned()),
    );
    assert!(fixture.wait_event(|event| matches!(event, CoreEvent::TransferFinished { transfer: actual, outcome: kanso_types::event::TransferOutcome::Completed { .. } } if *actual == transfer)));
    assert_eq!(std::fs::read(path).unwrap(), vec![0; 128]);
    fixture.wait_screen(id, "XMODEM_DONE");
    fixture.core.close_session(id);
}

#[test]
fn zmodem_local_shell_save_failure_skips_and_keeps_first_error() {
    receive_three_files(true);
}

fn receive_three_files(save_failure: bool) {
    use kanso_types::{PromptRequest, PromptResponse};
    let Some(program) = zmodem_program("sz") else {
        return;
    };
    let fixture = fixture();
    let source = fixture._tmp.path().join("source");
    let destination = fixture._tmp.path().join("destination");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    let content: Vec<u8> = (0..123_456).map(|index| index as u8).collect();
    for name in ["a.bin", "b.bin", "c.bin"] {
        std::fs::write(source.join(name), &content).unwrap();
    }
    let session = fixture.open();
    fixture
        .core
        .text_input(
            session,
            &format!(
                "stty -echo; '{}' -b '{}' '{}' '{}'; printf '\\nAFTER_ZMODEM\\n'\n",
                program.display(),
                source.join("a.bin").display(),
                source.join("b.bin").display(),
                source.join("c.bin").display()
            ),
        )
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut answered = std::collections::HashSet::new();
    loop {
        let events = fixture.events();
        for event in &events {
            if let CoreEvent::Prompt {
                prompt,
                request: PromptRequest::ZmodemSaveFile { suggested_name, .. },
                ..
            } = event
                && answered.insert(*prompt)
            {
                fixture.core.respond_prompt(
                    *prompt,
                    if suggested_name == "b.bin" {
                        if save_failure {
                            PromptResponse::SavePath(destination.to_string_lossy().into_owned())
                        } else {
                            PromptResponse::Cancel
                        }
                    } else {
                        PromptResponse::SavePath(
                            destination
                                .join(suggested_name)
                                .to_string_lossy()
                                .into_owned(),
                        )
                    },
                );
            }
        }
        if let Some(CoreEvent::TransferFinished { outcome, .. }) = events
            .iter()
            .find(|event| matches!(event, CoreEvent::TransferFinished { .. }))
        {
            if save_failure {
                assert!(
                    matches!(
                        outcome,
                        kanso_types::event::TransferOutcome::Failed {
                            message: Message::XferLocalWriteFailed { .. },
                            ..
                        }
                    ),
                    "{outcome:?}"
                );
            } else {
                assert!(
                    matches!(
                        outcome,
                        kanso_types::event::TransferOutcome::Completed { skipped: 1 }
                    ),
                    "{outcome:?}"
                );
            }
            break;
        }
        assert!(Instant::now() < deadline, "{events:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(answered.len(), 3);
    for name in ["a.bin", "c.bin"] {
        assert_eq!(std::fs::read(destination.join(name)).unwrap(), content);
    }
    assert!(!destination.join("b.bin").exists());
    let screen = fixture.wait_screen(session, "AFTER_ZMODEM");
    assert!(!screen.contains("**B0000"), "{screen}");
    fixture.core.shutdown();
}

#[test]
fn zmodem_local_shell_send_multiple_files() {
    use kanso_types::{PromptRequest, PromptResponse};
    let Some(program) = zmodem_program("rz") else {
        return;
    };
    let fixture = fixture();
    let destination = fixture._tmp.path().join("received");
    std::fs::create_dir(&destination).unwrap();
    let content: Vec<u8> = (0..72_345).map(|index| index as u8).collect();
    let paths: Vec<_> = ["a.bin", "b.bin"]
        .iter()
        .map(|name| {
            let path = fixture._tmp.path().join(name);
            std::fs::write(&path, &content).unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect();
    let session = fixture.open();
    fixture
        .core
        .text_input(
            session,
            &format!(
                "stty -echo; cd '{}'; '{}' -b -y; printf '\\nSENT_ZMODEM\\n'\n",
                destination.display(),
                program.display()
            ),
        )
        .unwrap();
    assert!(fixture.wait_event(|event| matches!(
        event,
        CoreEvent::Prompt {
            request: PromptRequest::ZmodemPickFiles,
            ..
        }
    )));
    let prompt = fixture
        .events()
        .iter()
        .find_map(|event| match event {
            CoreEvent::Prompt {
                prompt,
                request: PromptRequest::ZmodemPickFiles,
                ..
            } => Some(*prompt),
            _ => None,
        })
        .unwrap();
    fixture
        .core
        .text_input(session, "INPUT_MUST_BE_BLOCKED\n")
        .unwrap();
    fixture
        .core
        .paste(
            session,
            PasteContent::Text("PASTE_MUST_BE_BLOCKED\n".into()),
        )
        .unwrap();
    fixture
        .core
        .respond_prompt(prompt, PromptResponse::Files(paths));
    assert!(
        fixture.wait_event(|event| matches!(event, CoreEvent::TransferFinished { .. })),
        "{:?}",
        fixture.events()
    );
    assert!(
        fixture.events().iter().any(|event| matches!(
            event,
            CoreEvent::TransferFinished {
                outcome: kanso_types::event::TransferOutcome::Completed { skipped: 0 },
                ..
            }
        )),
        "{:?}",
        fixture.events()
    );
    for name in ["a.bin", "b.bin"] {
        assert_eq!(std::fs::read(destination.join(name)).unwrap(), content);
    }
    fixture.wait_screen(session, "SENT_ZMODEM");
    fixture.core.shutdown();
}

#[test]
fn zmodem_session_close_fails_and_dismisses_prompt_immediately() {
    use kanso_types::PromptRequest;
    let Some(program) = zmodem_program("rz") else {
        return;
    };
    let fixture = fixture();
    let session = fixture.open();
    fixture
        .core
        .text_input(session, &format!("'{}' -b\n", program.display()))
        .unwrap();
    assert!(fixture.wait_event(|event| matches!(
        event,
        CoreEvent::Prompt {
            request: PromptRequest::ZmodemPickFiles,
            ..
        }
    )));
    fixture.core.close_session(session);
    assert!(fixture.wait_event(|event| matches!(
        event,
        CoreEvent::TransferFinished {
            outcome: kanso_types::event::TransferOutcome::Failed {
                message: Message::XferSessionEnded {},
                ..
            },
            ..
        }
    )));
    assert!(
        fixture
            .events()
            .iter()
            .any(|event| matches!(event, CoreEvent::PromptDismissed { .. }))
    );
    fixture.core.shutdown();
}

fn closed(e: &CoreEvent) -> Option<&CloseReason> {
    match e {
        CoreEvent::SessionStateChanged {
            state: SessionState::Closed(r),
            ..
        } => Some(r),
        _ => None,
    }
}

#[test]
fn new_creates_missing_data_dir() {
    let fx = fixture();
    // `data/nested` 在 fixture 里不存在，由 core 创建。
    assert!(fx._tmp.path().join("data/nested").is_dir());
}

#[test]
fn new_fails_with_internal_error_when_data_dir_is_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("occupied");
    std::fs::write(&file, b"x").unwrap();
    let sink = Arc::new(CollectingEventSink::new());
    let r = Core::new(
        CoreConfig::new(file.join("sub"), "0", "t"),
        Platform::for_tests(sink),
    );
    assert!(matches!(r, Err(CoreError::Internal { .. })));
}

#[test]
fn open_emits_connecting_connected_and_text_reaches_frame() {
    let fx = fixture();
    let id = fx.open();
    let events = fx.events();
    assert_eq!(
        &events[..2],
        &[
            CoreEvent::SessionStateChanged {
                session: id,
                state: SessionState::Connecting
            },
            CoreEvent::SessionStateChanged {
                session: id,
                state: SessionState::Connected
            },
        ]
    );
    fx.core.text_input(id, "echo hello\r").unwrap();
    fx.wait_screen(id, "hello");
    fx.core.text_input(id, "echo he$((1+1))llo\r").unwrap();
    fx.wait_screen(id, "he2llo");
    let info = fx.core.session_info(id).unwrap();
    assert_eq!(info.state, SessionState::Connected);
    assert_eq!(fx.core.sessions().len(), 1);
}

#[test]
fn take_frame_contract_reuse_kept_without_new_frame_and_taken_with_one() {
    let fx = fixture();
    let id = fx.open();
    fx.wait_screen(id, "$");
    std::thread::sleep(Duration::from_millis(500));
    // 清掉脏标记。
    let mut reuse = fx.core.take_frame(id, &mut None);
    if reuse.is_none() {
        reuse = Some(kanso_frame::mock::mock_frame(80, 24, 1));
    }
    let ptr = reuse.as_ref().unwrap().as_ptr();
    assert!(fx.core.take_frame(id, &mut reuse).is_none());
    assert_eq!(
        reuse.as_ref().map(Frame::as_ptr),
        Some(ptr),
        "无新帧时 reuse 必须原样保留"
    );

    fx.core.text_input(id, "echo z$((1))\r").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if fx.core.take_frame(id, &mut reuse).is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(reuse.is_none(), "有新帧时 reuse 必须被取走");
    // 不存在的会话：None，reuse 不动。
    let mut keep = Some(kanso_frame::mock::mock_frame(80, 24, 2));
    assert!(
        fx.core
            .take_frame(SessionId::from_raw(777), &mut keep)
            .is_none()
    );
    assert!(keep.is_some());
}

#[test]
fn shell_exit_reports_code_once_then_input_is_ok_and_dropped() {
    let fx = fixture();
    let id = fx.open();
    fx.core.text_input(id, "exit 3\r").unwrap();
    assert!(fx.wait_event(|e| closed(e).is_some()));
    std::thread::sleep(Duration::from_millis(400));
    let reasons: Vec<_> = fx
        .events()
        .iter()
        .filter_map(|e| closed(e).cloned())
        .collect();
    assert_eq!(reasons, vec![CloseReason::Exited { code: Some(3) }]);
    assert_eq!(
        fx.core.session_info(id).unwrap().state,
        SessionState::Closed(CloseReason::Exited { code: Some(3) })
    );
    // 之后输入返回 Ok 并被丢弃。
    fx.core.text_input(id, "echo x\r").unwrap();
    fx.core
        .key(
            id,
            KeyEvent {
                key: kanso_types::Key::Enter,
                mods: Modifiers::NONE,
                text: None,
                kind: KeyEventKind::Press,
            },
        )
        .unwrap();
    fx.core.paste(id, PasteContent::Text("x".into())).unwrap();
    // 会话没了的 id 才是 NoSuchSession。
    fx.core.close_session(id);
    assert_eq!(
        fx.core.text_input(id, "x"),
        Err(CoreError::NoSuchSession(id))
    );
}

#[test]
fn close_session_emits_by_user_only_and_shutdown_is_idempotent() {
    let fx = fixture();
    let id = fx.open();
    fx.core.close_session(id);
    assert!(fx.wait_event(|e| closed(e).is_some()));
    std::thread::sleep(Duration::from_millis(400));
    let reasons: Vec<_> = fx
        .events()
        .iter()
        .filter_map(|e| closed(e).cloned())
        .collect();
    assert_eq!(reasons, vec![CloseReason::ByUser]);
    assert!(fx.core.sessions().is_empty());

    let other = fx.open();
    fx.core.shutdown();
    fx.core.shutdown();
    assert!(fx.wait_event(|e| matches!(
        e,
        CoreEvent::SessionStateChanged { session, state: SessionState::Closed(CloseReason::ByUser) } if *session == other
    )));
    // shutdown 之后不能再开会话，但也不能 panic。
    assert!(
        fx.core
            .open_session(OpenTarget::DefaultLocalShell, GRID)
            .is_err()
    );
}

#[test]
fn resize_reaches_the_shell() {
    let fx = fixture();
    let id = fx.open();
    fx.core
        .resize(
            id,
            GridSize {
                cols: 100,
                rows: 30,
            },
            8,
            16,
        )
        .unwrap();
    fx.core.text_input(id, "stty size\r").unwrap();
    fx.wait_screen(id, "30 100");
}

#[test]
fn working_directory_uses_foreground_process_cwd() {
    let fx = fixture();
    let id = fx.open();
    fx.core.text_input(id, "cd /tmp\r").unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let want = std::fs::canonicalize("/tmp").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(p) = rt.block_on(fx.core.working_directory(id))
            && std::fs::canonicalize(&p).ok().as_deref() == Some(want.as_path())
        {
            break;
        }
        assert!(Instant::now() < deadline, "cwd 一直没变成 /tmp");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        rt.block_on(fx.core.working_directory(SessionId::from_raw(4242))),
        Err(CoreError::NoSuchSession(SessionId::from_raw(4242)))
    );
}

#[test]
fn selection_search_and_timestamp_mode_delegate_to_the_terminal() {
    let fx = fixture();
    let id = fx.open();
    fx.core.text_input(id, "echo sea$((1))rchme\r").unwrap();
    fx.wait_screen(id, "sea1rchme");
    assert!(!fx.core.has_selection(id));
    fx.core.select_all(id).unwrap();
    assert!(fx.core.has_selection(id));
    assert!(fx.core.selection_text(id).unwrap().contains("sea1rchme"));
    fx.core.clear_selection(id).unwrap();
    assert!(!fx.core.has_selection(id));
    let st = fx
        .core
        .search(
            id,
            SearchQuery {
                text: "sea1rchme".into(),
                case_sensitive: true,
                regex: false,
            },
        )
        .unwrap();
    assert_eq!(st.total, 1);
    fx.core.search_close(id);
    fx.core.set_timestamp_mode(Some(id), TimestampMode::Off);
    fx.core.set_timestamp_mode(None, TimestampMode::Time);
    fx.core.clear_buffer(id).unwrap();
    fx.core.set_visible(id, false);
}

#[test]
fn terminal_options_apply_to_existing_and_new_sessions() {
    use kanso_frame::{CURSOR_BAR, FRAME_CURSOR_BLINK};
    use kanso_types::{CursorShape, TerminalOptions};

    let fx = fixture();
    let existing = fx.open();
    fx.core.text_input(existing, "echo seed\r").unwrap();
    fx.wait_screen(existing, "seed");

    fx.core.set_terminal_options(TerminalOptions {
        scrollback_lines: 123,
        cursor_shape: CursorShape::Bar,
        cursor_blink: true,
        semantic_coloring: true,
    });
    // 新开的会话在第一帧起就使用当前选项。
    let fresh = fx.open();
    fx.core.text_input(fresh, "echo fresh\r").unwrap();
    fx.wait_screen(fresh, "fresh");

    for id in [existing, fresh] {
        // 失焦时块状光标会画成空心块；竖线不受影响，所以先聚焦再断言。
        fx.core.focus(id, true).unwrap();
        let mut reuse = None;
        let frame = loop {
            if let Some(f) = fx.core.take_frame(id, &mut reuse) {
                break f;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let header = *frame.view().unwrap().header();
        assert_eq!(header.cursor_shape, CURSOR_BAR);
        assert_ne!(header.flags & FRAME_CURSOR_BLINK, 0);
    }
}

#[test]
fn duplicate_starts_a_second_session_and_unknown_profile_is_reported() {
    let fx = fixture();
    let id = fx.open();
    let dup = fx
        .core
        .open_session(OpenTarget::Duplicate(id), GRID)
        .unwrap();
    assert_ne!(dup, id);
    assert_eq!(
        fx.core
            .open_session(OpenTarget::Duplicate(SessionId::from_raw(55)), GRID),
        Err(CoreError::NoSuchSession(SessionId::from_raw(55)))
    );
    let ghost = ProfileId::new();
    assert_eq!(
        fx.core.open_session(OpenTarget::Profile(ghost), GRID),
        Err(CoreError::NoSuchProfile(ghost))
    );
}

#[test]
fn spawn_failure_is_a_user_error_without_events() {
    let tmp = tempfile::tempdir().unwrap();
    let sink = Arc::new(CollectingEventSink::new());
    let mut config = CoreConfig::new(tmp.path().join("d"), "0", "t");
    config.default_shell = Some("/nonexistent/kanso-shell".into());
    let core = Core::new_with_base_env(
        config,
        Platform::for_tests(sink.clone()),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
    )
    .unwrap();
    let err = core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap_err();
    assert!(err.user_message().is_some(), "{err:?}");
    assert!(sink.take().is_empty());
}

/// 尚未实现的方法返回 `NotSupported` / 空 / `None`，绝不 panic（panic 穿过 FFI 会让整个 App 崩溃）。
#[test]
fn unimplemented_methods_do_not_panic() {
    let fx = fixture();
    let c = &fx.core;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let local = fx.open();
    let ghost = SessionId::from_raw(31337);

    // T8：存储已实现，行为见 real_store.rs；这里只确认空库上不会 panic。
    assert!(c.profiles().is_empty());
    assert!(c.groups().is_empty());
    assert!(c.snippets().is_empty());
    assert_eq!(c.clear_history(), Ok(()));
    let ghost_group = kanso_types::GroupId::new();
    assert_eq!(
        c.group_delete_impact(ghost_group),
        Err(CoreError::NoSuchGroup(ghost_group))
    );
    let ghost_profile = ProfileId::new();
    assert_eq!(
        rt.block_on(c.delete_profile(ghost_profile)),
        Err(CoreError::NoSuchProfile(ghost_profile))
    );
    // I2 / T7b / F4：SSH、SFTP、终端内传输。本地会话 NotSupported，不存在的会话 NoSuchSession。
    c.respond_prompt(
        kanso_types::PromptId::from_raw(1),
        kanso_types::PromptResponse::Cancel,
    );
    assert_eq!(
        rt.block_on(c.sftp_home(local)),
        Err(CoreError::NotSupported)
    );
    assert_eq!(
        rt.block_on(c.sftp_home(ghost)),
        Err(CoreError::NoSuchSession(ghost))
    );
    assert!(matches!(
        c.sftp_list(local, "/"),
        Err(CoreError::NotSupported)
    ));
    assert_eq!(
        rt.block_on(c.sftp_stat(local, "/a")),
        Err(CoreError::NotSupported)
    );
    assert_eq!(
        rt.block_on(c.sftp_stat(ghost, "/a")),
        Err(CoreError::NoSuchSession(ghost))
    );
    assert_eq!(
        c.download(local, "/a", "/b", ConflictPolicy::Fail),
        Err(CoreError::NotSupported)
    );
    assert_eq!(
        c.xmodem_receive(ghost, "unused"),
        Err(CoreError::NoSuchSession(ghost))
    );
    c.cancel_transfer(kanso_types::TransferId::from_raw(1));
    c.cancel_terminal_transfer(local);
    // F6：OpenSSH 配置导入已实现，行为见 real_sshconfig.rs；这里只确认显式给不存在的文件会报错而不是 NotSupported。
    assert!(matches!(
        rt.block_on(c.scan_ssh_config(Some("/nonexistent/kanso-test/ssh_config"))),
        Err(CoreError::User(Message::ImportReadFailed { .. }))
    ));
    // T11 / I5：授权。
    assert_eq!(c.entitlement(), Entitlement::for_channel());
    assert_eq!(
        rt.block_on(c.activate_license("k")),
        Err(CoreError::NotSupported)
    );
    assert_eq!(rt.block_on(c.purchase()), Err(CoreError::NotSupported));
    assert_eq!(
        rt.block_on(c.restore_purchases()),
        Err(CoreError::NotSupported)
    );
}
