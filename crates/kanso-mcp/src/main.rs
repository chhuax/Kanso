//! `kanso-mcp`：AI 工具以 stdio 启动的转发程序（architecture §16.1）。
//!
//! 连上正在运行的 Kanso 的本机 socket，先发一行握手（令牌、控制终端），之后在 stdin/stdout 与
//! socket 之间原样双向转发字节，**不理解 MCP**。连不上时区分"Kanso 未运行"与"MCP 开关关闭"，
//! 对客户端的第一条请求回一个 JSON-RPC 错误后退出（spec/mcp.md §1）。
//!
//! 本程序不读数据库、钥匙串、私钥，也不发起任何网络连接（spec/mcp.md §8）。

use std::process::ExitCode;

#[cfg(unix)]
mod unix {
    use std::ffi::CStr;
    use std::io::{self, BufRead, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use kanso_mcp::paths::McpPaths;
    use kanso_mcp::{DIRECT_BUNDLE_ID, TOKEN_ENV, hello, jsonrpc};

    /// JSON-RPC 的"服务端错误"区间（-32000 到 -32099）里的一个码。
    const SERVER_ERROR: i32 = -32000;
    const NOT_RUNNING: &str = "Kanso is not running. Ask the user to open Kanso.";
    const DISABLED: &str =
        "MCP is disabled in Kanso settings. Ask the user to enable it in Settings → MCP.";
    /// 找带 id 的请求时最多看几行：正常情况下第一行就是 initialize。
    const MAX_LINES_BEFORE_REQUEST: usize = 16;

    pub fn main() -> ExitCode {
        let paths = match paths() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("kanso-mcp: {e}");
                return reply_error(NOT_RUNNING);
            }
        };
        match UnixStream::connect(&paths.socket) {
            Ok(stream) => relay(stream),
            Err(_) if app_running(&paths.lock) => reply_error(DISABLED),
            Err(_) => reply_error(NOT_RUNNING),
        }
    }

    /// 产品构建只按 Direct 的 bundle id 推出路径。
    /// Debug 构建允许用 `KANSO_MCP_DIR` 指向测试用的目录，Release 构建忽略它：
    /// 能改写本程序环境变量的程序本来就能直接冒充客户端，这个开关不增加风险，但也没有理由留在正式版里。
    fn paths() -> io::Result<McpPaths> {
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("KANSO_MCP_DIR") {
            return Ok(McpPaths::in_dir(&PathBuf::from(dir), DIRECT_BUNDLE_ID));
        }
        McpPaths::for_bundle(DIRECT_BUNDLE_ID)
    }

    fn relay(mut stream: UnixStream) -> ExitCode {
        // 令牌缺失也照常连接：由 App 拒绝并回复"令牌无效"，用户能看到明确的原因。
        let token = std::env::var(TOKEN_ENV).unwrap_or_default();
        let line = hello::encode(&token, controlling_tty().as_deref());
        if stream.write_all(line.as_bytes()).is_err() {
            return reply_error(NOT_RUNNING);
        }
        let Ok(mut upstream) = stream.try_clone() else {
            return reply_error(NOT_RUNNING);
        };
        // stdin → socket 放在另一个线程；stdin 结束时半关闭写方向，让 App 知道客户端不会再发了。
        std::thread::spawn(move || {
            let _ = io::copy(&mut io::stdin().lock(), &mut upstream);
            let _ = upstream.shutdown(std::net::Shutdown::Write);
        });
        // socket → stdout。stdout 按行缓冲，MCP 消息以换行结尾，每条消息都会及时刷出。
        let mut stdout = io::stdout().lock();
        match io::copy(&mut stream, &mut stdout).and_then(|_| stdout.flush()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        }
    }

    /// App 是否在运行：它从启动起就一直持有单实例锁（architecture §16.1）。
    /// 能拿到锁说明没有在运行；拿到后立即随文件关闭而释放，不改动任何文件。
    fn app_running(lock: &Path) -> bool {
        // 锁文件不存在（从没运行过）也视为未运行；不创建它。
        let Ok(file) = std::fs::File::open(lock) else {
            return false;
        };
        // SAFETY: `file` 持有有效的文件描述符，flock 只作用于它；文件在本函数结束时关闭，锁随之释放。
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        rc != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
    }

    /// 读到客户端第一条带 id 的请求，回一个错误后退出。客户端会把消息显示给用户或 AI。
    fn reply_error(message: &str) -> ExitCode {
        let stdin = io::stdin();
        let mut lines = stdin.lock().lines();
        for _ in 0..MAX_LINES_BEFORE_REQUEST {
            let Some(Ok(line)) = lines.next() else { break };
            if let Some(id) = jsonrpc::request_id(&line) {
                let reply = jsonrpc::error_response(&id, SERVER_ERROR, message);
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(reply.as_bytes());
                let _ = stdout.flush();
                break;
            }
        }
        eprintln!("kanso-mcp: {message}");
        ExitCode::FAILURE
    }

    /// 本进程的控制终端路径，例如 `/dev/ttys003`；没有控制终端时为 `None`。
    /// stdin/stdout 是与 AI 工具之间的管道，所以打开 `/dev/tty` 取控制终端，而不是看 stdin。
    fn controlling_tty() -> Option<String> {
        // 带 O_NOCTTY：只是查询，不能让本进程因此获得控制终端。
        let tty = std::fs::File::options()
            .read(true)
            .custom_flags(libc::O_NOCTTY)
            .open("/dev/tty")
            .ok()?;
        // SAFETY: `tty` 是有效的已打开描述符；ttyname 返回指向静态缓冲区的指针或 NULL，
        // 本程序在此刻只有主线程调用它，立即复制出来，不保留指针。
        let name = unsafe { libc::ttyname(tty.as_raw_fd()) };
        if name.is_null() {
            return None;
        }
        // SAFETY: 非空时指向以 NUL 结尾的字符串。
        Some(
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        unix::main()
    }
    #[cfg(not(unix))]
    {
        eprintln!("kanso-mcp: only supported on macOS");
        ExitCode::FAILURE
    }
}
