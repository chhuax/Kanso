<p align="center">
  <img src="docs/icon.png" alt="Kanso" width="128">
</p>

[English](README.md) | [简体中文](README.zh-CN.md)

**一个小巧的终端，只做本地 shell 和 SSH——不挡路。**

Kanso 只做两件事：在这台机器上开一个 shell，在另一台机器上开一个 shell。软件里的一切都为这两件事服务，其余部分在你工作时让开。

用 **Rust + Tauri** 构建：单个原生二进制，安装包 4–5 MB，没有 Electron 运行时，不捆绑字体，没有常驻进程，也没有遥测。

<img src="docs/screenshot-dark.png" alt="Kanso 深色主题" width="100%">

<img src="docs/screenshot-light.png" alt="Kanso 浅色主题" width="100%">

## 小，而且一直小

| 安装包 | 下载体积 |
| --- | --- |
| Windows x64 安装程序（`.exe`） | **3.9 MB** |
| macOS Apple Silicon（`.dmg`） | **4.7 MB** |
| Linux `.deb`（x64 / ARM64） | **5.3 MB** / **5.2 MB** |

界面是 web view，终端、SSH 客户端、PTY 和文件传输都是 Rust。启动 Kanso 是"窗口出现"，不是"运行时启动"。

## 本地 shell

真正的伪终端，窗口缩放会同步给进程。Shell 字段接受带参数的命令行，所以 `wsl.exe -d Ubuntu` 或 `pwsh -NoLogo` 就是一个会话。在 macOS 上，不带参数的 shell 以登录 shell 启动，和 Terminal.app 的做法一致，因此 `~/.zprofile` 和 Homebrew 的 `PATH` 会在 `~/.zshrc` 之前就位。

每个提示符上方会画一条细线，一条命令的输出和上一条不会糊在一起。

## SSH

支持密码、公钥和 ssh-agent 认证，之后还能继续进行 keyboard-interactive 轮次——服务器用它们来要第二因素，比如一次性验证码或推送确认。跳板机（ProxyJump）通过另一个已保存的会话连接，一条路由需要多跳时会自动串联。

**老设备也连得上。** 交换机、路由器、防火墙上的 SSH 服务常常停在这些年现代客户端已经不再提供的算法上。Kanso 同时提供 SHA-1 密钥交换、CBC 加密和 SHA-1 MAC——排在所有现代算法之后，不需要改任何设置。支持更好算法的服务器会拿到更好的；而由于双方的算法列表都由服务器主机密钥签名，中间人无法剥掉更好的选项来强推旧算法。用到了旧算法的会话会在状态栏标出 **Legacy SSH**。

**编码。** 默认 UTF-8，会话里可以改：对方说 GB18030 / GBK、Big5、Shift_JIS、EUC-JP、EUC-KR 或某个 Windows / KOI8 代码页时，它的输出会为终端解码，你键入的内容会为对方编码。

**传文件不用二次登录。** Filer 面板复用同一条 SSH 连接，所以浏览和传输文件不需要再认证一次：两个方向都能拖拽，也可以直接跳到 shell 当前所在的目录。

## 终端本身

- xterm.js 走 WebGL 渲染，带 Unicode 11 宽度表。
- 时间戳与行号边栏，长会话也读得下去。
- 搜索、右键菜单、中键粘贴，需要的话还有选中即复制。
- 可嵌套的分栏，每个窗格有自己的标签栏。
- 不捆绑字体：字体字段列出本机已装字体，也接受你手输的任何名字，私有 Nerd Font 构建同样可用；Nerd Font 字形会自动回退到它。
- 深色与浅色主题，所有快捷键都可改。

## 安装

| 平台 | 安装包 |
| --- | --- |
| Windows x64 | NSIS 安装程序（`.exe`）与便携版 `.zip` |
| macOS Apple Silicon | `.dmg` |
| Linux x64 / ARM64 | `.AppImage` 与 `.deb` |

下载见 [Releases 页面](https://github.com/chhuax/Kanso/releases/latest)。

Windows 便携版把所有设置放在可执行文件旁边的 `data` 目录里，整个目录可以在机器之间搬，也可以放在移动盘上；AppImage 在 Linux 上免安装就地运行。Release 不做 macOS 公证，也没有 Windows Authenticode 签名，首次安装时系统可能弹出安全提示。

本版本已关闭自动更新。**Help → Check for Updates…** 会在浏览器中打开 Releases 页面，由你自行下载新版本。

## 项目来源

Kanso 是 miskin-lee 的 [EdgeTerm](https://github.com/miskin-lee/EdgeTerm) 的**修改版** fork：分叉点为提交 `39d35ca`（v0.8.3），日期 2026 年 9 月 17 日，此后持续修改。它是一个独立项目：与 EdgeTerm 及其作者无隶属关系、未获其赞助或背书，也不是 EdgeTerm 本身。上游代码部分的版权仍归 EdgeTerm 作者所有，改动的版权归 Kanso 贡献者所有。改动内容以 `git log 39d35ca..HEAD` 为准；GPL-3.0 第 5(a) 条要求的修改声明见 [NOTICE](NOTICE)。

## 许可证

Kanso 沿用 EdgeTerm 的 [GNU General Public License v3.0](LICENSE)，且**仅此一版**（`GPL-3.0-only`）。它是自由软件：你可以按相同条款再分发和修改；你分发的任何衍生作品都必须以 GPL-3.0 发布，并提供完整的对应源码。本软件**不提供任何担保**。

随应用一起分发的第三方组件：

- **Material Icon Theme** —— Filer 面板的文件类型图标。MIT，Copyright (c) 2025 Material Extensions。
- **Codicons** —— 界面图标，[Microsoft](https://github.com/microsoft/vscode-codicons)。按 CC BY 4.0 使用。

它们的许可证全文和署名声明见 [NOTICE](NOTICE)。
