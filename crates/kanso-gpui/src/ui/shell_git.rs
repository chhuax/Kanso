//! `ShellView` 与 Git 改动面板的接线（filer.md §10）：
//!
//! - 面板在终端右侧，可与左侧 Filer 同时打开；初始宽度 420，拖动范围 280–760，重启后不记忆；
//! - 入口是状态栏右侧的“改动”按钮（Windows 没有原生标题栏，状态栏与 Filer 开关同一排）；
//! - 跟随活动标签：只对已连接的本地 shell 读取 Git 数据，依据 shell 当前目录；
//! - 命令结束、位置变化时（节流后）自动刷新；
//! - 面板要的菜单与确认框由外壳承载，结果交回面板。

use gpui::{
    AnyElement, Context, CursorStyle, Entity, MouseButton, MouseDownEvent, Subscription, Window,
    div, prelude::*, px,
};
use kanso_gpui::app::filer_text::msg;
use kanso_gpui::app::git_panel::{GitLayout, target_for, totals_text};
use kanso_gpui::app::tab::TabActivity;
use kanso_gpui::widgets::dialog::DialogSpec;
use kanso_gpui::widgets::menu::MenuItem;
use kanso_gpui::widgets::style::number;
use kanso_types::message::Message;
use kanso_types::{CoreEvent, SessionId};

use super::git_panel::{GitEvent, GitPanelView, with_tip};
use super::shell::{ShellView, Theme, status_bar_h};
use super::widgets::dialog::{Dialog, DialogEvent};
use super::widgets::icons::icon;
use super::widgets::menu::{ContextMenu, MenuEvent};

/// 外壳持有的 Git 面板状态。
pub(super) struct GitState {
    panel: Entity<GitPanelView>,
    _panel_sub: Subscription,
    /// 显隐与宽度（不持久化）。
    layout: GitLayout,
    /// 拖动宽度：(按下时的鼠标 x, 按下时的宽度)。
    drag: Option<(f32, f32)>,
    /// 自检：固定的读取对象（不跟随活动标签）。
    pub(super) selftest_target: Option<kanso_gpui::app::git_panel::GitTarget>,
    /// 面板请求的确认框。
    pub(super) dialog: Option<Entity<Dialog>>,
    dialog_sub: Option<Subscription>,
}

impl GitState {
    /// 面板是否显示（给菜单的勾选状态用）。
    pub(super) fn visible(&self) -> bool {
        self.layout.visible
    }

    /// 自检用：设置显隐。
    pub(super) fn layout_set_visible(&mut self, visible: bool) {
        self.layout.visible = visible;
    }

    /// 自检用：面板实体。
    pub(super) fn panel_entity(&self) -> Entity<GitPanelView> {
        self.panel.clone()
    }
}

/// 在 `ShellView::new` 里建面板及其事件订阅。
pub(super) fn new_state(window: &mut Window, cx: &mut Context<ShellView>) -> GitState {
    let panel = cx.new(|cx| GitPanelView::new(window, cx));
    let sub = cx.subscribe_in(&panel, window, |this, _panel, ev: &GitEvent, window, cx| {
        this.on_git_event(ev, window, cx)
    });
    GitState {
        panel,
        _panel_sub: sub,
        layout: GitLayout::default(),
        drag: None,
        selftest_target: None,
        dialog: None,
        dialog_sub: None,
    }
}

fn local_names() -> Vec<String> {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .collect()
}

/// 这批核心事件里，哪些会话的“命令结束 / 位置变化”值得刷新 Git 摘要。
fn refresh_sessions(batch: &[CoreEvent]) -> Vec<SessionId> {
    batch
        .iter()
        .filter_map(|ev| match ev {
            CoreEvent::LocationChanged { session, .. } => Some(*session),
            CoreEvent::ActivityChanged { session, activity }
                if !TabActivity::from(*activity).is_busy() =>
            {
                Some(*session)
            }
            _ => None,
        })
        .collect()
}

impl ShellView {
    // ---- 宽度 ----

    /// 面板占用的宽度（逻辑像素）：Filer 之后剩余的宽度里，保证终端仍可操作。
    pub(super) fn git_width(&self, window: &Window) -> f32 {
        let total = f32::from(window.viewport_size().width);
        let sidebar = if self.sidebar_visible {
            number("spacing", "sidebar.defaultWidth")
        } else {
            0.0
        };
        self.git
            .layout
            .effective_width(total - sidebar - self.filer_width(window))
    }

    // ---- 跟随活动标签与刷新 ----

    /// 让面板跟随活动标签（只有目标变化时才会动面板）。
    pub(super) fn sync_git(&mut self, cx: &mut Context<Self>) {
        let ws = self.ctl.workspace();
        let item = ws.active_session().and_then(|s| ws.item(s));
        let names = local_names();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let target = self
            .git
            .selftest_target
            .clone()
            .or_else(|| target_for(item, &refs));
        let panel = self.git.panel.clone();
        if panel.read(cx).model.target == target && !panel.read(cx).model.writing {
            return;
        }
        panel.update(cx, |p, cx| p.follow(target, cx));
    }

    /// 核心事件处理完（工作区已更新）之后调用：跟随标签，并对命令结束 / 位置变化做节流刷新。
    pub(super) fn git_after_events(&mut self, sessions: &[SessionId], cx: &mut Context<Self>) {
        self.sync_git(cx);
        let panel = self.git.panel.clone();
        let Some(target) = panel.read(cx).model.target.clone() else {
            return;
        };
        if sessions.contains(&target.session) {
            panel.update(cx, |p, cx| p.request_refresh(false, cx));
        }
    }

    /// 从一批核心事件里取出要触发刷新的会话（在事件被消费前调用）。
    pub(super) fn git_event_sessions(batch: &[CoreEvent]) -> Vec<SessionId> {
        refresh_sessions(batch)
    }

    // ---- 事件、菜单、对话框 ----

    fn on_git_event(&mut self, ev: &GitEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            GitEvent::Close => {
                self.git.layout.visible = false;
                self.sync_focus(window, cx);
                cx.notify();
            }
            GitEvent::Changed => cx.notify(),
            GitEvent::Menu { items, position } => {
                self.open_git_menu(items.clone(), *position, window, cx)
            }
            GitEvent::Dialog(spec) => self.open_git_dialog(spec.clone(), window, cx),
        }
    }

    fn open_git_menu(
        &mut self,
        items: Vec<MenuItem>,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() || self.filer.dialog.is_some() || self.git.dialog.is_some() {
            return;
        }
        self.menu_sub = None;
        let menu = cx.new(|cx| ContextMenu::new(items, position, window, cx));
        self.menu_sub =
            Some(
                cx.subscribe_in(&menu, window, |this, _m, ev: &MenuEvent, window, cx| {
                    this.menu = None;
                    this.menu_sub = None;
                    if let MenuEvent::Activate(id) = ev {
                        let id = id.clone();
                        let panel = this.git.panel.clone();
                        panel.update(cx, |p, cx| p.on_menu_action(&id, cx));
                    }
                    this.sync_focus(window, cx);
                    cx.notify();
                }),
            );
        self.menu = Some(menu);
        cx.notify();
    }

    fn open_git_dialog(&mut self, spec: DialogSpec, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.menu_sub = None;
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self.git.dialog_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, DialogEvent::Result(r): &DialogEvent, window, cx| {
                this.git.dialog = None;
                this.git.dialog_sub = None;
                let r = r.clone();
                let panel = this.git.panel.clone();
                panel.update(cx, |p, cx| p.on_dialog_result(&r, cx));
                this.sync_focus(window, cx);
                cx.notify();
            },
        ));
        self.git.dialog = Some(dialog);
        cx.notify();
    }

    // ---- 显隐 ----

    /// 视图菜单 / 状态栏按钮：显示或收起。没有专用默认快捷键。
    pub(super) fn toggle_git(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.git.layout.visible = !self.git.layout.visible;
        if !self.git.layout.visible {
            self.sync_focus(window, cx);
        }
        cx.notify();
    }

    // ---- 绘制 ----

    /// 终端右侧的面板与左边缘的拖动条。
    pub(super) fn render_git(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let w = self.git_width(window);
        if w <= 0.0 {
            return Vec::new();
        }
        let total = f32::from(window.viewport_size().width);
        let x = total - w;
        let h = (f32::from(window.viewport_size().height) - status_bar_h()).max(0.0);
        let panel = div()
            .id("git-host")
            .absolute()
            .left(px(x))
            .top_0()
            .w(px(w))
            .h(px(h))
            .border_l_1()
            .border_color(th.c("border.subtle"))
            .child(self.git.panel.clone());
        let handle = div()
            .id("git-resize")
            .absolute()
            .left(px(x - 3.0))
            .top_0()
            .w(px(6.0))
            .h(px(h))
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(move |s| s.bg(th.ca("accent", 0x55)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    this.git.drag = Some((f32::from(ev.position.x), this.git.layout.width));
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        vec![panel.into_any_element(), handle.into_any_element()]
    }

    /// 拖动中：返回 `true` 表示这次鼠标移动被宽度拖动吃掉。
    pub(super) fn git_drag_move(
        &mut self,
        ev: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((start_x, start_w)) = self.git.drag else {
            return false;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.git.drag = None;
            return false;
        }
        self.git.layout.width =
            self.git
                .layout
                .drag_width(start_w, start_x, f32::from(ev.position.x));
        cx.notify();
        true
    }

    /// 结束宽度拖动（宽度不保存）。
    pub(super) fn git_drag_end(&mut self) {
        self.git.drag = None;
    }

    /// 状态栏右侧的“改动”按钮：分支图标；有仓库摘要且有改动时带总新增 / 总删除。
    pub(super) fn git_status_item(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let on = self.git.layout.visible;
        let stats = self.git.panel.read(cx).model.button_stats();
        let color = if on {
            th.c("accent")
        } else {
            th.c("text.secondary")
        };
        let el = div()
            .id("status-git")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .py(px(2.0))
            .rounded(px(number("radius", "md")))
            .cursor_pointer()
            .hover(move |s| s.bg(th.c("bg.rowHover")))
            .text_color(color)
            .child(icon("git-branch", 13.0, color))
            .children(stats.map(|(a, d)| {
                let (plus, minus) = totals_text(a, d);
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(div().text_color(th.c("diff.added")).child(plus))
                    .child(div().text_color(th.c("diff.removed")).child(minus))
            }))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_git(window, cx)));
        with_tip(el, msg(Message::UiGitTitle {}), th).into_any_element()
    }
}
