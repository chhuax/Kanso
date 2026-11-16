//! AI CLI 识别（terminal.md §6.1）：根据用户提交的命令行判断是不是在启动一个 AI 命令行工具的**交互会话**。
//!
//! 这是纯函数，不读屏幕、不读时钟；"提交的命令行"由命令跟踪（`track` 模块）从屏幕内容里取出。
//!
//! # 识别流程
//!
//! 1. 按 shell 的引号 / 转义规则切词，并按 `;` `&&` `||` `&` `|` 切成命令段。
//! 2. 处在管道里的段（前面或后面有 `|`）不算：从管道读输入、或把输出交给管道的工具并没有在操作终端。
//!    其余的段依次检查，第一个识别出工具的就是结果（所以 `cd repo && claude` 算启动）。
//! 3. 每段：跳过 `VAR=value` → 跳过前缀命令（sudo、env、npx……）及其选项 → 跳过两词包运行器
//!    （`npm exec`、`uv run`……）及其选项 → 取可执行文件名（路径最后一段、去掉 `.exe`、去掉 npm 包的版本后缀）匹配工具清单。
//! 4. 一次性运行不算会话：参数里出现 `-p` / `--print` / `-h` 等选项，或第一个位置参数是 `exec` / `run` 等子命令。
//!
//! # 已知限制
//!
//! 不展开变量、别名与函数；不理解子 shell、重定向。带参数的工具选项（例如 `--model opus`）的取值会被当作
//! 位置参数，所以 `claude --model run` 会被误判成子命令；规格明确"只看第一个位置参数"，这里照做。

use kanso_types::agent::AgentKind;

/// 可执行文件名（小写）→ 工具。同一工具的不同写法归为同一个工具。
///
/// `qwen-code`、`opencode-ai`、`aider-chat` 是这些工具在 npm / PyPI 上的包名，规格要求"也要能识别
/// npm 包名的写法"，所以一并收录。
const TOOLS: &[(&str, AgentKind)] = &[
    ("claude", AgentKind::Claude),
    ("claude-code", AgentKind::Claude),
    ("codex", AgentKind::Codex),
    ("gemini", AgentKind::Gemini),
    ("gemini-cli", AgentKind::Gemini),
    ("aider", AgentKind::Aider),
    ("aider-chat", AgentKind::Aider),
    ("cursor-agent", AgentKind::CursorAgent),
    ("copilot", AgentKind::Copilot),
    ("amp", AgentKind::Amp),
    ("opencode", AgentKind::OpenCode),
    ("opencode-ai", AgentKind::OpenCode),
    ("crush", AgentKind::Crush),
    ("goose", AgentKind::Goose),
    ("qwen", AgentKind::Qwen),
    ("qwen-code", AgentKind::Qwen),
    ("droid", AgentKind::Droid),
    ("openhands", AgentKind::OpenHands),
    ("grok", AgentKind::Grok),
];

/// 一次性运行的选项（精确匹配，或 `--opt=value` 的形式）。
const ONE_SHOT_OPTIONS: &[&str] = &[
    "-p",
    "--print",
    "--prompt",
    "-m",
    "--message",
    "-h",
    "--help",
    "-v",
    "-V",
    "--version",
];

/// 第一个位置参数是这些子命令时是一次性运行。
const ONE_SHOT_SUBCOMMANDS: &[&str] = &[
    "exec",
    "run",
    "mcp",
    "update",
    "upgrade",
    "install",
    "uninstall",
    "doctor",
    "config",
    "login",
    "logout",
    "auth",
    "help",
    "version",
    "completion",
    "setup-token",
];

/// 前缀命令。
const PREFIXES: &[&str] = &[
    "sudo", "doas", "env", "command", "exec", "time", "nohup", "npx", "pnpx", "bunx", "uvx",
];

/// 两词包运行器：`(第一个词, 第二个词)`。
const RUNNERS: &[(&str, &str)] = &[
    ("npm", "exec"),
    ("pnpm", "dlx"),
    ("pnpm", "exec"),
    ("yarn", "dlx"),
    ("bun", "x"),
    ("uv", "run"),
    ("deno", "run"),
];

/// 一个词：引号 / 转义处理后的文本，以及它是否带过引号。
///
/// 带引号这件事要保留：`codex "run the tests"` 的开场提示不算子命令，`claude "-p"` 也不是选项。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    text: String,
    quoted: bool,
}

/// 命令段之间的分隔符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    /// 行尾。
    End,
    /// `|` 或 `|&`。
    Pipe,
    /// `;`、`&&`、`||`。
    Next,
    /// `&`：命令放到后台，不占用终端。
    Background,
}

struct Segment {
    words: Vec<Word>,
    sep: Sep,
}

/// 按 shell 规则切词并切段。不认识的语法（子 shell、重定向）按普通字符处理。
fn parse(line: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut in_word = false;
    let mut chars = line.chars().peekable();

    macro_rules! flush_word {
        () => {
            if in_word {
                words.push(Word {
                    text: std::mem::take(&mut cur),
                    quoted,
                });
                quoted = false;
                in_word = false;
            }
        };
    }
    macro_rules! end_segment {
        ($sep:expr) => {{
            flush_word!();
            segments.push(Segment {
                words: std::mem::take(&mut words),
                sep: $sep,
            });
        }};
    }

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => flush_word!(),
            '\'' => {
                in_word = true;
                quoted = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            cur.push(chars.next().unwrap_or('\\'));
                        }
                        _ => cur.push(q),
                    }
                }
            }
            // 反斜杠只在后面是空白、引号、反斜杠或 shell 元字符时才是转义；否则按字面保留，
            // 这样 Windows 路径 `C:\tools\claude.exe` 不会被吃掉分隔符。
            '\\' => {
                in_word = true;
                match chars.peek() {
                    Some(&n) if n.is_whitespace() || "'\"\\&;|".contains(n) => {
                        cur.push(n);
                        chars.next();
                    }
                    _ => cur.push('\\'),
                }
            }
            ';' => end_segment!(Sep::Next),
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                    end_segment!(Sep::Next);
                } else {
                    if chars.peek() == Some(&'&') {
                        chars.next();
                    }
                    end_segment!(Sep::Pipe);
                }
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    end_segment!(Sep::Next);
                } else {
                    end_segment!(Sep::Background);
                }
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    // 行尾：最后一个词与最后一段直接收尾（不再需要重置 `in_word` / `quoted`）。
    if in_word {
        words.push(Word { text: cur, quoted });
    }
    if !words.is_empty() {
        segments.push(Segment {
            words,
            sep: Sep::End,
        });
    }
    segments
}

/// 是不是 `VAR=value` 赋值：名字部分是合法标识符。
fn is_assignment(w: &Word) -> bool {
    let Some((name, _)) = w.text.split_once('=') else {
        return false;
    };
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 可执行文件名：取路径最后一段（`/` 与 `\` 都算分隔符），去掉 `.exe`，转小写。
fn base_name(word: &str) -> String {
    let last = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = last.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

/// 把 npm 包名写法还原成工具名：去掉 `npm:` 前缀、作用域（`@scope/`）与版本后缀（`@latest`、`@1.2.3`）。
fn package_tool_name(word: &str) -> String {
    let w = word.strip_prefix("npm:").unwrap_or(word);
    // 作用域包以 `@` 开头，路径最后一段就是包名本身。
    let last = w.rsplit(['/', '\\']).next().unwrap_or(w);
    // 版本后缀的 `@` 一定不在开头（开头的是作用域）。
    let name = match last.find('@') {
        Some(i) if i > 0 => &last[..i],
        _ => last,
    };
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

fn tool_of(name: &str) -> Option<AgentKind> {
    TOOLS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// 前缀命令 / 包运行器的选项里，哪些后面跟一个独立的取值词。
///
/// 不查清楚会把取值当成要执行的命令（`sudo -u root claude` 会停在 `root`）。
/// 只收录常见选项；未收录的选项按"没有取值"处理。
fn option_takes_value(owner: &str, opt: &str) -> bool {
    let table: &[&str] = match owner {
        "sudo" => &[
            "-u", "--user", "-g", "--group", "-h", "--host", "-p", "--prompt", "-C", "-D",
            "--chdir", "-R", "--chroot", "-T", "-r", "--role", "-t", "--type", "-U",
        ],
        "doas" => &["-u", "-C"],
        "env" => &["-u", "--unset", "-C", "--chdir"],
        "exec" => &["-a"],
        "time" => &["-f", "--format", "-o", "--output"],
        "npx" | "pnpx" | "bunx" => &["-p", "--package", "-c", "--call"],
        "uvx" => &["--from", "--with", "-p", "--python"],
        "npm" | "pnpm" | "yarn" | "bun" => {
            &["-p", "--package", "-c", "--call", "-w", "--workspace"]
        }
        "uv" => &[
            "--with",
            "--from",
            "-p",
            "--python",
            "--directory",
            "--project",
            "--env-file",
            "--extra",
            "--group",
        ],
        _ => &[],
    };
    table.contains(&opt)
}

/// 从 `words[i..]` 起跳过选项（及其取值）与赋值，返回第一个"命令词"的下标。
/// 遇到 `--` 就跳过它并停止选项解析。
fn skip_options(owner: &str, words: &[Word], mut i: usize) -> usize {
    while let Some(w) = words.get(i) {
        if w.quoted {
            break;
        }
        if w.text == "--" {
            return i + 1;
        }
        if is_assignment(w) {
            i += 1;
        } else if w.text.starts_with('-') && w.text.len() > 1 {
            i += 1;
            if !w.text.contains('=') && option_takes_value(owner, &w.text) {
                i += 1;
            }
        } else {
            break;
        }
    }
    i
}

/// 在一个命令段里识别工具。
fn detect_in_segment(words: &[Word]) -> Option<AgentKind> {
    let mut i = 0;
    // 命令词前的 `VAR=value`。
    while words.get(i).is_some_and(is_assignment) {
        i += 1;
    }
    loop {
        let w = words.get(i)?;
        let base = base_name(&w.text);
        if PREFIXES.contains(&base.as_str()) {
            // `command -v claude` 是在查找命令，不是启动。
            if base == "command"
                && words
                    .get(i + 1)
                    .is_some_and(|n| matches!(n.text.as_str(), "-v" | "-V"))
            {
                return None;
            }
            i = skip_options(&base, words, i + 1);
            continue;
        }
        if RUNNERS
            .iter()
            .any(|(first, sub)| *first == base && words.get(i + 1).is_some_and(|n| n.text == *sub))
        {
            i = skip_options(&base, words, i + 2);
            // `npx` 风格的运行器之后跟的是包名写法。
            return finish(words, i, true);
        }
        // 前缀命令之后的命令词：`npx @anthropic-ai/claude-code` 走包名写法。
        let from_package_runner = i > 0
            && words[..i]
                .iter()
                .any(|p| matches!(base_name(&p.text).as_str(), "npx" | "pnpx" | "bunx" | "uvx"));
        return finish(words, i, from_package_runner);
    }
}

/// 命令词在 `words[i]`：匹配工具名，再检查一次性运行的条件。
fn finish(words: &[Word], i: usize, package_form: bool) -> Option<AgentKind> {
    let w = words.get(i)?;
    let name = if package_form {
        package_tool_name(&w.text)
    } else {
        base_name(&w.text)
    };
    let kind = tool_of(&name)?;
    if is_one_shot(&words[i + 1..]) {
        return None;
    }
    Some(kind)
}

/// 工具的参数是否表示一次性运行。
fn is_one_shot(args: &[Word]) -> bool {
    let mut first_positional_seen = false;
    let mut options_done = false;
    for w in args {
        if !options_done && !w.quoted {
            if w.text == "--" {
                options_done = true;
                continue;
            }
            if w.text.starts_with('-') && w.text.len() > 1 {
                // `--print=x` / `-m=x` 与单独的选项等价。
                let opt = w.text.split_once('=').map_or(w.text.as_str(), |(o, _)| o);
                if ONE_SHOT_OPTIONS.contains(&opt) {
                    return true;
                }
                continue;
            }
        }
        if !first_positional_seen {
            first_positional_seen = true;
            // 带引号的第一个位置参数是开场提示，不是子命令。
            if !w.quoted && ONE_SHOT_SUBCOMMANDS.contains(&w.text.as_str()) {
                return true;
            }
        }
    }
    false
}

/// 根据提交的命令行识别 AI CLI 会话（规则见模块文档与 terminal.md §6.1）。
///
/// 返回 `Some` 表示这条命令会让一个 AI 命令行工具占住终端；一次性运行（`claude -p hi`）、
/// 管道里的工具、普通命令都返回 `None`。
pub fn detect_agent(command_line: &str) -> Option<AgentKind> {
    let segments = parse(command_line);
    let mut prev_pipe = false;
    for seg in &segments {
        let in_pipeline = prev_pipe || seg.sep == Sep::Pipe;
        prev_pipe = seg.sep == Sep::Pipe;
        // 放到后台的命令不占用终端。
        if in_pipeline || seg.sep == Sep::Background {
            continue;
        }
        if let Some(kind) = detect_in_segment(&seg.words) {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentKind::*;

    #[test]
    fn detects_executable_names() {
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("claude", Some(Claude)),
            ("claude-code", Some(Claude)),
            ("codex", Some(Codex)),
            ("gemini", Some(Gemini)),
            ("gemini-cli", Some(Gemini)),
            ("aider", Some(Aider)),
            ("cursor-agent", Some(CursorAgent)),
            ("copilot", Some(Copilot)),
            ("amp", Some(Amp)),
            ("opencode", Some(OpenCode)),
            ("crush", Some(Crush)),
            ("goose", Some(Goose)),
            ("qwen", Some(Qwen)),
            ("droid", Some(Droid)),
            ("openhands", Some(OpenHands)),
            ("grok", Some(Grok)),
            ("ls -la", None),
            ("claudex", None),
            ("", None),
            ("   ", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn every_agent_kind_is_reachable() {
        for kind in AgentKind::ALL {
            assert!(
                TOOLS.iter().any(|(_, k)| k == kind),
                "{kind:?} 没有可执行文件名"
            );
        }
    }

    #[test]
    fn path_exe_and_assignments() {
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("/usr/local/bin/claude", Some(Claude)),
            ("~/.local/bin/codex --yolo", Some(Codex)),
            ("./node_modules/.bin/gemini", Some(Gemini)),
            ("claude.exe", Some(Claude)),
            ("Claude.EXE", Some(Claude)),
            (r"C:\tools\claude.exe", Some(Claude)),
            ("FOO=1 claude", Some(Claude)),
            ("FOO=1 BAR=\"a b\" codex", Some(Codex)),
            ("FOO=1", None),
            // 赋值之后不是命令名的情况。
            ("FOO=claude ls", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn prefix_commands_and_their_options() {
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("sudo claude", Some(Claude)),
            ("sudo -E claude", Some(Claude)),
            ("sudo -u root claude", Some(Claude)),
            ("sudo -u root -E codex", Some(Codex)),
            ("doas -u me aider", Some(Aider)),
            ("env FOO=1 claude", Some(Claude)),
            ("env -u HOME claude", Some(Claude)),
            ("env -i FOO=1 BAR=2 codex", Some(Codex)),
            ("command claude", Some(Claude)),
            ("command -v claude", None),
            ("exec claude", Some(Claude)),
            ("exec -a name claude", Some(Claude)),
            ("time claude", Some(Claude)),
            ("time -p claude", Some(Claude)),
            ("nohup claude", Some(Claude)),
            ("sudo env FOO=1 claude", Some(Claude)),
            ("FOO=1 sudo -E nohup claude", Some(Claude)),
            ("sudo", None),
            ("sudo ls", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn package_runners() {
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("npx @anthropic-ai/claude-code", Some(Claude)),
            ("npx -y @anthropic-ai/claude-code", Some(Claude)),
            ("npx @anthropic-ai/claude-code@latest", Some(Claude)),
            ("npx @openai/codex", Some(Codex)),
            ("npx @google/gemini-cli", Some(Gemini)),
            ("npx opencode-ai@1.2.3", Some(OpenCode)),
            ("npx -p @anthropic-ai/claude-code claude", Some(Claude)),
            ("pnpx @anthropic-ai/claude-code", Some(Claude)),
            ("bunx @openai/codex", Some(Codex)),
            ("uvx aider-chat", Some(Aider)),
            ("uvx --from aider-chat aider", Some(Aider)),
            ("npm exec @openai/codex", Some(Codex)),
            ("npm exec -- claude", Some(Claude)),
            ("npm exec --package=@openai/codex codex", Some(Codex)),
            ("pnpm dlx @openai/codex", Some(Codex)),
            ("pnpm exec claude", Some(Claude)),
            ("yarn dlx @anthropic-ai/claude-code", Some(Claude)),
            ("bun x @openai/codex", Some(Codex)),
            ("uv run aider", Some(Aider)),
            ("uv run --with rich aider", Some(Aider)),
            ("deno run npm:@anthropic-ai/claude-code", Some(Claude)),
            ("sudo npx @openai/codex", Some(Codex)),
            // 不是包运行器的子命令。
            ("npm run claude", None),
            ("npm install -g @anthropic-ai/claude-code", None),
            ("pnpm add codex", None),
            ("npx cowsay hi", None),
            ("npx", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn command_lists_and_pipes() {
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("cd repo && claude", Some(Claude)),
            ("cd x && codex", Some(Codex)),
            ("cd x; claude", Some(Claude)),
            ("false || claude", Some(Claude)),
            ("cd x && FOO=1 sudo claude", Some(Claude)),
            ("claude && echo done", Some(Claude)),
            ("echo x | claude", None),
            ("claude | tee log", None),
            ("cat a | grep b | claude", None),
            ("echo x |& claude", None),
            // 管道之外的段照常识别。
            ("echo x | cat && claude", Some(Claude)),
            ("claude &", None),
            ("claude & codex", Some(Codex)),
            // 引号里的操作符不是操作符。
            ("echo \"a | claude\"", None),
            ("echo 'a && claude'", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn one_shot_options() {
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("claude -p hi", None),
            ("claude -p \"hi there\"", None),
            ("claude --print hi", None),
            ("claude --print=hi", None),
            ("codex --prompt x", None),
            ("aider -m \"fix\"", None),
            ("aider --message fix", None),
            ("claude -h", None),
            ("claude --help", None),
            ("claude -v", None),
            ("claude -V", None),
            ("claude --version", None),
            ("codex --version", None),
            ("sudo claude -p hi", None),
            ("npx @anthropic-ai/claude-code -p hi", None),
            // 选项出现在别的参数之后也算。
            ("claude --model opus -p hi", None),
            // 其他选项不影响。
            ("claude --dangerously-skip-permissions", Some(Claude)),
            ("claude --model opus", Some(Claude)),
            ("claude -c", Some(Claude)),
            // 带引号的选项样子的词是开场提示，不是选项。
            ("claude \"-p\"", Some(Claude)),
            // `--` 之后不再解析选项。
            ("claude -- -p", Some(Claude)),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn one_shot_subcommands() {
        for sub in ONE_SHOT_SUBCOMMANDS {
            assert_eq!(detect_agent(&format!("codex {sub}")), None, "{sub}");
            assert_eq!(detect_agent(&format!("claude {sub} x")), None, "{sub}");
        }
        let cases: &[(&str, Option<AgentKind>)] = &[
            ("codex exec \"fix it\"", None),
            ("claude mcp list", None),
            ("claude update", None),
            ("claude doctor", None),
            ("npx @openai/codex exec hi", None),
            // 选项之后的第一个位置参数才是子命令。
            ("claude --verbose doctor", None),
            // 带引号的开场提示不算子命令。
            ("codex \"run the tests\"", Some(Codex)),
            ("codex \"exec\"", Some(Codex)),
            ("codex 'login'", Some(Codex)),
            // 只有第一个位置参数可能是子命令。
            ("codex fix run", Some(Codex)),
            ("claude explain exec", Some(Claude)),
            ("claude \"hello\" doctor", Some(Claude)),
            // 开场提示不是子命令。
            ("claude fix the bug", Some(Claude)),
        ];
        for (line, want) in cases {
            assert_eq!(detect_agent(line), *want, "{line:?}");
        }
    }

    #[test]
    fn acceptance_cases() {
        // terminal.md §18。
        assert_eq!(detect_agent("npx @anthropic-ai/claude-code"), Some(Claude));
        assert_eq!(detect_agent("sudo -E claude"), Some(Claude));
        assert_eq!(detect_agent("cd x && codex"), Some(Codex));
        assert_eq!(detect_agent("claude -p hi"), None);
        assert_eq!(detect_agent("codex --version"), None);
        assert_eq!(detect_agent("echo x | claude"), None);
    }

    #[test]
    fn unterminated_quotes_do_not_panic() {
        assert_eq!(detect_agent("claude \"abc"), Some(Claude));
        assert_eq!(detect_agent("claude 'abc"), Some(Claude));
        assert_eq!(detect_agent("claude \\"), Some(Claude));
        assert_eq!(detect_agent("\"claude"), Some(Claude));
    }
}
