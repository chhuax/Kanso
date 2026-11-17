//! `--git-selftest`：不靠截图，在临时仓库里驱动 Git 改动面板走一遍：
//! 摘要（分支、文件、状态、增删行数、入口按钮统计）、展开差异（有删除和新增行）、
//! 提交（列表变干净、仓库里多一个提交）、丢弃（文件恢复提交时的内容）。
//! 每一步的 PASS / FAIL 写进日志，最后按结果设置退出码（除非带了 `--keep`）。
//! 需要本机装有 `git` 命令；没有时第一步就会 FAIL 并说明。

use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{Context, Window};
use kanso_gpui::app::git_panel::{DiffKind, EmptyState, GitTarget};

use super::shell::ShellView;
use super::{EXIT_CODE, KEEP_OPEN, log};

fn check(results: &mut Vec<bool>, name: &str, ok: bool, detail: &str) {
    log(&format!(
        "git-selftest {} {name} {detail}",
        if ok { "PASS" } else { "FAIL" }
    ));
    results.push(ok);
}

/// 在 `dir` 里跑 git（带固定身份，不读用户配置里的钩子之类），返回标准输出。
fn run(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=kanso",
            "-c",
            "user.email=kanso@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl ShellView {
    /// 启动自检（`ShellView::new` 在带 `--git-selftest` 时调用）。
    pub(super) fn run_git_selftest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let dir = std::env::temp_dir().join(format!("kanso-git-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
            let mut results: Vec<bool> = Vec::new();

            // 0. 准备仓库：一个提交，然后改一个文件、加一个未跟踪文件。
            let ready = run(&dir, &["init", "-q", "-b", "main"]).is_some()
                && std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").is_ok()
                && run(&dir, &["add", "--all"]).is_some()
                && run(&dir, &["commit", "-q", "-m", "init"]).is_some()
                && std::fs::write(dir.join("a.txt"), "one\n2\nthree\nfour\n").is_ok()
                && std::fs::write(dir.join("new file.txt"), "hello\n").is_ok();
            check(&mut results, "prepare-repo", ready, "需要 git 命令");

            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            // 1. 面板跟随这个目录：显示面板并固定读取对象。
            let cwd = dir.to_string_lossy().into_owned();
            let _ = this.update_in(cx, |this, _w, cx| {
                let Some(session) = this.ctl.workspace().active_session() else {
                    return;
                };
                this.git.selftest_target = Some(GitTarget { session, cwd });
                this.git.layout_set_visible(true);
                this.sync_git(cx);
                cx.notify();
            });
            cx.background_executor()
                .timer(Duration::from_millis(2000))
                .await;
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.git.panel_entity();
                let m = &panel.read(cx).model;
                let s = m.summary.clone();
                check(
                    &mut results,
                    "summary-read",
                    s.is_some(),
                    &format!("{:?}", m.error),
                );
                let s = s.unwrap_or_else(|| kanso_gpui::app::git_panel::Summary {
                    root: String::new(),
                    branch: String::new(),
                    files: vec![],
                    added: 0,
                    deleted: 0,
                });
                check(&mut results, "branch-main", s.branch == "main", &s.branch);
                let names: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
                check(
                    &mut results,
                    "files-sorted-and-spaces-kept",
                    names == ["a.txt", "new file.txt"],
                    &format!("{names:?}"),
                );
                let st: Vec<&str> = s.files.iter().map(|f| f.status.as_str()).collect();
                check(
                    &mut results,
                    "statuses-M-U",
                    st == ["M", "U"],
                    &format!("{st:?}"),
                );
                // a.txt：+2（"2"、"four"）-1（"two"）；new file.txt：+1。
                check(
                    &mut results,
                    "totals",
                    (s.added, s.deleted) == (3, 1),
                    &format!("+{} -{}", s.added, s.deleted),
                );
                check(
                    &mut results,
                    "button-stats",
                    m.button_stats() == Some((3, 1)),
                    &format!("{:?}", m.button_stats()),
                );
                // 2. 展开差异。
                panel.update(cx, |p, cx| p.toggle("a.txt", cx));
            });
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.git.panel_entity();
                let m = &panel.read(cx).model;
                let d = m.diffs.get("a.txt");
                let has = |k: DiffKind| d.is_some_and(|d| d.lines.iter().any(|l| l.kind == k));
                check(
                    &mut results,
                    "diff-has-added-and-removed",
                    has(DiffKind::Added) && has(DiffKind::Removed),
                    &format!("{:?}", m.diff_errors),
                );
                // 3. 提交。
                panel.update(cx, |p, cx| p.selftest_commit("selftest commit", cx));
            });
            cx.background_executor()
                .timer(Duration::from_millis(3000))
                .await;
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.git.panel_entity();
                let m = &panel.read(cx).model;
                check(
                    &mut results,
                    "commit-leaves-clean",
                    m.empty_state() == EmptyState::Clean,
                    &format!("{:?} {:?}", m.empty_state(), m.error),
                );
                let count = run(&dir, &["rev-list", "--count", "HEAD"]).unwrap_or_default();
                check(
                    &mut results,
                    "commit-created",
                    count.trim() == "2",
                    count.trim(),
                );
                // 4. 丢弃：再改一次，手动刷新，丢弃该文件。
                let _ = std::fs::write(dir.join("a.txt"), "changed again\n");
                panel.update(cx, |p, cx| p.request_refresh(true, cx));
            });
            cx.background_executor()
                .timer(Duration::from_millis(2000))
                .await;
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.git.panel_entity();
                let listed = panel
                    .read(cx)
                    .model
                    .summary
                    .as_ref()
                    .is_some_and(|s| s.files.len() == 1);
                check(&mut results, "refresh-sees-new-change", listed, "");
                panel.update(cx, |p, cx| p.discard(Some("a.txt".into()), cx));
            });
            cx.background_executor()
                .timer(Duration::from_millis(3000))
                .await;
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.git.panel_entity();
                let content = std::fs::read_to_string(dir.join("a.txt")).unwrap_or_default();
                check(
                    &mut results,
                    "discard-restores-file",
                    content == "one\n2\nthree\nfour\n",
                    &format!("{content:?}"),
                );
                check(
                    &mut results,
                    "discard-leaves-clean",
                    panel.read(cx).model.empty_state() == EmptyState::Clean,
                    "",
                );
                let ok = results.iter().all(|r| *r);
                log(&format!(
                    "git-selftest {} ({}/{})",
                    if ok { "ALL PASS" } else { "FAILED" },
                    results.iter().filter(|r| **r).count(),
                    results.len()
                ));
                this.git.selftest_target = None;
                let _ = std::fs::remove_dir_all(&dir);
                if !ok {
                    EXIT_CODE.store(1, Ordering::SeqCst);
                }
                if !KEEP_OPEN.load(Ordering::Relaxed) {
                    cx.quit();
                }
            });
        })
        .detach();
    }
}
