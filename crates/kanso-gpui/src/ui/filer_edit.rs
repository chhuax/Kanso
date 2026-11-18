//! 用本地应用编辑远端文件（docs/spec/filer.md §9）的界面侧协调：下载到临时副本、交给系统应用、
//! 轮询检测保存、防抖后回传、冲突确认、断线重试、关闭 / 退出时的清理与保留。
//!
//! 纯逻辑（防抖、冲突、重试退避、重新打开 / 关闭的决定、临时目录布局与清理）在
//! `kanso_gpui::app::remote_edit`，有单元测试；这里只做接线：核心调用（stat / download / upload）、
//! 定时器、对话框与状态行。这些代码是 `FilerPanel` 的一部分（`impl FilerPanel`），只有辅助 Filer 会用到。
//!
//! 保存检测用轮询（[`POLL_MS`]）而不是文件监视：依赖树里没有文件监视库，轮询只盯最初那条路径的
//! `(大小, 修改时间)`，天然支持"先写新文件再替换原文件"的保存方式，也不会被备份 / 交换文件触发。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::Context;
use kanso_gpui::app::drag_out::DragPrep;
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::launcher::{core_error_text, render_message};
use kanso_gpui::app::remote_edit::{
    self, CloseAction, ConflictCheck, ConflictChoice, CopyOutcome, EditFiles, EditState,
    FileVersion, LARGE_FILE_BYTES, POLL_MS, Phase, ReopenPlan, close_action, conflict_choice,
    local_name_ok, reopen_plan,
};
use kanso_gpui::widgets::dialog::DialogResult;
use kanso_types::event::{SessionState, TransferOutcome};
use kanso_types::message::{Message, Reason};
use kanso_types::transfer::ConflictPolicy;
use kanso_types::{CoreEvent, SessionId, TransferId};

use super::filer_panel::{FilerEvent, FilerPanel, PanelMode, Pending, Source, open_with_dialog};

/// 已同步的状态行在回传成功后保留多久（毫秒）。
const STATUS_LINGER_MS: u64 = 3000;

/// 一份远程编辑的标识：哪个会话的哪个远端完整路径。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EditKey {
    pub session: SessionId,
    pub path: String,
}

/// 下载完成后怎么交给应用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Launch {
    /// 系统为该文件关联的默认应用。
    Default,
    /// 系统"打开方式"选择器。
    Chooser,
}

/// 开始下载前需要的全部信息（大文件确认对话框期间由 `Pending` 持有）。
#[derive(Clone, Debug)]
pub(crate) struct EditStart {
    pub key: EditKey,
    pub name: String,
    pub source: String,
    /// `None`：只下载、不再启动应用（冲突后"重新下载"，应用已经开着）。
    pub launch: Option<Launch>,
    pub remote: FileVersion,
}

/// 一份进行中的远程编辑。
pub(crate) struct Edit {
    pub key: EditKey,
    pub name: String,
    /// 来源标识（状态行 / 提示里的"原会话"）。
    pub source: String,
    pub record: PathBuf,
    pub file: PathBuf,
    pub state: EditState,
    /// 正在进行的下载或上传。
    pub transfer: Option<TransferId>,
    /// 回传进行中（快照、上传、回传后读远端版本）。
    pub uploading: bool,
    /// 正在做回传前的远端检查。
    pub attempting: bool,
    /// 本轮回传快照时本地副本的版本。
    pub upload_version: Option<FileVersion>,
    /// 下载前读到的远端版本。
    pub pending_remote: Option<FileVersion>,
    pub launch_after: Option<Launch>,
    pub retry_at: Option<u64>,
    /// 用户在冲突框里取消：不再自动弹冲突框，直到有新的保存或重新打开。
    pub conflict_paused: bool,
    /// 冲突对话框正在显示。
    pub conflict_open: bool,
    /// 另存副本进行中：本轮回传上传为这个新文件名（不覆盖，且不改变原文件的远端基线）。
    pub copy_to: Option<String>,
    /// 回传后读不到远端版本：下一次回传跳过冲突检查（否则会误报）。
    pub skip_check: bool,
    pub done: u64,
    pub total: Option<u64>,
    pub synced_at: Option<u64>,
}

/// 传输 id 对应的用途。
pub(crate) enum Role {
    EditDownload(EditKey),
    EditUpload(EditKey),
    Drag(Arc<DragRole>),
}

/// 一次拖出准备里的所有下载（可能多选）。
pub(crate) struct DragGroup {
    pub dir: PathBuf,
    pub names: Vec<String>,
    pub remaining: std::sync::atomic::AtomicUsize,
    pub failure: std::sync::Mutex<Option<String>>,
    pub transfers: std::sync::Mutex<Vec<TransferId>>,
    pub label: String,
}

/// 准备拖出的远端条目下载（每个条目一个传输）。
pub(crate) struct DragRole {
    pub prep: Arc<DragPrep>,
    pub group: Arc<DragGroup>,
    pub session: SessionId,
}

/// 拖出准备的进度（状态行显示第一个传输的进度）。
pub(crate) struct DragStatus {
    pub session: SessionId,
    pub title: String,
    pub done: u64,
    pub total: Option<u64>,
    pub transfer: Option<TransferId>,
}

/// 辅助面板持有的远程编辑与拖出状态。
pub(crate) struct EditHub {
    pub files: Option<EditFiles>,
    pub edits: HashMap<EditKey, Edit>,
    pub roles: HashMap<TransferId, Role>,
    pub connected: HashMap<SessionId, bool>,
    pub opening: HashSet<EditKey>,
    pub polling: bool,
    pub drag: Option<DragStatus>,
}

impl EditHub {
    pub(crate) fn new(files: Option<EditFiles>) -> EditHub {
        EditHub {
            files,
            edits: HashMap::new(),
            roles: HashMap::new(),
            connected: HashMap::new(),
            opening: HashSet::new(),
            polling: false,
            drag: None,
        }
    }
}

/// 状态行内容。
pub(crate) struct EditStatus {
    pub text: String,
    pub is_error: bool,
    /// 画进度条（下载 / 拖出准备）。
    pub progress: bool,
    /// 已知比例；`None` 画不确定进度。
    pub fraction: Option<f64>,
    /// 可取消的传输。
    pub cancel: Option<TransferId>,
}

/// 远端路径的父目录。
pub(crate) fn remote_parent(path: &str) -> String {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_owned(),
        Some((dir, _)) => dir.to_owned(),
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn frac(done: u64, total: Option<u64>) -> Option<f64> {
    total
        .filter(|t| *t > 0)
        .map(|t| (done as f64 / t as f64).clamp(0.0, 1.0))
}

fn is_not_found(e: &kanso_core::CoreError) -> bool {
    matches!(
        e.user_message(),
        Some(Message::SftpOpFailed {
            reason: Reason::NotFound,
            ..
        })
    )
}

impl FilerPanel {
    fn say(&mut self, text: String, cx: &mut Context<Self>) {
        cx.emit(FilerEvent::Notice(text));
    }

    // ---- 打开 ----

    /// 双击 / 菜单"打开" / "打开方式"一个远端文件（只在辅助 Filer 里）。
    pub(crate) fn open_remote(
        &mut self,
        idx: usize,
        id: u32,
        chooser: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mode != PanelMode::Aux {
            return;
        }
        let Some(row) = self.panes[idx].model.row(id).cloned() else {
            return;
        };
        let Source::Remote(session) = self.panes[idx].source else {
            return;
        };
        if row.is_dir {
            return;
        }
        if !self.panes[idx].connected {
            self.say(msg(Message::RemoteEditNotConnected {}), cx);
            return;
        }
        if !local_name_ok(&row.name) || self.edits.files.is_none() {
            self.say(
                msg(Message::RemoteEditBadName {
                    name: row.name.clone(),
                }),
                cx,
            );
            return;
        }
        self.edits.connected.insert(session, true);
        let key = EditKey {
            session,
            path: row.path.clone(),
        };
        let launch = if chooser {
            Launch::Chooser
        } else {
            Launch::Default
        };
        let now = self.now_ms();
        let existing = self.edits.edits.get(&key).map(|e| {
            let local = FileVersion::local(&e.file);
            (
                reopen_plan(&e.state.phase, e.state.has_changes(local), e.uploading),
                e.file.clone(),
            )
        });
        if let Some((plan, file)) = existing {
            match plan {
                ReopenPlan::Ignore => return,
                ReopenPlan::KeepLocal => {
                    // 有未回传的修改：不用远端内容覆盖，直接重新打开本地副本并说明（§9.4）。
                    self.say(
                        msg(Message::RemoteEditUnsavedKept {
                            name: row.name.clone(),
                        }),
                        cx,
                    );
                    self.edit_launch(&file, launch, cx);
                    if let Some(e) = self.edits.edits.get_mut(&key)
                        && e.state.phase == Phase::Conflict
                    {
                        e.state.phase = Phase::Pending;
                        e.conflict_paused = false;
                        e.state.watcher.mark_pending(now);
                    }
                    self.edit_ensure_polling(cx);
                    cx.notify();
                    return;
                }
                ReopenPlan::Redownload => {}
            }
        }
        if !self.edits.opening.insert(key.clone()) {
            return;
        }
        let source = self.panes[idx].identity.clone();
        let core = self.core.clone();
        let path = row.path.clone();
        let name = row.name.clone();
        cx.spawn(async move |this, cx| {
            let r = core
                .sftp_stat(session, &path)
                .await
                .map_err(|e| core_error_text(&e));
            let _ = this.update(cx, |t, cx| {
                t.edit_stat_done(key, name, source, Some(launch), r, cx)
            });
        })
        .detach();
    }

    fn edit_stat_done(
        &mut self,
        key: EditKey,
        name: String,
        source: String,
        launch: Option<Launch>,
        stat: Result<kanso_types::fs::FileStat, String>,
        cx: &mut Context<Self>,
    ) {
        self.edits.opening.remove(&key);
        let stat = match stat {
            Ok(s) => s,
            Err(why) => {
                self.say(msg(Message::RemoteEditOpenFailed { name, why }), cx);
                return;
            }
        };
        let start = EditStart {
            key,
            name,
            source,
            launch,
            remote: FileVersion::from_stat(&stat),
        };
        if let Some(size) = stat.size.filter(|s| *s > LARGE_FILE_BYTES) {
            // 大文件先确认（D6 未决：只提醒，不设上限）。
            if self.pending.is_none() {
                cx.emit(FilerEvent::Dialog(remote_edit::large_spec(
                    &start.name,
                    size,
                )));
                self.pending = Some(Pending::EditLarge(Box::new(start)));
                return;
            }
        }
        self.edit_begin_download(start, cx);
    }

    fn edit_launch(&mut self, file: &Path, launch: Launch, cx: &mut Context<Self>) {
        match launch {
            Launch::Default => cx.open_with_system(file),
            Launch::Chooser => open_with_dialog(&path_str(file)),
        }
    }

    /// 下载到记录目录的暂存位置，成功后再替换成正式副本并启动应用。
    fn edit_begin_download(&mut self, start: EditStart, cx: &mut Context<Self>) {
        let session = start.key.session;
        let Some(files) = self.edits.files.clone() else {
            return;
        };
        let (record, file) = match self.edits.edits.get(&start.key) {
            Some(e) => (e.record.clone(), e.file.clone()),
            None => {
                let record = files.record_dir(session.raw());
                let file = EditFiles::content_path(&record, &start.name);
                (record, file)
            }
        };
        let staging = record.join("download");
        let prepared = std::fs::remove_dir_all(&staging)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .and_then(|()| std::fs::create_dir_all(&staging));
        let first = !self.edits.edits.contains_key(&start.key);
        let fail = |this: &mut Self, why: String, cx: &mut Context<Self>| {
            if first {
                EditFiles::remove_record(&record);
            }
            this.say(
                msg(Message::RemoteEditOpenFailed {
                    name: start.name.clone(),
                    why,
                }),
                cx,
            );
        };
        if let Err(e) = prepared {
            fail(self, e.to_string(), cx);
            return;
        }
        let id = match self.core.download(
            session,
            &start.key.path,
            &path_str(&staging),
            ConflictPolicy::Overwrite,
        ) {
            Ok(id) => id,
            Err(e) => {
                fail(self, core_error_text(&e), cx);
                return;
            }
        };
        let edit = self
            .edits
            .edits
            .entry(start.key.clone())
            .or_insert_with(|| Edit {
                key: start.key.clone(),
                name: start.name.clone(),
                source: start.source.clone(),
                record: record.clone(),
                file: file.clone(),
                state: EditState::new(start.remote),
                transfer: None,
                uploading: false,
                attempting: false,
                upload_version: None,
                pending_remote: None,
                launch_after: None,
                retry_at: None,
                conflict_paused: false,
                conflict_open: false,
                copy_to: None,
                skip_check: false,
                done: 0,
                total: None,
                synced_at: None,
            });
        edit.state.phase = Phase::Downloading;
        edit.transfer = Some(id);
        edit.pending_remote = Some(start.remote);
        edit.launch_after = start.launch;
        edit.done = 0;
        edit.total = None;
        self.edits
            .roles
            .insert(id, Role::EditDownload(start.key.clone()));
        self.edit_ensure_polling(cx);
        cx.notify();
    }

    // ---- 核心事件 ----

    /// 面板收到的每个核心事件先过这里（与普通传输队列互不干扰：只认自己登记的传输 id）。
    pub(crate) fn edit_on_core_event(&mut self, ev: &CoreEvent, cx: &mut Context<Self>) {
        if self.mode != PanelMode::Aux {
            return;
        }
        match ev {
            CoreEvent::SessionStateChanged { session, state } => match state {
                SessionState::Connected => {
                    self.edits.connected.insert(*session, true);
                    self.edit_reconnected(*session, cx);
                }
                SessionState::Closed(_) | SessionState::Connecting => {
                    self.edits.connected.insert(*session, false);
                }
            },
            CoreEvent::TransferStarted {
                transfer,
                total_bytes,
                ..
            } => self.edit_progress(*transfer, 0, *total_bytes, cx),
            CoreEvent::TransferProgress {
                transfer,
                done_bytes,
                total_bytes,
            } => self.edit_progress(*transfer, *done_bytes, *total_bytes, cx),
            CoreEvent::TransferFinished { transfer, outcome } => {
                if let Some(role) = self.edits.roles.remove(transfer) {
                    self.edit_transfer_finished(role, outcome, cx);
                }
            }
            _ => {}
        }
    }

    fn edit_progress(
        &mut self,
        transfer: TransferId,
        done: u64,
        total: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        match self.edits.roles.get(&transfer) {
            Some(Role::EditDownload(k) | Role::EditUpload(k)) => {
                if let Some(e) = self.edits.edits.get_mut(k) {
                    e.done = done;
                    e.total = total;
                }
            }
            Some(Role::Drag(_)) => {
                if let Some(d) = self.edits.drag.as_mut()
                    && d.transfer == Some(transfer)
                {
                    d.done = done;
                    d.total = total;
                }
            }
            None => return,
        }
        cx.notify();
    }

    fn edit_transfer_finished(
        &mut self,
        role: Role,
        outcome: &TransferOutcome,
        cx: &mut Context<Self>,
    ) {
        let why = match outcome {
            TransferOutcome::Completed { .. } => None,
            TransferOutcome::Cancelled => Some(None),
            TransferOutcome::Failed { message, .. } => Some(Some(render_message(message))),
        };
        match role {
            Role::EditDownload(key) => {
                if let Some(e) = self.edits.edits.get_mut(&key) {
                    e.transfer = None;
                }
                match why {
                    None => self.edit_download_done(key, cx),
                    Some(w) => self.edit_download_failed(key, w, cx),
                }
            }
            Role::EditUpload(key) => {
                if let Some(e) = self.edits.edits.get_mut(&key) {
                    e.transfer = None;
                }
                match why {
                    None => self.edit_after_upload(key, cx),
                    Some(w) => {
                        let text = w.unwrap_or_else(|| msg(Message::UiFilerCancelled {}));
                        self.edit_upload_failed(key, text, cx);
                    }
                }
            }
            Role::Drag(drag) => self.drag_transfer_finished(&drag, why, cx),
        }
    }

    fn edit_download_done(&mut self, key: EditKey, cx: &mut Context<Self>) {
        let now = self.now_ms();
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        let staging = e.record.join("download");
        let downloaded = staging.join(&e.name);
        let file = e.file.clone();
        let moved = (|| -> std::io::Result<()> {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if file.exists() {
                std::fs::remove_file(&file)?;
            }
            std::fs::rename(&downloaded, &file)
        })();
        let local = FileVersion::local(&file);
        let (Ok(()), Some(local)) = (moved, local) else {
            self.edit_download_failed(key, Some(msg(Message::UiOperationFailed2 {})), cx);
            return;
        };
        let _ = std::fs::remove_dir_all(&staging);
        let remote = e.pending_remote.take().unwrap_or(e.state.remote);
        // 刚下载完成本身不算保存：以此为基线，不立即回传。
        e.state.downloaded(remote, local);
        e.synced_at = Some(now);
        e.conflict_paused = false;
        e.retry_at = None;
        let _ = EditFiles::mark_synced(&e.record, &e.name, local);
        let launch = e.launch_after.take();
        if let Some(l) = launch {
            self.edit_launch(&file, l, cx);
        }
        self.edit_ensure_polling(cx);
        cx.notify();
    }

    /// 下载失败或取消：不启动编辑器；第一次下载丢弃半成品，重新下载则保留旧副本。
    fn edit_download_failed(&mut self, key: EditKey, why: Option<String>, cx: &mut Context<Self>) {
        let now = self.now_ms();
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        let _ = std::fs::remove_dir_all(e.record.join("download"));
        let name = e.name.clone();
        if e.state.synced.is_none() {
            let record = e.record.clone();
            self.edits.edits.remove(&key);
            EditFiles::remove_record(&record);
        } else {
            let local = FileVersion::local(&e.file);
            if e.state.has_changes(local) {
                e.state.phase = Phase::Pending;
                e.state.watcher.mark_pending(now);
            } else {
                e.state.phase = Phase::Synced;
            }
            e.launch_after = None;
        }
        if let Some(why) = why {
            self.say(msg(Message::RemoteEditOpenFailed { name, why }), cx);
        }
        cx.notify();
    }

    // ---- 保存检测（轮询）----

    pub(crate) fn edit_ensure_polling(&mut self, cx: &mut Context<Self>) {
        if self.edits.polling {
            return;
        }
        self.edits.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_MS))
                    .await;
                let keep = this.update(cx, |t, cx| t.edit_tick(cx)).unwrap_or(false);
                if !keep {
                    break;
                }
            }
            let _ = this.update(cx, |t, _| t.edits.polling = false);
        })
        .detach();
    }

    /// 一次轮询。返回是否还需要继续轮询。
    fn edit_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if self.edits.edits.is_empty() {
            return false;
        }
        let now = self.now_ms();
        let mut attempt = Vec::new();
        let mut ask = Vec::new();
        let mut visible = false;
        for (key, e) in &mut self.edits.edits {
            if e.state.synced.is_none() || e.state.phase == Phase::Downloading {
                visible = true;
                continue;
            }
            let local = FileVersion::local(&e.file);
            if e.state.watcher.observe(now, local) {
                // 发现新的保存：恢复"待回传"，提前唤起重试 / 冲突复查。
                if matches!(e.state.phase, Phase::Conflict) {
                    e.conflict_paused = false;
                }
                if !matches!(e.state.phase, Phase::Syncing) {
                    e.state.phase = Phase::Pending;
                }
                e.retry_at = None;
            }
            if e.state.phase != Phase::Synced
                || e.synced_at
                    .is_some_and(|t| now.saturating_sub(t) < STATUS_LINGER_MS + 400)
            {
                visible = true;
            }
            if e.uploading || e.attempting {
                continue;
            }
            if e.state.phase == Phase::Conflict {
                if !e.conflict_paused && !e.conflict_open {
                    ask.push(key.clone());
                }
                continue;
            }
            let due_save = e.state.watcher.due(now);
            let due_retry = e.retry_at.is_some_and(|t| now >= t);
            if due_save || due_retry {
                attempt.push(key.clone());
            }
        }
        for k in attempt {
            self.edit_attempt(&k, cx);
        }
        for k in ask {
            self.edit_ask_conflict(&k, cx);
        }
        if visible {
            cx.notify();
        }
        true
    }

    // ---- 回传 ----

    /// 开始一次回传：先读远端版本做冲突检查，再快照上传。
    fn edit_attempt(&mut self, key: &EditKey, cx: &mut Context<Self>) {
        let now = self.now_ms();
        let connected = self
            .edits
            .connected
            .get(&key.session)
            .copied()
            .unwrap_or(false);
        let Some(e) = self.edits.edits.get_mut(key) else {
            return;
        };
        e.retry_at = None;
        e.state.watcher.take_due(now);
        let local = FileVersion::local(&e.file);
        if !e.state.has_changes(local) {
            // 改回了原样（或文件暂时不在）：没有可回传的内容。
            if matches!(e.state.phase, Phase::Pending | Phase::Failed(_)) {
                e.state.phase = Phase::Synced;
            }
            return;
        }
        if !connected {
            // 暂时断线：保留待回传内容，原会话重连后继续（edit_reconnected）。
            e.state.phase = Phase::Pending;
            return;
        }
        e.attempting = true;
        let (session, path) = (key.session, key.path.clone());
        let core = self.core.clone();
        let key = key.clone();
        cx.spawn(async move |this, cx| {
            let r = match core.sftp_stat(session, &path).await {
                Ok(s) => Ok(Some(FileVersion::from_stat(&s))),
                Err(e) if is_not_found(&e) => Ok(None),
                Err(e) => Err(core_error_text(&e)),
            };
            let _ = this.update(cx, |t, cx| t.edit_after_stat(key, r, cx));
        })
        .detach();
    }

    fn edit_after_stat(
        &mut self,
        key: EditKey,
        stat: Result<Option<FileVersion>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        e.attempting = false;
        match stat {
            Err(why) => self.edit_upload_failed(key, why, cx),
            Ok(current) => {
                let skip = std::mem::take(&mut e.skip_check);
                let check = if skip {
                    ConflictCheck::Clean
                } else {
                    remote_edit::check_conflict(&e.state.remote, current.as_ref())
                };
                if check == ConflictCheck::Clean {
                    self.edit_start_upload(key, cx);
                } else {
                    // D5 未决：不自动覆盖，暂停并让用户选择（tick 负责弹对话框）。
                    e.state.phase = Phase::Conflict;
                    e.conflict_paused = false;
                    cx.notify();
                }
            }
        }
    }

    fn edit_ask_conflict(&mut self, key: &EditKey, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return; // 别的对话框占着，下一次轮询再试
        }
        let Some(e) = self.edits.edits.get_mut(key) else {
            return;
        };
        e.conflict_open = true;
        let spec = remote_edit::conflict_spec(&e.name, &e.source);
        self.pending = Some(Pending::EditConflict(key.clone()));
        cx.emit(FilerEvent::Dialog(spec));
    }

    /// 冲突对话框的结果。
    pub(crate) fn edit_on_conflict(
        &mut self,
        key: EditKey,
        result: &DialogResult,
        cx: &mut Context<Self>,
    ) {
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        e.conflict_open = false;
        match conflict_choice(result) {
            ConflictChoice::Overwrite => {
                e.skip_check = false;
                self.edit_start_upload(key, cx);
            }
            ConflictChoice::Redownload => {
                let (name, source) = (e.name.clone(), e.source.clone());
                let core = self.core.clone();
                let (session, path) = (key.session, key.path.clone());
                e.state.phase = Phase::Downloading; // 暂停轮询，直到下载结束
                cx.spawn(async move |this, cx| {
                    let r = core
                        .sftp_stat(session, &path)
                        .await
                        .map_err(|e| core_error_text(&e));
                    let _ = this.update(cx, |t, cx| match r {
                        Ok(s) => t.edit_begin_download(
                            EditStart {
                                key,
                                name,
                                source,
                                launch: None,
                                remote: FileVersion::from_stat(&s),
                            },
                            cx,
                        ),
                        Err(why) => {
                            if let Some(e) = t.edits.edits.get_mut(&key) {
                                e.state.phase = Phase::Conflict;
                                e.conflict_paused = true;
                            }
                            t.say(msg(Message::RemoteEditOpenFailed { name, why }), cx);
                        }
                    });
                })
                .detach();
            }
            ConflictChoice::Cancel => e.conflict_paused = true,
            ConflictChoice::SaveCopy => {
                // 先暂停轮询再弹名称框；名称框取消 / 失败都保持暂停。
                e.conflict_paused = true;
                let initial = remote_edit::default_copy_name(&e.name);
                self.pending = Some(Pending::EditCopyName(key));
                cx.emit(FilerEvent::Dialog(remote_edit::copy_spec(&initial, None)));
            }
        }
        cx.notify();
    }

    /// 另存副本名称框的结果：合规就把本地副本上传为同目录下的新文件（不覆盖已有文件）。
    pub(crate) fn edit_on_copy_name(
        &mut self,
        key: EditKey,
        result: &DialogResult,
        cx: &mut Context<Self>,
    ) {
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        match remote_edit::parse_copy_name(result, &e.name) {
            CopyOutcome::Cancel => e.conflict_paused = true,
            CopyOutcome::Invalid => {
                let initial = result.value(remote_edit::COPY_FIELD).unwrap_or_default();
                let spec =
                    remote_edit::copy_spec(initial, Some(&msg(Message::RemoteEditCopyPrompt {})));
                self.pending = Some(Pending::EditCopyName(key));
                cx.emit(FilerEvent::Dialog(spec));
            }
            CopyOutcome::Name(name) => {
                let connected = self
                    .edits
                    .connected
                    .get(&key.session)
                    .copied()
                    .unwrap_or(false);
                if !connected || e.uploading {
                    e.conflict_paused = true; // 断线或正好有回传在跑：保持冲突，状态行已有说明
                } else {
                    e.copy_to = Some(name);
                    self.edit_start_upload(key, cx);
                }
            }
        }
        cx.notify();
    }

    /// 把本地副本快照到 `upload/<名称>`（保证上传期间的新保存不影响这一轮），再发起上传。
    fn edit_start_upload(&mut self, key: EditKey, cx: &mut Context<Self>) {
        let now = self.now_ms();
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        let Some(v1) = FileVersion::local(&e.file) else {
            e.state.phase = Phase::Pending;
            e.state.watcher.mark_pending(now);
            return;
        };
        e.state.phase = Phase::Syncing;
        e.uploading = true;
        e.upload_version = Some(v1);
        e.done = 0;
        e.total = None;
        let dir = e.record.join("upload");
        let snap = dir.join(e.copy_to.as_ref().unwrap_or(&e.name));
        let file = e.file.clone();
        cx.spawn(async move |this, cx| {
            let job = cx.background_executor().spawn(async move {
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)
                    .and_then(|()| std::fs::copy(&file, &snap))
                    .map(|_| path_str(&snap))
                    .map_err(|e| e.to_string())
            });
            let r = job.await;
            let _ = this.update(cx, |t, cx| t.edit_snapshot_done(key, v1, r, cx));
        })
        .detach();
        cx.notify();
    }

    fn edit_snapshot_done(
        &mut self,
        key: EditKey,
        v1: FileVersion,
        snap: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let now = self.now_ms();
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        let snap = match snap {
            Ok(s) => s,
            Err(why) => {
                self.edit_upload_failed(key, why, cx);
                return;
            }
        };
        // 复制期间本地又变了：这份快照可能是半份内容，丢弃，等下一次防抖。
        if FileVersion::local(&e.file) != Some(v1) {
            e.uploading = false;
            e.state.phase = Phase::Pending;
            e.state.watcher.mark_pending(now);
            let _ = std::fs::remove_dir_all(e.record.join("upload"));
            return;
        }
        let dir = remote_parent(&key.path);
        // 另存副本不能覆盖远端已有文件；普通回传（含用户选了覆盖）按名覆盖。
        let policy = if e.copy_to.is_some() {
            ConflictPolicy::Fail
        } else {
            ConflictPolicy::Overwrite
        };
        match self.core.upload(key.session, &snap, &dir, policy) {
            Ok(id) => {
                e.transfer = Some(id);
                self.edits.roles.insert(id, Role::EditUpload(key));
            }
            Err(err) => self.edit_upload_failed(key, core_error_text(&err), cx),
        }
    }

    /// 上传成功：读远端新版本，登记已同步，刷新所在目录的属性。
    fn edit_after_upload(&mut self, key: EditKey, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let (session, path) = (key.session, key.path.clone());
        cx.spawn(async move |this, cx| {
            let r = core
                .sftp_stat(session, &path)
                .await
                .ok()
                .map(|s| FileVersion::from_stat(&s));
            let _ = this.update(cx, |t, cx| t.edit_upload_done(key, r, cx));
        })
        .detach();
    }

    fn edit_upload_done(
        &mut self,
        key: EditKey,
        remote: Option<FileVersion>,
        cx: &mut Context<Self>,
    ) {
        let now = self.now_ms();
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        e.uploading = false;
        let _ = std::fs::remove_dir_all(e.record.join("upload"));
        let snap = e.upload_version.take().unwrap_or_default();
        let local_now = FileVersion::local(&e.file);
        if e.copy_to.take().is_some() {
            // 另存副本（照 mac 版）：原远端文件的基线不动，下次保存仍会提示冲突。
            e.state.copied(snap, local_now);
            e.synced_at = Some(now);
            e.conflict_paused = true;
            let _ = EditFiles::mark_synced(&e.record, &e.name, snap);
            let parent = remote_parent(&key.path);
            if let Some(&pane) = self.by_session.get(&key.session)
                && self.panes[pane].browse.path == parent
            {
                self.refresh(pane, cx);
            }
            cx.notify();
            return;
        }
        if remote.is_none() {
            e.skip_check = true;
        }
        let remote = remote.unwrap_or(e.state.remote);
        e.state.uploaded(remote, snap, local_now);
        e.synced_at = Some(now);
        let _ = EditFiles::mark_synced(&e.record, &e.name, snap);
        if e.state.phase == Phase::Pending && !e.state.watcher.has_pending() {
            // 回传期间又保存过：本轮完成后继续处理，不能被"已同步"盖掉。
            e.state.watcher.mark_pending(now);
        }
        // 刷新条目所在目录的属性，但不把用户正在浏览的别的目录切回来（§9.2）。
        let parent = remote_parent(&key.path);
        if let Some(&pane) = self.by_session.get(&key.session)
            && self.panes[pane].browse.path == parent
        {
            self.refresh(pane, cx);
        }
        cx.notify();
    }

    /// 回传失败：记原因，按 6 / 12 / 24 / 48 / 60 秒退避重试；相同错误不反复弹。
    fn edit_upload_failed(&mut self, key: EditKey, why: String, cx: &mut Context<Self>) {
        let now = self.now_ms();
        let Some(e) = self.edits.edits.get_mut(&key) else {
            return;
        };
        e.uploading = false;
        e.attempting = false;
        e.upload_version = None;
        let _ = std::fs::remove_dir_all(e.record.join("upload"));
        if e.copy_to.take().is_some() {
            // 另存副本失败（如同名文件已存在）：回到冲突并保持暂停，不自动重试，下一次保存再询问。
            e.state.phase = Phase::Conflict;
            e.conflict_paused = true;
            let line = remote_edit::status_line(
                &e.source,
                &e.key.path,
                &msg(Message::RemoteEditFailed { why }),
            );
            self.say(line, cx);
            cx.notify();
            return;
        }
        let delay = e.state.failed(why.clone());
        e.retry_at = Some(now + delay);
        if e.state.should_announce(&why) {
            let line = remote_edit::status_line(
                &e.source,
                &e.key.path,
                &msg(Message::RemoteEditFailed { why }),
            );
            self.say(line, cx);
        }
        cx.notify();
    }

    /// 原会话重新连上：立即继续回传尚未发送的保存。
    fn edit_reconnected(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let keys: Vec<EditKey> = self
            .edits
            .edits
            .iter()
            .filter(|(k, e)| {
                k.session == session
                    && !e.uploading
                    && !e.attempting
                    && matches!(e.state.phase, Phase::Pending | Phase::Failed(_))
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            self.edit_attempt(&k, cx);
        }
    }

    // ---- 对话框结果 ----

    /// 大文件确认的结果。
    pub(crate) fn edit_on_large(
        &mut self,
        start: EditStart,
        result: &DialogResult,
        cx: &mut Context<Self>,
    ) {
        if remote_edit::large_confirmed(result) {
            self.edit_begin_download(start, cx);
        }
    }

    // ---- 关闭 / 退出 ----

    /// 标签永久关闭：结束它的监测；已全部回传的副本清理，有未回传修改的保留并告知位置（§9.3）。
    pub(crate) fn edit_close_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let keys: Vec<EditKey> = self
            .edits
            .edits
            .keys()
            .filter(|k| k.session == session)
            .cloned()
            .collect();
        let mut retained = Vec::new();
        for k in keys {
            if let Some(e) = self.edits.edits.remove(&k) {
                if let Some(id) = e.transfer {
                    self.edits.roles.remove(&id);
                    self.core.cancel_transfer(id);
                }
                if Self::edit_retire(&e) == CloseAction::Retain {
                    retained.push(e.file.clone());
                }
            }
        }
        self.edits.opening.retain(|k| k.session != session);
        self.edits.connected.remove(&session);
        if self
            .edits
            .drag
            .as_ref()
            .is_some_and(|d| d.session == session)
        {
            self.edits.drag = None;
        }
        if let Some(first) = retained.first() {
            let dir = first.parent().map_or_else(|| path_str(first), path_str);
            self.say(
                msg(Message::RemoteEditRetained {
                    count: retained.len() as i64,
                    dir,
                }),
                cx,
            );
        }
    }

    /// 退出应用：同上，但没法再提示用户（副本保留在临时目录下的 `kanso-remote-edit`）。
    pub(crate) fn edit_close_all(&mut self) {
        let edits: Vec<Edit> = self.edits.edits.drain().map(|(_, e)| e).collect();
        for e in &edits {
            if let Some(id) = e.transfer {
                self.core.cancel_transfer(id);
            }
            Self::edit_retire(e);
        }
        self.edits.roles.clear();
    }

    /// 按关闭规则处理一份副本的磁盘内容，返回采取的动作。
    fn edit_retire(e: &Edit) -> CloseAction {
        let local = FileVersion::local(&e.file);
        let action = close_action(&e.state.phase, e.state.has_changes(local));
        // 临时的下载 / 回传快照目录总是清掉。
        let _ = std::fs::remove_dir_all(e.record.join("download"));
        let _ = std::fs::remove_dir_all(e.record.join("upload"));
        if action == CloseAction::Clean {
            EditFiles::remove_record(&e.record);
        }
        action
    }

    // ---- 状态行 ----

    /// 该会话要显示的状态：拖出准备 > 正在下载 > 尚未同步 > 刚同步完成的几秒内。
    pub(crate) fn edit_status(&self, session: SessionId) -> Option<EditStatus> {
        if let Some(d) = self.edits.drag.as_ref().filter(|d| d.session == session) {
            return Some(EditStatus {
                text: d.title.clone(),
                is_error: false,
                progress: true,
                fraction: frac(d.done, d.total),
                cancel: None,
            });
        }
        let now = self.now_ms();
        let mut list: Vec<&Edit> = self
            .edits
            .edits
            .values()
            .filter(|e| e.key.session == session)
            .collect();
        list.sort_by(|a, b| a.key.path.cmp(&b.key.path));
        let pick = list
            .iter()
            .find(|e| e.state.phase == Phase::Downloading)
            .or_else(|| list.iter().find(|e| e.state.phase != Phase::Synced))
            .or_else(|| {
                list.iter().find(|e| {
                    e.synced_at
                        .is_some_and(|t| now.saturating_sub(t) < STATUS_LINGER_MS)
                })
            })?;
        let downloading = pick.state.phase == Phase::Downloading;
        Some(EditStatus {
            text: remote_edit::status_line(
                &pick.source,
                &pick.key.path,
                &pick.state.phase.text(&pick.name),
            ),
            is_error: pick.state.phase.is_error(),
            progress: downloading,
            fraction: frac(pick.done, pick.total),
            cancel: if downloading { pick.transfer } else { None },
        })
    }

    /// 取消一次下载（编辑准备或拖出准备）。
    pub(crate) fn edit_cancel_transfer(&mut self, id: TransferId, cx: &mut Context<Self>) {
        self.core.cancel_transfer(id);
        cx.notify();
    }

    /// 某栏对应的会话。
    pub(crate) fn session_of_pane(&self, idx: usize) -> Option<SessionId> {
        self.by_session
            .iter()
            .find(|&(_, &i)| i == idx)
            .map(|(s, _)| *s)
    }
}
