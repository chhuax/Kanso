//! `ShellView` 与文件功能的接线：
//!
//! - 辅助 Filer（标签栏与分屏区之间的可收起面板）：显隐、宽度拖动与持久化、跟随活动标签、定位终端目录（⌘J）；
//! - 纯 SFTP 标签：主工作区直接显示双栏文件浏览器（每个标签一个 [`FilerPanel`]）；
//! - 文件面板要的菜单与对话框（由外壳承载，结果交回面板）；
//! - 终端内传输（ZMODEM / XMODEM）：核心提问对应的系统文件选择器、进度条、XMODEM 菜单命令。
//!
//! 浮层打开期间不处理应用级快捷键，与启动器 / 提示框一致。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, CursorStyle, Entity, MouseButton, MouseDownEvent, Subscription, Window,
    div, prelude::*, px, relative,
};
use kanso_core::{Core, XmodemBlock};
use kanso_gpui::app::filer_layout::{FilerLayout, SETTING_KEY, identity_text};
use kanso_gpui::app::filer_state::{Locator, cwd_host_is_local};
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::launcher::{core_error_text, render_message};
use kanso_gpui::app::tab::{TabConnection, TabKind};
use kanso_gpui::app::term_transfer::PickerPlan;
use kanso_gpui::app::term_transfer::{TermTransfers, picker_hint, picker_plan, response_for};
use kanso_gpui::app::transfer_queue::{Bar, BarFinish, Finish};
use kanso_gpui::widgets::dialog::DialogSpec;
use kanso_gpui::widgets::menu::MenuItem;
use kanso_gpui::widgets::style::number;
use kanso_types::event::TransferOutcome;
use kanso_types::message::Message;
use kanso_types::profile::ProfileKind;
use kanso_types::prompt::PromptRequest;
use kanso_types::transfer::XferProtocol;
use kanso_types::{CoreEvent, PromptId, SessionId};

use super::filer_panel::{FilerEvent, FilerPanel, Source};
use super::log;
use super::shell::{ShellView, Theme, status_bar_h};
use super::widgets::dialog::{Dialog, DialogEvent};
use super::widgets::icons::icon;
use super::widgets::menu::{ContextMenu, MenuEvent};

/// 外壳持有的文件功能状态。
pub(super) struct FilerState {
    /// 辅助 Filer 面板。
    panel: Entity<FilerPanel>,
    _panel_sub: Subscription,
    /// 显隐与宽度（重启后恢复）。
    layout: FilerLayout,
    /// 拖动宽度：(按下时的鼠标 x, 按下时的宽度)。
    drag: Option<(f32, f32)>,
    /// 纯 SFTP 标签的双栏面板。
    dual: HashMap<SessionId, (Entity<FilerPanel>, Subscription)>,
    locator: Locator,
    /// 辅助面板最近一次显示的标签。
    shown_session: Option<SessionId>,
    /// 当前菜单属于哪个面板。
    menu_owner: Option<Entity<FilerPanel>>,
    /// 面板请求的模态对话框。
    pub(super) dialog: Option<Entity<Dialog>>,
    dialog_sub: Option<Subscription>,
    dialog_owner: Option<Entity<FilerPanel>>,
    /// 终端内传输（ZMODEM / XMODEM）的进度。
    term: TermTransfers,
    /// 正在显示系统文件选择器的提问。
    picker: Option<PromptId>,
    t0: Instant,
}

impl FilerState {
    pub(super) fn new(core: &Core, panel: Entity<FilerPanel>, sub: Subscription) -> FilerState {
        FilerState {
            panel,
            _panel_sub: sub,
            layout: FilerLayout::from_json(core.setting(SETTING_KEY).as_deref()),
            drag: None,
            dual: HashMap::new(),
            locator: Locator::default(),
            shown_session: None,
            menu_owner: None,
            dialog: None,
            dialog_sub: None,
            dialog_owner: None,
            term: TermTransfers::default(),
            picker: None,
            t0: Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    /// 面板是否显示（给菜单的勾选状态用）。
    pub(super) fn visible(&self) -> bool {
        self.layout.visible
    }
}

/// 在 `ShellView::new` 里建辅助面板及其事件订阅。
pub(super) fn new_state(
    core: &std::sync::Arc<Core>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> FilerState {
    let panel = cx.new(|cx| FilerPanel::new_aux(core.clone(), cx));
    let sub = cx.subscribe_in(
        &panel,
        window,
        |this, panel, ev: &FilerEvent, window, cx| this.on_filer_event(panel, ev, window, cx),
    );
    FilerState::new(core, panel, sub)
}

fn local_host_names() -> Vec<String> {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .collect()
}

impl ShellView {
    // ---- 活动标签 ----

    fn active_filer_kind(&self) -> Option<TabKind> {
        let ws = self.ctl.workspace();
        ws.active_session().and_then(|s| ws.item(s)).map(|i| i.kind)
    }

    /// 标签对应的文件来源与标题里的来源标识。
    fn filer_identity(&self, sid: SessionId) -> Option<(Source, String)> {
        let item = self.ctl.workspace().item(sid)?;
        Some(match item.kind {
            TabKind::Local => (Source::Local, identity_text(false, &item.name, None)),
            TabKind::Ssh | TabKind::Sftp => {
                let host = item.profile.and_then(|pid| {
                    self.core
                        .profiles()
                        .into_iter()
                        .find(|p| p.id == pid)
                        .map(|p| match p.kind {
                            ProfileKind::Ssh(s) => s.target.host,
                            ProfileKind::Sftp(t) => t.host,
                            ProfileKind::Local(_) => String::new(),
                        })
                });
                (
                    Source::Remote(sid),
                    identity_text(true, &item.name, host.as_deref()),
                )
            }
        })
    }

    /// 辅助面板占用的宽度（逻辑像素）：纯 SFTP 标签里不显示，其余看显隐与窗口宽度。
    pub(super) fn filer_width(&self, window: &Window) -> f32 {
        if self.active_filer_kind() == Some(TabKind::Sftp) {
            return 0.0;
        }
        let total = f32::from(window.viewport_size().width);
        let sidebar = self.sidebar_width(window);
        self.filer.layout.effective_width(total - sidebar)
    }

    /// 让辅助面板跟随活动标签。只在面板显示时才建栏（隐藏时不去读目录）。
    pub(super) fn sync_filer_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sid) = self.ctl.workspace().active_session() else {
            return;
        };
        if self.active_filer_kind() == Some(TabKind::Sftp) || !self.filer.layout.visible {
            return;
        }
        let panel = self.filer.panel.clone();
        if self.filer.shown_session == Some(sid) && panel.read(cx).has_session(sid) {
            return;
        }
        let Some((source, identity)) = self.filer_identity(sid) else {
            return;
        };
        panel.update(cx, |p, cx| {
            p.show_session(Some((sid, source, identity)), window, cx)
        });
        self.filer.shown_session = Some(sid);
    }

    /// 标签关闭：丢掉它的文件状态。
    pub(super) fn filer_forget(&mut self, sid: SessionId, cx: &mut Context<Self>) {
        self.filer.panel.update(cx, |p, _| p.forget_session(sid));
        self.filer.dual.remove(&sid);
        self.filer.locator.forget(sid);
        self.filer.term.forget_session(sid);
        if self.filer.shown_session == Some(sid) {
            self.filer.shown_session = None;
        }
    }

    // ---- 纯 SFTP 标签 ----

    /// 为已打开的会话建立界面：SFTP 会话是双栏文件浏览器，其余是终端。
    pub(super) fn attach_session(
        &mut self,
        sid: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_sftp = self
            .ctl
            .workspace()
            .item(sid)
            .is_some_and(|i| i.kind == TabKind::Sftp);
        if is_sftp {
            self.attach_sftp(sid, window, cx);
        } else {
            self.attach_terminal(sid, window, cx);
        }
    }

    fn attach_sftp(&mut self, sid: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let identity = self
            .filer_identity(sid)
            .map_or_else(|| msg(Message::UiFilerLocal {}), |(_, i)| i);
        let core = self.core.clone();
        let panel = cx.new(|cx| FilerPanel::new_dual(core, sid, identity, window, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this, panel, ev: &FilerEvent, window, cx| this.on_filer_event(panel, ev, window, cx),
        );
        self.filer.dual.insert(sid, (panel, sub));
    }

    /// 纯 SFTP 标签的双栏面板（渲染分屏时用）。
    pub(super) fn sftp_panel(&self, sid: SessionId) -> Option<Entity<FilerPanel>> {
        self.filer.dual.get(&sid).map(|(p, _)| p.clone())
    }

    // ---- 面板事件、菜单与对话框 ----

    fn on_filer_event(
        &mut self,
        panel: &Entity<FilerPanel>,
        ev: &FilerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            FilerEvent::Menu { items, position } => {
                self.open_filer_menu(panel.clone(), items.clone(), *position, window, cx)
            }
            FilerEvent::Dialog(spec) => {
                self.open_filer_dialog(panel.clone(), spec.clone(), window, cx)
            }
            FilerEvent::Notice(text) => self.show_notice(text.clone(), cx),
            FilerEvent::Locate => self.locate_terminal_dir(false, window, cx),
        }
    }

    fn open_filer_menu(
        &mut self,
        owner: Entity<FilerPanel>,
        items: Vec<MenuItem>,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() || self.filer.dialog.is_some() {
            return;
        }
        self.menu_sub = None;
        let menu = cx.new(|cx| ContextMenu::new(items, position, window, cx));
        self.filer.menu_owner = Some(owner);
        self.menu_sub =
            Some(
                cx.subscribe_in(&menu, window, |this, _m, ev: &MenuEvent, window, cx| {
                    this.menu = None;
                    this.menu_sub = None;
                    let owner = this.filer.menu_owner.take();
                    if let Some(owner) = owner {
                        window.focus(&owner.read(cx).focus);
                        if let MenuEvent::Activate(id) = ev {
                            let id = id.clone();
                            owner.update(cx, |p, cx| p.on_menu_action(&id, window, cx));
                        }
                    } else {
                        this.sync_focus(window, cx);
                    }
                    cx.notify();
                }),
            );
        self.menu = Some(menu);
        cx.notify();
    }

    fn open_filer_dialog(
        &mut self,
        owner: Entity<FilerPanel>,
        spec: DialogSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.menu_sub = None;
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self.filer.dialog_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            move |this, _d, DialogEvent::Result(r): &DialogEvent, window, cx| {
                // 先清掉自己：面板处理结果时可能再请求一个对话框（名称不合规要重输）。
                this.filer.dialog = None;
                this.filer.dialog_sub = None;
                if let Some(owner) = this.filer.dialog_owner.take() {
                    window.focus(&owner.read(cx).focus);
                    let r = r.clone();
                    owner.update(cx, |p, cx| p.on_dialog_result(&r, window, cx));
                }
                cx.notify();
            },
        ));
        self.filer.dialog_owner = Some(owner);
        self.filer.dialog = Some(dialog);
        cx.notify();
    }

    // ---- 显隐、宽度 ----

    fn save_filer_layout(&self) {
        if let Err(e) = self
            .core
            .set_setting(SETTING_KEY, self.filer.layout.to_json())
        {
            log(&format!("保存 Filer 布局失败：{e}"));
        }
    }

    /// 视图菜单 / Ctrl+Alt+→：显示或隐藏，使用已记住的浏览位置。
    pub(super) fn toggle_filer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_filer_kind() == Some(TabKind::Sftp) {
            self.show_notice(msg(Message::UiFilerStatusAlreadyOpen {}), cx);
            return;
        }
        self.filer.layout.visible = !self.filer.layout.visible;
        self.save_filer_layout();
        self.sync_filer_session(window, cx);
        cx.notify();
    }

    /// 状态栏“文件浏览器”：已展开就立即收起；隐藏时先尝试定位当前 shell 目录，成功后展开。
    fn filer_status_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filer.layout.visible {
            self.filer.layout.visible = false;
            self.save_filer_layout();
            cx.notify();
        } else {
            self.locate_terminal_dir(true, window, cx);
        }
    }

    /// 状态栏左侧的 Filer 开关。
    pub(super) fn filer_toggle_item(
        &self,
        th: Theme,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sftp = self.active_filer_kind() == Some(TabKind::Sftp);
        let on = self.filer.layout.visible && !sftp;
        let el = div()
            .id("status-filer")
            .flex_none()
            .px(px(6.0))
            .py(px(2.0))
            .rounded(px(number("radius", "md")))
            .text_color(if on {
                th.c("accent")
            } else {
                th.c("text.secondary")
            })
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(super::widgets::icons::icon(
                "folder",
                12.,
                if on {
                    th.c("accent")
                } else {
                    th.c("text.secondary")
                },
            ))
            .when(!compact, |el| el.child(msg(Message::UiFileBrowser {})));
        if sftp {
            // 双栏文件浏览器已经在当前标签打开：开关不可操作并说明原因。
            return el
                .opacity(0.5)
                .tooltip(move |_w, cx| {
                    cx.new(|_| SimpleTip {
                        text: msg(Message::UiFilerStatusAlreadyOpen {}).into(),
                        th,
                    })
                    .into()
                })
                .into_any_element();
        }
        el.cursor_pointer()
            .hover(move |s| s.bg(th.c("bg.rowHover")))
            .on_click(cx.listener(|this, _, window, cx| this.filer_status_clicked(window, cx)))
            .into_any_element()
    }

    // ---- 定位终端目录 ----

    /// ⌘J / 终端右键菜单 / 面板上的定位按钮 / 状态栏展开：同一个用户动作（filer.md §2）。
    /// `expand` 为真时成功后展开面板。
    pub(super) fn locate_terminal_dir(
        &mut self,
        expand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sid) = self.ctl.workspace().active_session() else {
            self.show_notice(msg(Message::UiFilerLocateNoSession {}), cx);
            return;
        };
        let Some(item) = self.ctl.workspace().item(sid).cloned() else {
            return;
        };
        if item.kind == TabKind::Sftp {
            self.show_notice(msg(Message::UiFilerLocateSftp {}), cx);
            return;
        }
        if !matches!(item.connection, TabConnection::Connected) {
            self.show_notice(msg(Message::UiFilerLocateNotConnected {}), cx);
            return;
        }
        if item.kind == TabKind::Local {
            let names = local_host_names();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            if !cwd_host_is_local(item.cwd_host.as_deref(), &refs) {
                let host = item.cwd_host.clone().unwrap_or_default();
                self.show_notice(
                    msg(Message::UiFilerLocateForeignHost {
                        host: host.to_string(),
                    }),
                    cx,
                );
                return;
            }
        }
        // 锁定发起时的会话：结果返回时按票据判断是否仍有效。
        let ticket = self.filer.locator.begin(sid);
        let core = self.core.clone();
        let name = self.ctl.tab_title(sid).unwrap_or_else(|| item.name.clone());
        cx.spawn_in(window, async move |this, cx| {
            let r = core.working_directory(sid).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.on_located(sid, ticket, name, r, expand, window, cx)
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn on_located(
        &mut self,
        sid: SessionId,
        ticket: u64,
        name: String,
        result: Result<String, kanso_core::CoreError>,
        expand: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 请求期间标签被关闭或又发起了新的定位：迟到的结果作废。
        if !self.filer.locator.is_current(sid, ticket) || self.ctl.workspace().item(sid).is_none() {
            return;
        }
        match result {
            Ok(path) => {
                let still_active = self.ctl.workspace().active_session() == Some(sid);
                if expand && still_active && !self.filer.layout.visible {
                    self.filer.layout.visible = true;
                    self.save_filer_layout();
                }
                // 面板显示（或刚被要求展开）时确保这个标签有栏，再直接读取目标目录，不先显示家目录。
                if still_active {
                    self.sync_filer_session(window, cx);
                }
                let shown = path.clone();
                self.filer
                    .panel
                    .update(cx, |p, cx| p.navigate_session(sid, shown, window, cx));
                self.show_notice(
                    msg(Message::UiFilerLocated {
                        path: path.to_string(),
                    }),
                    cx,
                );
            }
            Err(e) => {
                self.show_notice(
                    msg(Message::UiFilerLocateFailed {
                        name: name.to_string(),
                        why: (core_error_text(&e)).to_string(),
                    }),
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// 自检：显示面板并让活动标签的栏直接读取 `target`。
    pub(super) fn filer_selftest_show(
        &mut self,
        target: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filer.layout.visible = true;
        self.sync_filer_session(window, cx);
        if let Some(sid) = self.ctl.workspace().active_session() {
            let target = target.to_owned();
            self.filer
                .panel
                .update(cx, |p, cx| p.navigate_session(sid, target, window, cx));
        }
        cx.notify();
    }

    /// 自检用：辅助面板实体。
    pub(super) fn filer_selftest_panel(&self) -> Entity<FilerPanel> {
        self.filer.panel.clone()
    }

    // ---- 渲染 ----

    /// 辅助面板与它右侧的宽度拖动条。
    pub(super) fn render_filer(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let w = self.filer_width(window);
        if w <= 0.0 {
            return Vec::new();
        }
        let x = self.sidebar_width(window);
        let top = super::toolbar::HEIGHT;
        let h = (f32::from(window.viewport_size().height) - status_bar_h() - top).max(0.);
        let panel = div()
            .id("filer-host")
            .absolute()
            .left(px(x))
            .top(px(top))
            .w(px(w))
            .h(px(h.max(0.0)))
            .border_r_1()
            .border_color(th.c("border.subtle"))
            .child(self.filer.panel.clone());
        let handle = div()
            .id("filer-resize")
            .absolute()
            .left(px(x + w - 3.0))
            .top(px(top))
            .w(px(6.0))
            .h(px(h.max(0.0)))
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(move |s| s.bg(th.ca("accent", 0x55)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    this.filer.drag = Some((f32::from(ev.position.x), this.filer.layout.width));
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        vec![panel.into_any_element(), handle.into_any_element()]
    }

    /// 拖动中：返回 `true` 表示这次鼠标移动被宽度拖动吃掉。
    pub(super) fn filer_drag_move(
        &mut self,
        ev: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((start_x, start_w)) = self.filer.drag else {
            return false;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.filer_drag_end();
            return false;
        }
        let w = start_w + (f32::from(ev.position.x) - start_x);
        self.filer.layout.width = self.filer.layout.dragged(w);
        cx.notify();
        true
    }

    /// 结束宽度拖动（并保存）。
    pub(super) fn filer_drag_end(&mut self) {
        if self.filer.drag.take().is_some() {
            self.save_filer_layout();
        }
    }

    // ---- 核心事件 ----

    /// 把核心事件转给文件面板与终端内传输模型。
    pub(super) fn filer_core_events(&mut self, batch: &[CoreEvent], cx: &mut Context<Self>) {
        let now = self.filer.now_ms();
        let mut term_finished = false;
        for ev in batch {
            self.filer.panel.update(cx, |p, cx| p.on_core_event(ev, cx));
            let duals: Vec<_> = self.filer.dual.values().map(|(p, _)| p.clone()).collect();
            for p in duals {
                p.update(cx, |p, cx| p.on_core_event(ev, cx));
            }
            match ev {
                CoreEvent::TransferStarted {
                    transfer,
                    session,
                    protocol,
                    direction,
                    name,
                    total_bytes,
                } if *protocol != XferProtocol::Sftp => {
                    self.filer.term.on_started(
                        *transfer,
                        *session,
                        *protocol,
                        *direction,
                        name,
                        *total_bytes,
                    );
                    cx.notify();
                }
                CoreEvent::TransferProgress {
                    transfer,
                    done_bytes,
                    total_bytes,
                } => {
                    self.filer
                        .term
                        .on_progress(*transfer, *done_bytes, *total_bytes, now);
                    cx.notify();
                }
                CoreEvent::TransferFinished { transfer, outcome } => {
                    let finish = match outcome {
                        TransferOutcome::Completed { skipped } => {
                            Finish::Completed { skipped: *skipped }
                        }
                        TransferOutcome::Cancelled => Finish::Cancelled,
                        TransferOutcome::Failed {
                            message,
                            already_exists,
                        } => Finish::Failed {
                            text: render_message(message),
                            already_exists: *already_exists,
                        },
                    };
                    if self
                        .filer
                        .term
                        .on_finished(*transfer, &finish, now)
                        .is_some()
                    {
                        term_finished = true;
                        if let Finish::Failed { text, .. } = &finish {
                            self.show_notice(
                                msg(Message::UiXferFailed {
                                    why: text.to_string(),
                                }),
                                cx,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        if term_finished {
            // 收尾的进度条停留一会儿后收起。
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(3100))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let now = this.filer.now_ms();
                    if this.filer.term.prune(now) {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// 终端上方的传输进度条（ZMODEM / XMODEM）；没有则 `None`。
    pub(super) fn render_term_bar(&self, sid: SessionId, th: Theme) -> Option<AnyElement> {
        let now = self.filer.now_ms();
        let bar: Bar = self.filer.term.bar(sid, now)?;
        let color = match bar.finish {
            Some(BarFinish::Failed) => th.c("status.error"),
            Some(BarFinish::Cancelled) => th.c("text.secondary"),
            _ => th.c("accent"),
        };
        let track = div()
            .relative()
            .h(px(4.0))
            .w_full()
            .rounded(px(2.0))
            .bg(th.c("bg.field"))
            .overflow_hidden();
        let track = match bar.fraction {
            Some(f) => track.child(div().h_full().w(relative(f as f32)).bg(color)),
            None if bar.finish.is_some() => track,
            None => {
                let phase = ((now % 1600) as f32 / 800.0 - 1.0).abs();
                track.child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(relative(phase * 0.7))
                        .w(relative(0.3))
                        .bg(color),
                )
            }
        };
        let core = self.core.clone();
        Some(
            div()
                .absolute()
                .left_0()
                .bottom_0()
                .w_full()
                .px(px(12.0))
                .py(px(6.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .bg(th.ca("bg.panel", 0xF0))
                .border_t_1()
                .border_color(th.c("border.subtle"))
                .text_size(px(number("font", "ui.caption")))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(th.c("text.primary"))
                                .child(bar.title.clone()),
                        )
                        .children((!bar.detail.is_empty()).then(|| {
                            div()
                                .text_color(th.c("text.secondary"))
                                .child(bar.detail.clone())
                        }))
                        .children(bar.cancellable.then(|| {
                            div()
                                .id(("term-cancel", sid.raw() as usize))
                                .size(px(18.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(3.0))
                                .cursor_pointer()
                                .hover(move |s| s.bg(th.c("bg.rowHover")))
                                .child(icon("x", 13.0, th.c("text.secondary")))
                                .on_click(move |_, _, _| core.cancel_terminal_transfer(sid))
                        })),
                )
                .child(track)
                .into_any_element(),
        )
    }

    // ---- ZMODEM / XMODEM ----

    /// 核心的文件传输类提问：打开系统文件选择器，按结果应答（取消就应答取消）。
    /// 提问留在队首直到选择结束，期间不重复打开。
    pub(super) fn begin_transfer_picker(
        &mut self,
        id: PromptId,
        request: PromptRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filer.picker == Some(id) {
            return;
        }
        let Some(plan) = picker_plan(&request) else {
            self.ctl.prompts_mut().cancel();
            return;
        };
        self.filer.picker = Some(id);
        if let Some(hint) = picker_hint(&request) {
            self.show_notice(hint, cx);
        }
        match plan {
            PickerPlan::OpenFiles { multiple } => {
                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple,
                    prompt: None,
                });
                cx.spawn_in(window, async move |this, cx| {
                    let picked = match rx.await {
                        Ok(Ok(Some(paths))) => Some(path_strings(paths)),
                        _ => None,
                    };
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.finish_transfer_picker(id, &request, picked, window, cx)
                    });
                })
                .detach();
            }
            PickerPlan::Save { suggested_name } => {
                let dir = PathBuf::from(kanso_gpui::app::term_transfer::default_save_dir(
                    self.ctl.home_dir(),
                ));
                let rx = cx.prompt_for_new_path(Path::new(&dir), Some(&suggested_name));
                cx.spawn_in(window, async move |this, cx| {
                    let picked = match rx.await {
                        Ok(Ok(Some(path))) => Some(path_strings(vec![path])),
                        _ => None,
                    };
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.finish_transfer_picker(id, &request, picked, window, cx)
                    });
                })
                .detach();
            }
        }
    }

    fn finish_transfer_picker(
        &mut self,
        id: PromptId,
        request: &PromptRequest,
        picked: Option<Vec<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filer.picker == Some(id) {
            self.filer.picker = None;
        }
        // 提问在选择期间被核心撤销（超时、会话关闭）：不再应答。
        if self.ctl.prompts().current().map(|e| e.id) == Some(id) {
            let response = response_for(request, picked);
            self.ctl.prompts_mut().respond(response);
        }
        self.sync_prompt(window, cx);
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// 会话 → 文件传输 → XMODEM 发送 / 接收。`block` 为 `None` 表示接收。
    pub(super) fn run_xmodem(&mut self, block: Option<XmodemBlock>, cx: &mut Context<Self>) {
        let Some(sid) = self.ctl.workspace().active_session() else {
            return;
        };
        if let Err(e) = self.core.xmodem_start(sid, block) {
            self.show_notice(
                msg(Message::UiXferStartFailed {
                    why: (core_error_text(&e)).to_string(),
                }),
                cx,
            );
        }
    }

    /// 取消活动会话正在进行的终端内传输。
    pub(super) fn cancel_term_transfer(&mut self) {
        if let Some(sid) = self.ctl.workspace().active_session() {
            self.core.cancel_terminal_transfer(sid);
        }
    }
}

fn path_strings(paths: Vec<PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// 单行悬停提示。
struct SimpleTip {
    text: gpui::SharedString,
    th: Theme,
}

impl gpui::Render for SimpleTip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.th;
        div()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(number("radius", "md")))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("border.strong"))
            .text_size(px(12.0))
            .text_color(th.c("text.primary"))
            .child(self.text.clone())
    }
}
