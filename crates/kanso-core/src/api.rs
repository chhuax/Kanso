//! `Core` 公开 API：宿主（Swift UI / 测试）与内核之间的控制面契约。
//!
//! # 通用约定
//!
//! - `Core` 是 `Send + Sync` 的，所有方法取 `&self`，可以从任意线程调用；内部可变状态自行加锁。
//! - 同步方法分两类：**输入类**（`key`、`text_input`、`paste`、`mouse`、`scroll`、`focus`）
//!   不阻塞，可在主线程调用；**数据类**（`profiles`、`save_profile` 等）只做本地数据库操作，
//!   足够快，可在主线程调用，但不得在渲染线程里调用。
//! - `async fn` 可能涉及网络或磁盘，调用方在自己的运行时 / Swift Task 里 `await`；
//!   core 不要求调用方使用特定的异步运行时，本 crate 不依赖任何运行时。
//! - 异步进展（连接状态、传输进度、提问等）通过 `Platform.events` 推送 [`CoreEvent`]，
//!   不通过返回值轮询。
//! - 错误统一为 [`CoreError`]：能展示给用户的带 [`Message`]，其余只进日志。
//! - 终端内容走数据面：UI 收到 `TerminalDirty` 后在渲染线程调用 [`Core::take_frame`]
//!   拉取 [`Frame`]（architecture §4）。
//!
//! # 为什么用枚举分发而不是 trait 对象
//!
//! [`Core`] 在 mock 与真实实现之间切换。`async fn` 放进 `dyn Trait` 需要 `async-trait`
//! 之类的额外依赖，而 FFI 层只需要一个具体类型，所以用 `enum Backend` 加 `match` 分发，
//! 零额外依赖，也不产生装箱 future。
//!
//! # 本版刻意不包含的接口
//!
//! 下列接口留给后续契约 PR，在对应工作包开工前补充：
//!
//! - 窗口布局保存与恢复（T10）。
//!
//! # MCP 相关接口
//!
//! "MCP" 一节的方法只供 `kanso-mcp-server`（Rust）使用，类型见 [`crate::mcp`]，不导出到 FFI；
//! 例外是授权与审计的读写，设置页（Swift）也要用，将来由 FFI 按需导出。
//! 这些方法是通用能力，不按发行渠道裁剪；只有服务本身（`kanso-mcp-server`、`kanso-mcp`）仅在 Direct 编译。

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;

use kanso_frame::Frame;
use kanso_license::{Entitlement, PurchaseOutcome};
use kanso_secret::SecretKind;
use kanso_types::event::{SessionState, TransferOutcome};
use kanso_types::mcp::{McpAccess, McpAuditEntry, McpAuditFilter, McpAuditRecord};
use kanso_types::profile::{Group, LocalProfile, Profile, SessionKind, Snippet};
use kanso_types::secret::SecretString;
use kanso_types::transfer::ConflictPolicy;
use kanso_types::{
    DirEntry, GridSize, GroupId, KeyEvent, Message, Modifiers, MouseEvent, PasteContent, ProfileId,
    PromptId, PromptResponse, SessionId, TerminalOptions, TimestampMode, TransferId, ViewportPoint,
};

use crate::mcp::{
    ConnectMode, ExecOutcome, McpConnectOutcome, McpInput, McpSessionStatus, SftpWriteMode,
    TerminalRunOutcome, TerminalSendOutcome, TerminalText, TextScope, WaitLimits,
};
use crate::mock::MockCore;
use crate::platform::Platform;
use crate::real::RealCore;

/// `Result` 简写，错误类型固定为 [`CoreError`]。
pub type Result<T> = std::result::Result<T, CoreError>;

/// 命令编辑快照，只含光标前输入与当前会话的目录上报。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionState {
    /// 光标前输入。
    pub input: String,
    /// 输入起点列。
    pub start_column: u16,
    /// 最新上报路径。
    pub directory: Option<String>,
    /// 上报中的主机名。
    pub directory_host: Option<String>,
}

/// 宿主为纯补全引擎提供的外部数据。
#[derive(Clone, Debug, Default)]
pub struct CompletionSources {
    /// 可信工作目录；未知时为空。
    pub cwd: Option<String>,
    /// 当前机器的家目录；未知时为空。
    pub home: Option<String>,
    /// 是否可使用该会话的文件服务。
    pub allow_paths: bool,
    /// 有限目录流提供的条目。
    pub entries: Vec<kanso_complete::DirectoryEntry>,
    /// 本地配置文件内容；SSH 留空。
    pub configs: Vec<String>,
}

/// 候选及下一步确实需要读取的来源。
#[derive(Clone, Debug, Default)]
pub struct CompletionSuggestions {
    /// 已有候选。
    pub candidates: Vec<kanso_complete::Candidate>,
    /// 适用的目录读取请求。
    pub directory: Option<String>,
    /// 是否在配置名称值位置。
    pub needs_kube_names: bool,
    /// 相对路径是否需要查询可信工作目录。
    pub needs_cwd: bool,
    /// 波浪线路径是否需要查询当前机器家目录。
    pub needs_home: bool,
}

/// `Core::new` 的配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreConfig {
    /// 数据目录（数据库、日志等）。由宿主决定（沙盒容器内路径），必须可写。
    pub data_dir: PathBuf,
    /// 应用版本号，用于导出文件元数据与日志。
    pub app_version: String,
    /// 应用 bundle id，用于凭据存储 service 名等。
    pub bundle_id: String,
    /// 系统首选语言标识（如 `zh-Hans-CN`、`en`），由宿主取自系统设置；本地 shell 推算 `LANG` 时用
    /// （local-shell.md §4）。
    pub preferred_language: String,
    /// 用户在系统设置里选的地区代码（如 `CN`）；没有为 `None`。同样只用于 locale 推算。
    pub region: Option<String>,
    /// 默认本地 shell 的命令行；`None` 取 `$SHELL`（没有则 `/bin/bash`）。
    /// 语法同 Profile 的 Shell 字段（local-shell.md §1）。用于测试和将来的设置项。
    pub default_shell: Option<String>,
    /// 是否启用 zsh 提示符集成（local-shell.md §5.1 的开关）。默认开启。
    pub shell_integration: bool,
}

impl CoreConfig {
    /// 以默认值构造：语言 `en`、无地区、默认 shell 取 `$SHELL`、开启 shell 集成。
    pub fn new(data_dir: impl Into<PathBuf>, app_version: &str, bundle_id: &str) -> CoreConfig {
        CoreConfig {
            data_dir: data_dir.into(),
            app_version: app_version.to_owned(),
            bundle_id: bundle_id.to_owned(),
            preferred_language: "en".to_owned(),
            region: None,
            default_shell: None,
            shell_integration: true,
        }
    }
}

/// core 返回的错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// 可直接展示给用户的错误。
    User(Message),
    /// 会话不存在（已关闭或 id 无效）。
    NoSuchSession(SessionId),
    /// Profile 不存在。
    NoSuchProfile(ProfileId),
    /// 分组不存在。
    NoSuchGroup(GroupId),
    /// 该会话不支持此操作，例如对纯 SFTP 会话发键盘输入。
    /// 需要给用户提示时，core 改用 `User(Message::SessionNotTerminal)`。
    NotSupported,
    /// 内部错误；`detail` 为英文技术细节，只进日志，不展示给用户。
    Internal {
        /// 技术细节。
        detail: String,
    },
}

impl CoreError {
    /// 可展示给用户的消息；只有 [`CoreError::User`] 有。其余错误 UI 应显示通用失败提示。
    pub fn user_message(&self) -> Option<&Message> {
        match self {
            CoreError::User(m) => Some(m),
            _ => None,
        }
    }
}

impl fmt::Display for CoreError {
    /// 英文，供日志使用；不得直接展示给用户。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::User(m) => write!(f, "user-facing error: {}", m.key()),
            CoreError::NoSuchSession(id) => write!(f, "no such session: {id}"),
            CoreError::NoSuchProfile(id) => write!(f, "no such profile: {id}"),
            CoreError::NoSuchGroup(id) => write!(f, "no such group: {id}"),
            CoreError::NotSupported => f.write_str("operation not supported for this session"),
            CoreError::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// 打开会话的目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    /// 按已保存的 Profile 打开。
    Profile(ProfileId),
    /// 用默认配置打开本地 shell（没有对应 Profile）。
    DefaultLocalShell,
    /// 使用本次配置打开临时本地 shell。
    LocalShell(LocalProfile),
    /// 复制标签：SSH 复用已认证的连接再开一个 shell 通道，
    /// 本地 shell 用同一配置重新启动（ui-shell.md §3.3、ssh.md §6）。
    Duplicate(SessionId),
}

/// 会话摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    /// 会话 id。
    pub id: SessionId,
    /// 对应的 Profile；`DefaultLocalShell` 打开的会话为 `None`。
    pub profile: Option<ProfileId>,
    /// 会话种类。
    pub kind: SessionKind,
    /// 默认显示名（Profile 名或会话默认名）。
    pub name: String,
    /// 用户或 MCP 为标签设置的自定义名；`None` 表示使用默认标题。
    pub custom_title: Option<String>,
    /// 当前状态。
    pub state: SessionState,
}

/// 删除分组会连带删除的内容，删除前向用户确认用（store-import.md §3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeleteImpact {
    /// 将被删除的 Profile 数。
    pub profiles: u32,
    /// 将被删除的 Sender 命令数。
    pub snippets: u32,
}

/// 搜索请求（terminal.md §11）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// 搜索文本；`regex` 为真时是正则。
    pub text: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 是否按正则解释。
    pub regex: bool,
}

/// 搜索结果状态，用于显示“第几个 / 共几个”。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchStatus {
    /// 正则无效，与有效查询的零个匹配区分。
    pub invalid_regex: bool,
    /// 匹配总数。
    pub total: u32,
    /// 当前匹配项，从 1 开始；没有匹配为 `None`。
    pub current: Option<u32>,
}

/// XMODEM 块大小。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmodemBlock {
    /// 128 字节块。
    Bytes128,
    /// 1K 块（XMODEM-1K）。
    Bytes1K,
}

/// `.kanso` 数据文件导入结果（store-import.md §6）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReport {
    /// 新增条数。
    pub added: u32,
    /// 更新条数。
    pub updated: u32,
    /// 被跳过的条目及原因。
    pub skipped: Vec<Message>,
    /// 已转换的命令及原因。
    pub converted: Vec<Message>,
}

/// OpenSSH 配置里扫描出的一个可导入主机（store-import.md §7）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfigCandidate {
    /// Host 别名。
    pub alias: String,
    /// 解析后的主机名。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// 用户名。
    pub user: String,
    /// IdentityFile（原样路径）。
    pub identity_file: Option<String>,
    /// ProxyJump 的原始文本。
    pub proxy_jump: Option<String>,
    /// 已存在的同名 Profile；导入时会更新它而不是新增。
    pub existing: Option<ProfileId>,
    /// 该条目的警告（例如不支持的指令）。
    pub warnings: Vec<Message>,
}

/// [`Core::scan_ssh_config`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfigScan {
    /// 可导入的主机，按首次出现的顺序。
    pub candidates: Vec<SshConfigCandidate>,
    /// 文件级警告（不属于任何单个别名）。
    pub warnings: Vec<Message>,
}

/// OpenSSH 配置导入结果（store-import.md §7.3）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfigImportReport {
    /// 新增的 Profile 数。
    pub added: u32,
    /// 更新的 Profile 数。
    pub updated: u32,
    /// 为跳板机新增的 Profile 数。
    pub jumps_added: u32,
    /// 因多跳 ProxyJump 而忽略的条目数。
    pub multi_hop_ignored: u32,
    /// 警告。
    pub warnings: Vec<Message>,
}

/// 大目录的流式列表（ssh.md §8）。
///
/// 由 [`Core::sftp_list`] 创建；调用方反复调用 [`next_batch`](Self::next_batch)
/// 直到返回 `Ok(None)`。丢弃它即取消后续读取。
pub struct DirStream {
    source: DirSource,
}

/// 目录条目的来源：mock 的预排批次，或真实 SFTP 连接上按需读取的列表。
enum DirSource {
    Batches(VecDeque<Vec<DirEntry>>),
    Remote(crate::real::RemoteDirList),
}

impl DirStream {
    /// 由已有批次构造（mock 与测试用）。
    pub(crate) fn from_batches(batches: Vec<Vec<DirEntry>>) -> DirStream {
        DirStream {
            source: DirSource::Batches(batches.into()),
        }
    }

    /// 由真实 SFTP 列表构造（real backend 用）。
    pub(crate) fn from_remote(list: crate::real::RemoteDirList) -> DirStream {
        DirStream {
            source: DirSource::Remote(list),
        }
    }

    /// 取下一批条目；`Ok(None)` 表示已列完。
    ///
    /// - 线程：任意线程；同一个流不能并发调用（`&mut self` 已保证）。
    /// - 阻塞：异步，等待服务器返回下一批。
    /// - 出错：返回 `User(SftpOpFailed)` 等；出错后流不再可用。
    /// - 实现：T7b（SFTP 引擎）。
    pub async fn next_batch(&mut self) -> Result<Option<Vec<DirEntry>>> {
        self.next_batch_limited(usize::MAX).await
    }

    /// 最多解码一批中的指定条数；到达补全读取上限后调用方丢弃流。
    pub async fn next_batch_limited(&mut self, limit: usize) -> Result<Option<Vec<DirEntry>>> {
        if limit == 0 {
            return Ok(None);
        }
        match &mut self.source {
            DirSource::Batches(b) => Ok(b.pop_front().map(|mut batch| {
                batch.truncate(limit);
                batch
            })),
            DirSource::Remote(list) => list.next_batch(limit).await,
        }
    }
}

/// 把调用转发给当前 backend。`$core` 是 `&Core`。
macro_rules! dispatch {
    ($core:expr, $m:ident ( $($a:expr),* $(,)? )) => {
        match &$core.backend {
            Backend::Mock(b) => b.$m($($a),*),
            Backend::Real(b) => b.$m($($a),*),
        }
    };
}

/// 同 [`dispatch!`]，用于 `async fn`：两个分支的 future 类型不同，各自在分支内 `await`。
macro_rules! dispatch_async {
    ($core:expr, $m:ident ( $($a:expr),* $(,)? )) => {
        match &$core.backend {
            Backend::Mock(b) => b.$m($($a),*).await,
            Backend::Real(b) => b.$m($($a),*).await,
        }
    };
}

/// `mcp_terminal_run` 对标签的占用；离开作用域（含 future 被丢弃）时释放。
struct McpClaim<'a> {
    sender: &'a std::sync::Mutex<crate::sender::Scheduler>,
    session: SessionId,
}

impl Drop for McpClaim<'_> {
    fn drop(&mut self) {
        // Drop 里不能 panic（可能正在展开）：锁中毒时照样取出数据释放占用。
        self.sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mcp_release(self.session);
    }
}

/// mock core 的 bundle id。用不会与真实 App 冲突的值，MCP 服务的锁与 socket 路径也就不会撞上真实 App。
const MOCK_BUNDLE_ID: &str = "mock.kanso";

/// 具体实现。见模块文档“为什么用枚举分发”。
enum Backend {
    Mock(MockCore),
    /// 装箱：`RealCore` 持有会话管理器等，比 mock 大得多（clippy::large_enum_variant）。
    Real(Box<RealCore>),
}

/// core 门面：宿主持有的唯一入口。
///
/// 构造后在整个进程生命周期内共享（通常包在 `Arc` 里）；退出前调用 [`Core::shutdown`]。
pub struct Core {
    backend: Backend,
    sender: std::sync::Mutex<crate::sender::Scheduler>,
    sender_clock: std::time::Instant,
    /// MCP 授权变化的通知器，与后端共用同一个，见 [`Core::mcp_policy_watch`]。
    mcp_policy: crate::mcp::PolicyNotifier,
    /// 构造时的 bundle id，见 [`Core::bundle_id`]。
    bundle_id: String,
}

// 编译期断言：FFI 层与多线程宿主依赖 Core 可跨线程共享。
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Core>();
    assert_send_sync::<DirStream>();
};

impl Core {
    // ---- 生命周期 ----

    /// 创建真实 core。
    ///
    /// - 线程：任意；通常在应用启动时调用一次。
    /// - 阻塞：会创建数据目录、启动 tokio 运行时、打开 / 迁移数据库（`<data_dir>/kanso.db`），
    ///   耗时与数据量相关，不要在主线程做首帧之前的关键路径上调用。
    /// - 出错：数据目录无法创建或不可用、数据库损坏或版本过新时返回 `Internal`（不降级运行）。
    /// - 实现：I1（会话主干：本地会话）+ T8（存储）+ I2（单跳 SSH 会话）+ T6b（跳板链、ProxyCommand、远端 cwd）+ T7b（SFTP 会话、文件操作、传输），均已完成。
    ///   **未实现的方法**（终端内传输见 F4，授权见 T11 / I5 等）不会 panic：
    ///   返回 `NotSupported`、空集合或 `None`，具体见各方法的文档与 `RealCore` 的模块文档。
    pub fn new(config: CoreConfig, platform: Platform) -> Result<Core> {
        let mcp_policy = crate::mcp::PolicyNotifier::new();
        let bundle_id = config.bundle_id.clone();
        Ok(Core {
            bundle_id,
            backend: Backend::Real(Box::new(RealCore::new(
                config,
                platform,
                None,
                mcp_policy.clone(),
            )?)),
            sender: Default::default(),
            sender_clock: std::time::Instant::now(),
            mcp_policy,
        })
    }

    /// 同 [`Core::new`]，但指定本地 shell 子进程的环境变量基线（默认是本进程的环境）。
    ///
    /// 只为测试隔离存在：测试要把 `HOME`、`ZDOTDIR` 指向临时目录，而修改本进程的环境变量不安全
    /// （多线程下 `set_var` 是 unsafe，且会影响并行的其他测试）。产品代码用 [`Core::new`]。
    /// 不属于对外契约（不导出到 FFI，文档中隐藏），以后可能随测试需要改动。
    #[doc(hidden)]
    pub fn new_with_base_env(
        config: CoreConfig,
        platform: Platform,
        base_env: Vec<(String, String)>,
    ) -> Result<Core> {
        let env = kanso_session::EnvSnapshot::from_pairs(base_env);
        let mcp_policy = crate::mcp::PolicyNotifier::new();
        let bundle_id = config.bundle_id.clone();
        Ok(Core {
            bundle_id,
            backend: Backend::Real(Box::new(RealCore::new(
                config,
                platform,
                Some(env),
                mcp_policy.clone(),
            )?)),
            sender: Default::default(),
            sender_clock: std::time::Instant::now(),
            mcp_policy,
        })
    }

    /// 创建 mock core：纯内存、确定性数据，不碰磁盘与网络，供 UI 在内核完成前开发与截图测试。
    ///
    /// - 线程：任意。阻塞：不阻塞。出错：不会失败。
    /// - 事件通过 `platform.events` 发出；凭据写入 `platform.secrets`。
    /// - 实现：P0.4（本 crate 的 `mock` 模块）。
    pub fn new_mock(platform: Platform) -> Core {
        let mcp_policy = crate::mcp::PolicyNotifier::new();
        Core {
            bundle_id: MOCK_BUNDLE_ID.to_owned(),
            backend: Backend::Mock(MockCore::new(platform, mcp_policy.clone())),
            sender: Default::default(),
            sender_clock: std::time::Instant::now(),
            mcp_policy,
        }
    }

    /// 构造时传入的 bundle id（mock core 为 `mock.kanso`）。
    ///
    /// MCP 服务用它推出单实例锁与 socket 的路径（architecture §16.1）。服务必须在 App 启动时就持有锁，
    /// 而那时还没有调用 `start`，所以从 core 取，而不是等 `start` 的参数。
    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    /// 关闭 core：停止后台任务（传输、连接）、刷盘。
    ///
    /// - 线程：任意；应用退出前调用一次。调用之后不应再使用 `Core`。
    /// - 阻塞：可能短暂阻塞等待后台任务收尾。
    /// - 出错：无返回值，失败只记日志。
    /// - 实现：I1。
    pub fn shutdown(&self) {
        dispatch!(self, shutdown())
    }

    // ---- 数据 ----

    /// 全部 Profile。
    ///
    /// - 线程：任意（数据库读很快，可在主线程）。阻塞：同步，极短。出错：不会失败。
    /// - 规格：store-import.md §1。实现：T8。
    pub fn profiles(&self) -> Vec<Profile> {
        dispatch!(self, profiles())
    }

    /// 新增或更新（按 `id`）一个 Profile。
    ///
    /// - 线程：任意。阻塞：同步，极短。
    /// - 校验：SSH / SFTP 主机或用户名为空返回 `User(ProfileHostEmpty / ProfileUserEmpty)`；
    ///   跳板链成环、缺失、非 SSH、超过 8 跳返回对应 `Jump*` 消息；同时设置跳板机与
    ///   ProxyCommand 返回 `User(JumpProxyCommandConflict)`。校验逻辑见 `validate_profile`。
    /// - 其他出错：所属分组不存在返回 `NoSuchGroup`。
    /// - 规格：store-import.md §3、ssh.md §5。实现：T8。
    pub fn save_profile(&self, profile: Profile) -> Result<()> {
        dispatch!(self, save_profile(profile))
    }

    /// 删除 Profile，连带删除它的凭据和只属于它的 Sender 命令。
    ///
    /// - 线程：任意。阻塞：`async`，因为钥匙串访问可能弹出系统授权框、等待用户操作，
    ///   同步方法会卡住调用线程（例如主线程）。真实实现在 `spawn_blocking` 线程里访问凭据存储。
    /// - 出错：不存在返回 `NoSuchProfile`；凭据存储失败返回 `User(SecretStoreFailed)`。
    /// - 已打开的会话不受影响。其他 Profile 对它的跳板引用不会被改写，下次连接时报 `JumpMissing`。
    /// - 规格：store-import.md §3。实现：T8。
    pub async fn delete_profile(&self, id: ProfileId) -> Result<()> {
        // 删除会连带删除 MCP 授权，后端在数据库提交后立即通知订阅者（见 `PolicyNotifier`）。
        dispatch_async!(self, delete_profile(id))
    }

    /// 全部分组。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：不会失败。
    /// - 规格：store-import.md §1。实现：T8。
    pub fn groups(&self) -> Vec<Group> {
        dispatch!(self, groups())
    }

    /// 新增或更新（按 `id`）一个分组。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：存储失败返回 `Internal`。
    /// - 规格：store-import.md §1。实现：T8。
    pub fn save_group(&self, group: Group) -> Result<()> {
        dispatch!(self, save_group(group))
    }

    /// 删除分组会连带删除多少内容；UI 在确认对话框里显示这两个数字。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：不存在返回 `NoSuchGroup`。
    /// - 数字必须与随后的 [`Core::delete_group`] 实际删除的一致。
    /// - 规格：store-import.md §3。实现：T8。
    pub fn group_delete_impact(&self, id: GroupId) -> Result<DeleteImpact> {
        dispatch!(self, group_delete_impact(id))
    }

    /// 删除分组：组内 Profile（连同凭据）以及作用域是该分组或组内 Profile 的 Sender 命令一并删除。
    ///
    /// - 调用方应先用 `group_delete_impact` 让用户确认。
    /// - 线程：任意。阻塞：`async`，因为钥匙串访问可能弹出系统授权框、等待用户操作，
    ///   同步方法会卡住调用线程（例如主线程）。真实实现在 `spawn_blocking` 线程里访问凭据存储。
    /// - 出错：不存在返回 `NoSuchGroup`；凭据存储失败返回 `User(SecretStoreFailed)`。
    /// - 规格：store-import.md §3。实现：T8。
    pub async fn delete_group(&self, id: GroupId) -> Result<()> {
        // 同 delete_profile：组内会话的 MCP 授权随之删除，由后端通知。
        dispatch_async!(self, delete_group(id))
    }

    /// 全部 Sender 命令（快捷片段）。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：不会失败。
    /// - 规格：store-import.md §4。实现：T8。
    pub fn snippets(&self) -> Vec<Snippet> {
        dispatch!(self, snippets())
    }

    /// 读取当前标签适用的 Sender 命令；存储失败通过错误返回。
    pub fn visible_snippets(&self, session: Option<SessionId>) -> Result<Vec<Snippet>> {
        let context = session.and_then(|id| self.session_info(id));
        let profile = context.as_ref().and_then(|info| info.profile);
        let kind = context.as_ref().map(|info| info.kind);
        match &self.backend {
            Backend::Real(backend) => backend.visible_snippets(profile, kind),
            Backend::Mock(backend) => Ok(kanso_store::visible_snippets(
                backend.snippets(),
                &backend.profiles(),
                profile,
                kind,
            )),
        }
    }

    /// 冻结发送内容并开始任务；`sessions` 是同窗口全部打开的标签。
    pub fn sender_start(
        &self,
        request: crate::sender::SenderRequest,
        sessions: Vec<SessionId>,
    ) -> Result<()> {
        let connected = self.sender_connected(&sessions);
        self.sender
            .lock()
            .expect("Sender 锁未中毒")
            .start(
                request,
                connected,
                self.sender_clock.elapsed().as_millis() as u64,
            )
            .map_err(CoreError::User)
    }

    fn sender_connected(&self, sessions: &[SessionId]) -> Vec<SessionId> {
        sessions
            .iter()
            .copied()
            .filter(|id| {
                self.session_info(*id).is_some_and(|info| {
                    info.kind != SessionKind::Sftp && info.state == SessionState::Connected
                })
            })
            .collect()
    }

    /// 停止后续发送，保留已经写入的内容。
    pub fn sender_stop(&self, repeating: bool) {
        self.sender.lock().expect("Sender 锁未中毒").stop(repeating);
    }

    /// 按当前时间推进任务，返回供面板与状态栏显示的进度。
    pub fn sender_tick(&self, sessions: Vec<SessionId>) -> crate::sender::SenderStatus {
        let connected = self.sender_connected(&sessions);
        self.sender.lock().expect("Sender 锁未中毒").tick(
            self.sender_clock.elapsed().as_millis() as u64,
            &sessions,
            &connected,
            |id| match &self.backend {
                Backend::Real(backend) => backend.sender_ready(id),
                Backend::Mock(_) => self
                    .session_info(id)
                    .filter(|info| {
                        info.state == SessionState::Connected && info.kind != SessionKind::Sftp
                    })
                    .map(|_| true)
                    .ok_or(Message::SenderTerminalUnavailable {}),
            },
            |id, text, ending| match &self.backend {
                Backend::Real(backend) => backend.sender_line(id, text, ending),
                Backend::Mock(backend) => backend
                    .text_input(id, &format!("{text}{ending}"))
                    .map(|_| false)
                    .map_err(|error| Message::SenderFailed {
                        detail: error.to_string(),
                    }),
            },
        )
    }

    /// 新增或更新（按 `id`）一条 Sender 命令。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：存储失败返回 `Internal`。
    /// - 规格：store-import.md §4。实现：T8。
    pub fn save_snippet(&self, snippet: Snippet) -> Result<()> {
        if snippet.text.trim().is_empty() {
            return Err(CoreError::User(Message::SenderEmptyCommand {}));
        }
        dispatch!(self, save_snippet(snippet))
    }

    /// 删除一条 Sender 命令；不存在时视为成功（幂等）。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：存储失败返回 `Internal`。
    /// - 规格：store-import.md §4。实现：T8。
    pub fn delete_snippet(&self, id: kanso_types::SnippetId) -> Result<()> {
        dispatch!(self, delete_snippet(id))
    }

    /// 保存某个 Profile 的凭据到凭据存储。
    ///
    /// - 线程：任意。阻塞：`async`，因为钥匙串访问可能弹出系统授权框、等待用户操作，
    ///   同步方法会卡住调用线程（例如主线程）。真实实现在 `spawn_blocking` 线程里访问凭据存储。
    /// - 出错：Profile 不存在返回 `NoSuchProfile`；存储失败返回 `User(SecretStoreFailed)`。
    /// - 明文只经 [`SecretString`] 传入，不会写入日志或数据库（store-import.md §2）。
    /// - 实现：T8。
    pub async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> Result<()> {
        dispatch_async!(self, set_credential(profile, kind, secret))
    }

    /// 清除命令历史。
    ///
    /// - 线程：任意。阻塞：同步，可能触及磁盘。出错：存储失败返回 `Internal`。
    /// - 规格：store-import.md §5。实现：T8。
    pub fn clear_history(&self) -> Result<()> {
        dispatch!(self, clear_history())
    }

    /// 获取普通终端正在编辑的输入；锁定、运行中或不明确时为空。
    pub fn completion_state(&self, session: SessionId) -> Option<CompletionState> {
        dispatch!(self, completion_state(session))
    }

    /// 对仍然有效的输入生成候选，不执行外部查询。
    pub fn complete(
        &self,
        session: SessionId,
        input: &str,
        sources: CompletionSources,
    ) -> CompletionSuggestions {
        dispatch!(self, complete(session, input, sources))
    }

    /// 只解析外部来源需求，不读历史、不生成或排序候选。
    pub fn completion_plan(
        &self,
        session: SessionId,
        input: &str,
        sources: CompletionSources,
    ) -> CompletionSuggestions {
        dispatch!(self, completion_plan(session, input, &sources))
    }

    // ---- 设置 ----

    /// 读设置值（JSON 文本）；没有保存过返回 `None`（调用方按默认值处理）。
    ///
    /// - 线程：任意。阻塞：同步，只读本地数据库。读失败时记日志并返回 `None`。
    /// - 键与值的结构由宿主（F7）定义，core 只存取；设置随 `.kanso` 导出（store-import.md §6.1、§9）。
    pub fn setting(&self, key: &str) -> Option<String> {
        dispatch!(self, setting(key))
    }

    /// 写设置值。`json` 必须是合法 JSON，否则返回 `Internal`（写入时挡住坏数据，导出与读取方不必各自处理）。
    ///
    /// - 线程：任意。阻塞：同步，可能触及磁盘。出错：存储失败返回 `Internal`。
    pub fn set_setting(&self, key: &str, json: String) -> Result<()> {
        dispatch!(self, set_setting(key, json))
    }

    /// 清空全部设置（"帮助 → 恢复默认设置…"）。**只动设置**，不动会话、分组、Sender 命令与历史
    /// （store-import.md §9）。调用方随后自行重新加载设置。
    ///
    /// - 线程：任意。阻塞：同步，可能触及磁盘。出错：存储失败返回 `Internal`。
    pub fn reset_settings(&self) -> Result<()> {
        dispatch!(self, reset_settings())
    }

    /// 读取新建本地 Shell 的默认配置。
    pub fn local_shell_defaults(&self) -> LocalProfile {
        self.setting(kanso_store::LOCAL_SHELL_DEFAULTS)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// 保存以后新建本地 Shell 使用的默认配置。
    pub fn set_local_shell_defaults(&self, profile: &LocalProfile) -> Result<()> {
        let json = serde_json::to_string(profile).expect("本地 Shell 配置可序列化");
        self.set_setting(kanso_store::LOCAL_SHELL_DEFAULTS, json)
    }

    // ---- 会话 ----

    /// 打开会话，立即返回会话 id；连接进度通过 `SessionStateChanged`、`Prompt` 等事件推送。
    ///
    /// - 线程：任意。阻塞：不阻塞；连接 / 启动在后台进行。
    /// - 出错：Profile 或被复制的会话不存在返回 `NoSuchProfile` / `NoSuchSession`；
    ///   连接类失败不在这里返回，而是通过事件（`Closed(Error(..))`）通知。
    /// - SSH Profile：立即返回 id，状态 `Connecting`；认证提问（`Prompt`）、主机密钥变更确认后的自动重连、
    ///   连接失败（`Closed(Error(..))`）都经事件推送。带跳板机的 Profile 连整条链
    ///   （成环 / 缺失 / 非 SSH / 超 8 跳返回对应 `User(Jump*)`，不会悄悄直连）；带 ProxyCommand 的 Profile 用命令作为传输，
    ///   MAS 渠道返回 `NotSupported`；
    ///   主机或用户为空返回 `User(ProfileHostEmpty / ProfileUserEmpty)`。
    /// - 纯 SFTP Profile：握手、认证、跳板、提问与 SSH Profile 完全相同，只是不开 shell；状态 `Connecting → Connected`，
    ///   `SessionInfo.kind = Sftp`。终端类方法返回 `NotSupported`，`take_frame` 返回 `None`。
    ///   连接断开（对端关闭、keepalive 超时）后状态变为 `Closed(Error(ssh-connection-lost))`；
    ///   复制纯 SFTP 会话得到同一连接上的另一个纯 SFTP 会话。
    /// - 复制 SSH 会话（`Duplicate`）：在同一个已认证连接上再开 shell，不重新认证；连接已断时新会话
    ///   以 `SshSharedConnectionClosed` 关闭，不偷偷重连。
    /// - 规格：ui-shell.md §3.3、ssh.md §5、§6、local-shell.md。实现：I1（本地）、I2（单跳 SSH）、T6b（跳板链、ProxyCommand）、T7b（纯 SFTP）。
    pub fn open_session(&self, target: OpenTarget, size: GridSize) -> Result<SessionId> {
        let target = match target {
            OpenTarget::DefaultLocalShell => OpenTarget::LocalShell(self.local_shell_defaults()),
            target => target,
        };
        dispatch!(self, open_session(target, size))
    }

    /// 用户主动关闭会话；之后发 `SessionStateChanged(Closed(ByUser))`。会话不存在时忽略。
    ///
    /// - 线程：任意。阻塞：不阻塞；进程 / 连接在后台收尾。出错：无。
    /// - 规格：local-shell.md §7、ssh.md §6。实现：I1（本地）、I2（SSH：同时取消该会话未决的提问并发 `PromptDismissed`）。
    pub fn close_session(&self, session: SessionId) {
        dispatch!(self, close_session(session))
    }

    /// 当前全部会话。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：不会失败。
    /// - 实现：I1。
    pub fn sessions(&self) -> Vec<SessionInfo> {
        dispatch!(self, sessions())
    }

    /// 单个会话的摘要；不存在为 `None`。
    ///
    /// - 线程：任意。阻塞：同步，极短。
    /// - 实现：I1。
    pub fn session_info(&self, session: SessionId) -> Option<SessionInfo> {
        dispatch!(self, session_info(session))
    }

    /// 设置或清除标签的自定义名（去掉首尾空白后为空视为清除，恢复默认标题）。
    ///
    /// - 线程：任意（通常主线程）。阻塞：同步，极短。出错：会话不存在时忽略。
    /// - 规格：ui-shell.md §2.2、§2.4，mcp.md §3.1。
    pub fn set_session_title(&self, session: SessionId, title: Option<String>) {
        let normalized = title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        dispatch!(self, set_session_title(session, normalized))
    }

    /// 告知 core 标签是否可见。不可见的标签只解析不渲染，后台完成时才发系统通知。
    ///
    /// - 线程：任意（通常主线程）。阻塞：不阻塞。出错：会话不存在时忽略。
    /// - 规格：terminal.md §5、§6，architecture §4。实现：I1。
    pub fn set_visible(&self, session: SessionId, visible: bool) {
        dispatch!(self, set_visible(session, visible))
    }

    /// 调整终端尺寸（网格与像素）。像素尺寸用于图片协议与鼠标像素上报。
    ///
    /// - 线程：任意（通常主线程）。阻塞：不阻塞；PTY / SSH 通道的窗口变更在后台发送。
    /// - 出错：会话不存在返回 `NoSuchSession`。纯 SFTP 会话没有终端，返回 `NotSupported`。
    /// - 终端因此变脏，会触发 `TerminalDirty`。
    /// - 规格：architecture §4。实现：I1。
    pub fn resize(
        &self,
        session: SessionId,
        size: GridSize,
        cell_width_px: u16,
        cell_height_px: u16,
    ) -> Result<()> {
        dispatch!(self, resize(session, size, cell_width_px, cell_height_px))
    }

    // ---- 输入 ----
    //
    // 输入类方法的共同约定：同步、不阻塞，可在主线程调用。会话已结束或文件传输进行中时
    // 输入被丢弃并返回 `Ok`（terminal.md §1、§13）；纯 SFTP 会话返回 `NotSupported`；
    // 会话不存在返回 `NoSuchSession`。

    /// 一次按键。core 负责编码成终端字节序列（含 kitty keyboard protocol）。
    ///
    /// IME 组字期间的按键不要发给本方法；IME 上屏的文字用 [`Core::text_input`]。
    ///
    /// - 规格：terminal.md §9、§16。实现：T4（编码）+ I1（接线）。
    pub fn key(&self, session: SessionId, event: KeyEvent) -> Result<()> {
        dispatch!(self, key(session, event))
    }

    /// 直接输入文本（IME 上屏、输入法候选、辅助功能听写等），不经过按键编码。
    ///
    /// - 规格：terminal.md §16。实现：T4 + I1。
    pub fn text_input(&self, session: SessionId, text: &str) -> Result<()> {
        dispatch!(self, text_input(session, text))
    }

    /// 粘贴。转义、AI CLI 会话的特殊处理、bracketed paste 都由 core 决定；
    /// UI 只负责读剪贴板并归类为 [`PasteContent`]。
    ///
    /// - 规格：terminal.md §8.3。实现：T4 + I1。
    pub fn paste(&self, session: SessionId, content: PasteContent) -> Result<()> {
        dispatch!(self, paste(session, content))
    }

    /// 鼠标事件。程序开启鼠标上报时转成上报序列；否则用于选区。
    /// 按住 Shift 时即使开启了上报也由终端自己处理（选区）。
    ///
    /// - 规格：terminal.md §8.1。实现：T4 + I1。
    pub fn mouse(&self, session: SessionId, event: MouseEvent) -> Result<()> {
        dispatch!(self, mouse(session, event))
    }

    /// 滚轮 / 触摸板滚动。`delta_lines` 为带小数的行数，正数向上（看更早的内容）；
    /// core 累积小数部分。备用屏或程序开启鼠标上报时按终端规则转成按键或上报序列。
    ///
    /// - 规格：terminal.md §8、§9。实现：T4 + I1。
    pub fn scroll(
        &self,
        session: SessionId,
        delta_lines: f32,
        at: ViewportPoint,
        mods: Modifiers,
    ) -> Result<()> {
        dispatch!(self, scroll(session, delta_lines, at, mods))
    }

    /// 终端获得 / 失去焦点。程序开启焦点上报（DECSET 1004）时转成序列，并影响光标样式。
    ///
    /// - 规格：terminal.md §9。实现：T4 + I1。
    pub fn focus(&self, session: SessionId, focused: bool) -> Result<()> {
        dispatch!(self, focus(session, focused))
    }

    // ---- 选区与缓冲区 ----

    /// 当前是否有选区。
    ///
    /// UI 据此判断：复制键绑定为 Ctrl+字母且没有选区时，按键照常发给 shell（terminal.md §8.2）。
    ///
    /// - 线程：任意（通常主线程）。阻塞：同步，极短。会话不存在返回 `false`。
    /// - 实现：T4 + I1。
    pub fn has_selection(&self, session: SessionId) -> bool {
        dispatch!(self, has_selection(session))
    }

    /// 提取命令块文本，范围为命令（0）、输出（1）或全部（2）。
    pub fn command_block_text(&self, session: SessionId, id: u64, part: u8) -> Option<String> {
        dispatch!(self, command_block_text(session, id, part))
    }

    /// 滚动到相邻命令块，不改变选区或 shell 输入。
    pub fn jump_command_block(&self, session: SessionId, forward: bool) -> Result<Option<u64>> {
        dispatch!(self, jump_command_block(session, forward))
    }

    /// 选区文本；没有选区或会话不存在为 `None`。
    ///
    /// - 线程：任意。阻塞：同步，选区很大时复制成本与大小成正比，持锁时间应受限。
    /// - 规格：terminal.md §8.2。实现：T4 + I1。
    pub fn selection_text(&self, session: SessionId) -> Option<String> {
        dispatch!(self, selection_text(session))
    }

    /// 选中视口内 `at` 处的单词。macOS 右键弹菜单前先调用，使菜单作用于指针下的单词。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：会话不存在返回 `NoSuchSession`；纯 SFTP 返回 `NotSupported`。
    /// - 规格：terminal.md §8.1。实现：T4 + I1。
    pub fn select_word_at(&self, session: SessionId, at: ViewportPoint) -> Result<()> {
        dispatch!(self, select_word_at(session, at))
    }

    /// 全选（含回滚缓冲区）。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：同 `select_word_at`。
    /// - 规格：terminal.md §8.1。实现：T4 + I1。
    pub fn select_all(&self, session: SessionId) -> Result<()> {
        dispatch!(self, select_all(session))
    }

    /// 清除选区。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：同 `select_word_at`。
    /// - 实现：T4 + I1。
    pub fn clear_selection(&self, session: SessionId) -> Result<()> {
        dispatch!(self, clear_selection(session))
    }

    /// 清空缓冲区（⌘K）：清空屏幕、回滚缓冲和行元数据。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：同 `select_word_at`。
    /// - 终端因此变脏，会触发 `TerminalDirty`。
    /// - 规格：terminal.md §3。实现：T4 + I1。
    pub fn clear_buffer(&self, session: SessionId) -> Result<()> {
        dispatch!(self, clear_buffer(session))
    }

    /// 由帧里的超链接 id（OSC 8）取 URL；id 无效或会话不存在为 `None`。
    ///
    /// - 线程：任意。阻塞：同步，极短。
    /// - 规格：terminal.md §10。实现：T4。
    pub fn hyperlink_url(&self, session: SessionId, id: u32) -> Option<String> {
        dispatch!(self, hyperlink_url(session, id))
    }

    // ---- 搜索 ----

    /// 开始或更新搜索。增量：查询文本变化时保持当前匹配项尽量不跳动。
    ///
    /// - 线程：任意。阻塞：同步，但应只扫描可见部分并限制耗时；全量扫描在后台完成后更新计数。
    /// - 出错：会话不存在返回 `NoSuchSession`；纯 SFTP 返回 `NotSupported`；
    ///   非法正则返回 `User(..)`。
    /// - 规格：terminal.md §11。实现：T4。
    pub fn search(&self, session: SessionId, query: SearchQuery) -> Result<SearchStatus> {
        dispatch!(self, search(session, query))
    }

    /// 跳到下一个（`backwards` 为真则上一个）匹配项，到头循环。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：同 `search`。
    /// - 规格：terminal.md §11。实现：T4。
    pub fn search_next(&self, session: SessionId, backwards: bool) -> Result<SearchStatus> {
        dispatch!(self, search_next(session, backwards))
    }

    /// 只读取随终端输出增量维护的搜索状态。
    pub fn search_status(&self, session: SessionId) -> Result<SearchStatus> {
        dispatch!(self, search_status(session))
    }

    /// 关闭搜索并清除高亮。会话不存在时忽略。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：无。
    /// - 规格：terminal.md §11。实现：T4。
    pub fn search_close(&self, session: SessionId) {
        dispatch!(self, search_close(session))
    }

    // ---- 显示 ----

    /// 设置时间戳列的显示模式。`session` 为 `None` 表示全局；`Some` 只对该会话生效，
    /// 用于 AI CLI 会话的临时设置（terminal.md §2）。
    ///
    /// - 线程：任意。阻塞：同步，极短。出错：会话不存在时忽略。
    /// - 实现：F5。
    pub fn set_timestamp_mode(&self, session: Option<SessionId>, mode: TimestampMode) {
        dispatch!(self, set_timestamp_mode(session, mode))
    }

    /// 下发终端显示选项（回滚行数上限、默认光标样式），对所有现有会话立即生效，之后新开的会话也使用它
    /// （terminal.md §15）。进程启动后默认值为 [`TerminalOptions::default`]；宿主要在读到用户设置后调用一次，
    /// 之后每次设置变化再调用。
    ///
    /// - 回滚上限调小时，超出的最老的行立即丢弃；程序用 DECSCUSR 指定过的光标样式不受影响，
    ///   DECSCUSR 0 回到这里设置的默认样式。
    /// - 光标闪烁只体现在帧的 `FRAME_CURSOR_BLINK` 标志上，闪烁动画由渲染层负责。
    /// - 线程：任意。阻塞：同步，持各会话终端锁的时间很短（丢弃回滚是线性的，上限内可接受）。
    pub fn set_terminal_options(&self, options: TerminalOptions) {
        dispatch!(self, set_terminal_options(options))
    }

    // ---- 帧（数据面）----

    /// 取最新帧。
    ///
    /// 约定：只有终端自上次取帧后变脏时才返回 `Some`，并清除脏标记；之后的新输出会再次
    /// 触发 `TerminalDirty`。没有变化、会话不存在或是纯 SFTP 会话时返回 `None`。
    ///
    /// **`reuse` 的所有权约定**：`reuse` 里放调用方不再需要的上一帧，供复用其缓冲区、避免每帧分配。
    /// - 返回 `Some`（有新帧）时，`reuse` 里的帧被**取走**（复用其缓冲区，调用后 `*reuse` 为 `None`）；
    /// - 返回 `None`（没有新帧）时，`reuse` **原样保留**，调用方可以继续使用或下次再传。
    ///
    /// 为什么用 `&mut Option<Frame>` 而不是按值传入：按值传入时，"没有新帧"的分支也已经把缓冲区
    /// 吞掉了；而渲染视图在没有新帧时仍要重绘旧帧（窗口缩放等），旧帧必须还在。让 core 在确定产出
    /// 新帧之后才取走 `reuse`，调用方就不需要自己再绕一层"备用槽"（docs/frame-abi.md §8）。
    ///
    /// - 线程：可在渲染线程调用。阻塞：内部持锁时间不超过 1 ms（architecture §4）。
    /// - 规格：docs/frame-abi.md。实现：T4（产帧）+ I1（接线）。
    pub fn take_frame(&self, session: SessionId, reuse: &mut Option<Frame>) -> Option<Frame> {
        dispatch!(self, take_frame(session, reuse))
    }

    // ---- 提问 ----

    /// 回答一个 `Prompt` 事件。未知或已过期的 `prompt` 静默忽略；
    /// 应答类型与请求不匹配按 `Cancel` 处理。
    ///
    /// - 线程：任意。阻塞：不阻塞。出错：无。
    /// - 规格：ssh.md §3、§4。实现：I2（认证提问与主机密钥变更；ZMODEM 相关提问待 F4）。
    pub fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
        dispatch!(self, respond_prompt(prompt, response))
    }

    // ---- 工作目录 ----

    /// 会话当前工作目录（⌘J）。优先用 OSC 7，其次系统（本地）或服务器（SSH）查询。
    ///
    /// - 线程：任意。阻塞：异步，远端查询可能耗时，内部有超时。
    /// - 出错：拿不到时返回 `User(Cwd*)`（`CwdNoShell`、`CwdTimeout` 等）；会话不存在返回
    ///   `NoSuchSession`；纯 SFTP 返回 `NotSupported`。
    /// - SSH 会话没有 OSC 7 等上报时，在同一连接上另开 exec 通道查询（5 秒超时，不碰正在用的 shell 通道）。
    /// - 规格：ssh.md §10、local-shell.md §6。实现：I1（本地）、I2（SSH 的 OSC 7 部分）、T6b（远端 exec 查询）。
    pub async fn working_directory(&self, session: SessionId) -> Result<String> {
        dispatch_async!(self, working_directory(session))
    }

    // ---- SFTP（会话为 SSH 或 SFTP 类型）----
    //
    // 本组方法对本地会话返回 `NotSupported`，对不存在的会话返回 `NoSuchSession`；
    // 操作失败返回 `User(SftpOpFailed / SftpSubsystemFailed)`。规格：ssh.md §8、§9。

    /// 远端用户主目录。
    ///
    /// - 线程：任意。阻塞：异步，需要一次远端往返。实现：T7b。
    pub async fn sftp_home(&self, session: SessionId) -> Result<String> {
        dispatch_async!(self, sftp_home(session))
    }

    /// 查询远端文件版本；异步执行一次 STAT，跟随符号链接。
    pub async fn sftp_stat(
        &self,
        session: SessionId,
        path: &str,
    ) -> Result<kanso_types::fs::FileStat> {
        dispatch_async!(self, sftp_stat(session, path))
    }

    /// 列出远端目录，流式返回。
    ///
    /// - 线程：任意。阻塞：本调用本身不阻塞，读取在 [`DirStream::next_batch`] 里进行。
    /// - 实现：T7b（接线）、F1（Filer 使用）。
    pub fn sftp_list(&self, session: SessionId, path: &str) -> Result<DirStream> {
        dispatch!(self, sftp_list(session, path))
    }

    /// 创建远端目录。
    ///
    /// - 线程：任意。阻塞：异步，一次远端往返。实现：T7b。
    pub async fn sftp_mkdir(&self, session: SessionId, path: &str) -> Result<()> {
        dispatch_async!(self, sftp_mkdir(session, path))
    }

    /// 删除远端文件或目录；`is_dir` 为真时递归删除目录。
    ///
    /// - 线程：任意。阻塞：异步，递归删除可能耗时较长。实现：T7b。
    pub async fn sftp_remove(&self, session: SessionId, path: &str, is_dir: bool) -> Result<()> {
        dispatch_async!(self, sftp_remove(session, path, is_dir))
    }

    /// 重命名 / 移动远端条目。
    ///
    /// - 线程：任意。阻塞：异步，一次远端往返。实现：T7b。
    pub async fn sftp_rename(&self, session: SessionId, from: &str, to: &str) -> Result<()> {
        dispatch_async!(self, sftp_rename(session, from, to))
    }

    /// 创建空的远端文件；已存在则失败（`User(SftpFileExists)`），不会覆盖。
    ///
    /// - 线程：任意。阻塞：异步，一次远端往返。实现：T7b。
    pub async fn sftp_create_file(&self, session: SessionId, path: &str) -> Result<()> {
        dispatch_async!(self, sftp_create_file(session, path))
    }

    /// 下载远端文件或目录到本地目录 `local_dir`。立即返回传输 id，
    /// 进度通过 `TransferStarted / TransferProgress / TransferFinished` 事件推送。
    ///
    /// `conflict` 决定目标已存在时的行为（`ConflictPolicy`）：
    /// - `Fail`：不覆盖，传输以 `TransferFinished(Failed { already_exists: true, .. })` 结束（目录传输在写任何文件之前
    ///   检查本地冲突；上传的远端冲突只能传到该文件时才发现）。UI 先用它发起，收到 `already_exists` 后弹
    ///   "覆盖 / 跳过 / 取消"，再用对应策略重试；
    /// - `Overwrite`：覆盖已存在的文件；
    /// - `Skip`：已存在的文件跳过、其余照传，以 `Completed { skipped }` 结束并带回跳过数。
    ///   目录传输总是合并，策略只作用于其中的同名文件。
    ///
    /// - 线程：任意。阻塞：不阻塞。出错：本调用只报参数类错误（会话不存在 `NoSuchSession`、
    ///   本地会话 `NotSupported`、本地目录不是目录等），此时不发任何事件；传输失败走 `TransferFinished(Failed)`。
    /// - 事件：目录传输的总字节数来自扫描计划，扫描完成前 `TransferStarted.total_bytes` 为 `None`，
    ///   `TransferProgress.total_bytes` 在计划定稿后给出；单文件的 `TransferStarted` 已带总大小。
    /// - 实现：T7b（接线）、F1。
    pub fn download(
        &self,
        session: SessionId,
        remote_path: &str,
        local_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferId> {
        dispatch!(self, download(session, remote_path, local_dir, conflict))
    }

    /// 上传本地文件或目录到远端目录 `remote_dir`。事件与错误约定同 [`Core::download`]（含 `conflict`）。
    ///
    /// - 线程：任意。阻塞：不阻塞。实现：T7b、F1。
    pub fn upload(
        &self,
        session: SessionId,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferId> {
        dispatch!(self, upload(session, local_path, remote_dir, conflict))
    }

    /// 发起下载并等它结束（MCP 用）。规则与事件同 [`Core::download`]：传输照常出现在传输列表里、照常发事件。
    ///
    /// - 参数类错误与 `download` 一样立即返回 `Err`；传输本身的结果（含失败、取消）在 `Ok(TransferOutcome)` 里。
    /// - 丢弃返回的 future **不会**取消传输（spec/mcp.md §4.4：客户端断开时传输继续完成，由用户决定是否取消）。
    /// - 等待期间可以用 [`Core::cancel_transfer`] 取消，此时返回 `Cancelled`。
    /// - 线程：任意。阻塞：异步，耗时同传输本身。出错：运行时关闭导致传输任务被销毁时返回 `Internal`。
    pub async fn download_and_wait(
        &self,
        session: SessionId,
        remote_path: &str,
        local_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferOutcome> {
        dispatch_async!(
            self,
            download_and_wait(session, remote_path, local_dir, conflict)
        )
    }

    /// 发起上传并等它结束（MCP 用）。约定同 [`Core::download_and_wait`]。
    pub async fn upload_and_wait(
        &self,
        session: SessionId,
        local_path: &str,
        remote_dir: &str,
        conflict: ConflictPolicy,
    ) -> Result<TransferOutcome> {
        dispatch_async!(
            self,
            upload_and_wait(session, local_path, remote_dir, conflict)
        )
    }

    /// 取消传输（SFTP 传输）；结束时发 `TransferFinished(Cancelled)`，并清理残缺文件。
    /// 未知或已结束的传输忽略。
    ///
    /// - 线程：任意。阻塞：不阻塞。出错：无。
    /// - 规格：ssh.md §9。实现：T7b。
    pub fn cancel_transfer(&self, transfer: TransferId) {
        dispatch!(self, cancel_transfer(transfer))
    }

    // ---- 终端内传输 ----
    //
    // ZMODEM 由 core 自动检测，不需要启动接口（encoding-transfer.md §2）。

    /// 通过终端用 XMODEM 发送本地文件 `path`。立即返回传输 id，进度走传输事件。
    ///
    /// - 线程：任意。阻塞：不阻塞。出错：会话不存在返回 `NoSuchSession`；纯 SFTP 返回 `NotSupported`。
    /// - 传输期间该会话的输入被丢弃（terminal.md §13）。
    /// - 规格：encoding-transfer.md §3。实现：F4。
    pub fn xmodem_send(
        &self,
        session: SessionId,
        path: &str,
        block: XmodemBlock,
    ) -> Result<TransferId> {
        dispatch!(self, xmodem_send(session, path, block))
    }

    /// 通过终端用 XMODEM 接收文件，保存到本地 `path`。约定同 [`Core::xmodem_send`]。
    ///
    /// - 规格：encoding-transfer.md §3。实现：F4。
    pub fn xmodem_receive(&self, session: SessionId, path: &str) -> Result<TransferId> {
        dispatch!(self, xmodem_receive(session, path))
    }

    /// 启动 XMODEM 并询问文件路径；block 为 None 时接收，否则按所选块大小发送。
    /// 对话框期间独占并暂存原始字节，取消选择后恢复终端输出。
    pub fn xmodem_start(
        &self,
        session: SessionId,
        block: Option<XmodemBlock>,
    ) -> Result<TransferId> {
        dispatch!(self, xmodem_start(session, block))
    }

    /// 取消该会话正在进行的终端内传输（ZMODEM / XMODEM）；没有进行中的传输时忽略。
    ///
    /// - 线程：任意。阻塞：不阻塞。出错：无。
    /// - 规格：encoding-transfer.md §2、§3。实现：F4。
    pub fn cancel_terminal_transfer(&self, session: SessionId) {
        dispatch!(self, cancel_terminal_transfer(session))
    }

    // ---- 导入导出 ----

    /// 导出全部会话、分组、Sender 命令到 `.kanso` 文件。**不含任何凭据**。
    ///
    /// - 线程：任意。阻塞：异步，写磁盘。出错：写文件失败返回 `User(..)`。
    /// - 规格：store-import.md §6.1。实现：F6（UI）+ T8（逻辑）。
    pub async fn export_data(&self, path: &str) -> Result<()> {
        dispatch_async!(self, export_data(path))
    }

    /// 从 `.kanso` 文件导入。
    ///
    /// - 线程：任意。阻塞：异步，读磁盘。
    /// - 出错：扩展名不对、不是 Kanso 文件、版本过新分别返回 `User(ImportWrongExtension /
    ///   ImportNotKansoFile / ImportVersionTooNew)`；单条无效不整体失败，记入 `skipped`。
    /// - 规格：store-import.md §6.2、§6.3。实现：F6 + T8。
    pub async fn import_data(&self, path: &str) -> Result<ImportReport> {
        dispatch_async!(self, import_data(path))
    }

    /// 扫描 OpenSSH 配置，列出可导入的主机。`path` 为 `None` 用默认的 `~/.ssh/config`。
    ///
    /// - 线程：任意。阻塞：异步，读磁盘。出错：文件不可读返回 `User(ImportReadFailed)`。
    /// - 返回：候选列表与文件级警告（`Include` 嵌套超限、`Match` 不支持等）；
    ///   只与某个别名相关的警告在候选自己的 `warnings` 里。
    /// - 规格：store-import.md §7.1、§7.2。实现：T3（解析）+ F6。
    pub async fn scan_ssh_config(&self, path: Option<&str>) -> Result<SshConfigScan> {
        dispatch_async!(self, scan_ssh_config(path))
    }

    /// 导入所选别名（`aliases`）到分组 `group`（`None` 为顶层）。
    ///
    /// `path` 与 [`scan_ssh_config`](Self::scan_ssh_config) 一致，`None` 用默认的 `~/.ssh/config`；
    /// 调用方应传扫描时用的同一个路径。每次调用都重新读取并求值，不依赖扫描结果（文件可能已变）。
    ///
    /// - 线程：任意。阻塞：异步。
    /// - 出错：分组不存在返回 `NoSuchGroup`；文件不可读返回 `User(ImportReadFailed)`。
    /// - 全部写入在一个事务里。
    /// - 规格：store-import.md §7.3。实现：T3 + F6。
    pub async fn import_ssh_config(
        &self,
        path: Option<&str>,
        aliases: &[String],
        group: Option<GroupId>,
    ) -> Result<SshConfigImportReport> {
        dispatch_async!(self, import_ssh_config(path, aliases, group))
    }

    // ---- MCP：授权与审计（spec/mcp.md §1、§2、§6）----
    //
    // 数据在本机专用表里（schema v3），不随 `.kanso` 导出，也不参与同步。

    /// MCP 总开关；从未设置过为关闭。
    ///
    /// - 线程：任意。阻塞：同步，极短。读失败记日志并按关闭处理（宁可保守）。
    pub fn mcp_enabled(&self) -> bool {
        dispatch!(self, mcp_enabled())
    }

    /// 设置 MCP 总开关，成功后通知 [`Core::mcp_policy_watch`] 的订阅者。
    ///
    /// 只改开关本身，不改各会话的级别（spec/mcp.md §2.1）。
    /// - 线程：任意。阻塞：同步，可能触及磁盘。出错：存储失败返回 `Internal`。
    pub fn set_mcp_enabled(&self, enabled: bool) -> Result<()> {
        dispatch!(self, set_mcp_enabled(enabled))
    }

    /// 会话配置的 MCP 访问级别；没有授权、会话不存在或已删除都是 `Off`。
    ///
    /// **不含总开关**：总开关关闭时调用方应当把所有会话都按 `Off` 处理（spec/mcp.md §2.1）。
    /// - 线程：任意。阻塞：同步，极短。读失败记日志并按 `Off` 处理。
    pub fn mcp_access(&self, profile: ProfileId) -> McpAccess {
        dispatch!(self, mcp_access(profile))
    }

    /// 同 [`Core::mcp_access`]，但读库失败时返回错误而不是按 `Off` 处理。
    ///
    /// 给界面用：编辑对话框等要把"读不到"如实告诉用户，不能显示成"关闭"让用户基于错误的现状修改。
    /// 授权判断（MCP 服务）仍用 [`Core::mcp_access`]，读失败按 `Off` 处理更保守。
    /// - 出错：存储读失败返回 `Internal`。线程：任意。阻塞：同步，极短。
    pub fn try_mcp_access(&self, profile: ProfileId) -> Result<McpAccess> {
        dispatch!(self, try_mcp_access(profile))
    }

    /// 全部不是 `Off` 的授权（设置页列表用）。
    ///
    /// - 出错：存储读失败返回 `Internal`。**调用方不得把错误当成"全部关闭"**：授权状态未知时，
    ///   展示层应显示"读取失败"，MCP 服务应拒绝服务，否则会误导用户或泄露本应隐藏的会话。
    pub fn mcp_accesses(&self) -> Result<Vec<(ProfileId, McpAccess)>> {
        dispatch!(self, mcp_accesses())
    }

    /// 设置会话配置的访问级别，`Off` 即撤销。成功后通知 [`Core::mcp_policy_watch`] 的订阅者。
    ///
    /// - 线程：任意。阻塞：同步，极短。
    /// - 出错：会话配置不存在返回 `NoSuchProfile`；存储失败返回 `Internal`。
    pub fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> Result<()> {
        dispatch!(self, set_mcp_access(profile, level))
    }

    /// 订阅授权变化。值是一个只增的计数，变化即表示总开关、某个会话的级别或会话的存在性变了；
    /// 订阅者收到后重新读取，并中断已经不再被允许的调用（spec/mcp.md §2.1、§4.4）。
    ///
    /// - 线程：任意。接收端可以在任意 tokio 运行时里 `changed().await`。
    pub fn mcp_policy_watch(&self) -> tokio::sync::watch::Receiver<u64> {
        self.mcp_policy.subscribe()
    }

    /// 追加审计记录（一批一个事务）。
    ///
    /// - 线程：任意；调用方应在后台批量调用，不要每次工具调用都同步落盘（architecture §16.4）。
    /// - 出错：存储失败返回 `Internal`。
    pub fn mcp_audit_append(&self, entries: Vec<McpAuditEntry>) -> Result<()> {
        dispatch!(self, mcp_audit_append(entries))
    }

    /// 查询审计记录，按 id 倒序。出错：存储失败返回 `Internal`。
    pub fn mcp_audit(&self, filter: McpAuditFilter) -> Result<Vec<McpAuditRecord>> {
        dispatch!(self, mcp_audit(filter))
    }

    /// 清空审计日志。出错：存储失败返回 `Internal`。
    pub fn mcp_audit_clear(&self) -> Result<()> {
        dispatch!(self, mcp_audit_clear())
    }

    /// 按保留规则清理审计日志（30 天、10000 条），返回删除条数。`now_ms` 为当前 Unix 毫秒，
    /// 由调用方传入，便于测试。出错：存储失败返回 `Internal`。
    pub fn mcp_audit_prune(&self, now_ms: i64) -> Result<u32> {
        dispatch!(self, mcp_audit_prune(now_ms))
    }

    // ---- MCP：终端、连接与执行（spec/mcp.md §3、§4，architecture §16.3）----
    //
    // 授权判断、标签写锁（同一标签同时只允许一个 MCP 写操作）、"客户端所在标签"与
    // 用户接管后的 10 秒冷却都由 `kanso-mcp-server` 负责；这里只提供能力和 core 才知道的状态。
    //
    // async 方法都是**取消安全**的：调用方丢弃 future 即停止等待（级别下调、客户端断开时就这么做），
    // 已经写入终端或发到远端的内容不撤回；`kanso_terminal_run` 的命令登记也不撤销（命令已经发给
    // shell，提示符返回时与用户亲手输入的命令一样被清除），Sender 占用（`McpClaim`）随 `Drop` 释放。

    /// MCP 用的会话状态；会话不存在为 `None`。无界面会话也能查到（`headless` 为真）。
    ///
    /// - 线程：任意。阻塞：同步，极短。
    /// - 真实后端：本地与 SSH 会话已实现；纯 SFTP 会话没有终端，终端相关字段取空值。
    pub fn mcp_session_status(&self, session: SessionId) -> Option<McpSessionStatus> {
        let mut status = dispatch!(self, mcp_session_status(session))?;
        // Sender 的任务状态在 Core 层，不在后端。
        status.sender_active = self
            .sender
            .lock()
            .expect("Sender 锁未中毒")
            .is_sending_to(session);
        Some(status)
    }

    /// 读取终端文本（`kanso_terminal_read`）。超过 `max_bytes` 时保留末尾。
    ///
    /// - 线程：任意。阻塞：同步，持终端锁的时间与范围成正比，`Recent` 的行数由调用方限制在 2000 以内。
    /// - 出错：会话不存在返回 `NoSuchSession`；纯 SFTP 与无界面会话返回 `NotSupported`。
    /// - 规格：spec/mcp.md §3.2、§5。
    pub fn terminal_text(
        &self,
        session: SessionId,
        scope: TextScope,
        max_bytes: usize,
    ) -> Result<TerminalText> {
        dispatch!(self, terminal_text(session, scope, max_bytes))
    }

    /// 在标签里输入一行命令并回车，等它返回提示符（`kanso_terminal_run`）。
    ///
    /// - 前置条件（spec/mcp.md §4.2）在 core 内与发送**在同一临界区**检查，不满足返回
    ///   `Ok(Rejected(..))`，什么也不发送；含换行的命令返回 `NotSupported`。
    ///   Sender 正在发送的判断在 Core 层完成，后端只检查其余条件。
    /// - 完成判断复用 Sender 的"登记即将发送 + 等待提示符"（terminal.md §5.3）。
    /// - 超时返回 `Unconfirmed`，不发送 Ctrl+C；用户在等待期间输入返回 `TakenOver`。
    /// - 线程：任意。阻塞：异步，最长 `limits.timeout`。
    /// - 出错：会话不存在返回 `NoSuchSession`；等待期间会话结束（标签关闭、断开、锁定）同样返回
    ///   `NoSuchSession`；纯 SFTP 与无界面会话返回 `NotSupported`。
    pub async fn mcp_terminal_run(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<TerminalRunOutcome> {
        // 在调度器的锁内同时完成"Sender 是否在发送"的检查和占用登记；Sender 的 tick 在同一把锁下
        // 看到占用后不会再向这个标签发内容（见 `Scheduler::mcp_claim`）。
        let claimed = self
            .sender
            .lock()
            .expect("Sender 锁未中毒")
            .mcp_claim(session);
        if !claimed {
            let reason = if self
                .sender
                .lock()
                .expect("Sender 锁未中毒")
                .is_sending_to(session)
            {
                crate::mcp::TerminalBusy::SenderActive
            } else {
                // 另一条 MCP 命令正占用这个标签；正常情况下 kanso-mcp-server 的标签写锁会先挡住。
                crate::mcp::TerminalBusy::CommandRunning
            };
            return Ok(TerminalRunOutcome::Rejected(reason));
        }
        // 用 Drop 释放：调用方丢弃 future（级别下调、客户端断开）时也必须释放，否则 Sender 会一直认为这个标签忙。
        let _claim = McpClaim {
            sender: &self.sender,
            session,
        };
        dispatch_async!(self, mcp_terminal_run(session, command, limits))
    }

    /// 向标签输入文字和按键（`kanso_terminal_send`）。`settle` 为 `Some` 时，发送后等待终端
    /// 安静这么久（调用方夹到 10 秒以内）再返回屏幕；等待期间用户输入返回 `TakenOver`。
    ///
    /// - 只检查未连接（含会话结束后的锁定）与终端内传输两项（见 [`TerminalSendOutcome::Rejected`]）。
    /// - 线程：任意。阻塞：异步，最长约 `settle`。
    /// - 出错：同 [`Core::mcp_terminal_run`]；等待安静期间会话结束同样返回 `NoSuchSession`。
    pub async fn mcp_terminal_send(
        &self,
        session: SessionId,
        inputs: Vec<McpInput>,
        settle: Option<std::time::Duration>,
        max_bytes: usize,
    ) -> Result<TerminalSendOutcome> {
        dispatch_async!(self, mcp_terminal_send(session, inputs, settle, max_bytes))
    }

    /// 为会话配置取得一个无界面会话（见 [`crate::mcp`] 模块文档），供后台执行与 SFTP 使用。
    ///
    /// - 复用：同一配置有多条存活的已认证连接时取最近建立的那条；复用用户的连接为借用，
    ///   用户关闭该连接上最后一个标签时 core 关闭这个无界面会话（spec/mcp.md §2.4）。
    /// - `NonInteractive` 新建连接时不发 `Prompt`；需要人参与即放弃并返回 `NeedsUser`。
    /// - 自建连接的 5 分钟空闲释放由调用方负责：到期调用 [`Core::close_session`]。
    /// - 线程：任意。阻塞：异步，新建连接时包含握手与认证。
    /// - 出错：配置不存在返回 `NoSuchProfile`；本地配置返回 `NotSupported`；
    ///   连接失败（网络、认证被拒）返回 `User(..)`，消息同普通连接。
    /// - 实现：F9。
    pub async fn mcp_connect(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
    ) -> Result<McpConnectOutcome> {
        dispatch_async!(self, mcp_connect(profile, mode))
    }

    /// 打开一个可见标签（`kanso_open_tab`），等连接建立或失败后返回。
    ///
    /// 有已认证连接时复用（相当于复制标签），否则按 `mode` 新建，规则同 [`Core::mcp_connect`]。
    /// core 不通知 UI 摆放标签：由 `kanso-mcp-server` 通过 `McpServer` 的回调通知宿主
    /// （不抢焦点，spec/mcp.md §3.3）。返回的会话与普通标签完全相同，会正常出现在 `sessions()` 里。
    /// - 出错：同 [`Core::mcp_connect`]；本地配置可以打开（不涉及认证）。实现：F9。
    pub async fn mcp_open_session(
        &self,
        profile: ProfileId,
        mode: ConnectMode,
        size: GridSize,
    ) -> Result<McpConnectOutcome> {
        dispatch_async!(self, mcp_open_session(profile, mode, size))
    }

    /// 在会话所在的 SSH 连接上开 exec 通道执行一条命令（`kanso_exec`），不请求 PTY。
    ///
    /// - `session` 可以是 SSH 标签，也可以是 [`Core::mcp_connect`] 得到的无界面会话。
    /// - 同一连接上同时进行的执行最多 4 个，超出的排队，排队时间计入超时。
    /// - 超时关闭通道并返回 `timed_out`；远端进程可能仍在运行。
    /// - 线程：任意。阻塞：异步，最长 `limits.timeout`。
    /// - 出错：会话不存在返回 `NoSuchSession`；本地会话返回 `NotSupported`；
    ///   连接已断开返回 `User(..)`。实现：F9。
    pub async fn mcp_exec(
        &self,
        session: SessionId,
        command: &str,
        limits: WaitLimits,
    ) -> Result<ExecOutcome> {
        dispatch_async!(self, mcp_exec(session, command, limits))
    }

    // ---- SFTP 补充（spec/mcp.md §3.5；Filer 也可以使用）----

    /// 查看单个远端路径的属性；`follow_symlinks` 为假时查看链接本身（LSTAT）。
    ///
    /// - 线程：任意。阻塞：异步，一次远端往返。出错：同本组其他 SFTP 方法。实现：F9。
    pub async fn sftp_entry(
        &self,
        session: SessionId,
        path: &str,
        follow_symlinks: bool,
    ) -> Result<DirEntry> {
        dispatch_async!(self, sftp_entry(session, path, follow_symlinks))
    }

    /// 读取远端文件 `[offset, offset + len)`；到文件末尾时返回的字节少于 `len`。
    ///
    /// - 线程：任意。阻塞：异步。调用方负责限制 `len`（spec/mcp.md §3.5：单次最多 1 MiB）。
    /// - 实现：F9。
    pub async fn sftp_read(
        &self,
        session: SessionId,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>> {
        dispatch_async!(self, sftp_read(session, path, offset, len))
    }

    /// 写远端文件，规则见 [`SftpWriteMode`]。
    ///
    /// - 线程：任意。阻塞：异步。调用方负责限制大小（spec/mcp.md §3.5：单次最多 8 MiB）。
    /// - 出错：`CreateNew` 且已存在返回 `User(SftpFileExists)`；`Overwrite` 而服务器没有
    ///   `posix-rename@openssh.com` 返回 `NotSupported`；其他失败返回 `User(SftpOpFailed)`，
    ///   残留的临时文件路径写在消息的 detail 里。实现：F9。
    pub async fn sftp_write(
        &self,
        session: SessionId,
        path: &str,
        data: Vec<u8>,
        mode: SftpWriteMode,
    ) -> Result<()> {
        dispatch_async!(self, sftp_write(session, path, data, mode))
    }

    /// 删除远端**空**目录，不递归（MCP 不提供递归删除，spec/mcp.md §3.5）。
    ///
    /// - 线程：任意。阻塞：异步，一次远端往返。实现：F9。
    pub async fn sftp_rmdir(&self, session: SessionId, path: &str) -> Result<()> {
        dispatch_async!(self, sftp_rmdir(session, path))
    }

    // ---- 授权 ----

    /// 当前授权状态；未授权返回 `None`。
    ///
    /// - 线程：任意。阻塞：同步，极短（读缓存）。出错：不会失败。
    /// - 实现：T11 / I5。
    pub fn entitlement(&self) -> Option<Entitlement> {
        dispatch!(self, entitlement())
    }

    /// 用 License Key 激活（仅 Direct 渠道）。成功返回新的授权。
    ///
    /// - 线程：任意。阻塞：异步，可能联网。出错：Key 无效 / 网络失败返回 `User(..)`。
    /// - 实现：T11 / I5。
    pub async fn activate_license(&self, key: &str) -> Result<Entitlement> {
        dispatch_async!(self, activate_license(key))
    }

    /// 发起商店购买（MAS）。
    ///
    /// - 线程：任意。阻塞：异步，等待系统购买流程。
    /// - 出错：购买提供方不可用（Direct 渠道）返回 `NotSupported`。
    /// - 实现：I5。
    pub async fn purchase(&self) -> Result<PurchaseOutcome> {
        dispatch_async!(self, purchase())
    }

    /// 恢复购买；没有可恢复的购买返回 `Ok(None)`。
    ///
    /// - 线程：任意。阻塞：异步，联网。出错：同 `purchase`。
    /// - 实现：I5。
    pub async fn restore_purchases(&self) -> Result<Option<Entitlement>> {
        dispatch_async!(self, restore_purchases())
    }
}
