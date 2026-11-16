//! 粘贴：文本编码与文件路径转义（terminal.md §8.3）。

/// 文件路径转义的目标 shell。两者需要转义的字符集不同（terminal.md §8.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellFlavor {
    /// POSIX 系 shell（sh / bash / zsh / fish）：反斜杠转义。
    Posix,
    /// Windows PowerShell / pwsh：反引号转义。
    PowerShell,
}

/// 编码一次文本粘贴。
///
/// 换行统一成 `\r`：终端里回车键发的就是 CR，`\n` 在 tty 行规程下会被当成 Ctrl+J，
/// 很多全屏程序（vim 的插入模式之外、各种 TUI 输入框）把它当成「换行快捷键」而不是提交。
///
/// 开启 bracketed paste（DECSET 2004）时用 `ESC [200~ … ESC [201~` 包裹，并**删除文本中所有 ESC 字符**：
/// 否则剪贴板里夹带的 `ESC[201~` 会提前结束包裹，其后的内容就被 shell 当成用户键入的命令执行
/// （经典的 paste injection）。删掉 ESC 后文本里不可能再构造出任何转义序列。
/// 没开 bracketed paste 时不删 ESC：此时程序本来就把粘贴当成普通键入，原样转发才与其它终端一致。
pub fn encode_paste(text: &str, modes: &crate::types::TermModes) -> Vec<u8> {
    let mut body = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // `\r\n` 只算一个换行。
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                body.push('\r');
            }
            '\n' => body.push('\r'),
            '\x1b' if modes.bracketed_paste => {}
            _ => body.push(c),
        }
    }
    if !modes.bracketed_paste {
        return body.into_bytes();
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// POSIX shell 里需要用反斜杠转义的字符。
///
/// 依据 POSIX Shell Command Language §2.2 / §2.6 的元字符与展开触发字符：
/// 空格（分词）、引号 `'` `"` 与 `\`、`$` `` ` ``（变量与命令替换）、`!`（历史展开）、
/// `& ; | < > ( )`（控制与重定向）、`* ? [ ]`（通配）、`{ }`（花括号展开）、`#`（注释）、
/// `~`（波浪号展开）、`^`（旧式管道，zsh 的 extendedglob）。
/// `~`、`#` 严格说只在词首有意义，但在任意位置转义都无害，所以不区分位置，规则更简单、不会漏。
const POSIX_SPECIAL: &str = " '\"\\`$!&*()[]{};<>|?#~^";

/// PowerShell 里需要用反引号转义的字符。
///
/// 依据 about_Special_Characters / about_Parsing：空格、`` ` ``（转义符本身）、`$`、
/// 单双引号、`( ) { } [ ]`、`; ,`、`& |`、`@`、`#`、`< >`。
/// PowerShell 还把弯引号 U+2018..U+201E 当作引号，粘贴自文档的路径可能带它们，同样转义。
const POWERSHELL_SPECIAL: &str =
    " `$'\"(){}[];,&|@#<>\u{2018}\u{2019}\u{201a}\u{201b}\u{201c}\u{201d}\u{201e}";

/// 把文件路径列表转成一行可粘贴的文本：逐个转义后用单个空格连接。
///
/// 用于普通 shell 会话。AI CLI 会话要「原样粘贴路径」让工具识别成附件，那是会话层的判断，
/// 不调用本函数（terminal.md §8.3）。
///
/// 控制字符的处理：
/// - POSIX：反斜杠加换行在 shell 里是「续行」（会被吞掉），不能用来转义换行；制表符转义后虽然合法，
///   但粘贴到 readline 时未转义的 Tab 会触发补全。所以控制字符统一写成 `$'\n'`、`$'\t'`、`$'\xHH'`
///   （ANSI-C 引号，bash / zsh / ksh 支持，dash 不支持）。
/// - PowerShell：`\t`、`\n`、`\r` 写成 `` `t ``、`` `n ``、`` `r ``，其它控制字符直接丢弃
///   （没有不依赖 PowerShell 版本的表示法）。
pub fn quote_paths(paths: &[String], flavor: ShellFlavor) -> String {
    let quoted: Vec<String> = paths
        .iter()
        .map(|p| match flavor {
            ShellFlavor::Posix => quote_posix(p),
            ShellFlavor::PowerShell => quote_powershell(p),
        })
        .collect();
    quoted.join(" ")
}

fn quote_posix(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        match c {
            '\n' => out.push_str("$'\\n'"),
            '\t' => out.push_str("$'\\t'"),
            '\r' => out.push_str("$'\\r'"),
            c if c.is_control() && (c as u32) < 0x100 => {
                out.push_str(&format!("$'\\x{:02x}'", c as u32));
            }
            c if POSIX_SPECIAL.contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

fn quote_powershell(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        match c {
            '\n' => out.push_str("`n"),
            '\t' => out.push_str("`t"),
            '\r' => out.push_str("`r"),
            c if c.is_control() => {}
            c if POWERSHELL_SPECIAL.contains(c) => {
                out.push('`');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}
