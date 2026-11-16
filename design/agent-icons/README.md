# AI 工具图标暂存

本目录保留图标源文件；单色版已复制到 Xcode 资源，彩色版仍仅暂存于此。
素材直接下载自 LobeHub 的公开发布包，未读取、搜索、列出或复制旧 GPL 仓库内容。SVG 保留上游原始字节，仅按新仓库的 AgentKind 标识重命名。

## 来源与许可证

- 图标库：[LobeHub Icons](https://github.com/lobehub/lobe-icons)
- 发布包：`@lobehub/icons-static-svg@1.102.0`
- 下载地址：<https://registry.npmjs.org/@lobehub/icons-static-svg/-/icons-static-svg-1.102.0.tgz>
- 对应提交：`989535d6c13da2dec5a2937fe2789d4af4fdd8e0`
- 原始许可证：[MIT](./LICENSE.lobe-icons)，取自该提交的 [LICENSE](https://raw.githubusercontent.com/lobehub/lobe-icons/989535d6c13da2dec5a2937fe2789d4af4fdd8e0/LICENSE)，后续分发时保留。
- 下载日期：2026-11-15（Asia/Shanghai）。

图标库许可证不等于各产品商标授权；正式上架前仍需按厂商规范核对使用方式和品牌标识。

## 对应关系

文件名与 `AgentKind::id()` 对齐。`copilot.svg` 取自 GitHub Copilot 的标识；Claude Code 取自 `claudecode`。

| AgentKind 标识 | 工具 | 暂存文件 | 上游文件 |
| --- | --- | --- | --- |
| `claude` | Claude Code | `claude.svg`、`claude-color.svg` | `icons/claudecode.svg` |
| `codex` | Codex | `codex.svg`、`codex-color.svg` | `icons/codex.svg` |
| `gemini` | Gemini CLI | `gemini.svg`、`gemini-color.svg` | `icons/gemini.svg` |
| `cursor-agent` | Cursor Agent | `cursor-agent.svg` | `icons/cursor.svg` |
| `copilot` | GitHub Copilot CLI | `copilot.svg` | `icons/githubcopilot.svg` |
| `amp` | Amp | `amp.svg`、`amp-color.svg` | `icons/amp.svg` |
| `opencode` | OpenCode | `opencode.svg` | `icons/opencode.svg` |
| `goose` | Goose | `goose.svg` | `icons/goose.svg` |
| `qwen` | Qwen Code | `qwen.svg`、`qwen-color.svg` | `icons/qwen.svg` |
| `openhands` | OpenHands | `openhands.svg`、`openhands-color.svg` | `icons/openhands.svg` |
| `grok` | Grok | `grok.svg` | `icons/grok.svg` |
| `aider` | Aider | 暂用现有 SF Symbol `sparkles` | 本次来源包未包含 |
| `crush` | Crush | 暂用现有 SF Symbol `sparkles` | 本次来源包未包含 |
| `droid` | Factory Droid | 暂用现有 SF Symbol `sparkles` | 本次来源包未包含 |

本次提供 11 类工具、17 个 SVG。Aider、Crush、Factory Droid 在本次来源包中未找到对应素材，继续使用规格允许的通用图标。

## 接入提示

- 无 `-color` 后缀的是单色版本，使用 `currentColor`；转换时可制作模板图片，沿用标签行的运行中、已完成状态着色。
- `-color.svg` 保留上游配色，供需要品牌配色时选择。
- SVG 使用 `viewBox="0 0 24 24"`；转换时保持原始宽高比。
- 单色 SVG 已接入 `apple/Kanso/Resources/Assets.xcassets/AgentIcons/`（模板渲染；资源内把 `currentColor` 改为实色、`1em` 改为 `24` 以便着色），标签行由 `AgentIcon` + `TabRowView` 按 `FfiAgentKind` 显示。
