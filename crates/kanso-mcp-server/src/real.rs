//! 真实服务（F9 阶段 1）：单实例锁、平台本机通道监听、令牌校验、rmcp 服务端、审计（architecture §16）。
//!
//! 子模块分工：`ipc`（单实例锁与本机通道）、`registry`（客户端登记与回调）、`audit`（审计写入与清理）、
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
mod gate;
mod identity;
#[cfg(unix)]
#[path = "real/instance.rs"]
mod ipc;
#[cfg(windows)]
#[path = "real/windows.rs"]
mod ipc;
mod registry;
mod terminal;
mod tools;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kanso_core::Core;
use kanso_types::secret::SecretString;
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use self::audit::AuditSink;
pub use self::gate::ClockFn;
use self::gate::TabGate;
use self::registry::Registry;
use crate::{
    McpClientInfo, McpServerConfig, McpServerDelegate, McpServerError, McpServerStatus,
    McpUnavailable, Result,
};

/// 停止时等审计任务写完剩余记录的上限。
const AUDIT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// 停止时等进行中的连接任务（含正在执行的工具调用）结束的上限。
const CONN_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// 停止时等运行时里的任务退出的上限。
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// 取进程祖先链的函数，见 `identity::ancestors`。
pub type AncestorsFn = Arc<dyn Fn(u32) -> Vec<u32> + Send + Sync>;

/// 仅供测试注入：时间来源与祖先链读取。生产代码用 [`TestHooks::default`]（真实时钟与真实进程表）。
/// 做成注入点而不是 `cfg(test)`，是因为集成测试（`tests/`）链接的是普通构建的库。
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct TestHooks {
    /// 冷却等用的单调时钟。
    pub clock: Option<ClockFn>,
    /// 按对端 pid 取祖先链（由近及远，含自身）。
    pub ancestors: Option<AncestorsFn>,
}

/// 连接任务与工具共用的上下文，每次 `start` 重建（审计队列随运行时一起重建）。
struct Shared {
    core: Arc<Core>,
    delegate: Arc<dyn McpServerDelegate>,
    registry: Arc<Registry>,
    /// 标签写锁与用户接管冷却，跨客户端共享。
    gate: Arc<TabGate>,
    /// 连接建立时取对端进程祖先链用。
    ancestors: AncestorsFn,
    audit: AuditSink,
    /// 所有连接任务都登记在这里，停止时要等它们结束：连接任务里正在执行的工具调用，
    /// 结束时还要写审计，必须赶在审计队列关闭之前。
    conns: TaskTracker,
}

/// 单实例锁的结果。
enum Lock {
    /// 持有锁；保留句柄直到进程结束。
    Held {
        _guard: ipc::InstanceGuard,
        socket: PathBuf,
    },
    /// 另一个实例持有锁。
    Another,
    /// 目录或锁文件无法创建，`start` 时先重试一次，仍失败就如实报告。
    Failed(String),
}

/// 运行中的状态；停止时被取走并拆除。
struct Running {
    runtime: Runtime,
    /// 只停 accept 循环；与 `shutdown` 分开，是为了先停止接新连接、等连接任务结束，最后才让审计收尾。
    accept_stop: CancellationToken,
    accept: tokio::task::JoinHandle<()>,
    conns: TaskTracker,
    shutdown: CancellationToken,
    audit: tokio::task::JoinHandle<()>,
    /// 置位后审计任务不再回调 `on_audit_changed`：`Drop` 拆除时宿主不应再收到任何事件。
    /// 用锁而不是原子量：审计任务在锁内回调，置位返回即保证之后没有回调，见 `audit::notify_changed`。
    audit_quiet: Arc<Mutex<bool>>,
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
    hooks: TestHooks,
    /// 串行化 `start` / `stop`：`stop` 在释放 `inner` 之后才拆运行时，期间若放任 `start` 进来，
    /// 会出现两个运行时同时监听同一个 socket。`status()` 等只读方法不走这把锁，不会被拆除过程拖住。
    /// 加锁顺序：`lifecycle` → `inner` → `Registry`。
    lifecycle: Mutex<()>,
    inner: Mutex<Inner>,
}

impl RealServer {
    pub(crate) fn new(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> RealServer {
        RealServer::with_dir(core, delegate, None, TestHooks::default())
    }

    pub(crate) fn in_dir(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        dir: PathBuf,
        hooks: TestHooks,
    ) -> RealServer {
        RealServer::with_dir(core, delegate, Some(dir), hooks)
    }

    /// 构造时就取锁：App 从启动起就要持有它，转发程序靠它区分"未运行"与"开关关闭"。
    fn with_dir(
        core: Arc<Core>,
        delegate: Arc<dyn McpServerDelegate>,
        dir: Option<PathBuf>,
        hooks: TestHooks,
    ) -> RealServer {
        let server = RealServer {
            registry: Arc::new(Registry::new(delegate.clone())),
            delegate,
            dir,
            hooks,
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
        match ipc::acquire(self.core.bundle_id(), self.dir.as_deref()) {
            Ok((Some(guard), socket)) => Lock::Held {
                _guard: guard,
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
        // `Another` 也要重试：转发程序为区分"未运行"与"开关关闭"会短暂对锁文件做探测加锁，
        // 构造时恰好撞上就会被误记成另一个实例，若不重新尝试，本实例此后永远起不来。
        // 仍是 `Another` 才说明真有另一个实例。
        if matches!(inner.lock, Lock::Failed(_) | Lock::Another) {
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
        let listener = {
            // 监听器必须在运行时上下文中注册其 IO 驱动。
            let _guard = runtime.enter();
            ipc::Listener::bind(socket)?
        };

        let shutdown = CancellationToken::new();
        let (sink, rx) = audit::channel();
        let conns = TaskTracker::new();
        let clock = self
            .hooks
            .clock
            .clone()
            .unwrap_or_else(|| Arc::new(std::time::Instant::now));
        let shared = Arc::new(Shared {
            core: self.core.clone(),
            delegate: self.delegate.clone(),
            registry: self.registry.clone(),
            gate: TabGate::new(clock),
            ancestors: self
                .hooks
                .ancestors
                .clone()
                .unwrap_or_else(|| Arc::new(identity::ancestors)),
            audit: sink,
            conns: conns.clone(),
        });
        self.registry.open(token);
        let accept_stop = CancellationToken::new();
        let accept = runtime.spawn(accept_loop(listener, shared, accept_stop.clone()));
        let audit_quiet = Arc::new(Mutex::new(false));
        let audit = runtime.spawn(audit::run(
            self.core.clone(),
            self.delegate.clone(),
            audit_quiet.clone(),
            rx,
            shutdown.clone(),
        ));
        Ok(Running {
            runtime,
            accept_stop,
            accept,
            conns,
            shutdown,
            audit,
            audit_quiet,
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
        Self::teardown(&self.registry, running, false);
    }

    /// 拆除运行时。顺序有意义：
    /// 1. 先断开客户端（连接任务随之收到取消）；
    /// 2. 停止 accept，此后不会有新连接任务；
    /// 3. 等连接任务全部结束：正在执行的工具调用结束后还要 `audit.push`，此时审计必须还在收；
    /// 4. 最后取消 `shutdown`，让审计任务写完队列里剩下的记录，再等它。
    ///
    /// 步骤 3、4 都设上限：连接任务或数据库卡住时不拖住宿主线程。`silent` 为真（`Drop`）时断开客户端不回调。
    fn teardown(registry: &Registry, running: Running, silent: bool) {
        if silent {
            // 要在等连接、等审计收尾之前置位：这期间的落库（含关停时最后一批）也不能回调。
            *running
                .audit_quiet
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = true;
            registry.close_silently();
        } else {
            registry.close();
        }
        running.accept_stop.cancel();
        let Running {
            runtime,
            accept,
            conns,
            shutdown,
            audit,
            socket,
            ..
        } = running;
        runtime.block_on(async {
            let _ = tokio::time::timeout(CONN_DRAIN_TIMEOUT, async {
                let _ = accept.await;
                conns.close();
                conns.wait().await;
            })
            .await;
            shutdown.cancel();
            let _ = tokio::time::timeout(AUDIT_FLUSH_TIMEOUT, audit).await;
        });
        runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        ipc::cleanup(&socket);
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
            Self::teardown(&self.registry, running, true);
        }
    }
}

async fn accept_loop(
    mut listener: ipc::Listener,
    shared: Arc<Shared>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    shared.conns.spawn(conn::handle(stream, shared.clone()));
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
