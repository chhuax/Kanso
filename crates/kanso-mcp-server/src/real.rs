//! 真实服务（F9 阶段 1）：单实例锁、Unix socket 监听、令牌校验、rmcp 服务端、审计（architecture §16）。
//!
//! 子模块分工：`instance`（目录与单实例锁）、`registry`（客户端登记与回调）、`audit`（审计写入与清理）、
//! `conn`（单个连接的校验与握手）、`tools`（rmcp 处理器与工具）。
//!
//! # 回调
//!
//! 与 mock 一样，所有 delegate 回调都在持有对应状态锁时发出（状态回调持 `inner`，客户端列表回调持
//! `Registry` 的锁），回调顺序与状态变化严格一致。回调契约禁止重入，所以持锁回调是安全的。
//! 加锁顺序固定为 `lifecycle` → `inner` → `Registry`，连接任务只会拿 `Registry` 的锁。
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

/// 单实例锁的结果。
enum Lock {
    /// 持有锁；文件要一直保持打开，锁随它释放。
    Held { _file: File, socket: PathBuf },
    /// 另一个实例持有锁。
    Another,
    /// 目录或锁文件无法创建，`start` 时先重试一次，仍失败就如实报告。
    Failed(String),
}

/// 运行中的状态；停止时被取走并拆除。
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
    /// 测试指定的目录；`None` 时按 `core.bundle_id()` 推出。
    dir: Option<PathBuf>,
    /// 串行化 `start` / `stop`：`stop` 在释放 `inner` 之后才拆运行时，期间若放任 `start` 进来，
    /// 会出现两个运行时同时监听同一个 socket。`status()` 等只读方法不走这把锁，不会被拆除过程拖住。
    /// 加锁顺序：`lifecycle` → `inner` → `Registry`。
    lifecycle: Mutex<()>,
    inner: Mutex<Inner>,
}

impl RealServer {
    pub(crate) fn new(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> RealServer {
        RealServer::with_dir(core, delegate, None)
    }

    pub(crate) fn in_dir(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        dir: PathBuf,
    ) -> RealServer {
        RealServer::with_dir(core, delegate, Some(dir))
    }

    /// 构造时就取锁：App 从启动起就要持有它，转发程序靠它区分"未运行"与"开关关闭"。
    fn with_dir(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        dir: Option<PathBuf>,
    ) -> RealServer {
        let server = RealServer {
            registry: Arc::new(Registry::new(delegate.clone())),
            delegate,
            dir,
            lifecycle: Mutex::new(()),
            inner: Mutex::new(Inner {
                status: McpServerStatus::Stopped,
                lock: Lock::Failed(String::new()),
                running: None,
            }),
            core,
        };
        let lock = server.acquire_lock();
        {
            let mut inner = server.lock();
            if matches!(lock, Lock::Another) {
                inner.status = McpServerStatus::Unavailable(McpUnavailable::AnotherInstance);
            }
            inner.lock = lock;
        }
        server
    }

    fn acquire_lock(&self) -> Lock {
        let paths = match &self.dir {
            Some(dir) => Ok(McpPaths::in_dir(dir, self.core.bundle_id())),
            None => McpPaths::for_bundle(self.core.bundle_id()),
        };
        match paths.and_then(|p| Ok((instance::acquire(&p)?, p.socket))) {
            Ok((Some(file), socket)) => Lock::Held {
                _file: file,
                socket,
            },
            Ok((None, _)) => Lock::Another,
            Err(e) => Lock::Failed(format!("cannot prepare mcp directory or lock: {e}")),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 状态里没有需要在 panic 后保护的跨字段不变式，中毒后继续使用。
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lifecycle(&self) -> MutexGuard<'_, ()> {
        self.lifecycle.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 设置状态并回调，调用方持有 `inner` 锁（见模块文档）。
    fn set_status(&self, inner: &mut Inner, status: McpServerStatus) {
        inner.status = status.clone();
        self.delegate.on_status_changed(status);
    }

    pub(crate) fn start(&self, config: McpServerConfig) -> Result<()> {
        let _lifecycle = self.lifecycle();
        self.stop_running();
        let mut inner = self.lock();
        // 目录创建失败可能是暂时的（例如磁盘刚恢复），先重试一次。
        if matches!(inner.lock, Lock::Failed(_)) {
            inner.lock = self.acquire_lock();
            if matches!(inner.lock, Lock::Another) {
                inner.status = McpServerStatus::Unavailable(McpUnavailable::AnotherInstance);
            }
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
        // 令牌为空等于没有令牌，不能监听；bundle id 不一致说明宿主传错了配置，
        // 路径在构造时已经按 core 的 bundle id 定死，不能静默忽略。
        if config.token.expose().is_empty() {
            return Err(self.fail(&mut inner, "mcp token is empty".to_owned()));
        }
        if config.bundle_id != self.core.bundle_id() {
            let detail = format!(
                "bundle id {:?} does not match the core's {:?}",
                config.bundle_id,
                self.core.bundle_id()
            );
            return Err(self.fail(&mut inner, detail));
        }
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

    /// 停止运行中的服务；没有运行时什么也不做。调用方持有 `lifecycle` 锁。
    ///
    /// 先在 `inner` 锁内取出 `Running`、改状态并回调，**放锁后**再拆除：拆除要等审计与运行时，
    /// 最长几秒，不能让 `status()` 等方法一直被挡住。
    fn stop_running(&self) {
        let running = {
            let mut inner = self.lock();
            let Some(running) = inner.running.take() else {
                return;
            };
            self.set_status(&mut inner, McpServerStatus::Stopped);
            running
        };
        Self::teardown(&self.registry, running);
    }

    /// 拆除运行时。顺序有意义：先断开客户端（此后不再有新调用产生审计），
    /// 再取消关停令牌让审计任务写完队列里剩下的记录，否则正在进行的调用写的审计会因队列已关闭而丢失。
    fn teardown(registry: &Registry, running: Running) {
        registry.close();
        running.shutdown.cancel();
        // 设上限：数据库卡住时不拖住宿主线程。
        let audit = running.audit;
        running.runtime.block_on(async {
            let _ = tokio::time::timeout(AUDIT_FLUSH_TIMEOUT, audit).await;
        });
        running.runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        let _ = fs::remove_file(&running.socket);
    }

    /// 停止后状态回到 `Stopped`。启动失败（`Unavailable(Failed)`）后用户关掉开关时也要回到 `Stopped`，
    /// 否则设置页在开关已关的情况下还一直显示"不可用"。没有拿到锁（`AnotherInstance`）时保持不变。
    pub(crate) fn stop(&self) {
        let _lifecycle = self.lifecycle();
        self.stop_running();
        let mut inner = self.lock();
        if matches!(
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

impl Drop for RealServer {
    /// 兜底：宿主退出时应当已经调用过 `stop`；没有的话这里也写完审计、删 socket，
    /// 不回调（对象已在销毁，宿主不应再收到事件）。
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = inner.running.take() {
            Self::teardown(&self.registry, running);
        }
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
