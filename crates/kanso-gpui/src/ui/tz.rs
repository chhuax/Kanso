//! 本地时区偏移（文件列表的修改时间按本地时区显示，filer.md §4.1）。

// 必须调用 Win32 API，与 `memory` 一样是本 crate 里放开 unsafe 的地方。
#![allow(unsafe_code)]

use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

/// 本地时间相对 UTC 的偏移（分钟，东为正，已含夏令时）；取不到时为 0。
pub(crate) fn local_offset_minutes() -> i32 {
    // SAFETY: 全零是 `TIME_ZONE_INFORMATION`（纯整数数组与结构）的有效值，函数只写入这块缓冲区。
    let mut info: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` 是有效的输出缓冲区。
    let kind = unsafe { GetTimeZoneInformation(&mut info) };
    // 返回值：0 未知、1 标准时间、2 夏令时、0xFFFFFFFF 失败。
    let extra = match kind {
        1 => info.StandardBias,
        2 => info.DaylightBias,
        _ => return 0,
    };
    -(info.Bias + extra)
}
