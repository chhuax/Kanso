//! 真实服务（F9 阶段 1）：单实例锁、Unix socket 监听、令牌校验、rmcp 服务端、审计（architecture §16）。
//!
//! 子模块分工：`instance`（目录与单实例锁）、`registry`（客户端登记与回调）、`audit`（审计写入与清理）、
//! `conn`（单个连接的校验与握手）、`tools`（rmcp 处理器与工具）。
//!
//! # 回调
//!
//! 与 mock 一样，所有 delegate 回调都在持有对应状态锁时发出（状态回调持 `inner`，客户端列表回调持
//! `Registry` 的锁），回调顺序与状态变化严格一致。回调契约禁止重入，所以持锁回调是安全的。
//! 加锁顺序固定为 `inner` → `Registry`，连接任务只会拿 `Registry` 的锁。
//!
//! # 线程
//!
//! 服务跑在自己的 tokio 运行时上（与 core 的运行时无关）。`start` / `stop` 由宿主线程调用，
//! 内部会 `block_on`，不能在运行时线程里调用。

mod audit;
mod conn;
mod instance;
mod registry;
mod tools;

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kanso_core::Core;
use kanso_mcp::paths::McpPaths;
use kanso_types::secret::SecretString;
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;

use self::audit::AuditSink;
use self::registry::Registry;
use crate::{
    McpClientInfo, McpServerConfig, McpServerDelegate, McpServerError, McpServerStatus,
    McpUnavailable, Result,
};

/// 停止时等审计任务写完剩余记录的上限。
const AUDIT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// 停止时等运行时里的任务退出的上限。
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// 连接任务与工具共用的上下文，每次 `start` 重建（审计队列随运行时一起重建）。
struct Shared {
    core: Arc<Core>,
    registry: Arc<Registry>,
    audit: AuditSink,
}

/// 单实例锁的结果，`new` 时确定。
enum Lock {
    /// 持有锁；文件要一直保持打开，锁随它释放。
    Held { _file: File, socket: PathBuf },
    /// 另一个实例持有锁。
    Another,
    /// 目录或锁文件无法创建，`start` 时如实报告。
    Failed(String),
}

/// 运行中的状态；`stop` 取走它并关闭。
struct Running {
    runtime: Runtime,
    shutdown: CancellationToken,
    audit: tokio::task::JoinHandle<()>,
    socket: PathBuf,
}

struct Inner {
    status: McpServerStatus,
    lock: Lock,
    running: Option<Running>,
}

pub(crate) struct RealServer {
    core: Arc<Core>,
    delegate: Arc<dyn McpServerDelegate>,
    registry: Arc<Registry>,
    inner: Mutex<Inner>,
}

impl RealServer {
    pub(crate) fn new(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> RealServer {
        let paths = McpPaths::for_bundle(core.bundle_id());
        RealServer::with_paths(core, delegate, paths)
    }

    pub(crate) fn in_dir(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        dir: PathBuf,
    ) -> RealServer {
        let paths = McpPaths::in_dir(&dir, core.bundle_id());
        RealServer::with_paths(core, delegate, Ok(paths))
    }

    /// 构造时就取锁：App 从启动起就要持有它，转发程序靠它区分"未运行"与"开关关闭"。
    fn with_paths(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        paths: io::Result<McpPaths>,
    ) -> RealServer {
        let lock = match paths.and_then(|p| Ok((instance::acquire(&p)?, p.socket))) {
            Ok((Some(file), socket)) => Lock::Held {
                _file: file,
                socket,
            },
            Ok((None, _)) => Lock::Another,
            Err(e) => Lock::Failed(format!("cannot prepare mcp directory or lock: {e}")),
        };
        let status = match lock {
            Lock::Another => McpServerStatus::Unavailable(McpUnavailable::AnotherInstance),
            _ => McpServerStatus::Stopped,
        };
        RealServer {
            core,
            registry: Arc::new(Registry::new(delegate.clone())),
            delegate,
            inner: Mutex::new(Inner {
                status,
                lock,
                running: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 状态里没有需要在 panic 后保护的跨字段不变式，中毒后继续使用。
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 设置状态并回调，调用方持有 `inner` 锁（见模块文档）。
    fn set_status(&self, inner: &mut Inner, status: McpServerStatus) {
        inner.status = status.clone();
        self.delegate.on_status_changed(status);
    }

    pub(crate) fn start(&self, config: McpServerConfig) -> Result<()> {
        let mut inner = self.lock();
        if inner.running.is_some() {
            self.stop_running(&mut inner);
        }
        let socket = match &inner.lock {
            Lock::Held { socket, .. } => socket.clone(),
            Lock::Another => {
                // 状态本来就是 AnotherInstance，仍按契约回调一次。
                let status = inner.status.clone();
                self.delegate.on_status_changed(status);
                return Err(McpServerError::AnotherInstance);
            }
            Lock::Failed(detail) => {
                let detail = detail.clone();
                return Err(self.fail(&mut inner, detail));
            }
        };
        match self.launch(&socket, config.token) {
            Ok(running) => {
                let status = McpServerStatus::Running {
                    socket_path: socket.to_string_lossy().into_owned(),
                };
                inner.running = Some(running);
                self.set_status(&mut inner, status);
                Ok(())
            }
            Err(e) => Err(self.fail(&mut inner, format!("cannot start mcp server: {e}"))),
        }
    }

    fn fail(&self, inner: &mut Inner, detail: String) -> McpServerError {
        tracing::warn!("{detail}");
        self.set_status(
            inner,
            McpServerStatus::Unavailable(McpUnavailable::Failed {
                detail: detail.clone(),
            }),
        );
        McpServerError::Failed { detail }
    }

    /// 建运行时、绑定 socket、启动接受连接与审计任务。
    fn launch(&self, socket: &Path, token: SecretString) -> io::Result<Running> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kanso-mcp")
            .enable_all()
            .build()?;
        // 上次异常退出可能留下 socket 文件，bind 遇到已存在的文件会失败。
        // 单实例锁已经保证没有别的实例在用它。
        match fs::remove_file(socket) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let std_listener = std::os::unix::net::UnixListener::bind(socket)?;
        // bind 按 umask 建文件，之后再收紧到 0600；所在目录是 0700，这段时间内其他用户也到不了。
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        std_listener.set_nonblocking(true)?;
        let listener = {
            // 转成 tokio 的监听器需要处在运行时上下文里（要向它注册 IO 驱动）。
            let _guard = runtime.enter();
            tokio::net::UnixListener::from_std(std_listener)?
        };

        let shutdown = CancellationToken::new();
        let (sink, rx) = audit::channel();
        let shared = Arc::new(Shared {
            core: self.core.clone(),
            registry: self.registry.clone(),
            audit: sink,
        });
        self.registry.open(token);
        runtime.spawn(accept_loop(listener, shared, shutdown.clone()));
        let audit = runtime.spawn(audit::run(self.core.clone(), rx, shutdown.clone()));
        Ok(Running {
            runtime,
            shutdown,
            audit,
            socket: socket.to_owned(),
        })
    }

    /// 停止运行中的服务，状态改为 `Stopped` 并回调；调用方持有 `inner` 锁且 `running` 为 `Some`。
    fn stop_running(&self, inner: &mut Inner) {
        let Some(running) = inner.running.take() else {
            return;
        };
        running.shutdown.cancel();
        self.registry.close();
        // 审计任务收到关停信号后会写完队列里剩下的记录；设上限，数据库卡住时不拖住宿主线程。
        let audit = running.audit;
        running.runtime.block_on(async {
            let _ = tokio::time::timeout(AUDIT_FLUSH_TIMEOUT, audit).await;
        });
        running.runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        let _ = fs::remove_file(&running.socket);
        self.set_status(inner, McpServerStatus::Stopped);
    }

    /// 停止后状态回到 `Stopped`。启动失败（`Unavailable(Failed)`）后用户关掉开关时也要回到 `Stopped`，
    /// 否则设置页在开关已关的情况下还一直显示"不可用"。没有拿到锁（`AnotherInstance`）时保持不变。
    pub(crate) fn stop(&self) {
        let mut inner = self.lock();
        if inner.running.is_some() {
            self.stop_running(&mut inner);
        } else if matches!(
            inner.status,
            McpServerStatus::Unavailable(McpUnavailable::Failed { .. })
        ) {
            self.set_status(&mut inner, McpServerStatus::Stopped);
        }
    }

    pub(crate) fn set_token(&self, token: SecretString) {
        self.registry.set_token(token);
    }

    pub(crate) fn status(&self) -> McpServerStatus {
        self.lock().status.clone()
    }

    pub(crate) fn clients(&self) -> Vec<McpClientInfo> {
        self.registry.list()
    }

    pub(crate) fn disconnect(&self, client_id: u64) {
        self.registry.remove(client_id);
    }
}

async fn accept_loop(
    listener: tokio::net::UnixListener,
    shared: Arc<Shared>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(conn::handle(stream, shared.clone()));
                }
                Err(e) => {
                    // 例如文件描述符耗尽：稍等再试，避免空转占满 CPU。
                    tracing::warn!("mcp accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}
