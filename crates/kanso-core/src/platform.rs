//! 平台 trait：由宿主（Swift）或 Rust 内置实现，启动时组装成 [`Platform`] 注入 core。
//!
//! 与 core 之外的 crate 相关的 trait 放在消费它的 crate 里（`SecretStore` 在 kanso-secret，
//! `HostKeyStore` 在 kanso-ssh，`PurchaseProvider` 在 kanso-license），保证依赖只能自上而下；
//! 只有 core 自己消费的 trait 定义在本模块。

use std::fmt;
use std::sync::{Arc, Mutex};

use kanso_license::PurchaseProvider;
use kanso_secret::{MemorySecretStore, SecretStore};
use kanso_ssh::{HostKeyStore, MemoryHostKeyStore};
use kanso_types::agent::AgentKind;
use kanso_types::event::TransferOutcome;
use kanso_types::{CoreEvent, SessionId, TransferId};

/// 事件接收端：core 把 [`CoreEvent`] 推给 UI 的出口。
///
/// - 实现者：Swift 宿主（把事件投递到主线程）。
/// - 调用者：core，在**任意工作线程**上调用。
/// - 阻塞：实现**必须立即返回**，Swift 侧只做“投递到主线程”这一件事；
///   不得在回调里同步回调 core，否则可能死锁。
/// - 顺序：同一会话内的事件保序。
/// - 出错：没有返回值，core 不关心事件是否被消费。
pub trait EventSink: Send + Sync + 'static {
    /// 收到一个事件。
    fn on_event(&self, event: CoreEvent);
}

/// 文件访问错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileAccessError {
    /// 沙盒或用户拒绝访问。
    Denied,
    /// 书签已失效，无法解析。
    Stale,
    /// 其他错误；`detail` 为英文技术信息，只进日志。
    Other {
        /// 技术细节。
        detail: String,
    },
}

impl fmt::Display for FileAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileAccessError::Denied => f.write_str("file access denied"),
            FileAccessError::Stale => f.write_str("bookmark is stale"),
            FileAccessError::Other { detail } => write!(f, "file access error: {detail}"),
        }
    }
}

impl std::error::Error for FileAccessError {}

/// [`FileAccess::start_access`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessGrant {
    /// 书签解析出的路径，之后对它的读写才被沙盒允许。
    pub path: String,
    /// 书签是否已过期：为 `true` 时路径仍可用，但调用方应重新创建书签并保存。
    pub stale: bool,
}

/// 沙盒文件访问（security-scoped bookmark）。
///
/// - 实现者：只有 MAS 版需要真实实现（Swift）；Direct / Setapp 用 [`NoopFileAccess`]。
/// - 调用者：core，在 `spawn_blocking` 的工作线程里调用，可以短暂阻塞。
/// - 配对约束：[`start_access`](Self::start_access) 与 [`stop_access`](Self::stop_access)
///   **必须成对调用**，否则沙盒内核资源泄漏。
/// - 出错处理：`create_bookmark` / `start_access` 失败时，调用方把该路径当作不可访问，
///   向用户提示并请求重新授权，不重试。
pub trait FileAccess: Send + Sync + 'static {
    /// 为路径创建可持久保存的书签。
    fn create_bookmark(&self, path: &str) -> Result<Vec<u8>, FileAccessError>;

    /// 解析书签并开始访问。成功后调用方负责在用完时调用 `stop_access`。
    fn start_access(&self, bookmark: &[u8]) -> Result<AccessGrant, FileAccessError>;

    /// 结束对 `path`（`start_access` 返回的路径）的访问。
    fn stop_access(&self, path: &str);
}

/// 不做任何沙盒处理的 [`FileAccess`]：书签就是路径本身的 UTF-8 字节。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopFileAccess;

impl FileAccess for NoopFileAccess {
    fn create_bookmark(&self, path: &str) -> Result<Vec<u8>, FileAccessError> {
        Ok(path.as_bytes().to_vec())
    }

    fn start_access(&self, bookmark: &[u8]) -> Result<AccessGrant, FileAccessError> {
        let path = String::from_utf8(bookmark.to_vec()).map_err(|e| FileAccessError::Other {
            detail: format!("bookmark is not valid utf-8: {e}"),
        })?;
        Ok(AccessGrant { path, stale: false })
    }

    fn stop_access(&self, _path: &str) {}
}

/// 需要提醒用户的通知。只含结构化数据，文字由宿主本地化（architecture §10）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notification {
    /// 命令执行结束。
    CommandFinished {
        /// 会话。
        session: SessionId,
        /// 退出码；未知为 `None`。
        exit_code: Option<i32>,
    },
    /// agent 一轮工作完成，等待用户输入。
    AgentTurnDone {
        /// 会话。
        session: SessionId,
        /// agent 种类。
        agent: AgentKind,
    },
    /// 文件传输结束。
    TransferFinished {
        /// 传输。
        transfer: TransferId,
        /// 结果。
        outcome: TransferOutcome,
    },
}

/// 系统通知发送端。
///
/// - 实现者：Swift 宿主（UserNotifications）。
/// - 调用者：core，在任意工作线程上调用，且**只在对应标签不可见时**调用
///   （可见性由 UI 通过控制面告知 core，接口在 P0.4 定）。
/// - 阻塞：实现必须立即返回。
/// - 出错处理：没有返回值；用户关闭了通知权限时实现静默丢弃。
pub trait Notifier: Send + Sync + 'static {
    /// 发出一条通知。
    fn notify(&self, notification: Notification);
}

/// 丢弃所有通知的 [`Notifier`]。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopNotifier;

impl Notifier for NoopNotifier {
    fn notify(&self, _notification: Notification) {}
}

/// 收集事件的 [`EventSink`]，供测试和 mock core 使用。
#[derive(Default)]
pub struct CollectingEventSink {
    events: Mutex<Vec<CoreEvent>>,
}

impl CollectingEventSink {
    /// 创建空的收集器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 取走已收集的全部事件（按到达顺序），内部清空。
    pub fn take(&self) -> Vec<CoreEvent> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl EventSink for CollectingEventSink {
    fn on_event(&self, event: CoreEvent) {
        // 测试用实现：中毒后数据仍可用。
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event);
    }
}

/// 宿主在启动时组装、交给 `Core::new` 的全部平台实现。
///
/// 各字段在 core 生命周期内不会被替换；core 以 `Arc` 共享它们给各工作线程。
#[derive(Clone)]
pub struct Platform {
    /// 事件出口。
    pub events: Arc<dyn EventSink>,
    /// 凭据存储。
    pub secrets: Arc<dyn SecretStore>,
    /// 主机密钥存储；`None` 表示使用 core 内置的 `~/.ssh/known_hosts` 实现
    /// （Direct / Setapp）。MAS 由宿主或 core 用数据库实现后传入。
    pub host_keys: Option<Arc<dyn HostKeyStore>>,
    /// 沙盒文件访问；Direct / Setapp 用 [`NoopFileAccess`]。
    pub file_access: Arc<dyn FileAccess>,
    /// 系统通知。
    pub notifier: Arc<dyn Notifier>,
    /// 商店购买；Direct 版为 `None`。
    pub purchase: Option<Arc<dyn PurchaseProvider>>,
}

impl Platform {
    /// 测试用组装：除事件出口外全部使用内存或空实现。
    ///
    /// `host_keys` 必须是内存实现而不能是 `None`：`None` 的含义是使用内置的
    /// `~/.ssh/known_hosts`，测试会因此读写开发者本机的真实文件。
    pub fn for_tests(events: Arc<dyn EventSink>) -> Platform {
        Platform {
            events,
            secrets: Arc::new(MemorySecretStore::new()),
            host_keys: Some(Arc::new(MemoryHostKeyStore::new())),
            file_access: Arc::new(NoopFileAccess),
            notifier: Arc::new(NoopNotifier),
            purchase: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_file_access_roundtrip() {
        let f = NoopFileAccess;
        let b = f.create_bookmark("/tmp/中文 dir").unwrap();
        assert_eq!(b, "/tmp/中文 dir".as_bytes());
        let g = f.start_access(&b).unwrap();
        assert_eq!(g.path, "/tmp/中文 dir");
        assert!(!g.stale);
        f.stop_access(&g.path);
        assert!(f.start_access(&[0xff]).is_err());
    }

    #[test]
    fn collecting_sink_collects_and_takes() {
        let sink = CollectingEventSink::new();
        let s = SessionId::from_raw(1);
        sink.on_event(CoreEvent::TerminalDirty { session: s });
        sink.on_event(CoreEvent::PromptDismissed {
            prompt: kanso_types::PromptId::from_raw(2),
        });
        let got = sink.take();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], CoreEvent::TerminalDirty { session: s });
        assert!(sink.take().is_empty());
    }

    #[test]
    fn platform_for_tests_and_object_safety() {
        fn assert_send_sync<T: Send + Sync + ?Sized>(_: &T) {}
        let sink = Arc::new(CollectingEventSink::new());
        let p = Platform::for_tests(sink.clone());
        assert_send_sync(&p.events);
        assert_send_sync(&p.secrets);
        assert_send_sync(&p.file_access);
        assert_send_sync(&p.notifier);
        assert_send_sync(&p);
        let h: Option<&Arc<dyn HostKeyStore>> = p.host_keys.as_ref();
        let pu: Option<&Arc<dyn PurchaseProvider>> = p.purchase.as_ref();
        assert!(h.is_some() && pu.is_none());
        p.events.on_event(CoreEvent::TerminalDirty {
            session: SessionId::from_raw(7),
        });
        assert_eq!(sink.take().len(), 1);
        p.notifier.notify(Notification::CommandFinished {
            session: SessionId::from_raw(7),
            exit_code: Some(0),
        });
    }
}
