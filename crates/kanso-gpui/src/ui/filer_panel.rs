//! 文件面板实体（docs/spec/filer.md）：辅助 Filer（跟随活动标签、每个标签各记各的目录）与纯 SFTP 标签的
//! 双栏文件浏览器共用同一个实体，区别只在 [`PanelMode`]。
//!
//! 分工：
//! - 纯逻辑在库里：路径（`app::fspath`）、本地文件操作（`app::local_fs`）、选中与浏览请求状态
//!   （`app::filer_state`）、传输队列（`app::transfer_queue`）、对话框内容（`app::filer_ops`）；
//! - 本文件：目录读取与后台任务、操作的执行、向外壳发事件（菜单、对话框、提示）；
//! - 绘制在 `filer_panel_view.rs`。
//!
//! 菜单与模态对话框由外壳（`ShellView`）承载（它们要按窗口坐标定位、盖住整个窗口）：
//! 面板发 [`FilerEvent::Menu`] / [`FilerEvent::Dialog`]，外壳把结果交回
//! [`FilerPanel::on_menu_action`] / [`FilerPanel::on_dialog_result`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    Point, ScrollStrategy, Subscription, Task, UniformListScrollHandle, Window, point, px,
};
use kanso_core::Core;
use kanso_gpui::app::filer_ops::{
    self, DeleteTarget, NameKind, NameOutcome, conflict_decision, conflict_spec, delete_confirmed,
    delete_spec, name_error_text, name_spec, parse_name,
};
use kanso_gpui::app::filer_state::{Browse, Phase, Selection};
use kanso_gpui::app::filer_text::{t, tf};
use kanso_gpui::app::fspath::{self, PathStyle};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::local_fs::{self, CopyPolicy, LocalErrorKind};
use kanso_gpui::app::transfer_queue::{
    Action, BatchEnd, Finish, Kind, TransferItem, TransferQueue,
};
use kanso_gpui::filer::{BatchBuffer, DirModel, ListEnd, Sort};
use kanso_gpui::widgets::dialog::{DialogResult, DialogSpec};
use kanso_gpui::widgets::menu::MenuItem;
use kanso_types::event::{SessionState, TransferOutcome};
use kanso_types::transfer::ConflictPolicy;
use kanso_types::{CoreEvent, SessionId};

use kanso_gpui::widgets::style::Appearance;

use super::log;
use super::shell::Theme;
use super::widgets::text_input::{TextInput, TextInputEvent};

/// 界面两次并入读取缓冲之间至少隔这么久（与原型的远程列表一致）。
const APPLY_INTERVAL: Duration = Duration::from_millis(16);
/// 本地目录每批条目数。
const LOCAL_BATCH: usize = 1000;

/// 面板形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelMode {
    /// 辅助 Filer：位于标签栏与终端之间，跟随活动标签。
    Aux,
    /// 纯 SFTP 标签的主工作区：左栏远端、右栏本地。
    Dual,
}

/// 一栏浏览的是哪里。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// 本机。
    Local,
    /// 某个 SSH / SFTP 会话的远端。
    Remote(SessionId),
}

/// 面板向外壳发的事件。
#[derive(Clone, Debug)]
pub(crate) enum FilerEvent {
    /// 请在窗口坐标 `position` 弹出菜单；选中后交给 [`FilerPanel::on_menu_action`]。
    Menu {
        items: Vec<MenuItem>,
        position: Point<Pixels>,
    },
    /// 请弹出模态对话框；结果交给 [`FilerPanel::on_dialog_result`]。
    Dialog(DialogSpec),
    /// 一句提示（状态栏上方的浮条）。
    Notice(String),
    /// “定位终端目录”按钮。
    Locate,
}

/// 等待对话框结果的操作。
#[derive(Clone, Debug)]
enum Pending {
    Delete {
        pane: usize,
        targets: Vec<DeleteTarget>,
    },
    Name {
        pane: usize,
        kind: NameKind,
        old: Option<String>,
    },
    Conflict {
        pane: usize,
    },
}

/// 一栏。
pub(crate) struct Pane {
    pub(crate) source: Source,
    /// 来源标识（“本地”或会话名 + 主机）。
    pub(crate) identity: String,
    pub(crate) style: PathStyle,
    pub(crate) browse: Browse,
    pub(crate) model: DirModel,
    pub(crate) sel: Selection,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) path_input: Entity<TextInput>,
    /// 当前来源的家目录（取到之后记下，供“回家”按钮）。
    pub(crate) home: Option<String>,
    /// 远端是否已连接。
    pub(crate) connected: bool,
    pub(crate) queue: TransferQueue,
    /// 本地复制的取消标志（后台线程每块检查）。
    copy_cancel: Arc<AtomicBool>,
    list_task: Option<Task<()>>,
    /// 连接成功后要进入的初始目录（定位请求先于连接完成时）。
    initial: Option<String>,
}

/// 文件面板。
pub(crate) struct FilerPanel {
    pub(crate) core: Arc<Core>,
    pub(crate) mode: PanelMode,
    pub(crate) panes: Vec<Pane>,
    /// Aux：当前显示的栏；Dual：拥有键盘焦点的栏。
    pub(crate) shown: usize,
    by_session: HashMap<SessionId, usize>,
    pub(crate) focus: FocusHandle,
    pub(crate) tz_min: i32,
    /// 最近一次绘制用的主题（列表行的处理函数里取色用）。
    pub(crate) cached_theme: Theme,
    pending: Option<Pending>,
    t0: Instant,
    /// 收尾进度条的收起重绘是否已排程。
    linger_scheduled: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<FilerEvent> for FilerPanel {}

impl Focusable for FilerPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

fn home_dir() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default()
}

impl FilerPanel {
    /// 辅助 Filer（还没有任何栏，等 [`Self::show_session`]）。
    pub(crate) fn new_aux(core: Arc<Core>, cx: &mut Context<Self>) -> Self {
        FilerPanel::new(core, PanelMode::Aux, cx)
    }

    /// 纯 SFTP 标签的双栏：左栏远端，右栏本地。
    pub(crate) fn new_dual(
        core: Arc<Core>,
        session: SessionId,
        identity: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut p = FilerPanel::new(core, PanelMode::Dual, cx);
        p.add_pane(Source::Remote(session), identity, window, cx);
        p.add_pane(Source::Local, t("ui-filer-local"), window, cx);
        p.start_pane(0, None, cx);
        p.start_pane(1, None, cx);
        p
    }

    fn new(core: Arc<Core>, mode: PanelMode, cx: &mut Context<Self>) -> Self {
        FilerPanel {
            core,
            mode,
            panes: Vec::new(),
            shown: 0,
            by_session: HashMap::new(),
            focus: cx.focus_handle(),
            tz_min: super::tz::local_offset_minutes(),
            cached_theme: Theme {
                appearance: Appearance::Dark,
            },
            pending: None,
            t0: Instant::now(),
            linger_scheduled: false,
            _subs: Vec::new(),
        }
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    fn style_of(source: Source) -> PathStyle {
        match source {
            Source::Local => PathStyle::native(),
            Source::Remote(_) => PathStyle::Posix,
        }
    }

    fn add_pane(
        &mut self,
        source: Source,
        identity: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let input = cx.new(|cx| TextInput::new(window, cx));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, ev: &TextInputEvent, window, cx| {
                this.on_path_input(input, ev, window, cx)
            },
        );
        self._subs.push(sub);
        let connected = match source {
            Source::Local => true,
            Source::Remote(s) => self
                .core
                .session_info(s)
                .is_some_and(|i| i.state == SessionState::Connected),
        };
        let mut browse = Browse::default();
        if !connected {
            browse.phase = Phase::Connecting;
        }
        self.panes.push(Pane {
            source,
            identity,
            style: Self::style_of(source),
            browse,
            model: DirModel::new(Sort::default()),
            sel: Selection::default(),
            scroll: UniformListScrollHandle::new(),
            path_input: input,
            home: None,
            connected,
            queue: TransferQueue::default(),
            copy_cancel: Arc::new(AtomicBool::new(false)),
            list_task: None,
            initial: None,
        });
        self.panes.len() - 1
    }

    /// 辅助 Filer 切到某个标签：没有这个标签的栏就建一个（本地从家目录开始，SSH 从远端家目录开始），
    /// 已有就恢复它记住的目录。`None` 表示没有可浏览的标签。
    pub(crate) fn show_session(
        &mut self,
        session: Option<(SessionId, Source, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((sid, source, identity)) = session else {
            return;
        };
        let idx = match self.by_session.get(&sid) {
            Some(&i) => i,
            None => {
                let i = self.add_pane(source, identity.clone(), window, cx);
                self.by_session.insert(sid, i);
                self.start_pane(i, None, cx);
                i
            }
        };
        self.panes[idx].identity = identity;
        self.shown = idx;
        cx.notify();
    }

    /// 标签关闭：丢掉它的栏状态（栏本身留在向量里避免下标错位，只是不再被引用并停止读取）。
    pub(crate) fn forget_session(&mut self, sid: SessionId) {
        if let Some(i) = self.by_session.remove(&sid) {
            let p = &mut self.panes[i];
            p.list_task = None;
            p.browse.on_disconnected();
            p.model.clear();
            p.sel.clear();
            if let Some(id) = p.queue.current_id() {
                self.core.cancel_transfer(id);
            }
        }
    }

    /// 某个标签当前是否有栏。
    pub(crate) fn has_session(&self, sid: SessionId) -> bool {
        self.by_session.contains_key(&sid)
    }

    // ---- 启动一栏 ----

    /// 栏刚建立或连接成功：进入初始目录（`path` 优先，其次来源的家目录）。
    fn start_pane(&mut self, idx: usize, path: Option<String>, cx: &mut Context<Self>) {
        let pane = &mut self.panes[idx];
        match (pane.source, path) {
            (_, Some(p)) => self.navigate(idx, p, cx),
            (Source::Local, None) => {
                let home = home_dir();
                let target = if home.is_empty() {
                    if cfg!(windows) {
                        fspath::THIS_PC.to_owned()
                    } else {
                        "/".to_owned()
                    }
                } else {
                    home.clone()
                };
                self.panes[idx].home = Some(target.clone());
                self.navigate(idx, target, cx);
            }
            (Source::Remote(s), None) => {
                if !pane.connected {
                    pane.browse.phase = Phase::Connecting;
                    cx.notify();
                    return;
                }
                let core = self.core.clone();
                // 先占住一个请求代数，让后到的家目录结果与用户随后的导航竞争时以后者为准。
                let generation = pane.browse.begin("");
                pane.browse.pending = None;
                pane.browse.phase = Phase::Loading;
                cx.spawn(async move |this, cx| {
                    let r = core.sftp_home(s).await;
                    let _ = this.update(cx, |t, cx| {
                        let Some(p) = t.panes.get_mut(idx) else {
                            return;
                        };
                        // 期间用户已经导航：家目录结果作废。
                        if p.browse.generation != generation {
                            return;
                        }
                        match r {
                            Ok(home) => {
                                p.home = Some(home.clone());
                                t.navigate(idx, home, cx);
                            }
                            Err(e) => {
                                p.browse.phase = Phase::Failed;
                                p.browse.error = Some(kanso_gpui::app::filer_state::BrowseError {
                                    target: "~".into(),
                                    text: core_error_text(&e),
                                });
                                cx.notify();
                            }
                        }
                    });
                })
                .detach();
                cx.notify();
            }
        }
    }

    /// 定位终端目录等外部请求：让标签 `sid` 的栏直接读取 `path`（不先显示家目录再跳转）。
    pub(crate) fn navigate_session(
        &mut self,
        sid: SessionId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(&idx) = self.by_session.get(&sid) else {
            return;
        };
        let _ = window;
        // 不改变 `shown`：请求期间用户可能已经切走，迟到的结果只更新原标签自己的栏。
        if self.panes[idx].connected {
            self.navigate(idx, path, cx);
        } else {
            self.panes[idx].initial = Some(path);
        }
    }

    // ---- 目录读取 ----

    /// 进入目录 `path`：先读取，成功后才换掉当前列表；失败时保留旧列表并显示原因。
    pub(crate) fn navigate(&mut self, idx: usize, path: String, cx: &mut Context<Self>) {
        let (source, style) = {
            let p = &self.panes[idx];
            (p.source, p.style)
        };
        let generation = self.panes[idx].browse.begin(&path);
        log(&format!(
            "Filer 读取 {path:?}（栏 {idx}，请求 #{generation}）"
        ));
        self.panes[idx].list_task = None;

        let buffer = Arc::new(Mutex::new(BatchBuffer::new(generation)));
        let (wake_tx, mut wake_rx) = futures::channel::mpsc::unbounded::<()>();

        let reader: Task<()> = match source {
            Source::Local if fspath::is_virtual_root(style, &path) => {
                let buffer = buffer.clone();
                cx.background_executor().spawn(async move {
                    let drives = local_fs::drives();
                    let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    let wake = b.push(drives) | b.finish(ListEnd::Done);
                    if wake {
                        let _ = wake_tx.unbounded_send(());
                    }
                })
            }
            Source::Local => {
                let buffer = buffer.clone();
                let dir = PathBuf::from(&path);
                cx.background_executor().spawn(async move {
                    let result = local_fs::list_dir_streaming(&dir, LOCAL_BATCH, |batch| {
                        let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                        if b.is_cancelled() {
                            return false;
                        }
                        if b.push(batch) {
                            let _ = wake_tx.unbounded_send(());
                        }
                        true
                    });
                    let end = match result {
                        Ok(()) => ListEnd::Done,
                        Err(e) => ListEnd::Failed(e.text()),
                    };
                    let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if b.finish(end) {
                        let _ = wake_tx.unbounded_send(());
                    }
                })
            }
            Source::Remote(session) => {
                let mut stream = match self.core.sftp_list(session, &path) {
                    Ok(s) => s,
                    Err(e) => {
                        let p = &mut self.panes[idx];
                        p.browse.on_failed(generation, core_error_text(&e));
                        cx.notify();
                        return;
                    }
                };
                let buffer = buffer.clone();
                cx.background_executor().spawn(async move {
                    loop {
                        if buffer
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .is_cancelled()
                        {
                            return;
                        }
                        let r = stream.next_batch().await;
                        let (wake, last) = {
                            let mut b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                            match r {
                                Ok(Some(batch)) => (b.push(batch), false),
                                Ok(None) => (b.finish(ListEnd::Done), true),
                                Err(e) => (b.finish(ListEnd::Failed(core_error_text(&e))), true),
                            }
                        };
                        if wake {
                            let _ = wake_tx.unbounded_send(());
                        }
                        if last {
                            return;
                        }
                    }
                })
            }
        };

        self.panes[idx].list_task = Some(cx.spawn(async move |this, cx| {
            let _reader = reader; // 随本任务一起被丢弃（取消）
            while wake_rx.next().await.is_some() {
                let keep = this.update(cx, |t, cx| t.apply_buffer(idx, generation, &buffer, cx));
                if !keep.unwrap_or(false) {
                    return;
                }
                cx.background_executor().timer(APPLY_INTERVAL).await;
            }
        }));
        cx.notify();
    }

    /// 把缓冲里累计的批次一次并入并重绘一次。返回 `false` 表示不必再等。
    fn apply_buffer(
        &mut self,
        idx: usize,
        generation: u64,
        buffer: &Mutex<BatchBuffer>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self.panes.get_mut(idx) else {
            return false;
        };
        let drained = buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(pane.browse.generation);
        let Some(d) = drained else { return false };
        if generation != pane.browse.generation {
            return false;
        }
        if !d.entries.is_empty() {
            match pane.browse.on_data(generation) {
                None => return false,
                Some(clear) => {
                    if clear {
                        pane.model.clear();
                        pane.sel.clear();
                        pane.scroll
                            .0
                            .borrow()
                            .base_handle
                            .set_offset(point(px(0.0), px(0.0)));
                    }
                    pane.model.push_batch(d.entries);
                }
            }
            cx.notify();
        }
        match d.end {
            None => true,
            Some(ListEnd::Done) => {
                if let Some(clear) = pane.browse.on_done(generation) {
                    if clear {
                        pane.model.clear();
                        pane.sel.clear();
                    }
                    let path = pane.browse.path.clone();
                    pane.path_input.update(cx, |i, cx| i.set_text(&path, cx));
                }
                cx.notify();
                false
            }
            Some(ListEnd::Failed(text)) => {
                if pane.browse.on_failed(generation, text) {
                    let path = pane.browse.path.clone();
                    pane.path_input.update(cx, |i, cx| i.set_text(&path, cx));
                }
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn refresh(&mut self, idx: usize, cx: &mut Context<Self>) {
        let path = self.panes[idx].browse.path.clone();
        let source = self.panes[idx].source;
        // 还没有任何成功目录（例如家目录读取失败）：刷新就是重新取家目录。
        if path.is_empty() && !fspath::is_virtual_root(self.panes[idx].style, &path) {
            self.start_pane(idx, None, cx);
            return;
        }
        let _ = source;
        self.navigate(idx, path, cx);
    }

    pub(crate) fn go_up(&mut self, idx: usize, cx: &mut Context<Self>) {
        let p = &self.panes[idx];
        let parent = fspath::parent(p.style, &p.browse.path);
        if parent != p.browse.path {
            self.navigate(idx, parent, cx);
        }
    }

    pub(crate) fn go_home(&mut self, idx: usize, cx: &mut Context<Self>) {
        match self.panes[idx].home.clone() {
            Some(h) => self.navigate(idx, h, cx),
            None => self.start_pane(idx, None, cx),
        }
    }

    pub(crate) fn toggle_hidden(&mut self, idx: usize, cx: &mut Context<Self>) {
        let p = &mut self.panes[idx];
        let hide = !p.model.hide_hidden();
        p.model.set_hide_hidden(hide);
        p.sel.retain_visible(&p.model);
        cx.notify();
    }

    // ---- 路径栏 ----

    fn on_path_input(
        &mut self,
        input: &Entity<TextInput>,
        ev: &TextInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.panes.iter().position(|p| &p.path_input == input) else {
            return;
        };
        match ev {
            TextInputEvent::Submit(text) => {
                let p = &self.panes[idx];
                let home = (p.source == Source::Local).then(home_dir);
                let target = fspath::resolve_input(
                    p.style,
                    &p.browse.path,
                    text,
                    home.as_deref().filter(|h| !h.is_empty()),
                );
                self.shown = idx;
                self.navigate(idx, target, cx);
                window.focus(&self.focus);
            }
            TextInputEvent::Escape => {
                let path = self.panes[idx].browse.path.clone();
                input.update(cx, |i, cx| i.set_text(&path, cx));
                window.focus(&self.focus);
            }
            _ => {}
        }
    }

    /// 路径输入框是否有键盘焦点（此时列表快捷键让路）。
    pub(crate) fn path_input_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.panes
            .iter()
            .any(|p| p.path_input.focus_handle(cx).is_focused(window))
    }

    // ---- 核心事件 ----

    /// 外壳转发的核心事件：会话状态、传输进度。
    pub(crate) fn on_core_event(&mut self, ev: &CoreEvent, cx: &mut Context<Self>) {
        match ev {
            CoreEvent::SessionStateChanged { session, state } => {
                let idxs: Vec<usize> = self
                    .panes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.source == Source::Remote(*session))
                    .map(|(i, _)| i)
                    .collect();
                for idx in idxs {
                    match state {
                        SessionState::Connected => {
                            let p = &mut self.panes[idx];
                            let was = p.connected;
                            p.connected = true;
                            // 重连成功：重新读取当前远端目录；第一次连接：进入初始目录。
                            if !was || p.browse.phase == Phase::Disconnected {
                                let initial = p.initial.take();
                                let has_dir = !p.browse.path.is_empty();
                                match (initial, has_dir) {
                                    (Some(path), _) => self.navigate(idx, path, cx),
                                    (None, true) => self.refresh(idx, cx),
                                    (None, false) => self.start_pane(idx, None, cx),
                                }
                            }
                        }
                        SessionState::Closed(_) => {
                            let p = &mut self.panes[idx];
                            p.connected = false;
                            p.list_task = None;
                            p.browse.on_disconnected();
                            cx.notify();
                        }
                        SessionState::Connecting => {
                            let p = &mut self.panes[idx];
                            p.connected = false;
                            if p.browse.path.is_empty() {
                                p.browse.phase = Phase::Connecting;
                            }
                            cx.notify();
                        }
                    }
                }
            }
            CoreEvent::TransferStarted {
                transfer,
                total_bytes,
                ..
            } => {
                let now = self.now_ms();
                let _ = now;
                for p in &mut self.panes {
                    p.queue.on_started(*transfer, *total_bytes);
                }
                cx.notify();
            }
            CoreEvent::TransferProgress {
                transfer,
                done_bytes,
                total_bytes,
            } => {
                let now = self.now_ms();
                for p in &mut self.panes {
                    p.queue
                        .on_progress(*transfer, *done_bytes, *total_bytes, now);
                }
                cx.notify();
            }
            CoreEvent::TransferFinished { transfer, outcome } => {
                let now = self.now_ms();
                let finish = match outcome {
                    TransferOutcome::Completed { skipped } => {
                        Finish::Completed { skipped: *skipped }
                    }
                    TransferOutcome::Cancelled => Finish::Cancelled,
                    TransferOutcome::Failed {
                        message,
                        already_exists,
                    } => Finish::Failed {
                        text: kanso_gpui::app::launcher::render_message(message),
                        already_exists: *already_exists,
                    },
                };
                let mut hit = None;
                for (i, p) in self.panes.iter_mut().enumerate() {
                    if let Some(action) = p.queue.on_finished(*transfer, finish.clone(), now) {
                        hit = Some((i, action));
                        break;
                    }
                }
                if let Some((i, action)) = hit {
                    self.drive(i, action, cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    // ---- 传输 ----

    /// 队列所在的栏：双栏共用第 0 栏的队列；辅助面板用各自标签的栏。
    fn queue_idx(&self, idx: usize) -> usize {
        match self.mode {
            PanelMode::Dual => 0,
            PanelMode::Aux => idx,
        }
    }

    fn session_of(&self, idx: usize) -> Option<SessionId> {
        match self.panes.get(idx)?.source {
            Source::Remote(s) => Some(s),
            Source::Local => None,
        }
    }

    /// 推进队列：把 [`Action`] 落实成核心调用 / 对话框 / 刷新，循环处理发起失败产生的后续动作。
    fn drive(&mut self, qidx: usize, first: Action, cx: &mut Context<Self>) {
        let mut action = first;
        loop {
            match action {
                Action::Wait => return,
                Action::Ask(conflict) => {
                    self.pending = Some(Pending::Conflict { pane: qidx });
                    cx.emit(FilerEvent::Dialog(conflict_spec(&conflict)));
                    return;
                }
                Action::Finished(result) => {
                    self.on_batch_finished(qidx, &result.end, result.skipped, cx);
                    return;
                }
                Action::Start(item, policy) => {
                    let now = self.now_ms();
                    match self.start_item(qidx, &item, policy, cx) {
                        Ok(()) => return,
                        Err(text) => {
                            action = self.panes[qidx].queue.start_failed(text, now);
                        }
                    }
                }
            }
        }
    }

    fn start_item(
        &mut self,
        qidx: usize,
        item: &TransferItem,
        policy: ConflictPolicy,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match item.kind {
            Kind::Upload | Kind::Download => {
                let Some(session) = self.session_of(self.remote_idx(qidx)) else {
                    return Err(t("ui-filer-not-remote"));
                };
                let r = if item.kind == Kind::Upload {
                    self.core
                        .upload(session, &item.source, &item.dest_dir, policy)
                } else {
                    self.core
                        .download(session, &item.source, &item.dest_dir, policy)
                };
                match r {
                    Ok(id) => {
                        self.panes[qidx].queue.bind(id);
                        Ok(())
                    }
                    Err(e) => Err(core_error_text(&e)),
                }
            }
            Kind::Copy => {
                self.spawn_local_copy(qidx, item, policy, cx);
                Ok(())
            }
        }
    }

    /// 传输使用的远端栏：双栏是第 0 栏，辅助面板是队列所在的栏本身。
    fn remote_idx(&self, qidx: usize) -> usize {
        match self.mode {
            PanelMode::Dual => 0,
            PanelMode::Aux => qidx,
        }
    }

    fn spawn_local_copy(
        &mut self,
        qidx: usize,
        item: &TransferItem,
        policy: ConflictPolicy,
        cx: &mut Context<Self>,
    ) {
        enum Msg {
            Progress(u64, u64),
            Done(Result<local_fs::CopyReport, local_fs::LocalError>),
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.panes[qidx].copy_cancel = cancel.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Msg>();
        let sources = vec![PathBuf::from(&item.source)];
        let dest = PathBuf::from(&item.dest_dir);
        let copy_policy = match policy {
            ConflictPolicy::Fail => CopyPolicy::Fail,
            ConflictPolicy::Overwrite => CopyPolicy::Overwrite,
            ConflictPolicy::Skip => CopyPolicy::Skip,
        };
        let job = cx.background_executor().spawn(async move {
            let mut last = Instant::now() - Duration::from_secs(1);
            let tx2 = tx.clone();
            let mut progress = |done: u64, total: u64| {
                if last.elapsed() >= Duration::from_millis(50) || done == total {
                    last = Instant::now();
                    let _ = tx2.unbounded_send(Msg::Progress(done, total));
                }
            };
            let r = local_fs::copy_into(&sources, &dest, copy_policy, &cancel, &mut progress);
            let _ = tx.unbounded_send(Msg::Done(r));
        });
        cx.spawn(async move |this, cx| {
            let _job = job;
            while let Some(msg) = rx.next().await {
                let alive = this.update(cx, |t, cx| {
                    let now = t.now_ms();
                    match msg {
                        Msg::Progress(done, total) => {
                            t.panes[qidx].queue.on_local_progress(done, total, now);
                            cx.notify();
                            true
                        }
                        Msg::Done(r) => {
                            let finish = match r {
                                Ok(report) => {
                                    if report.already_there > 0 {
                                        cx.emit(FilerEvent::Notice(
                                            kanso_gpui::app::filer_text::t("ui-filer-already-here"),
                                        ));
                                    }
                                    Finish::Completed {
                                        skipped: report.skipped_existing,
                                    }
                                }
                                Err(e) if e.kind == LocalErrorKind::Cancelled => Finish::Cancelled,
                                Err(e) => Finish::Failed {
                                    already_exists: e.kind == LocalErrorKind::AlreadyExists,
                                    text: e.text(),
                                },
                            };
                            if let Some(action) = t.panes[qidx].queue.on_local_finished(finish, now)
                            {
                                t.drive(qidx, action, cx);
                            }
                            cx.notify();
                            false
                        }
                    }
                });
                if !alive.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_batch_finished(
        &mut self,
        qidx: usize,
        end: &BatchEnd,
        skipped: u32,
        cx: &mut Context<Self>,
    ) {
        // 刷新接收侧：已经到达的内容要显示出来。
        match self.mode {
            PanelMode::Dual => {
                for i in 0..self.panes.len() {
                    self.refresh(i, cx);
                }
            }
            PanelMode::Aux => self.refresh(qidx, cx),
        }
        if let BatchEnd::Failed(text) = end {
            cx.emit(FilerEvent::Notice(text.clone()));
        } else if skipped > 0 {
            cx.emit(FilerEvent::Notice(tf(
                "ui-filer-skipped",
                &[("count", &skipped.to_string())],
            )));
        }
        // 收尾进度条的收起：排一次延时重绘。
        self.schedule_linger(cx);
        cx.notify();
    }

    /// 收尾进度条约 1.8 / 3 秒后收起：到点重绘一次。
    pub(crate) fn schedule_linger(&mut self, cx: &mut Context<Self>) {
        if self.linger_scheduled {
            return;
        }
        self.linger_scheduled = true;
        cx.spawn(async move |this, cx| {
            loop {
                let left = this
                    .update(cx, |t, _| {
                        let now = t.now_ms();
                        t.panes
                            .iter()
                            .filter_map(|p| p.queue.linger_left(now))
                            .max()
                    })
                    .ok()
                    .flatten();
                match left {
                    Some(ms) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(ms + 20))
                            .await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    }
                    None => break,
                }
            }
            let _ = this.update(cx, |t, _| t.linger_scheduled = false);
        })
        .detach();
    }

    /// 排入一批并开始。已有前台传输时拒绝并提示（filer.md §6.2）。
    fn enqueue(&mut self, qidx: usize, items: Vec<TransferItem>, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        if let Err(busy) = self.panes[qidx].queue.enqueue(items) {
            cx.emit(FilerEvent::Notice(busy.text()));
            return;
        }
        let action = self.panes[qidx].queue.next_action();
        self.drive(qidx, action, cx);
        cx.notify();
    }

    /// 关掉错误条。
    pub(crate) fn dismiss_error(&mut self, qidx: usize) {
        if let Some(p) = self.panes.get_mut(qidx) {
            p.queue.clear_error();
        }
    }

    /// 点取消。
    pub(crate) fn cancel_transfer(&mut self, qidx: usize, cx: &mut Context<Self>) {
        let p = &mut self.panes[qidx];
        let id = p.queue.request_cancel();
        p.copy_cancel.store(true, Ordering::Relaxed);
        if let Some(id) = id {
            self.core.cancel_transfer(id);
        }
        cx.notify();
    }

    // ---- 上传 / 下载入口 ----

    /// 目标目录当前能不能接收写入；不能时返回原因。
    fn write_block_reason(&self, idx: usize) -> Option<String> {
        let p = &self.panes[idx];
        if fspath::is_virtual_root(p.style, &p.browse.path) && p.source == Source::Local {
            return Some(t("ui-filer-no-write-this-pc"));
        }
        if matches!(p.browse.phase, Phase::Disconnected) {
            return Some(t("ui-filer-disconnected-write"));
        }
        if !p.browse.can_write() {
            return Some(t("ui-filer-no-dir-open"));
        }
        None
    }

    fn notify_block(&mut self, reason: String, cx: &mut Context<Self>) {
        cx.emit(FilerEvent::Notice(reason));
    }

    /// 上传：弹系统选择器，选完排队。`dirs` 为真选目录，否则选文件。
    pub(crate) fn pick_and_upload(&mut self, idx: usize, dirs: bool, cx: &mut Context<Self>) {
        if self.panes[idx].source == Source::Local {
            return;
        }
        if let Some(r) = self.write_block_reason(idx) {
            self.notify_block(r, cx);
            return;
        }
        // 锁定会话与目标目录：之后切标签或导航都不改写。
        let dest = self.panes[idx].browse.path.clone();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: !dirs,
            directories: dirs,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let _ = this.update(cx, |t, cx| t.upload_paths(idx, paths, dest, cx));
        })
        .detach();
    }

    /// 把本地路径排入上传（或双栏 / 本地面板的复制）。
    pub(crate) fn upload_paths(
        &mut self,
        idx: usize,
        paths: Vec<PathBuf>,
        dest: String,
        cx: &mut Context<Self>,
    ) {
        let kind = match self.panes[idx].source {
            Source::Remote(_) => Kind::Upload,
            Source::Local => Kind::Copy,
        };
        let items = paths
            .into_iter()
            .map(|p| TransferItem {
                kind,
                name: p.file_name().map_or_else(
                    || p.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                is_dir: p.is_dir(),
                source: p.to_string_lossy().into_owned(),
                dest_dir: dest.clone(),
            })
            .collect();
        let q = self.queue_idx(idx);
        self.enqueue(q, items, cx);
    }

    /// 外部拖入（filer.md §7.1）：整个面板是落点，写入当前显示的目录。
    pub(crate) fn drop_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.mode != PanelMode::Aux {
            return;
        }
        let idx = self.shown;
        let Some(p) = self.panes.get(idx) else {
            cx.emit(FilerEvent::Notice(t("ui-filer-no-dir-open")));
            return;
        };
        if let Some(r) = self.write_block_reason(idx) {
            self.notify_block(r, cx);
            return;
        }
        if self.panes[idx].queue.is_busy() {
            cx.emit(FilerEvent::Notice(
                kanso_gpui::app::transfer_queue::Busy.text(),
            ));
            return;
        }
        let dest = p.browse.path.clone();
        self.upload_paths(idx, paths, dest, cx);
    }

    /// 为什么现在不能接收拖入；能接收返回 `None`（绘制“放下以上传 / 复制到这里”的提示用）。
    pub(crate) fn drop_block_reason(&self) -> Option<String> {
        if self.mode != PanelMode::Aux || self.panes.get(self.shown).is_none() {
            return Some(t("ui-filer-no-dir-open"));
        }
        if self.panes[self.shown].queue.is_busy() {
            return Some(kanso_gpui::app::transfer_queue::Busy.text());
        }
        self.write_block_reason(self.shown)
    }

    /// 下载选中的远端项到用户选的目录。
    pub(crate) fn pick_and_download(&mut self, idx: usize, cx: &mut Context<Self>) {
        let (rows, ok) = {
            let p = &self.panes[idx];
            (self.selected_rows(idx), p.source != Source::Local)
        };
        if !ok || rows.is_empty() {
            return;
        }
        let items_src: Vec<(String, String, bool)> = rows
            .iter()
            .map(|r| (r.path.clone(), r.name.clone(), r.is_dir))
            .collect();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(dest) = paths.into_iter().next() else {
                return;
            };
            let dest = dest.to_string_lossy().into_owned();
            let _ = this.update(cx, |t, cx| {
                let items = items_src
                    .into_iter()
                    .map(|(source, name, is_dir)| TransferItem {
                        kind: Kind::Download,
                        source,
                        name,
                        dest_dir: dest.clone(),
                        is_dir,
                    })
                    .collect();
                let q = t.queue_idx(idx);
                t.enqueue(q, items, cx);
            });
        })
        .detach();
    }

    /// 双栏：把本地栏选中项上传到远端栏当前目录。
    pub(crate) fn dual_upload(&mut self, cx: &mut Context<Self>) {
        if self.mode != PanelMode::Dual {
            return;
        }
        if let Some(r) = self.write_block_reason(0) {
            self.notify_block(r, cx);
            return;
        }
        let dest = self.panes[0].browse.path.clone();
        let items: Vec<TransferItem> = self
            .selected_rows(1)
            .into_iter()
            .map(|r| TransferItem {
                kind: Kind::Upload,
                source: r.path,
                name: r.name,
                dest_dir: dest.clone(),
                is_dir: r.is_dir,
            })
            .collect();
        self.enqueue(0, items, cx);
    }

    /// 双栏：把远端栏选中项下载到本地栏当前目录（不再弹目标选择器）。
    pub(crate) fn dual_download(&mut self, cx: &mut Context<Self>) {
        if self.mode != PanelMode::Dual {
            return;
        }
        if let Some(r) = self.write_block_reason(1) {
            self.notify_block(r, cx);
            return;
        }
        let dest = self.panes[1].browse.path.clone();
        let items: Vec<TransferItem> = self
            .selected_rows(0)
            .into_iter()
            .map(|r| TransferItem {
                kind: Kind::Download,
                source: r.path,
                name: r.name,
                dest_dir: dest.clone(),
                is_dir: r.is_dir,
            })
            .collect();
        self.enqueue(0, items, cx);
    }

    // ---- 选中 ----

    /// 选中的行（按可见顺序）。
    pub(crate) fn selected_rows(&self, idx: usize) -> Vec<kanso_gpui::filer::Row> {
        let p = &self.panes[idx];
        (0..p.model.visible_len())
            .filter_map(|pos| p.model.visible_at(pos))
            .filter(|(id, _)| p.sel.contains(*id))
            .map(|(_, r)| r.clone())
            .collect()
    }

    // ---- 打开 ----

    /// 激活一行（双击 / Enter）：目录进入；本地文件用系统默认应用打开；远端文件暂不支持编辑。
    pub(crate) fn open_row(&mut self, idx: usize, id: u32, cx: &mut Context<Self>) {
        let Some(row) = self.panes[idx].model.row(id).cloned() else {
            return;
        };
        if row.is_symlink && self.panes[idx].source == Source::Local {
            // 用户激活链接时确认目标类型；悬空或不可访问时说明原因。
            match std::fs::metadata(&row.path) {
                Ok(m) if m.is_dir() => {
                    self.navigate(idx, row.path, cx);
                }
                Ok(_) => cx.open_with_system(Path::new(&row.path)),
                Err(e) => cx.emit(FilerEvent::Notice(tf(
                    "ui-filer-link-broken",
                    &[("name", &row.name), ("reason", &e.to_string())],
                ))),
            }
            return;
        }
        if row.is_dir {
            self.navigate(idx, row.path, cx);
        } else if self.panes[idx].source == Source::Local && self.mode == PanelMode::Aux {
            cx.open_with_system(Path::new(&row.path));
        } else if self.panes[idx].source != Source::Local && self.mode == PanelMode::Aux {
            cx.emit(FilerEvent::Notice(t("ui-filer-remote-edit-unsupported")));
        }
    }

    /// 在系统里复制文字（失败不会被 GPUI 报告，只能当作成功）。
    fn copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    // ---- 菜单 ----

    /// 行右键菜单（filer.md §5.2 的顺序）。
    pub(crate) fn open_row_menu(
        &mut self,
        idx: usize,
        id: u32,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.shown = if self.mode == PanelMode::Dual {
            idx
        } else {
            self.shown
        };
        self.panes[idx].sel.right_click(id);
        let Some(row) = self.panes[idx].model.row(id).cloned() else {
            return;
        };
        let remote = self.panes[idx].source != Source::Local;
        let aux = self.mode == PanelMode::Aux;
        let writable = self.write_block_reason(idx).is_none();
        let mut items = vec![MenuItem::action(
            format!("filer:{idx}:open"),
            t("ui-filer-menu-open"),
        )];
        if aux && !row.is_dir && !row.is_symlink && !remote {
            items.push(MenuItem::action(
                format!("filer:{idx}:open-with"),
                t("ui-filer-menu-open-with"),
            ));
        }
        if remote {
            items.push(MenuItem::action(
                format!("filer:{idx}:download"),
                t("ui-filer-menu-download"),
            ));
        }
        items.push(MenuItem::separator());
        items.push(MenuItem::action(
            format!("filer:{idx}:copy-path"),
            t("ui-filer-menu-copy-path"),
        ));
        items.push(MenuItem::action(
            format!("filer:{idx}:copy-name"),
            t("ui-filer-menu-copy-name"),
        ));
        items.push(MenuItem::separator());
        let rename = MenuItem::action(format!("filer:{idx}:rename"), t("ui-filer-rename"));
        items.push(if writable { rename } else { rename.disabled() });
        let delete = MenuItem::action(format!("filer:{idx}:delete"), t("ui-filer-delete"));
        items.push(if writable { delete } else { delete.disabled() });
        cx.emit(FilerEvent::Menu { items, position });
        cx.notify();
    }

    /// 列表空白处右键菜单。
    pub(crate) fn open_blank_menu(
        &mut self,
        idx: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.mode == PanelMode::Dual {
            self.shown = idx;
        }
        let remote = self.panes[idx].source != Source::Local;
        let writable = self.write_block_reason(idx).is_none();
        let gate = |it: MenuItem| if writable { it } else { it.disabled() };
        let mut items = vec![
            gate(MenuItem::action(
                format!("filer:{idx}:new-file"),
                t("ui-filer-new-file"),
            )),
            gate(MenuItem::action(
                format!("filer:{idx}:new-folder"),
                t("ui-filer-new-folder"),
            )),
        ];
        if remote && self.mode == PanelMode::Aux {
            items.push(MenuItem::separator());
            items.push(gate(MenuItem::action(
                format!("filer:{idx}:upload-files"),
                t("ui-filer-upload-files"),
            )));
            items.push(gate(MenuItem::action(
                format!("filer:{idx}:upload-dirs"),
                t("ui-filer-upload-dirs"),
            )));
        }
        items.push(MenuItem::separator());
        items.push(MenuItem::action(
            format!("filer:{idx}:copy-dir"),
            t("ui-filer-menu-copy-dir"),
        ));
        items.push(MenuItem::action(
            format!("filer:{idx}:refresh"),
            t("ui-filer-refresh"),
        ));
        cx.emit(FilerEvent::Menu { items, position });
    }

    /// 上传按钮的菜单（上传文件 / 上传目录）。
    pub(crate) fn open_upload_menu(
        &mut self,
        idx: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let items = vec![
            MenuItem::action(
                format!("filer:{idx}:upload-files"),
                t("ui-filer-upload-files"),
            ),
            MenuItem::action(
                format!("filer:{idx}:upload-dirs"),
                t("ui-filer-upload-dirs"),
            ),
        ];
        cx.emit(FilerEvent::Menu { items, position });
    }

    /// 外壳转发的菜单选择。
    pub(crate) fn on_menu_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rest) = id.strip_prefix("filer:") else {
            return;
        };
        let Some((idx, action)) = rest.split_once(':') else {
            return;
        };
        let Ok(idx) = idx.parse::<usize>() else {
            return;
        };
        if idx >= self.panes.len() {
            return;
        }
        let _ = window;
        match action {
            "open" => {
                if let Some(id) = self.panes[idx].sel.cursor() {
                    self.open_row(idx, id, cx);
                }
            }
            "open-with" => {
                if let Some(id) = self.panes[idx].sel.cursor()
                    && let Some(row) = self.panes[idx].model.row(id)
                {
                    open_with_dialog(&row.path);
                }
            }
            "download" => self.pick_and_download(idx, cx),
            "copy-path" => {
                let text = self
                    .selected_rows(idx)
                    .iter()
                    .map(|r| r.path.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.copy_text(text, cx);
            }
            "copy-name" => {
                let text = self
                    .selected_rows(idx)
                    .iter()
                    .map(|r| r.name.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.copy_text(text, cx);
            }
            "copy-dir" => {
                let text = self.panes[idx].browse.path.clone();
                self.copy_text(text, cx);
            }
            "rename" => self.ask_rename(idx, cx),
            "delete" => self.ask_delete(idx, cx),
            "new-file" => self.ask_name(idx, NameKind::NewFile, cx),
            "new-folder" => self.ask_name(idx, NameKind::NewFolder, cx),
            "upload-files" => self.pick_and_upload(idx, false, cx),
            "upload-dirs" => self.pick_and_upload(idx, true, cx),
            "refresh" => self.refresh(idx, cx),
            _ => {}
        }
    }

    // ---- 新建 / 重命名 / 删除 ----

    fn location_label(&self, idx: usize) -> String {
        self.panes[idx].identity.clone()
    }

    pub(crate) fn ask_name(&mut self, idx: usize, kind: NameKind, cx: &mut Context<Self>) {
        if let Some(r) = self.write_block_reason(idx) {
            self.notify_block(r, cx);
            return;
        }
        self.pending = Some(Pending::Name {
            pane: idx,
            kind,
            old: None,
        });
        cx.emit(FilerEvent::Dialog(name_spec(
            kind,
            "",
            &self.location_label(idx),
            None,
        )));
    }

    pub(crate) fn ask_rename(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(r) = self.write_block_reason(idx) {
            self.notify_block(r, cx);
            return;
        }
        let rows = self.selected_rows(idx);
        let [row] = rows.as_slice() else {
            return;
        };
        self.pending = Some(Pending::Name {
            pane: idx,
            kind: NameKind::Rename,
            old: Some(row.name.clone()),
        });
        cx.emit(FilerEvent::Dialog(name_spec(
            NameKind::Rename,
            &row.name,
            &self.location_label(idx),
            None,
        )));
    }

    pub(crate) fn ask_delete(&mut self, idx: usize, cx: &mut Context<Self>) {
        if let Some(r) = self.write_block_reason(idx) {
            self.notify_block(r, cx);
            return;
        }
        let targets: Vec<DeleteTarget> = self
            .selected_rows(idx)
            .into_iter()
            .map(|r| DeleteTarget {
                name: r.name,
                path: r.path,
                is_dir: r.is_dir,
                is_symlink: r.is_symlink,
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        cx.emit(FilerEvent::Dialog(delete_spec(
            &targets,
            &self.location_label(idx),
        )));
        self.pending = Some(Pending::Delete { pane: idx, targets });
    }

    /// 外壳转发的对话框结果。
    pub(crate) fn on_dialog_result(
        &mut self,
        result: &DialogResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = window;
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            Pending::Delete { pane, targets } => {
                if delete_confirmed(result) {
                    self.run_delete(pane, targets, cx);
                }
            }
            Pending::Name { pane, kind, old } => {
                let style = self.panes[pane].style;
                match parse_name(style, result, old.as_deref()) {
                    NameOutcome::Nothing => {}
                    NameOutcome::Invalid(e) => {
                        // 提示原因并让用户重输。
                        self.pending = Some(Pending::Name {
                            pane,
                            kind,
                            old: old.clone(),
                        });
                        cx.emit(FilerEvent::Dialog(name_spec(
                            kind,
                            result.value(filer_ops::NAME_FIELD).unwrap_or_default(),
                            &self.location_label(pane),
                            Some(&name_error_text(e)),
                        )));
                    }
                    NameOutcome::Name(name) => self.run_name(pane, kind, old, name, cx),
                }
            }
            Pending::Conflict { pane } => {
                let now = self.now_ms();
                let decision = conflict_decision(result);
                let action = self.panes[pane].queue.resolve(decision, now);
                self.drive(pane, action, cx);
            }
        }
        cx.notify();
    }

    /// 在栏上报告一次操作失败：名称与原因，附身份。
    fn report_error(
        &mut self,
        idx: usize,
        what: &str,
        name: &str,
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let text = tf(
            "ui-filer-op-error-at",
            &[
                ("what", what),
                ("name", name),
                ("where", &self.location_label(idx)),
                ("reason", reason),
            ],
        );
        cx.emit(FilerEvent::Notice(text));
    }

    pub(crate) fn run_name(
        &mut self,
        idx: usize,
        kind: NameKind,
        old: Option<String>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let (source, style, dir) = {
            let p = &self.panes[idx];
            (p.source, p.style, p.browse.path.clone())
        };
        let target = fspath::join(style, &dir, &name);
        let what = t(match kind {
            NameKind::NewFile => "ui-filer-new-file",
            NameKind::NewFolder => "ui-filer-new-folder",
            NameKind::Rename => "ui-filer-rename",
        });
        match source {
            Source::Local => {
                let r = match kind {
                    NameKind::NewFile => local_fs::create_file(Path::new(&target)),
                    NameKind::NewFolder => local_fs::mkdir(Path::new(&target)),
                    NameKind::Rename => {
                        let from = fspath::join(style, &dir, old.as_deref().unwrap_or_default());
                        local_fs::rename(Path::new(&from), Path::new(&target))
                    }
                };
                match r {
                    Ok(()) => self.refresh(idx, cx),
                    Err(e) => self.report_error(idx, &what, &name, &e.text(), cx),
                }
            }
            Source::Remote(session) => {
                let core = self.core.clone();
                let from = old.map(|o| fspath::join(style, &dir, &o));
                cx.spawn(async move |this, cx| {
                    let r = match kind {
                        NameKind::NewFile => core.sftp_create_file(session, &target).await,
                        NameKind::NewFolder => core.sftp_mkdir(session, &target).await,
                        NameKind::Rename => {
                            core.sftp_rename(session, from.as_deref().unwrap_or_default(), &target)
                                .await
                        }
                    };
                    let _ = this.update(cx, |t, cx| match r {
                        Ok(()) => t.refresh(idx, cx),
                        Err(e) => t.report_error(idx, &what, &name, &core_error_text(&e), cx),
                    });
                })
                .detach();
            }
        }
    }

    pub(crate) fn run_delete(
        &mut self,
        idx: usize,
        targets: Vec<DeleteTarget>,
        cx: &mut Context<Self>,
    ) {
        match self.panes[idx].source {
            Source::Local => {
                let mut failed = None;
                for tg in &targets {
                    if let Err(e) = local_fs::remove(Path::new(&tg.path)) {
                        // 对象在确认期间已经消失：说明已不存在，而不是误删别的。
                        failed = Some((tg.name.clone(), e.text()));
                        break;
                    }
                }
                if let Some((name, reason)) = failed {
                    let what = kanso_gpui::app::filer_text::t("ui-filer-delete");
                    self.report_error(idx, &what, &name, &reason, cx);
                }
                self.refresh(idx, cx);
            }
            Source::Remote(session) => {
                let core = self.core.clone();
                cx.spawn(async move |this, cx| {
                    let mut failed: Option<(String, String)> = None;
                    for tg in &targets {
                        // 链接只移除链接本身；目录只能删空目录（核心的目录删除是递归的，这里先确认为空）。
                        let r = if tg.is_symlink {
                            core.sftp_remove(session, &tg.path, false)
                                .await
                                .map_err(|e| core_error_text(&e))
                        } else if tg.is_dir {
                            match remote_dir_is_empty(&core, session, &tg.path).await {
                                Ok(true) => core
                                    .sftp_remove(session, &tg.path, true)
                                    .await
                                    .map_err(|e| core_error_text(&e)),
                                Ok(false) => Err(t("ui-filer-err-not-empty")),
                                Err(e) => Err(e),
                            }
                        } else {
                            core.sftp_remove(session, &tg.path, false)
                                .await
                                .map_err(|e| core_error_text(&e))
                        };
                        if let Err(reason) = r {
                            failed = Some((tg.name.clone(), reason));
                            break;
                        }
                    }
                    let _ = this.update(cx, |t, cx| {
                        if let Some((name, reason)) = failed {
                            let what = kanso_gpui::app::filer_text::t("ui-filer-delete");
                            t.report_error(idx, &what, &name, &reason, cx);
                        }
                        t.refresh(idx, cx);
                    });
                })
                .detach();
            }
        }
    }

    // ---- 键盘 ----

    /// 列表快捷键。返回是否已处理。
    pub(crate) fn on_list_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        shift: bool,
        page: isize,
        cx: &mut Context<Self>,
    ) -> bool {
        let idx = self.shown;
        if idx >= self.panes.len() {
            return false;
        }
        let moved = {
            let p = &mut self.panes[idx];
            match (key, ctrl, alt) {
                ("down", false, false) => p.sel.step(1, &p.model, shift),
                ("up", false, false) => p.sel.step(-1, &p.model, shift),
                ("pagedown", false, false) => p.sel.step(page, &p.model, shift),
                ("pageup", false, false) => p.sel.step(-page, &p.model, shift),
                ("home", false, false) => p.sel.move_to(0, &p.model, shift),
                ("end", false, false) => {
                    let last = p.model.visible_len().saturating_sub(1);
                    p.sel.move_to(last, &p.model, shift)
                }
                ("a", true, false) => {
                    p.sel.select_all(&p.model);
                    cx.notify();
                    return true;
                }
                _ => None,
            }
        };
        if let Some(pos) = moved {
            self.panes[idx]
                .scroll
                .scroll_to_item(pos, ScrollStrategy::Top);
            cx.notify();
            return true;
        }
        match (key, ctrl, alt) {
            ("enter", false, false) => {
                if let Some(id) = self.panes[idx].sel.cursor() {
                    self.open_row(idx, id, cx);
                }
            }
            ("backspace", false, false) | ("up", false, true) => self.go_up(idx, cx),
            ("f5", ..) => self.refresh(idx, cx),
            ("f2", ..) => self.ask_rename(idx, cx),
            ("delete", false, false) => self.ask_delete(idx, cx),
            _ => return false,
        }
        true
    }
}

/// 远端目录是否为空（只读第一批）。
async fn remote_dir_is_empty(core: &Core, session: SessionId, path: &str) -> Result<bool, String> {
    let mut stream = core
        .sftp_list(session, path)
        .map_err(|e| core_error_text(&e))?;
    match stream.next_batch().await {
        Ok(Some(batch)) => Ok(batch.is_empty()),
        Ok(None) => Ok(true),
        Err(e) => Err(core_error_text(&e)),
    }
}

/// “打开方式”：交给 Windows 的系统选择器（rundll32 的 OpenAs_RunDLL）。
fn open_with_dialog(path: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("rundll32.exe")
            .arg("shell32.dll,OpenAs_RunDLL")
            .arg(path)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

// ---- 自检辅助（`--filer-selftest`）----

impl FilerPanel {
    /// 当前显示的栏里可见条目的名称（按显示顺序）。
    pub(crate) fn selftest_names(&self) -> Vec<String> {
        let Some(p) = self.panes.get(self.shown) else {
            return Vec::new();
        };
        (0..p.model.visible_len())
            .filter_map(|i| p.model.visible_at(i).map(|(_, r)| r.name.clone()))
            .collect()
    }

    /// 当前显示的栏是否读取完成、以及它的目录。
    pub(crate) fn selftest_state(&self) -> (bool, String) {
        self.panes
            .get(self.shown)
            .map_or((false, String::new()), |p| {
                (p.browse.phase == Phase::Ready, p.browse.path.clone())
            })
    }

    /// 选中当前显示栏里名称为 `name` 的条目（自检用）。
    pub(crate) fn selftest_select(&mut self, name: &str) -> bool {
        let Some(p) = self.panes.get_mut(self.shown) else {
            return false;
        };
        let found = (0..p.model.visible_len())
            .filter_map(|i| p.model.visible_at(i))
            .find(|(_, r)| r.name == name)
            .map(|(id, _)| id);
        match found {
            Some(id) => {
                p.sel.select_only(id);
                true
            }
            None => false,
        }
    }

    /// 以当前选中项执行删除（跳过确认框，自检用）。
    pub(crate) fn selftest_delete_selected(&mut self, cx: &mut Context<Self>) {
        let idx = self.shown;
        let targets: Vec<DeleteTarget> = self
            .selected_rows(idx)
            .into_iter()
            .map(|r| DeleteTarget {
                name: r.name,
                path: r.path,
                is_dir: r.is_dir,
                is_symlink: r.is_symlink,
            })
            .collect();
        self.run_delete(idx, targets, cx);
    }

    /// 在当前显示的栏新建目录或文件（跳过对话框，自检用）。
    pub(crate) fn selftest_create(&mut self, kind: NameKind, name: &str, cx: &mut Context<Self>) {
        let idx = self.shown;
        self.run_name(idx, kind, None, name.to_owned(), cx);
    }

    /// 切换隐藏项过滤（自检用）。
    pub(crate) fn selftest_toggle_hidden(&mut self, cx: &mut Context<Self>) {
        let idx = self.shown;
        self.toggle_hidden(idx, cx);
    }
}
