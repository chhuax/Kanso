//! 输入编码测试：表格驱动，期望字节全部手写自 xterm ctlseqs / kitty keyboard protocol 文档，
//! 不用实现里的公式反推，避免「实现与测试一起错」。

use kanso_types::geometry::{CellSide, GridSize, ViewportPoint};
use kanso_types::input::{
    Key, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::types::kitty::{
    DISAMBIGUATE, REPORT_ALL_KEYS, REPORT_ALTERNATE_KEYS, REPORT_ASSOCIATED_TEXT,
    REPORT_EVENT_TYPES,
};
use crate::{
    KeyOptions, MouseMode, OptionAsMeta, Platform, ScrollAction, ShellFlavor, TermConfig,
    TermModes, Terminal, encode_key, encode_mouse, encode_paste, encode_scroll, quote_paths,
};

// ------------------------------------------------------------------ 公共工具

fn base_modes() -> TermModes {
    TermModes {
        alt_screen: false,
        app_cursor: false,
        app_keypad: false,
        bracketed_paste: false,
        mouse: MouseMode::None,
        mouse_sgr: false,
        mouse_utf8: false,
        focus_reporting: false,
        alternate_scroll: false,
        line_wrap: true,
        cursor_visible: true,
        line_feed_new_line: false,
        kitty_keyboard: 0,
    }
}

fn app_cursor() -> TermModes {
    TermModes {
        app_cursor: true,
        ..base_modes()
    }
}

fn kitty_modes(flags: u8) -> TermModes {
    TermModes {
        kitty_keyboard: flags,
        ..base_modes()
    }
}

const fn md(shift: bool, ctrl: bool, alt: bool) -> Modifiers {
    Modifiers {
        shift,
        ctrl,
        alt_left: alt,
        alt_right: false,
        meta: false,
    }
}

const NONE: Modifiers = Modifiers::NONE;
const SHIFT: Modifiers = md(true, false, false);
const CTRL: Modifiers = md(false, true, false);
const ALT: Modifiers = md(false, false, true);
const CTRL_SHIFT: Modifiers = md(true, true, false);

const OTHER: KeyOptions = KeyOptions {
    platform: Platform::Other,
    option_as_meta: OptionAsMeta::Off,
};

fn mac(option_as_meta: OptionAsMeta) -> KeyOptions {
    KeyOptions {
        platform: Platform::Mac,
        option_as_meta,
    }
}

fn kev(key: Key, mods: Modifiers, text: Option<&str>, kind: KeyEventKind) -> KeyEvent {
    KeyEvent {
        key,
        mods,
        text: text.map(str::to_owned),
        kind,
    }
}

fn press(key: Key, mods: Modifiers) -> KeyEvent {
    kev(key, mods, None, KeyEventKind::Press)
}

fn chr(c: &str) -> Key {
    Key::Character(c.to_owned())
}

/// 把编码结果转成可读字符串（所有按键输出都是合法 UTF-8）。
fn enc(ev: &KeyEvent, modes: &TermModes, opts: &KeyOptions) -> Option<String> {
    encode_key(ev, modes, opts).map(|b| String::from_utf8(b).expect("按键编码应是 UTF-8"))
}

fn check(label: &str, got: Option<String>, want: Option<&str>) {
    assert_eq!(got.as_deref(), want, "{label}");
}

// ------------------------------------------------------------------ 传统：特殊键

#[test]
fn legacy_simple_keys() {
    let cases: &[(&str, Key, Modifiers, &str)] = &[
        ("Enter", Key::Enter, NONE, "\r"),
        ("Shift+Enter 传统模式与 Enter 相同", Key::Enter, SHIFT, "\r"),
        ("Ctrl+Enter", Key::Enter, CTRL, "\r"),
        ("Alt+Enter", Key::Enter, ALT, "\x1b\r"),
        ("Tab", Key::Tab, NONE, "\t"),
        ("Shift+Tab", Key::Tab, SHIFT, "\x1b[Z"),
        ("Ctrl+Tab", Key::Tab, CTRL, "\t"),
        ("Alt+Tab", Key::Tab, ALT, "\x1b\t"),
        ("Backspace 发 DEL", Key::Backspace, NONE, "\x7f"),
        ("Shift+Backspace", Key::Backspace, SHIFT, "\x7f"),
        ("Ctrl+Backspace 发 BS", Key::Backspace, CTRL, "\x08"),
        ("Alt+Backspace", Key::Backspace, ALT, "\x1b\x7f"),
        (
            "Ctrl+Alt+Backspace",
            Key::Backspace,
            md(false, true, true),
            "\x1b\x08",
        ),
        ("Escape", Key::Escape, NONE, "\x1b"),
        ("Alt+Escape", Key::Escape, ALT, "\x1b\x1b"),
        ("Space", Key::Space, NONE, " "),
        ("Shift+Space", Key::Space, SHIFT, " "),
        ("Ctrl+Space 发 NUL", Key::Space, CTRL, "\0"),
        ("Ctrl+Shift+Space", Key::Space, CTRL_SHIFT, "\0"),
        ("Alt+Space", Key::Space, ALT, "\x1b "),
        (
            "Ctrl+Alt+Space",
            Key::Space,
            md(false, true, true),
            "\x1b\0",
        ),
    ];
    for (label, key, mods, want) in cases {
        for m in [base_modes(), app_cursor()] {
            check(
                label,
                enc(&press(key.clone(), *mods), &m, &OTHER),
                Some(want),
            );
        }
    }
}

#[test]
fn legacy_cursor_keys_by_modifier_and_decckm() {
    // (键, 结尾字符)
    let keys = [
        (Key::ArrowUp, 'A'),
        (Key::ArrowDown, 'B'),
        (Key::ArrowRight, 'C'),
        (Key::ArrowLeft, 'D'),
        (Key::Home, 'H'),
        (Key::End, 'F'),
    ];
    for (key, f) in keys {
        // 无修饰：DECCKM 决定 CSI / SS3。
        check(
            "无修饰",
            enc(&press(key.clone(), NONE), &base_modes(), &OTHER),
            Some(&format!("\x1b[{f}")),
        );
        check(
            "无修饰 DECCKM",
            enc(&press(key.clone(), NONE), &app_cursor(), &OTHER),
            Some(&format!("\x1bO{f}")),
        );
        // 带修饰：与 DECCKM 无关，一律 CSI 1;m X。
        for (mods, p) in [
            (SHIFT, 2),
            (CTRL, 5),
            (CTRL_SHIFT, 6),
            (md(true, false, true), 4),
        ] {
            for m in [base_modes(), app_cursor()] {
                // Shift+Alt 不触发单词跳转（只转换纯 Alt）。
                check(
                    "带修饰",
                    enc(&press(key.clone(), mods), &m, &OTHER),
                    Some(&format!("\x1b[1;{p}{f}")),
                );
            }
        }
    }
    // Home/End 的纯 Alt 没有单词跳转语义，保持 `CSI 1;3 X`。
    for (key, f) in [(Key::Home, 'H'), (Key::End, 'F')] {
        check(
            "Alt+Home/End",
            enc(&press(key, ALT), &base_modes(), &OTHER),
            Some(&format!("\x1b[1;3{f}")),
        );
    }
    // Ctrl+Alt+方向键不被转换。
    check(
        "Ctrl+Alt+Left",
        enc(
            &press(Key::ArrowLeft, md(false, true, true)),
            &base_modes(),
            &OTHER,
        ),
        Some("\x1b[1;7D"),
    );
}

#[test]
fn legacy_tilde_keys() {
    let keys = [
        (Key::Insert, 2),
        (Key::Delete, 3),
        (Key::PageUp, 5),
        (Key::PageDown, 6),
        (Key::F(5), 15),
        (Key::F(6), 17),
        (Key::F(7), 18),
        (Key::F(8), 19),
        (Key::F(9), 20),
        (Key::F(10), 21),
        (Key::F(11), 23),
        (Key::F(12), 24),
        (Key::F(13), 25),
        (Key::F(14), 26),
        (Key::F(15), 28),
        (Key::F(16), 29),
        (Key::F(17), 31),
        (Key::F(18), 32),
        (Key::F(19), 33),
        (Key::F(20), 34),
    ];
    for (key, n) in keys {
        for m in [base_modes(), app_cursor()] {
            check(
                "无修饰",
                enc(&press(key.clone(), NONE), &m, &OTHER),
                Some(&format!("\x1b[{n}~")),
            );
            for (mods, p) in [(SHIFT, 2), (ALT, 3), (CTRL, 5), (CTRL_SHIFT, 6)] {
                check(
                    "带修饰",
                    enc(&press(key.clone(), mods), &m, &OTHER),
                    Some(&format!("\x1b[{n};{p}~")),
                );
            }
        }
    }
}

#[test]
fn legacy_f1_to_f4_and_unmapped_function_keys() {
    for (n, f) in [(1u8, 'P'), (2, 'Q'), (3, 'R'), (4, 'S')] {
        for m in [base_modes(), app_cursor()] {
            check(
                "F1-4",
                enc(&press(Key::F(n), NONE), &m, &OTHER),
                Some(&format!("\x1bO{f}")),
            );
            for (mods, p) in [(SHIFT, 2), (ALT, 3), (CTRL, 5), (CTRL_SHIFT, 6)] {
                check(
                    "F1-4 带修饰",
                    enc(&press(Key::F(n), mods), &m, &OTHER),
                    Some(&format!("\x1b[1;{p}{f}")),
                );
            }
        }
    }
    // xterm 表里没有 F21..F24 的 `~` 编码，也没有 F0 / F25。
    for n in [0u8, 21, 22, 23, 24, 25, 99] {
        check(
            "无编码的功能键",
            enc(&press(Key::F(n), NONE), &base_modes(), &OTHER),
            None,
        );
    }
}

// ------------------------------------------------------------------ 传统：Ctrl

#[test]
fn ctrl_control_character_table() {
    // (字符, 是否按 Shift, 期望字节)
    let table: &[(&str, bool, u8)] = &[
        ("@", false, 0x00),
        ("2", false, 0x00),
        ("2", true, 0x00),
        ("[", false, 0x1b),
        ("3", false, 0x1b),
        ("\\", false, 0x1c),
        ("4", false, 0x1c),
        ("]", false, 0x1d),
        ("5", false, 0x1d),
        ("^", false, 0x1e),
        ("6", false, 0x1e),
        ("6", true, 0x1e),
        ("_", false, 0x1f),
        ("7", false, 0x1f),
        ("/", false, 0x1f),
        ("-", true, 0x1f),
        ("?", false, 0x7f),
        ("8", false, 0x7f),
        ("/", true, 0x7f),
    ];
    for (c, shift, byte) in table {
        let mods = md(*shift, true, false);
        let got = encode_key(&press(chr(c), mods), &base_modes(), &OTHER);
        assert_eq!(
            got,
            Some(vec![*byte]),
            "Ctrl+{}{c}",
            if *shift { "Shift+" } else { "" }
        );
    }
    // 字母：大小写、Shift 都一样。
    for (i, c) in ('a'..='z').enumerate() {
        let want = Some(vec![i as u8 + 1]);
        for (key, mods) in [
            (c.to_string(), CTRL),
            (c.to_string(), CTRL_SHIFT),
            (c.to_ascii_uppercase().to_string(), CTRL_SHIFT),
        ] {
            assert_eq!(
                encode_key(&press(chr(&key), mods), &base_modes(), &OTHER),
                want,
                "Ctrl+{key}"
            );
        }
    }
    // 没有控制字符映射的键：Ctrl 被忽略，发字符本身。
    for (c, text) in [("1", "1"), ("0", "0"), ("9", "9"), ("3", "#")] {
        let mods = if c == "3" { CTRL_SHIFT } else { CTRL };
        let ev = kev(chr(c), mods, Some(text), KeyEventKind::Press);
        check("无映射", enc(&ev, &base_modes(), &OTHER), Some(text));
    }
    // `{` 来自 Shift+[：不能当成 Ctrl+[。
    let ev = kev(chr("["), CTRL_SHIFT, Some("{"), KeyEventKind::Press);
    check("Ctrl+{", enc(&ev, &base_modes(), &OTHER), Some("{"));
}

// ------------------------------------------------------------------ 传统：Alt / Option

#[test]
fn alt_on_non_mac_is_meta_prefix() {
    let cases: &[(&str, Key, Modifiers, Option<&str>, &str)] = &[
        ("Alt+a", chr("a"), ALT, Some("a"), "\x1ba"),
        (
            "Alt+Shift+a",
            chr("a"),
            md(true, false, true),
            Some("A"),
            "\x1bA",
        ),
        (
            "Alt+Shift+1",
            chr("1"),
            md(true, false, true),
            Some("!"),
            "\x1b!",
        ),
        ("Alt+. ", chr("."), ALT, Some("."), "\x1b."),
        (
            "Ctrl+Alt+a",
            chr("a"),
            md(false, true, true),
            Some("\u{1}"),
            "\x1b\x01",
        ),
        ("Alt+中", chr("中"), ALT, Some("中"), "\x1b中"),
        ("Alt 无文本退回逻辑键", chr("b"), ALT, None, "\x1bb"),
    ];
    for (label, key, mods, text, want) in cases {
        let ev = kev(key.clone(), *mods, *text, KeyEventKind::Press);
        check(label, enc(&ev, &base_modes(), &OTHER), Some(want));
    }
    // 非 mac 平台忽略 OptionAsMeta 的取值。
    for oam in [
        OptionAsMeta::Off,
        OptionAsMeta::Left,
        OptionAsMeta::Right,
        OptionAsMeta::Both,
    ] {
        let opts = KeyOptions {
            platform: Platform::Other,
            option_as_meta: oam,
        };
        let ev = kev(chr("a"), ALT, Some("a"), KeyEventKind::Press);
        check(
            "非 mac 忽略设置",
            enc(&ev, &base_modes(), &opts),
            Some("\x1ba"),
        );
    }
}

#[test]
fn mac_option_as_meta_matrix() {
    let left = Modifiers {
        alt_left: true,
        ..NONE
    };
    let right = Modifiers {
        alt_right: true,
        ..NONE
    };
    let both = Modifiers {
        alt_left: true,
        alt_right: true,
        ..NONE
    };
    // (设置, 按下的 Option, 是否作为 Meta)
    let table = [
        (OptionAsMeta::Off, left, false),
        (OptionAsMeta::Off, right, false),
        (OptionAsMeta::Off, both, false),
        (OptionAsMeta::Left, left, true),
        (OptionAsMeta::Left, right, false),
        (OptionAsMeta::Left, both, true),
        (OptionAsMeta::Right, left, false),
        (OptionAsMeta::Right, right, true),
        (OptionAsMeta::Right, both, true),
        (OptionAsMeta::Both, left, true),
        (OptionAsMeta::Both, right, true),
        (OptionAsMeta::Both, both, true),
    ];
    for (setting, mods, meta) in table {
        // Option+e：系统生成 `´`（死键，text 为空时不发）；这里用 Option+a = å。
        let ev = kev(chr("a"), mods, Some("å"), KeyEventKind::Press);
        let want = if meta { "\x1ba" } else { "å" };
        check(
            &format!("{setting:?} {mods:?}"),
            enc(&ev, &base_modes(), &mac(setting)),
            Some(want),
        );
    }
    // 未作为 Meta 且系统没产生文本（死键）：不发送，不能漏出逻辑键。
    let ev = kev(chr("e"), left, None, KeyEventKind::Press);
    check(
        "死键",
        enc(&ev, &base_modes(), &mac(OptionAsMeta::Off)),
        None,
    );
    // 作为 Meta 时 Option+Shift+a 发 `ESC A`。
    let ev = kev(
        chr("a"),
        md(true, false, true),
        Some("Å"),
        KeyEventKind::Press,
    );
    check(
        "Meta+Shift",
        enc(&ev, &base_modes(), &mac(OptionAsMeta::Both)),
        Some("\x1bA"),
    );
    // Ctrl+Option（未作为 Meta）：Ctrl 优先，不带 ESC。
    let ev = kev(chr("a"), md(false, true, true), None, KeyEventKind::Press);
    check(
        "Ctrl+Option 未作 Meta",
        enc(&ev, &base_modes(), &mac(OptionAsMeta::Off)),
        Some("\x01"),
    );
    // 非字符键上的 Option 不受设置影响：Option+Backspace 永远是 ESC DEL。
    for setting in [OptionAsMeta::Off, OptionAsMeta::Both] {
        check(
            "Option+Backspace",
            enc(&press(Key::Backspace, ALT), &base_modes(), &mac(setting)),
            Some("\x1b\x7f"),
        );
    }
}

#[test]
fn option_arrow_word_jump() {
    for setting in [OptionAsMeta::Off, OptionAsMeta::Left, OptionAsMeta::Both] {
        let o = mac(setting);
        for m in [base_modes(), app_cursor()] {
            check(
                "mac Option+←",
                enc(&press(Key::ArrowLeft, ALT), &m, &o),
                Some("\x1bb"),
            );
            check(
                "mac Option+→",
                enc(&press(Key::ArrowRight, ALT), &m, &o),
                Some("\x1bf"),
            );
            // 规格没有要求转换 mac 的 Option+↑/↓。
            check(
                "mac Option+↑",
                enc(&press(Key::ArrowUp, ALT), &m, &o),
                Some("\x1b[1;3A"),
            );
        }
    }
    // 右 Option 同样触发，不论 OptionAsMeta。
    let right = Modifiers {
        alt_right: true,
        ..NONE
    };
    check(
        "右 Option+←",
        enc(
            &press(Key::ArrowLeft, right),
            &base_modes(),
            &mac(OptionAsMeta::Left),
        ),
        Some("\x1bb"),
    );
    // 非 mac：映射成 Ctrl+方向键。
    for (key, want) in [
        (Key::ArrowLeft, "\x1b[1;5D"),
        (Key::ArrowRight, "\x1b[1;5C"),
        (Key::ArrowUp, "\x1b[1;5A"),
        (Key::ArrowDown, "\x1b[1;5B"),
    ] {
        for m in [base_modes(), app_cursor()] {
            check(
                "Alt+方向键",
                enc(&press(key.clone(), ALT), &m, &OTHER),
                Some(want),
            );
        }
    }
    // Shift+Option+← 不是单词跳转，仍是带修饰的方向键。
    check(
        "Shift+Option+←",
        enc(
            &press(Key::ArrowLeft, md(true, false, true)),
            &base_modes(),
            &mac(OptionAsMeta::Off),
        ),
        Some("\x1b[1;4D"),
    );
}

#[test]
fn plain_text_meta_release_and_repeat() {
    let o = &OTHER;
    let m = &base_modes();
    let t = |k: &str, text: Option<&str>, mods| kev(chr(k), mods, text, KeyEventKind::Press);
    check("a", enc(&t("a", Some("a"), NONE), m, o), Some("a"));
    check("Shift+a", enc(&t("a", Some("A"), SHIFT), m, o), Some("A"));
    check("中", enc(&t("z", Some("中"), NONE), m, o), Some("中"));
    check(
        "组合字符整串",
        enc(&t("e", Some("é"), NONE), m, o),
        Some("é"),
    );
    check(
        "无文本退回逻辑键",
        enc(&t("a", None, NONE), m, o),
        Some("a"),
    );
    check(
        "无文本 Shift 大写",
        enc(&t("a", None, SHIFT), m, o),
        Some("A"),
    );

    // ⌘ 组合不发送。
    let cmd = Modifiers { meta: true, ..NONE };
    check("⌘+c", enc(&t("c", Some("c"), cmd), m, o), None);
    check("⌘+Enter", enc(&press(Key::Enter, cmd), m, o), None);
    check(
        "⌘+Left",
        enc(&press(Key::ArrowLeft, cmd), &kitty_modes(DISAMBIGUATE), o),
        None,
    );

    // 传统模式：Release 不发，Repeat 同 Press。
    let rel = kev(chr("a"), NONE, Some("a"), KeyEventKind::Release);
    check("Release", enc(&rel, m, o), None);
    let rel = kev(Key::Enter, NONE, None, KeyEventKind::Release);
    check("Enter Release", enc(&rel, m, o), None);
    let rep = kev(Key::ArrowUp, NONE, None, KeyEventKind::Repeat);
    check("Repeat", enc(&rep, m, o), Some("\x1b[A"));
}

// ------------------------------------------------------------------ kitty

#[test]
fn kitty_disambiguate_flag() {
    let m = kitty_modes(DISAMBIGUATE);
    let t = |k: &str, text: &str, mods| kev(chr(k), mods, Some(text), KeyEventKind::Press);
    let cases: Vec<(&str, KeyEvent, Option<&str>)> = vec![
        ("Enter 保持传统", press(Key::Enter, NONE), Some("\r")),
        ("Shift+Enter", press(Key::Enter, SHIFT), Some("\x1b[13;2u")),
        ("Ctrl+Enter", press(Key::Enter, CTRL), Some("\x1b[13;5u")),
        ("Alt+Enter", press(Key::Enter, ALT), Some("\x1b[13;3u")),
        (
            "Ctrl+Shift+Enter",
            press(Key::Enter, CTRL_SHIFT),
            Some("\x1b[13;6u"),
        ),
        ("Tab 保持传统", press(Key::Tab, NONE), Some("\t")),
        ("Shift+Tab", press(Key::Tab, SHIFT), Some("\x1b[9;2u")),
        (
            "Backspace 保持传统",
            press(Key::Backspace, NONE),
            Some("\x7f"),
        ),
        (
            "Ctrl+Backspace",
            press(Key::Backspace, CTRL),
            Some("\x1b[127;5u"),
        ),
        ("Escape", press(Key::Escape, NONE), Some("\x1b[27u")),
        (
            "Shift+Escape",
            press(Key::Escape, SHIFT),
            Some("\x1b[27;2u"),
        ),
        ("a 普通文本", t("a", "a", NONE), Some("a")),
        ("Shift+a 仍是文本", t("a", "A", SHIFT), Some("A")),
        (
            "Space 仍是文本",
            kev(Key::Space, NONE, Some(" "), KeyEventKind::Press),
            Some(" "),
        ),
        ("Ctrl+a", t("a", "\u{1}", CTRL), Some("\x1b[97;5u")),
        (
            "Ctrl+Shift+a 码点仍小写",
            t("a", "\u{1}", CTRL_SHIFT),
            Some("\x1b[97;6u"),
        ),
        ("Alt+a", t("a", "a", ALT), Some("\x1b[97;3u")),
        (
            "Alt+Shift+a",
            t("a", "A", md(true, false, true)),
            Some("\x1b[97;4u"),
        ),
        (
            "Ctrl+Alt+a",
            t("a", "a", md(false, true, true)),
            Some("\x1b[97;7u"),
        ),
        ("Ctrl+Space", press(Key::Space, CTRL), Some("\x1b[32;5u")),
        ("Ctrl+[", t("[", "\u{1b}", CTRL), Some("\x1b[91;5u")),
        // 方向键、Home/End、F 键、`~` 键
        ("Up", press(Key::ArrowUp, NONE), Some("\x1b[A")),
        ("Shift+Up", press(Key::ArrowUp, SHIFT), Some("\x1b[1;2A")),
        ("Ctrl+Left", press(Key::ArrowLeft, CTRL), Some("\x1b[1;5D")),
        (
            "Alt+Right 不做单词跳转",
            press(Key::ArrowRight, ALT),
            Some("\x1b[1;3C"),
        ),
        ("Home", press(Key::Home, NONE), Some("\x1b[H")),
        ("Ctrl+End", press(Key::End, CTRL), Some("\x1b[1;5F")),
        ("Insert", press(Key::Insert, NONE), Some("\x1b[2~")),
        ("Shift+Delete", press(Key::Delete, SHIFT), Some("\x1b[3;2~")),
        ("PageUp", press(Key::PageUp, NONE), Some("\x1b[5~")),
        (
            "Ctrl+PageDown",
            press(Key::PageDown, CTRL),
            Some("\x1b[6;5~"),
        ),
        ("F1", press(Key::F(1), NONE), Some("\x1bOP")),
        ("Shift+F1", press(Key::F(1), SHIFT), Some("\x1b[1;2P")),
        ("F3", press(Key::F(3), NONE), Some("\x1bOR")),
        (
            "Ctrl+F3 用 13~ 避开 CPR",
            press(Key::F(3), CTRL),
            Some("\x1b[13;5~"),
        ),
        ("Alt+F4", press(Key::F(4), ALT), Some("\x1b[1;3S")),
        ("F5", press(Key::F(5), NONE), Some("\x1b[15~")),
        ("Shift+F12", press(Key::F(12), SHIFT), Some("\x1b[24;2~")),
        ("F13", press(Key::F(13), NONE), Some("\x1b[57376u")),
        ("Ctrl+F20", press(Key::F(20), CTRL), Some("\x1b[57383;5u")),
        ("F35", press(Key::F(35), NONE), Some("\x1b[57398u")),
        ("F36 无编码", press(Key::F(36), NONE), None),
    ];
    for (label, ev, want) in cases {
        check(label, enc(&ev, &m, &OTHER), want);
    }
    // 标志 1 下 DECCKM 仍影响无修饰方向键。
    let m = TermModes {
        app_cursor: true,
        ..kitty_modes(DISAMBIGUATE)
    };
    check(
        "DECCKM",
        enc(&press(Key::ArrowUp, NONE), &m, &OTHER),
        Some("\x1bOA"),
    );
    check(
        "DECCKM+Shift",
        enc(&press(Key::ArrowUp, SHIFT), &m, &OTHER),
        Some("\x1b[1;2A"),
    );
}

#[test]
fn kitty_mac_option_without_meta_stays_text() {
    let m = kitty_modes(DISAMBIGUATE);
    let ev = kev(chr("a"), ALT, Some("å"), KeyEventKind::Press);
    check(
        "Option 未作 Meta",
        enc(&ev, &m, &mac(OptionAsMeta::Off)),
        Some("å"),
    );
    check(
        "Option 作 Meta",
        enc(&ev, &m, &mac(OptionAsMeta::Both)),
        Some("\x1b[97;3u"),
    );
    // 非字符键上的 Option 始终带 Alt 修饰。
    check(
        "Option+Enter",
        enc(&press(Key::Enter, ALT), &m, &mac(OptionAsMeta::Off)),
        Some("\x1b[13;3u"),
    );
}

#[test]
fn kitty_event_types_flag() {
    let m = kitty_modes(DISAMBIGUATE | REPORT_EVENT_TYPES);
    let k = |key: Key, mods, text: Option<&str>, kind| kev(key, mods, text, kind);
    use KeyEventKind::{Press, Release, Repeat};
    let cases: Vec<(&str, KeyEvent, Option<&str>)> = vec![
        (
            "Esc Press",
            k(Key::Escape, NONE, None, Press),
            Some("\x1b[27u"),
        ),
        (
            "Esc Repeat",
            k(Key::Escape, NONE, None, Repeat),
            Some("\x1b[27;1:2u"),
        ),
        (
            "Esc Release",
            k(Key::Escape, NONE, None, Release),
            Some("\x1b[27;1:3u"),
        ),
        (
            "Ctrl+a Release",
            k(chr("a"), CTRL, None, Release),
            Some("\x1b[97;5:3u"),
        ),
        (
            "Ctrl+a Repeat",
            k(chr("a"), CTRL, None, Repeat),
            Some("\x1b[97;5:2u"),
        ),
        (
            "Up Release",
            k(Key::ArrowUp, NONE, None, Release),
            Some("\x1b[1;1:3A"),
        ),
        (
            "Shift+Up Repeat",
            k(Key::ArrowUp, SHIFT, None, Repeat),
            Some("\x1b[1;2:2A"),
        ),
        (
            "F5 Release",
            k(Key::F(5), NONE, None, Release),
            Some("\x1b[15;1:3~"),
        ),
        (
            "F1 Release",
            k(Key::F(1), NONE, None, Release),
            Some("\x1b[1;1:3P"),
        ),
        (
            "F3 Release",
            k(Key::F(3), NONE, None, Release),
            Some("\x1b[13;1:3~"),
        ),
        // 没有标志 8：文本键、无修饰的 Enter/Tab/Backspace 没有 Release。
        ("a Release", k(chr("a"), NONE, Some("a"), Release), None),
        (
            "a Repeat 仍是文本",
            k(chr("a"), NONE, Some("a"), Repeat),
            Some("a"),
        ),
        ("Enter Release", k(Key::Enter, NONE, None, Release), None),
        ("Tab Release", k(Key::Tab, NONE, None, Release), None),
        (
            "Backspace Release",
            k(Key::Backspace, NONE, None, Release),
            None,
        ),
        (
            "Shift+Enter Release 也没有",
            k(Key::Enter, SHIFT, None, Release),
            None,
        ),
        (
            "Shift+Enter Repeat",
            k(Key::Enter, SHIFT, None, Repeat),
            Some("\x1b[13;2:2u"),
        ),
        (
            "Enter Repeat 传统",
            k(Key::Enter, NONE, None, Repeat),
            Some("\r"),
        ),
    ];
    for (label, ev, want) in cases {
        check(label, enc(&ev, &m, &OTHER), want);
    }
    // 没有标志 2：Release 一律不发，Repeat 同 Press。
    let m = kitty_modes(DISAMBIGUATE);
    check(
        "无标志2 Release",
        enc(&k(Key::Escape, NONE, None, Release), &m, &OTHER),
        None,
    );
    check(
        "无标志2 Repeat",
        enc(&k(Key::Escape, NONE, None, Repeat), &m, &OTHER),
        Some("\x1b[27u"),
    );
}

#[test]
fn kitty_report_all_keys_flag() {
    let m = kitty_modes(REPORT_ALL_KEYS);
    use KeyEventKind::{Press, Release};
    let cases: Vec<(&str, KeyEvent, Option<&str>)> = vec![
        ("a", kev(chr("a"), NONE, Some("a"), Press), Some("\x1b[97u")),
        (
            "Shift+a",
            kev(chr("a"), SHIFT, Some("A"), Press),
            Some("\x1b[97;2u"),
        ),
        (
            "Space",
            kev(Key::Space, NONE, Some(" "), Press),
            Some("\x1b[32u"),
        ),
        (
            "中",
            kev(chr("中"), NONE, Some("中"), Press),
            Some("\x1b[20013u"),
        ),
        ("Enter", press(Key::Enter, NONE), Some("\x1b[13u")),
        ("Tab", press(Key::Tab, NONE), Some("\x1b[9u")),
        ("Backspace", press(Key::Backspace, NONE), Some("\x1b[127u")),
        ("Up 仍是传统形式", press(Key::ArrowUp, NONE), Some("\x1b[A")),
        // 没有标志 2 时仍没有 Release。
        (
            "a Release 无标志2",
            kev(chr("a"), NONE, Some("a"), Release),
            None,
        ),
    ];
    for (label, ev, want) in cases {
        check(label, enc(&ev, &m, &OTHER), want);
    }
    // 标志 8 + 2：Enter 等也有 Release；文本键有 Release。
    let m = kitty_modes(REPORT_ALL_KEYS | REPORT_EVENT_TYPES);
    check(
        "Enter Release",
        enc(&kev(Key::Enter, NONE, None, Release), &m, &OTHER),
        Some("\x1b[13;1:3u"),
    );
    check(
        "a Release",
        enc(&kev(chr("a"), NONE, Some("a"), Release), &m, &OTHER),
        Some("\x1b[97;1:3u"),
    );
}

#[test]
fn kitty_associated_text_and_shifted_key() {
    use KeyEventKind::{Press, Release};
    let m = kitty_modes(REPORT_ALL_KEYS | REPORT_ASSOCIATED_TEXT);
    let cases: Vec<(&str, KeyEvent, Option<&str>)> = vec![
        (
            "a 带文本",
            kev(chr("a"), NONE, Some("a"), Press),
            Some("\x1b[97;;97u"),
        ),
        (
            "Shift+a 带文本",
            kev(chr("a"), SHIFT, Some("A"), Press),
            Some("\x1b[97;2;65u"),
        ),
        (
            "多码点",
            kev(chr("e"), NONE, Some("e\u{301}"), Press),
            Some("\x1b[101;;101:769u"),
        ),
        (
            "Ctrl+a 不带文本",
            kev(chr("a"), CTRL, Some("\u{1}"), Press),
            Some("\x1b[97;5u"),
        ),
        (
            "Enter 不带文本",
            kev(Key::Enter, NONE, Some("\r"), Press),
            Some("\x1b[13u"),
        ),
        (
            "Release 不带文本",
            kev(chr("a"), NONE, Some("a"), Release),
            None,
        ),
    ];
    for (label, ev, want) in cases {
        check(label, enc(&ev, &m, &OTHER), want);
    }
    // 标志 4：Shift 下附带 shifted key。
    let m = kitty_modes(DISAMBIGUATE | REPORT_ALTERNATE_KEYS);
    check(
        "Ctrl+Shift+a",
        enc(&kev(chr("a"), CTRL_SHIFT, None, Press), &m, &OTHER),
        Some("\x1b[97:65;6u"),
    );
    check(
        "Ctrl+a 无 shifted",
        enc(&kev(chr("a"), CTRL, None, Press), &m, &OTHER),
        Some("\x1b[97;5u"),
    );
    let m = kitty_modes(REPORT_ALL_KEYS | REPORT_ALTERNATE_KEYS);
    check(
        "Shift+1 取系统文本",
        enc(&kev(chr("1"), SHIFT, Some("!"), Press), &m, &OTHER),
        Some("\x1b[49:33;2u"),
    );
    check(
        "无 Shift 不附带",
        enc(&kev(chr("a"), NONE, Some("a"), Press), &m, &OTHER),
        Some("\x1b[97u"),
    );
}

#[test]
fn kitty_only_non_encoding_flags_use_legacy() {
    // 单独的 2 / 4 / 16 不改变编码形态。
    for flags in [
        REPORT_EVENT_TYPES,
        REPORT_ALTERNATE_KEYS,
        REPORT_ASSOCIATED_TEXT,
    ] {
        let m = kitty_modes(flags);
        check(
            "Enter",
            enc(&press(Key::Enter, SHIFT), &m, &OTHER),
            Some("\r"),
        );
        check(
            "Esc",
            enc(&press(Key::Escape, NONE), &m, &OTHER),
            Some("\x1b"),
        );
    }
}

// ------------------------------------------------------------------ kitty 状态（Handler 包装）

fn term() -> Terminal {
    Terminal::new(TermConfig {
        size: GridSize { cols: 20, rows: 5 },
        scrollback_lines: 10,
    })
}

#[test]
fn kitty_query_set_push_pop_via_terminal() {
    let mut t = term();
    assert_eq!(t.modes().kitty_keyboard, 0);
    // 查询：当前为 0。
    t.feed(b"\x1b[?u", 0);
    assert_eq!(t.take_replies(), b"\x1b[?0u");

    // push 标志 1 后查询得 1，并反映到 TermModes。
    t.feed(b"\x1b[>1u\x1b[?u", 0);
    assert_eq!(t.take_replies(), b"\x1b[?1u");
    assert_eq!(t.modes().kitty_keyboard, DISAMBIGUATE);

    // 再 push 一层 1|8；pop 一层回到 1。
    t.feed(b"\x1b[>9u\x1b[?u", 0);
    assert_eq!(t.take_replies(), b"\x1b[?9u");
    assert_eq!(t.modes().kitty_keyboard, DISAMBIGUATE | REPORT_ALL_KEYS);
    t.feed(b"\x1b[<u\x1b[?u", 0);
    assert_eq!(t.take_replies(), b"\x1b[?1u");
    assert_eq!(t.modes().kitty_keyboard, DISAMBIGUATE);

    // set（mode 2 置位、mode 3 清位）。
    t.feed(b"\x1b[=2;2u", 0);
    assert_eq!(t.modes().kitty_keyboard, DISAMBIGUATE | REPORT_EVENT_TYPES);
    t.feed(b"\x1b[=1;3u", 0);
    assert_eq!(t.modes().kitty_keyboard, REPORT_EVENT_TYPES);

    // 弹出全部：回到 0。
    t.feed(b"\x1b[<5u\x1b[?u", 0);
    assert_eq!(t.take_replies(), b"\x1b[?0u");
    assert_eq!(t.modes().kitty_keyboard, 0);
}

#[test]
fn terminal_modes_feed_key_encoding() {
    // 端到端：程序发 DECCKM 与 kitty push，编码结果随之变化。
    let mut t = term();
    t.feed(b"\x1b[?1h", 0);
    assert_eq!(
        enc(&press(Key::ArrowUp, NONE), &t.modes(), &OTHER).as_deref(),
        Some("\x1bOA")
    );
    t.feed(b"\x1b[>1u", 0);
    assert_eq!(
        enc(&press(Key::Enter, SHIFT), &t.modes(), &OTHER).as_deref(),
        Some("\x1b[13;2u")
    );
}

// ------------------------------------------------------------------ 鼠标

fn pt(col: u16, row: u16) -> ViewportPoint {
    ViewportPoint {
        col,
        row,
        side: CellSide::Left,
    }
}

fn mouse(
    kind: MouseEventKind,
    button: MouseButton,
    mods: Modifiers,
    p: ViewportPoint,
) -> MouseEvent {
    MouseEvent {
        kind,
        button,
        point: p,
        mods,
        click_count: 1,
    }
}

fn mouse_modes(mode: MouseMode, sgr: bool, utf8: bool) -> TermModes {
    TermModes {
        mouse: mode,
        mouse_sgr: sgr,
        mouse_utf8: utf8,
        ..base_modes()
    }
}

#[test]
fn mouse_sgr_encoding_table() {
    use MouseButton::{Left, Middle, None as NoBtn, Right};
    use MouseEventKind::{Down, Drag, Move, Up};
    let alt = md(false, false, true);
    let ctrl = CTRL;
    let both = md(false, true, true);
    // (模式, 事件, 期望；None 表示不上报)
    let p = pt(4, 9); // 终端坐标 (5, 10)
    #[allow(clippy::type_complexity)]
    let table: Vec<(MouseMode, MouseEvent, Option<&str>)> = vec![
        // 1000：只报按下/松开
        (
            MouseMode::Click,
            mouse(Down, Left, NONE, p),
            Some("\x1b[<0;5;10M"),
        ),
        (
            MouseMode::Click,
            mouse(Down, Middle, NONE, p),
            Some("\x1b[<1;5;10M"),
        ),
        (
            MouseMode::Click,
            mouse(Down, Right, NONE, p),
            Some("\x1b[<2;5;10M"),
        ),
        (
            MouseMode::Click,
            mouse(Up, Left, NONE, p),
            Some("\x1b[<0;5;10m"),
        ),
        (
            MouseMode::Click,
            mouse(Up, Right, NONE, p),
            Some("\x1b[<2;5;10m"),
        ),
        (MouseMode::Click, mouse(Drag, Left, NONE, p), None),
        (MouseMode::Click, mouse(Move, NoBtn, NONE, p), None),
        // 修饰键位：Alt 8、Ctrl 16
        (
            MouseMode::Click,
            mouse(Down, Left, alt, p),
            Some("\x1b[<8;5;10M"),
        ),
        (
            MouseMode::Click,
            mouse(Down, Left, ctrl, p),
            Some("\x1b[<16;5;10M"),
        ),
        (
            MouseMode::Click,
            mouse(Down, Right, both, p),
            Some("\x1b[<26;5;10M"),
        ),
        (
            MouseMode::Click,
            mouse(Up, Left, ctrl, p),
            Some("\x1b[<16;5;10m"),
        ),
        // 1002：另报按住拖动（+32）
        (
            MouseMode::Drag,
            mouse(Drag, Left, NONE, p),
            Some("\x1b[<32;5;10M"),
        ),
        (
            MouseMode::Drag,
            mouse(Drag, Right, alt, p),
            Some("\x1b[<42;5;10M"),
        ),
        (MouseMode::Drag, mouse(Move, NoBtn, NONE, p), None),
        (
            MouseMode::Drag,
            mouse(Down, Left, NONE, p),
            Some("\x1b[<0;5;10M"),
        ),
        // 1003：所有移动（无按键码 3，+32）
        (
            MouseMode::Motion,
            mouse(Move, NoBtn, NONE, p),
            Some("\x1b[<35;5;10M"),
        ),
        (
            MouseMode::Motion,
            mouse(Move, NoBtn, ctrl, p),
            Some("\x1b[<51;5;10M"),
        ),
        (
            MouseMode::Motion,
            mouse(Drag, Middle, NONE, p),
            Some("\x1b[<33;5;10M"),
        ),
        // 未开启
        (MouseMode::None, mouse(Down, Left, NONE, p), None),
        (MouseMode::None, mouse(Move, NoBtn, NONE, p), None),
        // 没有按键的按下/松开没有意义
        (MouseMode::Click, mouse(Down, NoBtn, NONE, p), None),
        // 大坐标 SGR 没有上限
        (
            MouseMode::Click,
            mouse(Down, Left, NONE, pt(999, 499)),
            Some("\x1b[<0;1000;500M"),
        ),
    ];
    for (mode, ev, want) in table {
        let got = encode_mouse(&ev, &mouse_modes(mode, true, false));
        assert_eq!(
            got.map(|b| String::from_utf8(b).unwrap()).as_deref(),
            want,
            "{mode:?} {ev:?}"
        );
    }
}

#[test]
fn mouse_default_encoding() {
    use MouseButton::{Left, Right};
    use MouseEventKind::{Down, Drag, Up};
    let m = mouse_modes(MouseMode::Drag, false, false);
    // 值 + 32：按钮 0 → 32 (' ')，坐标 (5,10) → 37, 42。
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(4, 9)), &m),
        Some(vec![0x1b, b'[', b'M', 32, 37, 42])
    );
    // 默认编码的松开不带按钮信息：按钮位为 3。
    assert_eq!(
        encode_mouse(&mouse(Up, Right, NONE, pt(4, 9)), &m),
        Some(vec![0x1b, b'[', b'M', 35, 37, 42])
    );
    // 松开保留修饰位：Ctrl(16) + 3 + 32 = 51。
    assert_eq!(
        encode_mouse(&mouse(Up, Left, CTRL, pt(0, 0)), &m),
        Some(vec![0x1b, b'[', b'M', 51, 33, 33])
    );
    // 拖动 +32。
    assert_eq!(
        encode_mouse(&mouse(Drag, Left, NONE, pt(0, 0)), &m),
        Some(vec![0x1b, b'[', b'M', 64, 33, 33])
    );
    // 边界：终端坐标 223 → 255 可表示；224 不可表示 → 不发送。
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(222, 0)), &m),
        Some(vec![0x1b, b'[', b'M', 32, 255, 33])
    );
    assert_eq!(encode_mouse(&mouse(Down, Left, NONE, pt(223, 0)), &m), None);
    assert_eq!(encode_mouse(&mouse(Down, Left, NONE, pt(0, 223)), &m), None);
}

#[test]
fn mouse_utf8_encoding() {
    use MouseButton::Left;
    use MouseEventKind::Down;
    let m = mouse_modes(MouseMode::Click, false, true);
    // 小值与默认编码相同。
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(4, 9)), &m),
        Some(vec![0x1b, b'[', b'M', 32, 37, 42])
    );
    // 坐标 224 → 256 → U+0100 → C4 80。
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(223, 0)), &m),
        Some(vec![0x1b, b'[', b'M', 32, 0xc4, 0x80, 33])
    );
    // 上限：值 2047（U+07FF）可表示，坐标 2015；2016 不可表示。
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(2014, 0)), &m),
        Some(vec![0x1b, b'[', b'M', 32, 0xdf, 0xbf, 33])
    );
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(2015, 0)), &m),
        None
    );
    // SGR 与 UTF-8 同时开启时 SGR 优先。
    let m = mouse_modes(MouseMode::Click, true, true);
    assert_eq!(
        encode_mouse(&mouse(Down, Left, NONE, pt(0, 0)), &m),
        Some(b"\x1b[<0;1;1M".to_vec())
    );
}

#[test]
fn mouse_shift_never_reports() {
    use MouseEventKind::{Down, Drag, Move, Up};
    for sgr in [false, true] {
        let m = mouse_modes(MouseMode::Motion, sgr, false);
        for kind in [Down, Up, Drag, Move] {
            let ev = mouse(kind, MouseButton::Left, SHIFT, pt(1, 1));
            assert_eq!(encode_mouse(&ev, &m), None, "{kind:?}");
            // Shift+Ctrl 也不上报。
            let ev = mouse(kind, MouseButton::Left, CTRL_SHIFT, pt(1, 1));
            assert_eq!(encode_mouse(&ev, &m), None);
        }
    }
}

// ------------------------------------------------------------------ 滚轮

#[test]
fn scroll_destinations() {
    let p = pt(4, 9);
    // 1. 开了鼠标上报：按滚轮按钮，每行一次。
    let m = mouse_modes(MouseMode::Click, true, false);
    assert_eq!(
        encode_scroll(1, p, NONE, &m),
        ScrollAction::Report(b"\x1b[<64;5;10M".to_vec())
    );
    assert_eq!(
        encode_scroll(-2, p, NONE, &m),
        ScrollAction::Report(b"\x1b[<65;5;10M\x1b[<65;5;10M".to_vec())
    );
    assert_eq!(
        encode_scroll(1, p, CTRL, &m),
        ScrollAction::Report(b"\x1b[<80;5;10M".to_vec())
    );
    assert_eq!(
        encode_scroll(1, p, ALT, &m),
        ScrollAction::Report(b"\x1b[<72;5;10M".to_vec())
    );
    // 默认编码：64 + 32 = 96。
    let m = mouse_modes(MouseMode::Motion, false, false);
    assert_eq!(
        encode_scroll(1, p, NONE, &m),
        ScrollAction::Report(vec![0x1b, b'[', b'M', 96, 37, 42])
    );
    // 坐标无法表示：吞掉，不退回滚动视图。
    assert_eq!(
        encode_scroll(1, pt(300, 0), NONE, &m),
        ScrollAction::Report(Vec::new())
    );
    // 即使在备用屏幕且开了 1007，鼠标上报优先。
    let m = TermModes {
        alt_screen: true,
        alternate_scroll: true,
        ..mouse_modes(MouseMode::Click, true, false)
    };
    assert!(matches!(
        encode_scroll(1, p, NONE, &m),
        ScrollAction::Report(_)
    ));

    // 2. 没开上报、备用屏幕 + 1007：方向键，遵守 DECCKM。
    let m = TermModes {
        alt_screen: true,
        alternate_scroll: true,
        ..base_modes()
    };
    assert_eq!(
        encode_scroll(2, p, NONE, &m),
        ScrollAction::ArrowKeys(b"\x1b[A\x1b[A".to_vec())
    );
    assert_eq!(
        encode_scroll(-1, p, NONE, &m),
        ScrollAction::ArrowKeys(b"\x1b[B".to_vec())
    );
    let m = TermModes {
        app_cursor: true,
        ..m
    };
    assert_eq!(
        encode_scroll(1, p, NONE, &m),
        ScrollAction::ArrowKeys(b"\x1bOA".to_vec())
    );
    assert_eq!(
        encode_scroll(-1, p, NONE, &m),
        ScrollAction::ArrowKeys(b"\x1bOB".to_vec())
    );

    // 3. 其它：滚动视图。
    assert_eq!(
        encode_scroll(3, p, NONE, &base_modes()),
        ScrollAction::ScrollViewport(3)
    );
    assert_eq!(
        encode_scroll(-4, p, NONE, &base_modes()),
        ScrollAction::ScrollViewport(-4)
    );
    // 备用屏幕但没开 1007。
    let m = TermModes {
        alt_screen: true,
        ..base_modes()
    };
    assert_eq!(
        encode_scroll(1, p, NONE, &m),
        ScrollAction::ScrollViewport(1)
    );
    // 开了 1007 但在主屏幕。
    let m = TermModes {
        alternate_scroll: true,
        ..base_modes()
    };
    assert_eq!(
        encode_scroll(1, p, NONE, &m),
        ScrollAction::ScrollViewport(1)
    );
    // 0 行。
    assert_eq!(
        encode_scroll(0, p, NONE, &base_modes()),
        ScrollAction::ScrollViewport(0)
    );
}

#[test]
fn scroll_with_shift_is_not_reported() {
    let p = pt(0, 0);
    let m = mouse_modes(MouseMode::Click, true, false);
    assert_eq!(
        encode_scroll(2, p, SHIFT, &m),
        ScrollAction::ScrollViewport(2)
    );
    // 备用屏幕 + 1007 + Shift：仍转成方向键。
    let m = TermModes {
        alt_screen: true,
        alternate_scroll: true,
        ..m
    };
    assert_eq!(
        encode_scroll(1, p, SHIFT, &m),
        ScrollAction::ArrowKeys(b"\x1b[A".to_vec())
    );
}

#[test]
fn scroll_repeat_is_capped() {
    let m = mouse_modes(MouseMode::Click, true, false);
    match encode_scroll(i32::MAX, pt(0, 0), NONE, &m) {
        ScrollAction::Report(b) => assert_eq!(b.len(), b"\x1b[<64;1;1M".len() * 1000),
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------ 粘贴

fn bracketed() -> TermModes {
    TermModes {
        bracketed_paste: true,
        ..base_modes()
    }
}

#[test]
fn paste_encoding() {
    let cases: &[(&str, &str, bool, &str)] = &[
        ("纯文本", "hello", false, "hello"),
        ("bracketed 包裹", "hello", true, "\x1b[200~hello\x1b[201~"),
        ("空文本 bracketed", "", true, "\x1b[200~\x1b[201~"),
        ("空文本", "", false, ""),
        ("LF → CR", "a\nb", false, "a\rb"),
        ("CRLF → CR", "a\r\nb", false, "a\rb"),
        ("CR 保持", "a\rb", false, "a\rb"),
        ("连续空行", "a\n\n\nb", false, "a\r\r\rb"),
        ("CR CR LF", "a\r\r\nb", false, "a\r\rb"),
        ("末尾换行", "ls\n", false, "ls\r"),
        (
            "bracketed 内换行",
            "a\r\nb\n",
            true,
            "\x1b[200~a\rb\r\x1b[201~",
        ),
        // 安全：bracketed 下 ESC 被删除，注入的结束标记失效。
        (
            "注入 ESC[201~",
            "safe\x1b[201~rm -rf ~\n",
            true,
            "\x1b[200~safe[201~rm -rf ~\r\x1b[201~",
        ),
        (
            "多个 ESC",
            "\x1b\x1b[200~x",
            true,
            "\x1b[200~[200~x\x1b[201~",
        ),
        (
            "中文与 emoji 不动",
            "你好😀",
            true,
            "\x1b[200~你好😀\x1b[201~",
        ),
        // 没开 bracketed 时不剔除 ESC（与其它终端一致，见 encode_paste 文档）。
        ("非 bracketed 保留 ESC", "a\x1bb", false, "a\x1bb"),
    ];
    for (label, text, br, want) in cases {
        let m = if *br { bracketed() } else { base_modes() };
        assert_eq!(
            String::from_utf8(encode_paste(text, &m)).unwrap(),
            *want,
            "{label}"
        );
    }
    // 结果里只有一对首尾包裹标记。
    let out = String::from_utf8(encode_paste("x\x1b[201~y\x1b[200~z", &bracketed())).unwrap();
    assert_eq!(out.matches('\x1b').count(), 2);
    assert!(out.starts_with("\x1b[200~") && out.ends_with("\x1b[201~"));
}

fn paths(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn quote_paths_posix() {
    let cases: &[(&str, &str)] = &[
        ("/usr/bin/ls", "/usr/bin/ls"),
        ("/Users/a b/c.txt", "/Users/a\\ b/c.txt"),
        ("/tmp/it's", "/tmp/it\\'s"),
        ("/tmp/say \"hi\"", "/tmp/say\\ \\\"hi\\\""),
        ("/tmp/$HOME", "/tmp/\\$HOME"),
        ("/tmp/`pwd`", "/tmp/\\`pwd\\`"),
        ("/tmp/a\\b", "/tmp/a\\\\b"),
        ("/tmp/文档 报告.pdf", "/tmp/文档\\ 报告.pdf"),
        ("/tmp/(1)[2]{3}", "/tmp/\\(1\\)\\[2\\]\\{3\\}"),
        ("/tmp/a&b;c|d<e>f", "/tmp/a\\&b\\;c\\|d\\<e\\>f"),
        ("/tmp/!x*y?z#w~v^u", "/tmp/\\!x\\*y\\?z\\#w\\~v\\^u"),
        ("/tmp/a\tb", "/tmp/a$'\\t'b"),
        ("/tmp/a\nb", "/tmp/a$'\\n'b"),
        ("/tmp/a\rb", "/tmp/a$'\\r'b"),
        ("/tmp/a\x1bb", "/tmp/a$'\\x1b'b"),
        ("/tmp/a-b_c.d=e,f%g+h@i:j", "/tmp/a-b_c.d=e,f%g+h@i:j"),
    ];
    for (input, want) in cases {
        assert_eq!(
            quote_paths(&paths(&[input]), ShellFlavor::Posix),
            *want,
            "{input:?}"
        );
    }
    assert_eq!(
        quote_paths(&paths(&["/a b", "/c", "/中 文"]), ShellFlavor::Posix),
        "/a\\ b /c /中\\ 文"
    );
    assert_eq!(quote_paths(&[], ShellFlavor::Posix), "");
}

#[test]
fn quote_paths_powershell() {
    let cases: &[(&str, &str)] = &[
        ("C:\\Users\\me\\a.txt", "C:\\Users\\me\\a.txt"),
        ("C:\\Program Files\\x", "C:\\Program` Files\\x"),
        ("C:\\it's", "C:\\it`'s"),
        ("C:\\say \"hi\"", "C:\\say` `\"hi`\""),
        ("C:\\$env", "C:\\`$env"),
        ("C:\\a`b", "C:\\a``b"),
        ("C:\\文档 报告.pdf", "C:\\文档` 报告.pdf"),
        ("C:\\(1)[2]{3}", "C:\\`(1`)`[2`]`{3`}"),
        ("C:\\a;b,c&d|e", "C:\\a`;b`,c`&d`|e"),
        ("C:\\@x#y<z>w", "C:\\`@x`#y`<z`>w"),
        ("C:\\\u{2018}q\u{2019}", "C:\\`\u{2018}q`\u{2019}"),
        ("C:\\a\tb", "C:\\a`tb"),
        ("C:\\a\nb", "C:\\a`nb"),
        ("C:\\a\x1bb", "C:\\ab"),
        (
            "C:\\a-b_c.d=e%f+g:h~i^j!k*l?m",
            "C:\\a-b_c.d=e%f+g:h~i^j!k*l?m",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(
            quote_paths(&paths(&[input]), ShellFlavor::PowerShell),
            *want,
            "{input:?}"
        );
    }
    assert_eq!(
        quote_paths(&paths(&["C:\\a b", "D:\\c"]), ShellFlavor::PowerShell),
        "C:\\a` b D:\\c"
    );
    assert_eq!(quote_paths(&[], ShellFlavor::PowerShell), "");
}

#[test]
fn quote_paths_roundtrip_has_no_unescaped_specials() {
    // POSIX：转义后，除了被反斜杠保护的字符，不应出现任何裸的元字符。
    let nasty = "a b'c\"d\\e$f`g!h&i*j(k)l[m]n{o}p;q<r>s|t?u#v~w^x";
    let out = quote_paths(&paths(&[nasty]), ShellFlavor::Posix);
    let mut chars = out.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else {
            assert!(
                !" '\"$`!&*()[]{};<>|?#~^".contains(c),
                "裸元字符 {c:?} 在 {out:?}"
            );
        }
    }
}
