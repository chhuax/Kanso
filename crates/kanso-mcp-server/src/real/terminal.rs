//! 终端类工具的纯函数部分：参数解析、按键映射、core 结果到 AI 可读 JSON 与错误码的映射。
//!
//! 不碰 core、不碰锁，便于单独测试。文字一律是给 AI 看的英文（spec/mcp.md §5）。

use kanso_core::mcp::{
    CapturedText, McpInput, TerminalBusy, TerminalRunOutcome, TerminalSendOutcome, TerminalText,
};
use kanso_types::{Key, KeyEvent, KeyEventKind, Modifiers};
use serde_json::{Map, Value, json};

use super::tools::ToolError;

/// `kanso_terminal_read` 的行数默认值与上下限（spec/mcp.md §3.2）。
pub(super) const READ_LINES_DEFAULT: u32 = 200;
pub(super) const READ_LINES_MAX: u32 = 2000;
/// `kanso_terminal_run` 的超时默认值与上下限，单位秒（spec/mcp.md §4.2）。
pub(super) const RUN_TIMEOUT_DEFAULT: u64 = 30;
pub(super) const RUN_TIMEOUT_MAX: u64 = 600;
/// `kanso_terminal_send` 的等待安静上限，单位毫秒（spec/mcp.md §3.3）。
pub(super) const SETTLE_MAX_MS: u64 = 10_000;
/// 一次 `kanso_terminal_send` 的输入项数与文字总字节数上限（spec/mcp.md §3.3：最多 256 项、文字合计最多 64 KiB）；
/// 超出返回 `TOO_LARGE`（spec/mcp.md §9）。
pub(super) const SEND_MAX_ITEMS: usize = 256;
pub(super) const SEND_MAX_TEXT_BYTES: usize = 64 * 1024;

/// 无效参数的错误码，对应 spec/mcp.md §9 的 `INVALID_ARGUMENT`：参数不合法（命令含控制字符、按键名无法识别等），
/// 不会触达终端，AI 应修正参数而不是重试，与 `TAB_BUSY` 等"稍后重试"类错误区分开。
pub(super) const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";

pub(super) fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::new(INVALID_ARGUMENT, message)
}

/// 把 `Option` 参数夹到 `[min, max]`，缺省取 `default`。
pub(super) fn clamp_or(value: Option<u64>, default: u64, min: u64, max: u64) -> u64 {
    value.unwrap_or(default).clamp(min, max)
}

/// 解析一个按键名（不区分大小写）：Enter、Tab、Esc/Escape、Backspace、Up/Down/Left/Right、
/// Home、End、PageUp、PageDown、Ctrl+<字母>。
///
/// Ctrl+字母按"字符键 + Ctrl 修饰"构造，由 core 的编码器按终端当前模式（含 kitty 协议）编码，
/// 与用户按键走同一套逻辑（spec/mcp.md §3.3）；这里不自己写控制字节。`text` 留空：
/// 带修饰键时系统产生的文本由 core 决定，不需要也不应由调用方伪造。
pub(super) fn parse_key(name: &str) -> Result<KeyEvent, ToolError> {
    let lower = name.trim().to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "esc" | "escape" => Some(Key::Escape),
        "backspace" => Some(Key::Backspace),
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        _ => None,
    };
    if let Some(key) = key {
        return Ok(key_event(key, Modifiers::NONE));
    }
    if let Some(rest) = lower.strip_prefix("ctrl+") {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && c.is_ascii_lowercase()
        {
            let mods = Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            };
            return Ok(key_event(Key::Character(c.to_string()), mods));
        }
    }
    Err(invalid(format!(
        "Unknown key {name:?}. Supported keys: Enter, Tab, Esc, Backspace, Up, Down, Left, Right, \
         Home, End, PageUp, PageDown, and Ctrl+<letter> (for example Ctrl+C)."
    )))
}

fn key_event(key: Key, mods: Modifiers) -> KeyEvent {
    KeyEvent {
        key,
        mods,
        text: None,
        kind: KeyEventKind::Press,
    }
}

/// `kanso_terminal_send` 的一项输入：`text` 与 `key` 恰好有一个。
pub(super) fn parse_input(text: Option<&str>, key: Option<&str>) -> Result<McpInput, ToolError> {
    match (text, key) {
        (Some(t), None) => Ok(McpInput::Text(t.to_owned())),
        (None, Some(k)) => parse_key(k).map(McpInput::Key),
        _ => Err(invalid(
            "Each input must have exactly one of \"text\" or \"key\".",
        )),
    }
}

/// 截断与清屏的说明，附在文本旁边；没有特殊情况时为 `None`。
fn capture_note(c: &CapturedText) -> Option<String> {
    let mut parts = Vec::new();
    if c.truncated {
        parts.push(format!(
            "Output truncated: only the last {} bytes are shown; the original size was {} bytes.",
            c.text.len(),
            c.original_bytes
        ));
    }
    if c.cleared {
        parts.push(
            "The screen was cleared while waiting; only the output after the clear is shown."
                .to_owned(),
        );
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// 把捕获的文本写进 `obj` 的 `key` 字段，并附上大小与说明字段。
fn put_capture(obj: &mut Map<String, Value>, key: &str, c: &CapturedText) {
    obj.insert(key.to_owned(), json!(c.text));
    obj.insert("original_bytes".to_owned(), json!(c.original_bytes));
    obj.insert("truncated".to_owned(), json!(c.truncated));
    if let Some(note) = capture_note(c) {
        obj.insert("note".to_owned(), json!(note));
    }
}

/// 捕获文本的 JSON 对象（`text`、`original_bytes`、`truncated`，有说明时带 `note`）。
/// `kanso_exec` 的 stdout / stderr 各用一个，字段与终端类工具的输出保持一致。
pub(super) fn capture_object(c: &CapturedText) -> Value {
    let mut obj = Map::new();
    put_capture(&mut obj, "text", c);
    Value::Object(obj)
}

/// 终端文本快照的 JSON：`text`、光标、尺寸、备用屏幕，以及截断说明。
pub(super) fn text_json(t: &TerminalText) -> Value {
    let mut obj = Map::new();
    put_capture(&mut obj, "text", &t.content);
    obj.insert(
        "cursor".to_owned(),
        json!(t.cursor.map(|(row, col)| json!({"row": row, "col": col}))),
    );
    obj.insert("size".to_owned(), json!({"rows": t.rows, "cols": t.cols}));
    obj.insert("alternate_screen".to_owned(), json!(t.alternate_screen));
    Value::Object(obj)
}

/// `TAB_BUSY` 的说明：写明是 spec/mcp.md §4.2 的哪一条前置条件。
fn busy_message(reason: TerminalBusy) -> &'static str {
    match reason {
        TerminalBusy::NotConnected => {
            "The tab is not connected (it may be connecting, closed or locked)."
        }
        TerminalBusy::CommandRunning => {
            "A command is already running in this tab, so it is not at a shell prompt. \
             Use kanso_terminal_read to look at it, or kanso_terminal_send to interact with it."
        }
        TerminalBusy::AgentRunning => {
            "An AI CLI is running in this tab. Use kanso_terminal_send to interact with it."
        }
        TerminalBusy::InputNotEmpty => {
            "The input line is not empty; the user may be typing. Nothing was sent."
        }
        TerminalBusy::Transferring => "A ZMODEM/XMODEM transfer is in progress in this tab.",
        TerminalBusy::SenderActive => "Kanso's Sender is sending multi-line content to this tab.",
    }
}

fn busy(reason: TerminalBusy) -> ToolError {
    ToolError::new("TAB_BUSY", busy_message(reason))
}

/// 把 `kanso_terminal_run` 的 core 结果映射成工具结果。`timeout_secs` 只用于说明文字。
///
/// `Unconfirmed` 与 `TakenOver` 是错误码，但仍带已有输出（放在错误 JSON 的顶层 `output` 里），
/// 让 AI 不必为了看输出再调一次读取。
pub(super) fn run_result(
    outcome: TerminalRunOutcome,
    timeout_secs: u64,
) -> Result<Value, ToolError> {
    match outcome {
        TerminalRunOutcome::Rejected(reason) => Err(busy(reason)),
        TerminalRunOutcome::Completed { output, exit_code } => {
            let mut obj = Map::new();
            obj.insert("status".to_owned(), json!("completed"));
            obj.insert(
                "exit_code".to_owned(),
                exit_code.map_or_else(|| json!("unknown"), |c| json!(c)),
            );
            put_capture(&mut obj, "output", &output);
            Ok(Value::Object(obj))
        }
        TerminalRunOutcome::Unconfirmed { output, osc133 } => {
            let mut message = format!(
                "The command was sent but its completion was not confirmed within {timeout_secs} \
                 seconds. It may still be running, or it may have finished without Kanso \
                 recognizing the prompt. The tab was left as is and no Ctrl+C was sent. Use \
                 kanso_terminal_read to check, or kanso_terminal_send to send Ctrl+C."
            );
            if !osc133 {
                // spec/mcp.md §4.2：没有 OSC 133 时完成判断只能靠提示符特征。
                message.push_str(
                    " This tab has no OSC 133 shell integration, so completion detection relies \
                     on recognizing the prompt; commands that change the prompt (such as cd) can \
                     keep it from being recognized. For SSH sessions, prefer kanso_exec.",
                );
            }
            let mut obj = Map::new();
            obj.insert("status".to_owned(), json!("unconfirmed"));
            put_capture(&mut obj, "output", &output);
            Err(ToolError::with_details("TIMEOUT", message, obj))
        }
        TerminalRunOutcome::TakenOver { output } => {
            let mut obj = Map::new();
            obj.insert("status".to_owned(), json!("user_took_over"));
            put_capture(&mut obj, "output", &output);
            Err(ToolError::with_details(
                "USER_TOOK_OVER",
                "The user typed in this tab while the call was waiting, so it ended. The user's \
                 input was not blocked. New write operations on this tab are refused for 10 \
                 seconds.",
                obj,
            ))
        }
    }
}

/// 把 `kanso_terminal_send` 的 core 结果映射成工具结果。
pub(super) fn send_result(outcome: TerminalSendOutcome) -> Result<Value, ToolError> {
    match outcome {
        TerminalSendOutcome::Rejected(reason) => Err(busy(reason)),
        TerminalSendOutcome::Sent { screen } => {
            let mut obj = Map::new();
            obj.insert("status".to_owned(), json!("sent"));
            if let Some(screen) = screen {
                obj.insert("screen".to_owned(), text_json(&screen));
            }
            Ok(Value::Object(obj))
        }
        TerminalSendOutcome::TakenOver { screen } => {
            let mut obj = Map::new();
            obj.insert("status".to_owned(), json!("user_took_over"));
            obj.insert("screen".to_owned(), text_json(&screen));
            Err(ToolError::with_details(
                "USER_TOOK_OVER",
                "The user typed in this tab while the call was waiting for output to settle. The \
                 input you sent was not undone. New write operations on this tab are refused for \
                 10 seconds.",
                obj,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(text: &str) -> CapturedText {
        CapturedText {
            text: text.to_owned(),
            original_bytes: text.len() as u64,
            truncated: false,
            cleared: false,
        }
    }

    #[test]
    fn parses_supported_keys() {
        for name in [
            "Enter",
            "tab",
            "ESC",
            "Backspace",
            "Up",
            "Down",
            "Left",
            "Right",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Ctrl+C",
            "ctrl+z",
        ] {
            assert!(parse_key(name).is_ok(), "{name}");
        }
        let e = parse_key("Ctrl+C").unwrap();
        assert_eq!(e.key, Key::Character("c".into()));
        assert!(e.mods.ctrl);
        for bad in ["Ctrl+", "Ctrl+10", "Ctrl+é", "Alt+x", "F1", ""] {
            assert_eq!(
                parse_key(bad).err().unwrap().code,
                INVALID_ARGUMENT,
                "{bad}"
            );
        }
    }

    #[test]
    fn input_needs_exactly_one_field() {
        assert!(matches!(
            parse_input(Some("ls"), None),
            Ok(McpInput::Text(_))
        ));
        assert!(matches!(
            parse_input(None, Some("Tab")),
            Ok(McpInput::Key(_))
        ));
        assert!(parse_input(None, None).is_err());
        assert!(parse_input(Some("a"), Some("Tab")).is_err());
    }

    #[test]
    fn run_outcomes_map_to_codes() {
        let ok = run_result(
            TerminalRunOutcome::Completed {
                output: cap("hi"),
                exit_code: None,
            },
            30,
        )
        .unwrap();
        assert_eq!(ok["exit_code"], "unknown");
        assert_eq!(ok["output"], "hi");

        let e = run_result(
            TerminalRunOutcome::Unconfirmed {
                output: cap("partial"),
                osc133: false,
            },
            5,
        )
        .err()
        .unwrap();
        assert_eq!(e.code, "TIMEOUT");
        assert!(e.message.contains("kanso_exec"));
        assert_eq!(e.details.unwrap()["output"], "partial");

        let e = run_result(
            TerminalRunOutcome::Unconfirmed {
                output: cap(""),
                osc133: true,
            },
            5,
        )
        .err()
        .unwrap();
        assert!(!e.message.contains("kanso_exec"));

        let e = run_result(TerminalRunOutcome::TakenOver { output: cap("x") }, 5)
            .err()
            .unwrap();
        assert_eq!(e.code, "USER_TOOK_OVER");
        let e = run_result(TerminalRunOutcome::Rejected(TerminalBusy::InputNotEmpty), 5)
            .err()
            .unwrap();
        assert_eq!(e.code, "TAB_BUSY");
        assert!(e.message.contains("input line"));
    }

    #[test]
    fn truncation_is_explained_with_original_size() {
        let c = CapturedText {
            text: "tail".into(),
            original_bytes: 999_999,
            truncated: true,
            cleared: false,
        };
        let v = text_json(&TerminalText {
            content: c,
            ..TerminalText::default()
        });
        assert_eq!(v["original_bytes"], 999_999);
        assert!(v["note"].as_str().unwrap().contains("999999"));
    }
}
