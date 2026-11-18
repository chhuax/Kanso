//! 进程内存读数（只用于启动日志）。

// 必须调用 Win32 API，是本 crate 里唯一放开 unsafe 的地方。
#![allow(unsafe_code)]

use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// 当前工作集与峰值工作集（字节）。
fn working_set() -> Option<(usize, usize)> {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    // SAFETY: `counters` 是有效的、大小与 `cb` 一致的输出缓冲区；伪句柄 `GetCurrentProcess()` 恒有效。
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some((counters.WorkingSetSize, counters.PeakWorkingSetSize))
}

/// 供日志用的一行描述。
pub(crate) fn describe() -> String {
    match working_set() {
        Some((cur, peak)) => format!(
            "工作集 {:.1} MiB（峰值 {:.1} MiB）",
            cur as f64 / 1048576.0,
            peak as f64 / 1048576.0
        ),
        None => "工作集 未知".to_owned(),
    }
}
