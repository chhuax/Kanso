# Kanso 代码代理约定

先读 `CLAUDE.md`：其中的独立开发约束（净室、许可证、禁止参考 Zed 应用代码）同样适用于你，且优先于本文件。

## 硬性约束

- 本仓库是独立重写（GPL-3.0-or-later）。**绝对不要读取、搜索、列出或参考旧版 `chhuax/Kanso-legacy` 仓库的任何内容**，也不要凭记忆复现其实现，不得参考 Zed 应用代码。需求只来自 `docs/spec/` 和任务说明。
- 新增依赖前确认许可证是 MIT / Apache-2.0 / BSD / ISC / Zlib；禁止 GPL / LGPL / AGPL。不要改 `deny.toml`。
- 测试不得触碰用户真实环境：`~/.ssh`、钥匙串、剪贴板、`UserDefaults.standard`、真实 HOME。用临时目录和注入的替身。
- 代码注释、提交信息用中文；提交遵循 Conventional Commits。

## 写法：简单优先

本项目更怕过度设计，而不是少一层防护。

- **只做任务要求的事。** 不加没被要求的配置项、扩展点、通用化抽象、"以防万一"的分支、未来功能的占位。
- **只有一处实现就不要抽 trait / 协议 / 工厂。** 新增公开类型或模块前先找仓库里有没有现成的（Rust 看 `kanso-types`、各 crate 的 `lib.rs`；Swift UI 组件在 `apple/Kanso/App/UIKit/`）。
- **信任边界：** 只在外部输入处校验——网络数据、远端/本地文件内容、用户输入、FFI 入口的字符串。core 内部调用、Swift 调 core、同一 crate 内函数之间互相信任，不做重复校验。
- **不可能发生的情况**用 `debug_assert!` 或带原因的 `expect("…")`，不要为它层层传 `Result`、加降级路径。
- **错误处理跟随现有约定**：core 对外用 `CoreError` / `Message`，不要新造错误体系。
- **给用户看的文字不许写死。** Rust 里会显示到界面的提示、错误、状态一律用 `Message` 变体 + `locales/en.ftl`、`locales/zh-Hans.ftl`（中英都补），不要在 core 里拼中文字符串；Swift 侧用 `String(localized:)` + `Localizable.xcstrings`。只有 `expect` 原因和日志可以直接写。
- 不加超时、重试、限流、加密、权限检查等安全或可靠性机制，除非规格或任务说明明确要求。认为确有必要时写进报告，不要自行加上。
- 不改与任务无关的代码，不顺手重构、改名、调格式。
- **改动预算：** 任务说明给了行数预算时遵守；超过时先停下，在报告里说明原因。

## 验证

- 改动后跑相关的测试：`cargo test -p <crate>`，Swift 用 `cargo xtask apple-test --only-testing KansoTests/<Suite>`。
- 交付前跑一次完整 `cargo xtask ci`（只改 Rust 时可加 `--skip-apple`，并在报告里注明）。
- 改了 `kanso-ffi` 导出接口后运行 `cargo xtask bindgen` 并提交生成的绑定。
- 先用 `rg` 定位再按行范围读取；不要在输出里回显整份源码或构建日志。

## 报告

完成后只回短报告（20 行以内）：

1. 改动：文件 / 模块级别，一句话说明每处做了什么。
2. 验证：跑了哪些命令、结果。
3. 风险与遗留：与规格的出入、没做的部分、你认为需要但没加的东西。
4. 提交位置：分支名与 commit hash（不能提交时说明原因）。
