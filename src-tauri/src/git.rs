//! Local Shell 的 Git 状态读取。分支名直接从 `.git/HEAD` 获取；未提交变更和
//! 单文件 diff 通过 `git -C` 按需读取，并在每条本地命令结束后刷新。

use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

const MAX_DIFF_BYTES: usize = 512 * 1024;

/// 未提交文件的主要状态，由 porcelain 状态码归一化后供前端展示。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
    TypeChanged,
}

/// Git 工作区中的一个未提交文件，路径始终相对于仓库根目录。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub path: String,
    pub previous_path: Option<String>,
    pub status: GitFileStatus,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

/// Local Shell 当前目录所属仓库的只读变更摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChanges {
    pub root: String,
    pub branch: Option<String>,
    pub files: Vec<GitFileChange>,
    pub additions: u64,
    pub deletions: u64,
}

/// 单文件展开时返回的统一 diff；内容超过上限时只返回前半部分。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub text: String,
    pub truncated: bool,
}

/// The branch of the repository holding `path`, or None when the directory is
/// in no repository.
pub fn branch_for(path: &Path) -> Option<String> {
    read_branch(&git_dir(path)?)
}

/// The `.git` directory at or above `path`. A worktree or a submodule keeps a
/// `.git` *file* naming its real git directory instead, so that indirection is
/// followed; a relative one is relative to the directory holding the file,
/// the way git resolves it.
fn git_dir(path: &Path) -> Option<PathBuf> {
    for dir in path.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text
                .lines()
                .find_map(|line| line.strip_prefix("gitdir:"))?
                .trim();
            if target.is_empty() {
                return None;
            }
            let target = Path::new(target);
            return Some(if target.is_absolute() {
                target.to_path_buf()
            } else {
                dir.join(target)
            });
        }
    }
    None
}

/// The branch `HEAD` names, or the short commit where the repository is on a
/// detached head and the commit is all there is to say.
fn read_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        // A branch may hold slashes (`feature/parser`), so only the namespace
        // comes off; anything else referenced from HEAD is shown as written.
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!name.is_empty()).then(|| name.to_string());
    }
    let short: String = head.chars().take(7).collect();
    (short.len() == 7 && short.chars().all(|c| c.is_ascii_hexdigit())).then_some(short)
}

fn git_command(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    // 避免刷新可选索引锁；写操作仍会获取必需锁，文件名则始终按字面匹配。
    command
        .args(["--no-optional-locks", "--literal-pathspecs", "-C"])
        .arg(path)
        .args(args)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn run_git(path: &Path, args: &[&str]) -> io::Result<Output> {
    git_command(path, args).output()
}

fn run_git_with_paths(path: &Path, args: &[&str], paths: &[String]) -> io::Result<Output> {
    let mut command = git_command(path, args);
    command.arg("--").args(paths).output()
}

fn command_error(action: &str, output: &Output) -> io::Error {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    io::Error::new(
        io::ErrorKind::Other,
        if detail.is_empty() {
            format!("git {action} failed with {}", output.status)
        } else {
            format!("git {action} failed: {detail}")
        },
    )
}

fn repository_root(path: &Path) -> io::Result<Option<PathBuf>> {
    let output = run_git(path, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string();
    Ok((!root.is_empty()).then(|| PathBuf::from(root)))
}

fn status_kind(code: &[u8]) -> GitFileStatus {
    if code.iter().any(|byte| *byte == b'U') || code == b"AA" || code == b"DD" {
        GitFileStatus::Conflicted
    } else if code.contains(&b'?') {
        GitFileStatus::Untracked
    } else if code.contains(&b'R') {
        GitFileStatus::Renamed
    } else if code.contains(&b'C') {
        GitFileStatus::Copied
    } else if code.contains(&b'D') {
        GitFileStatus::Deleted
    } else if code.contains(&b'A') {
        GitFileStatus::Added
    } else if code.contains(&b'T') {
        GitFileStatus::TypeChanged
    } else {
        GitFileStatus::Modified
    }
}

fn parse_status(bytes: &[u8]) -> Vec<GitFileChange> {
    let fields: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let record = fields[index];
        index += 1;
        if record.len() < 4 || record[2] != b' ' {
            continue;
        }
        let status = status_kind(&record[..2]);
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        let previous_path = if matches!(status, GitFileStatus::Renamed | GitFileStatus::Copied) {
            let previous = fields.get(index).copied().unwrap_or_default();
            index += usize::from(index < fields.len());
            Some(String::from_utf8_lossy(previous).into_owned())
        } else {
            None
        };
        files.push(GitFileChange {
            path,
            previous_path,
            status,
            additions: None,
            deletions: None,
        });
    }
    files
}

fn parse_count(bytes: &[u8]) -> Option<u64> {
    if bytes == b"-" {
        None
    } else {
        String::from_utf8_lossy(bytes).parse().ok()
    }
}

fn parse_numstat(bytes: &[u8]) -> HashMap<String, (Option<u64>, Option<u64>)> {
    let fields: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
    let mut counts = HashMap::new();
    let mut index = 0;
    while index < fields.len() {
        let record = fields[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        let mut columns = record.splitn(3, |byte| *byte == b'\t');
        let Some(additions) = columns.next() else {
            continue;
        };
        let Some(deletions) = columns.next() else {
            continue;
        };
        let Some(path) = columns.next() else {
            continue;
        };
        let path = if path.is_empty() {
            // `--numstat -z` 把 rename/copy 的旧、新路径放在后续两个 NUL 字段中。
            index += usize::from(index < fields.len());
            let current = fields.get(index).copied().unwrap_or_default();
            index += usize::from(index < fields.len());
            current
        } else {
            path
        };
        counts.insert(
            String::from_utf8_lossy(path).into_owned(),
            (parse_count(additions), parse_count(deletions)),
        );
    }
    counts
}

fn text_line_count(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_DIFF_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    if bytes.is_empty() {
        return Some(0);
    }
    Some(
        bytes.iter().filter(|byte| **byte == b'\n').count() as u64
            + u64::from(!bytes.ends_with(b"\n")),
    )
}

/// 读取 `path` 所属仓库的未提交变更；不在 Git 仓库中时返回 None。
pub fn changes_for(path: &Path) -> io::Result<Option<GitChanges>> {
    let Some(root) = repository_root(path)? else {
        return Ok(None);
    };
    let status = run_git(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status.status.success() {
        return Err(command_error("status", &status));
    }
    let mut files = parse_status(&status.stdout);
    let head = run_git(&root, &["rev-parse", "--verify", "HEAD"])?;
    let has_head = head.status.success();
    let counts = if has_head {
        let output = run_git(
            &root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--numstat",
                "-z",
                "HEAD",
                "--",
            ],
        )?;
        if !output.status.success() {
            return Err(command_error("diff --numstat", &output));
        }
        parse_numstat(&output.stdout)
    } else {
        HashMap::new()
    };

    let mut additions = 0;
    let mut deletions = 0;
    for file in &mut files {
        if let Some((added, removed)) = counts.get(&file.path) {
            file.additions = *added;
            file.deletions = *removed;
        } else if file.status == GitFileStatus::Untracked || !has_head {
            file.additions = text_line_count(&root.join(&file.path));
            file.deletions = file.additions.map(|_| 0);
        } else {
            // 暂存区和工作区可能互相抵消；状态仍有变化，但相对 HEAD 的净行数为零。
            file.additions = Some(0);
            file.deletions = Some(0);
        }
        additions += file.additions.unwrap_or(0);
        deletions += file.deletions.unwrap_or(0);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(GitChanges {
        root: root.to_string_lossy().into_owned(),
        branch: branch_for(&root),
        files,
        additions,
        deletions,
    }))
}

fn safe_relative_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (!path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_))))
    .then_some(path)
}

fn limited_text(bytes: &[u8]) -> GitFileDiff {
    let truncated = bytes.len() > MAX_DIFF_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_DIFF_BYTES)];
    GitFileDiff {
        text: String::from_utf8_lossy(bytes).into_owned(),
        truncated,
    }
}

fn working_file_diff(root: &Path, path: &str) -> io::Result<GitFileDiff> {
    let full = root.join(path);
    let metadata = std::fs::symlink_metadata(&full)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(&full)?;
        return Ok(GitFileDiff {
            text: format!(
                "diff --git a/{path} b/{path}\nnew symbolic link\n+{}\n",
                target.display()
            ),
            truncated: false,
        });
    }
    if !metadata.is_file() {
        return Ok(GitFileDiff {
            text: format!("No text diff available for {path}\n"),
            truncated: false,
        });
    }
    let mut bytes = Vec::new();
    File::open(&full)?
        .take(MAX_DIFF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.contains(&0) {
        return Ok(GitFileDiff {
            text: format!("Binary file {path} is new\n"),
            truncated: bytes.len() > MAX_DIFF_BYTES,
        });
    }
    let source = limited_text(&bytes);
    let mut text = format!("diff --git a/{path} b/{path}\nnew file\n--- /dev/null\n+++ b/{path}\n");
    for line in source.text.split_inclusive('\n') {
        text.push('+');
        text.push_str(line);
    }
    if !source.text.is_empty() && !source.text.ends_with('\n') {
        text.push('\n');
    }
    let mut diff = limited_text(text.as_bytes());
    diff.truncated |= source.truncated;
    Ok(diff)
}

/// 按需读取仓库内一个未提交文件的统一 diff；路径必须来自 `changes_for`。
pub fn diff_for(root: &Path, path: &str) -> io::Result<Option<GitFileDiff>> {
    let Some(relative) = safe_relative_path(path) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid git path",
        ));
    };
    let Some(actual_root) = repository_root(root)? else {
        return Ok(None);
    };
    let status = run_git(
        &actual_root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status.status.success() {
        return Err(command_error("status", &status));
    }
    let files = parse_status(&status.stdout);
    let Some(change) = files.iter().find(|file| file.path == path) else {
        return Ok(None);
    };
    let head = run_git(&actual_root, &["rev-parse", "--verify", "HEAD"])?;
    if change.status == GitFileStatus::Untracked || !head.status.success() {
        return working_file_diff(&actual_root, path).map(Some);
    }
    let path = relative.to_string_lossy();
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--unified=3",
        "HEAD",
        "--",
        &path,
    ];
    if let Some(previous) = change.previous_path.as_deref() {
        // 同时限定旧、新路径，Git 才能展示重命名 diff，而非整份新文件内容。
        args.push(previous);
    }
    let mut child = git_command(&actual_root, &args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    let result = child
        .stdout
        .take()
        .expect("piped stdout")
        .take(MAX_DIFF_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    // 大 diff 在生成阶段就停止读取，不能先把完整输出放进内存再截断。
    let truncated = bytes.len() > MAX_DIFF_BYTES;
    if truncated || result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    result?;
    if !status.success() && !truncated {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("git diff failed with {status}"),
        ));
    }
    Ok(Some(limited_text(&bytes)))
}

fn required_repository_root(path: &Path) -> io::Result<PathBuf> {
    repository_root(path)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the selected directory is no longer a Git repository",
        )
    })
}

fn updated_changes(root: &Path) -> io::Result<GitChanges> {
    changes_for(root)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "the selected directory is no longer a Git repository",
        )
    })
}

fn path_exists_in_head(root: &Path, path: &str) -> io::Result<bool> {
    let object = format!("HEAD:{path}");
    Ok(run_git(root, &["cat-file", "-e", &object])?
        .status
        .success())
}

/// 暂存仓库内的全部变更并创建提交；调用方必须提供非空提交说明。
pub fn commit_all(root: &Path, message: &str) -> io::Result<GitChanges> {
    let root = required_repository_root(root)?;
    let message = message.trim();
    if message.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "commit message cannot be empty",
        ));
    }
    if message.len() > 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "commit message is too long",
        ));
    }
    if updated_changes(&root)?.files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "there are no changes to commit",
        ));
    }

    let add = run_git(&root, &["add", "-A", "--"])?;
    if !add.status.success() {
        return Err(command_error("add", &add));
    }
    let commit = run_git(&root, &["commit", "-m", message])?;
    if !commit.status.success() {
        return Err(command_error("commit", &commit));
    }
    updated_changes(&root)
}

/// 丢弃一个当前未提交文件；路径必须仍出现在仓库状态中，新增内容会被永久删除。
pub fn discard_file(root: &Path, path: &str) -> io::Result<GitChanges> {
    safe_relative_path(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid git path"))?;
    let root = required_repository_root(root)?;
    let current = updated_changes(&root)?;
    let change = current
        .files
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the selected file is no longer changed",
            )
        })?;

    let mut candidates = vec![change.path.clone()];
    if change.status == GitFileStatus::Renamed {
        if let Some(previous) = change.previous_path.clone() {
            safe_relative_path(&previous).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid previous git path")
            })?;
            candidates.push(previous);
        }
    }

    let has_head = run_git(&root, &["rev-parse", "--verify", "HEAD"])?
        .status
        .success();
    let mut tracked = Vec::new();
    let mut added = Vec::new();
    for candidate in candidates {
        if has_head && path_exists_in_head(&root, &candidate)? {
            tracked.push(candidate);
        } else {
            added.push(candidate);
        }
    }

    if !tracked.is_empty() {
        let restore = run_git_with_paths(
            &root,
            &["restore", "--source=HEAD", "--staged", "--worktree"],
            &tracked,
        )?;
        if !restore.status.success() {
            return Err(command_error("restore", &restore));
        }
    }
    if !added.is_empty() {
        // 先从索引移除新增路径，再用 Git 自己清理工作区，避免自行跟随符号链接。
        let unstage = run_git_with_paths(
            &root,
            &["rm", "-r", "-f", "--cached", "--ignore-unmatch"],
            &added,
        )?;
        if !unstage.status.success() {
            return Err(command_error("rm --cached", &unstage));
        }
        let clean = run_git_with_paths(&root, &["clean", "-f", "-d"], &added)?;
        if !clean.status.success() {
            return Err(command_error("clean", &clean));
        }
    }

    updated_changes(&root)
}

/// 丢弃仓库内全部已跟踪和未跟踪变更，但保留被 `.gitignore` 忽略的内容。
pub fn discard_all(root: &Path) -> io::Result<GitChanges> {
    let root = required_repository_root(root)?;
    let current = updated_changes(&root)?;
    if current.files.is_empty() {
        return Ok(current);
    }

    let has_head = run_git(&root, &["rev-parse", "--verify", "HEAD"])?
        .status
        .success();
    if has_head {
        let reset = run_git(&root, &["reset", "--hard", "HEAD"])?;
        if !reset.status.success() {
            return Err(command_error("reset --hard", &reset));
        }
    } else {
        // 尚无首个提交时，先清空索引，随后才能把已暂存文件作为未跟踪内容清理。
        let unstage = run_git(
            &root,
            &["rm", "-r", "-f", "--cached", "--ignore-unmatch", "--", "."],
        )?;
        if !unstage.status.success() {
            return Err(command_error("rm --cached", &unstage));
        }
    }
    let clean = run_git(&root, &["clean", "-f", "-d"])?;
    if !clean.status.success() {
        return Err(command_error("clean", &clean));
    }
    updated_changes(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_test_git(dir: &Path, args: &[&str]) {
        let output = run_git(dir, args).expect("run git");
        assert!(
            output.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kanso-git-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".git")).expect("create .git");
        dir
    }

    fn write_head(dir: &Path, contents: &str) {
        std::fs::write(dir.join(".git").join("HEAD"), contents).expect("write HEAD");
    }

    fn initialized_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kanso-git-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create repository");
        run_test_git(&dir, &["init", "-q"]);
        run_test_git(&dir, &["config", "user.name", "Kanso Test"]);
        run_test_git(&dir, &["config", "user.email", "kanso@example.invalid"]);
        run_test_git(&dir, &["config", "commit.gpgsign", "false"]);
        // 测试不能继承 runner 的全局换行策略，否则 Git 恢复后的字节内容会随平台变化。
        run_test_git(&dir, &["config", "core.autocrlf", "false"]);
        dir
    }

    #[test]
    fn porcelain_status_handles_spaces_renames_and_conflicts() {
        let files = parse_status(
            b" M src/file name.rs\0R  new name.txt\0old name.txt\0?? fresh file.md\0UU conflict.txt\0",
        );

        assert_eq!(files.len(), 4);
        assert_eq!(files[0].path, "src/file name.rs");
        assert_eq!(files[0].status, GitFileStatus::Modified);
        assert_eq!(files[1].path, "new name.txt");
        assert_eq!(files[1].previous_path.as_deref(), Some("old name.txt"));
        assert_eq!(files[1].status, GitFileStatus::Renamed);
        assert_eq!(files[2].path, "fresh file.md");
        assert_eq!(files[2].status, GitFileStatus::Untracked);
        assert_eq!(files[3].status, GitFileStatus::Conflicted);
    }

    #[test]
    fn numstat_handles_binary_files_and_renames() {
        let mut bytes = b"3\t1\tsrc/lib.rs\0-\t-\tasset.bin\0".to_vec();
        bytes.extend_from_slice(b"1\t0\t\0old name.txt\0new name.txt\0");
        let counts = parse_numstat(&bytes);

        assert_eq!(counts.get("src/lib.rs"), Some(&(Some(3), Some(1))));
        assert_eq!(counts.get("asset.bin"), Some(&(None, None)));
        assert_eq!(counts.get("new name.txt"), Some(&(Some(1), Some(0))));
        assert!(!counts.contains_key("old name.txt"));
    }

    #[test]
    fn diff_paths_must_stay_below_the_repository() {
        assert!(safe_relative_path("src/lib.rs").is_some());
        assert!(safe_relative_path("").is_none());
        assert!(safe_relative_path("../outside").is_none());
        assert!(safe_relative_path("src/../outside").is_none());
        assert!(safe_relative_path("/tmp/outside").is_none());
    }

    #[test]
    fn changes_and_diffs_are_read_from_a_real_repository() {
        let dir =
            std::env::temp_dir().join(format!("kanso-git-changes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create repository");
        run_test_git(&dir, &["init", "-q"]);
        run_test_git(&dir, &["config", "user.name", "Kanso Test"]);
        run_test_git(&dir, &["config", "user.email", "kanso@example.invalid"]);
        run_test_git(&dir, &["config", "commit.gpgsign", "false"]);

        std::fs::write(dir.join("tracked.txt"), "one\n").expect("write tracked file");
        let initial = changes_for(&dir).unwrap().unwrap();
        assert_eq!(initial.additions, 1);
        assert!(diff_for(&dir, "tracked.txt")
            .unwrap()
            .unwrap()
            .text
            .contains("+one"));
        run_test_git(&dir, &["add", "tracked.txt"]);
        run_test_git(&dir, &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.join("tracked.txt"), "one\ntwo\n").expect("modify tracked file");
        std::fs::write(dir.join("new file.txt"), "alpha\nbeta\n").expect("write new file");

        let changes = changes_for(&dir)
            .expect("read changes")
            .expect("repository exists");
        assert_eq!(changes.additions, 3);
        assert_eq!(changes.deletions, 0);
        let tracked = changes
            .files
            .iter()
            .find(|file| file.path == "tracked.txt")
            .expect("tracked change");
        assert_eq!(tracked.status, GitFileStatus::Modified);
        assert_eq!((tracked.additions, tracked.deletions), (Some(1), Some(0)));
        let untracked = changes
            .files
            .iter()
            .find(|file| file.path == "new file.txt")
            .expect("untracked change");
        assert_eq!(untracked.status, GitFileStatus::Untracked);
        assert_eq!(
            (untracked.additions, untracked.deletions),
            (Some(2), Some(0))
        );

        let tracked_diff = diff_for(&dir, "tracked.txt")
            .expect("read tracked diff")
            .expect("tracked diff exists");
        assert!(tracked_diff.text.contains("+two"));
        let new_diff = diff_for(&dir, "new file.txt")
            .expect("read untracked diff")
            .expect("untracked diff exists");
        assert!(new_diff.text.contains("+alpha"));
        assert!(new_diff.text.contains("+beta"));

        run_test_git(&dir, &["mv", "tracked.txt", "renamed [file].txt"]);
        let renamed = changes_for(&dir).unwrap().unwrap();
        let file = renamed
            .files
            .iter()
            .find(|file| file.path == "renamed [file].txt")
            .unwrap();
        assert_eq!(file.status, GitFileStatus::Renamed);
        assert_eq!(file.previous_path.as_deref(), Some("tracked.txt"));
        assert_eq!(file.additions, Some(1));
        let rename_diff = diff_for(&dir, "renamed [file].txt").unwrap().unwrap();
        assert!(rename_diff.text.contains("rename from tracked.txt"));
        assert!(rename_diff.text.contains("+two"));

        std::fs::write(
            dir.join("renamed [file].txt"),
            "large line\n".repeat(MAX_DIFF_BYTES / 5),
        )
        .unwrap();
        let large_diff = diff_for(&dir, "renamed [file].txt").unwrap().unwrap();
        assert!(large_diff.truncated);
        assert!(large_diff.text.len() <= MAX_DIFF_BYTES);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn commit_and_discard_operations_cover_tracked_added_and_renamed_files() {
        let dir = initialized_repo("operations");
        std::fs::write(dir.join("tracked.txt"), "original\n").expect("write tracked file");
        run_test_git(&dir, &["add", "tracked.txt"]);
        run_test_git(&dir, &["commit", "-q", "-m", "initial"]);

        std::fs::write(dir.join("tracked.txt"), "modified\n").expect("modify tracked file");
        std::fs::write(dir.join("new file.txt"), "new\n").expect("write untracked file");
        discard_file(&dir, "tracked.txt").expect("discard tracked file");
        assert_eq!(
            std::fs::read_to_string(dir.join("tracked.txt")).unwrap(),
            "original\n"
        );
        discard_file(&dir, "new file.txt").expect("discard untracked file");
        assert!(!dir.join("new file.txt").exists());

        run_test_git(&dir, &["mv", "tracked.txt", "renamed.txt"]);
        discard_file(&dir, "renamed.txt").expect("discard rename");
        assert!(dir.join("tracked.txt").exists());
        assert!(!dir.join("renamed.txt").exists());

        std::fs::write(dir.join("tracked.txt"), "discard all\n").expect("modify tracked file");
        std::fs::write(dir.join("staged.txt"), "staged\n").expect("write staged file");
        std::fs::write(dir.join("untracked.txt"), "untracked\n").expect("write untracked file");
        run_test_git(&dir, &["add", "staged.txt"]);
        let clean = discard_all(&dir).expect("discard all changes");
        assert!(clean.files.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("tracked.txt")).unwrap(),
            "original\n"
        );
        assert!(!dir.join("staged.txt").exists());
        assert!(!dir.join("untracked.txt").exists());

        std::fs::write(dir.join("tracked.txt"), "committed\n").expect("modify tracked file");
        let committed = commit_all(&dir, "panel commit").expect("commit all changes");
        assert!(committed.files.is_empty());
        let subject = run_git(&dir, &["log", "-1", "--pretty=%s"]).expect("read commit");
        assert_eq!(
            String::from_utf8_lossy(&subject.stdout).trim(),
            "panel commit"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_branch_is_read_from_the_repository_above_the_directory() {
        let dir = repo("branch");
        write_head(&dir, "ref: refs/heads/main\n");
        assert_eq!(branch_for(&dir).as_deref(), Some("main"));

        // A shell deep inside the tree is still on the same branch.
        let deep = dir.join("src").join("parser");
        std::fs::create_dir_all(&deep).expect("create deep dir");
        assert_eq!(branch_for(&deep).as_deref(), Some("main"));

        // Branch names carry slashes; the namespace is not part of the name.
        write_head(&dir, "ref: refs/heads/feature/parser\n");
        assert_eq!(branch_for(&deep).as_deref(), Some("feature/parser"));

        // A detached head is named by the commit it is parked on.
        write_head(&dir, "0123456789abcdef0123456789abcdef01234567\n");
        assert_eq!(branch_for(&dir).as_deref(), Some("0123456"));

        // Nothing readable in HEAD is no branch, not a made-up one.
        write_head(&dir, "");
        assert_eq!(branch_for(&dir), None);
        write_head(&dir, "ref: \n");
        assert_eq!(branch_for(&dir), None);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_worktree_points_at_its_git_directory() {
        let dir = repo("worktree");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("create gitdir");
        std::fs::write(elsewhere.join("HEAD"), "ref: refs/heads/from-a-worktree\n")
            .expect("write HEAD");
        // A worktree's `.git` is a file, and the path in it is relative here.
        std::fs::remove_dir_all(dir.join(".git")).expect("remove inner .git");
        std::fs::write(dir.join(".git"), "gitdir: elsewhere\n").expect("write .git file");

        assert_eq!(
            branch_for(&dir).as_deref(),
            Some("from-a-worktree"),
            "the gitdir in the file is the repository"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_directory_in_no_repository_has_no_branch() {
        let dir = std::env::temp_dir().join(format!("kanso-git-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        assert_eq!(branch_for(&dir), None);
        std::fs::remove_dir_all(dir).ok();
    }
}
