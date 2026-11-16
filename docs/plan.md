# Kanso 执行计划（可并行）

> 依据：`docs/design.md`（产品）、`docs/architecture.md`（模块与契约）、`docs/spec/`（功能规格）。
> 规模：S ≤ 3 天，M ≈ 1–2 周，L ≈ 2–4 周（按一个全职开发者或一个 agent 估算）。

## 0. 执行方式

- **每个工作包（WP）**：在独立的 git worktree 里开分支 `wp/<编号>-<短名>`，由一个独立的 agent 会话（或一个人）完成，最后提 PR 到 `main`。
- **主会话（架构负责人）**：负责第 2 阶段的契约、评审每个 PR、合并，以及处理跨 WP 的接口变更。
- **WP 会话的输入只有三样**：`docs/spec/` 里对应的规格、`architecture.md`，以及已经合并的契约代码。**不读旧 GPL 仓库**，这条约束已写在 `CLAUDE.md` 里，会自动生效。
- **改契约必须走主会话**：WP 内部发现契约不够用时，先提一个只改契约的 PR，合并后再继续，不能在自己的 WP 里顺手改。
- **完成标准**：本 WP 的验收测试全部通过；CI 通过（fmt、clippy、test、cargo-deny、多语言 key 检查）；新增的公开 API 有中文文档注释。

## 1. 阶段 S：规格提炼（在旧仓库会话中进行，可以并行）

| 编号 | 规格文件 | 范围 | 供哪些 WP 使用 | 状态 |
|---|---|---|---|---|
| S1 | `ssh.md` | 连接、算法、主机密钥、认证、跳板、SFTP、传输、远端 cwd | T6 T7 I2 | ✅ 已完成 |
| S2 | `terminal.md` | 终端行为：时间戳栏、命令分隔线、提示行、选区、复制粘贴、按键、清屏、搜索、OSC、链接、活动检测与 AI CLI 识别 | T4 T9 F5 | ✅ 已完成 |
| S3 | `local-shell.md` | 本地 PTY、login shell、ZDOTDIR shim、locale、环境变量（含无彩色变量的处理）、前台进程 cwd | T5 I1 | ✅ 已完成 |
| S4 | `encoding-transfer.md` | 编码转换、ZMODEM、XMODEM | T1 T2 F4 | ✅ 已完成 |
| S5 | `store-import.md` | 会话、分组、设置、凭据、`.kanso` 导入导出、OpenSSH 配置导入 | T3 T8 F6 | ✅ 已完成 |
| S6 | `ui-shell.md` | 竖向标签栏（沿用现有风格，参考截图）、分屏、菜单、快捷键、会话启动器与对话框、显示设置、主题 | T10 F7 | ✅ 已完成 |
| S7 | `filer.md` | 本地与远端文件浏览、拖放、远程编辑、Git 改动面板 | F1 | ✅ 已完成 |
| S8 | `sender.md` | 发送面板、作用域、多行逐条发送、定时重复 | F2 | 待做 |
| S9 | `completion.md` | 命令建议、历史、路径补全、git/kubectl 补全、按键交互 | F3 | 待做 |

S2 到 S9 互相独立，可以同时进行。**优先做 S2、S3、S6**，因为它们分别卡着终端内核、本地 shell 和 UI 外壳这三条关键路径。

## 2. 阶段 P0：契约（串行，由主会话完成，约 1 周）

| 编号 | 内容 | 产出 | 依赖 |
|---|---|---|---|
| P0.1 | workspace 骨架和 CI | 各 crate 的空壳；CI 跑 fmt、clippy、test、cargo-deny；许可证白名单（含 uniffi 的 MPL 例外） | — |
| P0.2 | `kanso-types` | 各类 id、消息 key 枚举及参数、事件枚举 | P0.1 |
| P0.3 | 帧快照 ABI | C 头文件、格式说明，以及一个能生成测试帧的 mock 生产者 | P0.2 |
| P0.4 | `kanso-core` 公开 API 和 mock core | API 只有签名和文档；mock core 返回假会话、假输出、假 SFTP 目录 | P0.2 |
| P0.5 | 平台 trait | SecretStore、FileAccess、Purchase、HostKeyStore、Notifier（需要用户回答的提问不走 trait，统一用 `CoreEvent::Prompt` + 应答接口，见 `architecture.md` §7） | P0.2 |
| P0.6 | 数据库 schema v1 | 迁移脚本、表结构说明 | P0.2 |
| P0.7 | design token 和多语言约定 | `tokens.json` 的格式与第一版 token；`locales/` 骨架；key 命名规则；`xtask` 里的生成与校验命令 | P0.1 |
| P0.8 | Xcode 工程骨架 | 两个 scheme；能构建 XCFramework；应用能启动，并在窗口里显示 mock 帧 | P0.3、P0.4 |

P0.2 完成后，P0.3 到 P0.7 可以交给 2–3 个会话并行做；P0.8 可以和 P0.6、P0.7 同时进行。

## 3. 阶段 P1：基础模块（并行，对应 M1）

P0 完成后，下面这些工作包**可以同时开工**，最多 13 条线，实际人手按优先级取用。

| 编号 | 工作包 | 规模 | 依赖 | 验收要点 |
|---|---|---|---|---|
| **T4** | `kanso-term`：VT 封装、行元数据、按键编码、选区、搜索、帧快照 | L | S2、P0.3 | 通过 vttest/esctest 子集；网格快照测试；`cat` 1 GB 的解析吞吐达到目标 |
| **T9** | TerminalKit：Metal 渲染、字形图集、`NSTextInputClient`、鼠标 | L | P0.3（使用 mock 帧） | 120 Hz 不掉帧；中文、日文 IME 测试表全部通过；截图比对浅色和深色各一套 |
| **T6** | `kanso-ssh` 连接层：算法、主机密钥、认证、跳板、共享 transport | L | S1 ✅、P0.5 | `ssh.md` §12 的验收用例全部转成测试并通过 |
| T7 | `kanso-ssh` SFTP 与流水线传输引擎 | M | T6 的 transport 接口（可以先对接口开发） | 千兆局域网单文件 ≥ 80 MB/s；取消后不留残缺文件；内存占用有上限 |
| T5 | `kanso-pty` | M | S3 | 能启动 login shell；调整大小；查到前台进程的 cwd；shim 能加载用户原有的 rc 文件 |
| T1 | `kanso-encoding` | S | S4 | 各编码的往返测试；多字节字符被截断在两块之间时能正确拼接 |
| T2 | `kanso-xfer`（ZMODEM/XMODEM 状态机） | M | S4 | 与 `lrzsz` 实际互传测试；取消流程 |
| T3 | `kanso-sshconfig` | M | S5 | Host 匹配、Include、`%` 替换、ProxyJump 的测试矩阵 |
| T8 | `kanso-store` 和 `kanso-secret` | M | S5、P0.5、P0.6 | 迁移测试；凭据只存在钥匙串里；导入导出往返一致 |
| **T10** | KansoUI 外壳：窗口、竖向标签栏、分屏、主题 token、多语言基础设施、Settings | L | S6、P0.4（使用 mock core）、P0.7 | 浅色、深色、伪本地化三种模式下的截图测试；标签拖拽；系统外观切换时实时生效 |
| T11 | PlatformMac 与 `kanso-license`：Keychain、License Key、Sparkle（StoreKit 和 Setapp SDK 放到 M5） | M | P0.5 | License Key 能离线校验、激活和停用；Sparkle 能从测试 appcast 完成更新 |
| T12 | 发布流水线：Developer ID 签名、公证、DMG 打包、Sparkle appcast；CI 同时编译 `mas`、`setapp` 两种配置 | M | P0.8 | mock 版完成公证，在一台干净的 Mac 上打开时不弹安全警告 |
| T13 | `kanso-bench`，以及旧版的性能基线 | S | P0.1 | 产出旧版的基线报告（启动时间、内存、吞吐、延迟） |

**关键路径**：P0 → T4 + T9 → I1。T4 和 T9 是最长的两条线，需要投入最强的执行者，并且最先开工。

## 4. 阶段 P2：集成（对应 M2 MVP）

| 编号 | 内容 | 依赖 | 能否并行 |
|---|---|---|---|
| I3 | `kanso-ffi` 的真实实现，替换掉 mock | P0.4、T4 | 最先做 |
| I1 | `kanso-session`：本地会话走通全链路（PTY → 编码 → term → 帧 → 渲染） | T1、T4、T5、T9、T10、I3 | 关键路径 |
| I2 | SSH 会话，以及主机密钥变更、MFA 两个对话框 | T6、I1 | I1 完成后进行 |
| I5 | 购买全流程（官网下单 → License Key → 激活）；只有一个付费版本，不做试用和免费版/Pro（2026-11-16 决定） | T11、T10 | 与 I2 并行 |

完成 I1、I2、I3、I5 就是 **MVP**：一个公证过的官网版，具备完整的本地终端、SSH、标签、分屏、深浅色、中英文、License Key 购买。

## 5. 阶段 P3：功能补齐（对应 M3，全部可以并行）

| 编号 | 内容 | 依赖 |
|---|---|---|
| F1 | Filer：本地与 SFTP 浏览、拖入拖出、file promise、远程编辑 | S7、T7、I2 |
| F2 | Sender 及定时发送 | S8、I1 |
| F3 | 补全引擎和补全弹窗 | S9、I1 |
| F4 | ZMODEM/XMODEM 接入会话，以及对应的 UI | T2、I1 |
| F5 | 时间戳栏、命令分隔线、提示行 chips、活动与 agent 角标 | S2、I1 |
| F6 | OpenSSH 配置导入 UI、`.kanso` 导入导出 | T3、T8 |
| F7 | 设置：显示、快捷键、外观模式、终端配色、最低对比度 | T10 |
| F8 | 本地化全面检查（简中和英文全覆盖）、两种外观下的截图、App Store 素材 | 以上全部 |

## 6. 阶段 P4：发布（对应 M4）

- 性能调优：逐项对照 `design.md` 里的性能目标，与 T13 的旧版基线比较。
- 官网版正式发布：公证、收款渠道（Paddle 或 Lemon Squeezy）、官网。

## 7. 阶段 P5：其他渠道（对应 M5）

| 编号 | 内容 |
|---|---|
| C1 | 商店版：沙盒适配；实测本地 shell 和 ssh-agent（`design.md` §5 中标 ❓ 的项目）；首次启动的主目录授权引导；StoreKit 2；隐私清单；TestFlight 和提交审核 |
| C2 | Setapp 版：官网版满 3 个月后申请；接入 Setapp SDK；确认是否接受只有 arm64 的包 |
- 发布前用 JPlag 或 Dolos 对新旧代码做相似度自查。

## 8. 并行度一览

```
周次   1    2    3    4    5    6    7    8    9   10   11   12
S    [S2 S3 S6][S4 S5][S7 S8 S9]
P0   [P0.1-P0.8]
T4        [=========== T4 term ===========]
T9        [========= T9 TerminalKit =======]
T6        [====== T6 ssh ======][= T7 sftp =]
T10       [========== T10 UI 外壳 ==========]
T1-3,5,8       [各 1–2 周，按规格完成情况陆续开工]
T11-13    [=====][=====]
I                                  [I3][== I1 ==][I2 / I5]
F                                                   [F1-F8 并行 →]
```

（周次只用来表示先后顺序和并行关系，实际工期以 T13 基线和 M1 spike 的结论为准，到时再校准。）
