//! 所有真实 Git 操作都在隔离配置与 HOME 的子进程、临时仓库中执行。

use std::path::Path;
use std::process::Command;

use kanso_core::git::{GitPanel, GitSummary};
use tempfile::TempDir;

fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var("KANSO_GIT_TEST").as_deref() == Ok(name) {
        body();
        return;
    }
    let home = TempDir::new().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("KANSO_GIT_TEST", name)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_TEMPLATE_DIR")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Repo(TempDir);

impl Repo {
    fn new() -> Self {
        let repo = Self(TempDir::new().unwrap());
        repo.git(&["init", "-b", "feature/nested/name"]);
        repo.git(&["config", "user.name", "测试"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo
    }

    fn path(&self) -> &str {
        self.0.path().to_str().unwrap()
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        git(self.0.path(), args)
    }

    fn write(&self, path: &str, text: impl AsRef<[u8]>) {
        let path = self.0.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn initial(&self) {
        self.write("file.txt", "one\ntwo\nthree\n");
        self.git(&["add", "--all"]);
        self.git(&["commit", "-m", "初始"]);
    }

    fn read(&self, panel: &GitPanel) -> (u64, GitSummary) {
        let request = panel.begin();
        (request, panel.read(request, self.path()).unwrap().unwrap())
    }
}

fn git(path: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn identity_subdirectory_worktree_submodule_unborn_detached() {
    isolated(
        "identity_subdirectory_worktree_submodule_unborn_detached",
        || {
            let repo = Repo::new();
            let panel = GitPanel::default();
            repo.write("deep/path/new.txt", "new\n");
            let request = panel.begin();
            let summary = panel
                .read(request, &format!("{}/deep/path", repo.path()))
                .unwrap()
                .unwrap();
            assert_eq!(
                Path::new(&summary.root).canonicalize().unwrap(),
                repo.0.path().canonicalize().unwrap()
            );
            assert_eq!(summary.branch, "feature/nested/name");
            assert_eq!(summary.files[0].path, "deep/path/new.txt");
            assert_eq!(summary.added, 1);
            assert_eq!(
                panel
                    .diff(request, "deep/path/new.txt")
                    .unwrap()
                    .unwrap()
                    .lines[0]
                    .text,
                "new"
            );
            repo.git(&["add", "--all"]);
            repo.git(&["commit", "-m", "初始"]);
            let worktree = TempDir::new().unwrap();
            repo.git(&[
                "worktree",
                "add",
                "--detach",
                worktree.path().to_str().unwrap(),
            ]);
            let request = panel.begin();
            let summary = panel
                .read(request, worktree.path().to_str().unwrap())
                .unwrap()
                .unwrap();
            let oid = String::from_utf8(repo.git(&["rev-parse", "HEAD"])).unwrap();
            assert_eq!(summary.branch, &oid[..7]);
            assert!(summary.files.is_empty());
            let child = Repo::new();
            child.initial();
            repo.git(&[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                child.path(),
                "module",
            ]);
            let request = panel.begin();
            let summary = panel
                .read(request, &format!("{}/module", repo.path()))
                .unwrap()
                .unwrap();
            assert!(summary.root.ends_with("/module"));
            assert!(summary.files.is_empty());
        },
    );
}

#[test]
fn paths_net_changes_rename_binary_and_ignored() {
    isolated("paths_net_changes_rename_binary_and_ignored", || {
        let repo = Repo::new();
        repo.initial();
        let special = "空 格\t换\n行[ab].txt";
        repo.write(special, "old\n");
        repo.write("a.txt", "unchanged\n");
        repo.write("b.txt", "unchanged\n");
        repo.write("binary", b"old\0data");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-m", "路径"]);
        repo.write(special, "new\n");
        repo.write("binary", b"new\0data");
        repo.write("file.txt", "staged\n");
        repo.git(&["add", "file.txt"]);
        repo.write("file.txt", "one\ntwo\nthree\n");
        repo.git(&["mv", "a.txt", "renamed [ab]\n中文.txt"]);
        repo.write(".gitignore", "ignored\n");
        repo.write("ignored", "ignored\n");
        let panel = GitPanel::default();
        let (request, summary) = repo.read(&panel);
        assert!(!summary.files.iter().any(|file| file.path == "ignored"));
        let paths: Vec<_> = summary.files.iter().map(|file| file.path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        let special_file = summary
            .files
            .iter()
            .find(|file| file.path == special)
            .unwrap();
        assert_eq!(
            (special_file.added, special_file.deleted),
            (Some(1), Some(1))
        );
        assert!(
            panel
                .diff(request, special)
                .unwrap()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.text == "new")
        );
        let zero = summary
            .files
            .iter()
            .find(|file| file.path == "file.txt")
            .unwrap();
        assert_eq!((zero.added, zero.deleted), (Some(0), Some(0)));
        assert!(
            panel
                .diff(request, "file.txt")
                .unwrap()
                .unwrap()
                .notice
                .is_some_and(|message| message.key() == "git-diff-no-content-change")
        );
        let rename = summary
            .files
            .iter()
            .find(|file| file.status == "R")
            .unwrap();
        assert_eq!(rename.original.as_deref(), Some("a.txt"));
        assert_eq!(rename.added, Some(0));
        assert!(
            panel
                .diff(request, &rename.path)
                .unwrap()
                .unwrap()
                .lines
                .is_empty()
        );
        let binary = summary
            .files
            .iter()
            .find(|file| file.path == "binary")
            .unwrap();
        assert_eq!(binary.added, None);
        assert!(
            panel
                .diff(request, "binary")
                .unwrap()
                .unwrap()
                .notice
                .is_some_and(|message| message.key() == "git-diff-binary")
        );
        repo.write(special, "same\n");
        let (request, refreshed) = repo.read(&panel);
        assert_eq!(
            refreshed
                .files
                .iter()
                .find(|file| file.path == special)
                .unwrap()
                .added,
            Some(1)
        );
        assert!(
            panel
                .diff(request, special)
                .unwrap()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.text == "same")
        );
        repo.git(&["restore", "--worktree", "--", special]);
        assert!(
            panel
                .diff(request, special)
                .unwrap()
                .unwrap()
                .notice
                .is_some_and(|message| message.key() == "git-entry-unavailable")
        );
    });
}

#[test]
fn diff_limits_lines_numbers_and_external_programs_disabled() {
    isolated(
        "diff_limits_lines_numbers_and_external_programs_disabled",
        || {
            let repo = Repo::new();
            repo.initial();
            repo.write(".gitattributes", "file.txt diff=custom\n");
            repo.git(&["config", "diff.custom.textconv", "false"]);
            repo.git(&["config", "diff.external", "false"]);
            repo.write("file.txt", "one\nchanged\nthree\n");
            let panel = GitPanel::default();
            let (request, _) = repo.read(&panel);
            let diff = panel.diff(request, "file.txt").unwrap().unwrap();
            assert_eq!(
                diff.lines
                    .iter()
                    .map(|line| (line.kind.as_str(), line.number))
                    .collect::<Vec<_>>(),
                vec![(" ", 1), ("-", 2), ("+", 2), (" ", 3)]
            );
            repo.write("file.txt", "line\n".repeat(4000));
            let (request, _) = repo.read(&panel);
            let diff = panel.diff(request, "file.txt").unwrap().unwrap();
            assert!(diff.truncated);
            assert_eq!(diff.lines.len(), 3000);
            repo.write("file.txt", "x".repeat(700_000));
            let (request, _) = repo.read(&panel);
            let diff = panel.diff(request, "file.txt").unwrap().unwrap();
            assert!(diff.truncated);
            assert!(diff.lines.iter().map(|line| line.text.len()).sum::<usize>() <= 512 * 1024);
            repo.write("huge", "x".repeat(700_000));
            let (request, summary) = repo.read(&panel);
            assert_eq!(
                summary
                    .files
                    .iter()
                    .find(|file| file.path == "huge")
                    .unwrap()
                    .added,
                None
            );
            assert!(panel.diff(request, "huge").unwrap().unwrap().truncated);
            repo.write("empty", "");
            let (request, _) = repo.read(&panel);
            let diff = panel.diff(request, "empty").unwrap().unwrap();
            assert!(diff.lines.is_empty());
            assert_eq!(diff.notice.unwrap().key(), "git-diff-empty-file");
        },
    );
}

#[test]
fn commit_all_and_failure_retains_index() {
    isolated("commit_all_and_failure_retains_index", || {
        let repo = Repo::new();
        repo.initial();
        repo.write("file.txt", "modified\n");
        repo.write("new.txt", "new\n");
        let panel = GitPanel::default();
        let (request, _) = repo.read(&panel);
        assert!(panel.commit(request, " \n ").is_err());
        assert!(panel.commit(request, &"x".repeat(16385)).is_err());
        repo.git(&["config", "core.hooksPath", "/dev/null"]);
        repo.git(&["config", "user.name", ""]);
        assert!(panel.commit(request, "会失败").is_err());
        assert!(
            !repo
                .git(&["diff", "--cached", "--name-only", "-z"])
                .is_empty()
        );
        repo.git(&["config", "user.name", "测试"]);
        panel.commit(request, "  全部提交  ").unwrap();
        assert!(repo.read(&panel).1.files.is_empty());
        assert_eq!(
            String::from_utf8(repo.git(&["log", "-1", "--format=%s"]))
                .unwrap()
                .trim(),
            "全部提交"
        );
    });
}

#[test]
fn copied_file_discard_preserves_modified_source() {
    isolated("copied_file_discard_preserves_modified_source", || {
        let repo = Repo::new();
        let original = (0..30)
            .map(|index| format!("line {index}\n"))
            .collect::<String>();
        repo.write("source.txt", &original);
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-m", "原文件"]);
        repo.git(&["config", "status.renames", "copies"]);
        repo.write("copy [源]\n.txt", &original);
        let modified = format!("{original}changed source\n");
        repo.write("source.txt", &modified);
        repo.git(&["add", "--all"]);
        let panel = GitPanel::default();
        let (request, summary) = repo.read(&panel);
        let copied = summary
            .files
            .iter()
            .find(|file| file.status == "C")
            .unwrap();
        assert_eq!(copied.original.as_deref(), Some("source.txt"));
        let diff = panel.diff(request, &copied.path).unwrap().unwrap();
        assert!(!diff.lines.iter().any(|line| line.text == "changed source"));
        panel.discard(request, Some(&copied.path)).unwrap();
        assert!(!repo.0.path().join(&copied.path).exists());
        assert_eq!(
            std::fs::read_to_string(repo.0.path().join("source.txt")).unwrap(),
            modified
        );
        assert_eq!(repo.read(&panel).1.files.len(), 1);
    });
}

#[test]
fn discard_single_rename_and_all_including_unborn() {
    isolated("discard_single_rename_and_all_including_unborn", || {
        let repo = Repo::new();
        repo.initial();
        repo.write("other", "original\n");
        repo.git(&["add", "--all"]);
        repo.git(&["commit", "-m", "其他"]);
        repo.git(&["mv", "file.txt", "new [file]\n名字.txt"]);
        repo.write("new [file]\n名字.txt", "one\ntwo\nthree\nfour\n");
        repo.write("other", "keep\n");
        let panel = GitPanel::default();
        let (request, _) = repo.read(&panel);
        panel
            .discard(request, Some("new [file]\n名字.txt"))
            .unwrap();
        assert!(repo.0.path().join("file.txt").exists());
        assert!(!repo.0.path().join("new [file]\n名字.txt").exists());
        assert_eq!(
            std::fs::read_to_string(repo.0.path().join("other")).unwrap(),
            "keep\n"
        );
        repo.write(".gitignore", "ignored\n");
        repo.git(&["add", ".gitignore"]);
        repo.git(&["commit", "-m", "忽略"]);
        repo.write("ignored", "retained\n");
        repo.write("new", "delete\n");
        repo.git(&["add", "new"]);
        repo.write("untracked", "delete\n");
        let (request, _) = repo.read(&panel);
        panel.discard(request, None).unwrap();
        assert!(repo.read(&panel).1.files.is_empty());
        assert!(repo.0.path().join("ignored").exists());
        let unborn = Repo::new();
        unborn.write("staged", "new\n");
        unborn.git(&["add", "--all"]);
        unborn.write("untracked", "new\n");
        let (request, _) = unborn.read(&panel);
        panel.discard(request, None).unwrap();
        assert!(unborn.read(&panel).1.files.is_empty());
    });
}

#[test]
fn stale_requests_not_repo_and_read_errors_are_distinct() {
    isolated(
        "stale_requests_not_repo_and_read_errors_are_distinct",
        || {
            let repo = Repo::new();
            repo.initial();
            let panel = GitPanel::default();
            let (old, _) = repo.read(&panel);
            let newest = panel.begin();
            assert!(panel.read(old, repo.path()).unwrap().is_none());
            assert!(panel.diff(old, "file.txt").is_err());
            assert!(panel.discard(old, None).is_err());
            assert!(panel.is_current(newest));
            let plain = TempDir::new().unwrap();
            assert!(
                panel
                    .read(newest, plain.path().to_str().unwrap())
                    .unwrap()
                    .is_none()
            );
            assert!(
                panel
                    .read(newest, &format!("{}/missing", repo.path()))
                    .is_err()
            );
            repo.write(".git/index", "corrupt");
            assert!(panel.read(newest, repo.path()).is_err());
        },
    );
}

#[test]
fn conflicts_deletions_type_changes_and_untracked_newlines() {
    isolated(
        "conflicts_deletions_type_changes_and_untracked_newlines",
        || {
            let repo = Repo::new();
            repo.initial();
            repo.git(&["checkout", "-b", "other"]);
            repo.write("file.txt", "other branch\n");
            repo.git(&["commit", "-am", "另一分支"]);
            repo.git(&["checkout", "feature/nested/name"]);
            repo.write("file.txt", "this branch\n");
            repo.git(&["commit", "-am", "当前分支"]);
            let merge = Command::new("git")
                .current_dir(repo.path())
                .args(["merge", "other"])
                .output()
                .unwrap();
            assert!(!merge.status.success());
            let panel = GitPanel::default();
            let (_, summary) = repo.read(&panel);
            assert_eq!(summary.files[0].status, "!");
            assert_eq!(summary.files[0].added, None);
            repo.git(&["merge", "--abort"]);
            std::fs::remove_file(repo.0.path().join("file.txt")).unwrap();
            let (request, summary) = repo.read(&panel);
            assert_eq!(summary.files[0].status, "D");
            panel.discard(request, Some("file.txt")).unwrap();
            std::fs::remove_file(repo.0.path().join("file.txt")).unwrap();
            std::os::unix::fs::symlink("missing", repo.0.path().join("file.txt")).unwrap();
            let (request, summary) = repo.read(&panel);
            assert_eq!(summary.files[0].status, "T");
            panel.discard(request, Some("file.txt")).unwrap();
            assert!(
                !repo
                    .0
                    .path()
                    .join("file.txt")
                    .symlink_metadata()
                    .unwrap()
                    .is_symlink()
            );
            repo.write("new [file]\n新\t.txt", "first\nlast");
            repo.write("new binary", b"binary\0content");
            let (request, summary) = repo.read(&panel);
            let file = summary
                .files
                .iter()
                .find(|file| file.path == "new [file]\n新\t.txt")
                .unwrap();
            assert_eq!((file.status.as_str(), file.added), ("U", Some(2)));
            assert_eq!(
                panel.diff(request, &file.path).unwrap().unwrap().lines[1].text,
                "last"
            );
            panel.discard(request, Some(&file.path)).unwrap();
            assert!(!repo.0.path().join(&file.path).exists());
            assert!(repo.0.path().join("new binary").exists());
        },
    );
}

#[test]
fn in_flight_read_is_dropped_and_missing_git_is_an_error() {
    isolated(
        "in_flight_read_is_dropped_and_missing_git_is_an_error",
        || {
            if let Ok(root) = std::env::var("KANSO_DELAY_ROOT") {
                if std::env::var_os("KANSO_MISSING_GIT").is_some() {
                    let panel = GitPanel::default();
                    assert!(panel.read(panel.begin(), &root).is_err());
                    return;
                }
                let panel = std::sync::Arc::new(GitPanel::default());
                let request = panel.begin();
                let reader_panel = panel.clone();
                let reader_root = root.clone();
                let reader = std::thread::spawn(move || reader_panel.read(request, &reader_root));
                let started = std::time::Instant::now();
                while !Path::new(&root).join(".git/ready").exists() {
                    assert!(started.elapsed() < std::time::Duration::from_secs(5));
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let latest = panel.begin();
                std::fs::write(Path::new(&root).join(".git/release"), "").unwrap();
                assert!(reader.join().unwrap().unwrap().is_none());
                assert!(panel.read(latest, &root).unwrap().is_some());
                return;
            }
            let repo = Repo::new();
            repo.initial();
            let bin = repo.0.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            let wrapper = bin.join("git");
            std::fs::write(&wrapper, "#!/bin/sh\nif [ \"$5\" = rev-parse ]; then\n  touch .git/ready\n  while [ ! -e .git/release ]; do sleep 0.01; done\nfi\nexec /usr/bin/git \"$@\"\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "in_flight_read_is_dropped_and_missing_git_is_an_error",
                ])
                .env("KANSO_DELAY_ROOT", repo.path())
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "in_flight_read_is_dropped_and_missing_git_is_an_error",
                ])
                .env("KANSO_DELAY_ROOT", repo.path())
                .env("KANSO_MISSING_GIT", "1")
                .env("PATH", "")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        },
    );
}
