//! 查询指定进程的当前工作目录（local-shell.md §6）。

use std::path::PathBuf;

/// 读取进程 `pid` 的当前工作目录；进程已退出、没有权限或平台不支持时返回 `None`。
///
/// - macOS：`proc_pidinfo(PROC_PIDVNODEPATHINFO)`，返回的是解析过符号链接的真实路径
///   （`/tmp` 会得到 `/private/tmp`）。
/// - Linux：读符号链接 `/proc/<pid>/cwd`。
/// - 其他平台（含 Windows）没有对应的系统查询，只能依靠 OSC 7 等上报。
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    imp::process_cwd(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    pub fn process_cwd(pid: u32) -> Option<PathBuf> {
        let pid = libc::c_int::try_from(pid).ok()?;
        // SAFETY: `proc_vnodepathinfo` 是只含整数与字符数组的 C 结构体，全 0 是合法值。
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        // SAFETY: `info` 是有效的可写缓冲区，`size` 恰为它的字节数；
        // proc_pidinfo 只会在这个范围内写入，返回实际写入的字节数（失败返回 0 或负数）。
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&mut info as *mut libc::proc_vnodepathinfo).cast::<libc::c_void>(),
                size,
            )
        };
        if n != size {
            return None;
        }
        // libc 为兼容旧编译器把 MAXPATHLEN 长的数组声明成 [[c_char; 32]; 32]，内存上仍是连续的 1024 字节。
        let path = &info.pvi_cdir.vip_path;
        // SAFETY: `path` 指向一块连续的、已初始化的 1024 字节（32 * 32 个 c_char），
        // 且在返回前 `info` 一直存活；c_char 与 u8 大小、对齐相同。
        let bytes = unsafe {
            std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), std::mem::size_of_val(path))
        };
        let cstr = CStr::from_bytes_until_nul(bytes).ok()?;
        if cstr.to_bytes().is_empty() {
            return None;
        }
        Some(PathBuf::from(OsStr::from_bytes(cstr.to_bytes())))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    pub fn process_cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::path::PathBuf;

    pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
        None
    }
}
