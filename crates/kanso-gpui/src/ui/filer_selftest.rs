//! `--filer-selftest`：不靠截图，直接驱动辅助 Filer 走一遍本地目录的浏览与文件操作，
//! 把每一步的 PASS / FAIL 写进日志，最后按结果设置退出码（除非带了 `--keep`）。
//!
//! 覆盖：显示面板并跟随活动标签、直接读取目标目录、目录在前的排序、隐藏项开关、
//! 新建目录 / 文件、新建不清空已有文件、删除文件、非空目录保留。
//! 远端传输、拖放和系统文件选择器需要真实服务器 / 桌面，不在这里。

use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{Context, Window};
use kanso_gpui::app::filer_ops::NameKind;

use super::shell::ShellView;
use super::{EXIT_CODE, KEEP_OPEN, log};

fn check(results: &mut Vec<bool>, name: &str, ok: bool, detail: &str) {
    log(&format!(
        "filer-selftest {} {name} {detail}",
        if ok { "PASS" } else { "FAIL" }
    ));
    results.push(ok);
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

impl ShellView {
    /// 启动自检（`ShellView::new` 在带 `--filer-selftest` 时调用）。
    pub(super) fn run_filer_selftest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let dir = std::env::temp_dir().join(format!("kanso-filer-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(dir.join("sub"));
            let _ = std::fs::write(dir.join("b.txt"), b"b");
            let _ = std::fs::write(dir.join("A.txt"), b"a");
            let _ = std::fs::write(dir.join(".hidden"), b"h");
            let _ = std::fs::write(dir.join("sub").join("x"), b"x");
            let target = dir.to_string_lossy().into_owned();
            let mut results: Vec<bool> = Vec::new();

            // 1. 显示面板，直接读取目标目录（不先显示家目录再跳转）。
            let t2 = target.clone();
            let _ = this.update_in(cx, |this, window, cx| {
                this.filer_selftest_show(&t2, window, cx);
            });
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.filer_selftest_panel();
                let (ready, path) = panel.read(cx).selftest_state();
                check(&mut results, "listing-ready", ready, &path);
                check(&mut results, "path-is-target", path == target, &path);
                let names = panel.read(cx).selftest_names();
                check(
                    &mut results,
                    "dirs-first-then-name-order",
                    names == strings(&["sub", ".hidden", "A.txt", "b.txt"]),
                    &format!("{names:?}"),
                );
                panel.update(cx, |p, cx| p.selftest_toggle_hidden(cx));
                let names = panel.read(cx).selftest_names();
                check(
                    &mut results,
                    "hidden-toggle",
                    names == strings(&["sub", "A.txt", "b.txt"]),
                    &format!("{names:?}"),
                );
                panel.update(cx, |p, cx| {
                    p.selftest_toggle_hidden(cx);
                    p.selftest_create(NameKind::NewFolder, "made", cx);
                    p.selftest_create(NameKind::NewFile, "made.txt", cx);
                });
            });
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;

            // 2. 新建的目录与文件出现在列表里；同名新建不会清空已有文件。
            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.filer_selftest_panel();
                let names = panel.read(cx).selftest_names();
                check(
                    &mut results,
                    "created-visible",
                    names.contains(&"made".to_owned()) && names.contains(&"made.txt".to_owned()),
                    &format!("{names:?}"),
                );
                let _ = std::fs::write(dir.join("made.txt"), b"keep");
                panel.update(cx, |p, cx| {
                    p.selftest_create(NameKind::NewFile, "made.txt", cx)
                });
                let kept = std::fs::read(dir.join("made.txt")).unwrap_or_default() == b"keep";
                check(&mut results, "create-never-truncates", kept, "");
                // 3. 删除：文件删掉；非空目录保留。
                panel.update(cx, |p, cx| {
                    p.selftest_select("made.txt");
                    p.selftest_delete_selected(cx);
                    p.selftest_select("sub");
                    p.selftest_delete_selected(cx);
                });
            });
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;

            let _ = this.update_in(cx, |this, _w, cx| {
                let panel = this.filer_selftest_panel();
                let names = panel.read(cx).selftest_names();
                check(
                    &mut results,
                    "delete-file",
                    !names.contains(&"made.txt".to_owned()),
                    &format!("{names:?}"),
                );
                check(
                    &mut results,
                    "non-empty-dir-kept",
                    dir.join("sub").join("x").exists(),
                    "",
                );
                let ok = results.iter().all(|r| *r);
                log(&format!(
                    "filer-selftest {} ({}/{})",
                    if ok { "ALL PASS" } else { "FAILED" },
                    results.iter().filter(|r| **r).count(),
                    results.len()
                ));
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
