//! Windows 本地会话的退出、目录上报与路径粘贴实测。

#![cfg(windows)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use kanso_types::CoreEvent;
use kanso_types::event::{CloseReason, SessionState};
use kanso_types::input::PasteContent;

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

fn submit(session: &Arc<kanso_session::LocalSession>, mode: &str) {
    if mode == "wait" {
        std::thread::sleep(Duration::from_millis(700));
    }
    let enter = match mode {
        "win32" => "\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_",
        "lf" => "\n",
        _ => "\r",
    };
    session.text_input(enter);
}

#[test]
fn powershell_reports_directory_changes_through_conpty() {
    let fx = fixture("powershell.exe -NoLogo -NoProfile", true);
    let dir = fx.home.join("project with spaces");
    std::fs::create_dir(&dir).unwrap();
    let (id, session) = fx.open();
    let sees = |path: &std::path::Path| {
        fx.rec.wait_for(Duration::from_secs(15), |event| {
            matches!(event, CoreEvent::LocationChanged { session, cwd: Some(cwd), .. }
            if *session == id && std::path::Path::new(cwd) == path)
        })
    };
    assert!(sees(&fx.home), "PowerShell 应上报初始工作目录");
    let mode = std::env::var("KANSO_PTY_PROBE").unwrap_or_default();
    let change = "Set-Location -LiteralPath 'project with spaces'";
    if mode == "original" {
        session.text_input(&format!("{change}\r"));
    } else {
        session.text_input(change);
        submit(&session, &mode);
    }
    assert!(sees(&dir), "切换目录后应上报带空格的路径");
    fx.mgr.close(id);
}

#[test]
fn pasted_file_paths_are_readable_through_cmd_and_powershell() {
    for (shell, command) in [
        ("cmd.exe /d", "type "),
        (
            "powershell.exe -NoLogo -NoProfile",
            "Get-Content -LiteralPath ",
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
        if shell.starts_with("powershell") {
            submit(
                &session,
                &std::env::var("KANSO_PTY_PROBE").unwrap_or_default(),
            );
        } else {
            session.text_input("\r");
        }
        assert!(
            screen.wait_contains("KANSO_DROP_CONTENT_OK"),
            "{shell}: {}",
            screen.text
        );
        fx.mgr.close(id);
    }
}

#[test]
fn powershell_probe_console_read_key() {
    let shell = r#"powershell.exe -NoLogo -NoProfile -Command "[Console]::WriteLine('KANSO_KEY_READY'); $key = [Console]::ReadKey($true); [Console]::WriteLine(('KANSO_KEY:{0}:{1}:{2}' -f [int]$key.KeyChar, $key.Key, $key.Modifiers))""#;
    let fx = fixture(shell, false);
    let (id, session) = fx.open();
    let mut screen = Screen::new(&session);
    assert!(screen.wait_contains("KANSO_KEY_READY"), "{}", screen.text);
    let mode = std::env::var("KANSO_PTY_PROBE").unwrap_or_default();
    submit(&session, &mode);
    assert!(screen.wait_contains("KANSO_KEY:"), "{}", screen.text);
    println!("{}", screen.text.trim());
    fx.mgr.close(id);
}
