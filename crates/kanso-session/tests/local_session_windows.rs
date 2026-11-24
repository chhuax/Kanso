//! Windows 本地会话的退出、目录上报与路径粘贴实测。

#![cfg(windows)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use kanso_session::{OpenRequest, ReadScope};
use kanso_types::CoreEvent;
use kanso_types::event::{CloseReason, SessionState};
use kanso_types::input::PasteContent;

// Windows 托管 Runner 上，PowerShell 5.1 首条交互命令回显后有时要等约半分钟才返回结果。
const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(60);

fn closed_reasons(fx: &Fixture) -> Vec<CloseReason> {
    fx.rec
        .all()
        .into_iter()
        .filter_map(|e| match e {
            CoreEvent::SessionStateChanged {
                state: SessionState::Closed(r),
                ..
            } => Some(r),
            _ => None,
        })
        .collect()
}

fn exit_code_is_reported(code: i32) {
    let fx = fixture("cmd.exe", false);
    let (_id, s) = fx.open();
    s.text_input(&format!("exit {code}\r"));
    assert!(
        fx.rec.wait_for(Duration::from_secs(15), is_closed),
        "shell 退出后会话没有结束"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        closed_reasons(&fx),
        vec![CloseReason::Exited { code: Some(code) }]
    );
}

#[test]
fn cmd_exit_3_closes_session_with_code() {
    exit_code_is_reported(3);
}

#[test]
fn cmd_exit_0_closes_session_with_code() {
    exit_code_is_reported(0);
}

#[test]
fn powershell_reports_directory_and_git_branch_through_conpty() {
    check_powershell_prompt(true);
}

#[test]
fn powershell_can_show_a_plain_prompt_without_losing_git_branch() {
    check_powershell_prompt(false);
}

fn check_powershell_prompt(color_prompt: bool) {
    let fx = fixture("powershell.exe -NoLogo -NoProfile", true);
    let dir = fx.home.join("project with spaces");
    let git_dir = dir.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/example\n").unwrap();
    let id = fx
        .mgr
        .open_with_powershell_prompt(OpenRequest::DefaultLocalShell, GRID, color_prompt)
        .unwrap();
    let session = fx.mgr.get(id).unwrap();
    let sees = |path: &std::path::Path, branch: Option<&str>| {
        fx.rec.wait_for(POWERSHELL_TIMEOUT, |event| {
            matches!(event, CoreEvent::LocationChanged { session, cwd: Some(cwd), git_branch, .. }
            if *session == id && std::path::Path::new(cwd) == path && git_branch.as_deref() == branch)
        })
    };
    assert!(sees(&fx.home, None), "PowerShell 应上报初始工作目录");
    session.text_input("Set-Location -LiteralPath 'project with spaces'\r");
    assert!(
        sees(&dir, Some("feature/example")),
        "切换目录后应上报路径和分支"
    );
    let branch = if color_prompt {
        "\u{e0a0} feature/example"
    } else {
        "[feature/example]>"
    };
    // 按逻辑行读屏，长路径导致的软换行不会拆开分支提示符。
    let deadline = Instant::now() + POWERSHELL_TIMEOUT;
    let text = loop {
        let text = session.mcp_read(ReadScope::Screen).text;
        if text.contains(branch) {
            break text;
        }
        assert!(Instant::now() < deadline, "默认提示符应显示分支：{text}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(text.contains('\u{e0b0}'), color_prompt);
    fx.mgr.close(id);
}

#[test]
fn pasted_file_paths_are_readable_through_cmd_and_powershell() {
    for (shell, command, timeout) in [
        ("cmd.exe /d", "type ", TIMEOUT),
        (
            "powershell.exe -NoLogo -NoProfile",
            "Get-Content -LiteralPath ",
            POWERSHELL_TIMEOUT,
        ),
    ] {
        let fx = fixture(shell, false);
        let path = fx.home.join("drop 文档 & copy.txt");
        std::fs::write(&path, "KANSO_DROP_CONTENT_OK\r\n").unwrap();
        let (id, session) = fx.open();
        let mut screen = Screen::new(&session);
        assert!(screen.wait_contains(">"), "{shell}: {}", screen.text);
        session.text_input(command);
        session.paste(&PasteContent::Files(vec![
            path.to_string_lossy().into_owned(),
        ]));
        session.text_input("\r");
        assert!(
            screen.wait_for(timeout, |text| text.contains("KANSO_DROP_CONTENT_OK")),
            "{shell}: {}",
            screen.text
        );
        fx.mgr.close(id);
    }
}
