# Design token

界面主题（chrome）的颜色、间距、圆角、字号以 `design/tokens.json` 为唯一来源（architecture.md §11）。终端配色另存于 `design/terminal-themes/`。

## 命令

```sh
cargo xtask check-tokens                 # 校验格式与对比度（已纳入 cargo xtask ci）
cargo xtask gen-tokens [--out <dir>] [--themes-out <file>]   # 生成 Xcode 颜色集、DesignTokens.swift 与 TerminalThemes.swift
```

`gen-tokens` 默认输出到 `apple/Kanso/Generated/`，该目录在 `.gitignore` 中，生成物不入库。输出是确定性的，重复运行不产生差异。

## tokens.json 格式

```
{
  "version": 1,
  "color":   { "<token>": { "light": "#RRGGBB 或 #RRGGBBAA", "dark": "...", "note": "用途说明" }, ... },
  "spacing": { "<token>": <pt 数值>, ... },
  "radius":  { "<token>": <pt 数值>, ... },
  "font":    { "<token>": <pt 数值>, ... },
  "contrast": [ { "fg": "<color token>", "bg": "<color token>", "min": 4.5 }, ... ]
}
```

- token 名：小写字母开头、用 `.` 分层，每层内用 camelCase，如 `bg.tabSelected`、`status.agentRunning`。
- 每个 color token 必须同时有 `light`、`dark` 和非空 `note`。
- spacing / radius / font 的值必须是正数，单位 pt。
- `contrast` 里的 `bg` 必须是不透明色（半透明背景没有确定的对比度，直接报错）；半透明的 `fg` 会先与 `bg` 合成再计算。
- 对比度按 WCAG 2.x 相对亮度公式（sRGB 线性化）计算。文字要求 4.5:1（`text.tertiary` 例外为 3:1，只用于弱化信息），状态色是图形元素，按非文本对比度要求 3:1。

## 终端配色

`design/terminal-themes/kanso-light.json`、`kanso-dark.json`：

```
{ "version": 1, "name": "Kanso Light", "foreground", "background", "cursor", "cursorText",
  "selectionBackground", "ansi": [16 个 #RRGGBB], "semantic": [13 个 #RRGGBB] }
```

- `semantic` 下标即帧格式里的语义类别号：0 路径、1 字符串、2 数字、3 时间/版本、4 UUID/哈希、5 网络地址/URL、6 选项/环境变量、7 键/表头、8 权限、9 成功/新增、10 错误/删除、11 警告/修改、12 文件名（暖黄色）。每个都要对本配色的 background 达到 4.5:1（terminal.md §7）。
- `foreground` 对 `background` 要求 7:1。
- ANSI 16 色不强制对比度（运行时有最低对比度保护），但不得与 background 完全相同。
- `terminal.gutterText` 显示在终端背景上，所以它对 Kanso Light / Kanso Dark 的 background（分别对应 light / dark 外观）要求 4.5:1，这一项由 `check-tokens` 在终端配色检查里完成。

## 生成物

- `apple/Packages/TerminalKit/Sources/TerminalKit/TerminalThemes.swift`：两套终端配色（`TerminalPalette.kansoLight` / `kansoDark`），
  同时烘焙进 `terminal.gutterText`、`terminal.gutterCursorLine`、`terminal.commandSeparator` 三个界面 token。
  **与上面几项不同，这个文件入库**：TerminalKit 是独立 SwiftPM 包，单独 `swift build` 时不能依赖 xtask 先生成；
  代价由 `cargo xtask check-tokens`（已在 `ci` 中）兜底——它会校验入库文件与 JSON 逐字一致，不一致时提示运行 `gen-tokens`。

- `Colors.xcassets/<名字>.colorset/Contents.json`：名字为 token 名把 `.` 换成 `_`（`bg.tabSelected` 得到 `bg_tabSelected`），含 Any 与 Dark 两个外观，sRGB 分量为 0 到 1 的浮点。
- `DesignTokens.swift`：`enum DesignTokens`，内含 `Spacing` / `Radius` / `Font`（`CGFloat` 常量）与 `Color`（颜色集名字字符串）。标识符为 token 名按 `.` 拆开后 camelCase 拼接（`tabRow.height` 得到 `tabRowHeight`）。

## 修改流程

改 `design/tokens.json` 或终端配色后运行 `cargo xtask check-tokens`，失败信息会列出具体 token、外观和实际对比度。新增 color token 必须同时给出两个外观的值。
