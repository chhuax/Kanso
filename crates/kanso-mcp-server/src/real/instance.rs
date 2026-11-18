//! Unix 单实例锁、socket 与对端验证（architecture §16.1）。

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use kanso_mcp::paths::McpPaths;

pub(super) type InstanceGuard = File;
pub(super) type Stream = tokio::net::UnixStream;

pub(super) fn acquire(
    bundle_id: &str,
    dir: Option<&Path>,
) -> io::Result<(Option<InstanceGuard>, PathBuf)> {
    let paths = match dir {
        Some(dir) => McpPaths::in_dir(dir, bundle_id),
        None => McpPaths::for_bundle(bundle_id)?,
    };
    Ok((take_lock(&paths)?, paths.socket))
}

/// 创建目录（0700）并对锁文件加排他锁。`Ok(None)` 表示锁已被另一个实例持有。
///
/// 返回的 `File` 要一直持有到进程结束：锁随文件描述符释放。Rust 打开的文件带 `O_CLOEXEC`，
/// 所以 App 启动的 shell 等子进程不会继承它，否则 App 退出后锁还会被子进程占着。
#[allow(unsafe_code)]
fn take_lock(paths: &McpPaths) -> io::Result<Option<File>> {
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

pub(super) struct Listener(UnixListener);

impl Listener {
    pub(super) fn bind(socket: &Path) -> io::Result<Self> {
        // 上次异常退出可能留下 socket 文件。单实例锁保证没有别的实例在用它。
        match fs::remove_file(socket) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = std::os::unix::net::UnixListener::bind(socket)?;
        // bind 按 umask 建文件，之后再收紧到 0600。所在目录是 0700，这段时间内其他用户也到不了。
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self(UnixListener::from_std(listener)?))
    }

    pub(super) async fn accept(&mut self) -> io::Result<Stream> {
        self.0.accept().await.map(|(stream, _)| stream)
    }
}

pub(super) fn cleanup(socket: &Path) {
    let _ = fs::remove_file(socket);
}

#[allow(unsafe_code)]
pub(super) fn peer_pid(stream: &Stream) -> io::Result<Option<u32>> {
    let cred = stream.peer_cred()?;
    // SAFETY: getuid 没有参数与前置条件，不会失败。
    if cred.uid() != unsafe { libc::getuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "mcp peer has a different UID",
        ));
    }
    Ok(cred.pid().and_then(|pid| u32::try_from(pid).ok()))
}

pub(super) fn split(stream: Stream) -> (OwnedReadHalf, OwnedWriteHalf) {
    stream.into_split()
}
