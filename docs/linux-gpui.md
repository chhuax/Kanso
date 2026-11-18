# Linux GPUI

Linux 与 Windows 共用 `kanso-gpui/src/ui` 的主窗口、会话编辑、设置、启动器与终端，不另做一套界面。macOS 继续使用 AppKit。

## 构建与启动

Debian / Ubuntu 安装构建依赖及系统字体：

```sh
sudo apt-get install build-essential pkg-config clang libssl-dev \
  libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libvulkan-dev libfontconfig1-dev libfreetype-dev fonts-dejavu-core fonts-noto-cjk
cargo build -p kanso-gpui --release --locked
./target/release/kanso-gpui
```

运行需要 X11 或 Wayland 桌面、可用的 Vulkan 驱动。GPUI 根据桌面环境选择后端；应用未强制 XWayland 或关闭硬件加速。
密码存储使用已有的 Linux Secret Service 后端，桌面会话需要运行 GNOME Keyring、KWallet 或其他兼容服务。

- 默认配置与日志位置：`$XDG_DATA_HOME/Kanso`；未设置或使用相对路径时为 `$HOME/.local/share/Kanso`。`--data-dir`、`--log` 可沿用现有覆盖参数。
- 默认 Shell 沿用 core 的 `$SHELL` / `/bin/bash` 规则，Linux 不加 login 参数。会话编辑器只列出本机存在的 Bash / Zsh / Fish / sh，仍支持自定义命令。
- 默认字体取发行版的 DejaVu Sans / DejaVu Sans Mono，中文回退到系统字体；设置里可更换。字体不随应用打包。
- 主窗口沿用现有工具栏。拖动、双击最大化、窗口按钮与边缘缩放接到 GPUI 的 Linux 窗口 API，关闭仍经过会话确认。不支持客户端装饰的窗口管理器使用原生标题栏，避免重复显示窗口按钮。
- 设置窗口创建后补写 UTF-8 标题，修正 GPUI 0.2.2 在 X11 初始标题中的中文乱码。

## 发行包

推送 `v*` 标签时，发布流程在 Ubuntu 上运行 `scripts/linux-package.sh`，把 `Kanso`、`kanso-mcp`、`LICENSE.txt`、`README.txt` 和 `THIRD_PARTY_NOTICES.linux.md` 打成 `Kanso-<版本>-Linux-x64.tar.gz`，并附 SHA-256。压缩包未签名。运行库与 glibc 要求写在压缩包内的 `README.txt`。

## 验证

```sh
cargo test -p kanso-gpui
cargo clippy -p kanso-gpui --all-targets -- -D warnings
sudo apt-get install xvfb xauth openbox dbus-x11 mesa-vulkan-drivers
cargo build -p kanso-gpui --locked
sh scripts/gpui-linux-smoke.sh
```

自检每次新建 HOME、XDG 目录、D-Bus 总线和 Xvfb 显示，使用内存凭据与主机密钥替身。日志保存在 `target/linux-smoke`。终端自检覆盖真实 PTY、宽字符、模拟组字、回滚、多标签、分屏、焦点和会话关闭；设置检查覆盖打开窗口、字号、行距、字体与主题切换。模拟组字不等同于真实 IBus/Fcitx 输入法验收。

GPUI 0.2.2 的传递依赖 `xattr 0.2.3` 使用了 Linux 的旧 `ENOATTR` 别名，libc 0.2.190 删除该别名后无法编译。工作区固定到 MIT / Apache-2.0 的 libc 0.2.189，锁文件随兼容解析将 mio 1.2.5 调整为 1.2.4；待 GPUI 归档依赖升级后再解除固定。

### 2026-11-18 验证记录

- Debian 12 x86_64 隔离容器：`cargo build -p kanso-gpui --locked`、`cargo clippy -p kanso-gpui --all-targets --locked -- -D warnings`、369 项测试通过（368 项库测试、1 项 XDG 路径测试）。
- X11 / Openbox 与原生 Wayland / Weston 均通过终端及设置自检。Wayland 测试使用嵌套于 Xvfb 的 Weston X11 后端提供虚拟输入设备，应用本身连接 Wayland。截图确认主界面可见，并检查了 X11 会话编辑器、设置和缩小窗口。
- 渲染验证使用 Mesa 25.0.7 lavapipe。容器最初的 Mesa 22.3.6 软件 Vulkan 驱动出现空白画面，升级容器驱动后正常；因此不能仅凭 selftest PASS 判断画面可用，也不能声称已兼容所有旧驱动。
- `cargo xtask ci --skip-apple` 的格式、clippy、工作区测试、C ABI、tokens、locales、绑定与渠道矩阵通过；`cargo deny` 仍因基线中的许可证与 unmaintained advisory 失败，未修改审查规则。

Ubuntu、硬件 GPU、真实 IBus/Fcitx 和远端 SSH/SFTP 尚未实机验收。功能范围仍与当前 Windows GPUI 一致，未完成模块不在此次平台适配中补画或重写。

## 合入 main 时的差异（2026-11-18）

这次改动原在 `codex/linux-gpui`（28ef2b3）上验证，基线早于 #22 的 squash 合入。移植到 main 后补齐了 main 新增的 Windows 专用模块：

- MCP 宿主改用 `PlatformSecretStore`（Windows 仍是凭据管理器，Linux 走 Secret Service），`kanso-mcp-server` 改为三平台依赖；设置 → MCP 页面在 Linux 上也出现。
- 系统拖放（OLE `DoDragDrop`）仍只在 Windows 编译；Linux 只做应用内拖动，拖出到文件管理器不生效。
- 终端粘贴在 Linux 只用 GPUI 的文本 / 图片通道，读不到文件列表。
- 界面语言在 Linux 上按 `LC_ALL` / `LC_MESSAGES` / `LANG` 解析。
- 上述新增路径尚未在 Linux 实机复验；以 CI 的 Linux 构建与冒烟自检为准。
