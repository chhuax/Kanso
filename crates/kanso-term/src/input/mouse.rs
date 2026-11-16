//! 鼠标与滚轮上报（xterm ctlseqs「Mouse Tracking」一节）。
//!
//! 已知限制：
//! - 不支持 SGR-Pixels（DECSET 1016）：`KeyEvent` 之外的 `MouseEvent` 只有单元格坐标，没有像素；
//!   alacritty_terminal 0.25.1 也不识别 1016。
//! - 不支持 X10 兼容模式（DECSET 9）：alacritty_terminal 0.25.1 忽略它，`TermModes` 里没有对应状态。
//!   现代程序用 1000 及以上。
//! - 不做同一单元格内的移动去重：1002/1003 下每个 `Drag`/`Move` 事件都会编码，由 UI 层只在单元格
//!   变化时才调用。

use kanso_types::geometry::ViewportPoint;
use kanso_types::input::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::types::{MouseMode, TermModes};

/// 按钮码里的修饰位：Alt 8、Ctrl 16。Shift（4）永远不会出现，因为按住 Shift 时由终端自己处理。
fn mod_bits(m: &Modifiers) -> u32 {
    (if m.alt() { 8 } else { 0 }) + (if m.ctrl { 16 } else { 0 })
}

/// 把鼠标事件编码成上报字节。
///
/// 返回 `None` 表示不发给程序：程序没有开启鼠标上报、当前模式不上报这类事件、按住 Shift
/// （交给终端自己做选择与右键，所有平台一致，terminal.md §8.1），或坐标在当前编码下无法表示。
///
/// 滚轮不走这里，见 [`encode_scroll`]。
pub fn encode_mouse(event: &MouseEvent, modes: &TermModes) -> Option<Vec<u8>> {
    if event.mods.shift {
        return None;
    }
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::None => None,
    };
    let mode = modes.mouse;
    // 1003 的「无按键」码是 3，再加拖动位 32。
    let (code, release) = match event.kind {
        MouseEventKind::Down if mode != MouseMode::None => (button_code(event.button)?, false),
        MouseEventKind::Up if mode != MouseMode::None => (button_code(event.button)?, true),
        MouseEventKind::Drag if matches!(mode, MouseMode::Drag | MouseMode::Motion) => {
            (32 + button_code(event.button).unwrap_or(3), false)
        }
        MouseEventKind::Move if mode == MouseMode::Motion => (32 + 3, false),
        _ => return None,
    };
    report(code + mod_bits(&event.mods), event.point, release, modes)
}

/// 滚轮的去向。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScrollAction {
    /// 程序开启了鼠标上报：这些字节是滚轮按钮事件（每行一次），发给程序。
    /// 坐标在当前编码下无法表示时为空字节：吞掉事件，而不是退回去滚动视图（程序在管滚动）。
    Report(Vec<u8>),
    /// 备用屏幕且开了 alternate scroll（1007）：这些字节是上下方向键（遵守 DECCKM），发给程序。
    ArrowKeys(Vec<u8>),
    /// 终端自己滚动视图；数值含义同 `Terminal::scroll_display`（正数向上看更早的内容）。
    ScrollViewport(i32),
}

/// 一次滚轮事件最多重复上报的行数，防止 UI 传入异常大的值时分配巨大缓冲区。
const MAX_SCROLL_REPEAT: usize = 1000;

/// 决定一次滚轮事件的去向并编码。`lines` 正数向上、负数向下；0 行不产生任何动作。
///
/// 去向优先级：
/// 1. 程序开启了鼠标上报且没按 Shift → 按滚轮按钮（64 上 / 65 下）上报，每行一次；
/// 2. 在备用屏幕且开了 1007 → 转成方向键；
/// 3. 否则由终端滚动视图。
///
/// 按住 Shift 时不上报，理由同 [`encode_mouse`]；Shift+滚轮落到 2 或 3。
pub fn encode_scroll(
    lines: i32,
    at: ViewportPoint,
    mods: Modifiers,
    modes: &TermModes,
) -> ScrollAction {
    if lines == 0 {
        return ScrollAction::ScrollViewport(0);
    }
    let n = (lines.unsigned_abs() as usize).min(MAX_SCROLL_REPEAT);
    let up = lines > 0;
    if modes.mouse_reporting() && !mods.shift {
        let code = if up { 64 } else { 65 } + mod_bits(&mods);
        // 滚轮只有「按下」没有「松开」，所以始终不是 release。
        return ScrollAction::Report(
            report(code, at, false, modes)
                .map(|one| one.repeat(n))
                .unwrap_or_default(),
        );
    }
    if modes.alt_screen && modes.alternate_scroll {
        let final_byte = if up { b'A' } else { b'B' };
        let one = [0x1b, if modes.app_cursor { b'O' } else { b'[' }, final_byte];
        return ScrollAction::ArrowKeys(one.repeat(n));
    }
    ScrollAction::ScrollViewport(lines)
}

/// 按当前编码拼一条上报。`code` 已含按钮、修饰与拖动位；`release` 只影响编码方式：
/// SGR 用结尾 `m` 并保留原按钮；其它编码没有「哪个键松开」的信息，按钮位统一写 3。
///
/// 编码优先级 SGR > UTF-8 > 默认：程序可能同时设置过 1005 与 1006，xterm 以后设置的为准，
/// 但 SGR 是明确更可靠的格式（不受坐标范围限制），所以固定优先。
fn report(code: u32, at: ViewportPoint, release: bool, modes: &TermModes) -> Option<Vec<u8>> {
    // 终端坐标从 1 开始。
    let x = u32::from(at.col) + 1;
    let y = u32::from(at.row) + 1;
    if modes.mouse_sgr {
        let fin = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{fin}").into_bytes());
    }
    let code = if release { (code & !0b11) | 3 } else { code };
    let mut out = vec![0x1b, b'[', b'M'];
    for v in [code, x, y] {
        // 三个值都以「值 + 32」发送。
        let v = v + 32;
        if modes.mouse_utf8 {
            // 1005：大于 127 的值按 UTF-8 多字节发送，上限 U+07FF（坐标 2015）。
            if v > 0x7ff {
                return None;
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(char::from_u32(v)?.encode_utf8(&mut buf).as_bytes());
        } else {
            // 默认编码每个值只有一个字节，坐标超过 223 时无法表示。
            // 不能截断成 255：会把点击报告到错误的单元格；规格要求直接不发送。
            out.push(u8::try_from(v).ok()?);
        }
    }
    Some(out)
}
