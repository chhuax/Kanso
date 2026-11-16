# 本地 shell 实现说明（`kanso-pty`）

本文记录 `local-shell.md` 在 `crates/kanso-pty` 里的具体实现：环境变量清单、locale 决策、zsh shim 的加载机制与提示符合并规则。规格本身以 `spec/local-shell.md` 为准，序列格式以 `shell-integration.md` 为准。

## 1. 环境变量清单

`build_env(base, opts)` 返回相对 `base` 的变更列表（`Some` 设置、`None` 删除）；不在列表里的变量一律继承。常量表在 `crates/kanso-pty/src/env.rs`。

### 1.1 覆盖（无条件设置）

| 变量 | 值 | 说明 |
|---|---|---|
| `TERM` | `xterm-256color` | `FORCED_ENV` |
| `COLORTERM` | `truecolor` | `FORCED_ENV` |
| `TERM_PROGRAM` | `Kanso` | `FORCED_ENV` |
| `TERM_PROGRAM_VERSION` | 应用版本号 | 由 `EnvOptions::term_program_version` 传入 |
| `LANG` | locale 决策的结果 | 只有 `decide_locale` 返回 `Some` 时设置，见 §2 |
| `ZDOTDIR` | shim 目录 | 只有 `EnvOptions::zsh` 为 `Some` 时设置，见 §3 |
| `KANSO_USER_ZDOTDIR` | 用户原来的 `ZDOTDIR` | 用户没有设置过则**删除**（防止继承到过期值）。Kanso 从终端里启动（继承的环境有 `TERM`）时，继承的 `ZDOTDIR` 可能是那个终端自己的 shim 目录，一律视为没有设置过，与从 Dock 启动一致；`~/.zshenv` 里设置的 `ZDOTDIR` 仍由 shim 识别 |
| `KANSO_SHELL_INTEGRATION` | `1` / `0` | `0` 表示关闭提示符集成 |

### 1.2 删除

| 变量 | 条件 | 依据 |
|---|---|---|
| `NO_COLOR` | 存在即删（任何值） | no-color.org 约定，规格 §3 明确要求 |
| `NODE_DISABLE_COLORS` | 存在即删 | Node.js 的 `tty` 颜色检测把它与 `NO_COLOR` 同等对待 |
| `ANSI_COLORS_DISABLED` | 存在即删 | Python `termcolor` 等库认的同类"存在即禁用"开关 |
| `CLICOLOR` | 值为 `0` 才删 | BSD / macOS `ls` 的"不要颜色"约定；`CLICOLOR=1` 是要求颜色，保留 |
| `FORCE_COLOR` | 值为 `0` 或 `false` 才删 | Node.js `supports-color` 家族把它们解释为强制无色；其余值（`1`/`2`/`3`）是要求颜色，保留 |

判断标准：这个变量是不是"启动 Kanso 的父进程（构建工具、代理进程、CI 包装器）为了自己的输出带来的、会让**用户 shell 里所有工具**无色"的约束。同样检查过但**保留**的：

- `CLICOLOR_FORCE`：只会强制开启颜色。
- `CARGO_TERM_COLOR`、`npm_config_color`、`GCC_COLORS`、`LS_COLORS` / `LSCOLORS`：是用户或工具自己的显式偏好，只影响单个工具，不属于"全局禁用颜色"，删除会覆盖用户在 profile 里的有意设置。
- `TERM=dumb`：本来就被 `TERM` 覆盖。

### 1.3 继承

除 §1.1、§1.2 之外的所有变量原样继承，包括 `PATH`、`HOME`、`SHELL`、`LC_*`、`TERM_SESSION_ID`、各类 `ITERM_*` / `VSCODE_*` 之类宿主留下的变量。

`PtySession::spawn` 在启动前对子进程环境做 `env_clear`，再只放入 `SpawnConfig::env`（即 `base.apply(build_env(..))` 的完整结果），本进程自己的环境不会绕过上面三类清单混进去。唯一例外：`SHELL` 若不在 `SpawnConfig::env` 里，portable-pty 会按口令数据库补上登录 shell。

## 2. Locale 决策（`decide_locale`）

输入：显式 locale、会话是否 UTF-8、环境快照、平台、系统语言与地区、"系统是否带有某个 locale"的闭包。顺序（规格 §4）：

1. 显式 locale 通过 `kanso_types::Locale::parse` 校验（字母数字与 `_ - . @`，长度 ≤ 64）：直接作为 `LANG`。不合规视为没填，继续往下，不会写入环境变量。
2. 会话编码不是 UTF-8：不设置。
3. `LC_ALL` / `LC_CTYPE` / `LANG` 任一非空：不动。
4. 自动选择：
   - macOS：候选 = `<语言>_<用户地区>.UTF-8`、`<语言>_<语言标识自带的地区>.UTF-8`、`<语言>_<常用地区>.UTF-8`，取第一个 `/usr/share/locale/<名称>` 存在的；都没有用 `en_US.UTF-8`。语言标识去掉书写体系（`zh-Hans` → `zh`）与 `@` 后的修饰。
   - Linux / BSD：`C.UTF-8`。
   - Windows：不设置。

## 3. zsh shim 加载机制

### 3.1 文件与调用关系

`install_zsh_shim(data_dir)` 在 `<data_dir>/shell-integration/zsh/` 下生成 `.zshenv`、`.zprofile`、`.zshrc`、`.zlogin`、`.zlogout`（脚本源码在 `crates/kanso-pty/src/shim/zsh/`）。内容与已有文件一致时不重写；替换时先写临时文件再 `rename`。写入失败返回错误，调用方据此**不构造 `ZshIntegration`**，shell 按原样启动。

### 3.2 `ZDOTDIR` 的传递（时间线）

```
启动：ZDOTDIR=<shim>，KANSO_USER_ZDOTDIR=<用户原 ZDOTDIR，可能没有>
 │
 ├─ /etc/zshenv
 ├─ <shim>/.zshenv      ┐ 每个 shim 文件的动作相同：
 │                      │  1. 记下 shim 目录（当前 ZDOTDIR）
 │                      │  2. ZDOTDIR 临时改成用户的值（没有就 unset）
 │                      │  3. source 用户目录下的同名文件
 │                      │  4. 用户文件若设置了 ZDOTDIR，记入 KANSO_USER_ZDOTDIR
 │                      │  5. ZDOTDIR 切回 shim 目录
 │                      ┘ （保证 zsh 用「当时的 ZDOTDIR」去找下一个文件时仍然落在 shim）
 ├─ <shim>/.zprofile    （login）
 ├─ /etc/zshrc          （此时 HISTFILE 可能被设成 <shim>/.zsh_history）
 ├─ <shim>/.zshrc       定义函数 → 修正 HISTFILE → 加载用户 .zshrc → 注册 hook → 非 login 时还原 ZDOTDIR
 ├─ <shim>/.zlogin      （login）加载用户 .zlogin → 还原 ZDOTDIR
 └─ 退出：用户自己的 .zlogout（ZDOTDIR 已还原，zsh 直接读用户目录；<shim>/.zlogout 只在异常路径兜底）
```

- 步骤 2 的原因：用户文件里的 `$ZDOTDIR` 引用（如 `HISTFILE=$ZDOTDIR/hist`）、`[[ -z $ZDOTDIR ]]` 判断必须与没有 shim 时一致。
- 步骤 4 的原因：用户常在 `~/.zshenv` 里设置 `ZDOTDIR=~/.config/zsh`，之后的启动文件应去新目录找，这是 zsh 本来的语义。
- 用户文件必须在 shim 的**顶层**直接 `source`，不能包进函数：否则用户文件里的 `typeset` 会变成函数局部变量。
- **还原 `ZDOTDIR`**（规格没有要求，实现时的补充）：启动完成后把 `ZDOTDIR` 还原成用户自己的值（没有就 unset）。否则交互式 shell 里一直留着指向 shim 的 `ZDOTDIR`，有些安装器会据此把配置追加进 shim 的 `.zshrc`，用户启动的子 zsh 也会误加载 shim（嵌套的 zsh 因此没有提示符集成）。还原时用 `export`，让子 zsh 与本 shell 使用同一个用户配置目录。非交互非 login 的 zsh（脚本）在 `.zshenv` 末尾就还原。

### 3.3 HISTFILE

macOS 的 `/etc/zshrc` 把 `HISTFILE` 设成 `${ZDOTDIR:-$HOME}/.zsh_history`，那时 `ZDOTDIR` 指向 shim。`.zshrc` shim 在加载用户 `.zshrc` 之前，**仅当** `HISTFILE` 恰好等于 `<shim>/.zsh_history` 时改回 `<用户 ZDOTDIR 或 HOME>/.zsh_history`。用户自己设置过其他值不动；本来没设置（部分 Linux 发行版）保持没设置。

## 4. 提示符集成

### 4.1 输出序列

见 `shell-integration.md` 末尾的"shim 实际输出的序列顺序"。要点：precmd 里 D（有上一条命令时）→ OSC 7 → A，PS1 末尾带 B，preexec 里发 C。

### 4.2 提示符合并规则

对用户当前的 PS1 做如下处理，再与目录、分支组装（`.zshrc` shim 的 `__kanso_analyze_ps1` / `__kanso_build_ps1`）：

- 按 token 扫描（不是整串替换，避免 `%%~` 被误当成 `%~`）。去掉 `%~`、`%d`、`%/`、`%c`、`%C`、`%.` 及带数字的写法（`%1~`、`%-3~`、`%2d`、`%2c`）；去掉 `%n`、`%N`、`@%m`、`@%M`。**不会**去掉单独的 `%m`（没有与用户名相连的主机名可能是用户刻意显示的）。
- 原提示符里出现 `$PWD`、`${PWD`、`$(pwd`、`` `pwd `` 时视为用户自己显示目录：不去掉任何目录转义，也不再显示我们的目录；分支仍显示。
- 有 `%#` 时分支插在第一个 `%#` 前面；没有时紧跟在目录后面（没有目录时放最前面）。
- 合并连续空格、去掉开头空格；原提示符末尾有空格时保留一个（正文被全部去掉时也至少补一个）。
- 目录：`~` 缩写主目录；超过 36 个字符时保留尾部若干完整路径段，前面加 `.../`（含 `.../` 不超过 36；最后一段本身超长时也完整保留）。颜色：目录用默认前景色、分支 `%F{2}`、`%#` `%F{1}`，用调色板索引。
- 目录与分支都是数据：`%` 转义成 `%%`；控制字符分别被丢弃 / 替换成 `?`（防止终端把它们当转义序列）。`promptsubst` 开启时改为插入 `${__kanso_p_dir}` / `${__kanso_p_branch}` 变量引用（变量里同样已转义 `%`），分支名里的 `$(...)` 不会被执行。
- Git 分支：自 `$PWD` 向上找 `.git`；目录读 `HEAD`；文件按 `gitdir:` 找（相对路径以 `.git` 文件所在目录为基准）。`ref:` 去掉 `refs/heads/`；detached 取 40 或 64 位十六进制的前 7 位；其他内容不显示。全程不启动 git 进程。

### 4.3 「用户原始 PS1」如何保存

- `__kanso_last_ps1` 记录我们上次写入的 PS1。每次 precmd：当前 PS1 与它相同，沿用上次对用户 PS1 的分析结果；不同，说明用户 / 主题（如 oh-my-zsh 在 precmd 里重设 PS1）或别的脚本改过，重新捕获并分析。我们的 precmd 注册在用户 hook 之后，所以看到的是最终值。
- 如果新的 PS1 带有我们的 B 标记（别的脚本基于我们的 PS1 叠加，如 virtualenv 的 `PS1="(venv) $PS1"`），先把我们上次插入的目录文本、分支文本和 B 标记摘掉再分析，避免重复叠加。摘除依赖 `__kanso_ins_dir` / `__kanso_ins_branch` 保存的上次插入文本。
- 已知边界：如果别的脚本把 PS1 改成恰好与我们上次输出完全相同的文本，无法与"没改过"区分。

## 5. 关闭与卸载

- 关闭提示符集成：构造 `ZshIntegration { prompt_enabled: false, .. }`（环境变量 `KANSO_SHELL_INTEGRATION=0`）。shim 仍加载用户文件并修正 HISTFILE，但不定义函数、不注册 hook、不改 PS1。
- 完全不使用 shim：不构造 `ZshIntegration`（或 `install_zsh_shim` 失败时），`ZDOTDIR` 保持原样。
- 卸载：删除 `<data_dir>/shell-integration/zsh/` 目录。用户的任何文件从未被修改，已运行的 shell 不受影响。

## 6. 前台进程工作目录

`PtySession::foreground_cwd`：用 portable-pty 的 `MasterPty::process_group_leader`（内部就是对主端 fd 调 `tcgetpgrp`）取得前台进程组组长，再读它的 cwd（macOS `proc_pidinfo(PROC_PIDVNODEPATHINFO)`，Linux `/proc/<pid>/cwd`）；读不到（组长已退出等）退回 shell 进程本身。macOS 返回真实路径（`/tmp` → `/private/tmp`）。Windows 返回 `None`。

## 7. 已知限制

- 只集成 zsh；bash、fish 不在首版范围（fish 默认发 OSC 7）。
- Windows 的命令行拆分只是简化版（TODO M6），没有实现 ConPTY 相关的验证。
- 目录宽度按字符数计算，不考虑东亚宽字符。
- precmd 排在注册时已有的 hook 之后；之后才加入的 hook 会排在我们后面。

## 8. 会话层（`kanso-session`，I1）

本文前面描述的是 `kanso-pty` 提供的零件；把它们串成一个能用的本地会话的是 `kanso-session` 的 `LocalSession`，
线程、锁、唤醒合并、两种会话结束方式的设计写在该 crate 的模块文档（`crates/kanso-session/src/lib.rs`）里。这里只记与本地 shell 相关的几点：

- **启动顺序**：`resolve_command`（命令行取 `default_shell`，空则 `$SHELL`）→ `install_zsh_shim`（仅 `shell_integration` 开启时；
  失败记日志、不设 `ZDOTDIR`，照常启动）→ `decide_locale`（真实的 `system_locale_exists`）→ `build_env` → `PtySession::spawn`
  （起始目录为主目录，初始尺寸为 `open_session` 传入的尺寸）。`PtySession::spawn` 内部全局串行：macOS 上并发 `openpty` 会偶发失败，
  且 fd 在 close-on-exec 之前可能被别的线程 fork 出去的 shell 继承。
- **写入端**：启动后用 `PtySession::take_writer` 交给写线程独占，这样写阻塞（shell 不读输入、缓冲区满）不会占着 `PtySession` 的锁，
  `resize` / `kill` 不受影响。
- **关闭会话时读线程继续排空 PTY**：macOS 上 shell 退出时关闭 tty 要等已写出的输出被主端读走，没人读就卡在内核里，
  SIGKILL 也唤不醒（表现为 `kill` 里的 `wait` 永远不返回）。所以用户关闭后读线程继续读到 EOF，只是丢弃数据。
- **git 分支**：`kanso_session::git_branch` 与 zsh shim 的 `__kanso_git_branch` 规则一致（目录 / `gitdir:` 文件 / detached 7 位 / 丢弃控制字符，
  不启动 git），在每次 OSC 7 之后由读线程计算，`LocationChanged` 按 `(cwd, host, branch)` 去重。
- **读缓冲**：64 KiB，但 macOS 的 PTY 每次 `read` 实际只返回约 1 KiB（内核 tty 缓冲粒度），这是吞吐的主要限制；
  每次读到的数据立即交给终端内核（一次读取 = 一个批次），不攒批。
