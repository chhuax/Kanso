# 输入编码（kanso-term::input）

把 UI 交来的键盘、鼠标、滚轮、粘贴事件翻译成发往 PTY / SSH 通道的字节。sans-IO 纯函数，入口都在 `kanso_term` 根上 re-export。对应 `spec/terminal.md` §8（选区、复制与粘贴）、§9（按键）、§5.2（终端回应不算输入）。

一手出处：xterm「XTerm Control Sequences」（下称 ctlseqs）、kitty keyboard protocol 规范文档、ECMA-48。

## 1. API

| 函数 | 说明 |
|---|---|
| `encode_key(&KeyEvent, &TermModes, &KeyOptions) -> Option<Vec<u8>>` | `None` = 不发送 |
| `encode_mouse(&MouseEvent, &TermModes) -> Option<Vec<u8>>` | 没开上报、模式不报这类事件、按住 Shift、坐标无法表示时 `None` |
| `encode_scroll(lines, at, mods, &TermModes) -> ScrollAction` | `Report` / `ArrowKeys` / `ScrollViewport` |
| `encode_paste(&str, &TermModes) -> Vec<u8>` | |
| `quote_paths(&[String], ShellFlavor) -> String` | 文件粘贴的转义 |

`KeyOptions { platform: Mac | Other, option_as_meta: Off | Left | Right | Both }`。`TermModes` 取自 `Terminal::modes()`，必须是发送那一刻的值。

这里编码出的字节才算「用户输入」；终端自己的回应走 `take_replies`（terminal.md §5.2）。

## 2. 传统（未开 kitty）按键编码

修饰参数 `m = 1 + Shift(1) + Alt(2) + Ctrl(4)`（ctlseqs「PC-Style Function Keys」）。⌘ 为真时整个事件返回 `None`（terminal.md §9）。

| 键 | 无修饰 | 带修饰 / 说明 |
|---|---|---|
| Enter | `\r` | Alt 加 `ESC` 前缀；Shift/Ctrl 与无修饰相同 |
| Tab | `\t` | Shift+Tab `CSI Z`（ECMA-48 CBT）；Alt 加 `ESC` 前缀 |
| Backspace | `0x7f` | Ctrl `0x08`；Alt 加 `ESC` 前缀。发 DEL：现代 xterm 与 macOS Terminal 默认如此，`stty erase` 也是 `^?` |
| Escape | `ESC` | Alt+Esc = `ESC ESC` |
| Space | ` ` | Ctrl（含 Shift）NUL；Alt 加 `ESC` |
| ↑↓→← / Home / End | `CSI A/B/C/D/H/F`；DECCKM 下 `SS3 A/B/C/D/H/F` | `CSI 1;m X`，与 DECCKM 无关 |
| PageUp/PageDown/Insert/Delete | `CSI 5~ 6~ 2~ 3~` | `CSI n;m~` |
| F1–F4 | `SS3 P/Q/R/S`（与 DECCKM 无关） | `CSI 1;m P/Q/R/S` |
| F5–F12 | `CSI 15~ 17~ 18~ 19~ 20~ 21~ 23~ 24~` | `CSI n;m~` |
| F13–F20 | `CSI 25~ 26~ 28~ 29~ 31~ 32~ 33~ 34~` | `CSI n;m~`。编号跳过 27、30 是 VT220 历史，不能补齐 |
| F21–F24 | 不发送 | ctlseqs 没有它们的 `~` 编码 |

### Ctrl 控制字符

- `a`–`z`（大小写、有无 Shift 都一样）→ `0x01`–`0x1a`。
- `@` `2` `Space` → `0x00`；`[` `3` → `0x1b`；`\` `4` → `0x1c`；`]` `5` → `0x1d`；`^` `6` → `0x1e`；`_` `7` `/` → `0x1f`；`?` `8` → `0x7f`。
- Shift 后的符号按 US 布局还原：Ctrl+Shift+2 → `0x00`，Ctrl+Shift+6 → `0x1e`，Ctrl+Shift+`-` → `0x1f`，Ctrl+Shift+`/` → `0x7f`。`{ | }` 没有控制字符。
- 其它（Ctrl+0/1/9 等）Ctrl 被忽略，发字符本身（xterm 行为）。
- Ctrl+Alt+字符 = `ESC` + 控制字符。

### Alt / Option

- 非 mac：Alt+可打印键 = `ESC` + 该字符（有系统文本时用系统文本，因此 Alt+Shift+1 得 `ESC !`）。
- mac：只有被 `OptionAsMeta` 选中的那一侧 Option（左/右/两侧）才发 `ESC` + **不带 Option 的字符**（用逻辑键，不用 `event.text`，因为它是 å 之类的组合字符）；否则发 `event.text`，不带 `ESC`。死键（无文本）不发送，避免先漏出一个裸字母。已知限制：mac 上 Option(Meta)+Shift+符号键无法还原布局上的符号，只还原字母大写。
- 非字符键（方向键、Enter、Backspace、Tab、Esc）上的 Option/Alt 不受 `OptionAsMeta` 影响，始终按 Alt 修饰处理（所以 Option+Backspace 永远是 `ESC DEL`，readline 的删除单词）。
- **单词跳转（terminal.md §9）**：纯 Alt（无 Shift/Ctrl）+←/→：mac 发 `ESC b` / `ESC f`；其他平台发 `CSI 1;5D` / `CSI 1;5C`，Alt+↑/↓ 也映射成 Ctrl+↑/↓。原因：zsh/bash 默认没有绑定纯 Alt 修饰的方向键序列。mac 上 Option+↑/↓ 规格没要求转换，保持 `CSI 1;3A/B`。只在传统模式转换：程序启用 kitty 协议后应收到真实修饰（`CSI 1;3D`）。

### 事件类型

传统模式下 Release 不发送；Repeat 与 Press 相同。有系统文本且无 Ctrl/Alt 的字符键直接发 `text`；没有文本时退回逻辑键（Shift 下字母大写）。

## 3. kitty keyboard protocol

状态（`CSI > u` push、`CSI < u` pop、`CSI = u` set、`CSI ? u` 查询及回应）由 alacritty_terminal 0.25.1 处理，`Config::kitty_keyboard = true`；`TermModes::kitty_keyboard` 是当前标志位（`kitty::*`）。`CSI ? u` 的回应进 `take_replies`。有测试覆盖。

编码形态只由标志 1（DISAMBIGUATE）或 8（REPORT_ALL_KEYS）决定；单独的 2/4/16 按传统模式处理（它们只作用于已经用转义码发送的键）。

格式：`CSI code[:shifted] [; mods[:event]] [; text] u`，`mods = 1 + Shift + 2·Alt + 4·Ctrl`，事件 1 按下 / 2 重复 / 3 松开。`code` 恒为不带 Shift 的小写码点（Ctrl+Shift+A 是 `CSI 97;6u`）。mods 与事件都是默认值且无文本时整段省略。

| 标志 | 实现 |
|---|---|
| 1 | Esc → `CSI 27u`；Ctrl/Alt（Meta）修饰的字符键 → `CSI code;m u`；无 Ctrl/Alt 的字符键仍发文本；Enter/Tab/Backspace **无修饰保持传统字节，有修饰** → `CSI 13/9/127;m u`（Shift+Enter = `CSI 13;2u`，Claude Code / Codex 依此区分换行与提交） |
| 2 | Repeat/Release 带 `:2` / `:3`。文本键、无修饰的 Enter/Tab/Backspace 没有 Release（规范）；没有标志 2 时 Release 一律不发，Repeat 同 Press |
| 4 | 只实现 shifted key：Shift 下附带 `code:shifted`（取单字符系统文本，或 ASCII 字母大写）。base-layout key 不实现（`KeyEvent` 没有该信息） |
| 8 | 所有键（含普通文本、Enter/Tab/Backspace）都发 `CSI ... u`；方向键等仍用下表的传统形式 |
| 16 | 配合 8：文本键附带 `;;文本码点`（冒号分隔，滤掉控制字符；Ctrl/Alt 组合与 Release 不带文本） |

功能键（无修饰且为按下事件时与传统编码相同，包括 DECCKM）：

| 键 | 带修饰 / 事件类型 |
|---|---|
| 方向键、Home/End | `CSI 1;m[:e] X` |
| F1/F2/F4 | `CSI 1;m[:e] P/Q/S` |
| F3 | `CSI 13;m[:e] ~`（`CSI 1;m R` 会和光标位置报告冲突，规范已废弃） |
| Insert/Delete/PageUp/PageDown/F5–F12 | `CSI n;m[:e] ~` |
| F13–F35 | `CSI 57376..57398;m[:e] u`（专用码点，无传统形式） |

### 已知限制

- `kanso_types::Key` 没有修饰键本身和小键盘变体：标志 8 下单按 Shift 等的上报、小键盘专用码点（57399..）无法产生，需要时走契约 PR。
- `KeyEvent` 没有 Caps Lock / Num Lock 状态与基础布局键：修饰参数无锁定位，标志 4 无 base-layout key。
- 不上报 super/hyper/meta 修饰：⌘ 不会发给 core。

## 4. 鼠标

模式（`TermModes::mouse`）：1000 Click 报按下/松开；1002 Drag 另报按住拖动；1003 Motion 报所有移动。编码优先级 SGR(1006) > UTF-8(1005) > 默认。

按钮码：左 0 / 中 1 / 右 2 / 无按键 3；Alt +8、Ctrl +16；拖动与移动 +32；滚轮上 64 / 下 65。坐标 1 起。

- SGR：`CSI < b;x;y M`（松开为 `m`，保留原按钮码）。
- 默认：`CSI M` + 三个「值 + 32」的字节；松开的按钮位为 3；任一值超过 255（坐标 > 223）无法表示，**不发送**（不截断，避免点错格子）。
- UTF-8：三个值按 UTF-8 编码，上限 U+07FF（坐标 2015），超出不发送。
- **按住 Shift 一律不上报**（terminal.md §8.1），由终端自己处理选择与右键。
- 不支持：X10 兼容模式（DECSET 9，alacritty_terminal 不识别，`TermModes` 也没有）、SGR-Pixels（1016，`MouseEvent` 没有像素坐标）。
- 不做同一单元格内移动去重，由 UI 只在单元格变化时调用。

## 5. 滚轮

`lines` 正数向上。去向优先级：

1. 程序开了鼠标上报且没按 Shift → `Report`：滚轮按钮码，每行一次（最多 1000 次）；坐标无法表示时返回空 `Report`（吞掉事件）。
2. 备用屏幕且开了 1007 → `ArrowKeys`：上下方向键，遵守 DECCKM，每行一次。
3. 否则 → `ScrollViewport(lines)`（语义同 `Terminal::scroll_display`）。

Shift+滚轮跳过 1，落到 2 或 3。

## 6. 粘贴

`encode_paste`：
- 换行统一成 `\r`（`\r\n`、`\n` 都变一个 `\r`），无论是否 bracketed。
- 开了 bracketed paste（2004）：包裹 `ESC[200~ … ESC[201~`，并**删除文本里所有 ESC 字符**。安全原因：剪贴板里夹带的 `ESC[201~` 会提前结束包裹，其后的内容被 shell 当成用户键入的命令执行（paste injection）。没开 bracketed paste 时不删 ESC（程序本来就把粘贴当键入）。
- 没有做的事：不过滤其它控制字符。

`quote_paths`（terminal.md §8.3；AI CLI 会话「原样粘贴路径」由会话层判断，不调用它）：

- POSIX：反斜杠转义 ` ` `'` `"` `\` `` ` `` `$` `!` `&` `*` `(` `)` `[` `]` `{` `}` `;` `<` `>` `|` `?` `#` `~` `^`（依据 POSIX Shell Command Language §2.2、§2.6 的元字符与展开触发字符，`~` `#` 不区分位置一律转义；`^` 为旧式管道与 zsh extendedglob）。控制字符不能用反斜杠（反斜杠换行是续行），写成 ANSI-C 引号 `$'\n'` `$'\t'` `$'\r'` `$'\xHH'`（bash/zsh/ksh；dash 不支持）。
- PowerShell：反引号转义 空格 `` ` `` `$` `'` `"` `(` `)` `{` `}` `[` `]` `;` `,` `&` `|` `@` `#` `<` `>`，以及弯引号 U+2018–U+201E（PowerShell 视其为引号）。`\t` `\n` `\r` 写成 `` `t `` `` `n `` `` `r ``，其它控制字符丢弃。
- 多个路径用单个空格连接，空列表得空串。非 ASCII（中文等）不转义。

## 7. 与 terminal.md 的对应

| 条目 | 位置 |
|---|---|
| §5.2 终端回应不算输入 | 本模块只编码用户输入；回应在 `take_replies` |
| §8.1 Shift 绕过鼠标上报 | `encode_mouse` / `encode_scroll` |
| §8.3 粘贴（文件转义、bracketed paste） | `quote_paths`、`encode_paste` |
| §9 Option/Alt+←/→、Option 作为 Meta、⌘ 不发给 shell、kitty | `encode_key` |
| §14 补全弹窗的按键拦截 | 不在本模块，由会话层先于 `encode_key` 处理 |
