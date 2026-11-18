//! Windows stdio 转发。两个线程各执行自己的重叠 I/O，使等待回复时仍可写入请求。

#![allow(unsafe_code)]

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
#[cfg(debug_assertions)]
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanso_mcp::windows::{self, McpPaths, PIPE_EOF};
use kanso_mcp::{DIRECT_BUNDLE_ID, TOKEN_ENV, hello};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED,
    HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_WRITE_DATA, ReadFile, SECURITY_IDENTIFICATION,
    WriteFile,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::CreateEventW;
use windows_sys::core::BOOL;

use super::{CODE_MCP_DISABLED, CODE_NOT_RUNNING, DISABLED, NOT_RUNNING, pump, reply_error};

pub(super) fn main() -> ExitCode {
    let paths = match paths() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("kanso-mcp: {e}");
            return reply_error(CODE_NOT_RUNNING, NOT_RUNNING);
        }
    };
    match connect(&paths.socket) {
        Ok(pipe) => relay(pipe),
        Err(_) if windows::app_running(&paths.presence) => reply_error(CODE_MCP_DISABLED, DISABLED),
        Err(_) => reply_error(CODE_NOT_RUNNING, NOT_RUNNING),
    }
}

fn paths() -> io::Result<McpPaths> {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("KANSO_MCP_DIR") {
        return McpPaths::in_dir(&PathBuf::from(dir));
    }
    McpPaths::for_bundle(DIRECT_BUNDLE_ID)
}

fn connect(path: &Path) -> io::Result<File> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pipe = File::options()
            // 客户端不需要 FILE_CREATE_PIPE_INSTANCE，也不给对端以客户端身份执行操作的能力。
            .access_mode(FILE_GENERIC_READ | FILE_WRITE_DATA)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(path);
        match pipe {
            Ok(pipe) => {
                windows::require_same_user_server(&pipe)?;
                return Ok(pipe);
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                // 其他客户端刚占用接收实例。等待 App 建立下一实例，同一启动期限内重试。
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(e);
                };
                windows::wait_for_pipe(path, left.as_millis().max(1) as u32)?;
            }
            Err(e) => return Err(e),
        }
    }
}

fn relay(pipe: File) -> ExitCode {
    let hello = hello::encode(&std::env::var(TOKEN_ENV).unwrap_or_default(), None);
    let streams = open_streams(pipe, hello.as_bytes());
    let Ok((mut reader, mut writer)) = streams else {
        return reply_error(CODE_NOT_RUNNING, NOT_RUNNING);
    };
    std::thread::spawn(move || {
        let _ = pump(&mut io::stdin().lock(), &mut writer);
        // 在收到最后一条回复之前保持读取方向开启。
        let _ = writer.write_all(&[PIPE_EOF]);
    });
    match pump(&mut reader, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn open_streams(pipe: File, hello: &[u8]) -> io::Result<(PipeIo, PipeIo)> {
    let pipe = Arc::new(pipe);
    let reader = PipeIo::new(pipe.clone())?;
    let mut writer = PipeIo::new(pipe)?;
    writer.write_all(hello)?;
    Ok((reader, writer))
}

struct PipeIo {
    pipe: Arc<File>,
    event: OwnedHandle,
}

impl PipeIo {
    fn new(pipe: Arc<File>) -> io::Result<Self> {
        // SAFETY: 空安全属性和名字表示不可继承、匿名的事件。
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateEventW 返回有效且由当前调用方独占的句柄。
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        Ok(Self { pipe, event })
    }

    fn transfer(
        &mut self,
        operation: impl FnOnce(HANDLE, *mut OVERLAPPED) -> BOOL,
    ) -> io::Result<usize> {
        let handle = self.pipe.as_raw_handle();
        let mut state = OVERLAPPED {
            hEvent: self.event.as_raw_handle(),
            ..OVERLAPPED::default()
        };
        let state = &raw mut state;
        if operation(handle, state) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
        }
        let mut count = 0;
        // SAFETY: state、事件与调用方缓冲区在等待期间均有效，bWait 等到操作结束才返回。
        if unsafe { GetOverlappedResult(handle, state, &mut count, 1) } != 0 {
            Ok(count as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Read for PipeIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let result = self.transfer(|handle, state| {
            // SAFETY: 缓冲区至少 len 字节，在 transfer 等待操作结束前不会释放或被访问。
            unsafe { ReadFile(handle, buf.as_mut_ptr(), len, null_mut(), state) }
        });
        match result {
            Err(e) if matches!(e.raw_os_error(), Some(code) if [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED].contains(&(code as u32))) => {
                Ok(0)
            }
            other => other,
        }
    }
}

impl Write for PipeIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        self.transfer(|handle, state| {
            // SAFETY: 缓冲区至少 len 字节，在 transfer 等待操作结束前保持有效。
            unsafe { WriteFile(handle, buf.as_ptr(), len, null_mut(), state) }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // WriteFile 没有用户态缓冲。不能用 FlushFileBuffers，它还会等对端将管道中的数据读尽。
        Ok(())
    }
}
