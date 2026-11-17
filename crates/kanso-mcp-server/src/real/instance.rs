//! 目录与单实例锁（architecture §16.1）。

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use kanso_mcp::paths::McpPaths;

/// 创建目录（0700）并对锁文件加排他锁。`Ok(None)` 表示锁已被另一个实例持有。
///
/// 返回的 `File` 要一直持有到进程结束：锁随文件描述符释放。Rust 打开的文件带 `O_CLOEXEC`，
/// 所以 App 启动的 shell 等子进程不会继承它，否则 App 退出后锁还会被子进程占着。
pub(super) fn acquire(paths: &McpPaths) -> io::Result<Option<File>> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&paths.dir)?;
    // 目录可能是以前用别的权限建的：已存在时 `mode` 不生效，这里显式收紧。
    fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o700))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&paths.lock)?;
    // SAFETY: `file` 是刚打开、仍存活的有效文件描述符；flock 没有其他内存前置条件。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(file));
    }
    let err = io::Error::last_os_error();
    // 非阻塞加锁被占用时返回 EWOULDBLOCK，std 把它映射为 WouldBlock。
    if err.kind() == io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err)
    }
}
