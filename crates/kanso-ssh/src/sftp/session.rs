//! 一条 SFTP 子系统通道上的请求 / 响应复用。
//!
//! 流水线的前提：同一条通道上可以同时有很多个在途请求。做法是"每个请求一个 request id +
//! 一个 oneshot，后台读任务按 id 把响应投递给等待者"，请求发出（[`Session::send`]）与
//! 等待响应（[`Reply::wait`]）是两步，调用方可以先连发一批再按序收。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::task::JoinHandle;

use super::error::SftpError;
use super::proto::{self, Reader};
use crate::Connection;

/// 一个已收到的响应：类型 + 去掉类型与 id 之后的包体。
pub(crate) struct Response {
    pub ty: u8,
    buf: Vec<u8>,
    /// 包体在 `buf` 里的起点（跳过类型与 id）。
    start: usize,
}

impl Response {
    pub fn body(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    pub fn reader(&self) -> Reader<'_> {
        Reader::new(self.body())
    }
}

type ReplySlot = oneshot::Sender<Result<Response, SftpError>>;

/// 在途请求表。`None` 表示会话已结束：之后登记的请求一律立即失败，
/// 否则"读任务刚清空表、新请求才登记"会让等待者永远收不到回复。
struct Pending {
    next_id: u32,
    map: Option<HashMap<u32, ReplySlot>>,
}

/// 等待一个已发出请求的响应。
pub(crate) struct Reply {
    rx: oneshot::Receiver<Result<Response, SftpError>>,
}

impl Reply {
    /// 等响应。会话结束（读任务清空在途表）时返回 [`SftpError::Disconnected`]。
    pub async fn wait(self) -> Result<Response, SftpError> {
        self.rx.await.unwrap_or(Err(SftpError::Disconnected))
    }
}

/// 已完成版本协商的 SFTP 会话。
pub(crate) struct Session {
    writer: AsyncMutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: std::sync::Arc<Mutex<Pending>>,
    dead: std::sync::Arc<AtomicBool>,
    reader: JoinHandle<()>,
    /// 持有连接，保证子系统通道存活期间连接不会因最后一个使用者释放而断开（与 shell 通道一致）。
    _conn: Option<Connection>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // 读任务持有流的读半边；不终止它的话，会话被丢弃后通道仍被占着。
        self.reader.abort();
    }
}

fn lock(m: &Mutex<Pending>) -> std::sync::MutexGuard<'_, Pending> {
    // 持锁区间内只做 HashMap 增删，不会留下半更新状态，中毒后可继续使用。
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 读一个包，返回 `(类型, 包体)`；流结束返回 `Ok(None)`。
async fn read_packet<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<(u8, Vec<u8>)>, SftpError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(_) => return Ok(None),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > proto::MAX_PACKET {
        return Err(SftpError::Protocol {
            detail: format!("bad packet length {len}"),
        });
    }
    let mut buf = vec![0u8; len];
    if r.read_exact(&mut buf).await.is_err() {
        return Ok(None);
    }
    Ok(Some((buf[0], buf)))
}

impl Session {
    /// 在已打开的子系统流上做 INIT / VERSION 协商并启动读任务。
    pub async fn start<S>(stream: S, conn: Option<Connection>) -> Result<Self, SftpError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (mut rd, mut wr) = tokio::io::split(stream);
        let mut init = proto::begin_packet(proto::FXP_INIT, None, 4);
        proto::put_u32(&mut init, proto::PROTOCOL_VERSION);
        proto::finish_packet(&mut init);
        wr.write_all(&init)
            .await
            .map_err(|_| SftpError::Disconnected)?;
        wr.flush().await.map_err(|_| SftpError::Disconnected)?;

        let Some((ty, buf)) = read_packet(&mut rd).await? else {
            return Err(SftpError::Disconnected);
        };
        if ty != proto::FXP_VERSION {
            return Err(SftpError::Protocol {
                detail: format!("expected VERSION, got packet type {ty}"),
            });
        }
        let version = Reader::new(&buf[1..]).u32()?;
        if version < proto::PROTOCOL_VERSION {
            return Err(SftpError::Protocol {
                detail: format!("server only supports SFTP version {version}"),
            });
        }
        // 服务器回更高版本时按 v3 通信：协议要求取双方较小值，服务器不会发 v3 之外的内容。

        let pending = std::sync::Arc::new(Mutex::new(Pending {
            next_id: 1,
            map: Some(HashMap::new()),
        }));
        let dead = std::sync::Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(read_loop(rd, pending.clone(), dead.clone()));
        Ok(Self {
            writer: AsyncMutex::new(Box::new(wr)),
            pending,
            dead,
            reader,
            _conn: conn,
        })
    }

    /// 会话是否已结束（通道关闭 / 协议错误）。结束后不可复用，需要重新打开子系统。
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// 发一个请求，不等响应。`build` 往包体里追加请求参数（id 与类型已写好）。
    ///
    /// 写通道时持有 `writer` 锁直到整包写完：多个请求并发发送时包不能交错。
    pub async fn send(
        &self,
        ty: u8,
        body_hint: usize,
        build: impl FnOnce(&mut Vec<u8>),
    ) -> Result<Reply, SftpError> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut p = lock(&self.pending);
            let id = p.next_id;
            p.next_id = p.next_id.wrapping_add(1);
            match p.map.as_mut() {
                Some(m) => {
                    m.insert(id, tx);
                }
                None => return Err(SftpError::Disconnected),
            }
            id
        };
        let mut pkt = proto::begin_packet(ty, Some(id), body_hint);
        build(&mut pkt);
        proto::finish_packet(&mut pkt);
        let res = {
            let mut w = self.writer.lock().await;
            match w.write_all(&pkt).await {
                Ok(()) => w.flush().await,
                Err(e) => Err(e),
            }
        };
        if res.is_err() {
            // 写失败说明通道已不可用；登记过的请求不会再有回复，主动撤销以免泄漏。
            if let Some(m) = lock(&self.pending).map.as_mut() {
                m.remove(&id);
            }
            self.dead.store(true, Ordering::SeqCst);
            return Err(SftpError::Disconnected);
        }
        Ok(Reply { rx })
    }
}

/// 读任务：按 id 分发响应；通道结束或协议错误时让全部在途请求失败。
async fn read_loop<R: AsyncRead + Unpin>(
    mut rd: R,
    pending: std::sync::Arc<Mutex<Pending>>,
    dead: std::sync::Arc<AtomicBool>,
) {
    let end_err = loop {
        match read_packet(&mut rd).await {
            Ok(Some((ty, buf))) => {
                // 响应都带 id；INIT/VERSION 之外的无 id 包不应出现。
                let Ok(id) = Reader::new(&buf[1..]).u32() else {
                    break SftpError::Protocol {
                        detail: "response without request id".into(),
                    };
                };
                let slot = lock(&pending).map.as_mut().and_then(|m| m.remove(&id));
                // 找不到登记项：请求方已放弃等待（future 被丢弃），响应直接丢掉。
                if let Some(tx) = slot {
                    let _ = tx.send(Ok(Response { ty, buf, start: 5 }));
                }
            }
            Ok(None) => break SftpError::Disconnected,
            Err(e) => break e,
        }
    };
    dead.store(true, Ordering::SeqCst);
    let map = lock(&pending).map.take();
    for (_, tx) in map.into_iter().flatten() {
        let _ = tx.send(Err(end_err.clone()));
    }
}
