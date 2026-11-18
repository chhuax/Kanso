//! 审计的批量写入与定期清理（spec/mcp.md §6）。
//!
//! 工具调用只往有界队列里放一条记录就返回，不等数据库；后台任务攒批后用 `spawn_blocking`
//! 调 `Core::mcp_audit_append`（同步、会访问 SQLite，不能占着运行时线程）。

use std::sync::Arc;
use std::time::Duration;

use kanso_core::Core;
use kanso_types::mcp::McpAuditEntry;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::now_ms;

const QUEUE_CAPACITY: usize = 1024;
const BATCH_SIZE: usize = 64;
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 审计记录的发送端，可随意克隆。
#[derive(Clone)]
pub(super) struct AuditSink(mpsc::Sender<McpAuditEntry>);

impl AuditSink {
    /// 放入队列。队列满了就丢弃并告警：宁可少一条审计，也不能让数据库变慢拖住工具调用。
    pub(super) fn push(&self, entry: McpAuditEntry) {
        if self.0.try_send(entry).is_err() {
            tracing::warn!("mcp audit queue is full or closed, dropping an entry");
        }
    }
}

pub(super) fn channel() -> (AuditSink, mpsc::Receiver<McpAuditEntry>) {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    (AuditSink(tx), rx)
}

/// 后台任务：攒批写入，并启动时与每小时清理一次过期记录。
/// `shutdown` 取消后写完队列里剩下的记录再返回，`stop` 会等它（有上限）。
pub(super) async fn run(
    core: Arc<Core>,
    mut rx: mpsc::Receiver<McpAuditEntry>,
    shutdown: CancellationToken,
) {
    let mut flush = interval(FLUSH_INTERVAL);
    // 写库偶尔慢了就顺延，不要为了"补上"错过的节拍而连续触发。
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut prune = interval(PRUNE_INTERVAL);
    prune.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut batch: Vec<McpAuditEntry> = Vec::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = prune.tick() => {
                let core = core.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = core.mcp_audit_prune(now_ms()) {
                        tracing::warn!("mcp audit prune failed: {e}");
                    }
                })
                .await;
            }
            _ = flush.tick() => write(&core, &mut batch).await,
            entry = rx.recv() => match entry {
                Some(e) => {
                    batch.push(e);
                    if batch.len() >= BATCH_SIZE {
                        write(&core, &mut batch).await;
                    }
                }
                None => break,
            },
        }
    }
    // 关停：先关闭队列（之后的 push 被丢弃），再取走已入队的记录。
    rx.close();
    while let Ok(e) = rx.try_recv() {
        batch.push(e);
    }
    write(&core, &mut batch).await;
}

async fn write(core: &Arc<Core>, batch: &mut Vec<McpAuditEntry>) {
    if batch.is_empty() {
        return;
    }
    let entries = std::mem::take(batch);
    let core = core.clone();
    let result = tokio::task::spawn_blocking(move || core.mcp_audit_append(entries)).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("mcp audit append failed: {e}"),
        Err(e) => tracing::warn!("mcp audit append task failed: {e}"),
    }
}
