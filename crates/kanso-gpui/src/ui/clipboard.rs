//! 终端粘贴保留文件列表；GPUI 的 Windows 文本接口会把多个文件名连成一个字符串。
//!
//! Linux 没有等价的文件列表格式可直接读取，只用 GPUI 的文本 / 图片通道。
#![allow(unsafe_code)]

#[cfg(windows)]
use std::ptr::null_mut;

use gpui::{App, ClipboardEntry};
use kanso_types::input::PasteContent;
#[cfg(windows)]
use windows_sys::Win32::{
    System::{
        DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_HDROP, CF_UNICODETEXT},
    },
    UI::Shell::DragQueryFileW,
};

/// 只读打开，在离开作用域后立即交还给系统。
#[cfg(windows)]
struct Opened;

#[cfg(windows)]
impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: 只在本线程成功打开剪贴板后构造。
        unsafe { CloseClipboard() };
    }
}

#[cfg(windows)]
fn paths_or_text() -> Option<PasteContent> {
    // SAFETY: 只读访问可使用空窗口句柄；所有句柄仅在此保护作用域内使用。
    if unsafe { OpenClipboard(null_mut()) } == 0 {
        return None;
    }
    let _opened = Opened;
    let drop = unsafe { GetClipboardData(u32::from(CF_HDROP)) };
    if !drop.is_null() {
        // SAFETY: 该格式的系统句柄为 HDROP；先查询数量和各名称长度，再提供相应 UTF-16 缓冲。
        let count = unsafe { DragQueryFileW(drop, u32::MAX, null_mut(), 0) };
        let mut paths = Vec::new();
        for i in 0..count {
            let length = unsafe { DragQueryFileW(drop, i, null_mut(), 0) };
            if length == 0 {
                continue;
            }
            let mut buf = vec![0; length as usize + 1];
            let used = unsafe { DragQueryFileW(drop, i, buf.as_mut_ptr(), length + 1) };
            if used > 0 {
                paths.push(String::from_utf16_lossy(&buf[..used as usize]));
            }
        }
        if !paths.is_empty() {
            return Some(PasteContent::Files(paths));
        }
    }

    // SAFETY: CF_UNICODETEXT 是系统分配的全局内存；切片上界来自同一内存块的大小。
    let handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT)) };
    if handle.is_null() {
        return None;
    }
    let length = unsafe { GlobalSize(handle) } / size_of::<u16>();
    let ptr = unsafe { GlobalLock(handle) }.cast::<u16>();
    if ptr.is_null() {
        return None;
    }
    let words = unsafe { std::slice::from_raw_parts(ptr, length) };
    let end = words
        .iter()
        .position(|word| *word == 0)
        .unwrap_or(words.len());
    let text = String::from_utf16_lossy(&words[..end]);
    unsafe { GlobalUnlock(handle) };
    Some(PasteContent::Text(text))
}

/// 文本优先；纯图片交给核心按当前 AI 会话的规则处理。
fn text_or_image(cx: &App) -> Option<PasteContent> {
    let item = cx.read_from_clipboard()?;
    item.text().map(PasteContent::Text).or_else(|| {
        item.entries()
            .iter()
            .any(|e| matches!(e, ClipboardEntry::Image(_)))
            .then_some(PasteContent::ImageOnly)
    })
}

/// Windows 上文件优先，其次是文本；纯图片交给核心按当前 AI 会话的规则处理。
#[cfg(windows)]
pub(super) fn read_terminal_paste(cx: &App) -> Option<PasteContent> {
    paths_or_text().or_else(|| text_or_image(cx))
}

/// Linux 只用 GPUI 的文本 / 图片通道。
#[cfg(target_os = "linux")]
pub(super) fn read_terminal_paste(cx: &App) -> Option<PasteContent> {
    text_or_image(cx)
}
