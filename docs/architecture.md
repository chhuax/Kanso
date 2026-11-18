# Kanso 架构设计

> 配合 `docs/design.md`（产品与发行决策）阅读。本文定义模块边界、线程模型、数据流、存储、多语言和主题。
> 状态：草案 v1（2026-11-12）。

## 1. 总览

```
┌──────────────────────────── macOS App（Swift）────────────────────────────┐
│ KansoApp      应用入口、窗口、菜单、Settings、命令面板                      │
│ KansoUI       标签栏 / 分屏 / Filer / Sender / 对话框（SwiftUI + AppKit）  │
│ TerminalKit   终端 NSView：Metal 渲染、CoreText 字形、NSTextInputClient    │
│ PlatformMac   Keychain 辅助、security-scoped bookmark、StoreKit 2、        │
│               file promise、Secure Enclave、Sparkle（仅 Direct）           │
│ KansoBridge   UniFFI 生成的 Swift 绑定 + 按 C 头文件零拷贝读帧的封装       │
└───────────────▲─────────────────────────────────────────▲─────────────────┘
     控制面：UniFFI（异步函数、对象、回调 trait）    数据面：UniFFI 交出帧缓冲地址，按 C 布局零拷贝读
┌───────────────┴─────────────────────────────────────────┴─────────────────┐
│ kanso-ffi     对 Swift 暴露的唯一出口；把 core 的类型映射成 FFI 友好的形状  │
├───────────────────────────── kanso-core（Rust）────────────────────────────┤
│ runtime   tokio 运行时、任务调度、事件合并                                  │
│ term      VT 引擎封装、行元数据、按键编码、选区、搜索、帧快照               │
│ session   会话生命周期、本地 PTY、SSH 终端、SFTP、传输队列、活动检测         │
│ ssh       连接、算法、主机密钥、认证、跳板、共享 transport                  │
│ proto     ZMODEM / XMODEM、编码转换（与 UI 无关的协议库）                   │
│ store     SQLite：会话、分组、设置、命令库、历史；导入导出；迁移            │
│ secret    SecretStore trait 与各平台实现                                    │
│ complete  命令补全引擎                                                      │
│ license   Entitlement（License Key / StoreKit / Setapp 三种来源）           │
│ platform  宿主需要实现的回调 trait（文件授权、购买、通知……）               │
│ i18n      错误与消息的结构化定义（不在 core 里拼接用户可见文字）            │
└────────────────────────────────────────────────────────────────────────────┘
Windows / Linux（M6）：kanso-gpui 直接依赖 kanso-core，不经过 FFI。
```

原则：
- **业务逻辑全部在 core 里**，Swift 只负责界面和系统集成。core 不依赖任何 UI 框架，GPUI 版可以直接复用。
- **控制面和数据面分开**：
  - 控制面（连接、打开 SFTP、保存配置）走 UniFFI。调用频率低，追求开发效率。
  - 数据面（每帧的终端快照）**内容零拷贝，取帧接口走 UniFFI**：core 把一帧写进 Rust 持有的连续缓冲区，
    Swift 通过 UniFFI 的 `FrameHandle` 对象拿到缓冲区地址后直接按 `docs/frame-abi.md` 的布局读，不经过序列化。
    原定的"手写 C ABI 取帧"行不通：Swift 拿不到 UniFFI 对象（`KansoCore`）的裸指针，没法把 core 传给手写 C 函数；
    让 core 另外暴露一个全局句柄又会引入全局状态。所以只有句柄的创建与归还走 UniFFI（每帧一次轻量调用），
    唯一的 unsafe 约定是句柄的生命周期（见 frame-abi.md §8）。手写 C ABI 只保留 mock 帧的三个函数，
    供 C 冒烟测试与 `--snapshot` 截图模式使用。键盘等输入事件是低频小对象，直接走 UniFFI 控制面。
- **core 不产生面向用户的文字**。错误和提示都用"错误码 + 参数"的结构返回，由各平台 UI 做本地化（见 §10）。

## 2. Rust 内核拆分

内核拆成多个 crate，好处有三个：每个 crate 可以由一个人或一个 agent 独立开发和测试；编译边界能强制依赖方向；与 UI 无关的协议库将来可以单独以 MIT 协议开源。

### 2.1 crate 一览

| 层 | crate | 职责 | 关键依赖 | 是否 sans-IO |
|---|---|---|---|---|
| 基础 | `kanso-types` | 各类 id（会话、配置、分组）、几何与颜色基本类型、**用户可见消息的 key 枚举与参数**、core 推送给 UI 的事件枚举 | serde | ✅ |
| 协议 | `kanso-encoding` | 流式编码转换：能处理跨块截断的多字节字符；支持 GB18030/GBK/Big5/Shift_JIS/EUC-JP/EUC-KR/Windows/KOI8 | encoding_rs | ✅ |
| 协议 | `kanso-xfer` | ZMODEM、XMODEM（含 1K、CRC、checksum）的状态机，以及在输出流里识别 ZMODEM 握手 | — | ✅ |
| 协议 | `kanso-sshconfig` | OpenSSH 客户端配置的解析与解析求值（Host 匹配、Include、`%` 替换、ProxyJump） | — | ✅（读文件通过 trait） |
| 终端 | `kanso-term` | 封装 alacritty_terminal：网格与回滚、行元数据侧表、OSC 7/8/133/52、按键编码（含 kitty protocol）、选区、搜索、**帧快照**、活动检测 | alacritty_terminal、regex | ✅ |
| 终端 | `kanso-frame` | 帧快照的二进制格式（`#[repr(C)]` 布局）、写入器、读取器与 mock 生产者；kanso-term 产出帧，kanso-ffi 把帧指针交给 Swift。格式见 `docs/frame-abi.md` | bytemuck | ✅ |
| IO | `kanso-pty` | 启动本地 PTY、调整大小、查询前台进程与 cwd、shell 集成 shim、locale 环境 | portable-pty、libc | ❌ |
| IO | `kanso-ssh` | 算法表、主机密钥策略（存储通过 trait 注入）、认证（向用户提问走 `CoreEvent::Prompt`）、跳板链、共享 transport、SFTP 操作、**流水线传输引擎**、远端 cwd 查询 | russh、russh-sftp | ❌ |
| 数据 | `kanso-store` | SQLite 表结构与迁移、各类数据的读写、`.kanso` 导入导出、ssh_config 导入（调用 `kanso-sshconfig`） | rusqlite | ❌ |
| 数据 | `kanso-secret` | SecretStore trait，以及 Credential Manager（`windows-sys`）、Secret Service（`secret-service`）两种系统实现和内存实现；macOS 的 Keychain 实现在 Swift 宿主里，经 FFI 注入 | windows-sys、secret-service | ❌ |
| 业务 | `kanso-complete` | 补全引擎：历史、命令表、路径（通过 provider trait 获取）、git/kubectl | — | ✅ |
| 业务 | `kanso-license` | Entitlement 模型、License Key（Ed25519）离线校验；StoreKit、Setapp 来源通过平台 trait 接入 | ed25519-dalek | ✅ |
| 编排 | `kanso-session` | 会话管理器和每个会话的 actor：把 PTY/SSH、编码、传输哨兵、终端状态串起来；Sender 的发送与定时；传输队列 | tokio，以及以上各 crate | ❌ |
| 门面 | `kanso-core` | `Core` 对象：持有运行时，注入各平台 trait，对外提供**唯一的公开 API**，供 FFI 和 GPUI 使用 | kanso-session、store、license、complete | ❌ |
| 门面 | `kanso-mcp-server` | MCP 服务（仅 Direct）：工具定义、授权、审计，是 `Core` 公开 API 的又一个使用方，见 §16 | rmcp、schemars | ❌ |
| 绑定 | `kanso-ffi` | UniFFI 导出 Core 与镜像类型；帧通过 `FrameHandle` 交出缓冲区地址，Swift 按 `kanso_frame.h` 零拷贝读取（见 `docs/frame-abi.md` §8） | uniffi | ❌ |
| 工具 | `xtask` | 构建 XCFramework、生成 design token 和多语言目录、校验许可证与多语言 key | — | — |
| 工具 | `kanso-mcp` | 随 App 打包的 stdio 转发小程序，通过仅限当前用户的本机通道转发字节，见 §16 | libc（Unix）、windows-sys（Windows） | ❌ |
| 工具 | `kanso-bench` | 吞吐和延迟基准测试（大量 cat 输出、多会话并发、SFTP） | criterion | — |

"sans-IO"指 crate 里只有纯逻辑和状态机，不直接做网络或文件读写。这类 crate 最容易并行开发、做单元测试，也最适合将来以 MIT 协议开源。

### 2.2 依赖方向（只允许从上往下依赖）

```
                     kanso-ffi        kanso-gpui(M6)
                          │  └─ kanso-mcp-server（仅 Direct）
                          └──────┬───────┘
                            kanso-core
            ┌───────────────┬────┴─────┬──────────────┐
      kanso-session    kanso-store  kanso-license  kanso-complete
   ┌──────┬──────┬─────┬──────┐   │
 term   pty    ssh  encoding xfer  sshconfig
   └──────┴──────┴─────┴──────┴───┴──── kanso-types ────┘
                       kanso-secret（由 core 注入到 ssh / store）
```

- `kanso-types` 不依赖任何其他 crate。
- `kanso-mcp`（转发程序）不依赖任何内部 crate，它与 App 之间只通过本机 socket 通信。
- 协议类和终端类 crate 之间**互不依赖**，组合工作统一交给 `kanso-session`。
- 只有 `kanso-ffi` 可以出现 `extern "C"` 和 UniFFI 注解。

### 2.3 需要最先定下来的契约（并行开发的前提）

1. **`kanso-types`**：id、消息 key、事件枚举。
2. **帧快照 ABI**：C 头文件形式的单元格结构、样式位、颜色编码、光标与选区、行元数据。定下来后，Swift 的渲染器和 `kanso-term` 可以各自对着这份格式开发。
3. **`kanso-core` 公开 API**：先只写签名和文档，函数体统一是 `todo!()`；同时提供一个 **mock 实现**，让 Swift UI 不用等内核完成就能开工。
4. **平台 trait**：SecretStore、FileAccess、Purchase、HostKeyStore、Notifier。
5. **数据库 schema v1**。
6. **design token 格式** 和 **多语言 key 命名规则**。

## 3. 仓库结构

```
kanso/
  Cargo.toml                 # workspace
  crates/                    # §2.1 中的各个 crate
    kanso-gpui/              # M6
  xtask/
  apple/
    Kanso.xcodeproj          # 三个 scheme：Kanso-Direct（主力）、Kanso-MAS、Kanso-Setapp
    Packages/
      TerminalKit/  KansoUI/  PlatformMac/  KansoBridge/
    Kanso/                   # App target：Assets、Localizable.xcstrings、entitlements
  design/
    tokens.json              # 颜色、间距、圆角、字号的唯一来源（§11）
    terminal-themes/         # 内置终端配色
  locales/                   # 跨平台共享的消息目录（§10）
  scripts/                   # 构建 XCFramework、生成 token、校验多语言 key
  docs/
```

构建流程：`cargo build` 按架构编译 `aarch64-apple-darwin`（发行默认）或 `x86_64-apple-darwin`（Intel 开发机与官网 Intel 包，可选；不做 Universal 合并），打包成 XCFramework，同时生成 UniFFI 的 Swift 绑定；Xcode 通过 Run Script 调用这一步，再用 SwiftPM 引用生成结果。

## 4. 线程与并发模型

| 执行者 | 职责 |
|---|---|
| tokio 运行时（core 进程内唯一一个，工作线程数取 `min(4, 核数)`） | 所有网络、PTY 读写、SFTP、传输 |
| 每个会话一个 actor 任务 | 从命令通道收命令；读取字节流，喂给终端状态；向 UI 推事件 |
| 每个终端一份状态，用短临界区锁保护 | IO 任务写入；渲染时读取快照，持锁时间控制在 1 ms 以内 |
| 主线程（Swift `@MainActor`） | UI 状态、事件分发、按 display link 节拍渲染 |
| 后台 `Task`（Swift）和 `spawn_blocking`（Rust） | 回滚搜索、目录扫描、补全的文件查询 |

关键约定：
- **唤醒合并**：IO 任务每次写入后设置终端的脏标记。只有标记从"干净"变成"脏"时，才通过回调通知 UI 一次，大量输出不会产生大量回调。
- **按帧拉取**：macOS 14 的 `NSView.displayLink` 每帧只处理**可见且脏**的终端，从 core 拉取快照并渲染。没有脏终端时暂停 display link，避免空闲耗电。
- **洪水输出时跳帧**：解析一直跑到最新数据，渲染只画最新状态。
- **背压**：PTY/SSH 的读取跟着解析速度走，不在内存里无限堆积。传输队列有并发上限。
- core 把事件推给 UI 的方式是回调 trait；Swift 一侧收到后切到主线程处理，回调本身不做任何耗时工作。

## 5. 终端数据流

```
输出：PTY / SSH 通道字节
   → 传输协议哨兵（ZMODEM 握手检测，必须看原始字节）
   → 编码转换（非 UTF-8 会话：GB18030 / Big5 / Shift_JIS … → UTF-8）
   → VT 解析（alacritty_terminal）→ 网格 + 回滚
   → 行元数据侧表（按绝对行号）：时间戳、行号、OSC 133 提示符/命令边界、OSC 7 目录、OSC 8 链接
   → 活动检测（后台标签的运行/完成状态、AI CLI 的轮次）
   → 脏标记 → UI 拉帧

输入：NSEvent / IME 上屏文本
   → core 的按键编码器（根据终端当前模式：DECCKM、bracketed paste、kitty keyboard protocol）
   → 编码转换（非 UTF-8 会话）
   → PTY / SSH 通道
```

- **按键编码放在 core 里**，mac 和 GPUI 共用，保证行为一致。Swift 只把物理按键、修饰键和 IME 文本传过去。
- **帧快照格式**：由 Rust 持有一块连续缓冲区，每个单元格包含字形 id 或码点、前景/背景（调色板索引或 RGB）、样式位；外加光标、选区、行元数据。Swift 通过指针只读访问，用完通知释放。
- **颜色在网格里按"索引或 RGB"存储**，不预先解析成具体颜色。所以切换深色/浅色主题时只需要换一份调色板重画，不用重新解析输出（见 §11）。
- **回滚**：容量按字节设上限，可以配置；旧页可以压缩。搜索在后台线程用 `regex` 执行，结果按行号返回。

## 6. 渲染（macOS）

- `CAMetalLayer`，三层实例化绘制：背景色块、字形、装饰（下划线、光标、选区）。
- **字形图集**：灰度图集和彩色 emoji 图集分开；用 CoreText 栅格化；回退顺序是"用户字体 → Nerd Font（如果装了）→ 系统 CJK 字体 → emoji"。Powerline 分隔符（U+E0B0–E0BF）和少量上下文图标不走字体，由渲染器按单元格几何直接栅格化进灰度图集。
- 宽字符按 Unicode 东亚宽度处理，"歧义宽度"字符是 1 列还是 2 列可以设置。
- 时间戳/行号栏是独立的一列，用同一套管线绘制，不参与文本选择。
- 支持 ProMotion 120 Hz；连字作为可选项，放到后期做。
- 性能目标见 `design.md` §6。

## 7. 会话与连接

- core 里由会话管理器统一管理所有活动会话，UI 只持有会话 id。
- SSH 的具体规格见 `docs/spec/ssh.md`。架构层面要点：
  - 多个标签共享同一个已认证的 transport（包括整条跳板链），按引用计数关闭。
  - SFTP 子系统按需打开，每个 transport 只开一个；传输任务走单独的队列，不阻塞终端。
  - 传输进度等通知通过 `CoreEvent` 推给 UI。
  - **需要用户回答的请求**（主机密钥变更、keyboard-interactive、缺失凭据、ZMODEM 选文件）统一发 `CoreEvent::Prompt { prompt: PromptId, request }`，UI 通过控制面的统一应答接口回传 `PromptResponse`。超时（如 keyboard-interactive 的 180 秒）由 core 计时，到期发 `PromptDismissed` 让 UI 关掉对话框。
  - 不为提问单独定义平台 trait：一条事件流更容易 mock，GPUI 版也能直接复用，core 侧的超时与取消也只需实现一次。
- **断线重连** **[待定]**：Pro 功能，自动重连并重建通道（远端的 shell 状态无法恢复，可以提示用户用 tmux）。

## 8. 存储

- 一个 SQLite 数据库（rusqlite，用 bundled 方式自带 SQLite），文件位于 Application Support；MAS 版位于容器内。表包括：
  - `profiles`、`groups`：会话与分组；
  - `settings`：键值形式的设置；
  - `snippets`：Sender 保存的命令，带作用域；
  - `history`：补全用的命令历史；
  - `known_hosts`：仅 MAS 版使用；
  - 版本号用 SQLite 的 `PRAGMA user_version`，不单独建表；表结构与迁移规则见 `docs/schema.md`。
- **为将来的 iCloud 同步留好余地**：每条记录用 UUID 作为 id，带 `updated_at`，删除时留墓碑记录。
- **机密数据不进数据库**：密码、口令、私钥走 SecretStore，在数据库里只保存引用 id。
- 导出导入使用新的 `.kanso` 格式，带版本号的 JSON。**不兼容旧 GPL 版的格式**；如果需要迁移，单独评估。
- 窗口布局（标签、分屏、面板尺寸）也存进数据库，用于恢复上次的界面。

## 9. 平台抽象

core 定义 trait，宿主实现后在启动时注入：

| trait | macOS 实现 | Windows / Linux |
|---|---|---|
| SecretStore | Keychain（Rust `security-framework` crate） | Credential Manager / Secret Service |
| HardwareKey | Secure Enclave 签名（Swift 实现，通过回调接入 core） | 不提供 |
| FileAccess | security-scoped bookmark 的创建、恢复、开始访问、结束访问（MAS）；Direct 版为空实现 | 空实现 |
| Purchase | Direct：License Key（core 内置，Ed25519 签名，可离线验证）；MAS：StoreKit 2（Swift）；Setapp：Setapp SDK | License Key |
| Updater | Sparkle（仅 Direct；MAS 和 Setapp 由各自平台负责） | 自写更新器 |
| Notifier | UserNotifications（后台标签任务完成时提醒） | 系统通知 |

发行版的区分：
- cargo feature `direct`（默认）/ `mas` / `setapp`。
- Xcode 的 Swift 编译条件 `MAS`、`SETAPP`，三个 scheme 各用一份 entitlements 文件。
- MAS 构建在编译期去掉 License Key、更新器和外部购买链接；Setapp 构建在编译期去掉 License Key、Sparkle 和免费版/Pro 开关。
- 先只实现 Direct；`mas` 和 `setapp` 从一开始就保留开关和平台抽象，但具体实现放到 M5。CI 从第一天起就编译这两种配置，防止平台抽象被绕过。

## 10. 多语言

**首发语言**：简体中文、英文。之后计划加入繁体中文、日文。界面布局统一使用 leading/trailing，暂不支持从右到左的语言。

**分层**：

| 来源 | 存放位置 | 说明 |
|---|---|---|
| mac UI 文字 | `Localizable.xcstrings`（String Catalog） | Xcode 原生支持，处理复数、字符串变体和 App Store 本地化 |
| core 产生的消息（错误、认证提示、传输状态） | `locales/<lang>.ftl`（Fluent 格式），作为唯一来源 | core 只返回消息 key 和参数；由 `kanso-l10n` crate 用 `fluent-bundle` 在运行时渲染（`.ftl` 编进二进制），经 FFI 的 `render_message` 把文字交给 Swift；GPUI 版直接复用同一个渲染器 |
| Win/Linux UI 文字 | `locales/` 中的 Fluent 文件 | M6 时接入 |

**决定：core 消息由 Rust 运行时渲染，不生成 String Catalog。** 早先设想用脚本把 `.ftl` 转成 mac 的 String Catalog，已放弃，理由：
- Fluent 的 select 表达式、复数（`[one]` / `*[other]`）以及 `reason-*` / `endpoint-*` 子 key 的嵌套，无法忠实映射成 String Catalog 的条目与变体，转换会丢语义或要在 Swift 里重写一遍规则；
- 由 Rust 渲染才能保证 `.ftl` 是唯一来源，不会出现“生成物与源文件不一致”；
- 以后的 GPUI 版（Win/Linux）直接复用同一个渲染器，三个平台的文案行为一致。

渲染器关闭了 Fluent 默认的 Unicode 双向隔离符（U+2068/U+2069），因为文字会被复制到终端、日志和剪贴板。Swift 只负责系统格式化器：文件大小、列表先转成字符串再交给渲染器（见 i18n.md）。

**规则**：
- core 里的错误是一个枚举，每个分支对应一个消息 key，所有参数都带名字。
- 禁止在 Rust 里用 `format!` 拼出用户可见的句子。日志例外，日志统一用英文。
- CI 检查三件事：每种语言的 key 是否齐全，参数名是否一致，有没有未使用的 key。
- **语言切换**：
  - mac 默认跟随系统；单个应用的语言用系统设置里的"按 App 设置语言"切换，应用内不再做语言选择。
  - Win/Linux 在设置里提供语言选项。
- **格式化**：文件大小、日期、相对时间都用平台的格式化器（Foundation 的 `ByteCountFormatter`、`Date.FormatStyle`）。时间戳栏固定显示 `HH:MM:SS.mmm`，不随语言变化。
- **终端内容不做本地化**；终端相关的国际化在于编码转换、宽字符和 IME（见 §5、§6）。
- App Store 的元数据、截图、内购项名称也要同时提供中文和英文版本。

## 11. 主题：深色与浅色

**两层主题，彼此独立**：

1. **界面主题（chrome）**：窗口、标签栏、面板、对话框。
   - 颜色以语义 token 定义，存放在 `design/tokens.json`，每个 token 都有 light 和 dark 两个值，例如 `bg.sidebar`、`bg.tabSelected`、`text.secondary`、`accent`、`diff.added`、`status.agentRunning`。
   - 用脚本把 token 生成为 mac 的 Asset Catalog 颜色集（区分 Any 和 Dark 外观），以及 GPUI 版用的 Rust 常量。只维护这一份来源。
   - 尽量优先使用 AppKit 的语义色（`labelColor`、`separatorColor` 等），自动适配高对比度模式和强调色；侧边栏使用系统的毛玻璃材质。
2. **终端配色**：前景、背景、光标、选区，以及 16 色 ANSI。
   - **外观模式**：跟随系统 / 固定浅色 / 固定深色。
   - **浅色和深色分别绑定一套终端配色**，默认是 Kanso Light 和 Kanso Dark。系统外观切换时实时生效，正在运行的会话不受影响（依赖 §5 中"颜色按索引存储"的设计）。
   - **最低对比度保护**（可以关闭）：渲染时，如果前景色与背景色的对比度低于阈值，就自动调整前景色亮度。这解决了"为深色背景设计的程序输出，在浅色主题下看不清"的问题。
   - Pro 功能 **[待定]**：导入 iTerm2 的 `.itermcolors` 或 Base16 配色、自定义配色。
- 每个会话可以单独指定配色，用于区分生产环境，比如生产服务器用偏红的背景。
- **主题相关的验收**：每个界面的截图测试都要在浅色、深色两种外观下各跑一次，每个 token 的两种取值都要满足 WCAG AA 对比度。

## 12. 界面结构

```
┌ 标题栏（透明）：侧栏开关 │ 命令面板「搜索会话、标签、文件、命令」 │ git 改动统计 ┐
├──────────────┬───────────────────────────────────────────────┬─────────────┤
│ 竖向标签栏    │ 分屏区（每个分屏有自己的标签组，可以嵌套、拖拽）  │ Filer（可收起）│
│ ─搜索 过滤 ＋ │                                               │             │
│ ▸ 标签行      │   终端视图                                     │             │
│   图标/状态   │                                               │             │
│   主标题      ├───────────────────────────────────────────────┤             │
│   副标题(分支)│ 提示行 chips：运行时版本 │ 路径 │ 分支 │ 改动 │ PR │             │
│              ├───────────────────────────────────────────────┴─────────────┤
│              │ Sender（可收起）                                             │
└──────────────┴──────────────────────────────────────────────────────────────┘
```

- **竖向标签栏**（保留现有风格）：
  - 顶部依次是搜索框、过滤按钮、新建按钮。
  - 每个标签行包含：左侧图标（shell 图标；AI agent 运行时显示 agent 图标和状态）、主标题（路径或会话名，过长时保留末尾部分）、副标题（git 分支或主机）。
  - 选中的标签高亮；后台标签有新输出时显示角标，完成后保持高亮，直到用户点开为止。
  - 右键菜单与 ＋ 按钮弹出的菜单相同。
  - 可以拖拽排序，也可以拖到分屏区；宽度可以调整；整栏可以收起。
  - 详细规格另写为 `docs/spec/ui-tabs.md`。
- 使用原生菜单栏，设置窗口用 SwiftUI 的 `Settings` scene；支持多窗口，标签可以在窗口之间拖动。
- 所有快捷键都可以自定义，默认值遵循 mac 惯例。

## 13. 错误、日志与诊断

- core 的错误分两层：用户可见的错误（消息 key + 参数，见 §10），以及内部错误链（用于日志）。
- 日志使用 `tracing`：mac 上输出到 `os_log`（subsystem 为应用的 bundle id），其他平台写入滚动文件。日志**绝不记录**按键输入、密码、口令、终端内容。
- 性能埋点：mac 上用 `os_signpost` 标记解析、拉帧、渲染各阶段，方便在 Instruments 里分析。
- 崩溃报告：MAS 版使用 Apple 自带的；Direct 版 **[待定]**，可以接入第三方服务，但必须由用户主动开启。

## 14. 测试

| 层 | 手段 |
|---|---|
| core 单元 / 集成 | `cargo test`；SSH 用进程内的 russh 服务端模拟各种设备（只支持旧算法、多轮 MFA、跳板机） |
| VT 正确性 | vttest、esctest 的子集；对网格内容做快照测试 |
| 渲染 | 固定字体下的截图比对，浅色和深色各一套 |
| 性能 | criterion 基准测试，加上吞吐脚本（cat 1 GB、20 个会话同时输出），在 CI 上跟踪趋势 |
| UI | XCUITest 覆盖关键路径：新建会话、连接、分屏、内购恢复 |
| 多语言 | key 完整性检查；伪本地化（拉长文字）下检查有没有截断 |

## 15. 依赖与许可证

- 白名单：MIT、Apache-2.0、BSD-2/3、ISC、Zlib、Unicode-3.0。
- **例外**：MPL-2.0 只允许用于 `uniffi`。MPL 是文件级弱 copyleft；第三方声明保留其许可及源码获取方式，与本仓库的 GPL-3.0-or-later 许可并行遵守。该例外不扩大到其他依赖。
- Sparkle 用的是 MIT 协议，可以使用。
- 禁止 GPL、LGPL、AGPL；Zed 应用代码不得参考。
- 应用内的"第三方许可"页面由 `cargo about` 加上 Swift 包清单自动生成。

## 16. MCP 服务

行为规格见 `docs/spec/mcp.md`。本节只定进程结构、模块落点和契约改动。第一版只在 Direct 渠道提供服务：真实服务与 `kanso-mcp` 转发程序只在 `direct` feature 下工作、只随 Direct 版打包。服务的公开接口与 FFI 导出在三个渠道都存在（见 §16.2 `kanso-ffi` 一行），非 Direct 渠道里它只报告"不支持"。

### 16.1 进程结构

```
AI 工具（Claude Code / Codex …）
   │ stdio（JSON-RPC，按 MCP stdio 传输分帧）
kanso-mcp（随 App 打包，macOS 在 App 包内，Windows 在 Kanso.exe 旁）
   │ 本机 Unix socket 或 Windows 命名管道（仅当前用户）
Kanso App 进程
   └ AppKit 通过 kanso-ffi 或 GPUI 直接接入 → kanso-mcp-server（rmcp）→ kanso-core 公开 API
```

- **`kanso-mcp` 不理解 MCP**，连上 socket 后只做双向字节转发。MCP 协议处理、授权、审计全部在 App 进程里进行。这样做有三个好处：
  - `kanso-mcp` 与 App 的版本天然一致，协议升级只改 App。
  - 凭据、数据库、钥匙串只有 App 进程能碰，`kanso-mcp` 被替换或被单独调用也拿不到任何东西。
  - 不需要像独立 MCP 服务那样单独分发、单独更新。
- **握手**：`kanso-mcp` 连上后先发一行 JSON（客户端令牌、控制终端设备名），之后才是 MCP 字节流。App 读走第一行，再把剩下的流交给 rmcp。
  - **令牌**（`spec/mcp.md` §1.1）：`kanso-mcp` 从环境变量 `KANSO_MCP_TOKEN` 读取并原样转交。App 用常量时间比较，不匹配时对 `initialize` 回错误后关闭连接。令牌由宿主保存在 macOS 钥匙串或 Windows 凭据管理器的 `mcp/client-token` 条目。重新生成后，宿主更新 `McpServer` 并断开旧连接。core、数据库和按 `profile/<uuid>/...` 存放的会话凭据使用独立的契约。
  - **识别客户端所在标签**（`spec/mcp.md` §2.5）有两条线索，命中任一条即标记：
    - 控制终端：把握手里的设备名与本地会话 PTY 的从设备名比对。
    - 进程祖先链：App 用 `LOCAL_PEERPID` 套接字选项取得对端的真实 pid（不信任自报的 pid），再沿父进程向上查找，直到遇到某个本地会话的 shell 进程，或者到达 launchd。
  - 用户在本地标签里套了 tmux 或 screen 时，AI 工具的祖先是 tmux 服务进程，控制终端也是 tmux 的伪终端，两条线索都不会命中。这是已知的盲区，规格里也写明了。
- **Unix 上连不上 socket 时**，`kanso-mcp` 用非阻塞 `flock` 试探单实例锁文件，借此区分两种情况：
  - 锁被占着，说明 App 在运行、只是 MCP 开关关闭，回"MCP is disabled"。
  - 能拿到锁，说明 App 没有运行，回"Kanso is not running"；随即释放锁，不改动任何文件。
  - 两种情况下，`kanso-mcp` 都从 stdin 读第一条 JSON-RPC 请求，以相同 id 回错误后退出。这是它唯一需要理解的 JSON-RPC 内容。
- **socket 位置**：`~/Library/Application Support/<bundle id>/mcp/kanso.sock`。目录权限 0700，socket 权限 0600；App 用 `getpeereid` 校验对端 uid 与自己相同。macOS 的 socket 路径上限是 104 字节，超出时两边都改用 `$TMPDIR/<bundle id>.mcp.sock`。`kanso-mcp` 和 App 用同一个按渠道编译进去的 bundle id 计算路径，不靠配置文件互相告知。
- **单实例（Unix）**：App 从启动到退出持有同目录锁文件的 `flock`。拿不到锁时设置页显示另一个实例已占用。开始监听前删除残留的旧 socket 文件。
- **Windows**：两端用 App ID 和当前用户 SID 计算命名管道名称。App 启动即独占运行探测管道，启用 MCP 后才创建数据管道。转发程序用探测管道区分未运行与已关闭。管道本机限定，DACL 仅允许当前用户，两端从对端进程的令牌再次核对 SID。句柄关闭时系统回收管道。stdin 结束时，转发程序在数据管道中发送一个原始 NUL 表示半关闭，仍保持读取方向以接收最后一条回复。

### 16.2 模块落点

| 组件 | 位置 | 说明 |
|---|---|---|
| `kanso-mcp-server` | 与 `kanso-ffi` 同层，依赖 `kanso-core` | 工具、授权和审计。AppKit 与 GPUI 通过 `McpServer` / `McpServerDelegate` 共用真实服务。界面测试可使用 mock 服务 |
| `kanso-mcp`（二进制） | `crates/kanso-mcp` | std 加系统 API 绑定。Unix 用 `libc` 处理文件锁和控制终端，Windows 用 `windows-sys` 处理命名管道 |
| `kanso-ffi` | 现有 | `KansoCore` 导出授权、总开关、审计读写；导出 `KansoMcpServer` 对象（`start`、`stop`、`set_token`、状态、客户端列表、断开客户端，mock 另有 `debug_*`）、回调接口 `FfiMcpServerDelegate`（状态、客户端、标签摆放、MCP 标识、标签释放）和 `mcp_client_config`。**三个渠道都导出**：Swift 绑定只生成一份、三个渠道共用，按渠道裁剪会让 MAS 版链接失败；非 Direct 渠道的服务恒为"不支持"。界面**只按 `mcp_supported()`** 决定是否显示 MCP 页面，不看 `start` 的结果：Direct 构建里 `start` 失败要在页面上显示错误，若因此隐藏页面，用户就再也关不掉开关 |
| `kanso-store` | 现有 | 新增 `mcp_grants`、`mcp_local`、`mcp_audit` 三张表（§16.4） |
| Swift | KansoUI、KansoApp | 设置 → MCP 页面、会话编辑对话框字段、标签行 MCP 标识、状态栏指示器、处理 MCP 打开标签的事件 |
| xtask | `apple-build`、`dmg` | 为目标架构编译 `kanso-mcp`，由 Xcode Run Script 复制进 `Contents/MacOS/`，用与 App 相同的签名身份和 hardened runtime 签名 |

依赖：`rmcp`（Apache-2.0，官方 Rust SDK，只启用 server 和基于 AsyncRead/AsyncWrite 的传输），参数 schema 用 `schemars`（MIT）。引入前跑 `cargo deny` 确认传递依赖都在白名单内。

### 16.3 kanso-core 的契约

契约已落在 `kanso-core`（`Core` 的"MCP"两节方法与 `kanso_core::mcp` 模块）、`kanso-types::mcp`（授权与审计类型）和 `kanso-store`（schema v3）。真实后端已实现授权、总开关与审计的存取；终端、连接、执行和 SFTP 补充操作返回 `NotSupported`，由 F9 实现。mock 后端全部可用，供 `kanso-mcp-server` 测试。

1. **输入来源不改现有接口**：`key`、`text_input`、`paste` 一律视为用户输入；MCP 的输入只走 `mcp_terminal_run`、`mcp_terminal_send`。用户接管（`spec/mcp.md` §4.3）由这两个调用在等待期间自行检测并返回 `TakenOver`，10 秒冷却由 `kanso-mcp-server` 记录。这样不用给现有输入接口加来源参数，避免牵动 FFI 与 Swift。
2. **读终端文本**：`terminal_text(session, scope, max_bytes)`，规则见 `spec/mcp.md` §5，与 `selection_text` 共用网格转文本的逻辑。
3. **执行并等待**：`mcp_terminal_run(session, command, limits)`，复用 Sender 的"登记即将发送 + 等待提示符"能力（`terminal.md` §5.3）。前置条件在 core 内与发送**在同一临界区**检查，不满足返回 `Rejected(TerminalBusy)`，因为只有 core 知道锁定、传输的状态，分开检查会在检查通过后状态又变了。Sender 是否正在发送由 `Core` 层（调度器在那里）判断。
4. **无界面会话与连接复用**：`mcp_connect(profile, mode)` 返回一个无界面会话，机制同纯 SFTP 会话，不出现在 `sessions()` 里，也不发会话事件；后台执行和 SFTP 都用它的 SessionId，因此现有的 `sftp_*`、传输接口不用另做一套。
   - 内核需要补上按 ProfileId 查找已认证 transport 的能力（现在只能经 `OpenTarget::Duplicate` 复用某个标签的连接）；多条时取最近建立且仍存活的那条。
   - 复用用户的连接为借用：该连接上最后一个标签关闭时，core 关闭这个无界面会话。自建连接的 5 分钟空闲释放由 `kanso-mcp-server` 计时，到期调用 `close_session`。
   - 无界面会话发起的传输照常发传输事件，UI 必须容忍传输事件里出现自己没打开过的 SessionId。
5. **非交互连接**：`ConnectMode::NonInteractive` 认证走到任何需要发 `CoreEvent::Prompt` 的地方都改为放弃，返回 `NeedsUser(UserNeed)`；`ReuseOnly` 只复用不新建，访问级别为只读的会话用它（`spec/mcp.md` §2.4）。`mcp_open_session` 打开可见标签，规则相同。
6. **后台执行**：`mcp_exec(session, command, limits)`，在会话所在的连接上开 exec 通道，同一连接最多 4 个并发（`spec/mcp.md` §3.4）。
7. **会话状态**：`mcp_session_status(session)` 返回活动、锁定、终端内传输、Sender、是否无界面、有没有 OSC 133、本地 PTY 从设备路径与 shell pid。不扩展 `SessionInfo`，因为它经 FFI 导出给 Swift。
8. **不新增 `CoreEvent`**：标签摆放、MCP 活动标识、客户端数变化都由 `McpServer` 对象自己回调宿主（F9 定义），core 不需要知道。授权变化由 `mcp_policy_watch()` 通知 `kanso-mcp-server`（总开关、会话级别、会话删除都会触发）。
9. **SFTP 补充**：`sftp_entry`（可选不跟随符号链接）、`sftp_read`（按偏移分段）、`sftp_write`（`CreateNew` / `Overwrite`；覆盖用 `posix-rename@openssh.com`，目标是符号链接时写真实文件，服务器没有该扩展返回 `NotSupported`）、`sftp_rmdir`（只删空目录）。传输的同名冲突策略复用已有的 `ConflictPolicy`（`Fail` / `Overwrite` / `Skip`），不需要新增。

### 16.4 存储

- schema v3 新增 `mcp_grants`（会话级别，没有记录即关闭）、`mcp_local`（总开关）、`mcp_audit`（审计），都是本机专用表，不是同步候选表，不进入 `.kanso` 导出；表结构见 `docs/schema.md` §2.8。
- 删除会话时在同一事务里物理删除它的授权。
- 审计由 `kanso-mcp-server` 在后台批量写入，不阻塞工具调用；启动时和每小时调用一次 `mcp_audit_prune`。
- 客户端令牌不进数据库，见 §16.1。

### 16.5 并发

- 每个 MCP 客户端连接是 tokio 运行时里的一个任务，所有工具调用都在运行时里执行，不占 Swift 主线程。
- 标签写锁在 `kanso-mcp-server` 内按 SessionId 维护，跨客户端共享；拿不到锁立即返回 `TAB_BUSY`，不排队。
- 后台执行用每个 transport 一个容量为 4 的信号量限流。
- 授权在每次调用开始时读取；级别变更和总开关变更由 Core 广播，正在执行的调用订阅后自行中断（`spec/mcp.md` §2.1、§4.4）。

### 16.6 测试

- `kanso-mcp-server`：用 mock core 和进程内的 rmcp 客户端（经内存双工流连接）覆盖 `spec/mcp.md` §12 中不需要真实网络的用例。
- `kanso-mcp`：用临时目录里的假 socket 和锁文件，测转发、握手，以及"App 未运行""开关关闭"两种错误回复。
- SSH 相关用例复用 §14 的进程内 russh 服务端。
- 手工验收：用 Claude Code 和 Codex 各走一遍配置与常用工具。

## 17. 待定事项

1. 断线自动重连是否放在首版。
2. Direct 版的崩溃报告方案。
3. 终端配色的导入与自定义是否作为 Pro 功能。
4. 首发之后的语言顺序（繁中和日文哪个先做）。
5. ~~是否提供从旧 GPL 版的数据迁移~~：已定，支持导入旧版的 `.kanso` 导出文件（只涉及数据格式），见 `spec/store-import.md` §6.3。
