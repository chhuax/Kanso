//! 单个连接：校验对端、读握手、校验令牌，通过后交给 rmcp（architecture §16.1，spec/mcp.md §1、§8）。

use std::sync::Arc;
use std::time::Duration;

use kanso_mcp::jsonrpc::{
    CODE_INVALID_TOKEN, CODE_TOO_MANY_CLIENTS, CODE_VERSION_MISMATCH, SERVER_ERROR,
};
use kanso_mcp::{hello, jsonrpc};
use kanso_types::mcp::McpAuditEntry;
use rmcp::ServiceExt;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::identity::{ClientIdentity, find_own_tab};
use super::registry::Admission;
use super::tools::KansoHandler;
use super::{Shared, ipc};
use crate::now_ms;

/// 握手行必须在这段时间内读完，防止空连接占着名额。
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// 令牌校验通过后，客户端必须在这段时间内完成 initialize，否则释放名额。
/// 令牌已经通过，说明是用户配置过的客户端，所以放宽到 10 秒（启动慢的客户端也够）。
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// 拒绝时读取第一条请求的长度上限，与 rmcp 的单行上限（16 MiB）一致，
/// 否则很大的 initialize 收不到错误回复。
const MAX_REQUEST_LEN: usize = 16 * 1024 * 1024;
const MSG_INVALID_TOKEN: &str = "Invalid or missing Kanso MCP token. Ask the user to copy the configuration again from Kanso Settings → MCP.";
const MSG_VERSION_MISMATCH: &str =
    "Kanso and kanso-mcp versions do not match. Reinstall Kanso or copy the configuration again.";
const MSG_TOO_MANY: &str = "Too many MCP clients connected to Kanso.";

#[derive(Deserialize)]
struct Hello {
    v: u32,
    /// 缺失当作空串，随后按"令牌无效"处理。
    #[serde(default)]
    token: String,
    /// 客户端的控制终端，用于识别客户端所在标签（spec/mcp.md §2.5）。
    #[serde(default)]
    tty: Option<String>,
}

/// 处理一个已接受的连接，直到它结束。
pub(super) async fn handle(stream: ipc::Stream, shared: Arc<Shared>) {
    // 除了本机通道的 ACL，还要检查对端自身的系统用户身份。
    let pid = match ipc::peer_pid(&stream) {
        Ok(pid) => pid,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!("mcp peer rejected: {e}");
            return;
        }
        Err(e) => {
            tracing::debug!("mcp peer credentials unavailable: {e}");
            return;
        }
    };

    let (read, mut write) = ipc::split(stream);
    let mut reader = BufReader::new(read);
    let hello = match timeout(HELLO_TIMEOUT, read_hello(&mut reader)).await {
        Ok(Some(h)) if h.v == hello::VERSION => h,
        Ok(Some(_)) => {
            reject(
                &mut reader,
                &mut write,
                CODE_VERSION_MISMATCH,
                MSG_VERSION_MISMATCH,
            )
            .await;
            return;
        }
        // 超时、超长或不是合法的握手：直接断开。
        _ => return,
    };
    tracing::debug!(pid = ?pid, tty = ?hello.tty, "mcp connection handshake");

    let (id, cancel) = match shared.registry.admit(&hello.token) {
        Admission::Admitted { id, cancel } => (id, cancel),
        Admission::InvalidToken => {
            shared.audit.push(McpAuditEntry {
                at_ms: now_ms(),
                client: String::new(),
                mcp_session: String::new(),
                tool: String::new(),
                profile: None,
                session: None,
                args: String::new(),
                error_code: Some(CODE_INVALID_TOKEN.to_owned()),
                duration_ms: 0,
                bytes: 0,
            });
            reject(
                &mut reader,
                &mut write,
                CODE_INVALID_TOKEN,
                MSG_INVALID_TOKEN,
            )
            .await;
            return;
        }
        Admission::TooMany => {
            reject(&mut reader, &mut write, CODE_TOO_MANY_CLIENTS, MSG_TOO_MANY).await;
            return;
        }
        Admission::Closed => return,
    };

    // 祖先链在连接建立时算一次并缓存：进程存活期间它不会变；与标签的比对在每次调用时做（标签会增减）。
    // 读进程表是同步系统调用（每层一次），放进阻塞线程池，不占 MCP 运行时的 2 个工作线程。
    let ancestors = match pid {
        Some(pid) => {
            let read = shared.ancestors.clone();
            tokio::task::spawn_blocking(move || read(pid))
                .await
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    let identity = Arc::new(ClientIdentity {
        tty: hello.tty.filter(|t| !t.is_empty()),
        ancestors,
    });
    let own = {
        let core = shared.core.clone();
        let identity = identity.clone();
        tokio::task::spawn_blocking(move || find_own_tab(&core, &identity))
            .await
            .ok()
            .flatten()
    };
    if own.is_some() {
        shared.registry.set_own_session(id, own);
    }

    serve(reader, write, shared.clone(), id, cancel, identity).await;
    shared.registry.remove(id);
}

/// 把连接交给 rmcp，直到客户端断开、出错或被取消。
async fn serve(
    reader: BufReader<impl AsyncRead + Unpin + Send + 'static>,
    write: impl AsyncWrite + Unpin + Send + 'static,
    shared: Arc<Shared>,
    id: u64,
    cancel: CancellationToken,
    identity: Arc<ClientIdentity>,
) {
    let handler = KansoHandler::new(shared, id, cancel.clone(), identity);
    let running = tokio::select! {
        r = timeout(INIT_TIMEOUT, handler.serve((reader, write))) => match r {
            Ok(Ok(running)) => running,
            Ok(Err(e)) => {
                tracing::debug!("mcp initialize failed: {e}");
                return;
            }
            Err(_) => {
                tracing::debug!("mcp initialize timed out");
                return;
            }
        },
        _ = cancel.cancelled() => return,
    };
    let token = running.cancellation_token();
    tokio::select! {
        _ = running.waiting() => {}
        _ = cancel.cancelled() => token.cancel(),
    }
}

/// 读握手行（最多 `hello::MAX_LEN` 字节）。超长、提前结束或 JSON 解析失败返回 `None`；
/// 版本是否匹配由调用方判断，以便给出原因。
async fn read_hello(reader: &mut BufReader<impl AsyncRead + Unpin>) -> Option<Hello> {
    let line = read_line(reader, hello::MAX_LEN).await?;
    serde_json::from_str(&line).ok()
}

/// 最多读 `max` 字节、以换行结尾的一行。
async fn read_line(reader: &mut BufReader<impl AsyncRead + Unpin>, max: usize) -> Option<String> {
    let mut buf = Vec::new();
    // `take` 限制的是本次读取的总量；超过上限仍没遇到换行就当作非法输入。
    let n = reader
        .take(max as u64)
        .read_until(b'\n', &mut buf)
        .await
        .ok()?;
    if n == 0 || buf.last() != Some(&b'\n') {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// 读客户端的第一条请求，回复一个 JSON-RPC 错误后断开。
/// 转发程序只是原样转发，客户端发出 initialize 后一直在等回复；不回复它只会看到"连接意外断开"。
async fn reject(
    reader: &mut BufReader<impl AsyncRead + Unpin>,
    write: &mut (impl AsyncWrite + Unpin),
    stable_code: &str,
    message: &str,
) {
    let Ok(Some(line)) = timeout(HELLO_TIMEOUT, read_line(reader, MAX_REQUEST_LEN)).await else {
        return;
    };
    let Some(id) = jsonrpc::request_id(&line) else {
        return;
    };
    let reply = jsonrpc::error_response(&id, SERVER_ERROR, stable_code, message);
    let _ = write.write_all(reply.as_bytes()).await;
    let _ = write.shutdown().await;
}
