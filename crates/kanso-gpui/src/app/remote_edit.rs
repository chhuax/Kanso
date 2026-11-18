//! 用本地应用编辑远端文件的纯逻辑（docs/spec/filer.md §9）：文件版本、保存检测与 300 毫秒防抖、
//! 重试退避、冲突判断、重新打开 / 关闭时的决定，以及临时副本的目录布局与"下次启动清理"。
//!
//! 保存检测用轮询 `(大小, 修改时间)`：只盯最初那条本地路径，所以编辑器"先写新文件再替换原文件"
//! 也能持续检测；备份 / 交换文件在别的路径上，不会触发上传。时间一律由调用方以毫秒传入，便于测试。
//! 界面层（`ui/remote_edit.rs`）负责真正的下载 / 上传与定时器。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use kanso_types::fs::FileStat;
use kanso_types::message::Message;
use serde::{Deserialize, Serialize};

use crate::app::filer_text::msg;
use crate::app::fspath::{PathStyle, validate_child_name};
use crate::filer::format_size;
use crate::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec, InputSpec};

/// 保存动作安静多久后才回传（毫秒）。
pub const DEBOUNCE_MS: u64 = 300;
/// 轮询本地副本的间隔（毫秒）。
pub const POLL_MS: u64 = 150;
/// 超过这个大小（字节）先让用户确认再下载（D6 未决，沿用 mac 版的 50 MB）。
pub const LARGE_FILE_BYTES: u64 = 50_000_000;
/// 最近使用的应用最多保留几个（Windows 使用系统选择器，不用，保留给 Linux）。
pub const MAX_RECENT_APPS: usize = 6;
/// 运行目录下存放拖出暂存副本的子目录名（不是会话编号）。
pub const DRAG_DIR: &str = "drag";

/// 文件版本：大小与修改时间（毫秒）。不做内容哈希，所以"大小与时间都不变"的改写检测不到。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    /// 大小（字节）。
    pub size: Option<u64>,
    /// 修改时间（Unix 毫秒）。
    pub mtime_ms: Option<i64>,
}

impl FileVersion {
    /// 读本地文件的版本；文件不存在返回 `None`。
    pub fn local(path: &Path) -> Option<FileVersion> {
        let md = std::fs::metadata(path).ok()?;
        if !md.is_file() {
            return None;
        }
        let mtime_ms = md.modified().ok().and_then(|t| {
            let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
            i64::try_from(d.as_millis()).ok()
        });
        Some(FileVersion {
            size: Some(md.len()),
            mtime_ms,
        })
    }

    /// 远端 STAT 结果。
    pub fn from_stat(stat: &FileStat) -> FileVersion {
        FileVersion {
            size: stat.size,
            mtime_ms: stat.mtime_ms,
        }
    }
}

/// 一份远程编辑所处的阶段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 正在下载（还没有监测）。
    Downloading,
    /// 本地副本与远端一致。
    Synced,
    /// 有保存，等待回传（防抖中 / 排队 / 等待重连）。
    Pending,
    /// 正在回传。
    Syncing,
    /// 远端已变化，暂停回传，等用户决定。
    Conflict,
    /// 回传失败（会重试）。
    Failed(String),
}

impl Phase {
    /// 状态文字（不含来源与路径）。
    pub fn text(&self, name: &str) -> String {
        match self {
            Phase::Downloading => msg(Message::RemoteEditPreparing {
                name: name.to_owned(),
            }),
            Phase::Synced => msg(Message::RemoteEditSynced {}),
            Phase::Pending => msg(Message::RemoteEditPending {}),
            Phase::Syncing => msg(Message::RemoteEditSyncing {}),
            Phase::Conflict => msg(Message::RemoteEditConflict {}),
            Phase::Failed(why) => msg(Message::RemoteEditFailed { why: why.clone() }),
        }
    }

    /// 是否应当用错误色显示。
    pub fn is_error(&self) -> bool {
        matches!(self, Phase::Conflict | Phase::Failed(_))
    }
}

/// 状态行："来源 · 远端路径 · 状态"，错误包含原会话、文件名称和原因（§9.2）。
pub fn status_line(source: &str, path: &str, text: &str) -> String {
    msg(Message::RemoteEditStatusLine {
        source: source.to_owned(),
        path: path.to_owned(),
        text: text.to_owned(),
    })
}

/// 第 `attempt` 次（从 0 起）失败后等多久重试（毫秒）：6、12、24、48、60、60… 秒。
pub fn retry_delay_ms(attempt: u32) -> u64 {
    (6u64 << attempt.min(4)).min(60) * 1000
}

/// 本地副本的保存检测：轮询观察版本，变化后安静满 [`DEBOUNCE_MS`] 才算"可以回传"。
#[derive(Clone, Debug, Default)]
pub struct SaveWatcher {
    observed: Option<FileVersion>,
    /// 最近一次发现变化（或文件暂时消失）的时刻；没有待处理变化为 `None`。
    changed_at: Option<u64>,
}

impl SaveWatcher {
    /// 以当前版本为基线（刚下载完成不算保存）。
    pub fn baseline(version: Option<FileVersion>) -> SaveWatcher {
        SaveWatcher {
            observed: version,
            changed_at: None,
        }
    }

    /// 喂入一次观察。返回是否发现了新变化。
    ///
    /// 文件暂时不存在（编辑器正在"删旧写新"）视为仍在保存中：推迟回传，但不改变基线，
    /// 等它重新出现后与基线比较。
    pub fn observe(&mut self, now: u64, current: Option<FileVersion>) -> bool {
        match current {
            None => {
                if self.changed_at.is_some() {
                    self.changed_at = Some(now);
                }
                false
            }
            Some(v) if Some(v) != self.observed => {
                self.observed = Some(v);
                self.changed_at = Some(now);
                true
            }
            Some(_) => false,
        }
    }

    /// 是否有变化在等回传。
    pub fn has_pending(&self) -> bool {
        self.changed_at.is_some()
    }

    /// 防抖到点了吗。
    pub fn due(&self, now: u64) -> bool {
        self.changed_at
            .is_some_and(|t| now.saturating_sub(t) >= DEBOUNCE_MS)
    }

    /// 取走"到点"状态（开始回传前调用）。
    pub fn take_due(&mut self, now: u64) -> bool {
        if self.due(now) {
            self.changed_at = None;
            true
        } else {
            false
        }
    }

    /// 回传失败 / 回传期间又有保存：重新标记为待处理，下一次轮询按防抖处理。
    pub fn mark_pending(&mut self, now: u64) {
        self.changed_at = Some(now);
    }

    /// 距离到点还要多久（毫秒）；没有待处理变化为 `None`。
    pub fn wait_left(&self, now: u64) -> Option<u64> {
        self.changed_at
            .map(|t| DEBOUNCE_MS.saturating_sub(now.saturating_sub(t)))
    }
}

/// 回传前的远端冲突检查结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictCheck {
    /// 远端与打开时一致，可以回传。
    Clean,
    /// 远端已被改动。
    Changed,
    /// 远端文件已不存在。
    Gone,
}

/// 比较"打开时的远端版本"与"回传前的远端版本"。
pub fn check_conflict(opened: &FileVersion, current: Option<&FileVersion>) -> ConflictCheck {
    match current {
        None => ConflictCheck::Gone,
        Some(c) if c == opened => ConflictCheck::Clean,
        Some(_) => ConflictCheck::Changed,
    }
}

/// 用户对冲突的选择。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    /// 覆盖远端。
    Overwrite,
    /// 丢弃本地修改，重新下载。
    Redownload,
    /// 另存副本：由用户再输入一个新文件名，本地内容上传为远端同目录下的新文件。
    SaveCopy,
    /// 取消（取消按钮 / Esc / 点遮罩）：保持暂停。
    Cancel,
}

/// 重新打开同一远端文件时怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReopenPlan {
    /// 还在下载：什么也不做。
    Ignore,
    /// 有未回传的本地修改（或正在回传）：不下载覆盖，直接重新打开本地副本并说明。
    KeepLocal,
    /// 没有未同步内容：先停止监测，重新下载后再打开。
    Redownload,
}

/// 重新打开的决定（§9.4）。
pub fn reopen_plan(phase: &Phase, has_local_changes: bool, uploading: bool) -> ReopenPlan {
    if *phase == Phase::Downloading {
        ReopenPlan::Ignore
    } else if has_local_changes || uploading || matches!(phase, Phase::Pending | Phase::Syncing) {
        ReopenPlan::KeepLocal
    } else {
        ReopenPlan::Redownload
    }
}

/// 关闭标签 / 退出时对一份副本的处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    /// 已全部回传：可以清理。
    Clean,
    /// 有未回传的修改：必须保留并告知位置。
    Retain,
}

/// 关闭时的决定：有未回传修改就保留（§9.3）。下载中的半成品直接清理。
pub fn close_action(phase: &Phase, has_local_changes: bool) -> CloseAction {
    if has_local_changes && *phase != Phase::Downloading {
        CloseAction::Retain
    } else {
        CloseAction::Clean
    }
}

/// 一份远程编辑的状态（不含任何 IO）。
#[derive(Clone, Debug)]
pub struct EditState {
    /// 当前阶段。
    pub phase: Phase,
    /// 打开（或最近一次成功回传 / 下载）时远端的版本。
    pub remote: FileVersion,
    /// 最近一次与远端一致时本地副本的版本。
    pub synced: Option<FileVersion>,
    /// 连续失败次数（决定下次重试等多久）。
    pub retry_attempt: u32,
    /// 保存检测。
    pub watcher: SaveWatcher,
    /// 上一次提示过的错误文字，用来不重复弹相同错误。
    pub last_error: Option<String>,
}

impl EditState {
    /// 刚开始下载。
    pub fn new(remote: FileVersion) -> EditState {
        EditState {
            phase: Phase::Downloading,
            remote,
            synced: None,
            retry_attempt: 0,
            watcher: SaveWatcher::default(),
            last_error: None,
        }
    }

    /// 本地副本是否有没回传的修改。
    pub fn has_changes(&self, local: Option<FileVersion>) -> bool {
        match (self.synced, local) {
            (Some(s), Some(l)) => s != l,
            _ => false,
        }
    }

    /// 下载完成：记基线、进入已同步。
    pub fn downloaded(&mut self, remote: FileVersion, local: FileVersion) {
        self.remote = remote;
        self.synced = Some(local);
        self.watcher = SaveWatcher::baseline(Some(local));
        self.phase = Phase::Synced;
        self.retry_attempt = 0;
        self.last_error = None;
    }

    /// 回传成功。`local` 是回传时快照的版本，`now_local` 是现在本地副本的版本。
    /// 回传期间又保存过（`now_local != local`）则仍有待同步内容，不能标成已同步。
    pub fn uploaded(
        &mut self,
        remote: FileVersion,
        local: FileVersion,
        now_local: Option<FileVersion>,
    ) {
        self.remote = remote;
        self.synced = Some(local);
        self.retry_attempt = 0;
        self.last_error = None;
        self.phase = if now_local.is_some_and(|n| n != local) {
            Phase::Pending
        } else {
            Phase::Synced
        };
    }

    /// 另存副本成功（照 mac 版）：本地当前内容已存到远端的新文件，原远端文件仍是变化后的样子，
    /// 所以 `remote` 基线不动。本地没有再变就记为已同步；已变则仍是冲突（由调用方暂停，
    /// 下一次保存才重新询问）。
    pub fn copied(&mut self, local: FileVersion, now_local: Option<FileVersion>) {
        self.synced = Some(local);
        self.retry_attempt = 0;
        self.last_error = None;
        self.phase = if now_local.is_some_and(|n| n != local) {
            Phase::Conflict
        } else {
            Phase::Synced
        };
    }

    /// 回传失败：记原因并返回下次重试的等待（毫秒）。
    pub fn failed(&mut self, why: String) -> u64 {
        let delay = retry_delay_ms(self.retry_attempt);
        self.retry_attempt = self.retry_attempt.saturating_add(1);
        self.phase = Phase::Failed(why);
        delay
    }

    /// 这次错误是否要向用户弹出（与上一次相同就不再弹，§9.3）。
    pub fn should_announce(&mut self, why: &str) -> bool {
        if self.last_error.as_deref() == Some(why) {
            false
        } else {
            self.last_error = Some(why.to_owned());
            true
        }
    }
}

// ---- 对话框 ----

/// 大文件确认：继续。
pub const BTN_LARGE_GO: &str = "large-go";
/// 大文件确认 / 冲突：取消。
pub const BTN_CANCEL: &str = "cancel";
/// 冲突：覆盖远端。
pub const BTN_OVERWRITE: &str = "overwrite";
/// 冲突：重新下载。
pub const BTN_REDOWNLOAD: &str = "redownload";

/// 大文件打开前的确认（D6 未决：只做提醒，不设上限）。
pub fn large_spec(name: &str, size: u64) -> DialogSpec {
    DialogSpec::new(&msg(Message::RemoteEditLargeTitle {}))
        .text(&msg(Message::RemoteEditLargeBody {
            name: name.to_owned(),
            amount: format_size(size),
        }))
        .button(ButtonSpec::secondary(BTN_CANCEL, &msg(Message::UiFilerCancel {})).cancel())
        .button(
            ButtonSpec::primary(BTN_LARGE_GO, &msg(Message::RemoteEditLargeContinue {}))
                .default_button(),
        )
}

/// 是否确认继续下载大文件。
pub fn large_confirmed(result: &DialogResult) -> bool {
    result.button.as_deref() == Some(BTN_LARGE_GO)
}

/// 冲突：另存副本…
pub const BTN_SAVE_COPY: &str = "save-copy";
/// 另存副本名称框：保存。
pub const BTN_COPY_OK: &str = "copy-ok";
/// 另存副本名称框里输入框的标识。
pub const COPY_FIELD: &str = "name";

/// 远端冲突对话框（D5 已定，2026-11-18 用户决定照 mac 版）：
/// 取消（默认 / Esc）、覆盖、重新下载、另存副本…。默认是取消，不会悄悄覆盖。
pub fn conflict_spec(name: &str, source: &str) -> DialogSpec {
    DialogSpec::new(&msg(Message::RemoteEditConflictTitle {}))
        .text(&msg(Message::RemoteEditConflictBody {
            name: name.to_owned(),
            source: source.to_owned(),
        }))
        .button(
            ButtonSpec::secondary(BTN_CANCEL, &msg(Message::UiFilerCancel {}))
                .cancel()
                .default_button(),
        )
        .button(ButtonSpec::danger(
            BTN_OVERWRITE,
            &msg(Message::RemoteEditConflictOverwrite {}),
        ))
        .button(ButtonSpec::secondary(
            BTN_REDOWNLOAD,
            &msg(Message::RemoteEditConflictRedownload {}),
        ))
        .button(ButtonSpec::secondary(
            BTN_SAVE_COPY,
            &msg(Message::RemoteEditConflictSaveCopy {}),
        ))
}

/// 冲突对话框结果；取消 / Esc / 点遮罩都按取消。
pub fn conflict_choice(result: &DialogResult) -> ConflictChoice {
    match result.button.as_deref() {
        Some(BTN_OVERWRITE) => ConflictChoice::Overwrite,
        Some(BTN_REDOWNLOAD) => ConflictChoice::Redownload,
        Some(BTN_SAVE_COPY) => ConflictChoice::SaveCopy,
        _ => ConflictChoice::Cancel,
    }
}

/// 另存副本的默认文件名：`a.txt` -> `a-copy.txt`，无扩展名 `a` -> `a-copy`，`.bashrc` -> `.bashrc-copy`。
pub fn default_copy_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => format!("{}-copy{}", &name[..i], &name[i..]),
        _ => format!("{name}-copy"),
    }
}

/// 另存副本的文件名输入框。`error` 非空时显示在输入框上方（上一次提交被拒绝）。
pub fn copy_spec(initial: &str, error: Option<&str>) -> DialogSpec {
    let mut spec = DialogSpec::new(&msg(Message::RemoteEditConflictSaveCopy {}))
        .text(&msg(Message::RemoteEditCopyPrompt {}));
    if let Some(e) = error {
        spec = spec.text(e);
    }
    spec.input(InputSpec::new(COPY_FIELD, &msg(Message::UiFilerName {})).initial(initial))
        .button(ButtonSpec::secondary(BTN_CANCEL, &msg(Message::UiFilerCancel {})).cancel())
        .button(ButtonSpec::primary(BTN_COPY_OK, &msg(Message::UiSave {})).default_button())
}

/// 另存副本名称框的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    /// 取消：保持暂停。
    Cancel,
    /// 名称不合规，或与原文件同名（需要提示后让用户重输）。
    Invalid,
    /// 合规的新名称。
    Name(String),
}

/// 解析另存副本名称框的结果。名称必须是远端目录的直接子项、本机也能作文件名（快照按它命名），
/// 且不能与原文件同名。
pub fn parse_copy_name(result: &DialogResult, original: &str) -> CopyOutcome {
    if result.button.as_deref() != Some(BTN_COPY_OK) {
        return CopyOutcome::Cancel;
    }
    let raw = result.value(COPY_FIELD).unwrap_or_default();
    match validate_child_name(PathStyle::Posix, raw) {
        Ok(name) if name != original && local_name_ok(&name) => CopyOutcome::Name(name),
        _ => CopyOutcome::Invalid,
    }
}

// ---- 文件名 ----

/// 远端文件名能否原样作为本机文件名（回传时按文件名上传，不能改名）。
pub fn local_name_ok(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    !reserved
}

// ---- 临时副本布局 ----

/// 布局：`<root>/<run>/<session>/<record>/content/<name>`，记录目录里还有 `state.json`、
/// 临时的 `download/` 与 `upload/`。不同会话、不同远端路径各占一个记录目录，同名文件互不覆盖。
#[derive(Clone, Debug)]
pub struct EditFiles {
    /// 所有运行共用的根目录。
    pub root: PathBuf,
    /// 本次运行的目录。
    pub run: PathBuf,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成不会重复的名字片段（进程号 + 纳秒 + 计数）。
pub fn unique_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// 记在记录目录里的"已同步"标记（下次启动据此清理）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedMark {
    /// 副本文件名。
    pub name: String,
    /// 同步时副本的版本。
    pub version: FileVersion,
}

impl EditFiles {
    /// 以系统临时目录下的 `kanso-remote-edit` 为根，创建本次运行目录。
    pub fn new_default() -> std::io::Result<EditFiles> {
        EditFiles::new(std::env::temp_dir().join("kanso-remote-edit"))
    }

    /// 以 `root` 为根创建本次运行目录。
    pub fn new(root: PathBuf) -> std::io::Result<EditFiles> {
        let run = root.join(unique_id());
        std::fs::create_dir_all(&run)?;
        Ok(EditFiles { root, run })
    }

    /// 为一次"准备拖出"新建暂存目录（不创建，只给路径）。按每次拖动隔离，下次启动清理（§7.4）。
    pub fn drag_dir(&self) -> PathBuf {
        self.run.join(DRAG_DIR).join(unique_id())
    }

    /// 为一个远端条目新建记录目录（不创建，只给路径）。
    pub fn record_dir(&self, session: u64) -> PathBuf {
        self.run.join(session.to_string()).join(unique_id())
    }

    /// 记录目录里副本的位置。
    pub fn content_path(record: &Path, name: &str) -> PathBuf {
        record.join("content").join(name)
    }

    /// 写"已同步"标记。
    pub fn mark_synced(record: &Path, name: &str, version: FileVersion) -> std::io::Result<()> {
        let mark = SyncedMark {
            name: name.to_owned(),
            version,
        };
        let data = serde_json::to_vec(&mark).map_err(std::io::Error::other)?;
        let tmp = record.join("state.json.tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(tmp, record.join("state.json"))
    }

    /// 清除"已同步"标记（副本又被改了，之后若直接退出也要保留）。
    pub fn clear_mark(record: &Path) {
        let _ = std::fs::remove_file(record.join("state.json"));
    }

    /// 删除记录目录（连同可能为空的上层会话目录）。
    pub fn remove_record(record: &Path) {
        let _ = std::fs::remove_dir_all(record);
        if let Some(session_dir) = record.parent() {
            let _ = std::fs::remove_dir(session_dir); // 非空会失败，正好
        }
    }

    /// 下次启动清理：删除以前各次运行里"标记为已同步且内容未再变化"的记录，保留其余（未同步）的。
    /// 返回被保留的副本文件路径（有未同步内容的）。
    pub fn clean_previous_runs(root: &Path, current_run: Option<&Path>) -> Vec<PathBuf> {
        let mut kept = Vec::new();
        let Ok(runs) = std::fs::read_dir(root) else {
            return kept;
        };
        for run in runs.flatten().map(|e| e.path()) {
            if Some(run.as_path()) == current_run || !run.is_dir() {
                continue;
            }
            for session in read_dirs(&run) {
                if session.file_name().is_some_and(|n| n == DRAG_DIR) {
                    let _ = std::fs::remove_dir_all(&session);
                    continue;
                }
                for record in read_dirs(&session) {
                    let synced = std::fs::read(record.join("state.json"))
                        .ok()
                        .and_then(|d| serde_json::from_slice::<SyncedMark>(&d).ok())
                        .filter(|m| local_name_ok(&m.name))
                        .is_some_and(|m| {
                            FileVersion::local(&Self::content_path(&record, &m.name))
                                == Some(m.version)
                        });
                    if synced {
                        let _ = std::fs::remove_dir_all(&record);
                    } else if let Some(f) = first_content_file(&record) {
                        kept.push(f);
                    }
                }
                let _ = std::fs::remove_dir(&session);
            }
            let _ = std::fs::remove_dir(&run);
        }
        kept
    }
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn first_content_file(record: &Path) -> Option<PathBuf> {
    std::fs::read_dir(record.join("content"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(size: u64, t: i64) -> FileVersion {
        FileVersion {
            size: Some(size),
            mtime_ms: Some(t),
        }
    }

    #[test]
    fn retry_schedule() {
        let d: Vec<u64> = (0..7).map(|i| retry_delay_ms(i) / 1000).collect();
        assert_eq!(d, vec![6, 12, 24, 48, 60, 60, 60]);
    }

    #[test]
    fn debounce_waits_for_quiet() {
        let mut w = SaveWatcher::baseline(Some(v(1, 1)));
        assert!(!w.observe(0, Some(v(1, 1))));
        assert!(w.observe(100, Some(v(0, 2)))); // 编辑器先清空
        assert!(!w.due(300));
        assert!(w.observe(350, Some(v(9, 3)))); // 又写入完整内容
        assert!(!w.due(600));
        assert!(w.due(650));
        assert!(w.take_due(650));
        assert!(!w.has_pending());
    }

    #[test]
    fn replace_save_via_missing_file_is_one_change() {
        let mut w = SaveWatcher::baseline(Some(v(5, 1)));
        assert!(!w.observe(10, None)); // 删旧
        assert!(!w.has_pending()); // 文件消失本身不算一次保存……
        let _ = w.observe(20, None);
        assert!(w.observe(30, Some(v(6, 2)))); // ……新文件出现才算
        assert!(w.due(330));
    }

    #[test]
    fn missing_while_pending_postpones() {
        let mut w = SaveWatcher::baseline(Some(v(5, 1)));
        w.observe(0, Some(v(6, 2)));
        w.observe(250, None);
        assert!(!w.due(400));
        assert!(w.due(550));
    }

    #[test]
    fn download_itself_is_not_a_save() {
        let mut s = EditState::new(v(5, 1));
        s.downloaded(v(5, 1), v(5, 100));
        assert!(!s.watcher.has_pending());
        assert!(!s.has_changes(Some(v(5, 100))));
        assert!(s.has_changes(Some(v(6, 101))));
    }

    #[test]
    fn save_during_upload_keeps_pending() {
        let mut s = EditState::new(v(5, 1));
        s.downloaded(v(5, 1), v(5, 100));
        s.phase = Phase::Syncing;
        s.uploaded(v(6, 2), v(6, 101), Some(v(7, 102)));
        assert_eq!(s.phase, Phase::Pending);
        s.uploaded(v(7, 3), v(7, 102), Some(v(7, 102)));
        assert_eq!(s.phase, Phase::Synced);
    }

    #[test]
    fn failure_backoff_and_dedup() {
        let mut s = EditState::new(v(1, 1));
        assert_eq!(s.failed("x".into()), 6000);
        assert_eq!(s.failed("x".into()), 12000);
        assert!(s.should_announce("x"));
        assert!(!s.should_announce("x"));
        assert!(s.should_announce("y"));
        s.uploaded(v(1, 2), v(1, 2), Some(v(1, 2)));
        assert_eq!(s.retry_attempt, 0);
    }

    #[test]
    fn conflict_decisions() {
        let o = v(5, 1);
        assert_eq!(check_conflict(&o, Some(&o)), ConflictCheck::Clean);
        assert_eq!(check_conflict(&o, Some(&v(5, 2))), ConflictCheck::Changed);
        assert_eq!(check_conflict(&o, None), ConflictCheck::Gone);
    }

    #[test]
    fn reopen_never_overwrites_unsynced() {
        assert_eq!(
            reopen_plan(&Phase::Downloading, false, false),
            ReopenPlan::Ignore
        );
        assert_eq!(
            reopen_plan(&Phase::Synced, true, false),
            ReopenPlan::KeepLocal
        );
        assert_eq!(
            reopen_plan(&Phase::Pending, false, false),
            ReopenPlan::KeepLocal
        );
        assert_eq!(
            reopen_plan(&Phase::Synced, false, true),
            ReopenPlan::KeepLocal
        );
        assert_eq!(
            reopen_plan(&Phase::Synced, false, false),
            ReopenPlan::Redownload
        );
    }

    #[test]
    fn close_retains_unsynced() {
        assert_eq!(close_action(&Phase::Pending, true), CloseAction::Retain);
        assert_eq!(close_action(&Phase::Synced, false), CloseAction::Clean);
        assert_eq!(close_action(&Phase::Downloading, true), CloseAction::Clean);
    }

    fn pressed(spec: &DialogSpec, id: &str, name: &str) -> DialogResult {
        let i = spec.buttons.iter().position(|b| b.id == id).unwrap();
        let texts = vec![name.to_owned(); spec.inputs.len()];
        crate::widgets::dialog::result_for_button(
            spec,
            i,
            crate::widgets::dialog::Trigger::Click,
            &texts,
        )
    }

    #[test]
    fn conflict_dialog_matches_mac_order() {
        let spec = conflict_spec("a.txt", "host");
        let ids: Vec<&str> = spec.buttons.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(
            ids,
            [BTN_CANCEL, BTN_OVERWRITE, BTN_REDOWNLOAD, BTN_SAVE_COPY]
        );
        assert_eq!(
            conflict_choice(&pressed(&spec, BTN_SAVE_COPY, "")),
            ConflictChoice::SaveCopy
        );
        assert_eq!(
            conflict_choice(&pressed(&spec, BTN_OVERWRITE, "")),
            ConflictChoice::Overwrite
        );
        assert_eq!(
            conflict_choice(&pressed(&spec, BTN_REDOWNLOAD, "")),
            ConflictChoice::Redownload
        );
    }

    #[test]
    fn save_copy_keeps_remote_baseline() {
        let mut st = EditState::new(v(1, 1));
        st.downloaded(v(1, 1), v(5, 5));
        st.phase = Phase::Conflict;
        st.copied(v(6, 6), Some(v(6, 6)));
        assert_eq!(st.phase, Phase::Synced);
        assert_eq!(st.remote, v(1, 1));
        assert!(!st.has_changes(Some(v(6, 6))));
        st.copied(v(6, 6), Some(v(7, 7)));
        assert_eq!(st.phase, Phase::Conflict);
        assert!(st.has_changes(Some(v(7, 7))));
    }

    #[test]
    fn copy_names() {
        assert_eq!(default_copy_name("a.txt"), "a-copy.txt");
        assert_eq!(default_copy_name("a.tar.gz"), "a.tar-copy.gz");
        assert_eq!(default_copy_name("Makefile"), "Makefile-copy");
        assert_eq!(default_copy_name(".bashrc"), ".bashrc-copy");
        assert_eq!(default_copy_name("a."), "a.-copy");
        let spec = copy_spec("a-copy.txt", None);
        let ok = |n: &str| parse_copy_name(&pressed(&spec, BTN_COPY_OK, n), "a.txt");
        assert_eq!(ok(" b.txt "), CopyOutcome::Name("b.txt".into()));
        assert_eq!(ok("a.txt"), CopyOutcome::Invalid);
        assert_eq!(ok(""), CopyOutcome::Invalid);
        assert_eq!(ok("x/y"), CopyOutcome::Invalid);
        assert_eq!(ok("a:b"), CopyOutcome::Invalid);
        assert_eq!(
            parse_copy_name(&pressed(&spec, BTN_CANCEL, "b.txt"), "a.txt"),
            CopyOutcome::Cancel
        );
    }

    #[test]
    fn dialogs_default_is_safe() {
        let spec = conflict_spec("a.txt", "host");
        let d = spec.default_button().unwrap();
        assert_eq!(spec.buttons[d].id, BTN_CANCEL);
        let r = crate::widgets::dialog::result_for_cancel(
            &spec,
            crate::widgets::dialog::Trigger::Escape,
        );
        assert_eq!(conflict_choice(&r), ConflictChoice::Cancel);
        assert!(!large_confirmed(
            &crate::widgets::dialog::result_for_cancel(
                &large_spec("x", 1),
                crate::widgets::dialog::Trigger::Escape
            )
        ));
    }

    #[test]
    fn names() {
        assert!(local_name_ok("a.txt"));
        assert!(local_name_ok("中文 文件.md"));
        assert!(!local_name_ok("a:b"));
        assert!(!local_name_ok("con.txt"));
        assert!(!local_name_ok("COM1"));
        assert!(local_name_ok("COM10"));
        assert!(!local_name_ok("x."));
        assert!(!local_name_ok(""));
    }

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kanso-redit-test-{tag}-{}", unique_id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn records_are_isolated_and_cleanup_keeps_unsynced() {
        let root = scratch("clean");
        // 上一次运行：一份已同步、一份有未同步修改。
        let old = EditFiles::new(root.clone()).unwrap();
        let r1 = old.record_dir(1);
        let r2 = old.record_dir(1);
        assert_ne!(r1, r2);
        for (r, body) in [(&r1, "a"), (&r2, "b")] {
            let f = EditFiles::content_path(r, "same.txt");
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, body).unwrap();
        }
        let f1 = EditFiles::content_path(&r1, "same.txt");
        EditFiles::mark_synced(&r1, "same.txt", FileVersion::local(&f1).unwrap()).unwrap();
        // r2 标记为已同步后又被改。
        let f2 = EditFiles::content_path(&r2, "same.txt");
        EditFiles::mark_synced(&r2, "same.txt", FileVersion::local(&f2).unwrap()).unwrap();
        std::fs::write(&f2, "changed longer").unwrap();

        let now = EditFiles::new(root.clone()).unwrap();
        let kept = EditFiles::clean_previous_runs(&root, Some(&now.run));
        assert_eq!(kept, vec![f2.clone()]);
        assert!(!r1.exists());
        assert!(f2.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_version_reads_size() {
        let d = scratch("ver");
        let f = d.join("x");
        std::fs::write(&f, "abc").unwrap();
        assert_eq!(FileVersion::local(&f).unwrap().size, Some(3));
        assert!(FileVersion::local(&d.join("nope")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
