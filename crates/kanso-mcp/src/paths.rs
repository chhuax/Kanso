//! 单实例锁与 socket 的路径规则（architecture §16.1）。

use std::ffi::{CStr, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// macOS 的 `sockaddr_un.sun_path` 是 104 字节，含结尾 NUL，路径最多 103 字节。
const SUN_PATH_MAX: usize = 103;

/// 一个 App（按 bundle id 区分）的 MCP 路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpPaths {
    /// 存放锁文件与 socket 的目录，权限应为 0700。
    pub dir: PathBuf,
    /// 单实例锁文件。App 启动时就加锁并一直持有。
    pub lock: PathBuf,
    /// socket 文件。
    pub socket: PathBuf,
}

impl McpPaths {
    /// 按当前用户与 `bundle_id` 推出路径。
    ///
    /// 家目录取自用户数据库（`getpwuid_r`），**不读 `$HOME`**：转发程序由 AI 工具启动，
    /// 环境变量可能被工具改写，读 `$HOME` 可能和 App 算出不同的路径。
    pub fn for_bundle(bundle_id: &str) -> io::Result<McpPaths> {
        Ok(McpPaths::in_dir(
            &base_dir(&home_dir()?, bundle_id),
            bundle_id,
        ))
    }

    /// 以给定目录为基础推出路径；测试用临时目录，产品代码用 [`McpPaths::for_bundle`]。
    pub fn in_dir(dir: &Path, bundle_id: &str) -> McpPaths {
        let socket = dir.join("kanso.sock");
        // 家目录或 bundle id 很长时 socket 路径会超出上限，改用当前用户的临时目录。
        // 两端都按同一规则判断，所以会落到同一个位置。
        let socket = if socket.as_os_str().len() > SUN_PATH_MAX {
            user_temp_dir().join(format!("{bundle_id}.mcp.sock"))
        } else {
            socket
        };
        McpPaths {
            dir: dir.to_path_buf(),
            lock: dir.join("kanso.lock"),
            socket,
        }
    }
}

#[cfg(target_os = "macos")]
fn base_dir(home: &Path, bundle_id: &str) -> PathBuf {
    home.join("Library/Application Support")
        .join(bundle_id)
        .join("mcp")
}

/// 非 macOS 只为能在 Linux CI 上编译与测试；产品只在 macOS 上提供 MCP。
#[cfg(not(target_os = "macos"))]
fn base_dir(home: &Path, bundle_id: &str) -> PathBuf {
    home.join(".local/share").join(bundle_id).join("mcp")
}

/// 当前用户的家目录，取自用户数据库。
fn home_dir() -> io::Result<PathBuf> {
    // SAFETY: `passwd` 是只含指针与整数的 C 结构体，全 0 是合法的初始值；它只作为输出参数。
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: `pwd`、`buf`、`result` 都是有效的可写内存，`buf.len()` 是缓冲区的真实长度；
    // 成功时 `pwd` 中的指针指向 `buf` 内部，下面只在 `buf` 存活期间读取。
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return Err(io::Error::other("cannot resolve home directory"));
    }
    // SAFETY: `pw_dir` 非空，指向 `buf` 内以 NUL 结尾的字符串，`buf` 此时仍存活。
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Ok(PathBuf::from(OsStr::from_bytes(dir.to_bytes())))
}

/// 当前用户的临时目录。macOS 上用 `confstr(_CS_DARWIN_USER_TEMP_DIR)`，不读 `$TMPDIR`，理由同家目录。
#[cfg(target_os = "macos")]
fn user_temp_dir() -> PathBuf {
    let mut buf = vec![0 as libc::c_char; 1024];
    // SAFETY: `buf` 是有效的可写缓冲区，长度如实传入；confstr 最多写入 `buf.len()` 字节并以 NUL 结尾。
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
    if n == 0 || n > buf.len() {
        return PathBuf::from("/tmp");
    }
    // SAFETY: confstr 成功时 `buf` 以 NUL 结尾。
    let dir = unsafe { CStr::from_ptr(buf.as_ptr()) };
    PathBuf::from(OsStr::from_bytes(dir.to_bytes()))
}

#[cfg(not(target_os = "macos"))]
fn user_temp_dir() -> PathBuf {
    PathBuf::from("/tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_stay_in_dir() {
        let p = McpPaths::in_dir(Path::new("/Users/a/Library/Application Support/x/mcp"), "x");
        assert_eq!(
            p.lock,
            Path::new("/Users/a/Library/Application Support/x/mcp/kanso.lock")
        );
        assert_eq!(
            p.socket,
            Path::new("/Users/a/Library/Application Support/x/mcp/kanso.sock")
        );
    }

    #[test]
    fn long_socket_path_moves_to_temp_but_lock_stays() {
        let long = format!("/Users/{}/mcp", "u".repeat(120));
        let p = McpPaths::in_dir(Path::new(&long), "com.example.app");
        assert!(p.socket.ends_with("com.example.app.mcp.sock"));
        assert!(p.socket.as_os_str().len() <= SUN_PATH_MAX);
        assert!(p.lock.starts_with(&long));
    }

    #[test]
    fn home_dir_resolves() {
        assert!(home_dir().unwrap().is_absolute());
    }
}
