# macOS 构建（Xcode 工程）

P0.8 交付的工程骨架：应用能用三种发行版配置构建、启动；I1 之后默认显示真实本地 shell，`--mock-core` 显示 Rust 生成的 mock 终端帧。
总体结构见 `architecture.md` §1、§3、§9。

## 发行入口

在 macOS 执行 `cargo xtask dmg`，Intel 包用 `cargo xtask dmg --arch x86_64`。脚本让 Xcode 构建 `Release-Direct/Kanso.app`，AppKit 主程序链接 Rust `kanso-ffi`，Direct 渠道包含 `kanso-mcp` 转发程序。DMG 只复制这个 App Bundle、Applications 链接和 macOS 许可文本，使用 UDZO 的 zlib 9 压缩。Windows GPUI 由独立的 [Windows 发行命令](windows-release.md) 构建。

## 1. 目录结构

```
apple/
  project.yml                 # XcodeGen 工程描述（入库，改工程设置只改这里）
  Kanso.xcodeproj/            # 由 `cargo xtask xcode` 生成，不入库
  Kanso/                      # App target
    App/                      # main.swift、AppDelegate、MainWindowController、截图模式
    Resources/Localizable.xcstrings
    Entitlements/             # Kanso-Direct / Kanso-MAS / Kanso-Setapp .entitlements
    Generated/                # gen-tokens 输出（Colors.xcassets、DesignTokens.swift），不入库
    Info.plist                # 由 XcodeGen 按 project.yml 的 info 生成，不入库
  Tests/
    KansoTests/               # XCTest：截图测试、渲染器行为测试、性能测试（由 App 托管）
    Snapshots/                # 截图测试的参考 PNG
  Packages/                   # 四个本地 SwiftPM 包（均为 library，macOS 14+）
    KansoBridge/              # FFI 边界：FrameSnapshot、FrameColor（手写 C ABI 封装）+ UniFFI 生成的 Swift 绑定
                              # （Sources/KansoBridge/Generated，入库）+ 生成的 C 头文件目标 KansoFFIC
    TerminalKit/              # TerminalView / TerminalRenderer（Metal）、GlyphAtlas、FrameSource、TerminalPalette
                              # （TerminalThemes.swift 由 gen-tokens 生成并入库）
    KansoUI/                  # CoreEventDispatcher / CoreFrameSource / CoreInputSink（core 与 TerminalKit 的胶水）
    PlatformMac/              # 空壳
```

## 2. 渠道与配置

| 渠道 | scheme | Debug 配置 | Release 配置 | `KANSO_CHANNEL` | Swift 条件 | entitlements |
|---|---|---|---|---|---|---|
| Direct | `Kanso-Direct`（主力） | `Debug-Direct` | `Release-Direct` | `direct` | 无 | 空字典 |
| MAS | `Kanso-MAS` | `Debug-MAS` | `Release-MAS` | `mas` | `MAS` | 沙盒 + 网络 + 用户选择文件 + app-scope bookmark |
| Setapp | `Kanso-Setapp` | `Debug-Setapp` | `Release-Setapp` | `setapp` | `SETAPP` | 空字典 |

- Debug 配置另有 `DEBUG` 条件；`KANSO_RUST_PROFILE` 随配置为 `debug` / `release`。
- 每个 scheme 的 Run / Test / Profile / Analyze / Archive 分别用对应渠道的 Debug、Release、Release、Debug、Release 配置。
- 通用设置：macOS 14.0、架构按配置设置（Debug 用本机架构，Release 默认 arm64；官网版 Intel 包用命令行 `ARCHS=x86_64` 覆盖 Release-Direct，MAS / Setapp 只出 arm64，不做 Universal）、Hardened Runtime、Swift 6 语言模式（严格并发）、本地签名 `-`（Sign to Run Locally）。
- Swift 编译条件只作用于 App target；本地包目前不区分渠道，需要时再单独设计。

## 3. Rust 静态库如何进入 Xcode 构建

App target 的第一个构建阶段是 Run Script `Build Rust core`，每次构建都执行（不做依赖分析，增量交给 cargo）：

```
cargo xtask apple-lib --channel "$KANSO_CHANNEL" --profile "$KANSO_RUST_PROFILE" --arch "$ARCHS" --out "$TARGET_TEMP_DIR/rust"
```

`apple-lib` 以 `--no-default-features --features <渠道>` 为 `--arch` 指定的架构（`arm64` → `aarch64-apple-darwin`，`x86_64` → `x86_64-apple-darwin`；`ARCHS` 含多个架构时报错）构建 `kanso-ffi`，
每个渠道用独立的 target 目录 `target/apple/<渠道>`，切换 scheme 不会互相冲掉 cargo 缓存；
再把 `libkanso_ffi.a` 复制到 `$TARGET_TEMP_DIR/rust`（内容不变时不覆盖，避免无谓的重新链接）。
App target 通过 `LIBRARY_SEARCH_PATHS = $(TARGET_TEMP_DIR)/rust` 与 `OTHER_LDFLAGS = -lkanso_ffi` 链接。

**为什么不用 SwiftPM 的 `binaryTarget`**：它的路径写在 `Package.swift` 里，是静态的，无法随渠道切换；
而三个渠道的 Rust feature 不同、产物不同。因此 SwiftPM 包只提供头文件与 Swift 封装，符号在 App 最终链接时解析。

该 target 设置了 `ENABLE_USER_SCRIPT_SANDBOXING = NO`：脚本需要读整个仓库、写 `target/` 并调用 `~/.cargo/bin` 下的 cargo，
Xcode 的脚本沙盒会全部拒绝。脚本里手动补 `PATH`，因为 Xcode 的构建环境不含用户 shell 的 PATH。

`cargo xtask xcframework --channel <c>` 会 release 构建并打包 `target/apple/<c>/KansoCore.xcframework`，
**仅供发布流水线（T12）使用**，Xcode 日常构建不走它。

## 4. 头文件符号链接

`apple/Packages/KansoBridge/Sources/CKansoFFI/include/kanso_frame.h` 是指向
`crates/kanso-ffi/include/kanso_frame.h` 的相对符号链接，保证 C ABI 头文件只有一份来源。
不要把它替换成拷贝。`shim.c` 是空文件，仅因 SwiftPM 的 C 目标至少需要一个源文件。

## 4.1 UniFFI 绑定

`kanso-ffi` 的控制面用 UniFFI 导出，Swift 绑定由 `cargo xtask bindgen`（library mode，读已编译的 `libkanso_ffi.a`）生成，
**入库**（与 `TerminalThemes.swift` 同一策略：让包可以被单独的 `swift build` 编译）：

| 生成物 | 位置 |
|---|---|
| `KansoFFI.swift` | `Packages/KansoBridge/Sources/KansoBridge/Generated/` |
| `KansoFFIC.h`、`module.modulemap` | `Packages/KansoBridge/Sources/KansoFFIC/include/`（C 目标，`shim.c` 为空占位） |

- 改了 `kanso-ffi` 的导出接口（含文档注释，它们会进入 Swift 注释）后必须重新运行 `cargo xtask bindgen` 并提交；
  `cargo xtask check-bindings`（已加入 `ci`）重新生成到临时目录并逐字节比较，不一致即失败。
- 生成配置在 `crates/kanso-ffi/uniffi.toml`（文件名、C 模块名、值类型 `Sendable`）。生成的代码在 Swift 6 严格并发下
  编译无告警，所以没有为生成代码降级语言模式；以后若升级 UniFFI 后出现并发告警，只对生成代码所在目标设置
  `swiftLanguageMode(.v5)`，不要关掉其他目标的检查。
- 取帧走 UniFFI 但帧内容零拷贝，生命周期约定见 `frame-abi.md` §8。
- 手写 C 头文件（`CKansoFFI`，mock 帧函数）与 UniFFI 的 C 头文件（`KansoFFIC`）是两个独立的 C 模块，互不影响。

## 5. 常用命令

```sh
cargo xtask xcode                                  # 生成 token 产物与 Kanso.xcodeproj
cargo xtask bindgen                                # 重新生成 UniFFI Swift 绑定（改了 kanso-ffi 接口后必须运行）
cargo xtask check-bindings                         # 校验入库的绑定没有过期
cargo xtask apple-build                            # 构建 Direct / Debug
cargo xtask apple-build --channel mas --configuration release
cargo xtask apple-build --all                      # 依次构建三个渠道的 Debug
cargo xtask dmg                                   # Release Direct 构建、生成 arm64 DMG 并打印 SHA-256（临时签名、未公证）
cargo xtask dmg --arch x86_64                     # 可选：另出 Intel 包 Kanso-<版本>-macOS-x86_64.dmg
cargo xtask apple-build --arch x86_64             # 指定架构（默认本机架构；x86_64 的 Release 只支持 direct）
cargo xtask notices                               # 更新第三方依赖与素材声明
cargo xtask apple-test                             # Direct scheme 下跑 xcodebuild test（KansoTests）
KANSO_RECORD_SNAPSHOTS=1 cargo xtask apple-test   # 重新录制参考截图
cargo xtask ci --skip-apple                        # 没装 Xcode 时跳过 Apple 端构建
```

产物在 `target/apple/DerivedData.noindex/Build/Products/<配置>/Kanso.app`（x86_64 在 `target/apple/DerivedData-x86_64.noindex/` 下，与 arm64 互不覆盖）。目录名带 `.noindex` 是为了让聚焦（Spotlight）不收录这些构建产物，否则搜索 Kanso 会出现多个同名 App，容易打开到旧的 Debug 版。
需要 Xcode 与 XcodeGen（`brew install xcodegen`）。

## 6. 截图模式

不显示窗口，用 `TerminalRenderer.renderImage` 把一帧 mock 帧（seq 固定为 0，900x600 pt，2x）离屏渲染成 PNG 后退出，不碰真实屏幕：

```sh
target/apple/DerivedData.noindex/Build/Products/Debug-Direct/Kanso.app/Contents/MacOS/Kanso \
  --snapshot /tmp/kanso-dark.png --appearance dark     # appearance 可选 light | dark
```

成功退出码 0，失败（无 Metal 设备、无法创建帧、编码或写文件失败）退出码 1。

## 6.1 测试

`KansoTests`（`apple/Tests/KansoTests`）由 App 托管，能链接已有的 Rust 静态库。三个 scheme 的 test action 都包含它。
`cargo xtask apple-test` 在 Direct scheme 下运行，已加入 `cargo xtask ci`（`--skip-apple` 一并跳过）。

- **截图测试**：mock 帧在浅色、深色两种外观下离屏渲染，与 `apple/Tests/Snapshots/*.png` 逐像素比较（每通道差 <= 2，
  不同像素占比 < 0.1%）。时间戳栏固定 UTC。`KANSO_RECORD_SNAPSHOTS=1` 重录。参考图依赖系统字体，
  macOS 大版本升级后若出现整体小差异，重录即可。
- **性能测试**：200x60 满屏彩色文字连续渲染 300 帧，报告每帧 CPU 编码耗时的平均值与 P99，同时写入 `/tmp/kanso-t9a-perf.txt`。
  Debug 配置没有优化，数字只作参考；断言只在优化构建中生效。
- 只提供 Debug 配置：Release 的 Hardened Runtime 会拒绝加载自签名的测试包。

## 6.2 渲染器

`TerminalView`（`CAMetalLayer`，按 display link 取帧，空闲 / 不可见时暂停）与 `TerminalRenderer`（与 NSView 解耦，
可离屏渲染）的设计要点写在源码注释里：颜色空间选择、像素对齐、三层绘制顺序、图集策略。
着色器源码内嵌在 Swift 字符串中运行时编译（`swift build` 不编译 `.metal`），耗时记录在 `shaderCompileSeconds`。

## 7. 已知占位项

Bundle ID 已定：Direct 与 MAS 为 `com.chhuax.kanso`，Setapp 为 `com.chhuax.kanso-setapp`（Setapp 要求带 `-setapp` 后缀）。它决定钥匙串条目、数据目录和日志 subsystem，发布后不能再改。


- **签名团队**：不设 team，使用 Sign to Run Locally；Developer ID / MAS 签名由发布流水线（T12）处理。
- **帧来源**：App 的主窗口通过 `CoreFrameSource` 经 FFI 向 core 取帧。默认使用**真实 core**（`KansoCore(config:listener:)`，
  I1：本地 shell 全链路），数据目录是 `~/Library/Application Support/<bundle id>/`（目录不存在由 core 创建）；
  启动参数 `--mock-core` 改用 mock 后端（`KansoCore.newMock`：纯内存，按键只让帧序号加一，界面开发用）。
  会话的初始网格取自 `TerminalView` 按窗口算出的尺寸；shell 自己退出时窗口标题追加"会话已结束"，视图保留最后的画面
  （不自动关窗，关闭确认等是 T10）。应用退出时调用 `KansoCore.shutdown`（杀 shell、等读写线程，最多约 3 秒）。
  `MockFrameSource`（TerminalKit）仍保留给渲染器测试。
- **输入**：键盘、IME、鼠标、滚轮由 `TerminalView` 转成 `TerminalInputSink` 事件（T9b，类型与 `kanso-types/src/input.rs` 一一对应）；App 通过 `CoreInputSink` 映射到 `KansoCore.key / textInput / mouse / scroll / focus`（Debug 构建中间多一层 `LoggingInputSink`：先把事件写入 `os_log` 再转发给 core，供 IME 人工验收观察；Release 不经过它）。选区与菜单由 T4c / T10 实现。人工验收见 `docs/ime-test-checklist.md`。

## 自动发布

推送 `v*` 标签后，`.github/workflows/release.yml` 会在 `macos-15` 上构建 arm64 与 x86_64 两个 DMG，连同 `.sha256` 一起上传到同名 Release 草稿。Windows 安装包也会传到同一个 Release 草稿。详见 [Windows 发行包](windows-release.md#自动发布)。
