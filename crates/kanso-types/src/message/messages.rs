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
    /// 主窗口：搜索。
    UiSearch = "ui-search" {},
    /// 主窗口：文件浏览器。
    UiFileBrowser = "ui-file-browser" {},
    /// 主窗口：没有匹配结果。
    UiSearchEmpty = "ui-search-empty" {},
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
    /// GPUI 界面文字：跟随系统
    UiFollowSystem = "ui-follow-system" {},
    /// GPUI 界面文字：语言
    UiLanguage = "ui-language" {},
    /// GPUI 界面文字：界面语言
    UiInterfaceLanguage = "ui-interface-language" {},
    /// GPUI 界面文字：切换后立即生效；“跟随系统”按系统界面语言选择，不支持的语言显示英文。
    UiTakesEffectImmediatelyFollowSystem = "ui-takes-effect-immediately-follow-system" {},
    /// GPUI 界面文字：导出完成
    UiExportComplete = "ui-export-complete" {},
    /// GPUI 界面文字：已导出到 …。 文件里不包含任何密码或口令。
    UiExportedToFileContainsNo = "ui-exported-to-file-contains-no" { path: String },
    /// GPUI 界面文字：导入数据
    UiImportData = "ui-import-data" {},
    /// GPUI 界面文字：将导入这个文件：
    UiThisFileWillBeImported = "ui-this-file-will-be-imported" {},
    /// GPUI 界面文字：会按 id 合并：文件里 id 相同的会话、分组、命令会替换本地的，其余作为新数据加入，本地数据不会被删除；被替换的会话保留已保存的凭据。有问题的单条数据会被跳过。
    UiDataIsMergedById = "ui-data-is-merged-by-id" {},
    /// GPUI 界面文字：导入完成
    UiImportComplete = "ui-import-complete" {},
    /// GPUI 界面文字：操作没有成功
    UiOperationFailed = "ui-operation-failed" {},
    /// GPUI 界面文字：已跳过
    UiSkipped = "ui-skipped" {},
    /// GPUI 界面文字：警告
    UiWarnings = "ui-warnings" {},
    /// GPUI 界面文字：好
    UiOk = "ui-ok" {},
    /// GPUI 界面文字：导入
    UiImport = "ui-import" {},
    /// GPUI 界面文字：选择要导入的 .kanso 文件
    UiChooseKansoFileToImport = "ui-choose-kanso-file-to-import" {},
    /// GPUI 界面文字：导入 OpenSSH 配置
    UiImportOpensshConfig = "ui-import-openssh-config" {},
    /// GPUI 界面文字：导入 OpenSSH 配置…
    UiImportOpensshConfig2 = "ui-import-openssh-config-2" {},
    /// GPUI 界面文字：选择 OpenSSH 配置文件
    UiChooseOpensshConfigFile = "ui-choose-openssh-config-file" {},
    /// GPUI 界面文字：不分组
    UiNoGroup = "ui-no-group" {},
    /// GPUI 界面文字：导入到分组
    UiImportIntoGroup = "ui-import-into-group" {},
    /// GPUI 界面文字：全选（已选 {} / …）
    UiSelectAllOfSelected = "ui-select-all-of-selected" { selected: i64, total: i64 },
    /// GPUI 界面文字：已保存，将更新
    UiSavedWillUpdate = "ui-saved-will-update" {},
    /// GPUI 界面文字：私钥 …
    UiKey = "ui-key" { path: String },
    /// GPUI 界面文字：经由 …
    UiVia = "ui-via" { jump: String },
    /// GPUI 界面文字：正在读取…
    UiReading = "ui-reading" {},
    /// GPUI 界面文字：正在导入…
    UiImporting = "ui-importing" {},
    /// GPUI 界面文字：配置文件里没有可导入的主机。
    UiNoHostsToImportIn = "ui-no-hosts-to-import-in" {},
    /// GPUI 界面文字：找到 {} 台主机，其中 … 台已保存（导入时会更新）。
    UiFoundHostsOfThemAlready = "ui-found-hosts-of-them-already" { found: i64, existing: i64 },
    /// GPUI 界面文字：没有打开的会话
    UiNoOpenSessions = "ui-no-open-sessions" {},
    /// GPUI 界面文字：新建本地 Shell
    UiNewLocalShell = "ui-new-local-shell" {},
    /// GPUI 界面文字：新建会话…
    UiNewSession2 = "ui-new-session-2" {},
    /// GPUI 界面文字：或按
    UiOrPress = "ui-or-press" {},
    /// GPUI 界面文字：上一个标签
    UiPreviousTab = "ui-previous-tab" {},
    /// GPUI 界面文字：下一个标签
    UiNextTab = "ui-next-tab" {},
    /// GPUI 界面文字：上一个分屏
    UiPreviousPane = "ui-previous-pane" {},
    /// GPUI 界面文字：下一个分屏
    UiNextPane = "ui-next-pane" {},
    /// GPUI 界面文字：关闭会话
    UiCloseSession = "ui-close-session" {},
    /// GPUI 界面文字：导出数据…
    UiExportData = "ui-export-data" {},
    /// GPUI 界面文字：导入数据…
    UiImportData2 = "ui-import-data-2" {},
    /// GPUI 界面文字：复制
    UiCopy = "ui-copy" {},
    /// GPUI 界面文字：粘贴
    UiPaste = "ui-paste" {},
    /// GPUI 界面文字：全选
    UiSelectAll = "ui-select-all" {},
    /// GPUI 界面文字：清除缓冲区
    UiClearBuffer = "ui-clear-buffer" {},
    /// GPUI 界面文字：向右拆分
    UiSplitRight = "ui-split-right" {},
    /// GPUI 界面文字：向下拆分
    UiSplitDown = "ui-split-down" {},
    /// GPUI 界面文字：右键弹出菜单
    UiRightClickShowsMenu = "ui-right-click-shows-menu" {},
    /// GPUI 界面文字：标签栏
    UiTabBar = "ui-tab-bar" {},
    /// GPUI 界面文字：视图
    UiView = "ui-view" {},
    /// GPUI 界面文字：设置…
    UiSettings2 = "ui-settings-2" {},
    /// GPUI 界面文字：退出
    UiQuit = "ui-quit" {},
    /// GPUI 界面文字：编辑会话…
    UiEditSession2 = "ui-edit-session-2" {},
    /// GPUI 界面文字：关闭
    UiClose = "ui-close" {},
    /// GPUI 界面文字：关闭其他
    UiCloseOthers = "ui-close-others" {},
    /// GPUI 界面文字：关闭左侧
    UiCloseTabsToLeft = "ui-close-tabs-to-left" {},
    /// GPUI 界面文字：关闭右侧
    UiCloseTabsToRight = "ui-close-tabs-to-right" {},
    /// GPUI 界面文字：全部关闭
    UiCloseAll = "ui-close-all" {},
    /// GPUI 界面文字：…另外 N 个
    UiAndMore = "ui-and-more" { count: i64 },
    /// GPUI 界面文字：退出 Kanso？
    UiQuitKanso = "ui-quit-kanso" {},
    /// GPUI 界面文字：关闭仍在运行的会话？
    UiCloseRunningSessions = "ui-close-running-sessions" {},
    /// GPUI 界面文字：以下会话仍在运行或保持连接：
    UiTheseSessionsAreStillRunning = "ui-these-sessions-are-still-running" {},
    /// GPUI 界面文字：关闭会中断正在进行的文件传输。
    UiClosingWillInterruptFileTransfers = "ui-closing-will-interrupt-file-transfers" {},
    /// GPUI 界面文字：连接中…
    UiConnecting = "ui-connecting" {},
    /// GPUI 界面文字：会话已结束（退出码 …）
    UiSessionEndedExitCode = "ui-session-ended-exit-code" { code: i64 },
    /// GPUI 界面文字：会话已结束
    UiSessionEnded = "ui-session-ended" {},
    /// GPUI 界面文字：会话已断开
    UiSessionDisconnected = "ui-session-disconnected" {},
    /// GPUI 界面文字：命令运行中
    UiCommandRunning = "ui-command-running" {},
    /// GPUI 界面文字：AI 运行中
    UiAiRunning = "ui-ai-running" {},
    /// GPUI 界面文字：AI 已完成
    UiAiFinished = "ui-ai-finished" {},
    /// GPUI 界面文字：命令已完成，点击查看
    UiCommandFinishedClickToView = "ui-command-finished-click-to-view" {},
    /// GPUI 界面文字：操作失败。
    UiOperationFailed2 = "ui-operation-failed-2" {},
    /// GPUI 界面文字：默认 Shell
    UiDefaultShell = "ui-default-shell" {},
    /// GPUI 界面文字：编辑…
    UiEdit = "ui-edit" {},
    /// GPUI 界面文字：移到分组
    UiMoveToGroup = "ui-move-to-group" {},
    /// GPUI 界面文字：删除…
    UiDelete = "ui-delete" {},
    /// GPUI 界面文字：重命名…
    UiRename = "ui-rename" {},
    /// GPUI 界面文字：新建分组…
    UiNewGroup = "ui-new-group" {},
    /// GPUI 界面文字：删除这个分组会同时删除其中的 {} 个会话和 … 条快捷命令，此操作无法撤销。
    UiDeletingThisGroupAlsoDeletes = "ui-deleting-this-group-also-deletes" { profiles: i64, snippets: i64 },
    /// GPUI 界面文字：只会删除 Kanso 中保存的会话及其凭据，不会改动 ~/.ssh/config 和密钥文件。此操作无法撤销。
    UiOnlySessionAndItsCredentials = "ui-only-session-and-its-credentials" {},
    /// GPUI 界面文字：本地
    UiLocal = "ui-local" {},
    /// GPUI 界面文字：请输入会话名称。
    UiEnterSessionName = "ui-enter-session-name" {},
    /// GPUI 界面文字：端口必须是 1 到 65535 之间的数字。
    UiPortMustBeNumberFrom = "ui-port-must-be-number-from" {},
    /// GPUI 界面文字：Locale 只能包含字母、数字和 _ - . @，长度 1 到 64。
    UiLocaleMayOnlyContainLetters = "ui-locale-may-only-contain-letters" {},
    /// GPUI 界面文字：保存失败。
    UiSaveFailed = "ui-save-failed" {},
    /// GPUI 界面文字：会话已保存，但凭据没有存进去：…
    UiSessionSavedButCredentialsCould = "ui-session-saved-but-credentials-could" { detail: String },
    /// GPUI 界面文字：服务器：…
    UiServer = "ui-server" { host_port: String },
    /// GPUI 界面文字：服务器：{}（经由 …）
    UiServerVia = "ui-server-via" { host_port: String, via: String },
    /// GPUI 界面文字：标签：…
    UiTab = "ui-tab" { title: String },
    /// GPUI 界面文字：主机密钥已变更
    UiHostKeyChanged = "ui-host-key-changed" {},
    /// GPUI 界面文字：密钥类型
    UiKeyType = "ui-key-type" {},
    /// GPUI 界面文字：SHA256 指纹
    UiSha256Fingerprint = "ui-sha256-fingerprint" {},
    /// GPUI 界面文字：原记录位置
    UiPreviouslyRecordedAt = "ui-previously-recorded-at" {},
    /// GPUI 界面文字：接受新密钥并连接
    UiAcceptNewKeyAndConnect = "ui-accept-new-key-and-connect" {},
    /// GPUI 界面文字：需要验证
    UiVerificationRequired = "ui-verification-required" {},
    /// GPUI 界面文字：继续
    UiContinue = "ui-continue" {},
    /// GPUI 界面文字：需要私钥口令
    UiPrivateKeyPassphraseRequired = "ui-private-key-passphrase-required" {},
    /// GPUI 界面文字：{} 用户：{} 私钥：…
    UiCredentialIntroKey = "ui-credential-intro-key" { server: String, user: String, key: String },
    /// GPUI 界面文字：需要密码
    UiPasswordRequired = "ui-password-required" {},
    /// GPUI 界面文字：{} 用户：…
    UiCredentialIntro = "ui-credential-intro" { server: String, user: String },
    /// GPUI 界面文字：连接并保存
    UiConnectAndSave = "ui-connect-and-save" {},
    /// GPUI 界面文字：仅连接
    UiConnectOnly = "ui-connect-only" {},
    /// GPUI 界面文字：块状
    UiBlock = "ui-block" {},
    /// GPUI 界面文字：下划线
    UiUnderline = "ui-underline" {},
    /// GPUI 界面文字：竖线
    UiBar = "ui-bar" {},
    /// GPUI 界面文字：浅色
    UiLight = "ui-light" {},
    /// GPUI 界面文字：深色
    UiDark = "ui-dark" {},
    /// GPUI 界面文字：只显示行号
    UiLineNumbersOnly = "ui-line-numbers-only" {},
    /// GPUI 界面文字：只显示时间
    UiTimestampsOnly = "ui-timestamps-only" {},
    /// GPUI 界面文字：时间和行号
    UiTimestampsAndLineNumbers = "ui-timestamps-and-line-numbers" {},
    /// GPUI 界面文字：不显示
    UiOff = "ui-off" {},
    /// GPUI 界面文字：无法打开本地 shell：…
    UiCantOpenLocalShell = "ui-cant-open-local-shell" { detail: String },
    /// GPUI 界面文字：无法拆分：…
    UiCantSplit = "ui-cant-split" { detail: String },
    /// GPUI 界面文字：会话已结束。Ctrl+Shift+W 关闭此标签
    UiSessionEndedPressCtrlShift = "ui-session-ended-press-ctrl-shift" {},
    /// GPUI 界面文字：会话已结束（退出码 …）。Ctrl+Shift+W 关闭此标签
    UiSessionEndedExitCodePress = "ui-session-ended-exit-code-press" { code: i64 },
    /// GPUI 界面文字：会话已断开。Ctrl+Shift+W 关闭此标签
    UiSessionDisconnectedPressCtrlShift = "ui-session-disconnected-press-ctrl-shift" {},
    /// GPUI 界面文字：这个会话已经不存在
    UiThisSessionNoLongerExists = "ui-this-session-no-longer-exists" {},
    /// GPUI 界面文字：SFTP 会话需要文件面板，主窗口暂时还不能打开它
    UiSftpSessionsNeedFilePanel = "ui-sftp-sessions-need-file-panel" {},
    /// GPUI 界面文字：无法连接“{}”：…
    UiCantConnectTo = "ui-cant-connect-to" { name: String, detail: String },
    /// GPUI 界面文字：分屏 …
    UiPane = "ui-pane" { index: i64 },
    /// GPUI 界面文字：文件
    UiFiles = "ui-files" {},
    /// GPUI 界面文字：时间戳
    UiTimestamps = "ui-timestamps" {},
    /// GPUI 界面文字：查找
    UiFindPlaceholder = "ui-find-placeholder" {},
    /// GPUI 界面文字：无结果
    UiFindNoResults = "ui-find-no-results" {},
    /// GPUI 界面文字：无效的正则表达式
    UiFindInvalidRegex = "ui-find-invalid-regex" {},
    /// GPUI 界面文字：复制命令
    UiCopyCommand = "ui-copy-command" {},
    /// GPUI 界面文字：复制输出
    UiCopyOutput = "ui-copy-output" {},
    /// GPUI 界面文字：已复制
    UiCopied = "ui-copied" {},
    /// GPUI 界面文字：不支持打开这种链接
    UiLinkUnsupported = "ui-link-unsupported" {},
    /// GPUI 界面文字：新建分组
    UiNewGroup2 = "ui-new-group-2" {},
    /// GPUI 界面文字：分组名称
    UiGroupName = "ui-group-name" {},
    /// GPUI 界面文字：创建
    UiCreate = "ui-create" {},
    /// GPUI 界面文字：重命名分组
    UiRenameGroup = "ui-rename-group" {},
    /// GPUI 界面文字：重命名
    UiRename2 = "ui-rename-2" {},
    /// GPUI 界面文字：删除
    UiDelete2 = "ui-delete-2" {},
    /// GPUI 界面文字：删除分组“…”？
    UiDeleteGroup = "ui-delete-group" { name: String },
    /// GPUI 界面文字：删除会话“…”？
    UiDeleteSession = "ui-delete-session" { name: String },
    /// GPUI 界面文字：编辑
    UiEdit2 = "ui-edit-2" {},
    /// GPUI 界面文字：无
    UiNone = "ui-none" {},
    /// GPUI 界面文字：选择私钥文件
    UiChoosePrivateKeyFile = "ui-choose-private-key-file" {},
    /// GPUI 界面文字：搜索字体
    UiSearchFonts = "ui-search-fonts" {},
    /// GPUI 界面文字：搜索等宽字体
    UiSearchMonospacedFonts = "ui-search-monospaced-fonts" {},
    /// GPUI 界面文字：系统默认；也可直接输入字体名
    UiSystemDefaultOrTypeFont = "ui-system-default-or-type-font" {},
    /// GPUI 界面文字：… 行
    UiLines = "ui-lines" { lines: String },
    /// Filer 与文件传输界面文字：received.bin
    UiXferDefaultName = "ui-xfer-default-name" {},
    /// Filer 与文件传输界面文字：对方请求接收文件，请选择要发送的文件（可多选）
    UiXferHintPickFiles = "ui-xfer-hint-pick-files" {},
    /// Filer 与文件传输界面文字：请选择要用 XMODEM 发送的文件
    UiXferHintPickFile = "ui-xfer-hint-pick-file" {},
    /// Filer 与文件传输界面文字：对方要发送 { $name }（{ $size }），请选择保存位置
    UiXferHintSaveSized = "ui-xfer-hint-save-sized" { name: String, amount: String },
    /// Filer 与文件传输界面文字：对方要发送 { $name }，请选择保存位置
    UiXferHintSave = "ui-xfer-hint-save" { name: String },
    /// Filer 与文件传输界面文字：请选择 XMODEM 接收文件的保存位置
    UiXferHintXmodemSave = "ui-xfer-hint-xmodem-save" {},
    /// Filer 与文件传输界面文字：传输完成
    UiXferDone = "ui-xfer-done" {},
    /// Filer 与文件传输界面文字：已取消
    UiXferCancelled = "ui-xfer-cancelled" {},
    /// Filer 与文件传输界面文字：失败：{ $reason }
    UiXferFailed = "ui-xfer-failed" { why: String },
    /// Filer 与文件传输界面文字：发送
    UiXferSend = "ui-xfer-send" {},
    /// Filer 与文件传输界面文字：接收
    UiXferReceive = "ui-xfer-receive" {},
    /// Filer 与文件传输界面文字：删除链接
    UiFilerDeleteLink = "ui-filer-delete-link" {},
    /// Filer 与文件传输界面文字：只移除链接，不影响目标，无法撤销。
    UiFilerDeleteLinkNote = "ui-filer-delete-link-note" {},
    /// Filer 与文件传输界面文字：删除目录
    UiFilerDeleteDir = "ui-filer-delete-dir" {},
    /// Filer 与文件传输界面文字：只能删除空目录，无法撤销。
    UiFilerDeleteDirNote = "ui-filer-delete-dir-note" {},
    /// Filer 与文件传输界面文字：删除文件
    UiFilerDeleteFile = "ui-filer-delete-file" {},
    /// Filer 与文件传输界面文字：文件将被移除，无法撤销。
    UiFilerDeleteFileNote = "ui-filer-delete-file-note" {},
    /// Filer 与文件传输界面文字：位置：{ $location }
    UiFilerDeleteWhere = "ui-filer-delete-where" { location: String },
    /// Filer 与文件传输界面文字：删除 { $count } 个项目
    UiFilerDeleteMany = "ui-filer-delete-many" { count: i64 },
    /// Filer 与文件传输界面文字：还有 { $count } 项
    UiFilerDeleteMore = "ui-filer-delete-more" { count: i64 },
    /// Filer 与文件传输界面文字：这些项目将被永久移除，无法撤销；非空目录会保留。
    UiFilerDeleteManyNote = "ui-filer-delete-many-note" {},
    /// Filer 与文件传输界面文字：取消
    UiFilerCancel = "ui-filer-cancel" {},
    /// Filer 与文件传输界面文字：删除
    UiFilerDelete = "ui-filer-delete" {},
    /// Filer 与文件传输界面文字：新建文件
    UiFilerNewFile = "ui-filer-new-file" {},
    /// Filer 与文件传输界面文字：新建目录
    UiFilerNewFolder = "ui-filer-new-folder" {},
    /// Filer 与文件传输界面文字：重命名
    UiFilerRename = "ui-filer-rename" {},
    /// Filer 与文件传输界面文字：位置：{ $location }
    UiFilerNameWhere = "ui-filer-name-where" { location: String },
    /// Filer 与文件传输界面文字：名称
    UiFilerName = "ui-filer-name" {},
    /// Filer 与文件传输界面文字：确定
    UiFilerOk = "ui-filer-ok" {},
    /// Filer 与文件传输界面文字：名称不能为空
    UiFilerNameEmpty = "ui-filer-name-empty" {},
    /// Filer 与文件传输界面文字：名称不能是 . 或 ..
    UiFilerNameReserved = "ui-filer-name-reserved" {},
    /// Filer 与文件传输界面文字：名称不能包含目录分隔符，只能是当前目录里的一个项目
    UiFilerNameSeparator = "ui-filer-name-separator" {},
    /// Filer 与文件传输界面文字：名称不能包含字符 { $ch }
    UiFilerNameInvalidChar = "ui-filer-name-invalid-char" { ch: String },
    /// Filer 与文件传输界面文字：同名项目已存在
    UiFilerConflictTitle = "ui-filer-conflict-title" {},
    /// Filer 与文件传输界面文字：目标位置已有名为“{ $name }”的文件。
    UiFilerConflictFile = "ui-filer-conflict-file" { name: String },
    /// Filer 与文件传输界面文字：目标位置已有名为“{ $name }”的目录。
    UiFilerConflictDir = "ui-filer-conflict-dir" { name: String },
    /// Filer 与文件传输界面文字：目录会合并，其中的同名文件仍按你的选择处理。
    UiFilerConflictMergeNote = "ui-filer-conflict-merge-note" {},
    /// Filer 与文件传输界面文字：跳过
    UiFilerSkip = "ui-filer-skip" {},
    /// Filer 与文件传输界面文字：全部跳过
    UiFilerSkipAll = "ui-filer-skip-all" {},
    /// Filer 与文件传输界面文字：覆盖
    UiFilerOverwrite = "ui-filer-overwrite" {},
    /// Filer 与文件传输界面文字：全部覆盖
    UiFilerOverwriteAll = "ui-filer-overwrite-all" {},
    /// Filer 与文件传输界面文字：不存在
    UiFilerErrNotFound = "ui-filer-err-not-found" {},
    /// Filer 与文件传输界面文字：没有权限
    UiFilerErrDenied = "ui-filer-err-denied" {},
    /// Filer 与文件传输界面文字：不是目录
    UiFilerErrNotDir = "ui-filer-err-not-dir" {},
    /// Filer 与文件传输界面文字：已经存在
    UiFilerErrExists = "ui-filer-err-exists" {},
    /// Filer 与文件传输界面文字：目录不是空的，只能删除空目录
    UiFilerErrNotEmpty = "ui-filer-err-not-empty" {},
    /// Filer 与文件传输界面文字：已取消
    UiFilerErrCancelled = "ui-filer-err-cancelled" {},
    /// Filer 与文件传输界面文字：本地磁盘 ({ $letter }:)
    UiFilerDrive = "ui-filer-drive" { letter: String },
    /// Filer 与文件传输界面文字：不能把目录复制到它自己或它的子目录里
    UiFilerErrCopyIntoSelf = "ui-filer-err-copy-into-self" {},
    /// Filer 与文件传输界面文字：无法取得名称
    UiFilerErrNoName = "ui-filer-err-no-name" {},
    /// Filer 与文件传输界面文字：目标位置是符号链接，已拒绝写入
    UiFilerErrTargetLink = "ui-filer-err-target-link" {},
    /// Filer 与文件传输界面文字：目标位置已有同名目录
    UiFilerErrTargetDir = "ui-filer-err-target-dir" {},
    /// Filer 与文件传输界面文字：此电脑
    UiFilerThisPc = "ui-filer-this-pc" {},
    /// Filer 与文件传输界面文字：正在连接…
    UiFilerConnecting = "ui-filer-connecting" {},
    /// Filer 与文件传输界面文字：正在读取… 已收到 { $count } 项
    UiFilerLoading = "ui-filer-loading" { count: i64 },
    /// Filer 与文件传输界面文字：连接已断开（列表已过期，共 { $count } 项）
    UiFilerDisconnectedStats = "ui-filer-disconnected-stats" { count: i64 },
    /// Filer 与文件传输界面文字：{ $count } 项
    UiFilerEmpty = "ui-filer-empty" { count: i64 },
    /// Filer 与文件传输界面文字：{ $total } 项：{ $dirs } 个目录，{ $files } 个文件
    UiFilerStats = "ui-filer-stats" { items: String, dirs: String, files: String },
    /// Filer 与文件传输界面文字：{ $total } 项：{ $dirs } 个目录，{ $files } 个文件，{ $links } 个链接
    UiFilerStatsLinks = "ui-filer-stats-links" { items: String, dirs: String, files: String, links: String },
    /// Filer 与文件传输界面文字：链接
    UiFilerKindLink = "ui-filer-kind-link" {},
    /// Filer 与文件传输界面文字：目录
    UiFilerKindDir = "ui-filer-kind-dir" {},
    /// Filer 与文件传输界面文字：文件
    UiFilerKindFile = "ui-filer-kind-file" {},
    /// Filer 与文件传输界面文字：类型：{ $kind }
    UiFilerTipKind = "ui-filer-tip-kind" { kind: String },
    /// Filer 与文件传输界面文字：大小：{ $size }
    UiFilerTipSize = "ui-filer-tip-size" { amount: String },
    /// Filer 与文件传输界面文字：权限：{ $perm }
    UiFilerTipPerm = "ui-filer-tip-perm" { perm: String },
    /// Filer 与文件传输界面文字：属主：{ $owner }
    UiFilerTipOwner = "ui-filer-tip-owner" { owner: String },
    /// Filer 与文件传输界面文字：属组：{ $group }
    UiFilerTipGroup = "ui-filer-tip-group" { group: String },
    /// Filer 与文件传输界面文字：上传
    UiFilerOpUpload = "ui-filer-op-upload" {},
    /// Filer 与文件传输界面文字：下载
    UiFilerOpDownload = "ui-filer-op-download" {},
    /// Filer 与文件传输界面文字：复制
    UiFilerOpCopy = "ui-filer-op-copy" {},
    /// Filer 与文件传输界面文字：正在处理，请等待当前传输完成
    UiFilerBusy = "ui-filer-busy" {},
    /// Filer 与文件传输界面文字：完成：{ $count } 项
    UiFilerBatchDone = "ui-filer-batch-done" { count: i64 },
    /// Filer 与文件传输界面文字：已取消
    UiFilerCancelled = "ui-filer-cancelled" {},
    /// Filer 与文件传输界面文字：{ $op }失败：{ $reason }
    UiFilerOpFailed = "ui-filer-op-failed" { action: String, why: String },
    /// Filer 与文件传输界面文字：{ $op } { $name }：{ $reason }
    UiFilerOpError = "ui-filer-op-error" { action: String, name: String, why: String },
    /// Filer 与文件传输界面文字：等待处理同名文件…
    UiFilerAwaitingConflict = "ui-filer-awaiting-conflict" {},
    /// Filer 与文件传输界面文字：跳过 { $count } 个同名项
    UiFilerSkipped = "ui-filer-skipped" { count: i64 },
    /// Filer 与文件传输界面文字：本地
    UiFilerLocal = "ui-filer-local" {},
    /// Filer 与文件传输界面文字：{ $name } · { $host }
    UiFilerIdentity = "ui-filer-identity" { name: String, host: String },
    /// Filer 与文件传输界面文字：这一栏不是远端，无法上传下载
    UiFilerNotRemote = "ui-filer-not-remote" {},
    /// Filer 与文件传输界面文字：“此电脑”不是真实目录，不能在这里新建、删除或接收文件
    UiFilerNoWriteThisPc = "ui-filer-no-write-this-pc" {},
    /// Filer 与文件传输界面文字：连接已断开，已停止写入操作
    UiFilerDisconnectedWrite = "ui-filer-disconnected-write" {},
    /// Filer 与文件传输界面文字：没有打开的目录，或目录读取失败
    UiFilerNoDirOpen = "ui-filer-no-dir-open" {},
    /// Filer 与文件传输界面文字：文件已经在此目录，已跳过
    UiFilerAlreadyHere = "ui-filer-already-here" {},
    /// Filer 与文件传输界面文字：无法打开链接“{ $name }”：{ $reason }
    UiFilerLinkBroken = "ui-filer-link-broken" { name: String, why: String },
    /// Filer 与文件传输界面文字：暂不支持用本地应用编辑远端文件；可在右键菜单里选择“下载”
    UiFilerRemoteEditUnsupported = "ui-filer-remote-edit-unsupported" {},
    /// Filer 与文件传输界面文字：打开
    UiFilerMenuOpen = "ui-filer-menu-open" {},
    /// Filer 与文件传输界面文字：打开方式…
    UiFilerMenuOpenWith = "ui-filer-menu-open-with" {},
    /// Filer 与文件传输界面文字：下载
    UiFilerMenuDownload = "ui-filer-menu-download" {},
    /// Filer 与文件传输界面文字：复制完整路径
    UiFilerMenuCopyPath = "ui-filer-menu-copy-path" {},
    /// Filer 与文件传输界面文字：复制名称
    UiFilerMenuCopyName = "ui-filer-menu-copy-name" {},
    /// Filer 与文件传输界面文字：复制当前目录路径
    UiFilerMenuCopyDir = "ui-filer-menu-copy-dir" {},
    /// Filer 与文件传输界面文字：上传文件…
    UiFilerUploadFiles = "ui-filer-upload-files" {},
    /// Filer 与文件传输界面文字：上传目录…
    UiFilerUploadDirs = "ui-filer-upload-dirs" {},
    /// Filer 与文件传输界面文字：刷新
    UiFilerRefresh = "ui-filer-refresh" {},
    /// Filer 与文件传输界面文字：{ $what }“{ $name }”失败（{ $where }）：{ $reason }
    UiFilerOpErrorAt = "ui-filer-op-error-at" { what: String, name: String, place: String, why: String },
    /// Filer 与文件传输界面文字：上传
    UiFilerTipUpload = "ui-filer-tip-upload" {},
    /// Filer 与文件传输界面文字：只适用于已连接的 SSH 会话
    UiFilerTipUploadRemoteOnly = "ui-filer-tip-upload-remote-only" {},
    /// Filer 与文件传输界面文字：显示 / 隐藏以点开头的项目
    UiFilerTipHidden = "ui-filer-tip-hidden" {},
    /// Filer 与文件传输界面文字：定位终端目录
    UiFilerTipLocate = "ui-filer-tip-locate" {},
    /// Filer 与文件传输界面文字：上一级
    UiFilerTipUp = "ui-filer-tip-up" {},
    /// Filer 与文件传输界面文字：家目录
    UiFilerTipHome = "ui-filer-tip-home" {},
    /// Filer 与文件传输界面文字：取消传输
    UiFilerTipCancel = "ui-filer-tip-cancel" {},
    /// Filer 与文件传输界面文字：名称
    UiFilerColName = "ui-filer-col-name" {},
    /// Filer 与文件传输界面文字：修改日期
    UiFilerColDate = "ui-filer-col-date" {},
    /// Filer 与文件传输界面文字：大小
    UiFilerColSize = "ui-filer-col-size" {},
    /// Filer 与文件传输界面文字：修改时间
    UiFilerColTime = "ui-filer-col-time" {},
    /// Filer 与文件传输界面文字：这个目录是空的
    UiFilerFolderEmpty = "ui-filer-folder-empty" {},
    /// Filer 与文件传输界面文字：无法打开 { $target }：{ $reason }
    UiFilerOpenFailed = "ui-filer-open-failed" { dest: String, why: String },
    /// Filer 与文件传输界面文字：下面仍是 { $path } 的内容
    UiFilerStillShowing = "ui-filer-still-showing" { path: String },
    /// Filer 与文件传输界面文字：回到家目录
    UiFilerGoHome = "ui-filer-go-home" {},
    /// Filer 与文件传输界面文字：连接已断开
    UiFilerDisconnectedBadge = "ui-filer-disconnected-badge" {},
    /// Filer 与文件传输界面文字：放下以上传
    UiFilerDropUpload = "ui-filer-drop-upload" {},
    /// Filer 与文件传输界面文字：放下以复制到这里
    UiFilerDropCopy = "ui-filer-drop-copy" {},
    /// Filer 与文件传输界面文字：关闭
    UiFilerDismiss = "ui-filer-dismiss" {},
    /// Filer 与文件传输界面文字：上传到远端
    UiFilerSendToRemote = "ui-filer-send-to-remote" {},
    /// Filer 与文件传输界面文字：下载到本地
    UiFilerGetFromRemote = "ui-filer-get-from-remote" {},
    /// Filer 与文件传输界面文字：没有可浏览的会话
    UiFilerNoSession = "ui-filer-no-session" {},
    /// Filer 与文件传输界面文字：在 Filer 中显示工作目录
    UiFilerMenuLocate = "ui-filer-menu-locate" {},
    /// Filer 与文件传输界面文字：文件浏览器
    UiFilerMenuView = "ui-filer-menu-view" {},
    /// Filer 与文件传输界面文字：文件
    UiFilerStatusLabel = "ui-filer-status-label" {},
    /// Filer 与文件传输界面文字：文件浏览已经在当前标签打开
    UiFilerStatusAlreadyOpen = "ui-filer-status-already-open" {},
    /// Filer 与文件传输界面文字：已定位到 { $path }
    UiFilerLocated = "ui-filer-located" { path: String },
    /// Filer 与文件传输界面文字：没有可定位的会话
    UiFilerLocateNoSession = "ui-filer-locate-no-session" {},
    /// Filer 与文件传输界面文字：文件会话没有 shell 工作目录
    UiFilerLocateSftp = "ui-filer-locate-sftp" {},
    /// Filer 与文件传输界面文字：会话未连接
    UiFilerLocateNotConnected = "ui-filer-locate-not-connected" {},
    /// Filer 与文件传输界面文字：终端上报的目录属于另一台主机（{ $host }），不能当作本机路径
    UiFilerLocateForeignHost = "ui-filer-locate-foreign-host" { host: String },
    /// Filer 与文件传输界面文字：无法取得“{ $name }”的工作目录：{ $reason }
    UiFilerLocateFailed = "ui-filer-locate-failed" { name: String, why: String },
    /// Filer 与文件传输界面文字：文件传输
    UiXferMenu = "ui-xfer-menu" {},
    /// Filer 与文件传输界面文字：用 XMODEM 发送…
    UiXferMenuSend = "ui-xfer-menu-send" {},
    /// Filer 与文件传输界面文字：用 XMODEM-1K 发送…
    UiXferMenuSend1k = "ui-xfer-menu-send-1k" {},
    /// Filer 与文件传输界面文字：用 XMODEM 接收…
    UiXferMenuReceive = "ui-xfer-menu-receive" {},
    /// Filer 与文件传输界面文字：取消传输
    UiXferMenuCancel = "ui-xfer-menu-cancel" {},
    /// Filer 与文件传输界面文字：无法开始传输：{ $reason }
    UiXferStartFailed = "ui-xfer-start-failed" { why: String },
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

    // ---- Git 改动面板（gpui） ----
    /// Git 改动面板界面文字：改动
    UiGitTitle = "ui-git-title" {},
    /// Git 改动面板界面文字：刷新改动
    UiGitRefresh = "ui-git-refresh" {},
    /// Git 改动面板界面文字：关闭改动面板
    UiGitClose = "ui-git-close" {},
    /// Git 改动面板界面文字：更多操作
    UiGitMore = "ui-git-more" {},
    /// Git 改动面板界面文字：无法检测此目录的改动；仅适用于 Git 仓库
    UiGitUnsupported = "ui-git-unsupported" {},
    /// Git 改动面板界面文字：正在读取改动…
    UiGitLoading = "ui-git-loading" {},
    /// Git 改动面板界面文字：工作区干净
    UiGitClean = "ui-git-clean" {},
    /// Git 改动面板界面文字：提交说明（回车提交）
    UiGitCommitPlaceholder = "ui-git-commit-placeholder" {},
    /// Git 改动面板界面文字：提交
    UiGitCommit = "ui-git-commit" {},
    /// Git 改动面板界面文字：丢弃该文件改动
    UiGitDiscardFile = "ui-git-discard-file" {},
    /// Git 改动面板界面文字：丢弃全部改动
    UiGitDiscardAll = "ui-git-discard-all" {},
    /// Git 改动面板界面文字：复制完整路径
    UiGitCopyPath = "ui-git-copy-path" {},
    /// Git 改动面板界面文字：正在读取差异…
    UiGitReadingDiff = "ui-git-reading-diff" {},
    /// Git 改动面板界面文字：差异已截断
    UiGitDiffTruncated = "ui-git-diff-truncated" {},
    /// Git 改动面板界面文字：提交可能已经留下暂存的内容。
    UiGitCommitMayStaged = "ui-git-commit-may-staged" {},
    /// Git 改动面板界面文字：丢弃“{ $path }”的改动？
    UiGitDiscardTitleFile = "ui-git-discard-title-file" { path: String },
    /// Git 改动面板界面文字：丢弃“{ $repo }”中的全部改动？
    UiGitDiscardTitleAll = "ui-git-discard-title-all" { repo: String },
    /// Git 改动面板界面文字：新增的内容将被永久删除。
    UiGitDiscardBodyNew = "ui-git-discard-body-new" {},
    /// Git 改动面板界面文字：暂存和未暂存的修改都会丢失；重命名会恢复原路径。
    UiGitDiscardBodyTracked = "ui-git-discard-body-tracked" {},
    /// Git 改动面板界面文字：暂存、未暂存和未跟踪的改动都会永久丢失；被 Git 忽略的文件保留。
    UiGitDiscardBodyAll = "ui-git-discard-body-all" {},
    /// Git 改动面板界面文字：应用内没有撤销、回收站或自动备份。
    UiGitDiscardNoUndo = "ui-git-discard-no-undo" {},
    /// Git 改动面板界面文字：取消
    UiGitCancel = "ui-git-cancel" {},
    /// Git 改动面板界面文字：丢弃
    UiGitDiscard = "ui-git-discard" {},
    /// Git 改动面板界面文字：Git 改动
    UiGitMenuView = "ui-git-menu-view" {},
    /// Git 改动面板界面文字：{ $repo } · { $branch }
    UiGitBranchTooltip = "ui-git-branch-tooltip" { repo: String, branch: String },

    // ---- 键盘设置页与恢复默认设置（GPUI） ----
    /// GPUI 界面文字：ui-keys-page
    UiKeysPage = "ui-keys-page" {},
    /// GPUI 界面文字：ui-keys-group-other
    UiKeysGroupOther = "ui-keys-group-other" {},
    /// GPUI 界面文字：ui-keys-hint
    UiKeysHint = "ui-keys-hint" {},
    /// GPUI 界面文字：ui-keys-recording
    UiKeysRecording = "ui-keys-recording" {},
    /// GPUI 界面文字：ui-keys-unbound
    UiKeysUnbound = "ui-keys-unbound" {},
    /// GPUI 界面文字：ui-keys-fixed-tabs
    UiKeysFixedTabs = "ui-keys-fixed-tabs" {},
    /// GPUI 界面文字：ui-keys-fixed-tabs-key
    UiKeysFixedTabsKey = "ui-keys-fixed-tabs-key" {},
    /// GPUI 界面文字：ui-keys-reserved
    UiKeysReserved = "ui-keys-reserved" { chord: String },
    /// GPUI 界面文字：ui-keys-needs-modifier
    UiKeysNeedsModifier = "ui-keys-needs-modifier" {},
    /// GPUI 界面文字：ui-keys-conflict-title
    UiKeysConflictTitle = "ui-keys-conflict-title" { chord: String },
    /// GPUI 界面文字：ui-keys-conflict-body
    UiKeysConflictBody = "ui-keys-conflict-body" { chord: String, existing: String, new: String },
    /// GPUI 界面文字：ui-keys-replace
    UiKeysReplace = "ui-keys-replace" {},
    /// GPUI 界面文字：ui-keys-restore-item
    UiKeysRestoreItem = "ui-keys-restore-item" {},
    /// GPUI 界面文字：ui-keys-restore-all
    UiKeysRestoreAll = "ui-keys-restore-all" {},
    /// GPUI 界面文字：ui-keys-customized
    UiKeysCustomized = "ui-keys-customized" {},
    /// GPUI 界面文字：ui-reset-menu
    UiResetMenu = "ui-reset-menu" {},
    /// GPUI 界面文字：ui-reset-title
    UiResetTitle = "ui-reset-title" {},
    /// GPUI 界面文字：ui-reset-body
    UiResetBody = "ui-reset-body" {},
    /// GPUI 界面文字：ui-reset-confirm
    UiResetConfirm = "ui-reset-confirm" {},
    /// GPUI 界面文字：ui-reset-failed
    UiResetFailed = "ui-reset-failed" { detail: String },
    /// GPUI 界面文字：ui-reset-done
    UiResetDone = "ui-reset-done" {},
    /// GPUI 界面文字：ui-keys-cmd-open-launcher
    UiKeysCmdOpenLauncher = "ui-keys-cmd-open-launcher" {},
    /// GPUI 界面文字：ui-keys-cmd-find
    UiKeysCmdFind = "ui-keys-cmd-find" {},
    /// GPUI 界面文字：ui-keys-cmd-find-next
    UiKeysCmdFindNext = "ui-keys-cmd-find-next" {},
    /// GPUI 界面文字：ui-keys-cmd-find-prev
    UiKeysCmdFindPrev = "ui-keys-cmd-find-prev" {},
    /// GPUI 界面文字：ui-keys-cmd-prev-block
    UiKeysCmdPrevBlock = "ui-keys-cmd-prev-block" {},
    /// GPUI 界面文字：ui-keys-cmd-next-block
    UiKeysCmdNextBlock = "ui-keys-cmd-next-block" {},
    /// GPUI 界面文字：ui-keys-cmd-toggle-sidebar
    UiKeysCmdToggleSidebar = "ui-keys-cmd-toggle-sidebar" {},
    /// GPUI 界面文字：ui-keys-cmd-toggle-sender
    UiKeysCmdToggleSender = "ui-keys-cmd-toggle-sender" {},
    /// 快捷键设置页：命令搜索。
    UiKeysCmdOpenPalette = "ui-keys-cmd-open-palette" {},
    /// GPUI 界面文字：ui-keys-cmd-toggle-filer
    UiKeysCmdToggleFiler = "ui-keys-cmd-toggle-filer" {},
    /// GPUI 界面文字：ui-keys-cmd-zoom-in
    UiKeysCmdZoomIn = "ui-keys-cmd-zoom-in" {},
    /// GPUI 界面文字：ui-keys-cmd-zoom-out
    UiKeysCmdZoomOut = "ui-keys-cmd-zoom-out" {},
    /// GPUI 界面文字：ui-keys-cmd-zoom-reset
    UiKeysCmdZoomReset = "ui-keys-cmd-zoom-reset" {},
}
