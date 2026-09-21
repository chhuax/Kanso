//! 本地 zsh 的轻量启动 shim，用于目录、Git 分支和命令块间距。
//!
//! shim 通过 `ZDOTDIR` 加载用户原有启动文件，再追加 ZenTerm 自己的
//! `precmd`。用户目录不会被修改，删除 shim 目录也不会留下配置残留。

use std::fs;
use std::path::{Path, PathBuf};

use crate::store;

/// zsh's own startup files, in the order it reads them. Each gets a shim that
/// sources the user's copy, so pointing `ZDOTDIR` here changes nothing but
/// what is added at the end.
const ZSH_STARTUP: &[&str] = &[".zshenv", ".zprofile", ".zshrc", ".zlogin", ".zlogout"];

/// 最后加载的 hook：在命令符前显示目录与分支，并保留 OSC 133 命令块标记。
const ZSH_HOOK: &str = r#"# ZenTerm 的本地提示符：目录后紧跟当前 Git 分支。
zenterm_branch() {
  setopt localoptions extendedglob
  local dir=$1 head line target
  # 直接读取 HEAD，避免每次显示 prompt 都启动 Git 进程；worktree 的
  # .git 文件相对于其所在目录解析，与 git.rs 的分支读取规则保持一致。
  while true; do
    if [[ -d $dir/.git ]]; then
      head=$dir/.git/HEAD
      break
    elif [[ -f $dir/.git ]]; then
      line=$(<"$dir/.git") || return 0
      [[ $line == 'gitdir:'* ]] || return 0
      target=${line#gitdir:}
      target=${target##[[:space:]]##}
      target=${target%%[[:space:]]##}
      [[ -n $target ]] || return 0
      [[ $target == /* ]] || target=$dir/$target
      head=$target/HEAD
      break
    fi
    [[ $dir == / ]] && return 0
    dir=${dir:h}
  done
  [[ -f $head ]] || return 0
  line=$(<"$head") || return 0
  line=${line##[[:space:]]##}
  line=${line%%[[:space:]]##}
  if [[ $line == 'ref:'* ]]; then
    local reference=${line#ref:}
    reference=${reference##[[:space:]]##}
    reference=${reference%%[[:space:]]##}
    [[ -n $reference ]] && print -r -- "${reference#refs/heads/}"
    return 0
  fi
  # detached HEAD 与标签栏一样显示七位提交号，不把未知内容当成分支。
  [[ ${line[1,7]} == [0-9a-fA-F](#c7) ]] && print -r -- "${line[1,7]}"
}

zenterm_precmd() {
  setopt localoptions extendedglob

  local where
  where=${(%):-%~}
  # 长目录保留最有辨识度的尾部完整路径段，避免把命令符推得过远。
  local budget=36
  if (( ${#where} > budget )); then
    local -a parts
    parts=(${(s:/:)where})
    local tail=${parts[-1]} i candidate
    for (( i = ${#parts} - 1; i >= 1; i-- )); do
      candidate="${parts[i]}/$tail"
      (( ${#candidate} + 4 > budget )) && break
      tail=$candidate
    done
    where=".../$tail"
  fi

  # 目录由 ZenTerm 统一放在命令符前，因此移除模板中的目录转义；`$PWD`
  # 这类任意 shell 表达式不能安全重写，保留原配置且不再重复注入目录。
  ZENTERM_BASE=$ZENTERM_USER_PROMPT
  local directory_is_shown=no
  [[ $ZENTERM_BASE == *PWD* || $ZENTERM_BASE == *pwd* ]] &&
    directory_is_shown=yes

  # `~` 在 zsh pattern 中是排除运算符，放进字符类后才表示字面量；
  # `[0-9]#` 同时覆盖 `%~`、`%1~`、`%2d` 等目录转义。
  local escape='%[0-9]#[/d~]'
  ZENTERM_BASE=${ZENTERM_BASE//${~escape}/}
  # 用户名和与其绑定的主机名保持隐藏，终端标签已经标明会话身份。
  ZENTERM_BASE=${ZENTERM_BASE//'%n'/}
  ZENTERM_BASE=${ZENTERM_BASE//'%N'/}
  [[ $ZENTERM_BASE == *'@%m'* ]] && ZENTERM_BASE=${ZENTERM_BASE//'@%m'/}
  [[ $ZENTERM_BASE == *'@%M'* ]] && ZENTERM_BASE=${ZENTERM_BASE//'@%M'/}
  while [[ $ZENTERM_BASE == *'  '* ]]; do
    ZENTERM_BASE=${ZENTERM_BASE//'  '/' '}
  done
  # 只清理开头空格；命令符后的尾随空格仍是光标与 prompt 的正常间距。
  while [[ $ZENTERM_BASE == ' '* ]]; do
    ZENTERM_BASE=${ZENTERM_BASE# }
  done
  local text=$'%{\e[38;5;252m%}' green=$'%{\e[38;5;114m%}' off=$'%{\e[0m%}'
  local branch=$(zenterm_branch "$PWD") branch_label branch_suffix=""
  # 分支名是数据：转义 prompt 的百分号；启用 promptsubst 时通过变量引用
  # 延后插入，避免分支名中的命令替换被二次执行。
  ZENTERM_PROMPT_BRANCH=${branch//\%/%%}
  branch_label=$ZENTERM_PROMPT_BRANCH
  [[ -o promptsubst ]] && branch_label='${ZENTERM_PROMPT_BRANCH}'
  [[ -n $branch ]] && branch_suffix="${green} ${branch_label}${off} "

  if [[ $directory_is_shown == yes ]]; then
    PROMPT=$ZENTERM_BASE
    if [[ -n $branch && $PROMPT == *'%#'* ]]; then
      PROMPT=${PROMPT/'%#'/"${branch_suffix}%#"}
    elif [[ -n $branch ]]; then
      # 任意用户表达式不能安全拆分；没有标准命令符时沿用前置分支的行为。
      PROMPT="${branch_suffix}${PROMPT}"
    fi
  else
    PROMPT="${text}${where}${off} ${branch_suffix}${ZENTERM_BASE}"
  fi

  # 后续 prompt 保留两行命令块间距，前端把分割线画在正中；首个 prompt
  # 不写入空行，顶部间距由终端容器提供，避免时间线和缓冲区从第二行开始。
  local mark
  if [[ -n ${ZENTERM_PROMPT_SEEN-} ]]; then
    mark=$'%{\e]133;A;zenterm-spacer\a%}\n\n'
  else
    ZENTERM_PROMPT_SEEN=1
    mark=$'%{\e]133;A;zenterm-initial\a%}'
  fi
  PROMPT="${mark}${PROMPT}"
}

ZENTERM_USER_PROMPT=${PROMPT-'%m %~ %# '}
autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook precmd zenterm_precmd
"#;

/// Where the user's own startup files are: their `ZDOTDIR` if they set one,
/// the home directory otherwise.
fn user_dir() -> PathBuf {
    std::env::var_os("ZDOTDIR")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
        .or_else(dirs::home_dir)
        .unwrap_or_default()
}

/// One shim file: the user's own, and nothing else. Written for every startup
/// file zsh reads, so an earlier or later one is never silently skipped.
fn forwarding(file: &str) -> String {
    format!(
        r#"# ZenTerm's shim for {file}: the user's own file, and nothing else.
[[ -n $ZENTERM_USER_ZDOTDIR ]] || ZENTERM_USER_ZDOTDIR=$HOME
[[ -f $ZENTERM_USER_ZDOTDIR/{file} ]] && source $ZENTERM_USER_ZDOTDIR/{file}
"#
    )
}

/// The `.zshrc` shim: the user's own, the history file put back where it was,
/// then the hook.
const ZSH_RC: &str = r#"# ZenTerm's shim for .zshrc: the user's own file, plus the line above the
# prompt. zsh keeps its history file under `$ZDOTDIR`, which is this
# directory while a ZenTerm shell runs, so it is pointed back at the user's.
[[ -n $ZENTERM_USER_ZDOTDIR ]] || ZENTERM_USER_ZDOTDIR=$HOME
case $HISTFILE in
  $ZDOTDIR/*) HISTFILE=$ZENTERM_USER_ZDOTDIR/.zsh_history ;;
esac
[[ -f $ZENTERM_USER_ZDOTDIR/.zshrc ]] && source $ZENTERM_USER_ZDOTDIR/.zshrc
source $ZDOTDIR/zenterm.zsh
"#;

/// Writes the shim beside the store and answers the directory to point
/// `ZDOTDIR` at. None when it cannot be written: a shell then starts exactly
/// as it did before, without the line.
pub fn ensure_zsh_shim() -> Option<PathBuf> {
    ensure_zsh_shim_in(&store::data_dir())
}

/// The writing half, against any directory, so a test can use its own.
fn ensure_zsh_shim_in(root: &Path) -> Option<PathBuf> {
    let dir = root.join("shell").join("zsh");
    fs::create_dir_all(&dir).ok()?;
    for file in ZSH_STARTUP {
        let contents = if *file == ".zshrc" {
            ZSH_RC.to_string()
        } else {
            forwarding(file)
        };
        write(&dir.join(file), &contents)?;
    }
    write(&dir.join("zenterm.zsh"), ZSH_HOOK)?;
    Some(dir)
}

/// The environment a shimmed shell needs: where zsh is to look, and where the
/// user's own files really are.
pub fn zsh_env() -> Option<(PathBuf, PathBuf)> {
    let dir = ensure_zsh_shim()?;
    Some((dir, user_dir()))
}

fn write(path: &Path, contents: &str) -> Option<()> {
    // Rewriting an unchanged file would churn its mtime for nothing.
    if fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Some(());
    }
    fs::write(path, contents).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct HookFixture {
        root: PathBuf,
        hook: PathBuf,
        repo: PathBuf,
    }

    impl HookFixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("zenterm-prompt-{}", uuid::Uuid::new_v4()));
            let hook = ensure_zsh_shim_in(&root).unwrap().join("zenterm.zsh");
            let repo = root.join("project");
            fs::create_dir_all(repo.join(".git")).unwrap();
            fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature/parser\n").unwrap();
            Self { root, hook, repo }
        }

        fn run(&self, dir: &Path, script: &str) -> String {
            let output = Command::new("zsh")
                .args(["-f", "-c"])
                .arg(format!("source \"$1\"\n{script}"))
                .arg("zenterm-prompt-test")
                .arg(&self.hook)
                .current_dir(dir)
                .output()
                .expect("run zsh");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        }
    }

    impl Drop for HookFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn has_zsh() -> bool {
        if Command::new("zsh").arg("--version").output().is_ok() {
            true
        } else {
            eprintln!("跳过提示符测试：此平台未安装 zsh");
            false
        }
    }

    #[test]
    fn branch_follows_parent_worktree_and_detached_head() {
        if !has_zsh() {
            return;
        }
        let fixture = HookFixture::new();
        let deep = fixture.repo.join("src/parser");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            fixture.run(&deep, "zenterm_branch \"$PWD\""),
            "feature/parser\n"
        );

        let worktree = fixture.root.join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: ../project/.git\n").unwrap();
        assert_eq!(
            fixture.run(&worktree, "zenterm_branch \"$PWD\""),
            "feature/parser\n"
        );

        fs::write(
            fixture.repo.join(".git/HEAD"),
            "a1b2c3d4e5f67890123456789012345678901234\n",
        )
        .unwrap();
        assert_eq!(
            fixture.run(&worktree, "zenterm_branch \"$PWD\""),
            "a1b2c3d\n"
        );
    }

    #[test]
    fn prompt_places_branch_after_directory_and_keeps_command_spacing() {
        if !has_zsh() {
            return;
        }
        let fixture = HookFixture::new();
        let script = "ZENTERM_USER_PROMPT='%n %~ %# '; zenterm_precmd; print -rn -- \"$PROMPT\"";
        let prompt = fixture.run(&fixture.repo, script);
        let path_position = prompt.find("/project").expect("directory in prompt");
        let branch_position = prompt.find(" feature/parser").expect("branch in prompt");
        assert!(path_position < branch_position);
        assert!(branch_position < prompt.find("%#").unwrap());
        assert!(prompt.starts_with("%{\u{1b}]133;A\u{7}%}"));
        assert!(prompt.ends_with("%# "));

        let next = fixture.run(&fixture.repo, &format!("ZENTERM_PROMPT_SEEN=1; {script}"));
        assert!(next.starts_with("%{\u{1b}]133;A;zenterm-spacer\u{7}%}\n\n"));
        assert!(next.contains(" feature/parser"));
    }

    #[test]
    fn prompt_refreshes_branch_and_clears_it_after_leaving_repository() {
        if !has_zsh() {
            return;
        }
        let fixture = HookFixture::new();
        let prompt = fixture.run(
            &fixture.repo,
            r#"
ZENTERM_USER_PROMPT='%~ %# '
zenterm_precmd
[[ $PROMPT == *'feature/parser'* ]] || exit 1
print -r -- 'ref: refs/heads/another-branch' > .git/HEAD
zenterm_precmd
[[ $PROMPT == *'another-branch'* ]] || exit 2
cd ..
zenterm_precmd
print -rn -- "$PROMPT"
"#,
        );
        assert!(!prompt.contains(""));
        assert!(prompt.ends_with("%# "));
    }

    #[test]
    fn custom_directory_prompt_keeps_branch_names_literal() {
        if !has_zsh() {
            return;
        }
        let fixture = HookFixture::new();
        let branch = "topic/%F{red}$(false)";
        fs::write(
            fixture.repo.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
        let prompt = fixture.run(
            &fixture.repo,
            r#"
setopt promptsubst
ZENTERM_USER_PROMPT='$PWD %# '
zenterm_precmd
print -Prn -- "$PROMPT"
"#,
        );
        assert!(prompt.contains(&format!(" {branch}")), "{prompt:?}");
        assert!(prompt.find("/project").unwrap() < prompt.find("").unwrap());
    }

    #[test]
    fn every_startup_file_forwards_to_the_users_own() {
        for file in ZSH_STARTUP {
            let shim = forwarding(file);
            assert!(
                shim.contains(&format!("$ZENTERM_USER_ZDOTDIR/{file}")),
                "{file} does not source the user's copy"
            );
        }
        // `.zshrc` is the one that also carries the hook, and it puts the
        // history file back: zsh would otherwise keep it in our shim.
        assert!(ZSH_RC.contains("source $ZENTERM_USER_ZDOTDIR/.zshrc"));
        assert!(ZSH_RC.contains("HISTFILE=$ZENTERM_USER_ZDOTDIR/.zsh_history"));
        assert!(ZSH_RC.contains("source $ZDOTDIR/zenterm.zsh"));
        assert!(ZSH_HOOK.contains("add-zsh-hook precmd"));
    }

    #[test]
    fn the_shim_covers_every_file_zsh_reads() {
        let root = std::env::temp_dir().join(format!("zenterm-shim-{}", uuid::Uuid::new_v4()));
        let dir = ensure_zsh_shim_in(&root).expect("write the shim");
        for file in ZSH_STARTUP {
            assert!(dir.join(file).is_file(), "{file} was not written");
        }
        assert!(dir.join("zenterm.zsh").is_file());
        std::fs::remove_dir_all(root).ok();
    }
}
