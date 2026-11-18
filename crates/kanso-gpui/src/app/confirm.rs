//! 关闭 / 退出确认（ui-shell.md §8）的对话框内容与结果判断。
//!
//! 对照 mac 版 `Shell/CloseConfirmer.swift`：标题说明是关闭还是退出，正文列出仍在运行 / 连接的会话
//! （最多列 [`MAX_LISTED`] 个，其余汇总成“另外 N 个”），有传输正在进行时额外说明会被中断。
//! 批量关闭只弹一次，`titles` 已经包含本次要列出的全部会话。
//!
//! 默认按钮是“取消”：回车不会误关一个正在运行的会话，要关必须点“关闭”或 Tab 过去再回车。

use crate::app::i18n::{label, t};
use kanso_types::SessionId;
use kanso_types::message::Message;

use crate::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec};

/// 确认框最多列出的会话数。
pub const MAX_LISTED: usize = 8;
/// “确认关闭 / 退出”按钮的标识。
pub const CONFIRM_ID: &str = "confirm";
/// “取消”按钮的标识。
pub const CANCEL_ID: &str = "cancel";

/// 一次等待用户确认的关闭请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseRequest {
    /// 确认后要关闭的会话（关闭窗口 / 退出时为空，直接退出应用）。
    pub sessions: Vec<SessionId>,
    /// 请求来自退出应用 / 关闭窗口。
    pub quit: bool,
}

/// 组装确认对话框。
pub fn close_dialog_spec(titles: &[String], interrupts_transfers: bool, quit: bool) -> DialogSpec {
    let mut lines: Vec<String> = titles
        .iter()
        .take(MAX_LISTED)
        .map(|t| format!("• {t}"))
        .collect();
    if titles.len() > MAX_LISTED {
        lines.push(t(&Message::UiAndMore {
            count: (titles.len() - MAX_LISTED) as i64,
        }));
    }
    let mut spec = DialogSpec::new(if quit {
        label(Message::UiQuitKanso {})
    } else {
        label(Message::UiCloseRunningSessions {})
    })
    .text(label(Message::UiTheseSessionsAreStillRunning {}))
    .code(&lines.join("\n"));
    if interrupts_transfers {
        spec = spec.text(label(Message::UiClosingWillInterruptFileTransfers {}));
    }
    spec.button(
        ButtonSpec::secondary(CANCEL_ID, label(Message::UiCancel {}))
            .cancel()
            .default_button(),
    )
    .button(ButtonSpec::danger(
        CONFIRM_ID,
        if quit {
            label(Message::UiQuit {})
        } else {
            label(Message::UiClose {})
        },
    ))
}

/// 用户是否确认了关闭（Esc / 点遮罩 / 取消按钮都不算）。
pub fn is_confirmed(result: &DialogResult) -> bool {
    result.button.as_deref() == Some(CONFIRM_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::dialog::{BodyBlock, Trigger, result_for_button, result_for_cancel};

    fn titles(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("会话{i}")).collect()
    }

    fn code_of(spec: &DialogSpec) -> String {
        spec.body
            .iter()
            .find_map(|b| match b {
                BodyBlock::Code(c) => Some(c.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn lists_sessions_and_summarises_the_overflow() {
        let spec = close_dialog_spec(&titles(3), false, false);
        assert_eq!(code_of(&spec), "• 会话1\n• 会话2\n• 会话3");
        let spec = close_dialog_spec(&titles(MAX_LISTED + 3), false, false);
        let code = code_of(&spec);
        assert_eq!(code.lines().count(), MAX_LISTED + 1);
        assert!(code.ends_with("…另外 3 个"));
    }

    #[test]
    fn quit_and_close_differ_in_wording() {
        let close = close_dialog_spec(&titles(1), false, false);
        let quit = close_dialog_spec(&titles(1), false, true);
        assert!(close.title.contains("关闭"));
        assert!(quit.title.contains("退出"));
        assert_eq!(close.buttons[1].label, "关闭");
        assert_eq!(quit.buttons[1].label, "退出");
    }

    #[test]
    fn transfer_warning_only_when_transfers_are_running() {
        let has_warning = |spec: &DialogSpec| {
            spec.body
                .iter()
                .any(|b| matches!(b, BodyBlock::Text(t) if t.contains("中断")))
        };
        assert!(!has_warning(&close_dialog_spec(&titles(1), false, false)));
        assert!(has_warning(&close_dialog_spec(&titles(1), true, false)));
    }

    #[test]
    fn cancel_is_the_default_and_escape_button() {
        let spec = close_dialog_spec(&titles(1), false, false);
        assert_eq!(spec.default_button(), Some(0));
        assert_eq!(spec.cancel_button(), Some(0));
        assert_eq!(spec.buttons[0].id, CANCEL_ID);
    }

    #[test]
    fn only_the_confirm_button_confirms() {
        let spec = close_dialog_spec(&titles(1), false, false);
        assert!(is_confirmed(&result_for_button(
            &spec,
            1,
            Trigger::Click,
            &[]
        )));
        assert!(!is_confirmed(&result_for_button(
            &spec,
            0,
            Trigger::Enter,
            &[]
        )));
        assert!(!is_confirmed(&result_for_cancel(&spec, Trigger::Escape)));
        assert!(!is_confirmed(&result_for_cancel(&spec, Trigger::Scrim)));
    }
}
