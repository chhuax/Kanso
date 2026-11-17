//! 窗口根视图：左侧竖向标签栏 + 分屏区。持有应用控制器（工作区模型），
//! 每个会话对应一个 [`TerminalView`] 实体；核心事件在这里统一接收并分发给对应终端。
//!
//! 行为对照 mac 版 `MainWindowController` / `Shell/` 与 `docs/spec/ui-shell.md`：
//! - 每个分屏（pane）里有若干标签，只显示活动标签；侧栏按分屏分组列出所有标签；
//! - 点击（鼠标按下）切换标签；分隔条可拖动；活动分屏有强调色边框，其余分屏压暗；
//! - 应用级快捷键查 `kanso_gpui::app::shortcuts` 的键位表，在捕获阶段拦截，
//!   其余按键照常进终端；
//! - 会话正常结束（退出码 0）自动关标签，异常结束保留并提示；最后一个标签关闭后退出应用。
//!
//! 提示框（主机密钥、密码）与关闭确认对话框由控件组负责，这里只留 TODO 与过渡方案。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Context, CursorStyle, Entity, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, Subscription, Window, WindowAppearance, div, prelude::*, px,
    rgba,
};
use kanso_core::Core;
use kanso_gpui::app::controller::{AppController, Effects, FALLBACK_GRID};
use kanso_gpui::app::layout::{LayoutDivider, PaneId, Rect, SplitAxis};
use kanso_gpui::app::shell::{
    CloseDecision, CloseGuard, DIVIDER_THICKNESS, DividerDrag, min_fraction,
};
use kanso_gpui::app::shortcuts::{AppCommand, lookup, shortcut_text};
use kanso_gpui::app::tab::{CloseNote, TabConnection};
use kanso_gpui::keys::KeyInput;
use kanso_gpui::widgets::style::{Appearance, color, number};
use kanso_types::{CoreEvent, SessionId};

use super::shell_selftest::ShellSelfTest;
use super::view::{TerminalEvent, TerminalView};
use super::{EXIT_CODE, log};

/// 界面字体。
pub(crate) const UI_FONT: &str = "Microsoft YaHei UI";

/// 一帧内最多合并处理多少个核心事件，避免事件风暴时长时间占住 UI 线程。
const MAX_EVENT_BATCH: usize = 256;

/// 当前外观下的界面配色（取自 `design/tokens.json`，与 mac 前端同一份）。
#[derive(Clone, Copy)]
pub(crate) struct Theme {
    pub appearance: Appearance,
}

impl Theme {
    pub(crate) fn c(&self, name: &str) -> Hsla {
        rgba(color(name, self.appearance).to_u32()).into()
    }

    /// 带不透明度的颜色（0..=255）。
    pub(crate) fn ca(&self, name: &str, alpha: u8) -> Hsla {
        rgba(color(name, self.appearance).with_alpha(alpha).to_u32()).into()
    }
}

/// 根视图。
pub(crate) struct ShellView {
    pub(super) core: Arc<Core>,
    pub(super) ctl: AppController<Arc<Core>>,
    pub(super) terminals: HashMap<SessionId, Entity<TerminalView>>,
    subs: HashMap<SessionId, Subscription>,
    _window_subs: Vec<Subscription>,
    pub(super) sidebar_visible: bool,
    drag: Option<DividerDrag>,
    close_guard: CloseGuard,
    /// 底部的临时提示（文字、到期时刻）。
    notice: Option<(String, Instant)>,
    dark: bool,
    window_active: bool,
    /// 最近一次通知核心“有输入焦点”的会话。
    focused: Option<SessionId>,
    last_title: String,
    /// `--selftest`：终端阶段的超时；由第一个终端取走。
    pending_terminal_selftest: Option<Duration>,
    pub(super) selftest_timeout: Option<Duration>,
    pub(super) shell_selftest: Option<ShellSelfTest>,
}

impl ShellView {
    pub(crate) fn new(
        core: Arc<Core>,
        mut rx: UnboundedReceiver<CoreEvent>,
        selftest: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let dark = is_dark(window);
        let mut this = ShellView {
            ctl: AppController::new(core.clone(), home),
            core,
            terminals: HashMap::new(),
            subs: HashMap::new(),
            _window_subs: Vec::new(),
            sidebar_visible: true,
            drag: None,
            close_guard: CloseGuard::default(),
            notice: None,
            dark,
            window_active: true,
            focused: None,
            last_title: String::new(),
            pending_terminal_selftest: selftest,
            selftest_timeout: selftest,
            shell_selftest: None,
        };

        // 核心事件 → UI 线程。核心保证每帧最多唤醒一次（TerminalDirty 合并），这里再把已到达的事件合并成一批。
        cx.spawn_in(window, async move |view, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while batch.len() < MAX_EVENT_BATCH {
                    match rx.try_next() {
                        Ok(Some(ev)) => batch.push(ev),
                        _ => break,
                    }
                }
                let ok = view
                    .update_in(cx, |this, window, cx| {
                        this.on_core_events(batch, window, cx)
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
        .detach();

        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            this.sync_focus(window, cx);
            let active = this.window_active;
            for t in this.terminals.values() {
                t.update(cx, |t, cx| t.on_window_activation(active, cx));
            }
        });
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            this.dark = is_dark(window);
            let dark = this.dark;
            for t in this.terminals.values() {
                t.update(cx, |t, cx| t.set_dark(dark, cx));
            }
            cx.notify();
        });
        this._window_subs = vec![activation, appearance];

        // 第一个标签。
        match this.ctl.open_local_shell(None, FALLBACK_GRID) {
            Ok(id) => {
                log(&format!("本地会话已打开（{id:?}）"));
                this.attach_terminal(id, window, cx);
                this.after_change(window, cx);
            }
            Err(e) => {
                log(&format!("打开本地会话失败：{e}"));
                EXIT_CODE.store(1, Ordering::SeqCst);
                cx.defer(|cx| cx.quit());
            }
        }
        this
    }

    // ---- 终端实体 ----

    fn attach_terminal(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let selftest = self.pending_terminal_selftest.take();
        let track = self.selftest_timeout.is_some();
        let dark = self.dark;
        let term = cx.new(|cx| TerminalView::new(core, id, selftest, track, dark, window, cx));
        let sub = cx.subscribe_in(
            &term,
            window,
            |this, _term, ev: &TerminalEvent, window, cx| this.on_terminal_event(ev, window, cx),
        );
        self.terminals.insert(id, term);
        self.subs.insert(id, sub);
    }

    fn on_terminal_event(
        &mut self,
        ev: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TerminalEvent::SelfTestFinished(ok) => self.on_terminal_selftest(*ok, window, cx),
        }
    }

    // ---- 核心事件 ----

    fn on_core_events(
        &mut self,
        batch: Vec<CoreEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut fx = Effects::default();
        for ev in batch {
            fx.merge(self.ctl.handle_event(ev));
        }
        if fx.terminal {
            for id in self.ctl.take_dirty() {
                if let Some(t) = self.terminals.get(&id) {
                    t.update(cx, |t, cx| t.pull_frame(cx));
                }
            }
        }
        if fx.prompt {
            // TODO(prompts-ui)：主机密钥变更 / 密码提问的对话框由控件组提供；对话框接入前，
            // 提问留在 `ctl.prompts()` 队列里（本地 shell 不会产生提问）。
            log("收到核心提问（提示框 UI 尚未接入）");
        }
        if fx.notice {
            for n in self.ctl.take_notices() {
                // TODO(notice-ui)：用 kanso-l10n 渲染并显示成提示条。
                log(&format!("核心通知 {:?} {:?}", n.level, n.message));
            }
        }
        // shell 正常退出：关掉标签（异常退出的标签保留，侧栏显示退出码）。
        let finished = self.ctl.take_auto_close();
        if !finished.is_empty() {
            log(&format!("会话已结束，自动关闭标签：{finished:?}"));
            self.close_now(&finished, window, cx);
            return;
        }
        if !fx.is_none() {
            cx.notify();
        }
    }

    // ---- 命令 ----

    pub(super) fn run_command(
        &mut self,
        command: AppCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(command, AppCommand::CloseSession) {
            self.close_guard.clear();
        }
        match command {
            AppCommand::NewLocalTab => self.open_tab(window, cx),
            AppCommand::CloseSession => {
                if let Some(id) = self.ctl.workspace().active_session() {
                    self.request_close(id, window, cx);
                }
            }
            AppCommand::NextTab => self.navigate(window, cx, |w| w.select_next_tab_in_pane()),
            AppCommand::PrevTab => self.navigate(window, cx, |w| w.select_previous_tab_in_pane()),
            AppCommand::NextPane => self.navigate(window, cx, |w| w.select_next_pane()),
            AppCommand::PrevPane => self.navigate(window, cx, |w| w.select_previous_pane()),
            AppCommand::SelectTab(i) => self.navigate(window, cx, move |w| {
                w.select_tab_at_sidebar_index(i);
            }),
            AppCommand::SplitRight => self.split(SplitAxis::Horizontal, window, cx),
            AppCommand::SplitDown => self.split(SplitAxis::Vertical, window, cx),
            AppCommand::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                cx.notify();
            }
        }
    }

    fn navigate(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut kanso_gpui::app::workspace::Workspace),
    ) {
        self.ctl.edit(f);
        self.sync_focus(window, cx);
        cx.notify();
    }

    fn open_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.ctl.open_local_shell(None, FALLBACK_GRID) {
            Ok(id) => {
                self.attach_terminal(id, window, cx);
                self.after_change(window, cx);
            }
            Err(e) => self.show_notice(format!("无法打开本地 shell：{e}"), cx),
        }
    }

    fn split(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        match self.ctl.split(axis, FALLBACK_GRID) {
            Ok(id) => {
                self.attach_terminal(id, window, cx);
                self.after_change(window, cx);
            }
            Err(e) => self.show_notice(format!("无法拆分：{e}"), cx),
        }
    }

    fn request_close(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let needs_confirm = self.ctl.close_confirmation(&[id]).is_some();
        match self.close_guard.request(id, needs_confirm, Instant::now()) {
            CloseDecision::Close => self.close_now(&[id], window, cx),
            CloseDecision::Confirm => {
                // TODO(dialog)：对话框就绪后改为弹出确认框（列出会话、说明传输会被中断）。
                let key = shortcut_text(AppCommand::CloseSession).unwrap_or_default();
                self.show_notice(format!("该会话仍在运行，再按一次 {key} 确认关闭"), cx);
            }
        }
    }

    /// 立即关闭（确认已完成）。
    pub(super) fn close_now(
        &mut self,
        sessions: &[SessionId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closed = self.ctl.close_sessions(sessions);
        for id in closed {
            self.terminals.remove(&id);
            self.subs.remove(&id);
            if self.focused == Some(id) {
                self.focused = None;
            }
        }
        self.after_change(window, cx);
    }

    /// 工作区结构变化后的收尾：空了就退出，否则把焦点交给活动终端并重绘。
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ctl.workspace().is_empty() {
            log("最后一个标签已关闭，退出应用");
            cx.quit();
            return;
        }
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// 点击某个标签（侧栏）或分屏：切换活动会话。
    pub(super) fn activate_session(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ctl.workspace().active_session() == Some(id) {
            // 已经是活动标签：只确保焦点在它的终端里。
            self.sync_focus(window, cx);
            return;
        }
        self.close_guard.clear();
        self.ctl.edit(|w| w.activate(id));
        self.sync_focus(window, cx);
        cx.notify();
    }

    pub(super) fn activate_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ctl.edit(|w| w.activate_pane(pane));
        self.sync_focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_tab_clicked(
        &mut self,
        id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_close(id, window, cx);
    }

    pub(super) fn new_tab_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_command(AppCommand::NewLocalTab, window, cx);
    }

    /// 键盘焦点与核心的“有焦点”状态都跟随活动会话。
    pub(super) fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.ctl.workspace().active_session();
        let want = if self.window_active { active } else { None };
        if self.focused != want {
            if let Some(old) = self.focused {
                let _ = self.core.focus(old, false);
            }
            if let Some(new) = want {
                let _ = self.core.focus(new, true);
            }
            self.focused = want;
        }
        if let Some(handle) = active
            .and_then(|a| self.terminals.get(&a))
            .map(|t| t.read(cx).focus.clone())
            && !handle.is_focused(window)
        {
            window.focus(&handle);
        }
    }

    fn show_notice(&mut self, text: String, cx: &mut Context<Self>) {
        log(&format!("提示：{text}"));
        self.notice = Some((text, Instant::now() + Duration::from_secs(3)));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(3100))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    // ---- 键盘 ----

    /// 捕获阶段：命中应用级快捷键就执行并吞掉按键，终端收不到。
    fn on_capture_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let input = KeyInput {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            is_held: ev.is_held,
        };
        let Some(command) = lookup(&input) else {
            return;
        };
        cx.stop_propagation();
        if ev.is_held && !command.repeatable() {
            return;
        }
        if super::LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!("shortcut {command:?}"));
        }
        self.run_command(command, window, cx);
    }

    // ---- 分隔条 ----

    /// 分屏区矩形（窗口坐标）。
    pub(super) fn split_area(&self, window: &Window) -> Rect {
        let size = window.viewport_size();
        let x = if self.sidebar_visible {
            f64::from(number("spacing", "sidebar.defaultWidth"))
        } else {
            0.0
        };
        let w = f64::from(f32::from(size.width));
        let h = f64::from(f32::from(size.height));
        Rect::new(x, 0.0, (w - x).max(0.0), h)
    }

    fn start_drag(&mut self, divider: &LayoutDivider, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let mouse = axis_coord(divider.axis, ev.position);
        self.drag = Some(DividerDrag::start(divider, mouse));
        cx.stop_propagation();
        cx.notify();
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.clone() else {
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let area = self.split_area(window);
        let dividers = self
            .ctl
            .workspace()
            .layout()
            .dividers(area, DIVIDER_THICKNESS);
        let Some(current) = dividers.iter().find(|d| drag.is(d)) else {
            self.drag = None;
            return;
        };
        let delta = drag.delta(current, axis_coord(drag.axis, ev.position));
        if delta != 0.0 {
            let (path, index, parent) = (
                current.split_path.clone(),
                current.index,
                current.parent_length,
            );
            self.ctl
                .edit(|w| w.move_divider(&path, index, delta, min_fraction(parent)));
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    /// 模拟一次分隔条拖动（自检用，走与鼠标相同的换算）。
    pub(super) fn drag_divider_by(&mut self, window: &Window, px_delta: f64) -> bool {
        let area = self.split_area(window);
        let Some(d) = self
            .ctl
            .workspace()
            .layout()
            .dividers(area, DIVIDER_THICKNESS)
            .into_iter()
            .next()
        else {
            return false;
        };
        let start = axis_coord_f(d.axis, d.rect.x, d.rect.y) + 1.0;
        let drag = DividerDrag::start(&d, start);
        let delta = drag.delta(&d, start + px_delta);
        self.ctl
            .edit(|w| w.move_divider(&d.split_path, d.index, delta, min_fraction(d.parent_length)));
        true
    }

    // ---- 绘制 ----

    fn theme(&self) -> Theme {
        Theme {
            appearance: if self.dark {
                Appearance::Dark
            } else {
                Appearance::Light
            },
        }
    }

    fn render_panes(&self, area: Rect, th: Theme, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let ws = self.ctl.workspace();
        let frames = ws.layout().frames(area, DIVIDER_THICKNESS);
        let dividers = ws.layout().dividers(area, DIVIDER_THICKNESS);
        let active_pane = ws.active_pane();
        let multiple = frames.len() > 1;
        let mut out: Vec<gpui::AnyElement> = Vec::new();

        for (pane, r) in frames {
            let session = ws.active_tab_in(pane);
            let is_active = pane == active_pane;
            let mut el = div()
                .id(("pane", pane.0 as usize))
                .absolute()
                .left(px(r.x as f32))
                .top(px(r.y as f32))
                .w(px(r.width as f32))
                .h(px(r.height as f32))
                .overflow_hidden()
                .bg(th.c("bg.window"));
            match session.and_then(|s| self.terminals.get(&s).map(|t| (s, t.clone()))) {
                Some((sid, term)) => {
                    el = el
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                this.activate_session(sid, window, cx)
                            }),
                        )
                        .child(term);
                    // 会话已异常结束：窗格底部给出原因（退出码 / 断开），标签保留到用户手动关闭。
                    if let Some(item) = ws.item(sid)
                        && let Some(text) = closed_banner(&item.connection)
                    {
                        el = el.child(
                            div()
                                .absolute()
                                .left_0()
                                .bottom_0()
                                .w_full()
                                .px(px(10.0))
                                .py(px(4.0))
                                .bg(th.ca("bg.panel", 0xE6))
                                .text_size(px(12.0))
                                .text_color(th.c("status.warning"))
                                .child(text),
                        );
                    }
                }
                None => {
                    el = el.child(empty_state(th));
                }
            }
            if multiple {
                // 焦点一眼可见：活动分屏描强调色边框，其余分屏压暗。
                el = el.child(if is_active {
                    div()
                        .absolute()
                        .size_full()
                        .border_2()
                        .border_color(th.c("accent"))
                } else {
                    div().absolute().size_full().bg(th.ca("bg.window", 0x59))
                });
            }
            out.push(el.into_any_element());
        }

        for (i, d) in dividers.into_iter().enumerate() {
            let dragging = self.drag.as_ref().is_some_and(|x| x.is(&d));
            let horizontal = d.axis == SplitAxis::Horizontal;
            let rect = d.rect;
            let start = d.clone();
            let el = div()
                .id(("divider", i))
                .absolute()
                .left(px(rect.x as f32))
                .top(px(rect.y as f32))
                .w(px(rect.width as f32))
                .h(px(rect.height as f32))
                .bg(if dragging {
                    th.c("accent")
                } else {
                    th.c("border.strong")
                })
                .hover(|s| s.bg(th.c("accent")))
                .cursor(if horizontal {
                    CursorStyle::ResizeLeftRight
                } else {
                    CursorStyle::ResizeUpDown
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                        this.start_drag(&start, ev, cx)
                    }),
                );
            out.push(el.into_any_element());
        }
        out
    }
}

fn is_dark(window: &Window) -> bool {
    !matches!(
        window.appearance(),
        WindowAppearance::Light | WindowAppearance::VibrantLight
    )
}

fn axis_coord(axis: SplitAxis, p: gpui::Point<Pixels>) -> f64 {
    axis_coord_f(axis, f64::from(f32::from(p.x)), f64::from(f32::from(p.y)))
}

fn axis_coord_f(axis: SplitAxis, x: f64, y: f64) -> f64 {
    match axis {
        SplitAxis::Horizontal => x,
        SplitAxis::Vertical => y,
    }
}

/// 异常结束的会话在窗格底部显示的文字；正常运行 / 用户主动关闭没有。
fn closed_banner(connection: &TabConnection) -> Option<String> {
    match connection {
        TabConnection::Closed(Some(CloseNote::Exited { code: Some(c) })) => {
            Some(format!("会话已结束（退出码 {c}）。Ctrl+Shift+W 关闭此标签"))
        }
        TabConnection::Closed(Some(CloseNote::Exited { code: None })) => {
            Some("会话已结束。Ctrl+Shift+W 关闭此标签".to_owned())
        }
        // TODO(l10n)：用 kanso-l10n 渲染错误消息。
        TabConnection::Closed(Some(CloseNote::Error(_))) => {
            Some("会话已断开。Ctrl+Shift+W 关闭此标签".to_owned())
        }
        _ => None,
    }
}

/// 没有任何会话时的空状态（正常流程里最后一个标签关闭就退出，只在打开失败等边缘情况出现）。
fn empty_state(th: Theme) -> gpui::AnyElement {
    let key = shortcut_text(AppCommand::NewLocalTab).unwrap_or_default();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .text_color(th.c("text.secondary"))
        .child("没有打开的会话")
        .child(
            div()
                .text_size(px(12.0))
                .text_color(th.c("text.tertiary"))
                .child(format!("{key} 新建本地 Shell")),
        )
        .into_any_element()
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.theme();
        let area = self.split_area(window);

        // 窗口标题跟随活动标签。
        let title = self
            .ctl
            .workspace()
            .active_session()
            .and_then(|s| self.ctl.tab_title(s))
            .map_or_else(|| "Kanso".to_owned(), |t| format!("{t} - Kanso"));
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }

        let panes = self.render_panes(area, th, cx);
        let sidebar = self.sidebar_visible.then(|| self.render_sidebar(th, cx));
        let notice = self
            .notice
            .as_ref()
            .filter(|(_, until)| *until > Instant::now())
            .map(|(text, _)| text.clone());

        div()
            .relative()
            .size_full()
            .bg(th.c("bg.window"))
            .font_family(UI_FONT)
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            .key_context("Shell")
            .capture_key_down(cx.listener(Self::on_capture_key))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .children(sidebar)
            .children(panes)
            .children(notice.map(|text| {
                div()
                    .absolute()
                    .left(px(area.x as f32))
                    .bottom(px(14.0))
                    .w(px(area.width as f32))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(14.0))
                            .py(px(7.0))
                            .rounded(px(number("radius", "md")))
                            .bg(th.c("bg.popover"))
                            .border_1()
                            .border_color(th.c("border.strong"))
                            .text_size(px(12.0))
                            .child(text),
                    )
            }))
    }
}
