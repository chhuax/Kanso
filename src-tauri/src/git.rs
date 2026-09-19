//! Which branch a local shell is sitting on, for the line under its path in
//! the rail. Nothing here runs `git`: the directory a shell reports is enough
//! to find the answer, because the nearest `.git` above it names the
//! repository and that repository's `HEAD` names the branch. A `git checkout`
//! is a command like any other, so the answer is read again when a command
//! finishes rather than watched for.

use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zenterm-git-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".git")).expect("create .git");
        dir
    }

    fn write_head(dir: &Path, contents: &str) {
        std::fs::write(dir.join(".git").join("HEAD"), contents).expect("write HEAD");
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
        let dir = std::env::temp_dir().join(format!("zenterm-git-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        assert_eq!(branch_for(&dir), None);
        std::fs::remove_dir_all(dir).ok();
    }
}
