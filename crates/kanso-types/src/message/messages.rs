//! 消息清单。新增消息只需在 `messages!` 里加一行，key、参数表与目录自动同步。

use serde::{Deserialize, Serialize};

use super::args::*;
use crate::profile::AuthMethod;
use crate::transfer::XferProtocol;

/// 用一份清单同时生成 `Message` 枚举、`key()`、`args()` 与 `CATALOG`，
/// 三者不可能互相漂移。变体文档注释写触发场景与出处章节。
macro_rules! messages {
    ($(
        $(#[$meta:meta])*
        $name:ident = $key:literal { $($field:ident : $ty:ty),* $(,)? }
    ),+ $(,)?) => {
        /// 用户可见消息：key 加命名参数。
        ///
        /// core 不拼用户可见的句子，只返回 `Message`；各平台 UI 用 key 查本地化目录
        /// （`locales/<lang>.ftl`，Fluent）。日志另走英文，不用 `Message`。
        ///
        /// key 命名规则：`<领域>-<名称>`，全小写 kebab-case，只含 `[a-z0-9-]`，以字母开头；
        /// 参数名用 snake_case。同名参数在不同消息里类型必须一致（有单元测试守护）。
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum Message {
            $(
                $(#[$meta])*
                $name {
                    $(
                        #[doc = concat!("参数 `", stringify!($field), "`。")]
                        $field: $ty,
                    )*
                },
            )+
        }

        impl Message {
            /// 本地化 key。
            pub const fn key(&self) -> &'static str {
                match self {
                    $(Message::$name { .. } => $key,)+
                }
            }

            /// 命名参数，按声明顺序。
            #[allow(clippy::vec_init_then_push, unused_variables)]
            pub fn args(&self) -> Vec<(&'static str, MessageArg)> {
                match self {
                    $(
                        Message::$name { $($field),* } => {
                            vec![$((stringify!($field), IntoArg::to_arg($field))),*]
                        }
                    )+
                }
            }
        }

        /// 全部消息的规格，供多语言校验工具检查 key 是否齐全、参数名是否一致。
        pub const CATALOG: &[MessageSpec] = &[
            $(
                MessageSpec {
                    key: $key,
                    params: &[
                        $(ParamSpec {
                            name: stringify!($field),
                            kind: <$ty as IntoArg>::KIND,
                        }),*
                    ],
                },
            )+
        ];
    };
}

messages! {
    /// Sender 面板标题。
    UiSenderTitle = "ui-sender-title" {},
    /// Sender：运行。
    UiSenderRun = "ui-sender-run" {},
    /// Sender：停止。
    UiSenderStop = "ui-sender-stop" {},
    /// Sender：清空。
    UiSenderClear = "ui-sender-clear" {},
    /// Sender：重复发送。
    UiSenderRepeat = "ui-sender-repeat" {},
    /// Sender：保存。
    UiSenderSave = "ui-sender-save" {},
    /// Sender：更新。
    UiSenderUpdate = "ui-sender-update" {},
    /// Sender：取消编辑。
    UiSenderCancelEdit = "ui-sender-cancel-edit" {},
    /// Sender：名称框占位。
    UiSenderNamePlaceholder = "ui-sender-name-placeholder" {},
    /// Sender：命令框占位。
    UiSenderInputPlaceholder = "ui-sender-input-placeholder" {},
    /// Sender：重复间隔。
    UiSenderInterval = "ui-sender-interval" {},
    /// Sender：重复次数。
    UiSenderCount = "ui-sender-count" {},
    /// Sender：命令库为空。
    UiSenderLibraryEmpty = "ui-sender-library-empty" {},
    /// Sender：没有适用于当前会话的命令。
    UiSenderLibraryNone = "ui-sender-library-none" {},
    /// Sender：上一页。
    UiSenderPrevPage = "ui-sender-prev-page" {},
    /// Sender：下一页。
    UiSenderNextPage = "ui-sender-next-page" {},
    /// Sender：不追加行尾。
    UiSenderEndingNone = "ui-sender-ending-none" {},
    /// Sender：当前会话目标。
    UiSenderTargetCurrent = "ui-sender-target-current" { count: i64 },
    /// Sender：全部会话目标。
    UiSenderTargetAll = "ui-sender-target-all" { count: i64 },
    /// Sender：运行条里的全部会话。
    UiSenderTargetAllLabel = "ui-sender-target-all-label" {},
    /// Sender：运行条里的当前会话。
    UiSenderTargetCurrentLabel = "ui-sender-target-current-label" { name: String },
    /// Sender：全局范围。
    UiSenderScopeGlobal = "ui-sender-scope-global" {},
    /// Sender：SSH 类型范围。
    UiSenderScopeSsh = "ui-sender-scope-ssh" {},
    /// Sender：本地 Shell 类型范围。
    UiSenderScopeLocal = "ui-sender-scope-local" {},
    /// Sender：分组范围。
    UiSenderScopeGroup = "ui-sender-scope-group" { name: String },
    /// Sender：会话范围。
    UiSenderScopeProfile = "ui-sender-scope-profile" { name: String },
    /// Sender 菜单：编辑。
    UiSenderMenuEdit = "ui-sender-menu-edit" {},
    /// Sender 菜单：重复发送。
    UiSenderMenuRepeat = "ui-sender-menu-repeat" {},
    /// Sender 菜单：行尾。
    UiSenderMenuEnding = "ui-sender-menu-ending" {},
    /// Sender 菜单：作用域。
    UiSenderMenuScope = "ui-sender-menu-scope" {},
    /// Sender 菜单：删除。
    UiSenderMenuDelete = "ui-sender-menu-delete" {},
    /// Sender：无限次的已发轮数。
    UiSenderRounds = "ui-sender-rounds" { rounds: i64 },
    /// Sender：有限次的已发轮数。
    UiSenderRoundsOf = "ui-sender-rounds-of" { rounds: i64, count: i64 },
    /// Sender：正在发送。
    UiSenderSending = "ui-sender-sending" { progress: String },
    /// Sender：倒计时。
    UiSenderNextIn = "ui-sender-next-in" { progress: String, wait: i64 },
    /// Sender：重复任务运行条。
    UiSenderRepeatBar = "ui-sender-repeat-bar" { title: String, interval: i64, dest: String, progress: String },
    /// 状态栏：重复发送运行中（无限次）。
    UiSenderChip = "ui-sender-chip" { rounds: i64 },
    /// 状态栏：重复发送运行中（有限次）。
    UiSenderChipOf = "ui-sender-chip-of" { rounds: i64, count: i64 },
    /// Sender：多条提示之间的分隔。
    UiSenderJoin = "ui-sender-join" {},
    /// Sender：保存失败。
    UiSenderSaveFailed = "ui-sender-save-failed" { detail: String },
    /// Sender：更新失败。
    UiSenderUpdateFailed = "ui-sender-update-failed" { detail: String },
    /// Sender：删除失败。
    UiSenderDeleteFailed = "ui-sender-delete-failed" { detail: String },
    /// Sender：发送失败。
    UiSenderSendFailed = "ui-sender-send-failed" { detail: String },
    /// Sender：命令库已满。
    UiSenderLimit = "ui-sender-limit" { max: i64 },
    /// 补全类别：目录。
    UiCompletionDirectory = "ui-completion-directory" {},
    /// 补全类别：文件。
    UiCompletionFile = "ui-completion-file" {},
    /// 补全类别：历史。
    UiCompletionHistory = "ui-completion-history" {},
    /// 补全类别：Kubernetes。
    UiCompletionKubernetes = "ui-completion-kubernetes" {},
    /// 补全类别：Docker。
    UiCompletionDocker = "ui-completion-docker" {},
    /// 补全类别：Git。
    UiCompletionGit = "ui-completion-git" {},
    /// 补全类别：systemd。
    UiCompletionSystemd = "ui-completion-systemd" {},
    /// 菜单：命令建议。
    UiMenuCommandSuggestions = "ui-menu-command-suggestions" {},
    /// 菜单：清除命令历史。
    UiMenuClearHistory = "ui-menu-clear-history" {},
    /// 菜单：Sender。
    UiMenuSender = "ui-menu-sender" {},
    /// 清除命令历史对话框标题。
    UiHistoryClearTitle = "ui-history-clear-title" {},
    /// 清除命令历史对话框正文。
    UiHistoryClearBody = "ui-history-clear-body" {},
    /// 清除命令历史按钮。
    UiHistoryClearConfirm = "ui-history-clear-confirm" {},
    /// 取消按钮。
    UiHistoryClearCancel = "ui-history-clear-cancel" {},
    /// 清除命令历史失败。
    UiHistoryClearFailed = "ui-history-clear-failed" { detail: String },
    /// GPUI 设置页面：通用。
    UiGeneral = "ui-general" {},
    /// GPUI 设置页面：显示。
    UiDisplay = "ui-display" {},
    /// GPUI 设置页面：外观。
    UiAppearance = "ui-appearance" {},
    /// GPUI 设置页面：模式。
    UiTheme = "ui-theme" {},
    /// GPUI 设置页面：浅色终端配色。
    UiLightTheme = "ui-light-theme" {},
    /// GPUI 设置页面：深色终端配色。
    UiDarkTheme = "ui-dark-theme" {},
    /// GPUI 设置页面：字体。
    UiFont = "ui-font" {},
    /// GPUI 设置页面：终端字体。
    UiTerminalFont = "ui-terminal-font" {},
    /// GPUI 设置页面：字号。
    UiFontSize = "ui-font-size" {},
    /// GPUI 设置页面：行距。
    UiLineHeight = "ui-line-height" {},
    /// GPUI 设置页面：自定义字体。
    UiFontCustom = "ui-font-custom" {},
    /// GPUI 设置页面：留空使用系统等宽字体。字体缺少的图标会自动使用已安装的 Nerd Font。。
    UiFontHint = "ui-font-hint" {},
    /// GPUI 设置页面：正在检测本机等宽字体…。
    UiFontProbing = "ui-font-probing" {},
    /// GPUI 设置页面：光标。
    UiCursor = "ui-cursor" {},
    /// GPUI 设置页面：形状。
    UiShape = "ui-shape" {},
    /// GPUI 设置页面：光标闪烁。
    UiCursorBlink = "ui-cursor-blink" {},
    /// GPUI 设置页面：语义着色。
    UiSemanticColor = "ui-semantic-color" {},
    /// GPUI 设置页面：回滚。
    UiScrollback = "ui-scrollback" {},
    /// GPUI 设置页面：回滚行数。
    UiScrollbackLines = "ui-scrollback-lines" {},
    /// GPUI 设置页面：调小以后立即丢弃超出的最早的行。。
    UiScrollbackHint = "ui-scrollback-hint" {},
    /// GPUI 设置页面：对比度。
    UiContrast = "ui-contrast" {},
    /// GPUI 设置页面：最低对比度保护。
    UiContrastEnabled = "ui-contrast-enabled" {},
    /// GPUI 设置页面：对比度比例。
    UiContrastRatio = "ui-contrast-ratio" {},
    /// GPUI 设置页面：文字与背景的对比度低于此比例时，会自动调亮或调暗文字。。
    UiContrastHint = "ui-contrast-hint" {},
    /// GPUI 设置页面：时间轴。
    UiTimeline = "ui-timeline" {},
    /// GPUI 设置页面：界面字体。
    UiInterfaceFont = "ui-interface-font" {},
    /// GPUI 设置页面：设置。
    UiSettings = "ui-settings" {},
    /// GPUI 会话页面：会话。
    UiSession = "ui-session" {},
    /// GPUI 会话页面：SSH 连接。
    UiSshConnection = "ui-ssh-connection" {},
    /// GPUI 会话页面：本地 Shell。
    UiLocalSession = "ui-local-session" {},
    /// GPUI 会话页面：新建会话。
    UiNewSession = "ui-new-session" {},
    /// GPUI 会话页面：编辑会话。
    UiEditSession = "ui-edit-session" {},
    /// GPUI 会话页面：类型。
    UiType = "ui-type" {},
    /// GPUI 会话页面：名称。
    UiName = "ui-name" {},
    /// GPUI 会话页面：颜色。
    UiColor = "ui-color" {},
    /// GPUI 会话页面：分组。
    UiGroup = "ui-group" {},
    /// GPUI 会话页面：起始目录。
    UiStartDirectory = "ui-start-directory" {},
    /// GPUI 会话页面：主机。
    UiHost = "ui-host" {},
    /// GPUI 会话页面：端口。
    UiPort = "ui-port" {},
    /// GPUI 会话页面：用户名。
    UiUser = "ui-user" {},
    /// GPUI 会话页面：认证方式。
    UiAuthentication = "ui-authentication" {},
    /// GPUI 会话页面：密码。
    UiPassword = "ui-password" {},
    /// GPUI 会话页面：私钥。
    UiPrivateKey = "ui-private-key" {},
    /// GPUI 会话页面：私钥口令。
    UiKeyPassphrase = "ui-key-passphrase" {},
    /// GPUI 会话页面：跳板机。
    UiJumpHost = "ui-jump-host" {},
    /// GPUI 会话页面：编码。
    UiEncoding = "ui-encoding" {},
    /// GPUI 会话页面：Locale。
    UiLocale = "ui-locale" {},
    /// GPUI 会话页面：ProxyCommand。
    UiProxyCommand = "ui-proxy-command" {},
    /// GPUI 会话页面：选择…。
    UiChoose = "ui-choose" {},
    /// GPUI 会话页面：取消。
    UiCancel = "ui-cancel" {},
    /// GPUI 会话页面：保存。
    UiSave = "ui-save" {},
    /// GPUI 会话页面：连接。
    UiConnect = "ui-connect" {},
    /// GPUI 会话页面：自动。
    UiAutomatic = "ui-automatic" {},
    /// GPUI 会话页面：留空则保持已保存的值。
    UiKeepSaved = "ui-keep-saved" {},
    /// GPUI 会话页面：搜索已保存的会话。
    UiSearchSessions = "ui-search-sessions" {},
    /// GPUI 会话页面：已保存的会话。
    UiSavedSessions = "ui-saved-sessions" {},
    /// GPUI 会话页面：还没有保存的会话。选择“新建会话…”添加一个。。
    UiNoSessions = "ui-no-sessions" {},
    /// GPUI 会话页面：没有匹配的会话。
    UiNoMatchingSessions = "ui-no-matching-sessions" {},
    /// GPUI 会话页面：（空）。
    UiEmptyGroup = "ui-empty-group" {},
    /// Windows 本地会话的 Shell 选择。
    UiLocalShell = "ui-local-shell" {},
    /// 使用平台默认 Shell。
    UiShellDefault = "ui-shell-default" {},
    /// Shell 的完整命令行（可带参数）。
    UiShellCommand = "ui-shell-command" {},
    /// 自定义 Shell 命令（可带参数）。
    UiShellCustom = "ui-shell-custom" {},
    /// 侧栏路径筛选占位文字（ui-shell.md §2）。
    UiFilterPath = "ui-filter-path" {},
    /// 侧栏路径筛选无匹配项。
    UiNoMatchingPaths = "ui-no-matching-paths" {},
    /// 小于一秒的命令耗时。
    CommandDurationMilliseconds = "command-duration-milliseconds" { value: String },
    /// 小于一分钟的命令耗时。
    CommandDurationSeconds = "command-duration-seconds" { value: String },
    /// 分钟及补零的秒数。
    CommandDurationMinutes = "command-duration-minutes" { minutes: String, seconds: String },
    /// 小时及补零的分钟数。
    CommandDurationHours = "command-duration-hours" { hours: String, minutes: String },
    // ---- profile ----
    /// 保存 SSH Profile 时主机为空。
    ProfileHostEmpty = "profile-host-empty" {},
    /// 保存 SSH Profile 时用户名为空。
    ProfileUserEmpty = "profile-user-empty" {},
    /// locale 含非法字符或长度越界（local-shell.md §4、ssh.md §6）。
    ProfileLocaleInvalid = "profile-locale-invalid" { value: String },

    // ---- ssh ----
    /// 连接建立失败（ssh.md §11）。
    SshConnectFailed = "ssh-connect-failed" { endpoint: Endpoint, reason: Reason },
    /// 算法协商失败（ssh.md §2.3）。`server_offers` 只列服务器提供的算法，去掉扩展标记伪算法。
    SshNegotiationFailed = "ssh-negotiation-failed" {
        endpoint: Endpoint,
        category: AlgorithmCategory,
        server_offers: Vec<String>,
    },
    /// keepalive 无响应，判定连接已死。
    SshKeepaliveTimeout = "ssh-keepalive-timeout" { endpoint: Endpoint },
    /// 已建立的连接意外中断。
    SshConnectionLost = "ssh-connection-lost" { endpoint: Endpoint },
    /// 复制标签时共享连接已断；不偷偷重连（ssh.md §6）。
    SshSharedConnectionClosed = "ssh-shared-connection-closed" { endpoint: Endpoint },
    /// 私钥文件无法读取。
    SshKeyFileUnreadable = "ssh-key-file-unreadable" { path: String, reason: Reason },
    /// 私钥口令错误，解密失败。
    SshKeyDecryptFailed = "ssh-key-decrypt-failed" { path: String },
    /// 代理命令启动或运行失败（ssh.md §5）。
    SshProxyCommandFailed = "ssh-proxy-command-failed" { command: String, reason: Reason },

    // ---- hostkey ----
    /// 主机密钥与记录不一致且无法交互确认时的错误（ssh.md §3）。
    HostKeyChanged = "hostkey-changed" {
        endpoint: Endpoint,
        key_type: String,
        fingerprint: String,
    },
    /// 写入 known_hosts 失败（ssh.md §3）。
    HostKeyStoreWriteFailed = "hostkey-store-write-failed" { path: String, reason: Reason },

    // ---- auth ----
    /// 服务器要求多因素、部分认证已通过（ssh.md §4.1）。文案不得说“密码错误”。
    AuthPartialSuccess = "auth-partial-success" {
        endpoint: Endpoint,
        user: String,
        methods: Vec<String>,
    },
    /// 认证失败，按所用方式区分措辞（ssh.md §4）。
    AuthFailed = "auth-failed" { endpoint: Endpoint, user: String, method: AuthMethod },
    /// 服务器拒绝了该私钥。
    AuthKeyRejected = "auth-key-rejected" { endpoint: Endpoint, user: String, path: String },
    /// ssh-agent 里没有可用的身份。
    AuthAgentNoIdentity = "auth-agent-no-identity" { endpoint: Endpoint, user: String },
    /// 无法连上 ssh-agent（ssh.md §4.3）。Windows 要列出两种方式各自的失败原因，`details` 为英文技术信息。
    AuthAgentUnavailable = "auth-agent-unavailable" { details: Vec<String> },
    /// 用户取消认证，含 180 秒超时（ssh.md §4.2）。
    AuthCancelled = "auth-cancelled" { endpoint: Endpoint },
    /// keyboard-interactive 轮数超过上限（ssh.md §4.2）。
    AuthTooManyRounds = "auth-too-many-rounds" { endpoint: Endpoint, max_rounds: i64 },
    /// 服务器拒绝了 keyboard-interactive 的应答（ssh.md §4.2）。
    AuthResponseRejected = "auth-response-rejected" { endpoint: Endpoint },

    // ---- jump ----
    /// 跳板链成环（ssh.md §5）。
    JumpCycle = "jump-cycle" { profile: String },
    /// 跳板引用的 Profile 不存在。
    JumpMissing = "jump-missing" { profile: String },
    /// 跳板引用的 Profile 不是 SSH 类型。
    JumpNotSsh = "jump-not-ssh" { profile: String },
    /// 跳板链超过最大跳数。
    JumpTooLong = "jump-too-long" { max_hops: i64 },
    /// 某一跳连不上其下一跳目标。
    JumpUnreachable = "jump-unreachable" { jump: String, target: Endpoint, reason: Reason },
    /// 同时配置了跳板机与 proxy_command（二选一，ssh.md §5）。
    JumpProxyCommandConflict = "jump-proxy-command-conflict" {},

    // ---- session ----
    /// 会话进程正常结束且无退出码可报告（local-shell.md §7）。
    SessionExited = "session-exited" {},
    /// 会话进程结束并带退出码（local-shell.md §7）。
    SessionExitedWithCode = "session-exited-with-code" { code: i64 },
    /// SSH 连接被关闭导致会话结束（ssh.md §6）。
    SessionConnectionClosed = "session-connection-closed" { endpoint: Endpoint },
    /// 本地进程启动失败（local-shell.md §7）。
    SessionSpawnFailed = "session-spawn-failed" { command: String, reason: Reason },
    /// 纯 SFTP 会话拒绝终端类操作。
    SessionNotTerminal = "session-not-terminal" {},

    // ---- cwd ----
    // 以下翻译文本末尾统一加一句“会上报 OSC 7 的 shell 在任何系统上都能查到”（ssh.md §10 / local-shell.md §6）。
    /// 没找到这个连接的 shell。文案末尾统一加 OSC 7 提示。
    CwdNoShell = "cwd-no-shell" {},
    /// 服务器没有设置 `SSH_CONNECTION`。文案末尾统一加 OSC 7 提示。
    CwdNoSshConnection = "cwd-no-ssh-connection" {},
    /// 前台进程属于别的用户（如 sudo、su）。文案末尾统一加 OSC 7 提示。
    CwdOtherUser = "cwd-other-user" {},
    /// 服务器没有 `/proc`（只支持 Linux）。文案末尾统一加 OSC 7 提示。
    CwdNoProc = "cwd-no-proc" {},
    /// 查询超时。文案末尾统一加 OSC 7 提示。
    CwdTimeout = "cwd-timeout" {},
    /// 其他无法获取当前目录的情形。文案末尾统一加 OSC 7 提示。
    CwdUnavailable = "cwd-unavailable" {},
    /// Git 命令或仓库数据读取失败。
    GitFailed = "git-failed" { detail: String },
    /// 仓库摘要已过期，需要刷新。
    GitSummaryExpired = "git-summary-expired" {},
    /// 提交说明为空或超过长度上限。
    GitCommitMessageInvalid = "git-commit-message-invalid" {},
    /// 改动条目已不可用。
    GitEntryUnavailable = "git-entry-unavailable" {},
    /// 二进制、非普通文件或超限内容无法显示文本差异。
    GitDiffUnavailable = "git-diff-unavailable" {},
    /// 新增空文件没有文本差异。
    GitDiffEmptyFile = "git-diff-empty-file" {},
    /// 二进制文件没有文本差异。
    GitDiffBinary = "git-diff-binary" {},
    /// 相对当前提交没有净内容变化。
    GitDiffNoContentChange = "git-diff-no-content-change" {},
    /// 仅元数据变化或无法显示文本差异。
    GitDiffMetadataOnly = "git-diff-metadata-only" {},

    // ---- sftp ----
    /// 服务器没有启用或拒绝 SFTP 子系统。
    SftpSubsystemFailed = "sftp-subsystem-failed" { endpoint: Endpoint, reason: Reason },
    /// 某个 SFTP 操作失败。
    SftpOpFailed = "sftp-op-failed" { op: SftpOp, path: String, reason: Reason },
    /// 目标文件已存在。
    SftpFileExists = "sftp-file-exists" { path: String },

    // ---- xfer ----
    /// 传输完成（ssh.md §9）。
    XferCompleted = "xfer-completed" { name: String, size: ByteCount },
    /// 传输被取消。
    XferCancelled = "xfer-cancelled" {},
    /// 文件名不安全（路径分隔符、`..` 等），已拒绝（encoding-transfer.md §2）。
    XferUnsafeName = "xfer-unsafe-name" { name: String },
    /// 声明大小与实际接收量不符或超出上限。
    XferSizeOverflow = "xfer-size-overflow" {},
    /// 远端目标不是目录。
    XferRemoteNotDir = "xfer-remote-not-dir" { path: String },
    /// 写本地文件失败。
    XferLocalWriteFailed = "xfer-local-write-failed" { path: String, reason: Reason },
    /// 读本地文件失败。
    XferLocalReadFailed = "xfer-local-read-failed" { path: String, reason: Reason },
    /// 传输过程中会话结束。
    XferSessionEnded = "xfer-session-ended" {},
    /// 终端传输的字节流写入失败，区别于协议错误和文件错误。
    XferStreamWriteFailed = "xfer-stream-write-failed" { reason: Reason },
    /// 协议层错误，`detail` 为英文技术细节。
    XferProtocolError = "xfer-protocol-error" { protocol: XferProtocol, detail: String },

    // ---- zmodem（encoding-transfer.md §2）----
    /// 已启动接收，等待对方开始发送。
    ZmodemWaitingSender = "zmodem-waiting-sender" {},
    /// 对方提供了一个文件（未声明大小）。
    ZmodemFileOffered = "zmodem-file-offered" { name: String },
    /// 对方提供了一个文件（声明了大小）。
    ZmodemFileOfferedWithSize = "zmodem-file-offered-with-size" { name: String, size: ByteCount },
    /// 用户跳过了某个文件。
    ZmodemFileSkipped = "zmodem-file-skipped" { name: String },
    /// 接收结束的汇总。
    ZmodemReceiveSummary = "zmodem-receive-summary" { count: i64 },
    /// 提示用户选择要发送的文件。
    ZmodemSelectFiles = "zmodem-select-files" {},
    /// 用户取消了发送。
    ZmodemSendCancelled = "zmodem-send-cancelled" {},
    /// 对方跳过了某个文件。
    ZmodemPeerSkipped = "zmodem-peer-skipped" { name: String },
    /// 对方不支持断点续传。
    ZmodemResumeUnsupported = "zmodem-resume-unsupported" {},
    /// 等待对方确认。
    ZmodemWaitingConfirm = "zmodem-waiting-confirm" {},

    // ---- xmodem ----
    /// XMODEM 等待对方超时。
    XmodemTimeout = "xmodem-timeout" {},
    /// 对方取消了 XMODEM 传输。
    XmodemPeerCancelled = "xmodem-peer-cancelled" {},

    // ---- import（store-import.md §6、§7）----
    /// 读取要导入的文件失败。
    ImportReadFailed = "import-read-failed" { path: String, reason: Reason },
    /// 写入导出文件失败。
    ExportWriteFailed = "export-write-failed" { path: String, reason: Reason },
    /// Sender 命令已达上限，不能再新增（store-import.md §4）。
    SnippetLimitReached = "snippet-limit-reached" { limit: i64 },
    /// Sender 操作失败。
    SenderFailed = "sender-failed" { detail: String },
    /// Sender 拒绝空白命令（sender.md）。
    SenderEmptyCommand = "sender-empty-command" {},
    /// Sender 目标已有发送批次（sender.md）。
    SenderBusy = "sender-busy" {},
    /// Sender 没有可发送的会话（sender.md）。
    SenderNoSessions = "sender-no-sessions" {},
    /// Sender 新重复任务替换旧任务（sender.md）。
    SenderRepeatReplaced = "sender-repeat-replaced" {},
    /// Sender 停止时报告已发送行数（sender.md）。
    SenderStopped = "sender-stopped" { sent: i64, total: i64 },
    /// Sender 固定目标关闭后停止重复任务（sender.md）。
    SenderTargetClosed = "sender-target-closed" {},
    /// Sender 本轮发送结果（sender.md）。
    SenderSummary = "sender-summary" { success: i64, total: i64, skipped: i64 },
    /// Sender 重复任务达到计划轮次（sender.md）。
    SenderRepeatCompleted = "sender-repeat-completed" { rounds: i64 },
    /// Sender 发送目标已关闭（sender.md）。
    SenderTerminalClosed = "sender-terminal-closed" {},
    /// Sender 发送目标已断线或锁定（sender.md）。
    SenderTerminalUnavailable = "sender-terminal-unavailable" {},
    /// Sender 发送目标正在传输文件（sender.md）。
    SenderTerminalTransferring = "sender-terminal-transferring" {},
    /// Sender 向终端写入失败（sender.md）。
    SenderWriteFailed = "sender-write-failed" {},
    /// 导入的 SFTP 类型命令已转为全局作用域。
    ImportSnippetConverted = "import-snippet-converted" { name: String },
    /// 导入文件扩展名不对。
    ImportWrongExtension = "import-wrong-extension" {},
    /// 文件不是 Kanso 导出文件。
    ImportNotKansoFile = "import-not-kanso-file" {},
    /// 导出文件版本高于当前支持的版本。
    ImportVersionTooNew = "import-version-too-new" { version: i64, supported: i64 },
    /// 跳过了不支持的 Profile 类型。
    ImportSkippedUnsupportedKind = "import-skipped-unsupported-kind" { name: String, kind: String },
    /// 跳过了无效条目，`detail` 为英文技术细节。
    ImportSkippedInvalid = "import-skipped-invalid" { name: String, detail: String },
    /// 导入结果汇总。
    ImportReport = "import-report" { added: i64, updated: i64, skipped: i64 },
    /// 导入结果汇总后追加的已转换条数说明。
    ImportConvertedSuffix = "import-converted-suffix" { count: i64 },
    /// 导入结果明细标题：含转换与跳过。
    ImportConvertedAndSkipped = "import-converted-and-skipped" {},
    /// 导入 ssh config 时忽略了多跳 ProxyJump。
    SshConfigMultiHopIgnored = "sshconfig-multi-hop-ignored" { alias: String },
    /// ssh config 的 `Include` 嵌套超过上限，没有读取被引入的文件。
    SshConfigIncludeTooDeep = "sshconfig-include-too-deep" { file: String, include: String },
    /// ssh config 里有 `Match` 块；它没有被求值，块里的设置未生效。
    SshConfigMatchUnsupported = "sshconfig-match-unsupported" { file: String },
    /// 别名的 `Port` 不是有效端口，已按 22 处理。
    SshConfigInvalidPort = "sshconfig-invalid-port" { alias: String, value: String },
    /// 别名的某个选项值里有不支持的 `%` 替换符，已原样保留。
    SshConfigUnknownToken = "sshconfig-unknown-token" { alias: String, option: String, token: String },
    /// 导入 ssh config 的结果汇总。
    SshConfigImportReport = "sshconfig-import-report" {
        added: i64,
        updated: i64,
        jumps_added: i64,
        multi_hop_ignored: i64,
    },

    // ---- secret ----
    /// 凭据存储读写失败，`detail` 为英文技术细节。
    SecretStoreFailed = "secret-store-failed" { detail: String },
}
