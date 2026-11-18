# Kanso 设计文档

> 状态：2026-11-16 更新。本仓库为独立重写的 macOS 原生终端，以 GPL-3.0-or-later 开源，版权归 chhuax；与旧版 chhuax/Kanso-legacy（基于 EdgeTerm 的 Tauri 版）无代码继承关系。

## 1. 定位

macOS 原生终端 + SSH + SFTP 客户端，开源免费。Windows / Linux 版计划基于 GPUI 实现。

当前发行 Direct 开源构建。商业渠道（官网付费构建、App Store、Setapp）暂缓，日后再定；保留现有渠道构建配置。

## 2. 许可与独立开发（硬约束）

- 本仓库以 GPL-3.0-or-later 开源，版权归 chhuax；名称和应用图标不在 GPL 授权范围内。
- 旧版 `chhuax/Kanso-legacy` 的代码**不得**复制、改写、翻译进本仓库。唯一允许的输入是 `docs/spec/` 下的**纯文字规格**（行为、边界、踩坑结论，不含代码片段与结构图），以及 RFC、OpenSSH 文档、各依赖库文档、Apple 文档等一手资料。
- 实现阶段不打开旧源码，也不把旧源码提供给 AI 工具。规格提炼与实现在不同会话中进行。
- 依赖只允许宽松协议（MIT / Apache-2.0 / BSD / ISC / Zlib 等），CI 用 `cargo-deny` 拦截 GPL / LGPL / AGPL。
- 旧版仓库改名为 `chhuax/Kanso-legacy`，继续服务 Windows / Linux 用户（0.x）；与新版不共享代码。Bundle ID 使用作者控制的域名。

## 3. 发行版

以下保留商业渠道的原始规划表，全部暂缓，不代表当前开源发行的价格、签名或更新能力。当前 Direct 构建开源免费、临时签名、未公证。

| | **Kanso Direct**（官网，主力） | Kanso（Mac App Store） | Kanso for Setapp |
|---|---|---|---|
| 平台 | mac / Windows / Linux | mac | mac |
| 沙盒 | 无；mac 用 Developer ID 签名并公证，Windows 用 Authenticode 签名 | 强制 App Sandbox | 无（Developer ID + 公证） |
| 本地 shell | 完整 | 受限（见 §5） | 完整 |
| 收费 | 付费下载，License Key 激活（Paddle / Lemon Squeezy 作为 MoR） | 付费下载 | Setapp 会员订阅分成，不能有试用和付费功能 |
| 更新 | Sparkle（mac）/ 自写更新器 | App Store | Setapp 负责 |
| 构建开关 | 默认（`direct`） | `mas` | `setapp` |

- 商店版在**编译期**去掉 License Key、自带更新器和站外购买链接。
- Setapp 版在**编译期**去掉 License Key 和 Sparkle，接入 Setapp SDK（授权、使用时长上报、新版本说明）。
- 三个渠道的购买记录互不相通。

## 4. 收费（产品决定 2026-11-16）

开源免费（GPL-3.0-or-later）；商业渠道（官网付费构建、App Store、Setapp）暂缓，日后再定。

## 5. MAS 沙盒影响

本节只适用于商店版；Direct 版和 Setapp 版没有沙盒，所有功能完整。商店版是第二渠道，表中标 ❓ 的项目等商店版启动时再实测。

商店版首次启动时引导用户**一次性授权主目录**（NSOpenPanel 选择 `~`，保存为 app-scope 的 security-scoped bookmark，以后每次启动自动恢复）。用户拒绝时功能照常可用，只是需要在用到的地方逐个授权文件或目录。

| 功能 | 授权主目录后 | 说明 |
|---|---|---|
| SSH / SFTP / 跳板机 | ✅ | `network.client` |
| 读 `~/.ssh` 私钥 | ✅ | 也可以导入 Keychain，不依赖授权 |
| known_hosts | ✅ | 直接使用 `~/.ssh/known_hosts`；未授权时存到容器内 |
| Filer 本地浏览 | ✅ | 主目录以外的路径需要另行授权 |
| Git 面板 | ✅ | 用 `gix` 读取，不调用 git 命令行 |
| kubectl 补全 | ✅ | 读取 `~/.kube` |
| 拖入 / 拖出 | ✅ | file promise |
| 自动更新 | — | 由 App Store 负责，商店版不编译自带的更新器 |
| ssh-agent | ❓ | 待 spike：1Password 等放在主目录里的 agent socket 可能连得上；系统自带的 agent 大概率连不上。替代方案是内置 agent（Keychain / Secure Enclave） |
| 本地 shell | ❓ | 子进程继承沙盒。Homebrew 工具和 AI CLI 能不能正常用，取决于：用户授权能否被子进程继承、能否执行 `/opt/homebrew` 下的程序、能否写 `/tmp`、能否读钥匙串（Claude Code 的登录凭据存在这里）。待实测。备选方案：`NSUserUnixTask` 运行用户自己放置的脚本，在沙盒外拉起 PTY 辅助进程，审核能否通过需要提交验证 |
| ProxyCommand | ❌ | 只支持 ProxyJump（自实现） |

## 6. 架构

```
kanso-core (Rust，三平台共享，不依赖任何 UI 框架)
  term      VT 引擎封装（alacritty_terminal，可替换）+ 行元数据侧表（时间戳/行号/OSC 133）
  ssh       russh：认证、跳板、Legacy 算法、SFTP 流水线
  pty       portable-pty、cwd 探测、shell 集成
  session   会话管理、编码转换、ZMODEM/XMODEM、活动检测
  store     配置/会话/命令库；SecretStore trait
  complete  补全引擎
  license   Entitlement
platform    SecretStore / FileAccess / Purchase / Updater / DragOut / Locale 的各平台实现
        │  UniFFI 生成 Swift 绑定；终端每帧只传可见区域快照
        ├── macOS：Swift + AppKit（SwiftUI 做设置与对话框）+ Metal 渲染 + CoreText 排版
        └── Windows / Linux（M5）：倾向 GPUI（Apache-2.0），开工前用原型验证输入法、文件列表、包体积
```

要点：
- 终端视图在 mac 上自行实现 `NSTextInputClient`，负责预编辑显示、候选框定位、上屏、组字期间的按键归属、Ctrl/Option 组合键。
- 不得参考 Zed 编辑器的应用代码（GPL）；只使用 `gpui` 库本身。
- Windows/Linux 不使用 Tauri/React/xterm.js：一是性能，二是与旧代码同栈，衍生风险高。

### 性能设计
1. 数据路径不跨语言：IO 线程读 PTY/SSH → 解码 → 写 VT 状态 → 标记脏行，每帧最多唤醒 UI 一次。
2. 按 vsync 只取可见脏行渲染；后台标签只解析不渲染。
3. 洪水输出时跳帧，解析跑到底，屏幕只画最新状态。
4. 回滚按字节设上限，旧页可压缩；搜索在后台线程。
5. SFTP 读写流水线（多个在途请求 + 大窗口），目录列表流式返回。
6. 启动不加载 WebView，字体延迟加载。

### 性能目标（spike 阶段先测旧版基线再校准）

| 指标 | 目标 |
|---|---|
| 冷启动到可输入 | < 150 ms（M1） |
| 单窗口单标签空闲内存 | < 60 MB |
| `cat` 1 GB 吞吐 | ≥ Alacritty 的 80% |
| 按键到上屏 | < 1 帧 |
| 20 个后台会话持续输出 | UI 60/120 fps |
| SFTP 千兆局域网单文件 | ≥ 80 MB/s |
| 10 万行回滚搜索 | < 50 ms |

## 7. 发布合规清单

### Direct 商业版（暂缓，未来发布前执行）
- Developer ID 签名、Hardened Runtime、公证（notarytool），并 staple 公证票据。
- Sparkle 更新：用 EdDSA 签名更新包，appcast 走 HTTPS。
- License Key：Ed25519 签名，可以离线校验；激活设备数上限 **[待定]**。
- 隐私政策、EULA、第三方许可页面。

### 商店版（第二渠道，启动时执行）

- 权限：`app-sandbox`、`network.client`、`files.user-selected.read-write`、`files.bookmarks.app-scope`；不申请 temporary exception。
- Hardened Runtime。
- `PrivacyInfo.xcprivacy`：申报 required-reason API（Rust `Instant` 用到 `mach_absolute_time`、文件时间戳、UserDefaults、磁盘空间等逐项核对）。
- 出口合规：`ITSAppUsesNonExemptEncryption = YES`，按问卷申报，可能需向 BIS 提交年度自分类报告。
- 恢复购买入口、隐私政策 URL、支持 URL、隐私营养标签。
- 最低 macOS 14（需要 `NSView.displayLink` 与 Observation 框架）；商店版与 Setapp 版只提供 Apple Silicon（arm64）包，App Store 只会向 Apple Silicon 机型提供下载（商店每个版本只能上传一个包，单独出 Intel 包做不到）。
- 官网版（Direct）按架构分别出包：`Kanso-<版本>-macOS-arm64.dmg` 与 `Kanso-<版本>-macOS-x86_64.dmg`，不做 Universal 合并包，避免安装包体积翻倍。Windows 用同一规则：`Kanso-<版本>-Windows-x64-portable.zip` 与 `Kanso-<版本>-Windows-x64-setup.exe`。

## 8. 里程碑

- **M0**：商标检索、Apple Developer 账号、新 Bundle ID；提炼 `docs/spec/`。
- **M1 spike**：Rust 核心最小终端 + Swift/Metal 视图，测性能，与旧版基线对比。
- **M2**：mac MVP（Direct 版）：本地终端、SSH/SFTP、标签/分屏、Keychain、深浅色、中英文、License Key、Sparkle。
- **M3**：补齐功能（Filer、Sender、时间戳栏、补全、ZMODEM/XMODEM、OpenSSH 配置导入）。
- **M4**：发布 GPL-3.0-or-later 开源版；官网付费构建、公证与收款渠道暂缓。
- **M5**：App Store、Setapp 商业渠道暂缓，日后再定。
- **M6**：Windows / Linux（先做 GPUI 原型再定）。
