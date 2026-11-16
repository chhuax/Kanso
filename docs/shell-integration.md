# Shell 集成序列

本文定义终端内核（`crates/kanso-term`）识别的、由 shell 集成脚本发出的 OSC 序列，以及 Kanso 自己的扩展。
本地 zsh 的 shim（`local-shell.md` §5，T5）按本文输出；远程会话没有我们的集成，只能依赖 shell 自己
发出的 OSC 7。解析实现见 `crates/kanso-term/src/osc.rs`，行为验收见 `terminal.md` §4、§5、§12。

通用规则：
- 序列以 `ESC ]` 开头，以 `BEL`（0x07）或 `ESC \`（ST）结束；两种结束符都接受。
- 负载按“编号 `;` 其余部分”切分，**其余部分不再按 `;` 切分**（路径里可以有分号）。
- 终端不认识的编号、格式不对的负载一律静默忽略，不影响后续输出。
- 单个负载超过 8 KiB 的序列整条忽略。

## OSC 133：命令边界（FinalTerm / iTerm2 约定）

| 序列 | 含义 | 终端的处理 |
|---|---|---|
| `OSC 133 ; A [; 选项…]` | 提示符开始 | 记录在**处理到这个序列时光标所在的行**，该行带 `LINE_PROMPT`，产生 `PromptStart` 事件 |
| `OSC 133 ; B` | 提示符结束，用户开始输入命令 | 产生 `InputStart` 事件（带光标所在行） |
| `OSC 133 ; C` | 命令开始执行，输出从下一行起 | 产生 `CommandStart` 事件（带光标所在行） |
| `OSC 133 ; D [; 退出码]` | 命令结束 | 产生 `CommandFinished` 事件，退出码是 D 后面第一个纯数字参数；没有就是 `None` |

- A 的选项是 `key=value`（如 FinalTerm 的 `aid=…`、`cl=…`），终端只认 `kanso=first`，其余忽略。
- **同一行重复的 A 只记一次**：zsh 重绘提示符（窗口缩放、`zle reset-prompt`）时会在同一行再发一次 A，
  第二次起不产生事件、不叠加标记，否则半透明的分隔线会叠在一起越来越亮（`terminal.md` §4）。
  按“行”判断：窗口宽度变化导致软换行重排后，标记仍属于提示符所在的那个逻辑行（第一行）。
- 全屏（alternate screen）期间收到的 OSC 133 被忽略：全屏程序不属于我们的 shell 集成。
- 标记所在行的上方画分隔线（`LINE_PROMPT`），所以 shim 要保证 A 是在“分隔线应当落在其上方”的那一行
  发出的。`local-shell.md` §5.2 要求之后的每个提示符前有两个空行、分隔线画在两行正中间：
  shim 应在第一个空行之后发 A，再输出第二个空行与提示符文字。

### Kanso 扩展：`kanso=first`

会话的**第一个**提示符发 `OSC 133 ; A ; kanso=first`（`local-shell.md` §5.2：第一个提示符不插入空行，
上方不画分隔线）；之后的提示符发普通的 `OSC 133 ; A`。

- 带 `kanso=first` 的行同时带 `LINE_PROMPT` 与 `LINE_FIRST_PROMPT`，`PromptStart` 事件的 `first` 为 `true`。
- 同一行先发了普通 A、之后又发 `kanso=first` 的 A，会升级为首个提示符（不产生新事件）。
- 内核不根据“是不是第一个”自己推断，只看这个显式标记：重新连接、`clear` 之后的第一个提示符
  是否算“第一个”由 shim 决定。

## 工作目录上报

只保留**最新一次**上报，并记录主机。每次上报都会产生 `CwdChanged` 事件（不与上一次去重，
因为 `terminal.md` §12 要把它同时当作“提示符出现”的信号）。

### OSC 7：`OSC 7 ; file://主机/路径`

- 主机为空（`file:///path`）时 `host` 为 `None`；`localhost` 按原样保留，是否算本机由调用方判断。
- 路径做百分号解码（`%20`→空格，`%E4%B8%AD`→中）。**很多提示符脚本直接写 `$PWD` 而不编码**，
  所以解码失败（非法的 `%xx`，或解码结果不是合法 UTF-8）时按原样使用。
  推论：没有编码的路径里如果恰好含有形如 `%41` 的合法转义，会被误解码，这是不可避免的歧义，shim 应当编码。
- 也接受直接写绝对路径的 `OSC 7 ; /path`（主机为 `None`，不做百分号解码）。
- 不是 `file://` 也不以 `/` 开头的负载忽略。

### OSC 1337：`OSC 1337 ; CurrentDir=路径`（iTerm2）

- 路径原样使用（不解码），主机为 `None`。其它 1337 子命令忽略。

### OSC 9;9：`OSC 9 ; 9 ; 路径`（ConEmu / Windows Terminal）

- 路径两侧的双引号可有可无：`9;9;"C:\Users\me"` 与 `9;9;C:\Users\me` 等价。
- 主机为 `None`。OSC 9 的其它形式（iTerm2 的通知 `9;消息`、ConEmu 的进度 `9;4;…`）不是工作目录，忽略。

## 终端回应不算用户输入

终端对 DA、DSR/CPR、颜色查询、焦点上报等的应答（`Terminal::take_replies`）由调用方原样写回通道，
**不能**当作用户按键（`terminal.md` §5.2）。shell 集成脚本不应依赖这些回应。

## 本地 zsh shim 实际输出的序列顺序

`local-shell.md` §5 的 shim 按下面的顺序输出（实现见 `crates/kanso-pty/src/shim/zsh/zshrc.zsh`，说明见 `local-shell-impl.md`）。`␤` 表示换行；tty 的 ONLCR 会把它变成 `\r\n`，对终端内核无影响。

**第一个提示符**（precmd）：

```
OSC 7 ; file://主机/百分号编码的PWD
OSC 133 ; A ; kanso=first
提示符文字（PS1），末尾紧跟 OSC 133 ; B
```

不输出 D（之前没有命令），不插空行。

**之后的每个提示符**（precmd）：

```
OSC 133 ; D ; 退出码        ← 仅当上一条命令执行过（preexec 触发过）；空回车不发
OSC 7 ; file://主机/百分号编码的PWD
␤                          ← 第一个空行
OSC 133 ; A
␤                          ← 第二个空行；分隔线画在 A 所在行上方，即两个空行正中间
提示符文字（PS1），末尾紧跟 OSC 133 ; B
```

**命令执行前**（preexec）：`OSC 133 ; C`，此时 zle 已经把光标移到下一行。

- A 与 B 之间没有别的 OSC 133；B 放在 PS1 末尾并用 `%{ … %}` 包成零宽度，所以 zle 重绘提示符（`reset-prompt`、窗口缩放）时会再发一次 B，终端按"同一行重复只记一次"处理（见上文）。A 只在 precmd 里输出一次，重绘不会重复发 A。
- 退出码是上一条命令的 `$?`，在 shim 的 precmd 入口第一条语句里取，不受用户 precmd hook 影响（zsh 在运行 hook 之间会保持 `$?`）。
- OSC 7 的路径对除 `[A-Za-z0-9/._~-]` 之外的每个字节做 `%XX` 编码，主机取 zsh 的 `$HOST`，也经过同样的编码。
- `KANSO_SHELL_INTEGRATION=0` 时一个序列都不输出。
