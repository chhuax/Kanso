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
