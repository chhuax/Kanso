//! 命令跟踪推断与 AI 轮次的本地会话端到端测试（terminal.md §5、§6）。
//!
//! 真实 PTY + `/bin/sh -i`：它没有 OSC 133 集成，所以走的是提示符推断路径。
//! "2 秒无输出"这类时间规则在 `kanso-term` 的状态机测试里用手动时钟覆盖；这里只验证会话层的接线，
//! 所以会真实等待一次 2 秒。

#![cfg(unix)]

mod common;

use std::time::Duration;

use common::*;
use kanso_session::Registered;
use kanso_types::agent::AgentKind;
use kanso_types::event::Activity;
use kanso_types::{CoreEvent, Key, KeyEvent, KeyEventKind, Modifiers, PasteContent, SessionId};

fn ctrl_c() -> KeyEvent {
    KeyEvent {
        key: Key::Character("c".to_owned()),
        mods: Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        text: None,
        kind: KeyEventKind::Press,
    }
}

fn activities(fx: &Fixture, id: SessionId) -> Vec<Activity> {
    fx.rec
        .all()
        .into_iter()
        .filter_map(|e| match e {
            CoreEvent::ActivityChanged { session, activity } if session == id => Some(activity),
            _ => None,
        })
        .collect()
}

fn wait_activity(fx: &Fixture, id: SessionId, what: &str, pred: impl Fn(&[Activity]) -> bool) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while !pred(&activities(fx, id)) {
        assert!(
            std::time::Instant::now() < deadline,
            "等不到活动：{what}，实际 {:?}",
            activities(fx, id)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn finished_count(fx: &Fixture) -> usize {
    fx.rec.finished.lock().unwrap().len()
}

#[test]
fn plain_shell_command_is_tracked_by_prompt_inference() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    // 等提示符出现：输入起点要在提示符画好之后的第一次按键时记录。
    assert!(screen.wait_contains("$"), "{}", screen.text);
    s.set_visible(false);

    s.text_input("sleep 1\r");
    wait_activity(&fx, id, "Idle", |a| {
        a == [Activity::CommandRunning, Activity::Idle]
    });
    // 后台完成通知：与 OSC 133 路径一致，推断路径拿不到退出码。
    assert_eq!(*fx.rec.finished.lock().unwrap(), vec![(id, None)]);
}

#[test]
fn agent_turns_follow_user_keys_and_ignore_terminal_replies() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(screen.wait_contains("$"), "{}", screen.text);
    s.set_visible(false);

    // 假的 `claude`：启动时发一个设备属性查询（终端会回应），然后一直占着终端。
    s.text_input("claude() { printf '\\033[c'; sleep 30; }\r");
    wait_activity(&fx, id, "函数定义返回", |a| {
        a == [Activity::CommandRunning, Activity::Idle]
    });
    let base = activities(&fx, id).len();
    let notified = finished_count(&fx);
    assert!(fx.rec.agent_done.lock().unwrap().is_empty());

    s.text_input("claude\r");
    let k = AgentKind::Claude;
    wait_activity(&fx, id, "AgentIdle", |a| {
        a.get(base) == Some(&Activity::AgentIdle(k))
    });
    // 终端对查询的回应走绕过编码器的路径，不算用户按键，不会开启轮次。
    assert!(screen.wait_contains("^[[?"), "{}", screen.text);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(activities(&fx, id)[base..], [Activity::AgentIdle(k)]);

    // 用户按键开启轮次；2 秒没有输出之后结束，并发后台完成通知。
    s.text_input("x");
    wait_activity(&fx, id, "AgentWorking", |a| {
        a[base..] == [Activity::AgentIdle(k), Activity::AgentWorking(k)]
    });
    wait_activity(&fx, id, "轮次结束", |a| {
        a[base..]
            == [
                Activity::AgentIdle(k),
                Activity::AgentWorking(k),
                Activity::AgentIdle(k),
            ]
    });
    // 轮次结束发的是 agent 完成通知，不是普通命令完成通知。
    assert_eq!(finished_count(&fx), notified);
    assert_eq!(*fx.rec.agent_done.lock().unwrap(), vec![(id, k)]);

    // 工具退出（命令返回提示符）：回到 Idle。
    s.key(&ctrl_c());
    wait_activity(&fx, id, "Idle", |a| a.last() == Some(&Activity::Idle));
}

#[test]
fn one_shot_agent_command_is_a_plain_command() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(screen.wait_contains("$"), "{}", screen.text);
    s.text_input("claude() { sleep 1; }\r");
    wait_activity(&fx, id, "函数定义返回", |a| {
        a == [Activity::CommandRunning, Activity::Idle]
    });
    s.text_input("claude -p hi\r");
    wait_activity(&fx, id, "Idle", |a| {
        a[2..] == [Activity::CommandRunning, Activity::Idle]
    });
}

#[test]
fn sender_registration_waits_for_the_prompt_to_return() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(screen.wait_contains("$"), "{}", screen.text);

    // 没有命令在运行：立即返回。
    assert!(s.wait_command_idle(Duration::from_millis(1)));

    assert_eq!(s.register_send("sleep 1"), Registered::Command);
    assert!(
        !s.wait_command_idle(Duration::from_millis(20)),
        "已登记，视为运行中"
    );
    // 已登记、尚未返回时不能再登记。
    assert_eq!(s.register_send("x"), Registered::Busy);
    s.text_input("sleep 1\r");
    assert!(s.wait_command_idle(TIMEOUT));
    wait_activity(&fx, id, "Idle", |a| a.last() == Some(&Activity::Idle));

    // 发送失败：撤销登记，立即回到空闲。
    assert_eq!(s.register_send("x"), Registered::Command);
    s.cancel_send();
    assert!(s.wait_command_idle(Duration::from_millis(1)));
}

#[test]
fn session_end_wakes_waiters() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(screen.wait_contains("$"), "{}", screen.text);
    s.text_input("sleep 30\r");
    wait_activity(&fx, id, "Running", |a| a == [Activity::CommandRunning]);

    let waiter = {
        let s = std::sync::Arc::clone(&s);
        std::thread::spawn(move || s.wait_command_idle(Duration::from_secs(60)))
    };
    std::thread::sleep(Duration::from_millis(100));
    fx.mgr.close(id);
    let start = std::time::Instant::now();
    assert!(waiter.join().unwrap(), "会话结束要唤醒等待者并报告空闲");
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn ai_session_paste_rules() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(screen.wait_contains("$"), "{}", screen.text);

    // 普通 shell：只有图片什么也不发；文件路径转义。
    s.paste(&PasteContent::ImageOnly);
    s.paste(&PasteContent::Files(vec!["/tmp/a b.png".to_owned()]));
    assert!(screen.wait_contains("/tmp/a\\ b.png"), "{}", screen.text);
    assert!(activities(&fx, id).is_empty());
    s.key(&ctrl_c());
    // 等 shell 重画提示符：输入起点要在新提示符画好之后的第一次按键时记录。
    std::thread::sleep(Duration::from_millis(400));

    s.text_input("claude() { sleep 30; }\r");
    wait_activity(&fx, id, "函数定义返回", |a| {
        a == [Activity::CommandRunning, Activity::Idle]
    });
    s.text_input("claude\r");
    let k = AgentKind::Claude;
    wait_activity(&fx, id, "AgentIdle", |a| {
        a.get(2) == Some(&Activity::AgentIdle(k))
    });

    // AI 会话：路径原样（不转义），让工具识别成附件。
    s.paste(&PasteContent::Files(vec!["/tmp/c d.png".to_owned()]));
    assert!(screen.wait_contains("/tmp/c d.png"), "{}", screen.text);
    wait_activity(&fx, id, "粘贴是一次按键，开启轮次", |a| {
        a.last() == Some(&Activity::AgentWorking(k))
    });
}

#[test]
fn ai_session_image_paste_sends_ctrl_v_as_a_turn() {
    let fx = sh_fixture();
    let (id, s) = fx.open();
    let mut screen = Screen::new(&s);
    assert!(screen.wait_contains("$"), "{}", screen.text);
    s.text_input("claude() { sleep 30; }\r");
    wait_activity(&fx, id, "函数定义返回", |a| {
        a == [Activity::CommandRunning, Activity::Idle]
    });
    s.text_input("claude\r");
    let k = AgentKind::Claude;
    wait_activity(&fx, id, "AgentIdle", |a| {
        a.get(2) == Some(&Activity::AgentIdle(k))
    });
    s.paste(&PasteContent::ImageOnly);
    wait_activity(&fx, id, "图片粘贴键是一次按键", |a| {
        a.last() == Some(&Activity::AgentWorking(k))
    });
}
