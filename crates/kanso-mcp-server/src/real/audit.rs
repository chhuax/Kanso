//! 审计的批量写入与定期清理（spec/mcp.md §6）。
//!
//! 工具调用只往有界队列里放一条记录就返回，不等数据库；后台任务攒批后用 `spawn_blocking`
//! 调 `Core::mcp_audit_append`（同步、会访问 SQLite，不能占着运行时线程）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kanso_core::Core;
use kanso_types::mcp::McpAuditEntry;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::{McpServerDelegate, now_ms};

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
    delegate: Arc<dyn McpServerDelegate>,
    quiet: Arc<Mutex<bool>>,
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
                let removed = tokio::task::spawn_blocking(move || {
                    core.mcp_audit_prune(now_ms()).unwrap_or_else(|e| {
                        tracing::warn!("mcp audit prune failed: {e}");
                        0
                    })
                })
                .await;
                // 清掉了记录才通知：窗口里翻页到被清理的区间时，宿主需要知道数据变了。
                if matches!(removed, Ok(n) if n > 0) {
                    notify_changed(&delegate, &quiet);
                }
            }
            _ = flush.tick() => write(&core, &delegate, &quiet, &mut batch).await,
            entry = rx.recv() => match entry {
                Some(e) => {
                    batch.push(e);
                    if batch.len() >= BATCH_SIZE {
                        write(&core, &delegate, &quiet, &mut batch).await;
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
    write(&core, &delegate, &quiet, &mut batch).await;
}

async fn write(
    core: &Arc<Core>,
    delegate: &Arc<dyn McpServerDelegate>,
    quiet: &Mutex<bool>,
    batch: &mut Vec<McpAuditEntry>,
) {
    if batch.is_empty() {
        return;
    }
    let entries = std::mem::take(batch);
    let core = core.clone();
    let result = tokio::task::spawn_blocking(move || core.mcp_audit_append(entries)).await;
    match result {
        // 只在写库成功后通知：宿主收到回调就会去查库，早于落库会读到旧数据。
        // 一批只回调一次。
        Ok(Ok(())) => notify_changed(delegate, quiet),
        Ok(Err(e)) => tracing::warn!("mcp audit append failed: {e}"),
        Err(e) => tracing::warn!("mcp audit append task failed: {e}"),
    }
}

/// 回调宿主的 `on_audit_changed`，`quiet` 置位（服务正在 `Drop` 中拆除）时不回调。
///
/// 回调必须在 `quiet` 的锁内发出：拆除端持同一把锁置位，两者互斥，所以置位返回之后
/// 一定不会再有回调进入宿主（只看标志不持锁的话，检查通过后、回调发出前拆除可能已经完成）。
/// 回调契约要求实现立即返回、不得回调 `McpServer`，因此锁内调用不会死锁。
/// 此处不持有其他服务内部锁。
fn notify_changed(delegate: &Arc<dyn McpServerDelegate>, quiet: &Mutex<bool>) {
    let quiet = quiet.lock().unwrap_or_else(|e| e.into_inner());
    if !*quiet {
        delegate.on_audit_changed();
    }
}
