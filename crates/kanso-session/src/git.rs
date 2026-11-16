//! 读取当前目录所在 git 仓库的分支名，不启动 git 进程（local-shell.md §5.2）。
//!
//! 规则与 zsh shim（`kanso-pty` 的 `shim/zsh/zshrc.zsh` 里的 `__kanso_git_branch`）保持一致，
//! 让标签栏和提示符显示同一个结果。每次提示符出现（OSC 7）都会调用一次，所以只读几个很小的文件。

use std::fs;
use std::path::{Path, PathBuf};

/// 从 `cwd` 逐级向上找 `.git`，返回分支名；detached HEAD 返回提交号的前 7 位；
/// 不在仓库内或内容不认识返回 `None`。
///
/// - `.git` 是目录：读其中的 `HEAD`。
/// - `.git` 是文件（worktree / submodule）：内容为 `gitdir: <路径>`，相对路径以 `.git` 文件所在
///   目录为基准，读那里的 `HEAD`；没有 `gitdir:` 前缀视为不认识，**不再继续向上找**（与 shim 一致）。
/// - `HEAD` 为 `ref: refs/heads/x` 时返回 `x`；`ref:` 指向别处（如 `refs/remotes/...`）时返回
///   去掉 `ref: ` 前缀后的原文；40 或 64 位十六进制是 detached；其余不显示。
/// - 分支名是数据，可能含控制字符：全部丢弃，防止它被当成终端转义序列显示。
pub fn git_branch(cwd: &Path) -> Option<String> {
    let mut dir: PathBuf = cwd.to_path_buf();
    let head_path = loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            break dot_git.join("HEAD");
        }
        if dot_git.is_file() {
            let content = fs::read_to_string(&dot_git).ok()?;
            let line = first_line(&content);
            let gitdir = line.strip_prefix("gitdir: ")?;
            let gitdir = Path::new(gitdir);
            let resolved = if gitdir.is_absolute() {
                gitdir.to_path_buf()
            } else {
                dir.join(gitdir)
            };
            break resolved.join("HEAD");
        }
        if !dir.pop() {
            return None;
        }
    };
    let content = fs::read_to_string(head_path).ok()?;
    let line = first_line(&content);
    if let Some(r) = line.strip_prefix("ref: ") {
        let name = r.strip_prefix("refs/heads/").unwrap_or(r);
        let name: String = name.chars().filter(|c| !c.is_control()).collect();
        return (!name.is_empty()).then_some(name);
    }
    let hex = (line.len() == 40 || line.len() == 64) && line.bytes().all(|b| b.is_ascii_hexdigit());
    hex.then(|| line[..7].to_owned())
}

/// 第一行，去掉行尾空白（含 `\r`）。
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn plain_repo_and_subdirectory() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join(".git/HEAD"), "ref: refs/heads/feature/x\n");
        fs::create_dir_all(t.path().join("a/b")).unwrap();
        assert_eq!(git_branch(t.path()).as_deref(), Some("feature/x"));
        assert_eq!(
            git_branch(&t.path().join("a/b")).as_deref(),
            Some("feature/x")
        );
    }

    #[test]
    fn detached_head_shows_seven_hex() {
        let t = tempfile::tempdir().unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        write(&t.path().join(".git/HEAD"), &format!("{sha}\n"));
        assert_eq!(git_branch(t.path()).as_deref(), Some("0123456"));
        // 长度不对或含非十六进制：不显示。
        write(&t.path().join(".git/HEAD"), "0123456789\n");
        assert_eq!(git_branch(t.path()), None);
        write(&t.path().join(".git/HEAD"), &format!("{}g\n", &sha[..39]));
        assert_eq!(git_branch(t.path()), None);
    }

    #[test]
    fn gitdir_file_absolute_and_relative() {
        let t = tempfile::tempdir().unwrap();
        write(
            &t.path().join("real/wt/HEAD"),
            "ref: refs/heads/wt-branch\n",
        );
        // 相对路径以 .git 文件所在目录为基准。
        write(&t.path().join("proj/.git"), "gitdir: ../real/wt\n");
        assert_eq!(
            git_branch(&t.path().join("proj")).as_deref(),
            Some("wt-branch")
        );
        let abs = t.path().join("real/wt");
        write(
            &t.path().join("proj2/.git"),
            &format!("gitdir: {}\n", abs.display()),
        );
        assert_eq!(
            git_branch(&t.path().join("proj2")).as_deref(),
            Some("wt-branch")
        );
        // 没有 gitdir: 前缀：不认识，也不继续向上找（上层有一个能认的仓库也不用）。
        write(&t.path().join(".git/HEAD"), "ref: refs/heads/outer\n");
        write(&t.path().join("proj3/.git"), "garbage\n");
        assert_eq!(git_branch(&t.path().join("proj3")), None);
    }

    #[test]
    fn control_characters_are_dropped() {
        let t = tempfile::tempdir().unwrap();
        write(
            &t.path().join(".git/HEAD"),
            "ref: refs/heads/a\u{1b}[31mb\n",
        );
        assert_eq!(git_branch(t.path()).as_deref(), Some("a[31mb"));
    }
}
