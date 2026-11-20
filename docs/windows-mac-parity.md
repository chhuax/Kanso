# Windows GPUI 与 Mac 功能核对

本表按用户实际操作入口核对 2026-11-18 的 AppKit 和 GPUI 集成版。Mac 基准是本仓库 [AppDelegate 菜单](../apple/Kanso/App/AppDelegate.swift)、[主窗口](../apple/Kanso/App/MainWindowController.swift) 和各面板，规格索引见 [docs/spec](spec/README.md)。下列 Rust 路径的 `ui/` 与 `app/` 均位于 `crates/kanso-gpui/src/`。

证据标记分别为 **VM 操作**（隔离 Windows 桌面实际点击、键入或选择文件），**VM 自检**（Windows 程序驱动自身的处理函数），**单测**（指定逻辑或 core 测试），**代码**（接线可追溯，尚待操作），**缺口**（没有找到对应实现）。同一行中证据只覆盖写明的行为。构建成功和截图能分别证实编译、显示，不能单独证明菜单执行、文件上传或会话重连。

## 窗口、标签栏、分屏与退出

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 标题栏拖动、双击缩放、窗口按钮 | 自绘标题栏和系统窗口命中测试。**VM 操作** 200% 比例下拖动、最大化/还原、最小化、关闭。见交接中的单行标题栏实测 | 多显示器、跨 DPI 拖动 |
| 关闭后恢复大小、最大化状态，记住各面板 | `ui/window_layout.rs` 读写窗口和面板。**VM 操作** 两个不同 PID 共用临时库，重启后仍最大化为 2048×911 逻辑像素、侧栏收起、Sender 保持展开。再显示侧栏，宽度仍约 302。另在 1120×680 客户区把 Sender 分隔条从 y493 拖到 y395，再开新进程仍为 y395。见 `layout-before-restart.png` 与 `layout-reopened.png` | 非最大化窗口移到新位置再重开、拔掉原显示器后的回落 |
| 显示或收起竖向标签栏 | 标题栏左侧按钮、视图菜单、Ctrl+Alt+←。**VM 操作** 已见主界面开关，见交接 | 与窄窗、多个分屏同时使用 |
| 本地路径筛选，Esc 清空 | 左上搜索只匹配能确认的本地路径，空结果提示。源 `ui/sidebar.rs`、`app/tab.rs`。**单测** 路径匹配 | 混合本地/SSH/SFTP 标签的完整录屏。规格额外的类型/活动筛选器需与 Mac 现有入口分开核对 |
| 单击标签、标题/副标题、颜色、图标、未读和运行指示 | 行按下切换，路径和分支、活动/退出状态从 `app/tab.rs`、`app/workspace.rs` 映射。**VM 自检** 切换、**单测** `unread_set_when_background_tab_finishes_and_cleared_on_activate` | 长路径末尾截断、AI 品牌图标、窄栏和长列表。自动滚动使非可视活动项露出需实测 |
| 左栏同组拖动重排 | `ui/sidebar.rs` → `TabReorder` → `Workspace::move_tab`。**VM 操作** 从 Documents / Downloads 拖为 Downloads / Documents，选中项仍是原会话。**单测** `click_preview_move_back_and_group_boundaries` | 触控板滚动中断、移出窗口松开、行中关闭按钮。过滤时禁用重排 |
| 左栏空白双击、右键、加号 | **VM 操作** 空白双击后出现第三个本地标签，复制保留 Documents 目录。右键空白打开启动器，加号直接打开新建会话 | 双击空白复制 SSH/SFTP，检查既有认证复用 |
| 标签右键编辑来源会话、关闭当前/其他/左/右/全部 | `app/menus.rs::tab_menu`、`ui/shell.rs`，按当前分屏禁用空操作。**VM 自检** 右键菜单/关闭确认，**单测** 分组作用域 | 传输进行中确认文案及多连接列表 |
| 水平/垂直分屏、循环上/下个分屏、拖隔条 | `app/layout.rs`、`app/workspace.rs`、`ui/shell.rs`。**VM 自检** 拆分/缩放/关闭归并，**单测** 比例规范化及最小尺寸 | 多级嵌套在不同缩放下拖边界 |
| 把一个标签移到其他分屏中间或边缘、跨窗移动 | Mac 侧栏 `TabSidebarViewController` 只接 `moveTab(toIndex:)`，跨分屏接口见 KansoUI 的 Workspace 与 WorkspaceTests。Windows 模型有 `move_tab_across_panes_keeps_session_and_collapses_empty_source` 和 `move_tab_to_edge_creates_pane_on_that_side` 单测。两端实际侧栏均接同组重排 | 跨分屏、跨窗口拖放属于需与规格核对的双端入口，未找到 Mac 已发布的对应手势 |
| 关闭最后一个标签、没有会话的启动页、退出确认 | `ui/shell_data.rs::empty_state` 提供本地 shell、新会话、导入 OpenSSH。`app/confirm.rs` 处理退出、活跃会话/传输。**VM 自检** 关闭确认。自动自检有专用退出条件 | 实际关闭最后标签后从三个空状态入口分别恢复。用户工作区退出/重新打开 |

## 菜单、命令面板与会话管理

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| Kanso → 设置、关于、开源许可、退出 | **VM 操作** 打开设置、退出、帮助 → 关于 Kanso（1.0.0）、帮助 → 开源许可。已通过记事本进程确认实际打开安装目录内的 THIRD_PARTY_NOTICES.windows.md，绿色包也包含同一份声明。Mac 的隐藏和显示由系统菜单提供 | 此流程已验 |
| 会话 → 新建会话、新建本地、上/下标签、上/下分屏、转到 1–9、关闭 | `app/menus.rs`、`app/shortcuts.rs`、`ShellView::run_command`。**VM 自检** 新建/切换/关闭，**单测** 固定 Alt+1–9 | 逐个菜单点击与超过 9 个标签 |
| 会话 → 时间戳与行号、文件传输、OpenSSH、数据导入/导出 | 分别走 gutter、XMODEM 对话框、`ui/ssh_import.rs`、`ui/shell_data.rs`。操作证据见下面终端和数据表 | 各菜单层级、启用条件和快捷键标签需按当前最终构建复看 |
| 编辑 → 复制/粘贴/全选/清屏、前后命令块、查找、命令建议和清历史 | `app/menus.rs`、`ui/view.rs`、`ui/shell_completion.rs` 实现命令。**单测** 已含可改绑编辑操作、搜索、建议。真实搜索见下表 | 每个编辑项在无会话、纯 SFTP、输入框聚焦时的禁用/路由 |
| 视图 → 标签栏/Filer/Sender、定位目录、命令面板、拆分、字号、外观、显示设置、键盘快捷键。帮助 → 恢复默认 | 面板与拆分调度见 `ui/shell.rs`。显示/键盘菜单直接选中设置页。**VM 操作** 打开面板、设置、搜索。**单测** 菜单编解码 | 完整菜单逐项与恢复默认确认/取消，按最终版本验收 |
| 命令面板搜索标签、保存会话、菜单 | 标题栏中部和 Ctrl+Shift+P，方向键/Tab/Shift+Tab/Enter/Esc。**VM 操作** 切回 CMD、连接保存的 PowerShell、从搜索打开设置，窄窗可用 | 中文输入法组合、多命令同名、筛选时禁用项 |
| 命令面板搜当前目录文件，在 Filer 显示并选中 | Mac `CommandPalettePresenter` 调 `filer.reveal`，Windows `ui/toolbar.rs`、`ui/shell_filer.rs::reveal_palette_file`。**VM 操作** 从本地 CMD 搜 `qa-local-proof`，出现 Files 分组，Enter 自动打开 Filer，选中 `qa-local-proof.txt` | 切换会话时丢弃旧结果。目录项和 SSH/无 CWD 分支 |
| 空白右键或命令面板打开启动器，搜索、按分组显示、齿轮设置。加号直接新建会话 | `ui/launcher.rs`、`app/launcher.rs`。**VM 操作/自检** 打开启动器、保存并重开本地会话 | 大量已保存会话、空组、长文本、中文模糊查找 |
| 启动器单击或回车连接；右键与「…」为编辑、移组/移出、删除。组右键改名、删除。保存区新建组 | `profile_menu`、`group_menu`、`saved_menu` 和 `ui/launcher.rs` 确认结果写入 core。**单测** 菜单动作、分组删除影响与数据一致性 | 临时库逐项执行、取消删除、移组后重新打开。删除恢复不在 Mac 的操作承诺内 |
| 会话编辑器本地/SSH/SFTP 类型、名称与颜色、保存或连接 | `ui/session_editor.rs`、`app/sessions.rs`。**VM 操作** 类型、颜色、Tab 顺序、窄窗滚动及保存后重新打开 | 修改使用中的保存会话不意外重连 |
| Windows 本地 Shell 选择 | 默认、CMD、Windows PowerShell、检测到的 PowerShell 7、自定义命令。**VM 操作** 已独立启动 CMD/PowerShell。其他选项依安装状态 | 自定义带空格可执行路径和参数、指定起始目录/locale/编码 |
| SSH/SFTP 配置，认证方式、端口、用户名、私钥文件、跳板等 | `ui/session_editor.rs`、`app/sessions.rs`、core 连接实现。**VM 操作** 专用本机回环私钥 SFTP，非真实用户凭据 | 密码、密钥口令、跳板链及代理命令分别连接，取消认证后确保无残留对话框 |
| 首次主机密钥、变更警告、凭据提示、keyboard-interactive | `app/prompts.rs`、`app/prompt_dialogs.rs`、`ui/shell.rs`。**单测/代码** 提示响应与队列。Windows Credential Manager 与 Mac 钥匙串有不同宿主 | 独立 SSH fixture 下逐项确认/拒绝，包括 OTP、多问题回显、断开提示与旧算法说明 |
| .kanso 导出、选择文件导入、导入确认及结果 | **VM 操作** 原生文件选择器、导出文件、再次导入显示 3 项更新、0 项跳过。验收物见 `vmqa/proof/export-proof.kanso`、`lt-import-confirm.jpg` 和 `lt-import-result.jpg` | 文件损坏/错版本、重名合并、导入后 Sender/历史引用，需分别用隔离库确认 |
| 从 OpenSSH 配置选择单个主机、多选或取消 | **VM 操作** 临时 HOME 中的两个别名只选一个，再次导出核对库中恰有一项。勾选及结果见 `lt-openssh-selection.jpg`、`lt-openssh-result.jpg`。核心读取见 `real_sshconfig.rs` | 多文件 Include、通配符跳过与 ProxyJump 映射呈现 |

## 终端输出、输入、搜索和状态栏

Mac 对照 `apple/Packages/TerminalKit/Sources/TerminalKit/`、`docs/spec/terminal.md`、`command-blocks.md`、`local-shell.md` 和 `encoding-transfer.md`。Windows 绘制和输入分别在 `render.rs`、`ui/element.rs`、`ui/view.rs`。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| Shell 运行、窗口缩放、回滚/实时输出、清屏 | **VM 操作** CMD、PowerShell 输入命令及实际输出，**代码** ConPTY 尺寸来自活动分屏，清屏由 core 处理 | 跨 DPI 缩放过程中输入不丢字。长输出滚回、清屏后搜索、TUI 主/备用屏切换 |
| ANSI 16/256/真彩、粗斜体、下划线、反色、dim、隐藏文字、宽字符和光标 | 终端实际预设进入 `render.rs`。**单测** ANSI/语义色保留、选区覆盖、隐藏字、dim 顺序、宽字符占位。**VM 操作** 明暗主题示例输出。Windows 与 Mac 深浅两套实图的 16 个 ANSI 色、真彩 #256BC1 和各自背景均与主题文件精确一致 | 逐一目测渲染字体、组合字符/emoji、反白块光标及高 DPI 边框 |
| 未显式设置 ANSI 前景的输出按类别上色。提示符路径标签 | 共享 `kanso-term/src/semantic.rs` 分词。`theme.rs` 按选择的预设取色，`ui/element.rs` 绘制 OSC 777 标签。Mac 自带的双行路径提示符由 zsh 集成产生。Windows 的 CMD 和 PowerShell 保留自身提示符，另上报工作目录。**单测** 实际明暗预设、标签背景/边框、最低对比度。Mac/Windows 主题示例图见证据索引 | 多语言软换行、日志洪流、退出全屏后第一帧的着色和滚动帧率 |
| 编辑 → 在终端查找，大小写、正则、前/后匹配、关闭 | Ctrl+Shift+F、F3/Shift+F3。`app/find.rs` 与 `ui/find_bar.rs`。**VM 自检** `find-count`、`find-invalid-regex`、`find-close`。**单测** core 非法正则错误与明暗预设高亮色。当前、普通匹配与鼠标选区有独立优先级 | 中文输入查找、搜索跨回滚、用鼠标反复切换选项。当前匹配移出视口后回到命中行 |
| 时间戳/行号栏的四种模式，底部按钮，AI 会话默认隐藏 | **VM 操作** 点击状态栏 Timeline 后同时出现毫秒时间和行号，同一临时数据重启仍显示。截图 `timeline.png`。**VM 自检** `gutter-on`、`gutter-off`。四种模式入口在会话菜单 | 每个单独模式在宽字符和软换行的对齐、以真实 AI 工具启动/退出 |
| 命令分隔线、成功/失败标记、单击选块、跳上/下块，右键复制命令或输出 | `app/blocks.rs`、`ui/view.rs`、`app/menus.rs`。**单测** 块命中、滚动裁剪、失败条、选择状态与右键条件。终端自检里的 block-jump 仅记探测信息 | 具备 OSC 133 提示符的实际 shell 完成成功/失败命令，确认点击、跳转和两种复制内容 |
| 文本选区、双击选词、三击选行、全选，菜单/快捷键复制粘贴 | `ui/view.rs`、`ui/clipboard.rs`。Ctrl+Shift+C/V/A，可改绑，Shift+Insert 为粘贴别名。选区优先于选中的命令块。**单测** `clipboard_actions_drop_old_aliases_and_keep_menu_labels_in_sync` | 在专用测试桌面完整验证多行、换行、组合字符和前台 TUI。自动测试不用用户剪贴板 |
| Windows 粘贴多文件路径、文本和纯图片。把资源管理器的文件拖入终端 | 新的 Win32 剪贴板入口按文件列表/文本/图像识别，路径和 AI 模式交给 core。`ui/filer_drag.rs` 将外部拖入文件送至指针下的终端。**代码/单测** 路径插入与本地拖入状态 | 专用桌面的文件名含空格、多文件、纯图像、bracketed paste 及 AI CLI 对照 |
| 右键菜单或复制/粘贴模式、中键粘贴、TUI 鼠标上报、Shift 临时绕过 | "键盘与鼠标"设置页或编辑菜单切换右键 / 中键模式，`ui/view.rs` 决定何时将鼠标事件送程序。右键菜单支持复制、粘贴、全选、清屏、在 Filer 显示目录，有命令块时多两个复制项。**代码** | 真实鼠标中键、tmux/vim 的 SGR 鼠标上报、Shift 绕过与每个设置组合 |
| Ctrl+单击链接并交给系统打开 | `app/links.rs`、`ui/view.rs`，支持文本 URL/OSC 8，允许 http/https/ftp/ftps/mailto。**VM 自检** `link-detect`，**单测** 允许协议和边界 | Windows 系统关联实际开启、尾随标点与 Ctrl+悬停光标 |
| IME、宽字符、方向/功能键、Ctrl 组合键、焦点切换 | `ui/view.rs` 实现 GPUI 输入法接口，`keys.rs` 处理键码，组字时抑制建议与全局快捷键。**单测** 码点/修饰键路径 | 微软拼音/日文输入法候选位置，选中文字时 Ctrl+C 与运行程序时 Ctrl+C，拆分后焦点 |
| 跟踪 CWD、命令活动、退出/重连、AI CLI 轮次 | 终端 OSC/输入跟踪由 `kanso-term/src/track/` 与 core 提供。标签状态、Sender 和目录定位消费同一状态。**单测/代码** | 本地 CMD/PowerShell、SSH 的 OSC 7 与主机名，AI 多轮、后台完成提示、SSH 断线重连分别验证 |
| 底部 Filer/时间轴按钮、CWD、会话状态、旧 SSH 算法提示、时钟和 Sender 运行提示 | `ui/status_bar.rs`、`app/status_bar.rs`。**VM 操作** Filer/时间轴与时钟可见，**单测** 本地时钟分钟边界、路径尾部截断、Legacy SSH 多跳、无活动会话 | 实际连接中/关闭/重连，运行中收起 Sender、悬停 legacy 算法清单。窄窗通知不遮必要按钮 |
| 选择会话编码/locale、收发时转码 | 编辑器 `ui/session_editor.rs` 的编码列表，进入共用核心。终端协议传输单独接管原始字节。**代码** | 支持列表的代表性多字节编码在 SSH 双向输入/输出，分片字符及不可编码键入 |
| ZMODEM 自动发送与接收 | **Windows VM 实操** 在 SSH 标签中启动独立的 lrzsz 0.13.1 对端。原生“打开”选择上传文件，“另存为”选择下载位置。两次各传输 100003 字节，发送端与接收端 SHA-256 一致，两个对端进程均正常退出 | 其他协议实现的兼容性 |
| XMODEM 手动发送与接收 | **Windows VM 实操** “用 XMODEM-1K 发送”经原生文件选择器上传，“用 XMODEM 接收”经原生保存框下载。两个方向均逐字节核实 100003 字节原文和 93 个末块填充字节（0x1A），对端均正常退出。**协议互操作** Mac 上 lrzsz 0.12.20 与 Windows 上 lrzsz 0.13.1 各 8/8，覆盖 128 字节、1K、CRC 和累加和 | 其他协议实现的兼容性 |
| 传输进度、切标签、清屏和取消 | **Windows VM 实操** 接收 8 MiB 测试文件时切到另一终端，切回后清除缓冲区，进度继续增长。已收到 1088512 字节时取消，对端收到 CAN 并退出。终端保留对端的退出消息和无换行提示符，输入下一条命令得到响应。修复 `e0b288e` 后未再显示在途文件数据。本轮实际用 lsz 在接收约 94% 时取消，对端确认中止，终端恢复输入且没有 ZEOF/ZFIN 尾包乱码。分片测试验证晚到 XON 和保留 OO | 其他协议实现的取消收尾行为 |

## 文件浏览器、Git 与远程编辑

Mac 的具体行为及入口见 [Filer 规格](spec/filer.md)，实现分别在 `apple/Kanso/App/Filer/`、`apple/Kanso/App/Git/`。本表分别核对终端侧边 Filer 与纯 SFTP 工作区。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 辅助 Filer 开关、拖宽、展示当前活动标签文件 | Ctrl+Alt+→ 或状态栏，`ui/shell_filer.rs` 保存宽度和显隐。每个会话保留浏览状态。**VM 操作** 主窗口同时显示终端与面板，SSH 辅助 Filer 展示对端两个文件 | 重连后仍是正确远端身份。跨分屏切换不串目录，宽度重启恢复 |
| 终端右键/快捷键/面板按钮定位当前目录 | Ctrl+Shift+J，调用 `ui/shell_filer.rs`，`app/filer_state.rs` 拒绝过期结果。**单测** `stale_locate_results_are_ignored` | CMD/PowerShell 当前目录及 SSH 主机名上报，目标读失败后旧目录标记 |
| 纯 SFTP 双栏：左远端、右本地，分别导航。中间上传/下载 | **VM 操作** 专用回环 SFTP，见 `lt-sftp.jpg`。实际传输见下方。打开纯 SFTP 标签时辅助 Filer 让位 | 断开/重连、文件与目录双击差别、各栏目录与选择恢复 |
| 路径输入、家目录、向上、刷新、新目录、路径/名称复制、打开/打开方式 | 工具栏和条目/空白右键见 `ui/filer_panel_view.rs`、`ui/filer_panel.rs`。**VM 自检** `listing-ready`、`path-is-target` | Windows 关联程序及“打开方式”系统对话框、路径含空格/非 ASCII、路径复制到隔离剪贴板 |
| 文件名/大小/时间、类型/权限/属主、隐藏文件、排序、统计和选择 | `app/filer_state.rs`、`filer.rs`。**VM 自检** 本地文件/目录、隐藏项可见性，**单测** Ctrl/Shift 和方向键选择、右键保留多选、增量到达维持选择、缺失属性用短线 | 大目录滚动/排序、符号链接目标不可读、远端权限及属主缺失的实际显示 |
| 新建文件/目录、F2 重命名、Delete 删除、退出确认 | **VM 自检** 新建不截断已有文件、删除指定文件、非空目录仍在。**单测** `app/filer_ops.rs` 的校验、危险操作仅明确确认后执行 | F2 冲突后保留新名字、取消、多选删除、只读/权限错误。SFTP 非空目录独立验证 |
| 上传/下载、同名跳过/替换/全部、队列与取消 | **VM 操作** 回环 SFTP 上传并下载，源/目标 SHA-256 一致。覆盖对话框选跳过保留原 SHA，替换后等于新版 SHA。见 `lt-sftp-conflict.jpg` 和 `sftp-result.txt`。**单测** 取消及批量冲突决策 | 目录层级、取消进行中的传输及下一项继续，覆盖对话框显示的每个文件名与大小 |
| 从资源管理器拖入 Filer。从 Filer 拖到终端 | **VM 操作** 应用内拖入和出窗口后用 OLE 拖回终端均插入路径。CMD 实际读取了带空格的文件，Windows ConPTY 也验证 CMD 与 PowerShell 读出带中文、空格和 & 的同一测试内容。OLE 拖回原远端 Filer 无上传，下一次从 Explorer 拖入正常上传，SHA-256 相同 | 纯图片剪贴板及 AI CLI 附件交互见终端条目 |
| Mac 向 Finder/桌面拖出本地或远端文件 | **VM 操作** 单文件、两文件加含子文件目录拖至 Explorer，源文件保留。文件可拖到记事本。Esc 取消后可再拖。远端文件就绪后复制成功，慢速文件持键在窗口外等待后自动出现复制光标。提前松手停止下载；失败只提示一次；重拖提示已在界面验证。OLE 期间窗口重绘，独立下载进度继续增长 | 松手和下载完成间的窄竞态由定向测试覆盖，实机未稳定复现该时序 |
| 远端文件双击/Enter/右键打开，系统应用保存后回传 | `ui/filer_edit.rs` → 临时副本 → 监测大小/mtime → 防抖上传。**VM 自检** `--remote-edit-test` 10/10。**VM 操作** 专用 SFTP 下载后实际保存，自动回传并读取远端内容确认。未知扩展名触发系统应用选择器并成功取消 | 编辑器用先写新文件再替换的保存方式。同名文件位于两个远端时的隔离 |
| 远端编辑冲突、另存副本、退出时保留未同步副本 | **VM 操作** 四按钮、默认焦点、Enter/Esc/遮罩取消、再次保存重问、五种非法名回弹、同名目标不覆盖不重试。成功另存的文件与本地相同，原远端文件保留。另有之前的覆盖验证 | 断线重连与重开副本、远端重新下载后的下一次保存 |
| 右上 Git 按钮或视图菜单。显示仓库/分支、文件状态与行数，区分空状态 | **VM 操作** 真实 CMD 进入仓库、退到仓库外、再进入，Git 面板显示 M/U → 非仓库 → 恢复，见 `vmqa/proof5/cwd.jpg`。**VM 自检** `--git-selftest` 13/13。**Windows ConPTY** PowerShell 5.1 初始目录及含空格目录正确上报。**VM 操作** 在实际 PowerShell 5.1 中进/出/再次进入 repo，标签及 Git 面板切换正确，实际显示分支及 U 文件，见 `ps-cwd-{settled,left,back}.png`。实测旧配置宽 420、拖到 600 后重启仍为 600 且初始关闭，输入 100/9999 分别夹到 280/760 | worktree、子模块、Git 不可用 |
| Git 展开/收起 diff、新增/删除、行内改动、二进制/截断、复制路径、手动刷新 | `app/git_panel.rs`、`ui/git_panel.rs`，**单测** 差异行配对、行内高亮、刷新重读展开项 | 宽文件名、超长行、横向滚动、二进制和压缩后提示，复制正确相对路径 |
| Git 提交全部改动，单文件或全仓“丢弃”确认 | 面板提交说明、按钮和更多菜单。**单测** `commit_validation`、写入期间延后跟随。**VM 自检** `commit-created`、`commit-leaves-clean`、`discard-restores-file`、`discard-leaves-clean` 及刷新均通过 | 实际鼠标确认/取消。失败保留说明，无关文件不动 |

## Sender 与补全

Mac 入口见 [Sender](spec/sender.md) 和 [命令建议](spec/completion.md) 中已确定的交互。Windows 对应 `ui/sender.rs`、`ui/shell_sender.rs`、`ui/shell_completion.rs`。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 视图菜单开关 Sender、拖动高度、草稿保留，运行中收起仍有状态提示 | Ctrl+Alt+↓。`ui/shell_sender.rs` 的调度独立于面板绘制。**VM 操作** 面板展开，**单测** `panel_height_and_drag`、`progress_text_shows_rounds_and_countdown` | 收起期间继续发送，点击状态提示恢复面板。跨重启显隐/高度见窗口行 |
| 编写、Enter 发送、Shift+Enter 换行。不加行尾/LF/CRLF | **VM 自检** 对 CMD 选择 CRLF 后检查独立一行命令实际输出，排除把命令回显当作执行成功。默认行尾 LF。**单测** `enter_key_meaning` | 输入法确认、PowerShell 和 SSH 多行、空白行/不加行尾，发送后草稿保留 |
| 目标“当前”/“全部”、重复次数/间隔、开始/停止、状态汇报 | `app/sender.rs` 与 core 调度。**单测** `request_targets_follow_the_spec`、输入限值、计划轮次、提示。纯 SFTP、断线、锁定或传输中排除 | 两个终端同时执行时对各自实际输出取证。中途新建/关闭，停止后的已发送行数 |
| 保存/更新/取消编辑，选择全局、会话类型、分组或指定配置 | `app/sender.rs` 与 core 库。**单测** 更新复用 ID、恢复最初草稿、无配置时回退可用范围、1000 条上限 | 一份隔离库重启后查看每个作用域。变更所属组后显示正确集合 |
| 点击已保存命令执行，右键编辑/重复/改行尾/改作用域/删除。分页 | **单测** `snippet_menu_marks_the_current_values`、`paging_clamps_to_a_valid_page`、排序/提示。入口见 `ui/sender.rs` | 三页数据和同名项、取消删除。按当前目标使用条目保存的行尾执行 |
| 编辑菜单开关命令建议/清历史，键入时被动显示，↓ 导航，Tab 接受/替换，Esc 关闭 | **单测** `passive_state_only_down_enters_the_list`、`tab_is_held_back_while_a_request_is_pending_and_forwarded_otherwise`、仅接受最新结果、输入法抑制。关闭并清除历史需确认 | 在 CMD/PowerShell/SSH 的实际提示符键入，Tab 原生补全交接、中文光标定位、清历史后前后对比 |
| 历史、当前目录路径、命令/工具参数、Kubernetes 上下文/命名空间 | `kanso-complete` 生成候选，`ui/shell_completion.rs` 异步读取本地或远端来源。**单测** 去重排序、最多八项、超时及取消旧结果 | 临时 kubeconfig / 模拟 SSH 路径，慢响应时快速切换会话，候选点击正确替换 |

## 设置、配色与快捷键

Mac 基准为 `apple/Kanso/App/Settings/`。Windows 设置为独立窗口。`ui/settings.rs`、`ui/settings_keys.rs`、`app/settings.rs`、`app/keybindings.rs` 对应写入和派发。

| Mac 入口及行为 | Windows 入口、结果与证据 | 尚需验收 |
|---|---|---|
| 通用/显示/键盘设置导航。直接打开显示或键盘设置页 | **VM 操作** Ctrl+, 打开独立设置窗，点击顶部通用、显示、键盘页，三页均正确切换。视图菜单支持直接打开指定页，见 `general.png`、`display.png`、`keyboard.png` | 已打开窗口通过菜单再跳页及较矮显示器的底部控件可达性 |
| 外观跟随系统、浅色、深色。明/暗各自选择终端配色 | 设置 → 通用，主窗口和已存在终端订阅变化。**VM 操作** 切换深浅色和自定义 Ocean，见 `r3-screens/windows-ocean.jpg`。**单测** 实际颜色查表与持久 ID | 运行中切换 Windows 系统外观，两个同时打开的终端和设置窗共同更新 |
| 将 JSON 放入主题目录、重新发现、自定义名称下拉选择、重启恢复/移除回落 | 两端共用 v1 文件格式，目录按钮直接打开实际目录，见[主题安装说明](design-tokens.md)与 [JSON Schema](../design/terminal-theme-v1.schema.json)。**Intel Mac 实机** 发现、切换、恢复与回落。**VM 操作** 安装 Ocean 并切换，深色终端输出随之改变 | Windows 删除、改名、损坏、放回原文件再回设置。明色和深色各自的选项不被覆盖 |
| 通用 → 系统/简中/英文。语言即时应用到主窗、设置及现存输入框 | **VM 操作** 在已打开通用页由简中切为 English，模式下拉、页签、字号及字体占位符立即转为英文，关闭并新开进程仍为英文。Git 的提交提示曾保留上一次语言，已改为动态读取并通过本地化测试。持久控件文字由 `WidgetText` 及页面刷新负责 | 最终发行版复查 Git 提交提示、Sender/Filer/命令面板各自先打开再切换及非中英系统语言回退 |
| 显示 → 终端字体（等宽候选/手填字体）、字号/行高、界面字体和字号 | **VM 操作** 手填字号 19、行高 1.35 后终端立即变更，重新启动后显示页保留数值。英文终端字体选择按钮加宽后，已在 Windows 构建 d5895f0 实拍确认呈现完整的 Choose…。字体候选可探测，未安装族保留输入并提示 | 已装 Nerd Font、缺字体、数字上限/下限和跨高 DPI 字形 |
| 显示 → 光标块/下划线/竖线和闪烁，回滚行数、语义着色、最低对比度 | **VM 操作** 选择块光标并取消闪烁，终端变为块光标，重启仍选中 Block、Blinking cursor 仍关。**单测** 回滚、对比度、语义着色及显式 ANSI 的先后顺序 | 真实 TUI 抢占光标、调整长输出回滚上限、最低对比度的屏幕效果 |
| 键盘页分组、录制/取消/清除、保留键、冲突取消/替换，单项或全部恢复 | **VM 操作** 录入已占用快捷键出现冲突，取消后把新会话改为 Ctrl+Shift+Y。旧 Alt+N 回到终端，新键打开会话编辑器，Session → New Session 显示新键。重启仍生效，点击单项恢复后显示 Alt+N。**单测** 冲突替换、清除、恢复全表和保留键 | 在桌面实际点“替换”以及“恢复全部”，试录 Win 系统保留键 |
| 设置恢复默认，输入框文本编辑与撤销/重做 | 帮助 → 恢复默认走确认。`ui/widgets/text_input.rs` 与 `text_area.rs` 注册 Undo/Redo。**单测** 设置重置与独立数据编解码。Git/Filer 删除有独立确认 | 在测试库验证只重置约定的设置、会话与命令保留。中文组合输入、选择后编辑及多行撤销 |
| 窗口菜单最小化/缩放。应用关于、开源许可 | Mac 窗口菜单在 `AppDelegate.makeMainMenu`。**VM 操作** Windows 标题栏最小化/最大化及重启最大化。**VM 操作** 安装版帮助菜单打开了关于窗口及其随包声明，真实文件路径见交接记录 | 多显示器间移动最大化窗口 |

## 规格中另列的入口

`ui-shell.md` §2、§6 中还提到类型/活动筛选按钮、编辑菜单“选中即复制”、Direct 版检查更新与帮助教程。当前 `AppDelegate.makeMainMenu` 的 Mac 菜单没有后三个条目。对照 Mac 操作时，应用内“关于”和“开源许可”有实在入口，需要分别检查。右键模式、中键行为与选中即复制在设置的"键盘与鼠标"页和编辑菜单两处切换，存于 `mouse` 设置分区（ui-shell.md §7.1），两个平台一致。

## 验证入口与复查材料

- 仓库可重复运行 `cargo test -p kanso-gpui --lib`，其中 `render::tests::terminal_preset_search_colors_match_mac` 检查两套内置配色，`render::tests::dim_uses_final_colors_and_chip_runs_follow_cells` 检查输出和提示符色块，`app::tab::tests::click_preview_move_back_and_group_boundaries` 检查同组拖动边界。非法正则的独立 PTY 校验是 `crates/kanso-core/tests/real_core.rs::selection_search_and_timestamp_mode_delegate_to_the_terminal`。最终提交的 Mac GPUI 库测试共 603 项通过，Windows 的 602 项 GPUI 和 16 项会话库测试通过。长路径窄分屏自检按终端逻辑行统计命令回显。
- Windows 自检入口位于 `crates/kanso-gpui/src/ui/`。主外壳、终端、Filer、Sender、设置五套报告均有通过记录，增加的远程编辑自检另计。复查时给应用传单独的 `--data-dir`，使用临时 HOME 和专用 SSH fixture，不接触用户凭据、剪贴板或会话。
- 本轮联调机器的临时材料在 `/tmp/kanso-polish/`。Mac 的 `theme-proofs/mac-terminal-{light,dark,ocean}.png`、`mac-semantic-{light,dark}.png` 和 Windows 的 `r3-screens/windows-ocean.jpg`、`lt-find.png` 用于外观对照。`vmqa/proof/` 保存 SFTP、OpenSSH、导出/导入实拍，`vmqa/proof4/lt-virtual-filer.jpg` 为 SSH 辅助 Filer。Mac 与 Windows 协议互操作的逐项结果见 `vmqa/lrzsz.log` 与 `vmqa/windows-protocol-lrzsz-0.13.1.log`。
- Windows 应用前台四次传输与一次在途取消的核对结果在 `vmqa/protocol-gui/fixed/protocol-verified.txt`。同目录的 `lt-{x,z}-upload-complete.jpg` 和 `lt-{x,z}-restored.jpg` 记录上传、接收后的终端，`lt-z-cancelled.jpg` 记录对端取消及终端响应。
- 逐项操作 Windows 的原图保存在 `/tmp/kanso-polish/windows-parity/`。文件搜索与选中见 `file-results.png`、`file-revealed.png`，改键与恢复见 `key-conflict.png`、`session-menu.png`、`binding-reopen.png`、`key-reset.png`，侧栏拖动见 `tabs-before.png`、`tabs-after.png`、`panel-size.png`，新进程的显示设置见 `display-reopen.png`。Windows 浅色真实终端见 `windows-light-ansi.png`、与 Mac 逐色核对的原始 RGB 见 `light-color-check.txt`。
- 调试材料不随安装包分发。版本与后续实机结果见[Windows 交接记录](windows-gpui-handoff.md)。单测、VM 程序自检、真实外部程序和协议对端各证明对应步骤。
