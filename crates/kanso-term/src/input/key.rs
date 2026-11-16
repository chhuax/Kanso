//! 按键编码：传统（xterm）与 kitty keyboard protocol 两套。
//!
//! 已知限制：`kanso_types::Key` 没有修饰键本身（单按 Shift/Ctrl 等）和小键盘的变体，
//! 所以 kitty 标志 8 下「上报修饰键」和小键盘专用码点（57399..）目前无法产生。
//! 需要时走契约 PR 扩 `Key`，这里不改 `kanso-types`。
//! 另外 `KeyEvent` 不带 Caps Lock / Num Lock 状态与「基础布局键」，所以修饰参数里没有锁定位，
//! 标志 4 只实现 shifted key，不实现 base-layout key。

use kanso_types::input::{Key, KeyEvent, KeyEventKind, Modifiers};

use super::{KeyOptions, OptionAsMeta, Platform};
use crate::types::{TermModes, kitty};

const ESC: u8 = 0x1b;

/// 把一次按键编码成要发给会话的字节。
///
/// 返回 `None` 表示不发送任何字节：⌘ 组合键（terminal.md §9，⌘ 属于 UI 的快捷键层）、
/// 传统模式下的 Release、没有对应编码的键（如 F21..F24 的传统编码、未产生文本的死键）。
///
/// 调用方保证：IME 组字期间的按键不要传进来（terminal.md §16）；`modes` 取自发送这一刻的
/// `Terminal::modes()`，因为 DECCKM 和 kitty 标志随程序随时变化。
pub fn encode_key(event: &KeyEvent, modes: &TermModes, opts: &KeyOptions) -> Option<Vec<u8>> {
    // ⌘ / Win 键不发给 shell。放在最前面：kitty 模式下也一样，
    // 因为 `Key` 里没有 super 修饰位的编码需求（见 terminal.md §9）。
    if event.mods.meta {
        return None;
    }
    let m = Mods::resolve(&event.mods, opts);
    let flags = modes.kitty_keyboard;
    // 只有 DISAMBIGUATE（1）或 REPORT_ALL_KEYS（8）才改变编码形态；单独的 2/4/16 对「不使用
    // 转义码」的键没有意义（规范里它们只作用于已经用转义码发送的键），所以按传统模式处理。
    if flags & (kitty::DISAMBIGUATE | kitty::REPORT_ALL_KEYS) != 0 {
        encode_kitty(event, modes, flags, m)
    } else {
        encode_legacy(event, modes, m)
    }
}

/// 解析后的修饰键。
///
/// `alt` 与 `alt_meta` 分开是故意的：
/// - `alt` 是「物理上按着 Alt/Option」，用于非字符键（方向键、Enter、Backspace 等）。这些键系统不会
///   生成组合文本，所以 mac 上不论 Option-as-Meta 设置如何，Option+Backspace 都应该发 `ESC DEL`
///   （readline 的删除前一个单词），修饰参数也应该带 Alt。
/// - `alt_meta` 是「Alt 作为 Meta 前缀」，用于字符键：mac 上只有被设置为 Meta 的那一侧 Option 才算，
///   否则 Option+e 要发系统生成的组合字符（terminal.md §9）。
#[derive(Clone, Copy)]
struct Mods {
    mac: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
    alt_meta: bool,
}

impl Mods {
    fn resolve(m: &Modifiers, opts: &KeyOptions) -> Self {
        let alt = m.alt();
        let alt_meta = match opts.platform {
            Platform::Other => alt,
            Platform::Mac => match opts.option_as_meta {
                OptionAsMeta::Off => false,
                OptionAsMeta::Left => m.alt_left,
                OptionAsMeta::Right => m.alt_right,
                OptionAsMeta::Both => alt,
            },
        };
        Self {
            mac: opts.platform == Platform::Mac,
            shift: m.shift,
            ctrl: m.ctrl,
            alt,
            alt_meta,
        }
    }
}

/// xterm / kitty 共用的修饰参数：1 + Shift(1) + Alt(2) + Ctrl(4)。
/// Meta(8) 不会出现，因为 ⌘ 已经在入口处被拦掉。
fn modifier_param(shift: bool, alt: bool, ctrl: bool) -> u32 {
    1 + u32::from(shift) + 2 * u32::from(alt) + 4 * u32::from(ctrl)
}

/// 在字节前加 `ESC`（xterm 的 metaSendsEscape 行为：readline 的 Meta 层靠它识别 Alt+键）。
fn with_esc(alt: bool, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    if alt {
        out.push(ESC);
    }
    out.extend_from_slice(body);
    out
}

// ------------------------------------------------------------------ 键表

/// 方向键与 Home/End 的结尾字符（`CSI X` / `SS3 X`）。
fn cursor_final(key: &Key) -> Option<u8> {
    Some(match key {
        Key::ArrowUp => b'A',
        Key::ArrowDown => b'B',
        Key::ArrowRight => b'C',
        Key::ArrowLeft => b'D',
        Key::Home => b'H',
        Key::End => b'F',
        _ => return None,
    })
}

/// `CSI n ~` 形式的键编号（xterm ctlseqs 的 VT220 功能键表）。
///
/// F13..F20 取 xterm 表里的 25 26 28 29 31 32 33 34（跳过 27、30 是 VT220 历史遗留，
/// 不能「顺手补齐」成连续编号）。F21..F24 在 xterm 表里没有 `~` 形式，返回 `None`。
fn tilde_number(key: &Key) -> Option<u32> {
    Some(match key {
        Key::Insert => 2,
        Key::Delete => 3,
        Key::PageUp => 5,
        Key::PageDown => 6,
        Key::F(n) => match n {
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            12 => 24,
            13 => 25,
            14 => 26,
            15 => 28,
            16 => 29,
            17 => 31,
            18 => 32,
            19 => 33,
            20 => 34,
            _ => return None,
        },
        _ => return None,
    })
}

/// F1..F4 的结尾字符（`SS3 P/Q/R/S`）。
fn ss3_final(key: &Key) -> Option<u8> {
    match key {
        Key::F(1) => Some(b'P'),
        Key::F(2) => Some(b'Q'),
        Key::F(3) => Some(b'R'),
        Key::F(4) => Some(b'S'),
        _ => None,
    }
}

/// Ctrl 组合对应的控制字符（xterm / VT 惯例）。`None` 表示这个键没有控制字符，
/// 此时 Ctrl 被忽略、照常发字符（xterm 对 Ctrl+0、Ctrl+1、Ctrl+9 也是这样）。
///
/// `Key::Character` 是「不带修饰键时的字符」，所以 Shift 组合出来的符号要在这里按 US 布局还原：
/// Ctrl+Shift+2（`@`）、Ctrl+Shift+6（`^`）、Ctrl+Shift+-（`_`）、Ctrl+Shift+/（`?`）。
/// 其它布局下这些符号可能直接是 `Character("@")`，所以也直接接受符号本身。
fn ctrl_byte(c: char, shift: bool) -> Option<u8> {
    if c.is_ascii_alphabetic() {
        // Ctrl+Shift+字母与 Ctrl+字母相同。
        return Some(c.to_ascii_lowercase() as u8 - b'a' + 1);
    }
    // 符号本身（不依赖 Shift）。
    let direct = match c {
        ' ' | '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    };
    // `[ \ ]` 在 Shift 下是 `{ | }`，没有控制字符，不能沿用上面的结果；其余符号 Shift 与否都一样。
    if let Some(b) = direct {
        if shift && matches!(c, '[' | '\\' | ']') {
            return None;
        }
        return Some(b);
    }
    // 数字行与 `/`：xterm 把 Ctrl+2..8 映射到与 `@ [ \ ] ^ _ ?` 相同的控制字符（VT 惯例，
    // 为了在没有 `[`、`\` 等键的键盘上也能输入它们）。
    if shift {
        return match c {
            '2' => Some(0x00), // Shift+2 = @
            '6' => Some(0x1e), // Shift+6 = ^
            '-' => Some(0x1f), // Shift+- = _
            '/' => Some(0x7f), // Shift+/ = ?
            _ => None,
        };
    }
    match c {
        '2' => Some(0x00),
        '3' => Some(0x1b),
        '4' => Some(0x1c),
        '5' => Some(0x1d),
        '6' => Some(0x1e),
        '7' | '/' => Some(0x1f),
        '8' => Some(0x7f),
        _ => None,
    }
}

fn first_char(key: &Key) -> Option<char> {
    match key {
        Key::Character(s) => s.chars().next(),
        Key::Space => Some(' '),
        _ => None,
    }
}

/// 字符键没有系统文本时的兜底：不带修饰键的字符，Shift 下字母转大写。
fn fallback_char(c: char, shift: bool) -> char {
    if shift && c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    }
}

fn is_printable_text(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_control)
}

// ------------------------------------------------------------------ 传统编码

fn encode_legacy(event: &KeyEvent, modes: &TermModes, m: Mods) -> Option<Vec<u8>> {
    // 传统协议里没有「松开」。Repeat 与 Press 相同。
    if event.kind == KeyEventKind::Release {
        return None;
    }
    match &event.key {
        Key::Enter => Some(with_esc(m.alt, b"\r")),
        Key::Tab => {
            // Shift+Tab 是 CSI Z（后退制表，ECMA-48 CBT），不是 `\t`。
            let body: &[u8] = if m.shift { b"\x1b[Z" } else { b"\t" };
            Some(with_esc(m.alt, body))
        }
        // 为什么 Backspace 发 DEL（0x7f）而不是 BS（0x08）：现代 xterm（backarrowKey 默认关闭
        // 的 DECBKM 复位态）和 macOS Terminal 默认都发 0x7f，stty erase 也默认为 ^?。
        // Ctrl+Backspace 发 0x08，让程序有办法区分「删一个字符」和「删一个单词」。
        Key::Backspace => Some(with_esc(m.alt, if m.ctrl { &[0x08] } else { &[0x7f] })),
        Key::Escape => Some(with_esc(m.alt, &[ESC])),
        Key::Space | Key::Character(_) => legacy_char(event, m),
        key => legacy_named(key, modes, m),
    }
}

fn char_bytes(c: char) -> Vec<u8> {
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf).as_bytes().to_vec()
}

fn legacy_char(event: &KeyEvent, m: Mods) -> Option<Vec<u8>> {
    let c = first_char(&event.key)?;
    let text = event.text.as_deref().filter(|t| is_printable_text(t));

    let body: Vec<u8> = if let Some(b) = m.ctrl.then(|| ctrl_byte(c, m.shift)).flatten() {
        vec![b]
    } else if m.alt_meta {
        // Meta 前缀后面跟「不带 Option 的字符」。
        // 非 mac：系统文本就是 Alt 没有改变字符时的结果（含 Shift 后的符号），优先用它。
        // mac：event.text 是 Option 组合出的字符（Option+a 得到 å），绝不能用，只能用逻辑键；
        // 此时 Option+Shift+符号无从得知布局结果，只还原字母大写，是已知限制。
        match text {
            Some(t) if !m.mac => t.as_bytes().to_vec(),
            _ => char_bytes(fallback_char(c, m.shift)),
        }
    } else if let Some(t) = text {
        t.as_bytes().to_vec()
    } else if m.alt {
        // mac 上 Option 没有作为 Meta 且系统没给出文本（死键等待下一个键）：什么都不发，
        // 不能退回逻辑键，否则 Option+e 会先漏出一个 `e`。
        return None;
    } else {
        char_bytes(fallback_char(c, m.shift))
    };
    Some(with_esc(m.alt_meta, &body))
}

fn legacy_named(key: &Key, modes: &TermModes, m: Mods) -> Option<Vec<u8>> {
    let (shift, mut alt, mut ctrl) = (m.shift, m.alt, m.ctrl);

    if let Some(f) = cursor_final(key) {
        // Option/Alt+方向键单词跳转（terminal.md §9，不受 OptionAsMeta 影响）。
        // 原因：zsh / bash 默认没有绑定「纯 Alt 修饰的方向键序列」（`CSI 1;3D`），直接发出去
        // 会显示成乱码；它们认识的是 readline 的 `ESC b` / `ESC f`（backward-word / forward-word）
        // 和 Ctrl+方向键（`CSI 1;5D` / `CSI 1;5C`）。仅在没有 Shift/Ctrl 时转换，不吞掉用户
        // 明确按下的其它组合。
        let arrow = matches!(
            key,
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
        );
        if arrow && alt && !shift && !ctrl {
            match (m.mac, key) {
                (true, Key::ArrowLeft) => return Some(b"\x1bb".to_vec()),
                (true, Key::ArrowRight) => return Some(b"\x1bf".to_vec()),
                // mac 上 Option+↑/↓ 规格没有要求转换，保持 `CSI 1;3A`。
                (true, _) => {}
                (false, _) => {
                    alt = false;
                    ctrl = true;
                }
            }
        }
        let p = modifier_param(shift, alt, ctrl);
        return Some(if p == 1 {
            // DECCKM（应用光标键模式）只影响「无修饰」的方向键与 Home/End：xterm 带修饰时
            // 一律用 CSI 形式。
            vec![ESC, if modes.app_cursor { b'O' } else { b'[' }, f]
        } else {
            format!("\x1b[1;{p}{}", f as char).into_bytes()
        });
    }

    let p = modifier_param(shift, alt, ctrl);
    if let Some(f) = ss3_final(key) {
        // F1..F4 与 DECCKM 无关，永远是 SS3；带修饰时 xterm 用 `CSI 1;m P`。
        return Some(if p == 1 {
            vec![ESC, b'O', f]
        } else {
            format!("\x1b[1;{p}{}", f as char).into_bytes()
        });
    }
    let n = tilde_number(key)?;
    Some(if p == 1 {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{p}~").into_bytes()
    })
}

// ------------------------------------------------------------------ kitty 编码

/// 事件类型子参数：1 按下（默认）、2 重复、3 松开。
fn event_type(kind: KeyEventKind, report_events: bool) -> u32 {
    match kind {
        KeyEventKind::Press => 1,
        // 没有开标志 2 时，Repeat 与 Press 没有区别。
        KeyEventKind::Repeat if report_events => 2,
        KeyEventKind::Repeat => 1,
        KeyEventKind::Release => 3,
    }
}

/// 拼 `CSI code[:shifted][;mods[:event][;text]] u`。
///
/// 修饰参数与事件类型都是默认值（1）且没有文本时整段省略；有文本时修饰字段即使为默认值也要
/// 留空占位（`CSI 97;;97u`），否则文本会被解析成修饰参数。
fn csi_u(code: u32, shifted: Option<u32>, mods: u32, etype: u32, text: &[u32]) -> Vec<u8> {
    let mut s = format!("\x1b[{code}");
    if let Some(sh) = shifted {
        s.push_str(&format!(":{sh}"));
    }
    if mods != 1 || etype != 1 || !text.is_empty() {
        s.push(';');
        if mods != 1 || etype != 1 {
            s.push_str(&mods.to_string());
            if etype != 1 {
                s.push_str(&format!(":{etype}"));
            }
        }
        if !text.is_empty() {
            s.push(';');
            let joined: Vec<String> = text.iter().map(u32::to_string).collect();
            s.push_str(&joined.join(":"));
        }
    }
    s.push('u');
    s.into_bytes()
}

/// 拼 `CSI number[;mods[:event]] final`（方向键、F1..F4、`~` 键）。
fn csi_legacy_style(number: u32, mods: u32, etype: u32, final_byte: char) -> Vec<u8> {
    let mut s = format!("\x1b[{number}");
    if mods != 1 || etype != 1 {
        s.push_str(&format!(";{mods}"));
        if etype != 1 {
            s.push_str(&format!(":{etype}"));
        }
    }
    s.push(final_byte);
    s.into_bytes()
}

/// 码点（kitty 规定用「不带 Shift 的」码点，所以统一转小写）。
fn unshifted_code(c: char) -> u32 {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l as u32,
        _ => c as u32,
    }
}

/// 标志 4：Shift 下该键对应的字符（只做能确定的：系统文本恰好是单个不同字符，或 ASCII 字母大写）。
fn shifted_code(event: &KeyEvent, c: char, m: Mods) -> Option<u32> {
    if !m.shift {
        return None;
    }
    if !m.ctrl
        && !m.alt_meta
        && let Some(t) = event.text.as_deref()
    {
        let mut it = t.chars();
        if let (Some(ch), None) = (it.next(), it.next())
            && !ch.is_control()
            && ch != c
        {
            return Some(ch as u32);
        }
    }
    c.is_ascii_lowercase()
        .then(|| c.to_ascii_uppercase() as u32)
}

fn encode_kitty(event: &KeyEvent, modes: &TermModes, flags: u8, m: Mods) -> Option<Vec<u8>> {
    let all = flags & kitty::REPORT_ALL_KEYS != 0;
    let report_events = flags & kitty::REPORT_EVENT_TYPES != 0;
    let etype = event_type(event.kind, report_events);
    // Release 只有在「开了标志 2 且这个键确实用转义码发送」时才上报；
    // Enter / Tab / Backspace 即使有修饰键，没有标志 8 也不上报 Release（规范明确规定）。
    let release_ok =
        |reported: bool| event.kind != KeyEventKind::Release || (report_events && reported);

    match &event.key {
        Key::Character(_) | Key::Space => {
            let c = first_char(&event.key)?;
            let coded_by_mods = m.ctrl || m.alt_meta;
            if all || coded_by_mods {
                if !release_ok(true) {
                    return None;
                }
                let mods = modifier_param(m.shift, m.alt_meta, m.ctrl);
                let shifted = if flags & kitty::REPORT_ALTERNATE_KEYS != 0 {
                    shifted_code(event, c, m)
                } else {
                    None
                };
                // 标志 16 的关联文本：只有真正产生文本的按键才带（Ctrl/Meta 组合没有文本），
                // 松开事件不带。
                let text: Vec<u32> = if all
                    && flags & kitty::REPORT_ASSOCIATED_TEXT != 0
                    && !coded_by_mods
                    && event.kind != KeyEventKind::Release
                {
                    event
                        .text
                        .as_deref()
                        .map(|t| {
                            t.chars()
                                .filter(|ch| !ch.is_control())
                                .map(|ch| ch as u32)
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                return Some(csi_u(unshifted_code(c), shifted, mods, etype, &text));
            }
            // 标志 1 下无 Ctrl/Alt 的字符键仍是普通文本，没有 Release。
            if event.kind == KeyEventKind::Release {
                return None;
            }
            legacy_char(event, m)
        }
        Key::Enter | Key::Tab | Key::Backspace => {
            let code = match event.key {
                Key::Enter => 13,
                Key::Tab => 9,
                _ => 127,
            };
            let modified = m.shift || m.ctrl || m.alt;
            if all || modified {
                // 无修饰、无标志 8 时走传统字节（见下方分支）；其它情况用 CSI u。
                // 为什么带修饰键的 Enter 要用 CSI u：这正是 Claude Code / Codex 区分
                // Shift+Enter（换行）与 Enter（提交）的依据（terminal.md §9）。
                if !release_ok(all) {
                    return None;
                }
                Some(csi_u(
                    code,
                    None,
                    modifier_param(m.shift, m.alt, m.ctrl),
                    etype,
                    &[],
                ))
            } else if event.kind == KeyEventKind::Release {
                None
            } else {
                encode_legacy(event, modes, m)
            }
        }
        Key::Escape => {
            if !release_ok(true) {
                return None;
            }
            Some(csi_u(
                27,
                None,
                modifier_param(m.shift, m.alt, m.ctrl),
                etype,
                &[],
            ))
        }
        key => {
            if !release_ok(true) {
                return None;
            }
            let mods = modifier_param(m.shift, m.alt, m.ctrl);
            let unmodified_press = mods == 1 && etype == 1;
            if let Some(f) = cursor_final(key) {
                if unmodified_press {
                    return encode_legacy(event, modes, m);
                }
                return Some(csi_legacy_style(1, mods, etype, f as char));
            }
            if let Key::F(n) = key {
                match *n {
                    1..=4 if unmodified_press => return encode_legacy(event, modes, m),
                    // F3 带修饰时用 `CSI 13;m ~`：`CSI 1;m R` 会与光标位置报告（CPR）冲突，
                    // kitty 规范因此废弃了它。F1/F2/F4 仍用 `CSI 1;m P/Q/S`。
                    3 => return Some(csi_legacy_style(13, mods, etype, '~')),
                    1 | 2 | 4 => {
                        let f = ss3_final(key)?;
                        return Some(csi_legacy_style(1, mods, etype, f as char));
                    }
                    // F13..F35 在 kitty 里是专用码点 57376..57398，没有传统形式。
                    13..=35 => {
                        return Some(csi_u(57376 + u32::from(*n) - 13, None, mods, etype, &[]));
                    }
                    _ => {}
                }
            }
            let n = tilde_number(key)?;
            if unmodified_press {
                return encode_legacy(event, modes, m);
            }
            Some(csi_legacy_style(n, mods, etype, '~'))
        }
    }
}
