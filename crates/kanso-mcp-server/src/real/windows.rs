//! Windows 单实例占位和真实服务都走当前用户专用的本机命名管道。

use std::io;
use std::os::windows::io::{IntoRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use kanso_mcp::windows::{self, McpPaths, PIPE_EOF};
use tokio::io::{self as tokio_io, AsyncRead, ReadBuf, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::NamedPipeServer;

pub(super) type InstanceGuard = OwnedHandle;
pub(super) type Stream = NamedPipeServer;

pub(super) fn acquire(
    bundle_id: &str,
    dir: Option<&Path>,
) -> io::Result<(Option<InstanceGuard>, PathBuf)> {
    let paths = match dir {
        Some(dir) => McpPaths::in_dir(dir),
        None => McpPaths::for_bundle(bundle_id),
    }?;
    Ok((windows::take_presence(&paths.presence)?, paths.socket))
}

pub(super) struct Listener {
    name: PathBuf,
    pending: NamedPipeServer,
}

impl Listener {
    pub(super) fn bind(name: &Path) -> io::Result<Self> {
        Ok(Self {
            name: name.to_owned(),
            pending: open_server(name, true)?,
        })
    }

    pub(super) async fn accept(&mut self) -> io::Result<Stream> {
        let result = self.pending.connect().await;
        // 原句柄保持到下一实例建好，管道名不会空出来。即使客户端在接收前退出，也要换掉旧实例。
        let next = open_server(&self.name, false)?;
        let stream = std::mem::replace(&mut self.pending, next);
        result.map(|()| stream)
    }
}

#[allow(unsafe_code)]
fn open_server(name: &Path, first: bool) -> io::Result<NamedPipeServer> {
    let pipe = windows::create_pipe(name, first)?;
    // SAFETY: create_pipe 成功返回一个 FILE_FLAG_OVERLAPPED 的服务器句柄，所有权转交给 Tokio。
    unsafe { NamedPipeServer::from_raw_handle(pipe.into_raw_handle()) }
}

pub(super) fn cleanup(_: &Path) {
    // 没有文件需要删除。最后一个服务器句柄关闭后，内核自动回收名称。
}

pub(super) fn peer_pid(stream: &Stream) -> io::Result<Option<u32>> {
    windows::client_pid(stream).map(Some)
}

pub(super) fn split(stream: Stream) -> (PipeRead, WriteHalf<Stream>) {
    let (read, write) = tokio_io::split(stream);
    (PipeRead { read, eof: false }, write)
}

/// 吸收转发程序的半关闭标记，让上层与 Unix 一样读到 EOF 并保留写方向。
pub(super) struct PipeRead {
    read: ReadHalf<Stream>,
    eof: bool,
}

impl AsyncRead for PipeRead {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.eof {
            return Poll::Ready(Ok(()));
        }
        let start = buf.filled().len();
        ready!(Pin::new(&mut this.read).poll_read(cx, buf))?;
        if let Some(end) = buf.filled()[start..].iter().position(|b| *b == PIPE_EOF) {
            buf.set_filled(start + end);
            this.eof = true;
        }
        Poll::Ready(Ok(()))
    }
}
