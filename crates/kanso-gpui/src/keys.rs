//! GPUI 按键 → 核心按键事件的分流。
//!
//! 按键字节的编码（xterm 修饰键、DECCKM、kitty 协议等）全部在核心里（`Core::key`，
//! docs/input-encoding.md），本模块只决定**哪些按键交给 `Core::key`**，哪些留给文本路径。
//!
//! Windows 上的事件顺序决定了分流规则（GPUI 的 Windows 后端）：
//! - 没有被应用处理的 `WM_KEYDOWN` 会经 `TranslateMessage` 变成 `WM_CHAR`，再进入输入处理器的
//!   `replace_text_in_range`，由我们作为 `text_input` 发给会话；
//! - `WM_CHAR` 会过滤控制字符，所以回车、退格、Tab、Esc、Ctrl 组合键必须在按键阶段自己编码；
//! - 输入法组字期间 GPUI 根本不会派发 `KeyDown`，所以这里无需判断“是否在组字”；
//! - 输入法在组字之前处理的第一个键（`VK_PROCESSKEY`）仍会以普通 `KeyDown` 到达，
//!   如果这里对可打印键也发 `key`，就会把拼音首字母漏给 shell。所以**无修饰的可打印键一律不处理**，
//!   交给输入法 / `WM_CHAR`。
//!
//! 已知限制：程序开启 kitty “报告所有按键”之类的标志时，普通字符本应以 CSI u 发送；
//! 走文本路径的字符不会带这些编码（原型取舍，见 docs/ime-test-checklist.md 末尾的说明）。

use kanso_types::input::{Key, KeyEvent, KeyEventKind, Modifiers};

/// 与窗口系统无关的按键描述（从 GPUI 的 `Keystroke` / `KeyDownEvent` 拷贝而来）。
#[derive(Clone, Copy, Debug)]
pub struct KeyInput<'a> {
    /// GPUI 的 `key`：命名键用小写英文名（`enter`、`up`、`f5`），字符键为键帽字符（`a`、`;`）。
    pub key: &'a str,
    /// 这次按键可能产生的文本（`key_char`）。
    pub key_char: Option<&'a str>,
    /// Ctrl。
    pub ctrl: bool,
    /// Alt。
    pub alt: bool,
    /// Shift。
    pub shift: bool,
    /// Windows 键。
    pub platform: bool,
    /// 是否为长按重复。
    pub is_held: bool,
}

/// 分流结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// 交给 `Core::key`，并吞掉事件（不再产生 `WM_CHAR`）。
    Send(KeyEvent),
    /// 复制选区。
    Copy,
    /// 粘贴剪贴板。
    Paste,
    /// 不处理，让事件继续走文本路径（`WM_CHAR` / 输入法）。
    PassThrough,
}

fn named_key(key: &str) -> Option<Key> {
    Some(match key {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        _ => {
            let n: u8 = key.strip_prefix('f')?.parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            Key::F(n)
        }
    })
}

/// 按键分流。
pub fn classify_key(input: &KeyInput<'_>) -> KeyAction {
    if input.platform {
        // Win 键组合属于系统。
        return KeyAction::PassThrough;
    }
    // 复制 / 粘贴：Ctrl+Shift+C / V、Shift+Insert。不用裸 Ctrl+C / V，它们要照常发给 shell。
    if input.ctrl && input.shift && !input.alt {
        match input.key {
            "c" => return KeyAction::Copy,
            "v" => return KeyAction::Paste,
            _ => {}
        }
    }
    if input.shift && !input.ctrl && !input.alt && input.key == "insert" {
        return KeyAction::Paste;
    }

    let key = if let Some(k) = named_key(input.key) {
        k
    } else if input.key == "space" {
        if !(input.ctrl || input.alt) {
            return KeyAction::PassThrough;
        }
        Key::Space
    } else if input.key.chars().count() == 1 {
        if !(input.ctrl || input.alt) {
            return KeyAction::PassThrough;
        }
        Key::Character(input.key.to_ascii_lowercase())
    } else {
        return KeyAction::PassThrough;
    };

    // 文本：Ctrl 组合键没有可打印文本（GPUI 可能给出控制字符，丢弃）。
    let text = input
        .key_char
        .filter(|t| !input.ctrl && !t.is_empty() && !t.chars().any(char::is_control))
        .map(str::to_owned);
    KeyAction::Send(KeyEvent {
        key,
        mods: Modifiers {
            shift: input.shift,
            ctrl: input.ctrl,
            alt_left: input.alt,
            alt_right: false,
            meta: false,
        },
        text,
        kind: if input.is_held {
            KeyEventKind::Repeat
        } else {
            KeyEventKind::Press
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: &str) -> KeyInput<'_> {
        KeyInput {
            key,
            key_char: None,
            ctrl: false,
            alt: false,
            shift: false,
            platform: false,
            is_held: false,
        }
    }

    fn send(a: KeyAction) -> KeyEvent {
        match a {
            KeyAction::Send(e) => e,
            other => panic!("期望 Send，得到 {other:?}"),
        }
    }

    #[test]
    fn plain_printable_keys_pass_through_to_text_path() {
        for key in ["a", "z", "1", ";", "/", "space"] {
            let i = KeyInput {
                key_char: Some("x"),
                ..k(key)
            };
            assert_eq!(classify_key(&i), KeyAction::PassThrough, "{key}");
        }
        // Shift 不改变：Shift+a 走 WM_CHAR 得到 "A"。
        let i = KeyInput {
            shift: true,
            key_char: Some("A"),
            ..k("a")
        };
        assert_eq!(classify_key(&i), KeyAction::PassThrough);
    }

    #[test]
    fn named_keys_are_encoded() {
        let cases = [
            ("enter", Key::Enter),
            ("tab", Key::Tab),
            ("backspace", Key::Backspace),
            ("escape", Key::Escape),
            ("up", Key::ArrowUp),
            ("down", Key::ArrowDown),
            ("left", Key::ArrowLeft),
            ("right", Key::ArrowRight),
            ("home", Key::Home),
            ("end", Key::End),
            ("pageup", Key::PageUp),
            ("pagedown", Key::PageDown),
            ("insert", Key::Insert),
            ("delete", Key::Delete),
            ("f1", Key::F(1)),
            ("f12", Key::F(12)),
            ("f24", Key::F(24)),
        ];
        for (name, key) in cases {
            let e = send(classify_key(&k(name)));
            assert_eq!(e.key, key, "{name}");
            assert_eq!(e.mods, Modifiers::NONE);
            assert_eq!(e.kind, KeyEventKind::Press);
        }
    }

    #[test]
    fn invalid_function_keys_and_unknown_names_pass_through() {
        for key in ["f0", "f25", "fx", "menu", "back", "forward", ""] {
            assert_eq!(classify_key(&k(key)), KeyAction::PassThrough, "{key:?}");
        }
    }

    #[test]
    fn ctrl_letter_is_sent_without_text() {
        let i = KeyInput {
            ctrl: true,
            key_char: Some("\u{3}"),
            ..k("c")
        };
        let e = send(classify_key(&i));
        assert_eq!(e.key, Key::Character("c".into()));
        assert!(e.mods.ctrl && !e.mods.shift && !e.mods.alt());
        assert_eq!(e.text, None);
    }

    #[test]
    fn ctrl_space_and_ctrl_punctuation() {
        let e = send(classify_key(&KeyInput {
            ctrl: true,
            ..k("space")
        }));
        assert_eq!(e.key, Key::Space);
        let e = send(classify_key(&KeyInput {
            ctrl: true,
            ..k("[")
        }));
        assert_eq!(e.key, Key::Character("[".into()));
        let e = send(classify_key(&KeyInput {
            ctrl: true,
            shift: true,
            ..k("2")
        }));
        assert!(e.mods.ctrl && e.mods.shift);
    }

    #[test]
    fn alt_character_carries_text() {
        let i = KeyInput {
            alt: true,
            key_char: Some("x"),
            ..k("x")
        };
        let e = send(classify_key(&i));
        assert!(e.mods.alt());
        assert_eq!(e.text.as_deref(), Some("x"));
        // Alt+Shift+1：文本是 "!"。
        let i = KeyInput {
            alt: true,
            shift: true,
            key_char: Some("!"),
            ..k("1")
        };
        assert_eq!(send(classify_key(&i)).text.as_deref(), Some("!"));
    }

    #[test]
    fn alt_with_arrows_and_modified_named_keys() {
        let e = send(classify_key(&KeyInput {
            alt: true,
            ..k("left")
        }));
        assert_eq!(e.key, Key::ArrowLeft);
        assert!(e.mods.alt());
        let e = send(classify_key(&KeyInput {
            ctrl: true,
            shift: true,
            ..k("up")
        }));
        assert!(e.mods.ctrl && e.mods.shift);
        let e = send(classify_key(&KeyInput {
            shift: true,
            ..k("tab")
        }));
        assert_eq!(e.key, Key::Tab);
        assert!(e.mods.shift);
    }

    #[test]
    fn held_key_is_repeat() {
        let e = send(classify_key(&KeyInput {
            is_held: true,
            ..k("backspace")
        }));
        assert_eq!(e.kind, KeyEventKind::Repeat);
    }

    #[test]
    fn copy_paste_shortcuts() {
        assert_eq!(
            classify_key(&KeyInput {
                ctrl: true,
                shift: true,
                ..k("c")
            }),
            KeyAction::Copy
        );
        assert_eq!(
            classify_key(&KeyInput {
                ctrl: true,
                shift: true,
                ..k("v")
            }),
            KeyAction::Paste
        );
        assert_eq!(
            classify_key(&KeyInput {
                shift: true,
                ..k("insert")
            }),
            KeyAction::Paste
        );
        // 裸 Ctrl+C / Ctrl+V 照常发给 shell。
        assert!(matches!(
            classify_key(&KeyInput {
                ctrl: true,
                ..k("c")
            }),
            KeyAction::Send(_)
        ));
        assert!(matches!(
            classify_key(&KeyInput {
                ctrl: true,
                ..k("v")
            }),
            KeyAction::Send(_)
        ));
    }

    #[test]
    fn windows_key_combinations_belong_to_the_system() {
        assert_eq!(
            classify_key(&KeyInput {
                platform: true,
                ..k("enter")
            }),
            KeyAction::PassThrough
        );
        assert_eq!(
            classify_key(&KeyInput {
                platform: true,
                ctrl: true,
                ..k("c")
            }),
            KeyAction::PassThrough
        );
    }

    #[test]
    fn uppercase_key_is_normalized() {
        let e = send(classify_key(&KeyInput {
            ctrl: true,
            ..k("A")
        }));
        assert_eq!(e.key, Key::Character("a".into()));
    }
}
