# Windows GPUI 调试交接（Codex → Claude Code）

用户要求本轮只调试已经合并的版本，四个未完成模块下一轮再处理；随后要求深色界面更贴近当前仓库的 Mac 版，以及 Windows 可选择 CMD / PowerShell。用户告知 Claude Code 在 2:10am 恢复额度，要求同步本轮改动。

## 分支与范围

- 基线：`proto/gpui-integrate` 的 `01f8028b52e2004d1caaf455bd2ce2bbc962a4df`。
- 修复分支：`codex/windows-debug`（本目录已切换到该分支）。原 `proto/gpui-windows` 较旧。
- `Kanso-integrate` 工作树和 `.claude/worktrees/` 中 term、sender、filer、store 四组未合并改动均未动；不要直接覆盖本轮修复。
- 未读取旧版仓库或 Zed 应用代码；对照仅限本仓库 Apple UI、规格及 GPUI 库。

## 本轮改动

- `ui/shell.rs`：全是绝对定位子节点时，根 `.size_full()` 会得到空绘制区域。改为明确使用窗口 viewport 宽高，实际截图确认主界面可见；原先“首帧已绘制”和 selftest PASS 不能证明画面可见。
- `ui/mod.rs`：把 GPUI 的 `log` 日志接入现有运行日志，能看到 DirectX / 呈现失败。新增直接依赖 log（已有传递依赖，MIT / Apache-2.0）。
- `ui/sidebar.rs` + `shell.rs`：接入现成的路径筛选规则，Esc 清空并回到终端；紧凑线条图标、选中条、悬停关闭按钮向 Mac 对齐；沿用同一份 design tokens。
- `ui/display.rs` + `settings.rs`：主窗口与设置窗口的原生标题栏跟随应用深浅色，使用 DWM API，只作用当前窗口。raw-window-handle 已是传递依赖，许可证 MIT / Apache-2.0 / Zlib；windows-sys 增开 Dwm 特性。
- `ui/session_editor.rs`：新建/编辑本地会话增加 Shell 下拉：系统默认、CMD、Windows PowerShell、检测到的 PowerShell 7、自定义命令。命令行仍可编辑参数，沿用 Profile 保存与连接，不改核心默认 Shell 规则。
- 新增提示在 Message 及中英 Fluent 目录中成对添加。没有修改 FFI 导出接口。

## 图形排查

- 用户看到的是 Mac 上 VMware Fusion 窗口闪烁和卡顿。
- VM 使用 VMware SVGA 3D / D3D11.0；Mac 的 RX 6600 XT 在 VMware 日志里正常走 Metal。初查高分辨率为 4096×1918，宿主和 Windows 后台有负载；未改 VM 配置、显卡驱动或系统服务。
- 曾试验过 GPUI_DISABLE_DIRECT_COMPOSITION=1（仅测试进程），对原根布局空白无效；当前默认渲染模式，没把兼容开关写进产品。
- 测试中出现 DXGI_ERROR_DEVICE_REMOVED / DEVICE_RESET，反复重建 DirectX 设备。对照替换图标、停用标题栏更新及 DirectComposition 后仍出现；之前可见的 kanso-sized.exe 也开始出现相同错误，因此不能归因为图标或标题栏改动。用户授权后已正常重启 Windows（00:53:39），重新登录（约 00:58）仍复现。Win+Ctrl+Shift+B 也无效；未修改 VM 配置。用户已要求另开会话专门修复 VM，见下。
- DXGI debug interface 缺失的 0x887A002D 后会退回非调试设备，本身不代表主界面失败；真正要关注 Presenting swap chain failed / DirectX device lost。

## 验证与环境

本文件与修复一起提交，提交号以本分支日志为准。

- Mac：`cargo test -p kanso-gpui -p kanso-types --offline` 通过，分别 368 / 19 项。
- Windows：`cargo build -p kanso-gpui --offline -j 8`、`cargo clippy -p kanso-gpui --all-targets --offline -j 8 -- -D warnings`、`cargo test -p kanso-gpui --lib --offline -j 8` 通过（368 项）。日志在测试目录 build-final.log / clippy-final.log / tests-final.log。
- 完整 `cargo xtask ci --skip-apple` 已运行：fmt、clippy、workspace tests、C ABI、tokens、locales、bindings、渠道矩阵通过；cargo deny 的基线许可证/维护状态问题仍阻断，细节见下。仅改 Rust，跳过 Apple 构建。
- 在新增侧栏与 Shell 选择前，已合并版本的 `--selftest`、`--settings-test` 退出 0，输入框键盘测试、启动器保存默认本地会话和重开已通过。自测中的 IME 是合成回调，不能代替真实输入法验收。
- 新增 Shell 选择在日志中已看到 SavedCMD / SavedPowerShell 保存并连接，但同期设备重置导致画面不可用，**尚未完成最终 GUI 验收**。需恢复图形环境后复测两种 Shell 的输出、保存重开、路径筛选、深浅切换和菜单键盘操作。

- Mac 测试、CI 日志：`/tmp/kanso-codex-mac-tests-final.log`、`/tmp/kanso-codex-ci-final.log`。
- VM SSH：`huaxin@192.168.204.128`；工作副本 `C:\Users\huaxin\kanso-integ`，共享 target `C:\Users\huaxin\kanso-target`。
- VM 无外网：只在 Mac 同步源码，Windows 用 `cargo build -p kanso-gpui --offline -j 8`。遵守 `C:\kt\build.lock` / `gui.lock`。
- 所有本轮 GUI 测试数据在 `C:\kt\codex-debug`；HOME、USERPROFILE、APPDATA、LOCALAPPDATA 均注入临时目录。未读写真实 SSH、凭据、剪贴板。
- 原桌面“ Kanso 原型”指向早期 `KansoPrototype` 可执行文件，看到旧黑窗时先检查启动的是哪一版，不要误杀用户自己的终端。
- 本轮测试脚本在 Mac `/tmp/kanso-codex-debug` 及 VM `C:\kt\codex-debug`。`preview.json` 记录本轮预览 PID；只管理自己启动的进程。
- VM 自身 CopyFromScreen 曾得到黑图；最终视觉验证使用 Mac `screencapture` 截 VMware 窗口，截图为真实运行结果。

## 已知遗留

- 功能还未与 Mac 全面对齐；四个未合并模块按用户要求保留下一轮。
- 完整 CI 的依赖审查存在基线问题：MPL-2.0 的 cbindgen/dwrote/option-ext，NCSA 的 libfuzzer-sys，CC0 的 tiny-keccak，以及多个 unmaintained advisory。没有修改 deny.toml 或跳过审查来掩盖问题。
- 本轮未接触真实远端 SSH / SFTP 凭据；网络、传输及其未完成模块不应据此宣布实机验收。

## 专门排查虚拟机的新会话

用户在本轮末尾明确要求新开会话处理虚拟机问题，已创建 **修复 VMware Windows 图形闪烁与 GPUI 黑屏**，线程 ID `01a1ea76-5c87-7ad0-bc24-736c7ca9bc19`，host `local`。完整环境、已试排查、重启授权、测试脚本位置均已交接。该会话负责 VMware / 图形环境，不修改此仓库代码；原会话不再同时操作 VM。

用户另提供远端 Windows `192.168.137.1`，SSH 服务已开；本机 SSH 别名 macmini 指向 `huaxin@10.66.0.10`。用户又明确要求优先解决虚拟机，因此尚未尝试远端链路。


## 2026-11-18 继续：页面对齐与 macOS 26 外框

### 页面改动

- `session_editor.rs` 改为当前 Mac 的 DialogShell / KCard 结构：560 宽、最高 680，独立头部与底部、可滚动主体、会话与连接分区、112×52 类型卡片、名称和两行八色的色板并排。Tab 跨分区时按字段实际绘制边界滚动；类型卡片支持方向键。回车与 Mac 一致只保存，连接按钮显式保存并连接。
- 保留 Windows Shell 选择：CMD / Windows PowerShell / 已安装的 PowerShell 7 / 自定义命令；没有改核心启动规则。
- `settings.rs` 分通用 / 显示两页。字体、字号同排，光标用类型卡片；最低对比度接到已有设置字段。Windows 原有界面字体、终端配色与时间轴选项保留。未增加未实现的键盘快捷键页。
- `launcher.rs` 对齐 360 宽、行高和间距，补上可用的设置入口、搜索图标、编辑和更多图标；会话颜色使用 Mac 的小圆角方块。侧栏取消额外的底部快捷键提示，选中细条使用会话颜色。
- `widgets/card.rs` 复用卡片；正式 Lucide SVG 替换文字与手绘图标。素材 revision `149eba0aea48178a5ab1efe2c701de1da94f4be8`，ISC / MIT，原始许可在 assets/icons/LICENSE，THIRD_PARTY_NOTICES 及生成器已补。
- 改动涉及的界面标签加入 Message + 两种 Fluent 目录（总计 184 条消息）。没有新增 Rust 依赖或改 FFI 导出。

### Mac 修正的准确位置

用户最终确认要去掉的是**包住搜索框和全部页签、一直到底部的整列外框圆角**，不是单个灰色页签的圆角。

- 只改 `MainWindowController.swift`：系统主版本为 26 时改用普通 split item，保持宽度优先级，并在窗口实际布局后恢复默认侧栏宽度。27 和其它版本保留原来的 sidebar item 和显示流程；TabRowView 未改。
- Intel Debug-Direct 构建成功，隔离 mock 窗口实图确认外框移除、侧栏宽度 240、单个标签圆角保留。macOS 27 没有实机运行，不能把“保留原代码路径”写成“27 验收通过”。
- Intel 构建需要覆盖 ARCHS=x86_64；libkanso_ffi 在 target/apple/direct/x86_64-apple-darwin/debug。仓库 apple-test 写死 arm64，Intel 本机运行会在 destination 选择阶段失败。未顺手改架构政策。

### 运行与验证记录

- 虚拟机专门排障会话已结束：Fusion 共享显存 256 MB → 2 GB，完整冷启动，保留 Retina / 3D；100% 和 200% 自检、主题检查没有再报设备重置。不能单独归因于显存或者冷启动。
- 已在 200% 缩放下运行新 UI。隔离 GUI 脚本验证了 SavedCMD / SavedPowerShell 保存、连接、PowerShell 再次打开；实际终端输出 CODEX_REOPEN_OK。测试命令追加 /d 或 -NoProfile，避免读取用户的 Shell 启动配置。
- Windows build、clippy --all-targets -D warnings 通过，kanso-gpui --lib 368 项通过。后续最终自检结果以本节补充为准。
- 完整 cargo xtask ci 已执行：前序 Rust 检查与 Apple 三渠道构建通过，apple-test 因 Intel 机器无 arm64 destination 失败；额外 --skip-apple 检查走到 cargo deny，仍是已有的许可 / unmaintained advisory 问题，没有改 deny.toml。
- 工作区同时出现另一个任务的 Tauri 改动（Cargo.toml / Cargo.lock / desktop 等）。本轮不覆盖、不提交这些文件；CI 使用 /tmp/kanso-polish/verify 的本轮隔离副本。
- 本轮截图、日志与测试脚本：Mac /tmp/kanso-polish；VM C:\kt\codex-debug，最终可执行文件 kanso-polish-delivery.exe。不要再用 kanso-dark.exe、kanso-polish.exe 或桌面的早期“原型”快捷方式评估最终外观。
- 之前通过 Mac mini 的远程 Windows 实验已按用户要求断开；不要自动重连。四个未合并模块仍未合并。

### 仍然存在的差异

Windows 原生标题栏与字体栅格化不同，不能声称跨系统逐像素相同。主窗口完整工具栏、键盘设置页和四个未合并模块的完整功能不属于本轮已验收内容；本次只对已经合并的页面进行视觉和操作修正。

### 最终补充（本轮交付）

- 最终二进制 `kanso-polish-delivery.exe` 的 `--selftest`、`--settings-test` 均退出 0；日志分别在 `selftest-20261118020546`、`settings-test-20261118020558`，均无 DirectX device lost / DEVICE_RESET / Presenting swap chain failed。
- 实际点击通用页模式控件完成深色 / 浅色切换；缩小窗口后通过 Tab 定位到 SSH 表单底部，主体正确滚动且头尾操作区保留。截图分别为 `/tmp/kanso-polish/windows-settings-light-preview.png`、`windows-editor-small-preview.png`。
- Mac 外框修复独立提交 `97b992f`。本轮提交位于 `codex/windows-debug`，隔离 worktree 为 `/tmp/kanso-polish/review`；共享主目录已由另一个任务切到 Tauri 分支，本轮没有切回或混入它的改动。需要继续 GPUI 时从该分支接手。
- 最终 Rust CI 日志 `/tmp/kanso-polish/ci-rust-delivery.log`；Windows 368 项测试和最终 build / clippy 通过。完整 CI 的两项环境 / 基线阻断仍按上节记录，不能写成全绿。


## 2026-11-18 Round 3 merged (Claude Code → Codex)

Division of labour (user decision, 2026-11-18): **Claude Code writes code, Codex debugs**. Claude Code agents do not start the program, take no GUI lock and do no GUI debugging. All runtime debugging and visual checks are Codex's.

### Branch

- `proto/gpui-integrate` at `1b2f169`. It is based on Codex's `codex/windows-debug` (`514be40`) and merges four modules on top: term, sender, store, filer.
- Suggested flow: Codex fixes things on its own branch cut from `1b2f169` (for example `codex/windows-debug2`). Claude Code's next round of new features also branches from `1b2f169`, and the two sides are merged together afterwards.
- Checks already passed: Mac `cargo test -p kanso-gpui -p kanso-types -p kanso-l10n` (536 / 19 / 15); `cargo xtask check-locales` (520 messages), `check-bindings`; VM `cargo clippy -p kanso-gpui --offline -j 8 --all-targets -- -D warnings` with zero warnings. **The program has never been run.**

### Built-in self-test entry points (none of the new ones have been run)

| Argument | Module | Notes |
|---|---|---|
| `--selftest` | Main window / tabs / split panes / menus / close confirmation / clock | Passed before |
| `--settings-test --log-input` | Settings window, fonts, light/dark | Passed before |
| `--term-test [--keep]` | Find count, invalid regex, find bar close, URL detection, timestamp gutter width and column count, block navigation | New; exits 1 on failure |
| `--sender-test [--keep]` | Expands the Sender and sends `echo kanso-sender-ok`, checks screen output | New |
| `--filer-selftest [--keep]` | Local Filer workflow in a temp directory: list, sort, hidden items, create, delete | New |

### What each module added (need GUI checks)

- **term**: find bar (Ctrl+Shift+F, F3 / Shift+F3, case and regex toggles), timestamp / line-number gutter (4 modes; the status bar toggle works, AI CLI sessions hide it by default), command blocks (click to select, red bar on failure, Ctrl+Shift+↑/↓ to jump, Ctrl+Shift+C copies the block, "复制命令 / 复制输出" (Copy command / Copy output) in the context menu), command separator lines, links (Ctrl+click; only http/https/ftp/ftps/mailto allowed; supports OSC 8), minimum-contrast protection. New files: `app/{find,gutter,blocks,links}.rs`, `src/contrast.rs`, `ui/{find_bar,decor,term_selftest}.rs`.
  - Not yet checked: pixel alignment of gutter / separators / colour bars; focus and click pass-through on the find-bar overlay; whether a local shell's OSC 133 actually produces blocks; throughput under heavy output with all decorations on.
- **sender**: bottom Sender panel (Ctrl+Alt+↓, drag to resize 80–400, multi-line input with Enter to send / Shift+Enter for a newline, target current / all sessions, repeat sending with a status-bar hint), saving commands by scope, completion popup (Tab / ↓ / Esc, doesn't pop up during IME composition), "清除命令历史…" (Clear command history…). New multi-line input `ui/widgets/text_area.rs`.
  - Not yet checked: IME candidate-window position in the multi-line input; completion popup position and flipping above when there's no room; 300 ms / 1.5 s timeouts.
- **store**: `.kanso` export / import (Session menu), per-host checkbox OpenSSH import `ui/ssh_import.rs`, empty-state page when there are no tabs (closing the last tab no longer quits, matching Mac; self-tests still quit), language switch in the settings "General" page (follow system / 简体中文 / English).
  - Not yet checked: whether the main window and menus switch to English immediately after changing language; dropdowns that were already built in the settings window only change language after the window is reopened (known issue).
- **filer**: side Filer between the tab bar and the terminal (Ctrl+Alt+→, Ctrl+Shift+J to locate the terminal directory, status-bar "文件" (Files) toggle), pure-SFTP tabs as a two-pane view (remote left, local right), create / rename (F2) / delete (Delete, remote directories must be empty) / upload / download / external drag-in / transfer queue and conflict dialog, ZMODEM / XMODEM using the system file picker plus a progress bar at the bottom of the terminal (Session menu → 文件传输 / File transfer).
  - Not yet checked: real SFTP transfers (the launcher agent left a private sshd in `C:\kt\launcher\sshd\`, listening only on 127.0.0.1:2299, for local testing; do not change the system sshd or authorized_keys); the system file picker; drag-and-drop hit testing under 200% scaling; transfers with lrzsz.

### Other notes

- Newly added Lucide icons were downloaded from the upstream revision `149eba0…`; `trash-2` doesn't exist in that revision, so `trash` is used.
- In the messages catalogue, store's `UiFiles` is no longer referenced (the status bar uses filer's `UiFilerStatusLabel`); it can be cleaned up.
- The shared target `C:\Users\huaxin\kanso-target` gets overwritten when several worktrees build into it; copy the exe out before running it.

## Codex 回执：Windows 单行标题栏调试（ace4163）

已看到 Claude 的第三轮交接。另有一笔与四模块合并并行完成的提交：`codex/windows-debug` 的 `ace4163`（基于 `514be40`）实现单行标题栏、可恢复的侧栏、窗口控制和 Ctrl+Shift+P 搜索。它尚未进入 `proto/gpui-integrate` 的 `f4385d6`。两个分支都改过窗口布局、状态栏和快捷键，合并时请保留集成版已接通的文件浏览器和时间轴。

在 Windows 虚拟机 200% 缩放下，`ace4163` 的 368 项库测试、严格 Clippy、GUI 自检和设置自检通过。实际验证了窗口拖动和按钮、搜索保存的 PowerShell、切回 CMD、打开设置和 Esc 后的终端输入。退出时 GPUI 仍记录无效窗口句柄。上述结果只覆盖这个调试提交，四模块集成后的界面验收另记。

## 2026-11-18 Windows 主窗口与命令搜索

在 `codex/windows-debug` 上从 `514be40` 继续。运行 `C:\kt\codex-debug\kanso-chrome.exe` 可以查看本次主窗口。

- GPUI 主窗口改为 34 逻辑像素的单行标题栏。左侧开关控制标签栏，中央 240 × 22 搜索入口与 Ctrl+Shift+P 打开同一面板，右侧提供应用菜单和 Windows 窗口按钮。拖动、最大化、还原、最小化和关闭交由 Windows 处理。
- 搜索已打开的标签、保存的会话和应用菜单命令，支持名称、会话目标和快捷键文本。方向键、Tab、Shift+Tab 选择，回车执行，Esc 返回终端。文件结果等待 Filer 接入。文件浏览器和时间轴的状态栏入口仍置灰。
- 标签栏延伸到窗口底部。状态栏与终端同宽，窄窗口收起状态项的文字，为时钟留出空间。
- Windows 离线 build、clippy --all-targets -D warnings、kanso-gpui --lib 368 项通过。最终二进制的 selftest-20261118024915、settings-test-20261118024927 均退出 0，日志中设备重置为 0。
- 在 200% 缩放下实际操作窗口和搜索。preview-20261118024941 下有截图及 lt-steps.log，最终脚本退出 0。确认搜索能连接保存的 PowerShell、切回 CMD、打开设置，Esc 后 CMD 执行 echo 并输出 MAIN_WINDOW_OK。507 逻辑像素宽的窗口验证了菜单和键盘搜索。关闭时窗口消失、进程正常结束，GPUI 收尾日志有无效窗口句柄记录。
- 此轮只改 Rust 及配套资源。cargo xtask ci --skip-apple 通过工作区测试、C ABI、187 条消息、绑定和渠道矩阵，仍在 cargo deny 的已有依赖审查项失败。日志在 /tmp/kanso-polish/main-window-ci.log。


## 远程编辑与条目拖出（分支 feat/gpui-remote-edit，未运行过）

### 已实现

- 辅助 Filer 里双击 / Enter / 右键"打开"远端普通文件：先 `sftp_stat`，下载到 `%TEMP%\kanso-remote-edit\<run>\<session>\<record>\content\<原名>`，成功后用系统默认应用打开；右键"打开方式…"先下载再弹系统选择器（rundll32 OpenAs_RunDLL）。目录仍是进入目录。超过 50 MB 先弹确认。
- 保存检测用**轮询**（150 ms，只盯那一条路径的 大小+修改时间），安静 300 ms 后回传；"先写新文件再替换原文件"、备份 / 交换文件、刚下载完成都按规格处理。回传先 stat 远端做冲突检查（D5 未决：不替用户选，弹框"覆盖远端 / 丢弃本地修改并重新下载 / 暂不处理"，默认是暂不处理），再把副本快照到 `upload\<名称>` 上传到原路径，成功后读远端新版本、刷新所在目录（只在用户正停留在该目录时）。
- 失败按 6/12/24/48/60 秒退避重试，相同错误只提示一次；会话重连时立即继续；标签永久关闭时已同步的副本清理、有未回传修改的保留并提示位置；退出应用时同样（无提示）；下次启动清理上次遗留的已同步副本。有未同步修改时再次打开同一文件不重新下载，直接重新打开本地副本。
- 状态行（Filer 底部）：`来源 · 路径 · 状态`，下载带进度与取消，回传只显示文字（不伪装进度）。
- 条目拖出：辅助 Filer 的行可以拖，落在**终端**时向松手所在终端插入路径（`PasteContent::Files`，核心负责转义与 AI CLI 规则），聚焦该终端；外部文件拖到终端同样插入路径。远端条目开始拖动就下载暂存副本（状态行显示进度），准备期间松手立即取消下载，副本在松手后才完成则提示"请重新拖动"；暂存目录 `<run>\drag\<id>`，下次启动清理。拖回本 Filer 不会触发上传。
- 纯逻辑与单测：`app/remote_edit.rs`、`app/drag_out.rs`（Mac 上 `cargo test -p kanso-gpui --lib` 即可）。界面：`ui/filer_edit.rs`、`ui/filer_drag.rs`。核心 / FFI 未改。

### 做不到 / 没做

- **拖出到资源管理器 / 桌面不支持**：gpui 0.2.2 没有"发起系统拖放"的接口（Windows 要 OLE DoDragDrop，需 unsafe，本 crate 禁止）。只能拖到本应用内的终端。
- 拖动起始阈值是 gpui 固定的 2 px（规格约 5 px），改不了。
- 打开应用失败（`cx.open_with_system` 不返回错误）无法给出原因。"另存副本"（mac 版冲突框第四个选项）没做。最近使用的应用列表只对 macOS / Linux 有意义，未做。
- 没有文件监视库，所以是轮询；没新增依赖。

### 自检

- `kanso-gpui.exe --remote-edit-test`：不开窗口、不联网，用真实临时文件跑轮询 / 防抖 / 替换式保存 / 备份文件 / 关闭保留 / 下次启动清理，日志打 `remote-edit-test PASS/FAIL`，退出码 0/1。
- 端到端需要 SFTP 服务器（VM 里 127.0.0.1:2299 的私有 sshd）：连接后打开 txt，用记事本改并保存，观察状态行"等待同步→正在同步…→已同步"，服务器内容更新。

### 调试时请检查

1. 双击远端文件：状态行进度、记事本是否打开了正确文件；同名文件在两个目录 / 两个会话里互不覆盖。
2. 记事本保存一次只上传一次；VS Code 等"替换式"保存连续保存都能同步；上传期间再保存，最终远端是最新内容且不会提前显示"已同步"。
3. 在服务器上另行改该文件后再保存：应弹冲突框，三个按钮行为；Esc / 暂不处理后状态行为红色"远端文件已变化"，再保存会再次询问。
4. 断开网络（或 kill sshd）后保存：状态行"同步失败"、只提示一次；恢复后自动传上去；关闭标签时有未传修改应提示保留位置，`%TEMP%\kanso-remote-edit` 下仍有该文件。
5. 重新打开有未同步修改的文件：提示"没有重新下载"，不丢修改。
6. 条目拖动：mouse down 后重绘是否影响 `on_drag`（每行每帧新建载荷，依赖按下时的重绘换新 `DragPrep`）；拖到终端松手插入路径；远端大目录拖动中途松手应立刻停止下载；`has_active_drag` 轮询是否在松手后 50 ms 内触发收尾。
7. 终端落点：分屏时落到哪个终端就插入哪个；该终端是否同时成为活动终端（目前只 `window.focus`，未必切换活动标签 / 分屏）；弹出对话框时不应接收落下。
8. 下次启动清理：`%TEMP%\kanso-remote-edit` 下旧运行目录里已同步的记录被删，未同步的保留。
