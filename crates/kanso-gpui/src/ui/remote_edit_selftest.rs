//! `--remote-edit-test`：不开窗口、不联网，用真实临时文件检查远程编辑的本地部分，
//! 把每一步的 PASS / FAIL 写进日志，按结果退出（0 通过 / 1 失败）。
//!
//! 覆盖：下载完成不算保存；连续写入只在安静 300 毫秒后算一次；"先写新文件再替换原文件"的保存
//! 能持续检测；备份 / 交换文件不触发；有未回传修改时关闭会保留副本、已同步的被清理；
//! 下次启动清理只删已同步且未变化的副本。上传 / 下载 / 冲突要真实 SFTP 服务器，
//! 手动步骤见 docs/windows-gpui-handoff.md「远程编辑」一节。

use std::path::Path;
use std::time::{Duration, Instant};

use kanso_gpui::app::remote_edit::{
    CloseAction, EditFiles, EditState, FileVersion, POLL_MS, Phase, close_action,
};

use super::log;

fn check(results: &mut Vec<bool>, name: &str, ok: bool) {
    log(&format!(
        "remote-edit-test {} {name}",
        if ok { "PASS" } else { "FAIL" }
    ));
    results.push(ok);
}

/// 像 UI 的轮询那样跑 `ms` 毫秒；返回期间"防抖到点"的次数。
fn poll_for(state: &mut EditState, file: &Path, t0: Instant, ms: u64) -> u32 {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut fired = 0;
    while Instant::now() < end {
        let now = t0.elapsed().as_millis() as u64;
        state.watcher.observe(now, FileVersion::local(file));
        if state.watcher.take_due(now) {
            fired += 1;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS.min(40)));
    }
    fired
}

pub(crate) fn run() -> i32 {
    let mut r: Vec<bool> = Vec::new();
    let root = std::env::temp_dir().join(format!("kanso-redit-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let files = match EditFiles::new(root.clone()) {
        Ok(f) => f,
        Err(e) => {
            log(&format!("remote-edit-test FAIL 创建临时目录：{e}"));
            return 1;
        }
    };
    let record = files.record_dir(7);
    let file = EditFiles::content_path(&record, "note.txt");
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, "v1");
    let t0 = Instant::now();

    let mut st = EditState::new(FileVersion::default());
    let Some(local) = FileVersion::local(&file) else {
        log("remote-edit-test FAIL 读不到刚写的文件");
        return 1;
    };
    st.downloaded(FileVersion::default(), local);
    check(
        &mut r,
        "download-is-not-a-save",
        poll_for(&mut st, &file, t0, 700) == 0,
    );

    // 连续写入：只在安静之后到点一次。
    for i in 0..3 {
        let _ = std::fs::write(&file, format!("edit-{i}-{}", "x".repeat(i + 3)));
        std::thread::sleep(Duration::from_millis(80));
    }
    check(
        &mut r,
        "burst-writes-fire-once",
        poll_for(&mut st, &file, t0, 900) == 1,
    );
    check(
        &mut r,
        "changes-detected-vs-synced",
        st.has_changes(FileVersion::local(&file)),
    );

    // 编辑器式保存：写临时文件 + 替换原文件。
    let tmp = file.with_extension("txt.tmp");
    let _ = std::fs::write(&tmp, "replaced-content-longer");
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::rename(&tmp, &file);
    check(
        &mut r,
        "replace-save-detected",
        poll_for(&mut st, &file, t0, 900) == 1,
    );

    // 备份 / 交换文件：路径不同，不触发。
    let _ = std::fs::write(file.with_extension("txt~"), "backup");
    let _ = std::fs::write(file.with_extension("txt.swp"), "swap");
    check(
        &mut r,
        "backup-files-ignored",
        poll_for(&mut st, &file, t0, 600) == 0,
    );

    // 上传完成登记；之后再次保存仍被检测。
    let Some(snap) = FileVersion::local(&file) else {
        return 1;
    };
    st.uploaded(FileVersion::default(), snap, FileVersion::local(&file));
    check(&mut r, "synced-after-upload", st.phase == Phase::Synced);
    let _ = std::fs::write(&file, "after-upload-again");
    check(
        &mut r,
        "later-save-detected",
        poll_for(&mut st, &file, t0, 900) == 1,
    );

    // 关闭：有未回传修改保留。
    check(
        &mut r,
        "close-retains-unsynced",
        close_action(&Phase::Pending, st.has_changes(FileVersion::local(&file)))
            == CloseAction::Retain,
    );
    let Some(now_version) = FileVersion::local(&file) else {
        return 1;
    };
    // 下次启动：标记的版本与现状不同 -> 保留。
    let _ = EditFiles::mark_synced(&record, "note.txt", snap);
    if let Ok(next) = EditFiles::new(root.clone()) {
        let kept = EditFiles::clean_previous_runs(&root, Some(&next.run));
        check(
            &mut r,
            "next-start-keeps-unsynced",
            kept == vec![file.clone()],
        );
    }
    // 标记为当前版本后 -> 被清理。
    let _ = EditFiles::mark_synced(&record, "note.txt", now_version);
    if let Ok(next) = EditFiles::new(root.clone()) {
        let kept = EditFiles::clean_previous_runs(&root, Some(&next.run));
        check(
            &mut r,
            "next-start-cleans-synced",
            kept.is_empty() && !file.exists(),
        );
    }

    let _ = std::fs::remove_dir_all(&root);
    let ok = r.iter().all(|x| *x);
    log(&format!(
        "remote-edit-test {}（{} 项）",
        if ok { "全部通过" } else { "有失败" },
        r.len()
    ));
    i32::from(!ok)
}
