//! Windows 本地会话：shell 自己退出时会话必须结束（ConPTY 不关闭伪控制台就没有 EOF）。

#![cfg(windows)]

mod common;

use std::time::Duration;

use common::*;
use kanso_types::CoreEvent;
use kanso_types::event::{CloseReason, SessionState};

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
