//! Windows 本地会话的退出、目录上报与路径粘贴实测。

#![cfg(windows)]

mod common;

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

fn shell_for_probe(mode: &str, report_cwd: bool) -> String {
    let executable = if mode == "pwsh" {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    let preset = match mode {
        "plain" => "Remove-Module PSReadLine -ErrorAction SilentlyContinue; ",
        "no-history" => "Set-PSReadLineOption -HistorySaveStyle SaveNothing -ErrorAction Stop; ",
        _ => "",
    };
    let log = "[Console]::WriteLine(('KANSO_PROBE:{0}:{1}:{2}' -f $PSVersionTable.PSVersion, (Get-Module PSReadLine).Version, (Get-Module PSReadLine).Path)); ";
    let hook = if report_cwd {
        include_str!("../src/windows_prompt.ps1").trim()
    } else {
        ""
    };
    format!(r#"{executable} -NoLogo -NoProfile -NoExit -Command "{preset}{log}{hook}""#)
}

#[test]
fn powershell_reports_directory_changes_through_conpty() {
    let mode = std::env::var("KANSO_PTY_PROBE").unwrap_or_default();
    let fx = fixture(&shell_for_probe(&mode, true), false);
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
    session.text_input("Set-Location -LiteralPath 'project with spaces'\r");
    assert!(sees(&dir), "切换目录后应上报带空格的路径");
    fx.mgr.close(id);
}

#[test]
fn pasted_file_paths_are_readable_through_cmd_and_powershell() {
    let mode = std::env::var("KANSO_PTY_PROBE").unwrap_or_default();
    let powershell = shell_for_probe(&mode, false);
    for (shell, command) in [
        ("cmd.exe /d", "type "),
        (&powershell, "Get-Content -LiteralPath "),
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
            screen.wait_contains("KANSO_DROP_CONTENT_OK"),
            "{shell}: {}",
            screen.text
        );
        fx.mgr.close(id);
    }
}
