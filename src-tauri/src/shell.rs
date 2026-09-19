//! The line above a local shell's prompt, saying where the shell is.
//!
//! A shell draws its own prompt, so the only way to put a line there is to
//! give the shell one — which is what every terminal that does this does, and
//! what Starship is. This writes a small `ZDOTDIR` shim instead: zsh looks for
//! its startup files in that directory rather than in the home directory, each
//! shim file forwards to the user's own copy, and one of them adds a `precmd`
//! that prints the directory and the branch above the prompt.
//!
//! Nothing in the user's home is touched, and only the shells ZenTerm starts
//! are given the variable. Delete the directory and nothing is left behind.

use std::fs;
use std::path::{Path, PathBuf};

use crate::store;

/// zsh's own startup files, in the order it reads them. Each gets a shim that
/// sources the user's copy, so pointing `ZDOTDIR` here changes nothing but
/// what is added at the end.
const ZSH_STARTUP: &[&str] = &[".zshenv", ".zprofile", ".zshrc", ".zlogin", ".zlogout"];

/// The hook, sourced last: the last three path components and the branch, the
/// shape the coding CLIs print. Pure zsh builtins — finding the repository
/// walks up for a `.git` and reads `HEAD` rather than running `git`, which on
/// a machine without the developer tools raises a system prompt.
const ZSH_HOOK: &str = r#"# ZenTerm: where the shell is, as the chips the coding CLIs print, after the
# user's name on the prompt line. The prompt's own directory escape is taken
# out and shown in the chip instead, so the directory is said once; the chips
# go into `PROMPT` rather than being printed above it so zsh still knows how
# wide its prompt is (`%{...%}` marks the colour codes as taking no room).
zenterm_branch() {
  local dir=$1 head line target
  # `##` (one or more) is off by default in zsh, and trimming the space after
  # `gitdir:` needs it; the setting is local to this function.
  setopt localoptions extendedglob
  while true; do
    if [[ -d $dir/.git ]]; then
      head=$dir/.git/HEAD
      break
    elif [[ -f $dir/.git ]]; then
      line=$(<"$dir/.git") || return 0
      line=${line#gitdir:}
      line=${line##[[:space:]]##}
      line=${line%%[[:space:]]##}
      target=${line/#\~/$HOME}
      [[ $target == /* ]] || target=$dir/$target
      [[ -f $target/HEAD ]] || return 0
      head=$target/HEAD
      break
    fi
    [[ $dir == / ]] && return 0
    dir=${dir:h}
  done
  line=$(<"$head") || return 0
  line=${line%%$'\n'}
  [[ $line == 'ref: refs/heads/'* ]] || return 0
  print -r -- "${line#ref: refs/heads/}"
}

zenterm_precmd() {
  setopt localoptions extendedglob
  # Not `path`: that is zsh's array twin of `PATH`, so a scalar assigned to it
  # lands in an array and `${#path}` counts elements rather than characters.
  local where branch
  where=${(%):-%~}
  # A path too long for the line keeps its end — that is the half that says
  # where you are — whole components at a time, with `...` where the rest of
  # it went. A short path is left exactly as it is.
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
  branch=$(zenterm_branch $PWD)

  # A prompt that says its directory itself keeps it, and the chip stays off
  # the line: the same fact twice is worse than a short prompt. Read from the
  # template rather than from what it prints, because a numbered escape such
  # as `%1~` prints the last component alone, which is not the path it stands
  # for — `%~`, `%/`, `%d` and their numbered forms all count, and so does a
  # prompt that spells the directory out as `$PWD`.
  ZENTERM_BASE=$ZENTERM_USER_PROMPT
  local directory_is_shown=no
  [[ $ZENTERM_BASE == *$'%'[0-9]#[~/d]* || $ZENTERM_BASE == *PWD* || $ZENTERM_BASE == *pwd* ]] &&
    directory_is_shown=yes

  # `~` is zsh's pattern-exclusion operator, so each of these is quoted to be
  # taken as the characters it is; the space the escape sat in goes with it,
  # and the run of spaces that can leave is closed up again.
  ZENTERM_BASE=${ZENTERM_BASE//"%~"/}
  ZENTERM_BASE=${ZENTERM_BASE//"%\/"/}
  ZENTERM_BASE=${ZENTERM_BASE//"%d"/}
  # The user's name goes too: in a shell started here it is always the same
  # name, and the chip says the part that changes. The host it was joined to
  # goes with it — `%n@%m` would otherwise leave a bare `@` on the line, and
  # which machine the shell is on is what the window is for.
  ZENTERM_BASE=${ZENTERM_BASE//'%n'/}
  ZENTERM_BASE=${ZENTERM_BASE//'%N'/}
  [[ $ZENTERM_BASE == *'@%m'* ]] && ZENTERM_BASE=${ZENTERM_BASE//'@%m'/}
  [[ $ZENTERM_BASE == *'@%M'* ]] && ZENTERM_BASE=${ZENTERM_BASE//'@%M'/}
  while [[ $ZENTERM_BASE == *'  '* ]]; do
    ZENTERM_BASE=${ZENTERM_BASE//'  '/' '}
  done
  # Only the front: the trailing space is where the cursor sits after the
  # sign, and the prompt had one.
  while [[ $ZENTERM_BASE == ' '* ]]; do
    ZENTERM_BASE=${ZENTERM_BASE# }
  done

  local chip=$'%{\e[48;5;236m%}'
  local text=$'%{\e[38;5;252m%}'
  local green=$'%{\e[38;5;114m%}'
  local off=$'%{\e[0m%}'
  local fork=""

  ZENTERM_CHIPS=""
  [[ $directory_is_shown == no ]] &&
    ZENTERM_CHIPS="${chip} ${text}${where} ${off}"
  if [[ -n $branch ]]; then
    [[ -n $ZENTERM_CHIPS ]] && ZENTERM_CHIPS+=" "
    ZENTERM_CHIPS+="${chip} ${green}${fork} ${branch} ${off}"
  fi

  # After the user's name, where a prompt usually has the directory; in front
  # of the prompt for one that starts with something else.
  if [[ -z $ZENTERM_CHIPS ]]; then
    PROMPT=$ZENTERM_BASE
  elif [[ $ZENTERM_BASE == '%'[nNmM]* ]]; then
    PROMPT="${ZENTERM_BASE[1,2]} ${ZENTERM_CHIPS}${ZENTERM_BASE[3,-1]}"
  else
    PROMPT="${ZENTERM_CHIPS} ${ZENTERM_BASE}"
  fi

  # The prompt line begins here. The terminal brackets commands with this
  # (OSC 133), so a command's end is told rather than guessed from the shape
  # of a prompt the chips have changed.
  local mark=$'%{\e]133;A\a%}'
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

    /// The hook's own half, run by a real zsh: reading `HEAD` by hand is the
    /// part most likely to be wrong, and no amount of Rust testing sees it.
    /// Skipped where there is no zsh to run (the suite also runs on machines
    /// that have none).
    #[test]
    fn the_hook_reads_a_branch_and_prints_the_line() {
        use std::process::Command;

        let root = std::env::temp_dir().join(format!("zenterm-hook-{}", uuid::Uuid::new_v4()));
        let shim = ensure_zsh_shim_in(&root).expect("write the shim");
        let repo = root.join("work").join("api");
        fs::create_dir_all(repo.join(".git")).expect("create the repository");
        fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();

        let run = |dir: &Path, script: &str| -> Option<String> {
            let output = Command::new("zsh")
                .arg("-c")
                .arg(format!(
                    "source {}/zenterm.zsh\n{script}",
                    shim.display()
                ))
                .current_dir(dir)
                .output()
                .ok()?;
            Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
        };

        let Some(branch) = run(&repo, "zenterm_branch $PWD") else {
            eprintln!("no zsh on this machine; the hook itself is untested here");
            fs::remove_dir_all(root).ok();
            return;
        };
        assert_eq!(branch, "feature/x");

        // A shell inside the tree is still on the same branch.
        let deep = repo.join("src").join("parser");
        fs::create_dir_all(&deep).expect("create a subtree");
        assert_eq!(
            run(&deep, "zenterm_branch $PWD").as_deref(),
            Some("feature/x")
        );

        // A prompt that prints its own directory keeps it: the chip would be
        // the same fact a second time, so the prompt keeps its directory and
        // its sign, and the chips are left with the branch.
        // The marker after the prompt keeps a trailing space from being
        // trimmed away with the newline by the helper above.
        let with_dir = run(
            &deep,
            "PROMPT='%n %~ %# '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -rn -- \"$PROMPT\"; print -r -- '|'",
        )
        .expect("zsh");
        assert!(
            !with_dir.contains(".../"),
            "the directory is on the line twice: {with_dir}"
        );
        assert!(with_dir.contains("feature/x"), "no branch in: {with_dir}");
        assert!(
            !with_dir.contains("%~"),
            "the prompt kept a directory the chip alone should say: {with_dir}"
        );
        assert!(
            !with_dir.contains("%n"),
            "the user's name is still on the line: {with_dir}"
        );
        assert!(with_dir.contains("%#"), "the sign is gone: {with_dir}");
        assert!(
            with_dir.ends_with("%# |"),
            "the cursor has no room after the sign: {with_dir:?}"
        );
        assert!(
            with_dir.contains("\u{1b}]133;A\u{7}"),
            "no prompt-start marker: {with_dir}"
        );

        // A prompt that names its directory as `$PWD` cannot be edited safely;
        // it keeps its own and goes without the chip rather than saying it twice.
        let literal = run(
            &deep,
            "PROMPT='%n $PWD %# '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -r -- \"$PROMPT\"",
        )
        .expect("zsh");
        assert!(
            !literal.contains(".../"),
            "the directory is on the line twice: {literal}"
        );
        assert!(literal.contains("feature/x"), "no branch in: {literal}");

        // A numbered escape prints the last component (`%1~`) or a tail of the
        // path (`%2~`), not the path itself. The chip would repeat what the
        // line already says, so the prompt is left to say it and the chip
        // carries the branch alone.
        let tail = run(
            &deep,
            "PROMPT='%n@%m %1~ %# '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -r -- \"$PROMPT\"",
        )
        .expect("zsh");
        assert!(
            !tail.contains(".../"),
            "the directory is on the line twice: {tail}"
        );
        assert!(tail.contains("feature/x"), "no branch in: {tail}");
        assert!(
            tail.contains("%1~"),
            "the prompt lost its own directory: {tail}"
        );
        assert!(
            !tail.contains("@%m") && !tail.contains("@%M"),
            "the host stayed behind with no user: {tail}"
        );
        assert!(tail.contains("%#"), "the sign is gone: {tail}");

        let numbered = run(
            &deep,
            "PROMPT='%n %2~ %# '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -r -- \"$PROMPT\"",
        )
        .expect("zsh");
        assert!(
            !numbered.contains(".../"),
            "the chip repeated a path the prompt already shows: {numbered}"
        );
        assert!(
            numbered.contains("%2~"),
            "the prompt lost its own directory: {numbered}"
        );

        // A prompt with no user in front gets the chips in front of it, with
        // the colour codes marked as taking no room.
        let bare = run(
            &deep,
            "PROMPT='$ '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -r -- \"$PROMPT\"",
        )
        .expect("zsh");
        assert!(bare.contains(".../"), "no directory chip: {bare}");
        assert!(bare.contains("feature/x"), "no branch in: {bare}");
        assert!(bare.contains("%{"), "colour codes are not zero-width: {bare}");
        // The branch keeps its mark; the path has none in front of it.
        assert!(bare.contains("\u{f126}"), "no branch mark in: {bare}");
        assert!(
            !bare.contains("\u{f114}"),
            "the path grew a mark again: {bare}"
        );

        // A worktree's `.git` is a file naming the real git directory.
        let elsewhere = root.join("real-git");
        fs::create_dir_all(&elsewhere).expect("create a git dir");
        fs::write(elsewhere.join("HEAD"), "ref: refs/heads/from-a-worktree\n").unwrap();
        let worktree = root.join("worktree");
        fs::create_dir_all(&worktree).expect("create a worktree");
        fs::write(worktree.join(".git"), "gitdir: ../real-git\n").expect("write .git");
        assert_eq!(
            run(&worktree, "zenterm_branch $PWD").as_deref(),
            Some("from-a-worktree")
        );

        // A short path is not shortened at all: the `...` is there to stand
        // for something that was actually dropped.
        let short = run(
            Path::new("/tmp"),
            "PROMPT='$ '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -r -- \"$PROMPT\"",
        )
        .expect("zsh");
        // macOS resolves `/tmp` to `/private/tmp`, so match the end of it.
        assert!(short.contains("/tmp "), "the path was touched: {short}");
        assert!(!short.contains("..."), "a short path was shortened: {short}");

        // Somewhere with no repository, a bare prompt still gets the path and
        // nothing else.
        fs::remove_dir_all(repo.join(".git")).expect("remove the repository");
        let prompt = run(
            &repo,
            "PROMPT='$ '; ZENTERM_USER_PROMPT=$PROMPT; zenterm_precmd; print -r -- \"$PROMPT\"",
        )
        .expect("zsh");
        assert!(prompt.contains("/tmp") || prompt.contains(".../"), "unexpected prompt: {prompt}");
        assert!(
            !prompt.contains("\u{f126}"),
            "a branch appeared from nowhere: {prompt}"
        );

        fs::remove_dir_all(root).ok();
    }
}
