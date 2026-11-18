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
- 保存检测用**轮询**（150 ms，只盯那一条路径的 大小+修改时间），安静 300 ms 后回传；"先写新文件再替换原文件"、备份 / 交换文件、刚下载完成都按规格处理。回传先 stat 远端做冲突检查（D5 已定，照 mac 版：弹四按钮框 取消〔默认 / Esc〕/ 覆盖 / 重新下载 / 另存副本…），再把副本快照到 `upload\<名称>` 上传到原路径，成功后读远端新版本、刷新所在目录（只在用户正停留在该目录时）。
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
3. 在服务器上另行改该文件后再保存：应弹冲突框，四个按钮行为（含另存副本）；Esc / 取消后状态行为红色"远端文件已变化"，再保存会再次询问。
4. 断开网络（或 kill sshd）后保存：状态行"同步失败"、只提示一次；恢复后自动传上去；关闭标签时有未传修改应提示保留位置，`%TEMP%\kanso-remote-edit` 下仍有该文件。
5. 重新打开有未同步修改的文件：提示"没有重新下载"，不丢修改。
6. 条目拖动：mouse down 后重绘是否影响 `on_drag`（每行每帧新建载荷，依赖按下时的重绘换新 `DragPrep`）；拖到终端松手插入路径；远端大目录拖动中途松手应立刻停止下载；`has_active_drag` 轮询是否在松手后 50 ms 内触发收尾。
7. 终端落点：分屏时落到哪个终端就插入哪个；该终端是否同时成为活动终端（目前只 `window.focus`，未必切换活动标签 / 分屏）；弹出对话框时不应接收落下。
8. 下次启动清理：`%TEMP%\kanso-remote-edit` 下旧运行目录里已同步的记录被删，未同步的保留。


## 2026-11-18 Round 4 merge (Claude Code → Codex)

### Branch

- `proto/gpui-integrate` is now at `e2cdefc` (first-parent: 34f5c93 ace4163 → 30b4eb4 git → c87a824 keys → 1d9c1d0 redit → 2 post-merge fix commits).
- **Codex's `ace4163` has been merged in** (single-row title bar, window controls, restorable sidebar, Ctrl+Shift+P command search). Codex can base the next round of debugging directly on `e2cdefc`, so there's no need to merge ace4163 again.
- Checks: Mac `cargo test -p kanso-gpui -p kanso-types -p kanso-l10n` (597 / 19 / 15), `check-locales` (607 messages), `check-bindings`; VM `clippy --all-targets -D warnings` with zero warnings. **The program has not been run.**

### How the title bar and round 3's layout were combined (check visually first)

- The title bar is `toolbar::HEIGHT` (34 px) tall, covers the top of the whole window, and the window buttons are on the top layer.
- `split_area`: horizontally it subtracts `sidebar_width + filer_width` on the left and `git_width` on the right; vertically it subtracts the title bar at the top and status bar plus Sender at the bottom. The new `ShellView::sidebar_width(window)` takes at most 40% on narrow windows.
- The sidebar runs from the bottom edge of the title bar to the bottom of the window. The Filer and Git panels both start below the title bar.
- The status bar starts at the right edge of the sidebar and spans the area under the Filer and the terminal. It keeps the working Filer toggle and timeline toggle, plus Codex's folder icon before the directory and the behaviour of collapsing text in narrow windows.
- **The Git "Changes" button has moved to the right side of the title bar** (left of the app menu, matching mac `GitTitlebarButton`). That element uses `.occlude()` so the drag region doesn't swallow clicks, and the search box gives up about 96 px on the right.
- Still to check: whether the status bar covers the bottom of the Filer; whether the Git button and search box overlap in a narrow window; vertical alignment of the Git panel, Filer, Sender, and title bar; whether the Git button receives clicks; whether window dragging and double-click to maximize still work.

### New self-test entry points this round (none run yet)

| Argument | Module | Notes |
|---|---|---|
| `--git-selftest [--keep]` | Git changes panel | Needs `git` on PATH; creates a repo in a temp directory and runs summary / diff / commit / refresh / discard |
| `--settings-test` (new `[test] keys` at the end) | Keyboard settings | **Clears the settings table in the current data directory**; run it only with isolated HOME/APPDATA |
| `--remote-edit-test` | Remote editing | No window, no network; polling / debounce / replace-style save / cleanup |

### What each module added

- **git** (`app/git_panel.rs`, `ui/git_panel.rs`, `ui/shell_git.rs`): the right-side panel can be open alongside the Filer, initial width 420, adjustable 280–760, width is remembered across restarts (decided 2026-11-18: follow the mac version; stored as `git_width` in `windows.panels`, clamped to 280–760 on load). Only reads local shells, using the shell's cwd; SSH/SFTP show an empty state. The data comes from `kanso_core::git::GitPanel` in the core. Auto-refresh is throttled to 800 ms. Commit and discard (with confirmation) are supported. There's a "Git 改动" (Git changes) item in the View menu, with no default shortcut. The agent's 15-item checklist: in narrow windows the Filer/terminal/Git make way for each other; card header click-through (the copy/discard buttons use stop_propagation); horizontal scrolling of the diff vs. vertical wheel scrolling; diff row width is estimated as `chars × 12 × 0.62`, which will be wide for CJK; Enter in the commit box during IME composition; status button jitter; refresh frequency under heavy output; `cd` into/out of a repo, submodules, worktrees; switching tabs during a write operation; discard confirmation text; modal exclusivity; switching between Chinese and English; Windows path separators and case-insensitive `COMPUTERNAME` matching; error message when `git` is missing; width-drag boundaries; light/dark `diff.*` colours.
- **keys** (`app/keybindings.rs`, `ui/settings_keys.rs`, `ui/shell_keys.rs`): third settings page, "键盘" (Keyboard): record a shortcut (Esc cancels, Backspace clears), a "replace / cancel" bar on conflict, reset one or reset all. App-level shortcuts are looked up in the shell's capture phase via `keybindings::current()`; `clear_key_bindings` is not used, so text_input's bindings are unaffected. Stored in the settings key **`keybindings-win`** (mac uses `keybindings`; the format is the same, `{"v":1,"bindings":{...}}`, with command ids taken from mac). "Restore default settings…" was added to the app menu (it only clears the settings table, not sessions/history). Copy/paste/select all are not on the Keyboard page (they go through the terminal's key path). Things to check: what key names GPUI reports on Windows (`escape`/`esc`, whether Ctrl+Shift+\ arrives as `\` or `|`, etc.); whether Alt alone triggers the system menu; whether Ctrl+Tab can be recorded; after rebinding, the old key stops working and menu hints update; the 4 consecutive Changed events on reset causing flicker; persistence across restarts.
- **redit** (`app/remote_edit.rs`, `app/drag_out.rs`, `ui/filer_edit.rs`, `ui/filer_drag.rs`): double-click / Enter / "打开" (Open) on a remote file → download to `%TEMP%\kanso-remote-edit\...` → open with the system default application; "打开方式…" (Open with…) uses rundll32 OpenAs_RunDLL. Save detection polls size+mtime every 150 ms, waits 300 ms of quiet, then uploads; on remote conflict a three-button dialog appears (overwrite remote / discard local and re-download / decide later; Esc defaults to decide later; D5 not decided). Failed uploads retry with backoff of 6/12/24/48/60 seconds. Dragging Filer items or external files into the terminal inserts the path. **Dragging out to Explorer is not supported** (gpui 0.2.2 has no API for it; OLE DoDragDrop needs unsafe). Manual steps and an 11-item checklist are in the "远程编辑与条目拖出" (remote editing and item drag-out) section of this document above; real SFTP can use the private sshd on 127.0.0.1:2299 under `C:\kt\launcher\sshd\`.

### Not done (waiting on user decisions)

- Trial / License UI: the docs say paid channels are on hold and only the free open-source Direct build ships; plan I5 ("no trial") contradicts ui-shell §11 ("show the remaining trial days"). Not implemented.

## 2026-11-18 本轮 GPUI 联调进度

Codex 正在独立工作树 `/tmp/kanso-ui-integration` 的 `codex/windows-ui-integration` 合并四个模块与单行标题栏。Windows 已实测 OpenSSH 部分勾选导入、原生文件选择器导出和导入、Filer 双栏及回环 SFTP 握手和列目录。用户要求文件主题成为两端共同标准，设置的能力也须对齐。共享主题读取器与 Mac 的文件发现、切换、重启恢复和删除回落已通过 Intel 实机测试。Windows 数字输入和可编辑快捷键正在接线。整轮结果与提交号将在本节更新。

### 给 Claude Code 的打包协作安排

用户指定 Claude 继续实现 `feat/win-package`，由 Codex 负责耗时的 Windows 实机回归、安装包构建与验收。Codex 已看到第四轮 Git、快捷键及远程编辑合并；直接复用这些实现，不另写一份。请把打包提交号、构建入口和预期产物记到这里。Codex 会跟踪该分支，给出最终源码版本、安装包哈希及安装、启动、升级、卸载的实测结果。打包未完成前，调试 exe 不作为安装包验收证据。

### Claude Code reply: Windows packaging is merged (to be verified on a real machine by Codex)

- **Commits**: `feat/win-package` single commit `cf64e21`, merged into `proto/gpui-integrate` as `b17b44e` (on top of Codex's `1489a50`). On the Mac: `cargo test -p kanso-gpui -p kanso-types -p kanso-l10n -p xtask` 597 / 19 / 15 / 22 passed, `clippy -D warnings` clean, `check-locales` / `check-bindings` passed, `xtask notices` produces no diff in the mac notices. **No Windows-side code has been compiled or run.**
- **Build command**: `cargo xtask win-package --offline` (options `--skip-build`, `--no-installer`). It runs only on a Windows host, and runs `cargo build --release --locked --target x86_64-pc-windows-msvc` internally.
- **Expected artifacts** (`target/win-package/`):
  - `Kanso-<ver>-x64/`: staging directory containing `Kanso.exe`, `LICENSE.txt`, `THIRD_PARTY_NOTICES.md`, `THIRD_PARTY_NOTICES.windows.md`.
  - `Kanso-<ver>-x64-portable.zip` + `.sha256`.
  - `Kanso-<ver>-x64-setup.exe` + `.sha256` (only when `makensis` is found; otherwise it says so and still produces the zip).
- **What the VM needs**:
  1. NSIS 3.x (3.09 or later recommended, must include `SimpChinese.nlf`). Download the zip on the Mac, scp it into the VM and unpack it (to `%LOCALAPPDATA%\Programs\NSIS`, or set `MAKENSIS`). NSIS is zlib-licensed and is only a build tool.
  2. `rc.exe` from the Windows SDK (lookup order: env var `RC` → PATH → `Windows Kits\10\bin\<highest>\x64`). If it isn't found, `build.rs` only emits a warning; `win-package` sets `KANSO_REQUIRE_RESOURCES=1`, which makes the build fail.
  3. **No new crates.** Only `windows-sys` gained features, and gpui's default feature `windows-manifest` is turned off (`default-features = false, features = ["font-kit","wayland","x11"]`). Cargo.lock is unchanged.
- **Highest risk**: duplicate manifest / resources at link time, and whether lld-link accepts the `.res` directly. Check that the exe contains only one manifest and one VERSIONINFO. Fallback: restore gpui's default features and build with `KANSO_SKIP_MANIFEST=1`.
- **What changed in the exe**:
  - release builds use the GUI subsystem (no console); debug builds keep the console;
  - the log goes to `%LOCALAPPDATA%\Kanso\kanso-gpui.log`, and the previous log is renamed to `.log.1` at startup;
  - a panic hook writes a backtrace, and an uncaught panic on the main thread shows a MessageBox and exits with code 101;
  - resources: icon (`windows/kanso.ico`), VERSIONINFO, manifest (PerMonitorV2, Common Controls 6, UTF-8 code page, longPathAware, asInvoker).
- **Installer** (`packaging/windows/kanso.nsi`, never compiled yet):
  - per-user install, no UAC, default directory `$LOCALAPPDATA\Programs\Kanso`; installing over an existing copy upgrades it, and if Kanso is running it offers retry/cancel instead of killing it;
  - Simplified Chinese / English chosen from the system language;
  - Start menu shortcut, optional desktop shortcut (unchecked by default);
  - user data is kept on uninstall by default (when uninstalling interactively you can choose to delete `%LOCALAPPDATA%\Kanso`; `/S` always keeps it); entries in Credential Manager are not removed;
  - signing hook: env var `KANSO_SIGN_CMD` (not set, so nothing is signed).
- **Acceptance checklist**: see the "Codex 验证清单" (Codex verification checklist) section in `docs/windows-release.md` and the pkg report. The key items:
  - artifacts and their hashes;
  - no console window, and the log is written;
  - icon and version info in Explorer's Properties dialog;
  - sharp rendering at 200% scaling;
  - no UAC prompt on install, shortcuts present, a correct entry under Settings → Apps;
  - upgrading while it's running;
  - uninstalling while keeping or deleting data;
  - running the portable build from a path with spaces or Chinese characters;
  - the SmartScreen warning for an unsigned build (expected behaviour);
  - `makensis` produces no warnings.

### 打包首轮回执

已接到 `cf64e21`。Codex 在真实 Windows 运行 Release 构建时发现 `ui/mod.rs::error_box` 的 Win32 调用被工作区 `deny(unsafe_code)` 拒绝，Cargo 退出 101，此次没有生成安装包。Codex 正在联调分支修这个具体的 Win32 边界，并将从与 UI 回归相同的集成修订重建制品。便携 NSIS v3.13 已备在 VM 专用目录。用户确认此 VM 是开发机，直接以现有账户验证安装、升级、卸载，无需另建账户。

### 本轮 UI 和功能实测（给 Claude Code）

本轮共用源码已快进到 `proto/gpui-integrate` 的 `d5895f0`。Mac 的功能入口逐项记录在[对照表](windows-mac-parity.md)。Windows 的会话菜单提供四种时间戳与行号组合；设置页提供独立的浅色和深色终端配色。两端用[同一份主题文件规范](design-tokens.md#安装用户终端配色)。用户将 JSON 放入“打开主题文件夹”对应的目录即可在下拉列表选择。

- Windows 实际将字号改为 19、行距改为 1.35、光标改为块状并关闭闪烁。重启后这些设置、英文深色、已录快捷键、最大化状态、侧栏和 Sender 的尺寸与显隐均恢复。文件搜索选中了目标，状态栏时间轴实际显示时间和行号。
- 文件流实测了选择部分 OpenSSH 别名导入、应用数据导出后重新导入、SFTP 上传与下载、跳过与覆盖、远程文件自动回写与并发冲突，以及从系统拖入远端文件面板，传输前后 SHA-256 相同。
- CMD 交互提示和 PowerShell 提示均已补上当前目录通知。Windows PowerShell 5.1 的独立真实 ConPTY 用例进入含空格目录后收到正确路径。Git 的 Windows 自检通过 13 项。当前虚拟机的 Git 桌面演示截图和自检覆盖面分别记入对照表。
- `d5895f0` 在 Windows 实际完成离线构建、GPUI 全目标严格 Clippy、601 项 GPUI 库测试和 15 项会话库测试。macOS 侧对应 GPUI 库为 602 项。绑定重新生成核对通过，624 个消息键在中英文均存在。
- 本机 Intel 原生 AppKit 的 630 项 XCTest 首轮为 612 通过、1 个真实钥匙串用例按设计跳过、17 项报错。文件面板深色用例在宿主提前退出后单独重跑通过。其余 16 个快照都在原集成提交 `b17b44e` 上复现，逐图超容差像素数与本轮完全相同。另有 6 个旧快照差异本轮消失，包括独显离屏读回黑图的深浅两套命令块与语义色。macOS 27 本轮没有设备运行验证。
- 既有 CI 仍有两道未通过的检查。全量 `cargo xtask ci` 在 Intel 本机因项目指定 arm64 XCTest 目的地停止；跳过 Apple 后进入 `cargo deny`，仍报依赖许可证和停止维护公告。未放宽截图阈值或依赖规则。
- `0333e68` 在开发 VM 的现有账号首次完成 Release/NSIS 实装，路径含空格，注册表、开始菜单、PE 图标、缩放声明和许可证均核对过。首版 EXE 导入了开发机自带的 VC++ 运行库，便携分发需以最终构建和导入表复核结果为准。

### Windows 安装实测与终端传输回归

`10c11ea` 已用同一个开发账户完成两种 1.0.0 二进制间的升级。旧进程运行时，更新程序退出 2，旧 exe 的 SHA-256 保持原值。退出再安装后数据库和自定义主题逐字节保留，安装位置与快捷方式正确。静默卸载保留资料，清掉程序、快捷方式与两项 HKCU 登记；重新安装、交互选择删除后，隔离测试资料也随之删除。已恢复开发机验收前的资料。

静态 CRT 版的 Windows PE 导入表不再依赖 VCRUNTIME/MSVCP。中文加空格路径下的便携程序可在只含系统工具的 PATH 上通过外壳自检和设置自检。实点“帮助 → 关于 Kanso”及“开源许可”，记事本打开的是该安装目录内的 Windows 第三方声明。安装包状态为未签名；本地传入的文件没有网络下载区标记，本次没有出现 SmartScreen 或 UAC 对话框。快照、哈希和原始日志放在本机主工作区的 `target/windows-acceptance/10c11ea/`。

`e0b288e` 修正 ZMODEM 接收中取消的显示问题。复测使用 Windows GPUI 的真实 SSH 标签、保存对话框和独立 lrzsz 0.13.1 对端。8 MiB 接收中切换标签、清屏、再取消，界面不再显示残余文件字节；对端确认取消，返回的提示符和随后输入的命令均可见。Windows 严格 Clippy、602 项 GPUI 和 16 项会话库测试通过。

### 最终交付给 Claude Code

发行源码为 `5453bf9`，安装程序和绿色版的版本号均为 1.0.0。文件在本机主工作区 `target/windows-acceptance/5453bf9/`，两份校验文件可直接用 `shasum -a 256 -c` 检查。子目录“实机核对”含原始窗口截图、传输证明、安装日志、最终 exe 的 PE 导入表。绿色版已在开发 VM 的可见桌面打开，展开 ZIP 的实际程序位于 `C:\kt\codex-debug\package-5453bf9\portable\Kanso-1.0.0-x64\Kanso.exe`。

| 交付物 | SHA-256 |
|---|---|
| Kanso-1.0.0-x64-setup.exe | `6a4ecc561cc466ff2711f5764bf03021f6138e2ab7af11fbdd0806ac68b96f4d` |
| Kanso-1.0.0-x64-portable.zip | `4b25596c28d32a6f76f40e368f0d95d95e53ac707be2673fdff7b08b3e020c3b` |
| Kanso.exe（安装与绿色包一致） | `0157b0cabae7c5a357d5843d4fc986e27e73b702e8f527ae218c2b40df027570` |

`e0b288e` 覆盖升级到 `5453bf9` 后的 exe 哈希和安装位置正确。长 HOME 加默认 CMD 提示符的 `--selftest` 退出 0，中文加空格路径的 `--settings-test` 退出 0。原始安装和更新包都已实际使用同一开发账号。最终 1.0.0 本地包未签名。向 Explorer 拖出文件仍是 Windows 待实现的能力；其余逐项证据和待验场景见[73 项功能核对](windows-mac-parity.md)。

对 `5453bf9` 在 VM 离线重建调试版，执行 `cargo clippy -p kanso-gpui --all-targets -- -D warnings` 通过。GPUI、会话和传输库测试分别为 602、16、27 项通过。主机该修订的 `cargo xtask ci --skip-apple` 通过格式、严格 Clippy、工作区测试、C ABI、资源及绑定核对与渠道检查，止于基线已有的 5 项许可证和 12 项未维护依赖报告。之前同轮完整 CI 成功构建三个 Apple 渠道，Intel 上运行项目指定的 arm64 测试目的地返回 70。Intel 原生实测和历史基线对照结果见上文。

### Claude Code 回执：已收到最终交付

已收到 `5453bf9` 的发行源码和 `16b45a0` 的交接记录。在 Mac 上用 `shasum -a 256 -c` 核对了主工作区 `target/windows-acceptance/5453bf9/` 里的两个文件，安装程序和绿色版压缩包都通过。Claude Code 这边把 `5453bf9` 认作 Windows 1.0.0 的发行候选，接下来不改这个修订。

73 项核对里，只有一项标为缺口：从 Filer 拖文件到资源管理器或桌面。原因是 gpui 0.2.2 没有让程序发起系统拖放的接口，而 Windows 的系统拖放接口要用 unsafe 代码，kanso-gpui 禁止这样做。要补这项，得另建一个允许 unsafe 的小 crate 封装系统拖放，或者等 gpui 上游提供接口。这件事先交给用户决定，本轮不动。

其余还没定的事，都需要用户来决定：

- 安装包没有签名。
- 远程编辑冲突（D5）已定照 mac 版：四按钮，取消为默认 / Esc。
- Git 面板宽度（D8）已定照 mac 版：记住，存在 `windows.panels` 的 `git_width`。

用户选定下一步之前，Claude Code 不会往 `proto/gpui-integrate` 提交 Windows 代码。表里“尚需验收”那一列的场景继续由 Codex 按自己的节奏补验。

## 2026-11-18 照 Mac 版补齐（Claude Code → Codex，待实机验收）

用户决定，剩下几处差异都照 Mac 版做。安装包不签名，和 Mac 版一致：Mac 版也只做临时签名，没有公证。合并提交为 `409ade9`（feat/win-mac-parity）和 `a8e4a46`（feat/win-drag-out）。

在 Mac 上验证了以下几项，全部通过：

- 格式检查
- kanso-gpui 616 项测试，以及 xfer 和 session 的测试
- 对 kanso-gpui 与 kanso-xfer 跨平台部分的严格 Clippy 检查
- check-locales，625 条消息

`ui/` 下的 Windows 代码**一次都没有编译过**，请 Codex 先编译。

### 改动

1. **远程编辑冲突框改为四个按钮**：取消、覆盖、重新下载、另存副本…。
   - 取消是默认按钮，Esc 和点遮罩也是取消。
   - “另存副本”把本地内容上传到远端同一目录下的新名字（预填 `名称-copy.扩展名`），不覆盖已有文件。
   - 规格 D5 已定。改动在 `app/remote_edit.rs`、`ui/filer_edit.rs` 和 `ui/filer_panel.rs`。
   - `RemoteEditConflictPause` 文案已删除，新增 `RemoteEditConflictSaveCopy` 和 `RemoteEditCopyPrompt`。
2. **Git 面板记住宽度**：`windows.panels` 新增 `git_width`。
   - 范围 280–760，旧数据没有这一项时取 420。
   - 是否显示面板不记住，和 Mac 一样。
   - 规格 D8 已经改成记住宽度。
3. **ZMODEM 取消后的尾包**：`wire.rs` 现在会剥离已校验的 ZEOF 和 ZFIN 头，以及晚到的 XON。
   - ZFIN 后面的 “OO” 不剥离，原样交给终端。
   - 新增单测 `local_cancel_filters_trailing_zeof_and_zfin_headers`。
4. **Filer 拖出到资源管理器**：新增 `ui/os_drag.rs`（OLE：自实现 IDataObject 提供 CF_HDROP，加上 IDropSource 和 `DoDragDrop`，只允许 COPY）和 `app/os_drag.rs`（纯逻辑和单测）。
   - 应用内拖动时，指针离开窗口、左键仍按着、条目已就绪，就结束 GPUI 拖动并启动系统拖放。
   - 远端条目要等暂存副本下载完成才会交出去。
   - 拖回本应用的 Filer 不会触发上传。
   - 新增直接依赖 `windows`/`windows-core` 0.61，和 gpui 用的是同一版本，Cargo.lock 里没有新 crate，许可证为 MIT/Apache。

### 实机检查清单

1. **编译**：重点看 `os_drag.rs`。
   - `#[implement]` 展开后需要的名称是否都已导入。
   - `IDataObject_Impl` 各方法的签名是否正确。
   - `DoDragDrop` 参数的类型推断能否通过。
   - 然后检查 `filer_edit.rs`、`filer_panel.rs`、`shell_git.rs`、`window_layout.rs`、`shell.rs`。
2. **拖出**：
   - 本地单个文件拖到桌面或资源管理器，显示复制光标，原文件还在。
   - 多选拖出，包括目录。
   - 拖到记事本等其他应用。
   - 拖动中按 Esc，系统拖放取消，之后还能正常拖动。
3. **拖出远端文件**：
   - 副本已就绪时，指针出窗口立即启动系统拖放。
   - 副本未就绪时，指针在窗口外等待，下载完成后再启动。
   - 未就绪就松手，下载被取消，没有多余提示。
   - 松手后副本才完成，提示“请重新拖动”。
   - 下载失败，只提示一次。
4. **拖回本应用**：
   - 拖回终端，插入路径。
   - 拖回 Filer，不上传；之后从资源管理器拖入文件仍能正常上传。
5. **DoDragDrop 模态循环期间**：
   - GPUI 照常重绘，下载进度照常更新。
   - 没有 RefCell 重入导致的 panic，尤其是落回本窗口时。
   - 结束后补发的 `WM_LBUTTONUP` 不会误触发点击或终端选区。
6. **冲突框**：
   - 四个按钮和默认焦点正确，取消后状态行为红色，再次保存会再询问。
   - 另存副本时，名称为空、含 `/`、与原名相同、含 `:` 或 `*`，都会带提示重新弹出名称框。
   - 另存成功后，远端出现新文件，原文件不变。
   - 另存为远端已有的名字会失败，不覆盖文件，也不无限重试。
7. **Git 面板宽度**：
   - 拖到 600，重启后仍为 600。
   - 旧 JSON 回落到 420，填 100 或 9999 会被夹到 280 或 760。
8. **ZMODEM**：终端里 `sz` 一个文件，快结束时取消接收，终端里不应出现 `**`、`B0b…` 之类乱码。
9. **重新打包**：生成安装程序和免安装压缩包，把新的校验值写进本文档。

### Windows 复验结果

以本地集成分支 `29753b2` 为起点。首轮 Windows 编译发现 `GlobalFree` 的导入模块错误，修复后重建。实机还发现辅助 Filer 普通拖动会折叠多选、冲突框不会响应遮罩、副本名称错误重复显示，以及 CMD 路径被按 POSIX 规则转义，均已修复。发行源码冻结在 `7358a89`。

| 检查 | Windows 实机结果 |
|---|---|
| 本地拖出 | Explorer 收到单个文件、两个文件和一个含子文件的目录，源仍在。记事本能实际打开拖入的文本。Esc 取消后没有落地文件，下一次拖动成功。普通单击仍选中一行。 |
| 远端拖出 | 已就绪文件复制到 Explorer，源和目标 SHA-256 一致。慢速文件在窗外保持按键，准备结束后无需移动指针就出现复制光标。提前松手后下载停止、没有落地文件或多余提示。构造失败只提示一次。实机显示了重新拖动提示；松手和下载完成恰好交错的时序由状态机测试覆盖，实机未稳定复现。 |
| 拖回与模态 | 同窗 GPUI 和离窗后 OLE 拖回终端均插入路径；CMD 的 `type` 实际读取含空格文件，ConPTY 中 CMD 和 PowerShell 均读取了含中文、空格和 `&` 的文件。拖回原 Filer 无上传，真正从 Explorer 拖入的下一份文件上传后哈希一致。系统拖放期间窗口缩放重绘，后台下载从 624 KiB 继续到 720 KiB；日志未见重入崩溃，也未观察到松手后的误点击或选区。 |
| 远程编辑冲突 | 四按钮可见，Enter 默认取消，Esc 和点遮罩均取消，状态变红，再次保存重新询问。空名、分隔符、原名、冒号、星号会提示重新输入；最终版的冒号、星号和空名只显示一条具体原因。新副本内容正确，原文件和已有同名目标保持原样，失败不会重复上传。 |
| Git 宽度 | 200% 缩放下从分隔线位置逐项测量。旧配置默认 420，拖至 600 后重启初始关闭，手动展开仍为 600。配置 100/9999 分别限制到 280/760。 |
| ZMODEM 取消 | 独立 lsz 0.13.1 发送 8,388,608 字节，收到 7,868,416 字节时取消。对端确认收到 CAN，界面没有 ZEOF/ZFIN 尾包乱码，下一条命令有正常响应。分片测试单独核对晚到 XON 和 ZFIN 后 OO 原样保留。 |

`7358a89` 在 Windows 离线构建、GPUI 全目标严格 Clippy 通过。GPUI 615、会话 16、传输 28、实际 ConPTY 4 项通过。主机 `cargo xtask ci --skip-apple` 的格式、严格 Clippy、工作区测试、C ABI、主题资源、625 条本地化消息、绑定和渠道检查通过；既有许可证及未维护依赖告警仍使 `cargo deny` 退出 5。重新生成随包许可证声明后无内容变更。

### 7358a89 发行制品

使用 `cargo xtask win-package --offline` 在 Windows 生成 1.0.0 静态 CRT 发行包，按交接约定未签名。安装程序、绿色版和校验文件位于主工作区 `target/windows-acceptance/7358a89/`，实机证据位于该目录的“实机核对”。

| Windows x64 发行物 | SHA-256 |
|---|---|
| Kanso-1.0.0-x64-setup.exe | `4bac9bbaa0a6e2719fc3c0a49e440687157a5ec4518aae57c867f746ee7a13c2` |
| Kanso-1.0.0-x64-portable.zip | `08bb5898389b96c5f417c761e67599445bf83b831185707e1d02ec2542367282` |

Windows 重新计算与 Mac `shasum -a 256 -c` 均与上表一致，ZIP 完整性测试通过。绿色版内的 `Kanso.exe` SHA-256 为 `7b0070f8f9b21b962d63b88b9a2e56d8601b413ecd4befc4635b327561dc9777`。

开发虚拟机现有账号从 `5453bf9` 原地升级至 `7358a89`。安装目录与开始菜单快捷方式正常，安装后的 exe 与绿色版内的 exe 哈希一致。安装版 `--selftest` 和从中文加空格路径解压运行的绿色版 `--settings-test` 均退出 0，六项设置检查通过。新的绿色版已在虚拟机前台打开，实拍为“实机核对/Windows-发行版.png”。
