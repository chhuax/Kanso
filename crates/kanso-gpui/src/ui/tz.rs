//! 本地时区偏移（文件列表的修改时间按本地时区显示，filer.md §4.1）。

// 本地时区使用操作系统 API，不自行解析时区数据库。
#![allow(unsafe_code)]

#[cfg(windows)]
use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

/// 本地时间相对 UTC 的偏移（分钟，东为正，已含夏令时）；取不到时为 0。
#[cfg(windows)]
pub(crate) fn local_offset_minutes() -> i32 {
    // SAFETY: 全零是 `TIME_ZONE_INFORMATION`（纯整数数组与结构）的有效值，函数只写入这块缓冲区。
    let mut info: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `info` 是有效的输出缓冲区。
    let kind = unsafe { GetTimeZoneInformation(&mut info) };
    // 返回值：0 未知（没有夏令时的时区也返回它，如中国）、1 标准时间、2 夏令时、0xFFFFFFFF 失败。
    let extra = match kind {
        0 | 1 => info.StandardBias,
        2 => info.DaylightBias,
        _ => return 0,
    };
    -(info.Bias + extra)
}

/// Linux 当前本地时区偏移，包含夏令时。
#[cfg(target_os = "linux")]
pub(crate) fn local_offset_minutes() -> i32 {
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    // 输出缓冲区有效；localtime_r 成功后才读取初始化后的 tm。
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        if libc::localtime_r(&now, local.as_mut_ptr()).is_null() {
            return 0;
        }
        (local.assume_init().tm_gmtoff / 60) as i32
    }
}
