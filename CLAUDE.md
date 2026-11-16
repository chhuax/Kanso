# Kanso（GPL-3.0-or-later）

设计与已定决策见 `docs/design.md`，功能规格见 `docs/spec/`。

## 独立开发约束（硬性，不得违反）

- 本仓库是独立重写（GPL-3.0-or-later），版权归 chhuax，与旧版 `chhuax/Kanso-legacy`（基于 EdgeTerm 的 Tauri 版）**没有代码继承关系**。
- 在本仓库工作时，**不得读取、搜索、引用旧 GPL 仓库的任何源码**，也不得根据记忆复现其实现。
- 需求只来自 `docs/spec/` 的纯文字规格和一手资料（RFC、OpenSSH 文档、依赖库文档、Apple 文档）。规格不清楚时向用户提问，不要去翻旧代码。
- 新增依赖前确认许可证属于 MIT / Apache-2.0 / BSD / ISC / Zlib 等宽松协议；禁止 GPL / LGPL / AGPL。
- 不得参考 Zed 编辑器的应用代码（GPL），只能使用 `gpui` 库本身（Apache-2.0）。

## 常用命令

```sh
cargo xtask ci                 # 本地 CI：fmt、clippy、test、渠道矩阵、cargo deny
cargo xtask notices            # 重新生成第三方依赖与素材声明
cargo xtask dmg                # Release 构建 Direct 并生成 arm64 DMG 与 SHA-256；Intel 包另加 --arch x86_64
cargo xtask ci --skip-deny     # 本地没装 cargo-deny 时跳过许可证检查
cargo xtask ci --skip-apple    # 没装 Xcode 时跳过 apple-build（ci 默认会构建三个渠道的 Debug）
cargo xtask bindgen            # 重新生成 UniFFI Swift 绑定（改了 kanso-ffi 导出接口后必须运行并提交）
cargo xtask check-bindings     # 校验入库的 Swift 绑定未过期（ci 已包含）
cargo xtask xcode              # 生成 token 产物与 apple/Kanso.xcodeproj（需要 xcodegen）
cargo xtask apple-build [--channel direct|mas|setapp | --all] [--configuration debug|release] [--arch arm64|x86_64]（默认本机架构）
cargo build -p kanso-ffi       # 默认 direct 渠道
cargo build -p kanso-ffi --no-default-features --features mas      # MAS 渠道
cargo build -p kanso-ffi --no-default-features --features setapp   # Setapp 渠道
```
