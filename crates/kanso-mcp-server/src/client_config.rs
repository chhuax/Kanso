//! 设置页"复制 Claude Code 配置""复制 Codex 配置"生成的内容（spec/mcp.md §1、§1.1）。
//!
//! 生成的是一条终端命令（两者都提供 `mcp add` 子命令），用户粘贴到终端执行即可，
//! 不需要手改配置文件。命令格式取自两个 CLI 的 `mcp add --help`：
//! - `claude mcp add [options] <name> <commandOrUrl> [args...]`，`-e KEY=value`，`-s user`；
//! - `codex mcp add [OPTIONS] <NAME> -- <COMMAND>...`，`--env KEY=VALUE`。

/// 生成哪种客户端的配置命令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpClientKind {
    /// Claude Code。
    ClaudeCode,
    /// Codex CLI。
    Codex,
}

/// 注册到客户端里的服务名。
const SERVER_NAME: &str = "kanso";
/// 令牌的环境变量名，`kanso-mcp` 从这里读取（architecture §16.1）。
const TOKEN_ENV: &str = "KANSO_MCP_TOKEN";

/// 生成配置命令。`helper_path` 是 App 包内 `kanso-mcp` 的绝对路径，`token` 是客户端令牌。
///
/// 命令**以一个空格开头**：命令里带着令牌，而 Kanso 自己的命令历史不记录以空格开头的命令
/// （terminal.md §5.2），在 Kanso 的终端里粘贴执行时令牌不会进入 Kanso 的历史与补全。不要"顺手"去掉这个空格。
///
/// 它**挡不住 shell 自己的历史**：macOS 默认的 zsh 没有开启 `HIST_IGNORE_SPACE`，bash 默认也没有
/// `HISTCONTROL=ignorespace`，令牌仍会写进 `~/.zsh_history` 等文件。设置页在复制按钮旁要说明这一点。
pub fn client_config(kind: McpClientKind, helper_path: &str, token: &str) -> String {
    let env = shell_quote(&format!("{TOKEN_ENV}={token}"));
    let path = shell_quote(helper_path);
    command(kind, &env, &path, "--")
}

/// Windows 设置页供用户在 PowerShell 里运行的接入命令。
pub fn powershell_client_config(kind: McpClientKind, helper_path: &str, token: &str) -> String {
    let env = powershell_quote(&format!("{TOKEN_ENV}={token}"));
    let path = powershell_quote(helper_path);
    // 引号使 `--` 原样传给入口，即使 PATH 上找到的是 npm 安装的 PowerShell 脚本。
    command(kind, &env, &path, "'--'")
}

fn powershell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        quoted.push(ch);
        // PowerShell 把弯单引号也当作引号，路径中的每个单引号都要重复一次。
        if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn command(kind: McpClientKind, env: &str, path: &str, separator: &str) -> String {
    match kind {
        // 用户级（`-s user`）：在任何目录启动 Claude Code 都能用；默认的 local 只对当前目录生效。
        McpClientKind::ClaudeCode => {
            format!(" claude mcp add {SERVER_NAME} -s user -e {env} {separator} {path}")
        }
        McpClientKind::Codex => {
            format!(" codex mcp add {SERVER_NAME} --env {env} {separator} {path}")
        }
    }
}

/// POSIX shell 单引号转义：整体放进单引号，内部的 `'` 写成 `'\''`。
/// 路径可能含空格（例如 App 放在"应用程序 副本"目录里），令牌目前只有字母数字，但也一并转义。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_cli_syntax_and_start_with_space() {
        let path = "/Applications/Kanso.app/Contents/MacOS/kanso-mcp";
        assert_eq!(
            client_config(McpClientKind::ClaudeCode, path, "abc123"),
            " claude mcp add kanso -s user -e 'KANSO_MCP_TOKEN=abc123' -- \
             '/Applications/Kanso.app/Contents/MacOS/kanso-mcp'"
        );
        assert_eq!(
            client_config(McpClientKind::Codex, path, "abc123"),
            " codex mcp add kanso --env 'KANSO_MCP_TOKEN=abc123' -- \
             '/Applications/Kanso.app/Contents/MacOS/kanso-mcp'"
        );
    }

    #[test]
    fn quoting_survives_spaces_and_single_quotes() {
        let cmd = client_config(McpClientKind::Codex, "/Apps/Kan'so 2.app/kanso-mcp", "t");
        assert!(
            cmd.ends_with(r"-- '/Apps/Kan'\''so 2.app/kanso-mcp'"),
            "{cmd}"
        );
    }

    #[test]
    fn powershell_quotes_windows_paths_and_literal_separator() {
        let path = r"C:\程序 文件\Kan'so’s $app\kanso-mcp.exe";
        assert_eq!(
            powershell_client_config(McpClientKind::ClaudeCode, path, "abc123"),
            r" claude mcp add kanso -s user -e 'KANSO_MCP_TOKEN=abc123' '--' 'C:\程序 文件\Kan''so’’s $app\kanso-mcp.exe'"
        );
        assert_eq!(
            powershell_client_config(McpClientKind::Codex, path, "abc123"),
            r" codex mcp add kanso --env 'KANSO_MCP_TOKEN=abc123' '--' 'C:\程序 文件\Kan''so’’s $app\kanso-mcp.exe'"
        );
    }
}
