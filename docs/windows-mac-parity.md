# Windows GPUI 与 Mac 功能核对

本表按用户实际操作入口核对 2026-11-18 的 AppKit 和 GPUI 集成版。Mac 基准是本仓库 [AppDelegate 菜单](../apple/Kanso/App/AppDelegate.swift)、[主窗口](../apple/Kanso/App/MainWindowController.swift) 和各面板，规格索引见 [docs/spec](spec/README.md)。下列 Rust 路径的 `ui/` 与 `app/` 均位于 `crates/kanso-gpui/src/`。

证据标记分别为 **VM 操作**（隔离 Windows 桌面实际点击、键入或选择文件），**VM 自检**（Windows 程序驱动自身的处理函数），**单测**（指定逻辑或 core 测试），**代码**（接线可追溯，尚待操作），**缺口**（没有找到对应实现）。同一行中证据只覆盖写明的行为。构建成功和截图能分别证实编译、显示，不能单独证明菜单执行、文件上传或会话重连。

## 窗口、标签栏、分屏与退出

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 标题栏拖动、双击缩放、窗口按钮 | 自绘标题栏和系统窗口命中测试；**VM 操作** 200% 比例下拖动、最大化/还原、最小化、关闭。见交接中的单行标题栏实测 | 多显示器、跨 DPI 拖动 |
| 关闭后恢复大小、最大化状态，记住各面板 | Mac `WindowStateStore`；Windows `ui/window_layout.rs` 分别读写窗口位置、尺寸、最大化与侧栏/Sender 显隐和尺寸，`ui/shell_filer.rs` 保存 Filer 布局。**代码** | 以相同临时库逐项重启复查窗口和面板；拔除原显示器后的可见区域回落 |
| 显示或收起竖向标签栏 | 标题栏左侧按钮、视图菜单、Ctrl+Alt+←；**VM 操作** 已见主界面开关，见交接 | 与窄窗、多个分屏同时使用 |
| 本地路径筛选，Esc 清空 | 左上搜索只匹配能确认的本地路径，空结果提示。源 `ui/sidebar.rs`、`app/tab.rs`；**单测** 路径匹配 | 混合本地/SSH/SFTP 标签的完整录屏；规格额外的类型/活动筛选器需与 Mac 现有入口分开核对 |
| 单击标签、标题/副标题、颜色、图标、未读和运行指示 | 行按下切换，路径和分支、活动/退出状态从 `app/tab.rs`、`app/workspace.rs` 映射；**VM 自检** 切换、**单测** `unread_set_when_background_tab_finishes_and_cleared_on_activate` | 长路径末尾截断、AI 品牌图标、窄栏和长列表；自动滚动使非可视活动项露出需实测 |
| 左栏同组拖动重排 | `ui/sidebar.rs` → `TabReorder` → `Workspace::move_tab`；超过 5 逻辑像素显示预览，正常松开提交，过滤时禁用。**单测** `click_preview_move_back_and_group_boundaries` | **VM 操作待验** 拖回原位、触控板滚动中断、移出窗口松开、行中关闭按钮 |
| 左栏空白双击、右键、加号 | 双击复制当前会话，右键空白和加号打开启动器。**代码** `ui/sidebar.rs`、`AppController::duplicate_active_tab`；SSH 复用连接由 core 负责 | 双击空白复制 SSH/SFTP 时原认证不会重弹，当前桌面尚未复验 |
| 标签右键编辑来源会话、关闭当前/其他/左/右/全部 | `app/menus.rs::tab_menu`、`ui/shell.rs`，按当前分屏禁用空操作。**VM 自检** 右键菜单/关闭确认，**单测** 分组作用域 | 传输进行中确认文案及多连接列表 |
| 水平/垂直分屏、循环上/下个分屏、拖隔条 | `app/layout.rs`、`app/workspace.rs`、`ui/shell.rs`；**VM 自检** 拆分/缩放/关闭归并，**单测** 比例规范化及最小尺寸 | 多级嵌套在不同缩放下拖边界 |
| 把一个标签移到其他分屏中间或边缘、跨窗移动 | **单测** `move_tab_across_panes_keeps_session_and_collapses_empty_source` 和 `move_tab_to_edge_creates_pane_on_that_side` 验证模型；实际侧栏本轮接入同组重排 | **缺口** 跨分屏/跨窗口拖放入口未由这两项模型测试证明 |
| 关闭最后一个标签、没有会话的启动页、退出确认 | `ui/shell_data.rs::empty_state` 提供本地 shell、新会话、导入 OpenSSH；`app/confirm.rs` 处理退出、活跃会话/传输。**VM 自检** 关闭确认；自动自检有专用退出条件 | 实际关闭最后标签后从三个空状态入口分别恢复；用户工作区退出/重新打开 |

## 菜单、命令面板与会话管理

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| Kanso → 设置、关于、开源许可、退出 | 设置及退出有实际 GPUI 入口；**VM 操作** 设置和窗口关闭。Mac 另有关于、许可正文和系统隐藏/显示 | 关于及第三方许可阅读入口未用 Windows 打开；隐藏其他应用是 macOS 系统动作 |
| 会话 → 新建会话、新建本地、上/下标签、上/下分屏、转到 1–9、关闭 | `app/menus.rs`、`app/shortcuts.rs`、`ShellView::run_command`；**VM 自检** 新建/切换/关闭，**单测** 固定 Alt+1–9 | 逐个菜单点击与超过 9 个标签 |
| 会话 → 时间戳与行号、文件传输、OpenSSH、数据导入/导出 | 分别走 gutter、XMODEM 对话框、`ui/ssh_import.rs`、`ui/shell_data.rs`；操作证据见下面终端和数据表 | 各菜单层级、启用条件和快捷键标签需按当前最终构建复看 |
| 编辑 → 复制/粘贴/全选/清屏、前后命令块、查找、命令建议和清历史 | `app/menus.rs`、`ui/view.rs`、`ui/shell_completion.rs` 实现命令；**单测** 已含可改绑编辑操作、搜索、建议。真实搜索见下表 | 每个编辑项在无会话、纯 SFTP、输入框聚焦时的禁用/路由 |
| 视图 → 标签栏/Filer/Sender、定位目录、命令面板、拆分、字号、外观、显示设置、键盘快捷键；帮助 → 恢复默认 | 面板与拆分调度见 `ui/shell.rs`；显示/键盘菜单直接选中设置页。**VM 操作** 打开面板、设置、搜索；**单测** 菜单编解码 | 完整菜单逐项与恢复默认确认/取消，按最终版本验收 |
| 命令面板搜索标签、保存会话、菜单 | 标题栏中部和 Ctrl+Shift+P，方向键/Tab/Shift+Tab/Enter/Esc；**VM 操作** 切回 CMD、连接保存的 PowerShell、从搜索打开设置，窄窗可用 | 中文输入法组合、多命令同名、筛选时禁用项 |
| 命令面板搜当前目录文件，在 Filer 显示并选中 | Mac `CommandPalettePresenter` 回调 `filer.reveal`；Windows `ui/toolbar.rs` 读取当前本地会话目录，`ui/shell_filer.rs::reveal_palette_file` 导航并选择。**代码** | 切换目录、切换会话时丢弃旧结果；普通文件和目录分别选中，SSH/无 CWD 不展示本地文件组 |
| 加号/空白右键启动器，搜索、选中、按分组显示、齿轮设置 | `ui/launcher.rs`、`app/launcher.rs`；**VM 操作/自检** 打开启动器、保存并重开本地会话 | 大量已保存会话、空组、长文本、中文模糊查找 |
| 启动器会话右键连接、编辑、移组/移出、删除；组右键改名、删除；保存区新建组 | `profile_menu`、`group_menu`、`saved_menu` 和 `ui/launcher.rs` 确认结果写入 core；**单测** 菜单动作、分组删除影响与数据一致性 | 临时库逐项执行、取消删除、移组后重新打开；删除恢复不在 Mac 的操作承诺内 |
| 会话编辑器本地/SSH/SFTP 类型、名称与颜色、保存或连接 | `ui/session_editor.rs`、`app/sessions.rs`；**VM 操作** 类型、颜色、Tab 顺序、窄窗滚动及保存后重新打开 | 修改使用中的保存会话不意外重连 |
| Windows 本地 Shell 选择 | 默认、CMD、Windows PowerShell、检测到的 PowerShell 7、自定义命令；**VM 操作** 已独立启动 CMD/PowerShell；其他选项依安装状态 | 自定义带空格可执行路径和参数、指定起始目录/locale/编码 |
| SSH/SFTP 配置，认证方式、端口、用户名、私钥文件、跳板等 | `ui/session_editor.rs`、`app/sessions.rs`、core 连接实现。**VM 操作** 专用本机回环私钥 SFTP，非真实用户凭据 | 密码、密钥口令、跳板链及代理命令分别连接，取消认证后确保无残留对话框 |
| 首次主机密钥、变更警告、凭据提示、keyboard-interactive | `app/prompts.rs`、`app/prompt_dialogs.rs`、`ui/shell.rs`；**单测/代码** 提示响应与队列。Windows Credential Manager 与 Mac 钥匙串有不同宿主 | 独立 SSH fixture 下逐项确认/拒绝，包括 OTP、多问题回显、断开提示与旧算法说明 |
| .kanso 导出、选择文件导入、导入确认及结果 | **VM 操作** 原生文件选择器、导出文件、再次导入显示 3 项更新、0 项跳过；验收物见 `vmqa/proof/export-proof.kanso`、`lt-import-confirm.jpg` 和 `lt-import-result.jpg` | 文件损坏/错版本、重名合并、导入后 Sender/历史引用，需分别用隔离库确认 |
| 从 OpenSSH 配置选择单个主机、多选或取消 | **VM 操作** 临时 HOME 中的两个别名只选一个，再次导出核对库中恰有一项；勾选及结果见 `lt-openssh-selection.jpg`、`lt-openssh-result.jpg`；核心读取见 `real_sshconfig.rs` | 多文件 Include、通配符跳过与 ProxyJump 映射呈现 |

## 终端输出、输入、搜索和状态栏

Mac 对照 `apple/Packages/TerminalKit/Sources/TerminalKit/`、`docs/spec/terminal.md`、`command-blocks.md`、`local-shell.md` 和 `encoding-transfer.md`。Windows 绘制和输入分别在 `render.rs`、`ui/element.rs`、`ui/view.rs`。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| Shell 运行、窗口缩放、回滚/实时输出、清屏 | **VM 操作** CMD、PowerShell 输入命令及实际输出，**代码** ConPTY 尺寸来自活动分屏，清屏由 core 处理 | 跨 DPI 缩放过程中输入不丢字；长输出滚回、清屏后搜索、TUI 主/备用屏切换 |
| ANSI 16/256/真彩、粗斜体、下划线、反色、dim、隐藏文字、宽字符和光标 | 终端实际预设进入 `render.rs`；**单测** ANSI/语义色保留、选区覆盖、隐藏字、dim 顺序、宽字符占位；**VM 操作** 深色主题的示例输出 | 逐一目测渲染字体、组合字符/emoji、反白块光标及高 DPI 边框 |
| 未显式设置 ANSI 前景的输出按类别上色；提示符路径标签 | 共享 `kanso-term/src/semantic.rs` 分词；`theme.rs` 按选择的预设取色，`ui/element.rs` 绘制 OSC 777 标签；**单测** 实际明暗预设、标签背景/边框、最低对比度；Mac/Windows 主题示例图见证据索引 | 多语言软换行、日志洪流、退出全屏后第一帧的着色和滚动帧率 |
| 编辑 → 在终端查找，大小写、正则、前/后匹配、关闭 | Ctrl+Shift+F、F3/Shift+F3；`app/find.rs` 与 `ui/find_bar.rs`。**VM 自检** `find-count`、`find-invalid-regex`、`find-close`；**单测** core 非法正则错误与明暗预设高亮色。当前、普通匹配与鼠标选区有独立优先级 | 中文输入查找、搜索跨回滚、用鼠标反复切换选项；当前匹配移出视口后回到命中行 |
| 时间戳/行号栏的四种模式，底部按钮，AI 会话默认隐藏 | `app/gutter.rs`、`ui/decor.rs`；**VM 自检** `gutter-on`、`gutter-off`；**单测** 模式、宽度及行号显示 | 每种模式在宽字符和软换行的对齐、以真实 AI 工具启动/退出 |
| 命令分隔线、成功/失败标记、单击选块、跳上/下块，右键复制命令或输出 | `app/blocks.rs`、`ui/view.rs`、`app/menus.rs`；**单测** 块命中、滚动裁剪、失败条、选择状态与右键条件。终端自检里的 block-jump 仅记探测信息 | 具备 OSC 133 提示符的实际 shell 完成成功/失败命令，确认点击、跳转和两种复制内容 |
| 文本选区、双击选词、三击选行、全选，菜单/快捷键复制粘贴 | `ui/view.rs`、`ui/clipboard.rs`。Ctrl+Shift+C/V/A，可改绑，Shift+Insert 为粘贴别名；选区优先于选中的命令块。**单测** `clipboard_actions_drop_old_aliases_and_keep_menu_labels_in_sync` | 在专用测试桌面完整验证多行、换行、组合字符和前台 TUI；自动测试不用用户剪贴板 |
| Windows 粘贴多文件路径、文本和纯图片；把资源管理器的文件拖入终端 | 新的 Win32 剪贴板入口按文件列表/文本/图像识别，路径和 AI 模式交给 core。`ui/filer_drag.rs` 将外部拖入文件送至指针下的终端。**代码/单测** 路径插入与本地拖入状态 | 专用桌面的文件名含空格、多文件、纯图像、bracketed paste 及 AI CLI 对照 |
| 右键菜单或复制/粘贴模式、中键粘贴、TUI 鼠标上报、Shift 临时绕过 | 编辑菜单切换右键模式，`ui/view.rs` 决定何时将鼠标事件送程序；右键菜单支持复制、粘贴、全选、清屏、在 Filer 显示目录，有命令块时多两个复制项。**代码** | 真实鼠标中键、tmux/vim 的 SGR 鼠标上报、Shift 绕过与每个设置组合 |
| Ctrl+单击链接并交给系统打开 | `app/links.rs`、`ui/view.rs`，支持文本 URL/OSC 8，允许 http/https/ftp/ftps/mailto；**VM 自检** `link-detect`，**单测** 允许协议和边界 | Windows 系统关联实际开启、尾随标点与 Ctrl+悬停光标 |
| IME、宽字符、方向/功能键、Ctrl 组合键、焦点切换 | `ui/view.rs` 实现 GPUI 输入法接口，`keys.rs` 处理键码，组字时抑制建议与全局快捷键。**单测** 码点/修饰键路径 | 微软拼音/日文输入法候选位置，选中文字时 Ctrl+C 与运行程序时 Ctrl+C，拆分后焦点 |
| 跟踪 CWD、命令活动、退出/重连、AI CLI 轮次 | 终端 OSC/输入跟踪由 `kanso-term/src/track/` 与 core 提供；标签状态、Sender 和目录定位消费同一状态。**单测/代码** | 本地 CMD/PowerShell、SSH 的 OSC 7 与主机名，AI 多轮、后台完成提示、SSH 断线重连分别验证 |
| 底部 Filer/时间轴按钮、CWD、会话状态、旧 SSH 算法提示、时钟和 Sender 运行提示 | `ui/status_bar.rs`、`app/status_bar.rs`；**VM 操作** Filer/时间轴与时钟可见，**单测** 本地时钟分钟边界、路径尾部截断、Legacy SSH 多跳、无活动会话 | 实际连接中/关闭/重连，运行中收起 Sender、悬停 legacy 算法清单；窄窗通知不遮必要按钮 |
| 选择会话编码/locale、收发时转码 | 编辑器 `ui/session_editor.rs` 的编码列表，进入共用核心；终端协议传输单独接管原始字节。**代码** | 支持列表的代表性多字节编码在 SSH 双向输入/输出，分片字符及不可编码键入 |
| ZMODEM 自动收发、XMODEM 128 字节/1K 发送和接收，进度、取消 | 会话 → 文件传输，`app/term_transfer.rs`、`ui/shell_filer.rs`；独立协议端与真实 lrzsz 对端 **8 次互操作通过**，见证据索引 | Windows 程序实际连到协议端：系统选择器、接管输入、取消后终端恢复、传输时清屏/切标签 |

## 文件浏览器、Git 与远程编辑

Mac 的具体行为及入口见 [Filer 规格](spec/filer.md)，实现分别在 `apple/Kanso/App/Filer/`、`apple/Kanso/App/Git/`。本表分别核对终端侧边 Filer 与纯 SFTP 工作区。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 辅助 Filer 开关、拖宽、展示当前活动标签文件 | Ctrl+Alt+→ 或状态栏，`ui/shell_filer.rs` 保存宽度和显隐；每个会话保留浏览状态。**VM 操作** 主窗口同时显示终端与面板，SSH 辅助 Filer 展示对端两个文件 | 重连后仍是正确远端身份；跨分屏切换不串目录，宽度重启恢复 |
| 终端右键/快捷键/面板按钮定位当前目录 | Ctrl+Shift+J，调用 `ui/shell_filer.rs`，`app/filer_state.rs` 拒绝过期结果。**单测** `stale_locate_results_are_ignored` | CMD/PowerShell 当前目录及 SSH 主机名上报，目标读失败后旧目录标记 |
| 纯 SFTP 双栏：左远端、右本地，分别导航；中间上传/下载 | **VM 操作** 专用回环 SFTP，见 `lt-sftp.jpg`；实际传输见下方。打开纯 SFTP 标签时辅助 Filer 让位 | 断开/重连、文件与目录双击差别、各栏目录与选择恢复 |
| 路径输入、家目录、向上、刷新、新目录、路径/名称复制、打开/打开方式 | 工具栏和条目/空白右键见 `ui/filer_panel_view.rs`、`ui/filer_panel.rs`；**VM 自检** `listing-ready`、`path-is-target` | Windows 关联程序及“打开方式”系统对话框、路径含空格/非 ASCII、路径复制到隔离剪贴板 |
| 文件名/大小/时间、类型/权限/属主、隐藏文件、排序、统计和选择 | `app/filer_state.rs`、`filer.rs`；**VM 自检** 本地文件/目录、隐藏项可见性，**单测** Ctrl/Shift 和方向键选择、右键保留多选、增量到达维持选择、缺失属性用短线 | 大目录滚动/排序、符号链接目标不可读、远端权限及属主缺失的实际显示 |
| 新建文件/目录、F2 重命名、Delete 删除、退出确认 | **VM 自检** 新建不截断已有文件、删除指定文件、非空目录仍在。**单测** `app/filer_ops.rs` 的校验、危险操作仅明确确认后执行 | F2 冲突后保留新名字、取消、多选删除、只读/权限错误；SFTP 非空目录独立验证 |
| 上传/下载、同名跳过/替换/全部、队列与取消 | **VM 操作** 回环 SFTP 上传并下载，源/目标 SHA-256 一致；覆盖对话框选跳过保留原 SHA，替换后等于新版 SHA。见 `lt-sftp-conflict.jpg` 和 `sftp-result.txt`；**单测** 取消及批量冲突决策 | 目录层级、取消进行中的传输及下一项继续，覆盖对话框显示的每个文件名与大小 |
| 从资源管理器拖入 Filer；从 Filer 拖到终端 | `ui/filer_drag.rs`；远端条目拖动需先取临时副本，松开过早取消；**单测** `drop_while_preparing_is_refused_and_cancelled`、`remote_ready_then_drop` | 原生拖入远端 Filer 的上传目标，多个文件，以及拖到另一个分屏的终端 |
| Mac 向 Finder/桌面拖出本地或远端文件 | **缺口** 当前 Windows Filer 的 GPUI 拖动目标仅限应用内，没有向 Explorer/桌面发起系统文件拖放；应用内插入路径不能覆盖这个入口 | Windows 原生拖出及远端准备、失败、取消 |
| 远端文件双击/Enter/右键打开，系统应用保存后回传 | `ui/filer_edit.rs` → 临时副本 → 监测大小/mtime → 防抖上传；**VM 自检** `--remote-edit-test` 10/10，使用临时文件，不含外部编辑器或真实网络 | 专用 SFTP + 系统编辑器闭环，正常保存/替换保存实际远端内容校验 |
| 远端内容冲突、上传失败、稍后处理、退出时保留未同步副本 | **单测** `conflict_decisions`、`retry_schedule`、`reopen_never_overwrites_unsynced`、`close_retains_unsynced` | 远端同时修改后三个按钮、断线/重连及重开副本，出现提示时不会误改远端 |
| 右上 Git 按钮或视图菜单；显示仓库/分支、文件状态与行数，区分空状态 | `ui/shell_git.rs`、`ui/git_panel.rs`；**单测** 仅已连接的本地终端跟随仓库及请求合并。**VM 自检** 临时仓库的初次运行 8/13，提交身份导致后续检查失败 | CMD 切入真实临时仓库时发现缺 CWD，正在验证修正；worktree、子模块、无 Git 与非仓库状态 |
| Git 展开/收起 diff、新增/删除、行内改动、二进制/截断、复制路径、手动刷新 | `app/git_panel.rs`、`ui/git_panel.rs`，**单测** 差异行配对、行内高亮、刷新重读展开项 | 宽文件名、超长行、横向滚动、二进制和压缩后提示，复制正确相对路径 |
| Git 提交全部改动，单文件或全仓“丢弃”确认 | 面板提交说明、按钮和更多菜单。**单测** `commit_validation`、写入期间延后跟随；自检有 `commit-created`、`discard-restores-file` 等断言 | 临时仓库独立身份后的自检结果及实际确认/取消；失败保留说明，无关文件不动 |

## Sender 与补全

Mac 入口见 [Sender](spec/sender.md) 和 [命令建议](spec/completion.md) 中已确定的交互。Windows 对应 `ui/sender.rs`、`ui/shell_sender.rs`、`ui/shell_completion.rs`。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 视图菜单开关 Sender、拖动高度、草稿保留，运行中收起仍有状态提示 | Ctrl+Alt+↓；`ui/shell_sender.rs` 的调度独立于面板绘制。**VM 操作** 面板展开，**单测** `panel_height_and_drag`、`progress_text_shows_rounds_and_countdown` | 收起期间继续发送，点击状态提示恢复面板；跨重启显隐/高度见窗口行 |
| 编写、Enter 发送、Shift+Enter 换行；不加行尾/LF/CRLF | **VM 自检** 对 CMD 选择 CRLF 后检查独立一行命令实际输出，排除把命令回显当作执行成功。默认行尾 LF；**单测** `enter_key_meaning` | 输入法确认、PowerShell 和 SSH 多行、空白行/不加行尾，发送后草稿保留 |
| 目标“当前”/“全部”、重复次数/间隔、开始/停止、状态汇报 | `app/sender.rs` 与 core 调度；**单测** `request_targets_follow_the_spec`、输入限值、计划轮次、提示。纯 SFTP、断线、锁定或传输中排除 | 两个终端同时执行时对各自实际输出取证；中途新建/关闭，停止后的已发送行数 |
| 保存/更新/取消编辑，选择全局、会话类型、分组或指定配置 | `app/sender.rs` 与 core 库；**单测** 更新复用 ID、恢复最初草稿、无配置时回退可用范围、1000 条上限 | 一份隔离库重启后查看每个作用域；变更所属组后显示正确集合 |
| 点击已保存命令执行，右键编辑/重复/改行尾/改作用域/删除；分页 | **单测** `snippet_menu_marks_the_current_values`、`paging_clamps_to_a_valid_page`、排序/提示。入口见 `ui/sender.rs` | 三页数据和同名项、取消删除；按当前目标使用条目保存的行尾执行 |
| 编辑菜单开关命令建议/清历史，键入时被动显示，↓ 导航，Tab 接受/替换，Esc 关闭 | **单测** `passive_state_only_down_enters_the_list`、`tab_is_held_back_while_a_request_is_pending_and_forwarded_otherwise`、仅接受最新结果、输入法抑制；关闭并清除历史需确认 | 在 CMD/PowerShell/SSH 的实际提示符键入，Tab 原生补全交接、中文光标定位、清历史后前后对比 |
| 历史、当前目录路径、命令/工具参数、Kubernetes 上下文/命名空间 | `kanso-complete` 生成候选，`ui/shell_completion.rs` 异步读取本地或远端来源；**单测** 去重排序、最多八项、超时及取消旧结果 | 临时 kubeconfig / 模拟 SSH 路径，慢响应时快速切换会话，候选点击正确替换 |

## 设置、配色与快捷键

Mac 基准为 `apple/Kanso/App/Settings/`。Windows 设置为独立窗口；`ui/settings.rs`、`ui/settings_keys.rs`、`app/settings.rs`、`app/keybindings.rs` 对应写入和派发。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 通用/显示/键盘设置导航；直接打开显示或键盘设置页 | 设置窗口左侧导航，视图菜单可以直接进入指定页；**VM 操作** 深浅色设置截图。打开设置时读取当前数据目录 | 设置已打开时通过菜单再切页、返回主窗口焦点、垂直空间较小时滚动到末项 |
| 外观跟随系统、浅色、深色；明/暗各自选择终端配色 | 设置 → 通用，主窗口和已存在终端订阅变化。**VM 操作** 切换深浅色和自定义 Ocean，见 `r3-screens/windows-ocean.jpg`；**单测** 实际颜色查表与持久 ID | 运行中切换 Windows 系统外观，两个同时打开的终端和设置窗共同更新 |
| 将 JSON 放入主题目录、重新发现、自定义名称下拉选择、重启恢复/移除回落 | 两端共用 v1 文件格式，目录按钮直接打开实际目录，见[主题安装说明](design-tokens.md)与 [JSON Schema](../design/terminal-theme-v1.schema.json)。**Intel Mac 实机** 发现、切换、恢复与回落；**VM 操作** 安装 Ocean 并切换，深色终端输出随之改变 | Windows 删除、改名、损坏、放回原文件再回设置；明色和深色各自的选项不被覆盖 |
| 通用 → 系统/简中/英文；语言即时应用到主窗、设置及现存输入框 | 持久化设置 `appearance.language`；`WidgetText` 让占位符和下拉项在绘制时翻译。设置标题、打开的查找结果、可用字体/默认字体项随语言更新。**代码** | Windows 在设置打开时去回切换，同时检查 Sender、Filer、命令面板、设置原生标题、新旧终端 |
| 显示 → 终端字体（等宽候选/手填字体）、字号/行高、界面字体和字号 | 字体名异步探测，未知字体保留输入并提示；数值进设置模型。**VM 自检** 显示设置；**单测** 读写后复原、范围和默认值 | 已装 Nerd Font、系统缺字体、数字输入框 Enter/失焦与上下限，以及多显示器的实际字形 |
| 显示 → 光标块/下划线/竖线和闪烁，回滚行数、语义着色、最低对比度 | `app/settings.rs`、`render.rs`，按每个会话更新。**单测** 兼容既有数据、最低对比度与语义/显式 ANSI 先后顺序，**VM 操作** 深浅设置页可见 | 终端正在跑 TUI 时切换、滚动长输出后调回滚上限、关闭/开最低对比度对暗色文字的效果 |
| 键盘页分组、录制/取消/清除、保留键、冲突取消/替换，单项或全部恢复 | `ui/settings_keys.rs`；**单测** `record_handles_special_keys`、`conflict_detected_and_replaced`、`clear_restore_and_restore_all`、恢复时检查占用、`storage_round_trip`；显示值、全局派发与菜单标签读取同一份快捷键 | 在专用桌面给一个可见操作重新录制，确认新键执行、旧键释放、菜单文字、重启仍生效及恢复 |
| 设置恢复默认，输入框文本编辑与撤销/重做 | 帮助 → 恢复默认走确认；`ui/widgets/text_input.rs` 与 `text_area.rs` 注册 Undo/Redo。**单测** 设置重置与独立数据编解码；Git/Filer 删除有独立确认 | 在测试库验证只重置约定的设置、会话与命令保留；中文组合输入、选择后编辑及多行撤销 |
| 窗口菜单最小化/缩放；应用关于、开源许可 | Mac 菜单入口见 `AppDelegate.makeMainMenu`。Windows 最小化/最大化通过系统窗口按钮，**VM 操作** 已核对按钮；开源许可正文需用最终交付物检查 | 发行包包含的许可证文件与应用内入口分别检查 |

## 规格中另列的入口

`ui-shell.md` §2、§6 中还提到类型/活动筛选按钮、编辑菜单“选中即复制”、Direct 版检查更新与帮助教程。当前 `AppDelegate.makeMainMenu` 的 Mac 菜单没有后三个条目。对照 Mac 操作时，应用内“关于”和“开源许可”有实在入口，需要分别检查；Windows 特有的右键模式在编辑菜单切换并随面板设置保存。

## 验证入口与复查材料

- 仓库可重复运行 `cargo test -p kanso-gpui --lib`，其中 `render::tests::terminal_preset_search_colors_match_mac` 检查两套内置配色，`render::tests::dim_uses_final_colors_and_chip_runs_follow_cells` 检查输出和提示符色块，`app::tab::tests::click_preview_move_back_and_group_boundaries` 检查同组拖动边界。非法正则的独立 PTY 校验是 `crates/kanso-core/tests/real_core.rs::selection_search_and_timestamp_mode_delegate_to_the_terminal`。本轮 Mac 上的 GPUI 库测试共 602 项通过。
- Windows 自检入口位于 `crates/kanso-gpui/src/ui/`。主外壳、终端、Filer、Sender、设置五套报告均有通过记录，增加的远程编辑自检另计。复查时给应用传单独的 `--data-dir`，使用临时 HOME 和专用 SSH fixture，不接触用户凭据、剪贴板或会话。
- 本轮联调机器的临时材料在 `/tmp/kanso-polish/`。Mac 的 `theme-proofs/mac-terminal-{light,dark,ocean}.png`、`mac-semantic-{light,dark}.png` 和 Windows 的 `r3-screens/windows-ocean.jpg`、`lt-find.png` 用于外观对照。`vmqa/proof/` 保存 SFTP、OpenSSH、导出/导入实拍，`vmqa/proof4/lt-virtual-filer.jpg` 为 SSH 辅助 Filer。协议独立互操作的逐项结果见 `vmqa/lrzsz.log`。
- 调试材料不随安装包分发。版本与后续实机结果见[Windows 交接记录](windows-gpui-handoff.md)。单测、VM 程序自检、真实外部程序和协议对端各证明对应步骤。
