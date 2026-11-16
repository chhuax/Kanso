//! 把配置里的 Shell 字段（一条命令行）解析成要启动的程序与参数（local-shell.md §1、§2）。

use crate::env::EnvSnapshot;
use crate::error::PtyError;

/// 运行平台。作为参数显式传入，让 `-l`、默认 shell、locale 等平台相关决策都能在任何
/// 开发机上做表格测试，而不是靠 `cfg!` 只测得到当前平台。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// macOS。
    MacOs,
    /// Linux。
    Linux,
    /// FreeBSD 等其他 Unix。
    Bsd,
    /// Windows。
    Windows,
}

impl Platform {
    /// 当前编译目标对应的平台。
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Bsd
        }
    }
}

/// 要启动的程序与参数（不含 argv\[0\]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// 程序名或路径；相对名按子进程环境里的 `PATH` 查找。
    pub program: String,
    /// 参数列表。
    pub args: Vec<String>,
}

/// 需要在 macOS 上自动加 `-l` 的已知 shell（取路径最后一段匹配，区分大小写）。
///
/// 只认列表内的程序：不认识的程序（如 `wsl.exe`、`python`、`ssh`）不一定接受 `-l`，不猜测。
const KNOWN_LOGIN_SHELLS: &[&str] = &[
    "bash", "csh", "dash", "fish", "ksh", "nu", "pwsh", "sh", "tcsh", "xonsh", "zsh",
];

/// 解析命令行。
///
/// - 空白命令行使用默认 shell：Unix 取 `$SHELL`（没有或为空就用 `/bin/bash`），
///   Windows 取 `COMSPEC`（没有就用 `powershell.exe`）。默认 shell 当作**单个程序名**，
///   不再拆分（路径里可以有空格）。
/// - 非空命令行按 shell 规则拆分；引号不闭合返回 [`PtyError::InvalidCommandLine`]。
/// - **只有**同时满足三条才追加 `-l`：平台是 macOS、最终只有程序名没有任何参数、
///   程序是已知 shell。原因见 local-shell.md §2：Dock 启动的应用继承 launchd 的极简环境，
///   只有 login shell 才会执行 `/etc/zprofile` 与 `~/.zprofile`（Homebrew 等 PATH 在那里）。
///   命令行带参数时完全按用户写的执行，`zsh +l` 就是用户明确不要 login shell 的写法。
pub fn resolve_command(
    command_line: &str,
    env: &EnvSnapshot,
    platform: Platform,
) -> Result<CommandSpec, PtyError> {
    let mut tokens = split_command_line(command_line, platform)?;
    if tokens.is_empty() {
        tokens.push(default_shell(env, platform));
    }
    let program = tokens.remove(0);
    if program.is_empty() {
        return Err(PtyError::EmptyProgram);
    }
    let mut args = tokens;
    if platform == Platform::MacOs && args.is_empty() && is_known_shell(&program) {
        args.push("-l".to_owned());
    }
    Ok(CommandSpec { program, args })
}

/// 默认 shell。环境变量为空串视同未设置：空的 `$SHELL` 启动不了任何东西。
fn default_shell(env: &EnvSnapshot, platform: Platform) -> String {
    let (var, fallback) = if platform == Platform::Windows {
        ("COMSPEC", "powershell.exe")
    } else {
        ("SHELL", "/bin/bash")
    };
    match env.get_str(var) {
        Some(v) if !v.trim().is_empty() => v.to_owned(),
        _ => fallback.to_owned(),
    }
}

fn is_known_shell(program: &str) -> bool {
    let base = program.rsplit('/').next().unwrap_or(program);
    KNOWN_LOGIN_SHELLS.contains(&base)
}

fn split_command_line(line: &str, platform: Platform) -> Result<Vec<String>, PtyError> {
    if line.trim().is_empty() {
        return Ok(Vec::new());
    }
    if platform == Platform::Windows {
        return Ok(split_windows(line));
    }
    shell_words::split(line).map_err(|e| PtyError::InvalidCommandLine(e.to_string()))
}

/// Windows 命令行拆分的简化版：按空白切分，双引号内的空白保留，引号本身去掉，
/// 不处理反斜杠转义（Windows 路径里的反斜杠必须原样保留，不能套用 POSIX shell 规则）。
///
/// TODO(M6)：Windows 支持阶段改成完整的 `CommandLineToArgvW` 规则（`\"`、连续反斜杠等）。
fn split_windows(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut has_token = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> EnvSnapshot {
        EnvSnapshot::from_pairs(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn resolve(line: &str, e: &EnvSnapshot, p: Platform) -> (String, Vec<String>) {
        let c = resolve_command(line, e, p).unwrap();
        (c.program, c.args)
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn login_flag_table() {
        let e = env(&[("SHELL", "/bin/zsh")]);
        let cases: &[(&str, Platform, &str, &[&str])] = &[
            // 空命令行：默认 shell，macOS 加 -l。
            ("", Platform::MacOs, "/bin/zsh", &["-l"]),
            ("   ", Platform::MacOs, "/bin/zsh", &["-l"]),
            // Linux 不加。
            ("", Platform::Linux, "/bin/zsh", &[]),
            ("zsh", Platform::Linux, "zsh", &[]),
            ("zsh", Platform::Bsd, "zsh", &[]),
            // 只有程序名、已知 shell：加。
            ("zsh", Platform::MacOs, "zsh", &["-l"]),
            ("bash", Platform::MacOs, "bash", &["-l"]),
            (
                "/usr/local/bin/fish",
                Platform::MacOs,
                "/usr/local/bin/fish",
                &["-l"],
            ),
            (
                "/opt/homebrew/bin/nu",
                Platform::MacOs,
                "/opt/homebrew/bin/nu",
                &["-l"],
            ),
            ("pwsh", Platform::MacOs, "pwsh", &["-l"]),
            // 带参数：完全按用户写的。
            ("zsh +l", Platform::MacOs, "zsh", &["+l"]),
            ("pwsh -NoLogo", Platform::MacOs, "pwsh", &["-NoLogo"]),
            (
                "bash -c 'echo hi'",
                Platform::MacOs,
                "bash",
                &["-c", "echo hi"],
            ),
            // 未知程序：不猜。
            ("python3", Platform::MacOs, "python3", &[]),
            ("wsl.exe", Platform::MacOs, "wsl.exe", &[]),
            ("/usr/bin/zshx", Platform::MacOs, "/usr/bin/zshx", &[]),
            // 取路径最后一段匹配：目录名像 shell 的不算。
            (
                "/opt/zsh/bin/tool",
                Platform::MacOs,
                "/opt/zsh/bin/tool",
                &[],
            ),
            // 引号里带空格的程序路径。
            (
                "'/Applications/My Shell/zsh'",
                Platform::MacOs,
                "/Applications/My Shell/zsh",
                &["-l"],
            ),
        ];
        for (line, plat, prog, a) in cases {
            assert_eq!(
                resolve(line, &e, *plat),
                (prog.to_string(), args(a)),
                "{line:?} {plat:?}"
            );
        }
    }

    #[test]
    fn default_shell_fallbacks() {
        let none = env(&[]);
        assert_eq!(
            resolve("", &none, Platform::Linux),
            ("/bin/bash".into(), vec![])
        );
        assert_eq!(
            resolve("", &none, Platform::MacOs),
            ("/bin/bash".into(), args(&["-l"]))
        );
        // 空 $SHELL 视同未设置。
        let empty = env(&[("SHELL", "")]);
        assert_eq!(resolve("", &empty, Platform::Linux).0, "/bin/bash");
        // 默认 shell 路径含空格时不拆分。
        let sp = env(&[("SHELL", "/opt/my shells/zsh")]);
        assert_eq!(
            resolve("", &sp, Platform::Linux),
            ("/opt/my shells/zsh".into(), vec![])
        );
        // Windows。
        assert_eq!(
            resolve("", &none, Platform::Windows),
            ("powershell.exe".into(), vec![])
        );
        let cs = env(&[("COMSPEC", "C:\\Windows\\System32\\cmd.exe")]);
        assert_eq!(
            resolve("", &cs, Platform::Windows),
            ("C:\\Windows\\System32\\cmd.exe".into(), vec![])
        );
    }

    #[test]
    fn windows_splitting_keeps_backslashes() {
        let e = env(&[]);
        assert_eq!(
            resolve("wsl.exe -d Ubuntu", &e, Platform::Windows),
            ("wsl.exe".into(), args(&["-d", "Ubuntu"]))
        );
        assert_eq!(
            resolve(
                "\"C:\\Program Files\\Git\\bin\\bash.exe\" --login",
                &e,
                Platform::Windows
            ),
            (
                "C:\\Program Files\\Git\\bin\\bash.exe".into(),
                args(&["--login"])
            )
        );
        // Windows 上不加 -l。
        assert_eq!(
            resolve("bash", &e, Platform::Windows).1,
            Vec::<String>::new()
        );
    }

    #[test]
    fn errors() {
        let e = env(&[]);
        assert!(matches!(
            resolve_command("zsh -c 'unterminated", &e, Platform::MacOs),
            Err(PtyError::InvalidCommandLine(_))
        ));
        assert!(matches!(
            resolve_command("\"\"", &e, Platform::MacOs),
            Err(PtyError::EmptyProgram)
        ));
    }
}
