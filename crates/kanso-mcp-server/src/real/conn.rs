//! 单个连接：校验对端、读握手、校验令牌，通过后交给 rmcp（architecture §16.1，spec/mcp.md §1、§8）。

use std::sync::Arc;
use std::time::Duration;

use kanso_mcp::{hello, jsonrpc};
use kanso_types::mcp::McpAuditEntry;
use rmcp::ServiceExt;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::Shared;
use super::registry::Admission;
use super::tools::KansoHandler;
use crate::now_ms;

/// 握手行必须在这段时间内读完，防止空连接占着名额。
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// JSON-RPC 的"服务端错误"区间里的码，与转发程序一致。
const SERVER_ERROR: i32 = -32000;
const MSG_INVALID_TOKEN: &str = "Invalid or missing Kanso MCP token. Ask the user to copy the configuration again from Kanso Settings → MCP.";
const MSG_TOO_MANY: &str = "Too many MCP clients connected to Kanso.";

#[derive(Deserialize)]
struct Hello {
    v: u32,
    token: String,
    /// 阶段 1 只记录；识别客户端所在标签（spec/mcp.md §2.5）在后续阶段使用。
    #[serde(default)]
    tty: Option<String>,
}

/// 处理一个已接受的连接，直到它结束。
pub(super) async fn handle(stream: UnixStream, shared: Arc<Shared>) {
    // 只允许同一系统用户（spec/mcp.md §8）。socket 在 0700 目录里，这是第二道防线。
    let cred = match stream.peer_cred() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("mcp peer credentials unavailable: {e}");
            return;
        }
    };
    // SAFETY: getuid 没有参数与前置条件，不会失败。
    if cred.uid() != unsafe { libc::getuid() } {
        tracing::warn!("mcp connection from another user rejected");
        return;
    }

    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let Ok(Some(hello)) = timeout(HELLO_TIMEOUT, read_hello(&mut reader)).await else {
        return;
    };
    tracing::debug!(pid = ?cred.pid(), tty = ?hello.tty, "mcp connection handshake");

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
                error_code: Some("INVALID_TOKEN".to_owned()),
                duration_ms: 0,
                bytes: 0,
            });
            reject(&mut reader, &mut write, MSG_INVALID_TOKEN).await;
            return;
        }
        Admission::TooMany => {
            reject(&mut reader, &mut write, MSG_TOO_MANY).await;
            return;
        }
        Admission::Closed => return,
    };

    serve(reader, write, shared.clone(), id, cancel).await;
    shared.registry.remove(id);
}

/// 把连接交给 rmcp，直到客户端断开、出错或被取消。
async fn serve(
    reader: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    shared: Arc<Shared>,
    id: u64,
    cancel: CancellationToken,
) {
    let handler = KansoHandler::new(shared, id);
    let running = tokio::select! {
        r = handler.serve((reader, write)) => match r {
            Ok(running) => running,
            Err(e) => {
                tracing::debug!("mcp initialize failed: {e}");
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

/// 读一行（含换行）。超长、提前结束或不是合法握手都返回 `None`。
async fn read_hello(reader: &mut BufReader<OwnedReadHalf>) -> Option<Hello> {
    let line = read_line(reader).await?;
    let hello: Hello = serde_json::from_str(&line).ok()?;
    (hello.v == hello::VERSION).then_some(hello)
}

/// 最多读 `MAX_LEN` 字节、以换行结尾的一行。
async fn read_line(reader: &mut BufReader<OwnedReadHalf>) -> Option<String> {
    let mut buf = Vec::new();
    // `take` 限制的是本次读取的总量；超过上限仍没遇到换行就当作非法输入。
    let n = reader
        .take(hello::MAX_LEN as u64)
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
async fn reject(reader: &mut BufReader<OwnedReadHalf>, write: &mut OwnedWriteHalf, message: &str) {
    let Ok(Some(line)) = timeout(HELLO_TIMEOUT, read_line(reader)).await else {
        return;
    };
    let Some(id) = jsonrpc::request_id(&line) else {
        return;
    };
    let reply = jsonrpc::error_response(&id, SERVER_ERROR, message);
    let _ = write.write_all(reply.as_bytes()).await;
    let _ = write.shutdown().await;
}
