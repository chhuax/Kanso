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
    fixture_with_env(&[])
}

/// 同 [`fixture`]，另给子进程加几个环境变量（MCP 测试用 `PS1` 固定提示符）。
fn fixture_with_env(extra_env: &[(&str, &str)]) -> Fixture {
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
        [
            (
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            ),
            ("HOME".to_owned(), home.to_string_lossy().into_owned()),
        ]
        .into_iter()
        .chain(
            extra_env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        )
        .collect::<Vec<_>>(),
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        core,
        sink,
        log: Default::default(),
    }
}

#[test]
fn local_launch_override_does_not_change_defaults_or_create_profiles() {
    use kanso_types::profile::LocalProfile;

    let fixture = fixture();
    let defaults = LocalProfile {
        shell_command: "/bin/sh -c 'echo default-shell-marker; read value'".into(),
        ..LocalProfile::default()
    };
    fixture.core.set_local_shell_defaults(&defaults).unwrap();
    let temporary = LocalProfile {
        shell_command: "/bin/sh -c 'echo temporary-shell-marker; read value'".into(),
        ..LocalProfile::default()
    };
    let id = fixture
        .core
        .open_session(OpenTarget::LocalShell(temporary), GRID)
        .unwrap();
    assert!(
        fixture
            .wait_screen(id, "temporary-shell-marker")
            .contains("temporary-shell-marker")
    );
    assert_eq!(fixture.core.local_shell_defaults(), defaults);
    let id = fixture.open();
    assert!(
        fixture
            .wait_screen(id, "default-shell-marker")
            .contains("default-shell-marker")
    );
    assert!(fixture.core.profiles().is_empty());
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
    assert!(!st.invalid_regex);
    let invalid = fx
        .core
        .search(
            id,
            SearchQuery {
                text: "[".into(),
                case_sensitive: false,
                regex: true,
            },
        )
        .unwrap();
    assert!(invalid.invalid_regex);
    assert!(fx.core.search_status(id).unwrap().invalid_regex);
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

// ---- MCP：终端状态、读取、命令执行与输入（F9 阶段 2，spec/mcp.md §3.2、§3.3、§4.2、§4.3、§5）----

use kanso_core::mcp::{
    MCP_TEXT_LIMIT, McpInput, TerminalBusy, TerminalRunOutcome, TerminalSendOutcome, TextScope,
    WaitLimits,
};
use kanso_types::event::Activity;

/// 无 OSC 133 的固定提示符。子进程从环境继承 `PS1`（交互式 sh 不会覆盖已有的 PS1）。
const PLAIN_PS1: &str = "K> ";
/// 带 OSC 133 A / B 的固定提示符。`\x01` / `\x02` 是 readline 的"不可见字符"标记，
/// 不加的话 readline 会把转义序列算进提示符宽度。C / D 由测试命令自己输出。
const OSC_PS1: &str = "\x01\x1b]133;A\x07\x02K> \x01\x1b]133;B\x07\x02";

fn mcp_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn limits(timeout_ms: u64) -> WaitLimits {
    WaitLimits {
        timeout: Duration::from_millis(timeout_ms),
        max_bytes: MCP_TEXT_LIMIT,
    }
}

fn key(key: kanso_types::Key, ctrl: bool) -> KeyEvent {
    KeyEvent {
        key,
        mods: Modifiers {
            ctrl,
            ..Modifiers::NONE
        },
        text: None,
        kind: KeyEventKind::Press,
    }
}

fn screen_lines(fx: &Fixture, id: SessionId) -> Vec<String> {
    fx.core
        .terminal_text(id, TextScope::Screen, MCP_TEXT_LIMIT)
        .unwrap()
        .content
        .text
        .lines()
        .map(str::to_owned)
        .collect()
}

impl Fixture {
    /// 打开会话并等到停在提示符上（最后一行就是 `K>`，且没有命令在运行）。
    fn open_at_prompt(&self) -> SessionId {
        let id = self.open();
        self.wait_prompt(id);
        id
    }

    fn wait_prompt(&self, id: SessionId) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let at_prompt = screen_lines(self, id).last().map(String::as_str) == Some("K>");
            let idle = self
                .core
                .mcp_session_status(id)
                .is_some_and(|s| s.activity == Activity::Idle);
            if at_prompt && idle {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "一直没有回到提示符：{:?}",
                screen_lines(self, id)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn mcp_status_of_a_local_session_has_tty_and_shell_pid() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let status = fx.core.mcp_session_status(id).unwrap();
    assert_eq!(status.info.id, id);
    assert_eq!(status.activity, Activity::Idle);
    assert!(!status.locked);
    assert!(!status.terminal_transfer);
    assert!(!status.headless);
    assert!(!status.osc133, "没有 OSC 133 集成");
    let tty = status.local_tty.expect("本地会话有 PTY 从设备路径");
    assert!(tty.starts_with("/dev/"), "{tty}");
    assert!(status.shell_pid.is_some_and(|pid| pid > 1));
    assert_eq!(fx.core.mcp_session_status(SessionId::from_raw(31337)), None);
}

#[test]
fn mcp_status_reports_osc133_and_locked_after_exit() {
    let fx = fixture_with_env(&[("PS1", OSC_PS1)]);
    let id = fx.open_at_prompt();
    assert!(fx.core.mcp_session_status(id).unwrap().osc133);
    fx.core.text_input(id, "exit\r").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !fx.core.mcp_session_status(id).unwrap().locked {
        assert!(Instant::now() < deadline, "会话没有结束");
        std::thread::sleep(Duration::from_millis(10));
    }
    // 结束后锁定：写操作按未连接拒绝。
    let rt = mcp_rt();
    assert_eq!(
        rt.block_on(fx.core.mcp_terminal_run(id, "echo x", limits(1000)))
            .unwrap(),
        TerminalRunOutcome::Rejected(TerminalBusy::NotConnected)
    );
    assert_eq!(
        rt.block_on(
            fx.core
                .mcp_terminal_send(id, vec![McpInput::Text("x".into())], None, 100)
        )
        .unwrap(),
        TerminalSendOutcome::Rejected(TerminalBusy::NotConnected)
    );
}

#[test]
fn terminal_text_screen_and_recent_soft_wrap_trailing_spaces_and_wide_chars() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    fx.core
        .text_input(
            id,
            "printf 'W%0100d\\n' 0; printf 'TRAILSP   \\n'; printf '中文\\n'\r",
        )
        .unwrap();
    // 命令跑完、提示符回来。
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if screen_lines(&fx, id).iter().any(|l| l == "中文")
            && screen_lines(&fx, id).last().map(String::as_str) == Some("K>")
        {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", screen_lines(&fx, id));
        std::thread::sleep(Duration::from_millis(10));
    }
    let text = fx
        .core
        .terminal_text(id, TextScope::Screen, MCP_TEXT_LIMIT)
        .unwrap();
    assert!(!text.content.truncated);
    assert_eq!((text.rows, text.cols), (GRID.rows, GRID.cols));
    assert!(!text.alternate_screen);
    let lines: Vec<&str> = text.content.text.lines().collect();
    // 101 个字符在 80 列上软换行，应拼回一整行。
    let wrapped = format!("W{}", "0".repeat(100));
    assert!(lines.iter().any(|l| *l == wrapped), "{lines:?}");
    // 行尾空格去掉；宽字符只出现一次。
    assert!(lines.contains(&"TRAILSP"), "{lines:?}");
    assert!(lines.contains(&"中文"), "{lines:?}");
    // 光标在最后一行（提示符）的 "K> " 之后。光标行是**屏幕坐标**：软换行的那一行占两个屏幕行，
    // 所以比逻辑行数组的下标大 1。
    assert_eq!(usize::from(text.cursor.unwrap().0), lines.len());
    assert_eq!(text.cursor.unwrap().1, 3);
}

#[test]
fn terminal_text_recent_reaches_into_scrollback_and_truncation_keeps_the_end() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    fx.core
        .text_input(
            id,
            "i=0; while [ $i -lt 60 ]; do echo ROW$i; i=$((i+1)); done\r",
        )
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !screen_lines(&fx, id).iter().any(|l| l == "ROW59") {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    fx.wait_prompt(id);
    let screen = screen_lines(&fx, id);
    assert!(!screen.iter().any(|l| l == "ROW0"), "屏幕只有 24 行");
    let recent = fx
        .core
        .terminal_text(id, TextScope::Recent { lines: 500 }, MCP_TEXT_LIMIT)
        .unwrap();
    let lines: Vec<&str> = recent.content.text.lines().collect();
    assert!(lines.contains(&"ROW0"), "回滚里应有最早的输出");
    assert_eq!(lines.last(), Some(&"K>"));
    // 只取最后 3 个逻辑行。
    let last3 = fx
        .core
        .terminal_text(id, TextScope::Recent { lines: 3 }, MCP_TEXT_LIMIT)
        .unwrap();
    assert_eq!(last3.content.text, "ROW58\nROW59\nK>");
    // 超出上限保留末尾，并如实给出原始大小。
    let cut = fx
        .core
        .terminal_text(id, TextScope::Recent { lines: 500 }, 20)
        .unwrap();
    assert!(cut.content.truncated);
    assert_eq!(cut.content.original_bytes, recent.content.text.len() as u64);
    assert!(cut.content.text.ends_with("ROW59\nK>"));
}

#[test]
fn terminal_text_unknown_session_is_no_such_session() {
    let fx = fixture();
    let ghost = SessionId::from_raw(31337);
    assert_eq!(
        fx.core.terminal_text(ghost, TextScope::Screen, 10),
        Err(CoreError::NoSuchSession(ghost))
    );
}

#[test]
fn mcp_run_completes_with_output_and_unknown_exit_code_without_osc133() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let out = mcp_rt()
        .block_on(
            fx.core
                .mcp_terminal_run(id, "echo hello; echo world", limits(15_000)),
        )
        .unwrap();
    let TerminalRunOutcome::Completed { output, exit_code } = out else {
        panic!("应当完成：{out:?}");
    };
    // 输出只有命令之后到新提示符之前的内容：不含命令回显，也不含提示符。
    assert_eq!(output.text, "hello\nworld");
    assert!(!output.cleared && !output.truncated);
    assert_eq!(exit_code, None, "没有 OSC 133 D 时退出码未知");
    // 命令照常进了 shell，屏幕上能看到它。
    assert!(
        screen_lines(&fx, id)
            .iter()
            .any(|l| l == "K> echo hello; echo world")
    );
}

#[test]
fn mcp_run_with_osc133_returns_exit_code_and_output() {
    let fx = fixture_with_env(&[("PS1", OSC_PS1)]);
    let id = fx.open_at_prompt();
    // C / D 由命令自己输出，模拟 shell 集成：D 紧接着输出 A，与真实序列（D、A、提示符）一致。
    let out = mcp_rt()
        .block_on(fx.core.mcp_terminal_run(
            id,
            "printf '\\033]133;C\\007'; echo out; printf '\\033]133;D;7\\007'",
            limits(15_000),
        ))
        .unwrap();
    let TerminalRunOutcome::Completed { output, exit_code } = out else {
        panic!("应当完成：{out:?}");
    };
    assert_eq!(output.text, "out");
    assert_eq!(exit_code, Some(7));
}

#[test]
fn mcp_run_reports_clear_and_only_returns_text_after_it() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    // `clear` 的两种终端实现（先 2 后 3 / 先 3 后 2）都在同一批里成对出现；这里直接输出 ED 2 + ED 3。
    let out = mcp_rt()
        .block_on(fx.core.mcp_terminal_run(
            id,
            "echo before; printf '\\033[H\\033[2J\\033[3J'; echo after",
            limits(15_000),
        ))
        .unwrap();
    let TerminalRunOutcome::Completed { output, .. } = out else {
        panic!("应当完成：{out:?}");
    };
    assert!(output.cleared);
    assert_eq!(output.text, "after");
}

#[test]
fn mcp_run_rejects_newlines_and_empty_commands() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    for command in ["echo a\necho b", "echo a\r", "", "   "] {
        assert_eq!(
            rt.block_on(fx.core.mcp_terminal_run(id, command, limits(1000))),
            Err(CoreError::NotSupported),
            "{command:?}"
        );
    }
    // 什么也没有发送：屏幕上还是干净的提示符。
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(screen_lines(&fx, id), vec!["K>".to_owned()]);
}

#[test]
fn mcp_run_while_a_command_runs_is_rejected_and_timeout_does_not_interrupt_it() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    // 命令要 3 秒才结束；MCP 只等 1 秒：超时返回"未确认完成"，且不发 Ctrl+C。
    // 余量取大一些：慢机器上命令回显与输出到达的时间不确定。
    let started = Instant::now();
    let out = rt
        .block_on(fx.core.mcp_terminal_run(
            id,
            "echo start; sleep 3; echo FINISHED_MARK",
            limits(1000),
        ))
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "超时应当如期返回"
    );
    let TerminalRunOutcome::Unconfirmed { output, osc133 } = out else {
        panic!("应当超时：{out:?}");
    };
    assert!(!osc133);
    assert_eq!(output.text, "start");
    // 命令仍在运行：再次 run 被拒，原因是 CommandRunning，什么也没发送。
    assert_eq!(
        rt.block_on(fx.core.mcp_terminal_run(id, "echo second", limits(1000)))
            .unwrap(),
        TerminalRunOutcome::Rejected(TerminalBusy::CommandRunning)
    );
    // 没有被 Ctrl+C 打断：FINISHED_MARK 照常出现，之后回到提示符。
    fx.wait_screen(id, "FINISHED_MARK");
    fx.wait_prompt(id);
    assert!(!screen_lines(&fx, id).iter().any(|l| l.contains("second")));
    assert!(!fx.core.mcp_session_status(id).unwrap().locked);
}

#[test]
fn mcp_run_is_rejected_when_the_user_has_typed_on_the_prompt_until_ctrl_c() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    fx.core.text_input(id, "abc").unwrap();
    fx.wait_screen(id, "K> abc");
    assert_eq!(
        fx.core.mcp_session_status(id).unwrap().run_blocker,
        Some(TerminalBusy::InputNotEmpty)
    );
    assert_eq!(
        rt.block_on(fx.core.mcp_terminal_run(id, "echo x", limits(1000)))
            .unwrap(),
        TerminalRunOutcome::Rejected(TerminalBusy::InputNotEmpty)
    );
    // 用户用 Ctrl+C 放弃这一行之后，输入行为空，可以执行。
    fx.core
        .key(id, key(kanso_types::Key::Character("c".into()), true))
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let lines = screen_lines(&fx, id);
        if lines.last().map(String::as_str) == Some("K>") && lines.len() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "{lines:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = rt
        .block_on(fx.core.mcp_terminal_run(id, "echo ok", limits(15_000)))
        .unwrap();
    assert!(
        matches!(&out, TerminalRunOutcome::Completed { output, .. } if output.text == "ok"),
        "{out:?}"
    );
}

#[test]
fn mcp_input_does_not_count_as_user_input_for_the_input_line_check() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    // MCP 自己输入的半行文字不算用户输入：不影响随后的前置条件判断（这里它自己用 Ctrl+C 收拾）。
    let sent = rt
        .block_on(fx.core.mcp_terminal_send(
            id,
            vec![McpInput::Text("half".into())],
            Some(Duration::from_millis(200)),
            MCP_TEXT_LIMIT,
        ))
        .unwrap();
    assert!(matches!(
        sent,
        TerminalSendOutcome::Sent { screen: Some(_) }
    ));
    rt.block_on(fx.core.mcp_terminal_send(
        id,
        vec![McpInput::Key(key(
            kanso_types::Key::Character("c".into()),
            true,
        ))],
        None,
        0,
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    fx.wait_prompt(id);
    let out = rt
        .block_on(fx.core.mcp_terminal_run(id, "echo fine", limits(15_000)))
        .unwrap();
    assert!(
        matches!(&out, TerminalRunOutcome::Completed { output, .. } if output.text == "fine"),
        "{out:?}"
    );
}

#[test]
fn mcp_run_user_keypress_while_waiting_is_taken_over_and_still_reaches_the_terminal() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    let out = rt.block_on(async {
        let run = fx
            .core
            .mcp_terminal_run(id, "echo begin; sleep 3; echo end", limits(20_000));
        let user = async {
            tokio::time::sleep(Duration::from_millis(800)).await;
            // 用户按键：照常发给终端（sleep 期间按键会被 tty 回显）。
            fx.core.text_input(id, "u").unwrap();
        };
        let (out, ()) = tokio::join!(run, user);
        out
    });
    let TerminalRunOutcome::TakenOver { output } = out.unwrap() else {
        panic!("应当是用户接管");
    };
    // 接管时返回"当时已有的输出"：终端可能已经回显了用户按的 `u`（慢机器上回显先于等待者读输出），
    // 所以只要求以命令的第一段输出开头、且命令还没跑完，不要求恰好等于 "begin"。
    assert!(output.text.starts_with("begin"), "{:?}", output.text);
    assert!(!output.text.contains("end"), "{:?}", output.text);
    // 用户的输入没有被拦截。
    fx.wait_screen(id, "u");
    fx.wait_screen(id, "end");
}

#[test]
fn mcp_run_dropped_future_leaves_no_stuck_registration() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    // 在命令执行期间丢弃 future（客户端取消 / 级别被调低）。
    let dropped = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_millis(300),
            fx.core
                .mcp_terminal_run(id, "echo hi; sleep 1; echo DROPPED_DONE", limits(30_000)),
        )
        .await
    });
    assert!(dropped.is_err(), "future 应被丢弃");
    // 已经发出的命令不撤回，照常跑完。
    fx.wait_screen(id, "DROPPED_DONE");
    fx.wait_prompt(id);
    // 之后不被残留登记卡住：Sender 的占用已释放（Core 层），tracker 也回到空闲。
    let out = rt
        .block_on(fx.core.mcp_terminal_run(id, "echo again", limits(15_000)))
        .unwrap();
    assert!(
        matches!(&out, TerminalRunOutcome::Completed { output, .. } if output.text == "again"),
        "{out:?}"
    );
}

#[test]
fn mcp_run_session_closed_while_waiting_is_no_such_session() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    let started = Instant::now();
    let out = rt.block_on(async {
        let run = fx.core.mcp_terminal_run(id, "sleep 30", limits(20_000));
        let close = async {
            tokio::time::sleep(Duration::from_millis(800)).await;
            fx.core.close_session(id);
        };
        tokio::join!(run, close).0
    });
    assert_eq!(out, Err(CoreError::NoSuchSession(id)));
    assert!(started.elapsed() < Duration::from_secs(10), "应当立即中断");
}

#[test]
fn mcp_send_session_closed_while_settling_is_no_such_session() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    let started = Instant::now();
    let out = rt.block_on(async {
        let send = fx.core.mcp_terminal_send(
            id,
            vec![
                McpInput::Text("sleep 30".into()),
                McpInput::Key(key(kanso_types::Key::Enter, false)),
            ],
            Some(Duration::from_secs(10)),
            MCP_TEXT_LIMIT,
        );
        let close = async {
            tokio::time::sleep(Duration::from_millis(800)).await;
            fx.core.close_session(id);
        };
        tokio::join!(send, close).0
    });
    assert_eq!(out, Err(CoreError::NoSuchSession(id)));
    assert!(started.elapsed() < Duration::from_secs(8), "应当立即中断");
}

#[test]
fn mcp_send_text_and_enter_run_a_command_and_settle_returns_the_screen() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    let out = rt
        .block_on(fx.core.mcp_terminal_send(
            id,
            vec![
                McpInput::Text("echo SEND_MARK".into()),
                McpInput::Key(key(kanso_types::Key::Enter, false)),
            ],
            Some(Duration::from_millis(400)),
            MCP_TEXT_LIMIT,
        ))
        .unwrap();
    let TerminalSendOutcome::Sent {
        screen: Some(screen),
    } = out
    else {
        panic!("应当带屏幕返回：{out:?}");
    };
    let lines: Vec<&str> = screen.content.text.lines().collect();
    // 回显的命令行、输出、新的提示符。
    assert_eq!(lines, vec!["K> echo SEND_MARK", "SEND_MARK", "K>"]);
    assert_eq!(screen.cursor.unwrap().1, 3);
    // 没有请求等待时不带屏幕。
    let out = rt
        .block_on(
            fx.core
                .mcp_terminal_send(id, vec![McpInput::Text("".into())], None, 100),
        )
        .unwrap();
    assert_eq!(out, TerminalSendOutcome::Sent { screen: None });
}

#[test]
fn mcp_send_text_reaches_the_terminal_verbatim() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    // 文字原样输入（不加 bracketed paste 包裹，也没有按键编码）；Tab 键按终端模式编码成 \t，
    // 交给 shell 的补全。这里只验证文字里的控制字符原样到达 tty：`cat -v` 显示 ^A。
    let out = rt
        .block_on(fx.core.mcp_terminal_send(
            id,
            vec![
                McpInput::Text("printf 'a\\tb\\n' | cat -t".into()),
                McpInput::Key(key(kanso_types::Key::Enter, false)),
            ],
            Some(Duration::from_millis(400)),
            MCP_TEXT_LIMIT,
        ))
        .unwrap();
    let TerminalSendOutcome::Sent {
        screen: Some(screen),
    } = out
    else {
        panic!("{out:?}");
    };
    assert!(
        screen.content.text.contains("a^Ib"),
        "{}",
        screen.content.text
    );
}

#[test]
fn mcp_send_user_input_while_settling_is_taken_over() {
    let fx = fixture_with_env(&[("PS1", PLAIN_PS1)]);
    let id = fx.open_at_prompt();
    let rt = mcp_rt();
    let out = rt.block_on(async {
        // 命令回显之后一直没有输出，settle 长达 3 秒；用户在 300 毫秒时按键。
        let send = fx.core.mcp_terminal_send(
            id,
            vec![
                McpInput::Text("sleep 6".into()),
                McpInput::Key(key(kanso_types::Key::Enter, false)),
            ],
            Some(Duration::from_secs(3)),
            MCP_TEXT_LIMIT,
        );
        let user = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            fx.core.text_input(id, "z").unwrap();
        };
        tokio::join!(send, user).0
    });
    let TerminalSendOutcome::TakenOver { screen } = out.unwrap() else {
        panic!("应当是用户接管");
    };
    assert!(screen.content.text.contains("sleep 6"));
    // 已发送的输入不撤回：sleep 在运行（命令是 MCP 发的，仍被跟踪为"运行中"），提示符没有回来。
    // 状态机在回车后 100 毫秒才读到命令文本，所以轮询等它转成运行中。
    let deadline = Instant::now() + TIMEOUT;
    while fx.core.mcp_session_status(id).unwrap().activity != Activity::CommandRunning {
        assert!(Instant::now() < deadline, "命令没有被跟踪为运行中");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(screen_lines(&fx, id).last().map(String::as_str), Some("K>"));
}

#[test]
fn mcp_send_to_unknown_session_is_no_such_session() {
    let fx = fixture();
    let ghost = SessionId::from_raw(31337);
    let rt = mcp_rt();
    assert_eq!(
        rt.block_on(fx.core.mcp_terminal_send(ghost, vec![], None, 10)),
        Err(CoreError::NoSuchSession(ghost))
    );
    assert_eq!(
        rt.block_on(fx.core.mcp_terminal_run(ghost, "ls", limits(10))),
        Err(CoreError::NoSuchSession(ghost))
    );
}
