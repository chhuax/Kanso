//! 真实 core 后端。
//!
//! 已实现（T8）：数据存储——Profile、分组、Sender 命令、凭据、历史清除、`.kanso` 导入导出，
//! 以及按本地 Profile 打开会话；数据库为 `<data_dir>/kanso.db`（`kanso-store`）。
//!
//! 已实现（I1）：本地会话全链路——打开 / 关闭 / 输入 / 取帧 / 选区 / 搜索 / 工作目录 / 尺寸，
//! 由 `kanso-session` 的会话管理器完成，这里只负责 `Core` API 到它的映射、错误映射与 tokio 运行时。
//!
//! 已实现（I2）：单跳 SSH 会话——按 Profile 打开 / 复制标签、认证提问通道（`respond_prompt`）、
//! 主机密钥变更确认后自动重连、`~/.ssh/known_hosts`（Direct / Setapp）；接线在 `ssh.rs`，
//! 会话本身在 `kanso-session`。
//!
//! 已实现（T6b）：跳板链（按 Profile 的 jump 引用解析整条链）、ProxyCommand（Direct / Setapp）、
//! 没有 OSC 7 时经 exec 通道查询远端工作目录。
//!
//! 已实现（T7b）：纯 SFTP 会话（`ProfileKind::Sftp`，与 SSH 会话共用连接器，只是不开 shell）、
//! 文件操作（`sftp_*`）、SFTP 传输（`download` / `upload` / `cancel_transfer`，事件见各方法文档）。
//! 引擎在 `kanso-ssh::sftp`，会话持有的 `SftpClient` 在 `kanso-session`；这里负责 API 映射、
//! 传输任务与事件时序、会话关闭时取消传输。
//!
//! # 尚未实现的方法
//!
//! 签名与 `MockCore` 一一对应（保持同步靠编译器：`Core` 的 `match` 分发会让两个 backend 的签名不一致时
//! 直接编译失败）。尚未实现的方法**不得 panic**：panic 穿过 FFI 会让整个 App 崩溃。统一约定：
//! 返回 `Result` 的返回 `CoreError::NotSupported`；无返回值的什么也不做；返回集合的返回空；
//! 返回 `Option` 的返回 `None`。由哪个后续工作包实现，写在各方法的文档里：
//!
//! - 命令历史的写入与补全查询（`Store` 已提供，`Core` API 尚无对应入口）：**F3**；导入导出 UI：**F6**；
//! - SFTP 的界面（Filer）：**F1**；
//! - 终端内传输（XMODEM / ZMODEM）：**F4**；
//! - OpenSSH 配置扫描 / 导入：**T3**（解析）、**F6**；
//! - `.kanso` 旧版（应用标记 `Kanso`）兼容导入（store-import.md §6.3）：待拿到样例文件；
//! - 授权：**T11**（License Key）、**I5**（商店购买）；
//! - MCP 的连接、无界面会话、后台执行与 SFTP 补充操作：**F9**（授权与审计的存取、终端的状态 / 读取 / 命令 / 输入已实现，见 `real::mcp`）。

// 尚未实现的方法的参数没有用到；工作包落地后会自然用上。
#![allow(unused_variables)]

mod connect;
mod exec;
mod mcp;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kanso_frame::Frame;
use kanso_license::{Entitlement, PurchaseOutcome};
use kanso_secret::{SecretError, SecretKind, SecretRef, SecretStore};
use kanso_session::{
    EnvSnapshot, IdleKeepalive, ManagerConfig, OpenError, OpenRequest, Session, SessionHost,
    SessionManager, SshLaunch,
};
use kanso_ssh::sftp::{
    CancelToken, DirListing, MAX_DEPTH, SftpError, SkipReason, TransferOptions,
    TransferOutcome as SftpOutcome, TransferProgress, is_safe_name,
};
use kanso_ssh::{
    AuthMethod as SshAuthMethod, HostKeyStore, KnownHostsFile, RevokedKeys, SftpClient,
};
use kanso_store::{ImportData, ImportError, Store, StoreError};
use kanso_types::agent::AgentKind;
use kanso_types::event::{TransferDirection, TransferOutcome};
use kanso_types::message::{Endpoint, Reason};
use kanso_types::profile::{
    AuthMethod as ProfileAuthMethod, EncodingLabel, Group, Locale, Profile, ProfileKind,
    SessionKind, Snippet, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::transfer::ConflictPolicy;
use kanso_types::transfer::XferProtocol;
use kanso_types::{
    CoreEvent, GridSize, GroupId, KeyEvent, Message, Modifiers, MouseEvent, PasteContent,
    ProfileId, PromptId, PromptResponse, SessionId, SnippetId, TerminalOptions, TimestampMode,
    TransferId, ViewportPoint,
};

use crate::api::{
    CoreConfig, CoreError, DeleteImpact, DirStream, ImportReport, OpenTarget, Result, SearchQuery,
    SearchStatus, SessionInfo, SshConfigImportReport, SshConfigScan, XmodemBlock,
};
use crate::platform::{EventSink, Notification, Platform};
use crate::ssh::{ConnectSpec, CoreSshConnector, HostKeys, NeedSlot, PromptTable, SshEnv};
use crate::sshconfig_import::{self, SshConfigEnv};
use crate::validate::{MAX_JUMP_HOPS, validate_profile};

/// 数据库文件名，位于 `data_dir` 下。
const DB_FILE_NAME: &str = "kanso.db";

fn store_err(e: StoreError) -> CoreError {
    CoreError::Internal {
        detail: format!("store error: {e}"),
    }
}

fn secret_err(e: SecretError) -> CoreError {
    CoreError::User(Message::SecretStoreFailed {
        detail: e.to_string(),
    })
}

fn join_err(e: tokio::task::JoinError) -> CoreError {
    CoreError::Internal {
        detail: format!("blocking task failed: {e}"),
    }
}

pub(crate) fn io_reason(e: &std::io::Error) -> Reason {
    match e.kind() {
        std::io::ErrorKind::NotFound => Reason::NotFound,
        std::io::ErrorKind::PermissionDenied => Reason::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => Reason::AlreadyExists,
        _ => Reason::Other {
            detail: e.to_string(),
        },
    }
}

fn sftp_err(e: SftpError) -> CoreError {
    CoreError::User(e.to_message())
}

/// 真实 SFTP 目录流（[`DirStream`] 的来源之一）。
///
/// 惰性：创建时不做任何 IO，第一次 `next_batch` 才打开目录。
/// 每次读取都放到 core 的运行时上执行（理由同 `RealCore::on_runtime`）：`DirListing` 的读取需要 `&mut`，
/// 所以每次把它移进任务、读完再放回；出错或读完后状态变为 `Done`，之后一律返回 `Ok(None)`。
pub(crate) struct RemoteDirList {
    handle: tokio::runtime::Handle,
    client: SftpClient,
    path: String,
    state: ListState,
}

enum ListState {
    Pending,
    Open(DirListing),
    Done,
}

impl RemoteDirList {
    pub(crate) async fn next_batch(
        &mut self,
        limit: usize,
    ) -> Result<Option<Vec<kanso_types::fs::DirEntry>>> {
        let state = std::mem::replace(&mut self.state, ListState::Done);
        let client = self.client.clone();
        let path = self.path.clone();
        let joined = self
            .handle
            .spawn(async move {
                let mut listing = match state {
                    ListState::Done => return (Ok(None), ListState::Done),
                    ListState::Open(l) => l,
                    ListState::Pending => match client.list(&path).await {
                        Ok(l) => l,
                        Err(e) => return (Err(e), ListState::Done),
                    },
                };
                match listing.next_batch_limited(limit).await {
                    Ok(Some(batch)) => (Ok(Some(batch)), ListState::Open(listing)),
                    Ok(None) => (Ok(None), ListState::Done),
                    Err(e) => (Err(e), ListState::Done),
                }
            })
            .await
            .map_err(join_err)?;
        let (result, state) = joined;
        self.state = state;
        result.map_err(sftp_err)
    }
}

/// 递归删除远端目录。
///
/// 不用递归 async 函数，而是显式栈：先逐层列出并删掉文件，目录记下来最后按"后访问的先删"删除
/// （后代一定晚于祖先被访问，所以反序保证先空后删）。符号链接（含指向目录的）只删链接本身，
/// 绝不跟进，否则一个指回上级的链接会让递归删除越界或死循环；深度超过 [`MAX_DEPTH`] 报协议错误，
/// 挡住恶意服务器构造的无限深目录。
async fn remove_tree(client: &SftpClient, root: &str) -> std::result::Result<(), SftpError> {
    if client.lstat(root).await?.is_symlink() {
        return client.remove_file(root).await;
    }
    let mut stack = vec![(root.to_owned(), 0usize)];
    let mut dirs = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(SftpError::Protocol {
                detail: format!("directory nesting exceeds {MAX_DEPTH}"),
            });
        }
        let mut files = Vec::new();
        let mut listing = client.list(&dir).await?;
        while let Some(batch) = listing.next_batch().await? {
            for e in batch {
                if e.is_dir && !e.is_symlink {
                    stack.push((e.path, depth + 1));
                } else {
                    files.push(e.path);
                }
            }
        }
        // 列表读完（句柄已关）再删，避免在打开的目录句柄上边读边删。
        for f in files {
            client.remove_file(&f).await?;
        }
        dirs.push(dir);
    }
    for d in dirs.into_iter().rev() {
        client.remove_dir(&d).await?;
    }
    Ok(())
}

/// 路径的最后一个分量，必须能安全地作为单个本地文件名（下载落地用）；`/`、`..` 等返回 `XferUnsafeName`。
fn path_name(path: &str) -> Result<String> {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if is_safe_name(name) {
        Ok(name.to_owned())
    } else {
        Err(CoreError::User(Message::XferUnsafeName {
            name: name.to_owned(),
        }))
    }
}

/// 远端路径拼接（远端总是 `/` 分隔，与本机平台无关）。
fn remote_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn failed(e: &kanso_ssh::sftp::TransferError) -> TransferOutcome {
    TransferOutcome::Failed {
        message: e.to_message(),
        already_exists: e.is_already_exists(),
    }
}

/// 引擎的传输报告 → 对外的结果。
///
/// 只有"目标已存在而按 Skip 跳过"的项计入 `Completed.skipped`（UI 要汇报它）；
/// 目录传输里因文件名不安全、符号链接等被引擎跳过的项没有地方带给 UI，只记日志。
fn outcome_of(report: kanso_ssh::sftp::TransferReport) -> TransferOutcome {
    match report.outcome {
        SftpOutcome::Completed => {
            let existing = report
                .skipped
                .iter()
                .filter(|s| s.reason == SkipReason::AlreadyExists)
                .count();
            let other = report.skipped.len() - existing;
            if other > 0 {
                tracing::info!(skipped = other, "sftp transfer skipped items");
            }
            TransferOutcome::Completed {
                skipped: u32::try_from(existing).unwrap_or(u32::MAX),
            }
        }
        SftpOutcome::Cancelled => TransferOutcome::Cancelled,
        SftpOutcome::Failed(e) => failed(&e),
    }
}

/// 把传输结果交给 `*_and_wait` 调用方的通道。
type OutcomeTx = tokio::sync::oneshot::Sender<TransferOutcome>;

/// 等传输任务交回结果。发送端被丢弃而没有发结果（core 运行时已关闭、任务被销毁）时返回 `Internal`。
async fn wait_outcome(
    rx: tokio::sync::oneshot::Receiver<TransferOutcome>,
) -> Result<TransferOutcome> {
    rx.await.map_err(|_| CoreError::Internal {
        detail: "transfer task ended without a result".to_owned(),
    })
}

/// 传输任务向 UI 发事件的出口。只在传输任务内使用，所以事件天然按序。
struct TransferEmitter {
    events: Arc<dyn EventSink>,
    id: TransferId,
    session: SessionId,
    direction: TransferDirection,
    name: String,
}

impl TransferEmitter {
    fn started(&self, total_bytes: Option<u64>) {
        self.events.on_event(CoreEvent::TransferStarted {
            transfer: self.id,
            session: self.session,
            protocol: XferProtocol::Sftp,
            direction: self.direction,
            name: self.name.clone(),
            total_bytes,
        });
    }

    /// 引擎参数。进度回调直接转成事件：引擎已把回调节流到每 50 ms 最多一次，这里不再二次节流；
    /// `known_total` 是调用前已知的单文件大小，引擎报告里还没有总量时用它补上。
    fn options(
        &self,
        conflict: ConflictPolicy,
        cancel: CancelToken,
        known_total: Option<u64>,
    ) -> TransferOptions {
        let events = Arc::clone(&self.events);
        let id = self.id;
        TransferOptions {
            conflict,
            cancel,
            progress: Some(Arc::new(move |p: TransferProgress| {
                events.on_event(CoreEvent::TransferProgress {
                    transfer: id,
                    done_bytes: p.bytes,
                    total_bytes: p.total.or(known_total),
                });
            })),
            ..TransferOptions::default()
        }
    }
}

/// 关闭 core 时等待会话收尾（杀 shell、读写线程退出）的总时限。应用退出不能被卡住。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// 把会话层的出口接到宿主注入的 [`Platform`]：事件走 `events`，后台命令结束走 `notifier`。
struct HostAdapter {
    platform: Platform,
    prompts: Arc<PromptTable>,
    runtime: tokio::runtime::Handle,
    history: Arc<HistoryRecorder>,
    /// 有会话进入 `Closed` 时通知 core 的后台任务（借用连接的释放，见 `connect.rs`）。
    /// 在 `emit` 里只做一次无阻塞的 `send`：`emit` 是在会话的生命周期锁内被调用的，
    /// 不得回调管理器，所以真正的检查要交给运行时上的任务去做。
    closed_tx: tokio::sync::mpsc::UnboundedSender<SessionId>,
}

struct HistoryRecorder {
    store: Arc<Mutex<Store>>,
    enabled: std::sync::atomic::AtomicBool,
    generation: AtomicU64,
}

impl HistoryRecorder {
    fn enable(&self, enabled: bool) {
        let _store = self.store.lock().unwrap_or_else(|error| error.into_inner());
        self.enabled.store(enabled, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl SessionHost for HostAdapter {
    fn record_command(&self, command: &str, host: &str) {
        if !self.history.enabled.load(Ordering::SeqCst) {
            return;
        }
        let generation = self.history.generation.load(Ordering::SeqCst);
        let recorder = Arc::clone(&self.history);
        let command = command.to_owned();
        let host = host.to_owned();
        self.runtime.spawn_blocking(move || {
            let mut store = recorder
                .store
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if recorder.enabled.load(Ordering::SeqCst)
                && recorder.generation.load(Ordering::SeqCst) == generation
            {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                let _ = store.record_history_on_host(&command, &host, now);
            }
        });
    }

    fn emit(&self, event: CoreEvent) {
        if let CoreEvent::SessionStateChanged {
            session,
            state: kanso_types::event::SessionState::Closed(_),
        } = &event
        {
            // 接收端已退出说明 core 正在关闭，忽略。
            let _ = self.closed_tx.send(*session);
        }
        self.platform.events.on_event(event);
    }

    fn transfer_prompt(
        &self,
        session: SessionId,
        request: kanso_types::PromptRequest,
        reply: Box<dyn FnOnce(PromptResponse) + Send>,
    ) {
        let expects = if matches!(
            request,
            kanso_types::PromptRequest::ZmodemPickFiles
                | kanso_types::PromptRequest::XmodemPickFile
        ) {
            crate::ssh::Expects::Files
        } else {
            crate::ssh::Expects::SavePath
        };
        let answer = self.prompts.ask(Some(session), request, expects);
        self.runtime.spawn(async move {
            reply(answer.await.unwrap_or(PromptResponse::Cancel));
        });
    }

    fn transfer_prompt_dismiss(&self, session: SessionId) {
        self.prompts.cancel(Some(session));
    }

    fn command_finished_in_background(&self, session: SessionId, exit_code: Option<i32>) {
        self.platform
            .notifier
            .notify(Notification::CommandFinished { session, exit_code });
    }

    fn agent_turn_done_in_background(&self, session: SessionId, agent: AgentKind) {
        self.platform
            .notifier
            .notify(Notification::AgentTurnDone { session, agent });
    }
}

/// 把一个 SSH Profile 的连接目标转成链上一跳的连接参数。
fn hop_spec(id: ProfileId, name: &str, t: &kanso_types::profile::SshTarget) -> ConnectSpec {
    let (auth, secret_kind) = match t.auth {
        ProfileAuthMethod::Password => (SshAuthMethod::Password, Some(SecretKind::Password)),
        ProfileAuthMethod::PrivateKey => (
            SshAuthMethod::PublicKey {
                path: t.private_key_path.clone().unwrap_or_default(),
            },
            Some(SecretKind::KeyPassphrase),
        ),
        ProfileAuthMethod::Agent => (SshAuthMethod::Agent, None),
    };
    ConnectSpec {
        profile: id,
        name: name.to_owned(),
        endpoint: Endpoint {
            host: t.host.trim().to_owned(),
            port: t.port,
            via: Vec::new(),
        },
        user: t.user.trim().to_owned(),
        auth,
        secret_kind,
        keepalive: t.keepalive.clone(),
    }
}

/// 真实 core。
pub(crate) struct RealCore {
    /// tokio 运行时（architecture §4：进程内唯一，工作线程数 `min(4, 核数)`）。
    /// 本 WP 的本地会话用 std 线程，运行时供 async 方法与以后的 SSH 使用。
    /// `shutdown` 时取走并关闭，所以放在 `Option` 里。
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    handle: tokio::runtime::Handle,
    /// `Arc`：借用连接的释放任务要持有它的弱引用（见 `connect.rs`）。
    sessions: Arc<SessionManager>,
    /// 数据库。`Connection` 不是 `Sync`，所以包在 `Mutex` 里；每次操作都很短，在调用线程上同步执行。
    /// 放进 `Arc` 是因为 `async` 方法要把它交给 `spawn_blocking`（需要 `'static`）。
    store: Arc<Mutex<Store>>,
    history: Arc<HistoryRecorder>,
    secrets: Arc<dyn SecretStore>,
    app_version: String,
    /// 提问表：SSH 认证与主机密钥变更的待应答请求。
    prompts: Arc<PromptTable>,
    events: Arc<dyn EventSink>,
    /// 基础环境快照里的 SSH 相关值（`SSH_AUTH_SOCK`、`HOME`）。
    ssh_env: SshEnv,
    /// 宿主注入的主机密钥存储；`None` 表示用内置的 `~/.ssh/known_hosts`（Direct / Setapp）。
    platform_host_keys: Option<Arc<dyn HostKeyStore>>,
    /// 内置 known_hosts：全部会话共用同一个实例，写入锁才有意义；首次打开 SSH 会话时按 HOME 创建。
    builtin_host_keys: Mutex<Option<Arc<KnownHostsFile>>>,
    /// 由 Profile 打开的会话 → (Profile id, Profile 名)，供 `sessions()` 填 `SessionInfo`。
    /// 会话层不认识 Profile，所以由 core 记录；`close_session` 时清除。
    profile_sessions: Arc<Mutex<HashMap<SessionId, (ProfileId, String)>>>,
    /// 标签自定义名；`close_session` 时清除。
    custom_titles: Mutex<HashMap<SessionId, String>>,
    /// 无界面会话 → 是否借用（`true` 为借用用户建立的连接，`false` 为 MCP 自建），见 `connect.rs`。
    headless: Arc<Mutex<HashMap<SessionId, bool>>>,
    /// 每条连接（按 `Connection::id`）一个 exec 并发限额，见 `exec.rs`。
    exec_slots: Mutex<HashMap<u64, std::sync::Weak<tokio::sync::Semaphore>>>,
    /// ssh_config 求值用的本机环境（F6）。
    ssh_config_env: SshConfigEnv,
    /// 进行中的 SFTP 传输。任务结束（含取消、失败）时自己移除；会话关闭时按 `session` 批量取消。
    transfers: Arc<Mutex<HashMap<TransferId, TransferEntry>>>,
    next_transfer: AtomicU64,
    /// MCP 授权变化通知器，与 `Core` 共用（见 `crate::mcp::PolicyNotifier`）。
    mcp_policy: crate::mcp::PolicyNotifier,
}

/// 会话管理器的打开错误 → core 错误。
pub(crate) fn open_error(e: OpenError) -> CoreError {
    match e {
        OpenError::NoSuchSession(id) => CoreError::NoSuchSession(id),
        OpenError::Spawn { command, detail } => {
            tracing::warn!(%command, %detail, "failed to start local shell");
            CoreError::User(Message::SessionSpawnFailed {
                command,
                reason: Reason::Other { detail },
            })
        }
        OpenError::ShuttingDown => CoreError::Internal {
            detail: "core is shutting down".to_owned(),
        },
    }
}

/// [`RealCore::profile_launch`] 的结果。
pub(crate) enum ProfileLaunch {
    /// 本地 Profile 的 shell 配置。
    Local(kanso_types::profile::LocalProfile),
    /// SSH 或纯 SFTP Profile 的启动参数（纯 SFTP 已标记 `sftp_only`）。
    Ssh(SshLaunch),
}

/// 进行中的一次 SFTP 传输。
struct TransferEntry {
    session: SessionId,
    cancel: CancelToken,
}

impl RealCore {
    /// 创建真实 core：建数据目录、打开并迁移数据库、启动运行时、初始化会话管理器。
    ///
    /// 数据库打不开（损坏、版本过新、目录不可写）时返回 `Internal`，不降级成无存储运行：
    /// 没有存储就没有已保存的会话，静默降级会让用户以为数据丢了，之后的保存也会悄悄失败；
    /// 让宿主弹出明确的错误更安全。
    ///
    /// `base_env` 见 [`crate::Core::new_with_base_env`]。
    pub(crate) fn new(
        config: CoreConfig,
        platform: Platform,
        base_env: Option<EnvSnapshot>,
        mcp_policy: crate::mcp::PolicyNotifier,
    ) -> Result<RealCore> {
        std::fs::create_dir_all(&config.data_dir).map_err(|e| CoreError::Internal {
            detail: format!("cannot create data dir {}: {e}", config.data_dir.display()),
        })?;

        let store =
            Store::open(&config.data_dir.join(DB_FILE_NAME)).map_err(|e| CoreError::Internal {
                detail: format!("cannot open database: {e}"),
            })?;
        let secrets = Arc::clone(&platform.secrets);
        let events = Arc::clone(&platform.events);
        let platform_host_keys = platform.host_keys.clone();
        let app_version = config.app_version.clone();
        let env = base_env.clone().unwrap_or_else(EnvSnapshot::from_current);
        let ssh_env = SshEnv {
            agent_socket: env.get_non_empty("SSH_AUTH_SOCK").map(PathBuf::from),
            home_dir: env.get_non_empty("HOME").map(PathBuf::from),
        };

        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(4);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            // SSH 需要 IO 与时间驱动（TCP、keepalive、提问超时）；不启用时 `TcpStream::connect` 会 panic。
            .enable_all()
            .thread_name("kanso-rt")
            .build()
            .map_err(|e| CoreError::Internal {
                detail: format!("cannot start tokio runtime: {e}"),
            })?;
        let handle = runtime.handle().clone();

        let enabled = store
            .get_setting("completion")
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .is_some_and(|value| value["enabled"] == true);
        let store = Arc::new(Mutex::new(store));
        let history = Arc::new(HistoryRecorder {
            store: Arc::clone(&store),
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            generation: AtomicU64::new(0),
        });

        let prompts = PromptTable::new(Arc::clone(&events));
        let (closed_tx, closed_rx) = tokio::sync::mpsc::unbounded_channel();
        let sessions = SessionManager::new(
            ManagerConfig {
                data_dir: config.data_dir,
                app_version: config.app_version,
                preferred_language: config.preferred_language,
                region: config.region,
                default_shell: config.default_shell,
                shell_integration: config.shell_integration,
                base_env,
            },
            Arc::new(HostAdapter {
                platform,
                prompts: Arc::clone(&prompts),
                runtime: handle.clone(),
                history: Arc::clone(&history),
                closed_tx,
            }),
        )
        .with_runtime(handle.clone());
        let sessions = Arc::new(sessions);
        let profile_sessions = Arc::new(Mutex::new(HashMap::new()));
        let headless = Arc::new(Mutex::new(HashMap::new()));
        let transfers = Arc::new(Mutex::new(HashMap::new()));
        connect::spawn_release_task(
            &handle,
            closed_rx,
            connect::ReleaseCtx {
                sessions: Arc::downgrade(&sessions),
                profile_sessions: Arc::clone(&profile_sessions),
                headless: Arc::clone(&headless),
                transfers: Arc::clone(&transfers),
            },
        );
        Ok(RealCore {
            runtime: Mutex::new(Some(runtime)),
            handle,
            sessions,
            store,
            history,
            secrets,
            app_version,
            prompts,
            events,
            ssh_env,
            platform_host_keys,
            builtin_host_keys: Mutex::new(None),
            profile_sessions,
            custom_titles: Mutex::new(HashMap::new()),
            headless,
            exec_slots: Mutex::new(HashMap::new()),
            ssh_config_env: SshConfigEnv::from_snapshot(&env),
            transfers,
            next_transfer: AtomicU64::new(1),
            mcp_policy,
        })
    }

    /// 取会话；不存在（已关闭或 id 无效）返回 `NoSuchSession`。
    fn session(&self, id: SessionId) -> Result<Session> {
        self.sessions
            .session(id)
            .ok_or(CoreError::NoSuchSession(id))
    }

    /// 取有终端的会话：纯 SFTP 会话没有终端，返回 `NotSupported`。所有终端类操作都经由它取会话，
    /// 这样 SFTP 会话上的输入、选区、搜索、尺寸变化不会碰到它内部（未使用的）终端状态。
    fn terminal_session(&self, id: SessionId) -> Result<Session> {
        let s = self.session(id)?;
        if s.kind() == SessionKind::Sftp {
            return Err(CoreError::NotSupported);
        }
        Ok(s)
    }

    /// 关闭全部会话（最多等 [`SHUTDOWN_TIMEOUT`]）并停止运行时。可重复调用。
    pub(crate) fn shutdown(&self) {
        // 先取消传输：会话收尾会放手连接，传输任务需要在连接还在时清理残缺文件。
        self.cancel_transfers(None);
        self.sessions.shutdown(SHUTDOWN_TIMEOUT);
        // 会话任务被 abort 时各自的 guard 会清理；这里兜底，保证关闭后提问表为空。
        self.prompts.cancel(None);
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(rt) = runtime {
            // 不能直接 drop：在异步上下文里 drop 运行时会 panic；带超时的关闭则两种上下文都安全。
            rt.shutdown_timeout(Duration::from_secs(1));
        }
    }

    // ---- 数据（T8） ----

    /// 取数据库锁。持锁期间只做短小的 SQL；锁中毒时继续使用：事务要么提交要么回滚，不会留下半更新状态。
    fn db(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 全部 Profile。读失败时记日志并返回空（契约：不会失败）。
    pub(crate) fn profiles(&self) -> Vec<Profile> {
        self.db().profiles().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read profiles");
            Vec::new()
        })
    }

    pub(crate) fn save_profile(&self, profile: Profile) -> Result<()> {
        let mut db = self.db();
        // 校验与写入在同一把锁内：校验用的"现有 Profile"不会在中途被别的线程改掉。
        let all = db.profiles().map_err(store_err)?;
        validate_profile(&profile, &all).map_err(CoreError::User)?;
        if let Some(g) = profile.group
            && db.group(g).map_err(store_err)?.is_none()
        {
            return Err(CoreError::NoSuchGroup(g));
        }
        db.upsert_profile(&profile).map_err(store_err)
    }

    /// 删除 Profile：先删数据库，再删凭据。
    ///
    /// 顺序的理由：数据库是用户可见的事实来源，先删它能保证"配置消失"一定生效；
    /// 凭据存储可能弹授权框、被用户拒绝或不可用，失败时返回 `SecretStoreFailed`，
    /// 此时数据库删除**已经生效**（调用方重试会得到 `NoSuchProfile`）。遗留的凭据是孤儿条目：
    /// Profile id 是随机 UUID，不会被复用，所以孤儿凭据不会被误用，只是占一点空间。
    /// 反过来先删凭据再删库，失败后会留下"配置还在、密码没了"的半残状态，更糟。
    pub(crate) async fn delete_profile(&self, id: ProfileId) -> Result<()> {
        let store = Arc::clone(&self.store);
        let secrets = Arc::clone(&self.secrets);
        let mcp_policy = self.mcp_policy.clone();
        self.handle
            .spawn_blocking(move || {
                let deleted = store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .delete_profile(id)
                    .map_err(store_err)?;
                if !deleted {
                    return Err(CoreError::NoSuchProfile(id));
                }
                // 授权已随删除提交，立即通知；放在闭包里，调用方丢弃 future 也不会漏掉。
                mcp_policy.bump();
                secrets.delete_all_for(id).map_err(secret_err)
            })
            .await
            .map_err(join_err)?
    }

    pub(crate) fn groups(&self) -> Vec<Group> {
        self.db().groups().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read groups");
            Vec::new()
        })
    }

    pub(crate) fn save_group(&self, group: Group) -> Result<()> {
        self.db().upsert_group(&group).map_err(store_err)
    }

    pub(crate) fn group_delete_impact(&self, id: GroupId) -> Result<DeleteImpact> {
        let impact = self
            .db()
            .delete_impact(id)
            .map_err(store_err)?
            .ok_or(CoreError::NoSuchGroup(id))?;
        Ok(DeleteImpact {
            profiles: impact.profiles,
            snippets: impact.snippets,
        })
    }

    /// 删除分组：先删数据库，再逐个删组内 Profile 的凭据。顺序理由同 [`RealCore::delete_profile`]。
    /// 某个 Profile 的凭据删除失败时仍继续删其余的，最后返回第一个错误：
    /// 数据库里这些 Profile 已经没了，尽量多清理比半途停下更好。
    pub(crate) async fn delete_group(&self, id: GroupId) -> Result<()> {
        let store = Arc::clone(&self.store);
        let secrets = Arc::clone(&self.secrets);
        let mcp_policy = self.mcp_policy.clone();
        self.handle
            .spawn_blocking(move || {
                let profiles = store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .delete_group(id)
                    .map_err(store_err)?
                    .ok_or(CoreError::NoSuchGroup(id))?;
                // 同 delete_profile。
                mcp_policy.bump();
                let mut first_err = None;
                for p in profiles {
                    if let Err(e) = secrets.delete_all_for(p) {
                        first_err.get_or_insert(secret_err(e));
                    }
                }
                first_err.map_or(Ok(()), Err)
            })
            .await
            .map_err(join_err)?
    }

    pub(crate) fn snippets(&self) -> Vec<Snippet> {
        self.db().snippets().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to read snippets");
            Vec::new()
        })
    }

    /// 保存 Sender 命令。超过 1000 条上限时返回 `Internal`：
    /// 现有消息清单里没有"命令数已达上限"的 key（契约变更，留给主会话决定），UI 目前只能显示通用失败。
    pub(crate) fn save_snippet(&self, snippet: Snippet) -> Result<()> {
        self.db().upsert_snippet(&snippet).map_err(|e| match e {
            StoreError::SnippetLimitReached => CoreError::User(Message::SnippetLimitReached {
                limit: kanso_store::MAX_SNIPPETS as i64,
            }),
            e => store_err(e),
        })
    }

    pub(crate) fn delete_snippet(&self, id: SnippetId) -> Result<()> {
        self.db().delete_snippet(id).map_err(store_err)
    }

    pub(crate) fn visible_snippets(
        &self,
        profile: Option<ProfileId>,
        kind: Option<SessionKind>,
    ) -> Result<Vec<Snippet>> {
        let db = self.db();
        Ok(kanso_store::visible_snippets(
            db.snippets().map_err(store_err)?,
            &db.profiles().map_err(store_err)?,
            profile,
            kind,
        ))
    }

    pub(crate) fn sender_ready(&self, id: SessionId) -> std::result::Result<bool, Message> {
        self.session(id)
            .map_err(|_| Message::SenderTerminalClosed {})?
            .sender_ready()
    }

    pub(crate) fn sender_line(
        &self,
        id: SessionId,
        text: &str,
        ending: &str,
    ) -> std::result::Result<bool, Message> {
        self.session(id)
            .map_err(|_| Message::SenderTerminalClosed {})?
            .sender_line(text, ending)
    }

    pub(crate) async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> Result<()> {
        let store = Arc::clone(&self.store);
        let secrets = Arc::clone(&self.secrets);
        self.handle
            .spawn_blocking(move || {
                // 先确认 Profile 存在，再碰凭据存储（它可能弹授权框）；查库的锁在写凭据前释放。
                let exists = store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .profile(profile)
                    .map_err(store_err)?
                    .is_some();
                if !exists {
                    return Err(CoreError::NoSuchProfile(profile));
                }
                secrets
                    .set(&SecretRef { profile, kind }, &secret)
                    .map_err(secret_err)
            })
            .await
            .map_err(join_err)?
    }

    pub(crate) fn clear_history(&self) -> Result<()> {
        let mut store = self.db();
        self.history.generation.fetch_add(1, Ordering::SeqCst);
        store.clear_history().map_err(store_err)
    }

    pub(crate) fn completion_state(&self, session: SessionId) -> Option<crate::CompletionState> {
        if !self.history.enabled.load(Ordering::SeqCst) {
            return None;
        }
        let session = self.session(session).ok()?;
        let (input, start_column) = session.completion_input()?;
        let report = session.reported_directory();
        Some(crate::CompletionState {
            input,
            start_column,
            directory: report.as_ref().map(|report| report.path.clone()),
            directory_host: report.and_then(|report| report.host),
        })
    }

    pub(crate) fn complete(
        &self,
        session: SessionId,
        input: &str,
        sources: crate::CompletionSources,
    ) -> crate::CompletionSuggestions {
        let Some(state) = self.completion_state(session) else {
            return crate::CompletionSuggestions::default();
        };
        if state.input != input {
            return crate::CompletionSuggestions::default();
        }
        let Some(context) = kanso_complete::parse(input) else {
            return crate::CompletionSuggestions::default();
        };
        let mut suggestions = self.completion_plan(session, input, &sources);
        let Ok(session) = self.session(session) else {
            return crate::CompletionSuggestions::default();
        };
        let host = match &session {
            Session::Local(_) => "local".to_owned(),
            Session::Ssh(ssh) => ssh.endpoint_host().to_owned(),
        };
        let history = self
            .db()
            .completion_history(&host)
            .unwrap_or_default()
            .into_iter()
            .map(|(entry, same_host)| kanso_complete::History {
                command: entry.command,
                last_used_at: entry.last_used_at,
                same_host,
            })
            .collect::<Vec<_>>();
        let path = sources
            .allow_paths
            .then(|| context.path_request(sources.cwd.as_deref(), sources.home.as_deref()))
            .flatten();
        let local = matches!(session, Session::Local(_));
        let names = if local {
            kanso_complete::KubeNames::from_configs(&sources.configs)
        } else {
            kanso_complete::KubeNames::default()
        };
        suggestions.candidates =
            kanso_complete::complete(&context, &history, path.as_ref(), &sources.entries, &names);
        suggestions
    }

    pub(crate) fn completion_plan(
        &self,
        session: SessionId,
        input: &str,
        sources: &crate::CompletionSources,
    ) -> crate::CompletionSuggestions {
        let Some(context) = kanso_complete::parse(input) else {
            return crate::CompletionSuggestions::default();
        };
        let Ok(session) = self.session(session) else {
            return crate::CompletionSuggestions::default();
        };
        let local = matches!(session, Session::Local(_));
        let path = sources
            .allow_paths
            .then(|| context.path_request(sources.cwd.as_deref(), sources.home.as_deref()))
            .flatten();
        let plan = sources
            .allow_paths
            .then(|| context.path_request(Some("/"), Some("/")))
            .flatten();
        crate::CompletionSuggestions {
            candidates: Vec::new(),
            directory: path.map(|path| path.directory),
            needs_kube_names: local && context.name_kind().is_some(),
            needs_home: sources.home.is_none()
                && plan.as_ref().is_some_and(|path| path.expand_home),
            needs_cwd: sources.cwd.is_none()
                && plan.is_some()
                && context.path_request(None, Some("/")).is_none(),
        }
    }

    pub(crate) fn setting(&self, key: &str) -> Option<String> {
        self.db().get_setting(key).unwrap_or_else(|e| {
            tracing::warn!(error = %e, key, "failed to read setting");
            None
        })
    }

    pub(crate) fn set_setting(&self, key: &str, json: String) -> Result<()> {
        if key == "completion"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&json)
        {
            self.history.enable(value["enabled"] == true);
        }
        self.db().set_setting(key, &json).map_err(store_err)
    }

    pub(crate) fn reset_settings(&self) -> Result<()> {
        self.db().reset_settings().map_err(store_err)?;
        self.history.enable(false);
        Ok(())
    }

    pub(crate) fn set_terminal_options(&self, options: TerminalOptions) {
        self.sessions.set_terminal_options(options);
    }

    // ---- 会话 ----

    /// 打开会话（`DefaultLocalShell` / `Profile(id)` / `Duplicate(会话)`）。
    ///
    /// `Profile(id)`：本地 Profile 按它的 shell 命令行、起始目录、编码、locale 打开；
    /// SSH Profile 立即返回会话 id（`Connecting`），连接在后台进行，失败经 `Closed(Error(..))` 通知；
    /// 带跳板机的 SSH Profile 按引用解析出整条链（最多 8 跳），每一跳用自己的认证配置与凭据；
    /// 成环、引用缺失、引用的不是 SSH Profile、超过 8 跳按 `validate_profile` 的规则报
    /// `User(JumpCycle / JumpMissing / JumpNotSsh / JumpTooLong)`，**不会悄悄改成直连**。
    /// 带 ProxyCommand 的 SSH Profile 用命令作为传输；MAS 渠道（沙盒内无法可靠启动外部命令）返回 `NotSupported`。
    /// 纯 SFTP Profile 与 SSH Profile 走同一条连接路径（跳板、认证、提问），只是不开 shell（**T7b**）；
    /// 主机或用户为空按 `validate_profile`
    /// 的规则报 `User(ProfileHostEmpty / ProfileUserEmpty)`，不发起任何网络请求；
    /// MAS 渠道且宿主没有注入主机密钥存储时返回 `NotSupported`；Profile 不存在返回 `NoSuchProfile`。
    /// `Duplicate(SSH 会话)`：在同一个已认证连接上再开一个 shell，不重新认证；连接已断时
    /// 新会话以 `SshSharedConnectionClosed` 关闭。
    /// shell 启动失败（程序不存在等）直接返回 `User(SessionSpawnFailed)`，此时没有发出任何事件：
    /// 本地启动是同步的，立刻告诉调用方比"先返回 id 再发 `Closed(Error)`"更简单，UI 也不必为一个
    /// 从未显示过的会话建标签。
    pub(crate) fn open_session(&self, target: OpenTarget, size: GridSize) -> Result<SessionId> {
        let mut opened_profile = None;
        let request = match target {
            OpenTarget::Profile(id) => {
                let (profile, launch) = self.profile_launch(id, None)?;
                opened_profile = Some((id, profile.name.clone()));
                match launch {
                    ProfileLaunch::Local(local) => OpenRequest::Local(local),
                    ProfileLaunch::Ssh(launch) => {
                        // 这个 Profile 有 MCP 自建的连接时与它共享，不再新建连接、不再认证
                        // （spec/mcp.md §2.4）；用户自己已有的标签不触发共享，那是两条独立连接。
                        match self.find_self_built(id) {
                            Some(connection) => OpenRequest::SshShared { launch, connection },
                            None => OpenRequest::Ssh(launch),
                        }
                    }
                }
            }
            OpenTarget::DefaultLocalShell => OpenRequest::DefaultLocalShell,
            OpenTarget::LocalShell(profile) => OpenRequest::Local(profile),
            OpenTarget::Duplicate(src) => {
                // 复制由 Profile 打开的会话时，副本也属于同一个 Profile。
                opened_profile = self.profile_of(src);
                OpenRequest::Duplicate(src)
            }
        };
        let color_prompt = !cfg!(windows)
            || matches!(
                &request,
                OpenRequest::Ssh(_) | OpenRequest::SshShared { .. }
            )
            || self.setting(crate::POWERSHELL_COLOR_PROMPT).as_deref() != Some("false");
        let id = self
            .sessions
            .open_with_powershell_prompt(request, size, color_prompt)
            .map_err(open_error)?;
        if let Some(entry) = opened_profile {
            self.profile_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id, entry);
        }
        Ok(id)
    }

    /// 查出 Profile 并转成启动参数：本地 Profile 给出 shell 配置，SSH / 纯 SFTP 给出连接器已装配好的
    /// [`SshLaunch`]（纯 SFTP 已调用 `sftp_only`）。不发起网络请求。
    ///
    /// `non_interactive` 为 `Some` 时连接器按非交互方式工作（MCP，见 `CoreSshConnector::non_interactive`）。
    /// 校验失败、MAS 不支持等错误与 `open_session` 的契约一致。
    pub(crate) fn profile_launch(
        &self,
        id: ProfileId,
        non_interactive: Option<Arc<NeedSlot>>,
    ) -> Result<(Profile, ProfileLaunch)> {
        // 查库后立刻释放锁，再启动 shell（fork 不该占着数据库锁）。
        let profile = self
            .db()
            .profile(id)
            .map_err(store_err)?
            .ok_or(CoreError::NoSuchProfile(id))?;
        let launch = match &profile.kind {
            ProfileKind::Local(local) => ProfileLaunch::Local(local.clone()),
            ProfileKind::Ssh(ssh) => {
                // 链上每一跳都要重新校验：Profile 可能是导入的，或其引用在保存之后才失效；
                // 校验失败必须报错，不能退化成直连（store-import.md §3、ssh.md §5）。
                let all = self.db().profiles().map_err(store_err)?;
                validate_profile(&profile, &all).map_err(CoreError::User)?;
                ProfileLaunch::Ssh(self.ssh_launch(
                    &profile,
                    &ssh.target,
                    &ssh.encoding,
                    ssh.locale.clone(),
                    &all,
                    non_interactive,
                )?)
            }
            ProfileKind::Sftp(target) => {
                // 与 SSH Profile 完全相同的校验与连接器，只是会话不开 shell（ssh.md §7）。
                let all = self.db().profiles().map_err(store_err)?;
                validate_profile(&profile, &all).map_err(CoreError::User)?;
                ProfileLaunch::Ssh(
                    self.ssh_launch(
                        &profile,
                        target,
                        &EncodingLabel::default(),
                        None,
                        &all,
                        non_interactive,
                    )?
                    .sftp_only(),
                )
            }
        };
        Ok((profile, launch))
    }

    /// 内置 known_hosts 的存储与吊销查询；宿主注入了存储时用宿主的（没有吊销查询）。
    fn host_keys(&self) -> Result<HostKeys> {
        if let Some(store) = &self.platform_host_keys {
            return Ok(HostKeys {
                store: Arc::clone(store),
                revoked: None,
            });
        }
        // MAS 版的 known_hosts 在应用容器内、由宿主或 core 用数据库实现；那一份尚未接入，
        // 不能退回去读写 `~/.ssh/known_hosts`（沙盒内读不到，也违反 MAS 的规格）。
        if cfg!(feature = "mas") {
            return Err(CoreError::NotSupported);
        }
        let mut slot = self
            .builtin_host_keys
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let home = self
                .ssh_env
                .home_dir
                .as_ref()
                .ok_or_else(|| CoreError::Internal {
                    detail: "cannot locate known_hosts: HOME is not set".to_owned(),
                })?;
            *slot = Some(Arc::new(KnownHostsFile::new(
                home.join(".ssh").join("known_hosts"),
            )));
        }
        let file = slot.as_ref().map(Arc::clone).expect("刚刚设置过");
        Ok(HostKeys {
            store: Arc::clone(&file) as Arc<dyn HostKeyStore>,
            revoked: Some(file as Arc<dyn RevokedKeys>),
        })
    }

    /// 由 SSH Profile 构造会话层的启动参数（不做任何 IO；凭据在后台连接时才读）。
    ///
    /// 调用方已经对 `profile` 与 `all` 跑过 `validate_profile`；这里仍按"缺失 / 非 SSH / 过长"防御性地
    /// 返回对应的 `Jump*`，而不是 panic 或退化成直连。
    fn ssh_launch(
        &self,
        profile: &Profile,
        target: &SshTarget,
        encoding: &EncodingLabel,
        locale: Option<Locale>,
        all: &[Profile],
        non_interactive: Option<Arc<NeedSlot>>,
    ) -> Result<SshLaunch> {
        // 沿 jump 引用向外走，得到从内到外的跳板 Profile，再翻转成从外到内。
        let mut jumps: Vec<(&Profile, &kanso_types::profile::SshTarget)> = Vec::new();
        let mut next = target.jump;
        while let Some(jump_id) = next {
            if jumps.len() >= MAX_JUMP_HOPS {
                return Err(CoreError::User(Message::JumpTooLong {
                    max_hops: MAX_JUMP_HOPS as i64,
                }));
            }
            let jump = all.iter().find(|p| p.id == jump_id).ok_or_else(|| {
                CoreError::User(Message::JumpMissing {
                    profile: jump_id.to_string(),
                })
            })?;
            let ProfileKind::Ssh(jump_ssh) = &jump.kind else {
                return Err(CoreError::User(Message::JumpNotSsh {
                    profile: jump.name.clone(),
                }));
            };
            // 跳板 Profile 自己的主机 / 用户为空、跳板与 ProxyCommand 冲突等同样要报错。
            validate_profile(jump, all).map_err(CoreError::User)?;
            jumps.push((jump, &jump_ssh.target));
            next = jump_ssh.target.jump;
        }
        jumps.reverse();

        let mut chain: Vec<ConnectSpec> = jumps
            .iter()
            .map(|(p, t)| hop_spec(p.id, &p.name, t))
            .collect();
        chain.push(hop_spec(profile.id, &profile.name, target));

        // ProxyCommand 只能挂在最外层：带跳板机的 Profile 不能同时有 ProxyCommand（validate 已保证），
        // 所以链上只可能是第一跳有。
        let first_target = jumps.first().map_or(target, |(_, t)| t);
        let proxy_command = first_target
            .proxy_command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        // MAS 沙盒内无法可靠地启动外部命令（ssh.md §5），不能悄悄改成直连。
        if proxy_command.is_some() && cfg!(feature = "mas") {
            return Err(CoreError::NotSupported);
        }

        let endpoint = Endpoint {
            via: chain[..chain.len() - 1]
                .iter()
                .map(|s| s.name.clone())
                .collect(),
            ..chain[chain.len() - 1].endpoint.clone()
        };
        let connector = CoreSshConnector {
            prompts: Arc::clone(&self.prompts),
            events: Arc::clone(&self.events),
            secrets: Arc::clone(&self.secrets),
            host_keys: self.host_keys()?,
            env: self.ssh_env.clone(),
            chain,
            proxy_command,
            non_interactive,
        };
        let launch = SshLaunch::new(
            profile.name.clone(),
            endpoint,
            encoding,
            locale,
            Arc::new(connector),
        );
        // 空闲保活由 core 解析转义后传给会话层；SFTP 会话虽带上参数，但没有 shell channel，
        // 会话层会在 sftp_only 分支提前返回，不会启动定时器。
        Ok(if target.keepalive.idle_enabled {
            launch.idle_keepalive(IdleKeepalive {
                interval: Duration::from_secs(target.keepalive.idle_interval_secs as u64),
                bytes: target.keepalive.decoded_idle_string(),
            })
        } else {
            launch
        })
    }

    /// 会话对应的 Profile（id 与名字）；不是由 Profile 打开的返回 `None`。
    fn profile_of(&self, session: SessionId) -> Option<(ProfileId, String)> {
        self.profile_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session)
            .cloned()
    }

    pub(crate) fn close_session(&self, session: SessionId) {
        // 关闭前先记下它用的连接：关闭后会话已从表里移除，而借用连接的释放要知道是哪条连接
        // （无界面会话的关闭不触发释放）。
        let visible_connection = self
            .sessions
            .session(session)
            .filter(|s| !s.is_headless())
            .and_then(|s| s.connection_id());
        self.sessions.close(session);
        // 关闭会话取消它的全部传输（ssh.md §9）；任务自己会发 `TransferFinished(Cancelled)`。
        self.cancel_transfers(Some(session));
        // 会话任务被 abort 时提问 future 的 guard 会清理；这里同步取消，不依赖任务何时被调度。
        self.prompts.cancel(Some(session));
        self.profile_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session);
        self.headless
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session);
        self.custom_titles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session);
        // 用户关闭标签后，若这是该连接上最后一个可见会话，立即释放借用它的无界面会话。
        // 同步执行（只是登记关闭，不阻塞）：返回之后调用方再查一定看不到已释放的借用会话。
        if let Some(connection_id) = visible_connection {
            self.release_borrowed(connection_id);
        }
    }

    pub(crate) fn sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .sessions()
            .into_iter()
            .map(|s| self.session_info_from(s))
            .collect()
    }

    /// 会话层摘要 + Profile 关联 → `SessionInfo`。由 Profile 打开的会话显示 Profile 名。
    fn session_info_from(&self, s: kanso_session::SessionSummary) -> SessionInfo {
        let (profile, name) = match self.profile_of(s.id) {
            Some((id, name)) => (Some(id), name),
            None => (None, s.name),
        };
        let custom_title = self
            .custom_titles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&s.id)
            .cloned();
        SessionInfo {
            id: s.id,
            profile,
            kind: s.kind,
            name,
            custom_title,
            state: s.state,
        }
    }

    pub(crate) fn session_info(&self, session: SessionId) -> Option<SessionInfo> {
        self.sessions
            .summary(session)
            .map(|s| self.session_info_from(s))
    }

    pub(crate) fn set_session_title(&self, session: SessionId, title: Option<String>) {
        if self.sessions.summary(session).is_none() {
            return;
        }
        let mut titles = self.custom_titles.lock().unwrap_or_else(|e| e.into_inner());
        match title {
            Some(name) => {
                titles.insert(session, name);
            }
            None => {
                titles.remove(&session);
            }
        }
    }

    pub(crate) fn set_visible(&self, session: SessionId, visible: bool) {
        if let Some(s) = self.sessions.session(session) {
            s.set_visible(visible);
        }
    }

    pub(crate) fn resize(
        &self,
        session: SessionId,
        size: GridSize,
        cell_width_px: u16,
        cell_height_px: u16,
    ) -> Result<()> {
        self.terminal_session(session)?
            .resize(size, cell_width_px, cell_height_px);
        Ok(())
    }

    // ---- 输入（会话已结束时丢弃并返回 Ok，由会话层保证） ----

    pub(crate) fn key(&self, session: SessionId, event: KeyEvent) -> Result<()> {
        self.terminal_session(session)?.key(&event);
        Ok(())
    }

    pub(crate) fn text_input(&self, session: SessionId, text: &str) -> Result<()> {
        self.terminal_session(session)?.text_input(text);
        Ok(())
    }

    pub(crate) fn paste(&self, session: SessionId, content: PasteContent) -> Result<()> {
        self.terminal_session(session)?.paste(&content);
        Ok(())
    }

    pub(crate) fn mouse(&self, session: SessionId, event: MouseEvent) -> Result<()> {
        self.terminal_session(session)?.mouse(&event);
        Ok(())
    }

    pub(crate) fn scroll(
        &self,
        session: SessionId,
        delta_lines: f32,
        at: ViewportPoint,
        mods: Modifiers,
    ) -> Result<()> {
        self.terminal_session(session)?
            .scroll(delta_lines, at, mods);
        Ok(())
    }

    pub(crate) fn focus(&self, session: SessionId, focused: bool) -> Result<()> {
        self.terminal_session(session)?.focus(focused);
        Ok(())
    }

    // ---- 选区与缓冲区 ----

    pub(crate) fn has_selection(&self, session: SessionId) -> bool {
        self.sessions
            .session(session)
            .is_some_and(|s| s.has_selection())
    }

    pub(crate) fn command_block_text(
        &self,
        session: SessionId,
        id: u64,
        part: u8,
    ) -> Option<String> {
        self.sessions.session(session)?.command_block_text(id, part)
    }

    pub(crate) fn jump_command_block(
        &self,
        session: SessionId,
        forward: bool,
    ) -> Result<Option<u64>> {
        Ok(self.terminal_session(session)?.jump_command_block(forward))
    }

    pub(crate) fn selection_text(&self, session: SessionId) -> Option<String> {
        self.sessions.session(session)?.selection_text()
    }

    pub(crate) fn select_word_at(&self, session: SessionId, at: ViewportPoint) -> Result<()> {
        self.terminal_session(session)?.select_word_at(at);
        Ok(())
    }

    pub(crate) fn select_all(&self, session: SessionId) -> Result<()> {
        self.terminal_session(session)?.select_all();
        Ok(())
    }

    pub(crate) fn clear_selection(&self, session: SessionId) -> Result<()> {
        self.terminal_session(session)?.clear_selection();
        Ok(())
    }

    pub(crate) fn clear_buffer(&self, session: SessionId) -> Result<()> {
        self.terminal_session(session)?.clear_buffer();
        Ok(())
    }

    pub(crate) fn hyperlink_url(&self, session: SessionId, id: u32) -> Option<String> {
        self.sessions.session(session)?.hyperlink_url(id)
    }

    // ---- 搜索 ----

    /// 终端同时返回匹配数量和正则是否合法。
    pub(crate) fn search(&self, session: SessionId, query: SearchQuery) -> Result<SearchStatus> {
        let status = self
            .terminal_session(session)?
            .search(&kanso_session::SearchQuery {
                text: query.text,
                case_sensitive: query.case_sensitive,
                regex: query.regex,
            });
        Ok(SearchStatus {
            invalid_regex: status.invalid_regex,
            total: status.total,
            current: status.current,
        })
    }

    pub(crate) fn search_next(&self, session: SessionId, backwards: bool) -> Result<SearchStatus> {
        let status = self.terminal_session(session)?.search_next(backwards);
        Ok(SearchStatus {
            invalid_regex: status.invalid_regex,
            total: status.total,
            current: status.current,
        })
    }

    pub(crate) fn search_status(&self, session: SessionId) -> Result<SearchStatus> {
        let status = self.terminal_session(session)?.search_status();
        Ok(SearchStatus {
            invalid_regex: status.invalid_regex,
            total: status.total,
            current: status.current,
        })
    }

    pub(crate) fn search_close(&self, session: SessionId) {
        if let Some(s) = self.sessions.session(session) {
            s.search_close();
        }
    }

    // ---- 显示与帧 ----

    /// 只存储，渲染侧读取方式在 T10 定。
    pub(crate) fn set_timestamp_mode(&self, session: Option<SessionId>, mode: TimestampMode) {
        match session {
            None => self.sessions.set_timestamp_mode(mode),
            Some(id) => {
                if let Some(s) = self.sessions.session(id) {
                    s.set_timestamp_mode(mode);
                }
            }
        }
    }

    pub(crate) fn take_frame(
        &self,
        session: SessionId,
        reuse: &mut Option<Frame>,
    ) -> Option<Frame> {
        let s = self.sessions.session(session)?;
        if s.kind() == SessionKind::Sftp {
            return None;
        }
        s.take_frame(reuse)
    }

    /// 回答一个 `Prompt` 事件；未知或已过期的 id 静默忽略，类型不匹配按 `Cancel`（见 `PromptTable::respond`）。
    pub(crate) fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
        self.prompts.respond(prompt, response);
    }

    /// 优先用终端记录的 OSC 7 / 1337 / 9;9；没有上报时，本地会话查前台进程的 cwd（local-shell.md §6），
    /// SSH 会话在同一连接上另开 exec 通道查询服务器（ssh.md §10，5 秒超时）。
    /// 都拿不到返回 `User(CwdUnavailable)`；远端查询失败返回对应的 `User(Cwd*)`。
    ///
    /// 系统查询（`proc_pidinfo`）放进 tokio 的阻塞线程池，不占用调用方（Swift 协作线程池）的线程。
    pub(crate) async fn working_directory(&self, session: SessionId) -> Result<String> {
        let s = self.terminal_session(session)?;
        let for_blocking = s.clone();
        let found = match self
            .handle
            .spawn_blocking(move || for_blocking.working_directory())
            .await
        {
            Ok(v) => v,
            // 运行时已关闭（`shutdown` 之后）：直接在当前线程算，结果一样。
            Err(_) => s.working_directory(),
        };
        if let Some(path) = found {
            return Ok(path);
        }
        if let Session::Ssh(ssh) = &s {
            // 必须放到 core 自己的运行时上跑：本方法的 future 由宿主的执行器（FFI 侧）轮询，
            // 那里没有 tokio 的计时器，而查询里的 5 秒超时需要它。
            let ssh = Arc::clone(ssh);
            let joined = self
                .handle
                .spawn(async move { ssh.query_remote_working_directory().await })
                .await;
            return match joined {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, "remote cwd query failed");
                    Err(CoreError::User(e.to_message()))
                }
                // 任务被取消（运行时已关闭）：当作查不到。
                Err(_) => Err(CoreError::User(Message::CwdUnavailable {})),
            };
        }
        Err(CoreError::User(Message::CwdUnavailable {}))
    }

    // ---- SFTP（T7b）----
    //
    // 每个 SSH / 纯 SFTP 会话一个 `SftpClient`（在会话里，首次使用时创建）。本组方法对本地会话返回
    // `NotSupported`，对不存在的会话返回 `NoSuchSession`，会话还没连上或已结束返回 `User(SshSharedConnectionClosed)`。
    //
    // 所有 async 方法都把实际工作 spawn 到 core 自己的运行时上再等结果：本方法的 future 由宿主的执行器
    // （FFI 侧）轮询，那里没有 tokio 的计时器，而引擎内部有超时 / 计时需求；沿用 `working_directory` 的做法。

    /// 取会话的 SFTP 客户端。
    fn sftp_client(&self, session: SessionId) -> Result<SftpClient> {
        match self.session(session)? {
            Session::Ssh(ssh) => ssh
                .sftp_client()
                .map_err(|e| CoreError::User(e.to_message())),
            Session::Local(_) => Err(CoreError::NotSupported),
        }
    }

    /// 在 core 的运行时上跑 `fut` 并等待结果（原因见上）。运行时已关闭时返回 `Internal`。
    async fn on_runtime<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
    ) -> Result<T> {
        self.handle.spawn(fut).await.map_err(join_err)
    }

    pub(crate) async fn sftp_home(&self, session: SessionId) -> Result<String> {
        let client = self.sftp_client(session)?;
        self.on_runtime(async move { client.home().await })
            .await?
            .map_err(sftp_err)
    }

    pub(crate) async fn sftp_stat(
        &self,
        session: SessionId,
        path: &str,
    ) -> Result<kanso_types::fs::FileStat> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        let attrs = self
            .on_runtime(async move { client.stat(&path).await })
            .await?
            .map_err(sftp_err)?;
        Ok(kanso_types::fs::FileStat {
            size: attrs.size,
            mtime_ms: attrs.mtime.map(|seconds| i64::from(seconds) * 1000),
        })
    }

    /// 返回的流在第一次 `next_batch` 时才打开目录（本方法同步，不能等远端往返）；
    /// 路径不存在等错误因此出现在第一次 `next_batch` 上。
    pub(crate) fn sftp_list(&self, session: SessionId, path: &str) -> Result<DirStream> {
        let client = self.sftp_client(session)?;
        Ok(DirStream::from_remote(RemoteDirList {
            handle: self.handle.clone(),
            client,
            path: path.to_owned(),
            state: ListState::Pending,
        }))
    }

    pub(crate) async fn sftp_mkdir(&self, session: SessionId, path: &str) -> Result<()> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move { client.mkdir(&path).await })
            .await?
            .map_err(sftp_err)
    }

    /// `is_dir` 为真时递归删除（符号链接只删链接本身，不跟进目标，见 [`remove_tree`]）。
    pub(crate) async fn sftp_remove(
        &self,
        session: SessionId,
        path: &str,
        is_dir: bool,
    ) -> Result<()> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move {
            if is_dir {
                remove_tree(&client, &path).await
            } else {
                client.remove_file(&path).await
            }
        })
        .await?
        .map_err(sftp_err)
    }

    pub(crate) async fn sftp_rename(&self, session: SessionId, from: &str, to: &str) -> Result<()> {
        let client = self.sftp_client(session)?;
        let (from, to) = (from.to_owned(), to.to_owned());
        self.on_runtime(async move { client.rename(&from, &to).await })
            .await?
            .map_err(sftp_err)
    }

    pub(crate) async fn sftp_create_file(&self, session: SessionId, path: &str) -> Result<()> {
        let client = self.sftp_client(session)?;
        let path = path.to_owned();
        self.on_runtime(async move { client.create_file(&path).await })
            .await?
            .map_err(sftp_err)
    }

    // ---- SFTP 传输 ----
    //
    // 事件时序（全部由同一个任务按序发出，所以天然保序）：
    // `TransferStarted` →（若干 `TransferProgress`，引擎已节流到每 50 ms 最多一次，开始与结束各强制一次）→
    // `TransferFinished`。单文件的 Started 已带总大小（先 STAT / metadata）；目录传输要先扫描，
    // Started 的 `total_bytes` 为 `None`，计划定稿后的 Progress 才带总量。
    // 远端 STAT 失败（路径不存在等）也先发 Started 再发 `Finished(Failed)`，保证 UI 看到完整的生命周期。

    pub(crate) fn download(
        &self,
        session: SessionId,
        remote_path: &str,
        local_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferId> {
        self.download_inner(session, remote_path, local_dir, conflict, None)
    }

    /// 发起下载并等它结束。参数错误立即返回；传输本身走与 `download` 完全相同的路径（同样的事件与传输表项）。
    ///
    /// 丢弃返回的 future 只是不再等结果，传输照常完成（spec/mcp.md §4.4）。
    pub(crate) async fn download_and_wait(
        &self,
        session: SessionId,
        remote_path: &str,
        local_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.download_inner(session, remote_path, local_dir, conflict, Some(tx))?;
        wait_outcome(rx).await
    }

    fn download_inner(
        &self,
        session: SessionId,
        remote_path: &str,
        local_dir: &str,
        conflict: ConflictPolicy,
        waiter: Option<OutcomeTx>,
    ) -> Result<TransferId> {
        let client = self.sftp_client(session)?;
        let name = path_name(remote_path)?;
        // 参数类错误在调用时直接返回：下载目标目录必须已存在（引擎要求父目录存在）。
        match std::fs::metadata(local_dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(CoreError::User(Message::XferLocalWriteFailed {
                    path: local_dir.to_owned(),
                    reason: Reason::Other {
                        detail: "not a directory".to_owned(),
                    },
                }));
            }
            Err(e) => {
                return Err(CoreError::User(Message::XferLocalWriteFailed {
                    path: local_dir.to_owned(),
                    reason: io_reason(&e),
                }));
            }
        }
        let dest = Path::new(local_dir).join(&name);
        let remote = remote_path.to_owned();
        Ok(self.spawn_transfer(
            session,
            TransferDirection::Download,
            name,
            waiter,
            move |em, cancel| async move {
                let stat = client.stat(&remote).await;
                let (is_dir, total) = match &stat {
                    Ok(a) if a.is_dir() => (true, None),
                    Ok(a) => (false, a.size),
                    Err(_) => (false, None),
                };
                em.started(total);
                if let Err(e) = stat {
                    return failed(&e.into());
                }
                let opts = em.options(conflict, cancel, total);
                let report = if is_dir {
                    client.download_dir(&remote, &dest, &opts).await
                } else {
                    client.download_file(&remote, &dest, &opts).await
                };
                outcome_of(report)
            },
        ))
    }

    pub(crate) fn upload(
        &self,
        session: SessionId,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferId> {
        self.upload_inner(session, local_path, remote_dir, conflict, None)
    }

    /// 上传版的 [`RealCore::download_and_wait`]，约定相同。
    pub(crate) async fn upload_and_wait(
        &self,
        session: SessionId,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.upload_inner(session, local_path, remote_dir, conflict, Some(tx))?;
        wait_outcome(rx).await
    }

    fn upload_inner(
        &self,
        session: SessionId,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
        waiter: Option<OutcomeTx>,
    ) -> Result<TransferId> {
        let client = self.sftp_client(session)?;
        let local = PathBuf::from(local_path);
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| is_safe_name(n))
            .map(str::to_owned)
            .ok_or_else(|| {
                CoreError::User(Message::XferUnsafeName {
                    name: local_path.to_owned(),
                })
            })?;
        // 参数类错误在调用时直接返回：本地源必须存在（跟随符号链接）。
        let meta = std::fs::metadata(&local).map_err(|e| {
            CoreError::User(Message::XferLocalReadFailed {
                path: local_path.to_owned(),
                reason: io_reason(&e),
            })
        })?;
        let (is_dir, total) = if meta.is_dir() {
            (true, None)
        } else {
            (false, Some(meta.len()))
        };
        let remote = remote_join(remote_dir, &name);
        Ok(self.spawn_transfer(
            session,
            TransferDirection::Upload,
            name,
            waiter,
            move |em, cancel| async move {
                em.started(total);
                let opts = em.options(conflict, cancel, total);
                let report = if is_dir {
                    client.upload_dir(&local, &remote, &opts).await
                } else {
                    client.upload_file(&local, &remote, &opts).await
                };
                outcome_of(report)
            },
        ))
    }

    /// 取消传输；未知或已结束的 id 忽略。结束事件由传输任务自己发（`Cancelled`，残缺文件由引擎清理）。
    pub(crate) fn cancel_transfer(&self, transfer: TransferId) {
        if let Some(t) = self.transfers_lock().get(&transfer) {
            t.cancel.cancel();
        }
        for summary in self.sessions.sessions() {
            if let Some(session) = self.sessions.session(summary.id) {
                session.cancel_terminal_transfer(Some(transfer));
            }
        }
    }

    fn transfers_lock(&self) -> MutexGuard<'_, HashMap<TransferId, TransferEntry>> {
        // 临界区只有 HashMap 的增删与取消标记，不会留下半更新状态。
        self.transfers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 取消某会话（`None` 为全部）的传输。
    fn cancel_transfers(&self, session: Option<SessionId>) {
        for t in self.transfers_lock().values() {
            if session.is_none_or(|s| s == t.session) {
                t.cancel.cancel();
            }
        }
    }

    /// 登记并在 core 运行时上启动一次传输，立即返回 id。`body` 负责发 `TransferStarted` 并返回结果。
    ///
    /// 任务分两层：外层等内层的 `JoinHandle`，内层 panic 时仍能发出 `Finished(Failed)`，
    /// 否则 UI 会永远看着一个"进行中"的传输。运行时被关闭（内层被丢弃）时同样走这条路径，无害。
    /// 先从表里移除、再发 `Finished`：之后到达的 `cancel_transfer` 属于"已结束的 id"，应被忽略。
    fn spawn_transfer<F, Fut>(
        &self,
        session: SessionId,
        direction: TransferDirection,
        name: String,
        waiter: Option<OutcomeTx>,
        body: F,
    ) -> TransferId
    where
        F: FnOnce(TransferEmitter, CancelToken) -> Fut + Send + 'static,
        Fut: Future<Output = TransferOutcome> + Send + 'static,
    {
        let id = TransferId::from_raw(self.next_transfer.fetch_add(1, Ordering::Relaxed));
        let cancel = CancelToken::new();
        self.transfers_lock().insert(
            id,
            TransferEntry {
                session,
                cancel: cancel.clone(),
            },
        );
        let em = TransferEmitter {
            events: Arc::clone(&self.events),
            id,
            session,
            direction,
            name,
        };
        let events = Arc::clone(&self.events);
        let table = Arc::clone(&self.transfers);
        self.handle.spawn(async move {
            let outcome = match tokio::spawn(body(em, cancel)).await {
                Ok(o) => o,
                Err(e) => TransferOutcome::Failed {
                    message: Message::XferProtocolError {
                        protocol: XferProtocol::Sftp,
                        detail: format!("transfer task failed: {e}"),
                    },
                    already_exists: false,
                },
            };
            table.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            // 结果交给等待者与发事件在同一处完成：等待者在传输启动前就已登记（它随任务一起移入），
            // 所以"传输在开始等待之前就结束"不会丢结果。等待者已放弃（future 被丢弃）时 send 失败，忽略。
            // 必须先发事件、再交结果：等待者一被唤醒就可能去看传输列表或事件，顺序反过来时
            // 它会看到传输仍处于进行中。
            events.on_event(CoreEvent::TransferFinished {
                transfer: id,
                outcome: outcome.clone(),
            });
            if let Some(tx) = waiter {
                let _ = tx.send(outcome);
            }
        });
        id
    }

    // ---- 终端内传输（F4）----

    pub(crate) fn xmodem_send(
        &self,
        session: SessionId,
        path: &str,
        block: XmodemBlock,
    ) -> Result<TransferId> {
        self.terminal_session(session)?
            .xmodem(true, block == XmodemBlock::Bytes1K, Some(path))
            .map_err(CoreError::User)
    }

    pub(crate) fn xmodem_receive(&self, session: SessionId, path: &str) -> Result<TransferId> {
        self.terminal_session(session)?
            .xmodem(false, false, Some(path))
            .map_err(CoreError::User)
    }

    pub(crate) fn xmodem_start(
        &self,
        session: SessionId,
        block: Option<XmodemBlock>,
    ) -> Result<TransferId> {
        self.terminal_session(session)?
            .xmodem(block.is_some(), block == Some(XmodemBlock::Bytes1K), None)
            .map_err(CoreError::User)
    }

    /// 向对端发送取消序列，并撤销尚未回答的文件面板。
    pub(crate) fn cancel_terminal_transfer(&self, session: SessionId) {
        if let Some(session) = self.sessions.session(session) {
            session.cancel_terminal_transfer(None);
        }
    }

    // ---- 导入导出 ----

    /// 导出到 `.kanso` 文件（不含凭据）。文件 IO 与数据库读取放进 `spawn_blocking`。
    ///
    /// 写失败返回 `User(ExportWriteFailed)`。
    pub(crate) async fn export_data(&self, path: &str) -> Result<()> {
        let store = Arc::clone(&self.store);
        let app_version = self.app_version.clone();
        let path = path.to_owned();
        self.handle
            .spawn_blocking(move || {
                let snapshot = store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .export_snapshot()
                    .map_err(store_err)?;
                match snapshot.write(Path::new(&path), &app_version) {
                    Ok(_) => Ok(()),
                    Err(StoreError::Io(e)) => Err(CoreError::User(Message::ExportWriteFailed {
                        path: path.clone(),
                        reason: io_reason(&e),
                    })),
                    Err(e) => Err(store_err(e)),
                }
            })
            .await
            .map_err(join_err)?
    }

    /// 从 `.kanso` 文件导入。单条无效记入报告的 `skipped`，不整体失败。
    ///
    /// 读文件失败返回 `User(ImportReadFailed)`。
    /// Profile 的校验复用 `validate_profile`：导入的会话与手工保存的会话遵守同一套规则。
    pub(crate) async fn import_data(&self, path: &str) -> Result<ImportReport> {
        let store = Arc::clone(&self.store);
        let path = path.to_owned();
        self.handle
            .spawn_blocking(move || {
                let merge = || {
                    let data = ImportData::read(Path::new(&path))?;
                    store
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .import_data(data, &|p, all| validate_profile(p, all))
                };
                let outcome = merge().map_err(|e| match e {
                    ImportError::WrongExtension => {
                        CoreError::User(Message::ImportWrongExtension {})
                    }
                    ImportError::NotKansoFile => CoreError::User(Message::ImportNotKansoFile {}),
                    ImportError::VersionTooNew { version, supported } => {
                        CoreError::User(Message::ImportVersionTooNew { version, supported })
                    }
                    ImportError::Io(e) => CoreError::User(Message::ImportReadFailed {
                        path: path.clone(),
                        reason: io_reason(&e),
                    }),
                    ImportError::Store(e) => store_err(e),
                })?;
                Ok(ImportReport {
                    added: outcome.added,
                    updated: outcome.updated,
                    skipped: outcome.skipped,
                    converted: outcome.converted,
                })
            })
            .await
            .map_err(join_err)?
    }

    /// 扫描 ssh_config 并标出已保存的主机（store-import.md §7.1、§7.2）。
    ///
    /// 文件不可读返回 `User(ImportReadFailed)`。
    pub(crate) async fn scan_ssh_config(&self, path: Option<&str>) -> Result<SshConfigScan> {
        let store = Arc::clone(&self.store);
        let env = self.ssh_config_env.clone();
        let path = path.map(PathBuf::from);
        self.handle
            .spawn_blocking(move || {
                let loaded = sshconfig_import::load(&env, path.as_deref())?;
                let profiles = store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .profiles()
                    .map_err(store_err)?;
                Ok(sshconfig_import::scan_result(&loaded, &profiles))
            })
            .await
            .map_err(join_err)?
    }

    /// 导入所选别名（store-import.md §7.3）。
    ///
    /// 每次重新读取并求值（文件可能在扫描后改过）；规划在内存里完成，最后用一个事务写库。
    /// 持有数据库锁覆盖"读已保存的 Profile → 规划 → 写入"，期间别的线程改不了 Profile，
    /// 校验结果不会过期。不碰凭据。
    pub(crate) async fn import_ssh_config(
        &self,
        path: Option<&str>,
        aliases: &[String],
        group: Option<GroupId>,
    ) -> Result<SshConfigImportReport> {
        let store = Arc::clone(&self.store);
        let env = self.ssh_config_env.clone();
        let path = path.map(PathBuf::from);
        let aliases = aliases.to_vec();
        self.handle
            .spawn_blocking(move || {
                let loaded = sshconfig_import::load(&env, path.as_deref())?;
                let mut db = store.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(g) = group
                    && db.group(g).map_err(store_err)?.is_none()
                {
                    return Err(CoreError::NoSuchGroup(g));
                }
                let stored = db.profiles().map_err(store_err)?;
                let (writes, report) =
                    sshconfig_import::plan_import(&loaded, &aliases, group, stored);
                db.upsert_profiles(&writes).map_err(store_err)?;
                Ok(report)
            })
            .await
            .map_err(join_err)?
    }

    // ---- 授权 ----

    /// MAS 与 Setapp 随渠道授权；Direct 在 License Key 激活接入前保持未授权。
    pub(crate) fn entitlement(&self) -> Option<Entitlement> {
        Entitlement::for_channel()
    }

    /// 未实现（T11）。
    pub(crate) async fn activate_license(&self, key: &str) -> Result<Entitlement> {
        Err(CoreError::NotSupported)
    }

    /// 未实现（I5）。
    pub(crate) async fn purchase(&self) -> Result<PurchaseOutcome> {
        Err(CoreError::NotSupported)
    }

    /// 未实现（I5）。
    pub(crate) async fn restore_purchases(&self) -> Result<Option<Entitlement>> {
        Err(CoreError::NotSupported)
    }
}

impl Drop for RealCore {
    /// 没调用 `shutdown` 就丢弃：会话管理器自己的 `Drop` 会杀 shell；这里只负责运行时，
    /// 用 `shutdown_background` 保证在任何上下文里都不会阻塞或 panic。
    fn drop(&mut self) {
        let rt = self
            .runtime
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(rt) = rt {
            rt.shutdown_background();
        }
    }
}
