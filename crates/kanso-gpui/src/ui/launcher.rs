//! 会话启动器浮层（ui-shell.md §4）：新建本地 shell / 新建会话、搜索、按分组列出已保存的会话。
//!
//! 行为对照 mac 版 `Sessions/SessionLauncherViewController.swift`：
//! - 单击会话行或回车连接；行尾「…」与右键是同一菜单（编辑、移到分组、删除）。「…」只在悬停或键盘高亮时挂上，避免隐形按钮挡住单击。
//! - "已保存的会话"标题旁的"…"菜单：新建分组、导入 OpenSSH 配置；每个分组标题旁的"…"：重命名、删除；
//! - 搜索框始终有焦点：输入即过滤且不滚动，↑↓ 移动选中并滚入可见，回车连接选中项，Esc 关闭。
//!
//! 数据与规则在库里的 `app::sessions::LauncherModel`（三平台可测）；菜单项、确认文案在 `app::launcher`。
//! 本视图只发事件（[`LauncherEvent`]），由 `ShellView` 决定怎么开标签 / 弹编辑对话框。

use kanso_gpui::app::i18n::{label, t};
use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton,
    MouseDownEvent, ScrollHandle, Subscription, Window, div, prelude::*, px, rgb,
};
use kanso_core::Core;
use kanso_gpui::app::launcher::{
    self as logic, GroupAction, LauncherRow, ProfileAction, SavedAction, core_error_text,
};
use kanso_gpui::app::sessions::LauncherModel;
use kanso_gpui::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec, InputSpec};
use kanso_gpui::widgets::style::{Appearance, number};
use kanso_types::profile::{Group, Profile};
use kanso_types::{GroupId, ProfileId};

use super::log;
use super::shell::Theme;
use super::widgets::dialog::{Dialog, DialogEvent};
use super::widgets::icons::icon;
use super::widgets::menu::{ContextMenu, MenuEvent};
use super::widgets::text_input::{TextInput, TextInputEvent};
use kanso_types::message::Message;

/// 浮层宽度（mac 版 360）。
const PANEL_W: f32 = 360.0;
/// 浮层最大高度（mac 版 480）。
const PANEL_MAX_H: f32 = 480.0;
const ACTION_ROW_H: f32 = 34.0;
const PROFILE_ROW_H: f32 = 52.0;
const HEADER_ROW_H: f32 = 30.0;

/// 启动器向宿主发出的事件。
#[derive(Clone, Debug)]
pub(crate) enum LauncherEvent {
    /// 关闭（Esc、点浮层外）。
    Close,
    /// 打开设置。
    CustomizeShell,
    /// 新建本地 shell 标签。
    OpenLocal,
    /// 打开新建会话对话框。
    NewSession,
    /// 连接已保存的会话。
    Connect(ProfileId),
    /// 编辑已保存的会话。
    Edit(ProfileId),
    /// 导入 OpenSSH 配置（由外壳打开导入对话框）。
    ImportSshConfig,
}

/// 弹出的菜单属于谁。
#[derive(Clone)]
enum MenuTarget {
    Profile(ProfileId),
    Group(Group),
    Saved,
}

/// 正在等待结果的对话框。
#[derive(Clone)]
enum Pending {
    NewGroup,
    RenameGroup(Group),
    DeleteGroup(Group),
    DeleteProfile(ProfileId),
    /// 只有"好"按钮的提示。
    Info,
}

/// 启动器视图。
pub(crate) struct LauncherView {
    core: Arc<Core>,
    model: LauncherModel<Arc<Core>>,
    search: Entity<TextInput>,
    scroll: ScrollHandle,
    /// 鼠标悬停的会话行。行尾「…」只在这时或键盘高亮时才接收点击。
    hovered_profile: Option<ProfileId>,
    /// 嵌入左侧栏「会话」页时为 true：铺满父容器、小圆角，右键菜单与对话框交由 `ShellView` 顶层渲染。
    embedded: bool,
    root: FocusHandle,
    menu: Option<(Entity<ContextMenu>, MenuTarget)>,
    dialog: Option<(Entity<Dialog>, Pending)>,
    _subs: Vec<Subscription>,
    _menu_sub: Option<Subscription>,
    _dialog_sub: Option<Subscription>,
}

impl EventEmitter<LauncherEvent> for LauncherView {}

impl Focusable for LauncherView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root.clone()
    }
}

fn appearance(window: &Window) -> Appearance {
    super::display::window_appearance(window)
}

impl LauncherView {
    /// 创建浮层启动器并聚焦搜索框。必须在窗口上下文里创建。
    pub(crate) fn new(
        core: Arc<Core>,
        collapsed_groups: HashSet<GroupId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(core, collapsed_groups, false, window, cx)
    }

    /// 创建嵌入左侧栏「会话」页的常驻视图（不主动抢占终端焦点，由调用方按需聚焦搜索框）。
    pub(crate) fn new_embedded(
        core: Arc<Core>,
        collapsed_groups: HashSet<GroupId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(core, collapsed_groups, true, window, cx)
    }

    fn build(
        core: Arc<Core>,
        collapsed_groups: HashSet<GroupId>,
        embedded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            TextInput::new(window, cx)
                .search()
                .placeholder(Message::UiSearchSessions {})
        });
        let sub = cx.subscribe_in(
            &search,
            window,
            |this, input, ev: &TextInputEvent, window, cx| match ev {
                TextInputEvent::Change(t) => {
                    this.model.set_query(t);
                    cx.notify();
                }
                TextInputEvent::Submit(_) => {
                    if let Some(id) = this.model.selected() {
                        cx.emit(LauncherEvent::Connect(id));
                    }
                }
                TextInputEvent::Escape => {
                    if this.embedded && !this.model.query().is_empty() {
                        input.update(cx, |i, cx| i.set_text("", cx));
                        this.model.set_query("");
                        cx.notify();
                    } else {
                        this.close(window, cx);
                    }
                }
                _ => {}
            },
        );
        if !embedded {
            search.read(cx).focus(window);
        }
        LauncherView {
            model: LauncherModel::with_collapsed_groups(core.clone(), collapsed_groups),
            core,
            search,
            scroll: ScrollHandle::new(),
            hovered_profile: None,
            embedded,
            root: cx.focus_handle(),
            menu: None,
            dialog: None,
            _subs: vec![sub],
            _menu_sub: None,
            _dialog_sub: None,
        }
    }

    pub(super) fn collapsed_groups(&self) -> &HashSet<GroupId> {
        self.model.collapsed_groups()
    }

    /// 供侧栏切回「会话」页或库变更后主动刷新列表。
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.model.reload();
        cx.notify();
    }

    pub(super) fn has_overlay(&self) -> bool {
        self.menu.is_some() || self.dialog.is_some()
    }

    pub(super) fn overlay_menu(&self) -> Option<Entity<ContextMenu>> {
        self.menu.as_ref().map(|(m, _)| m.clone())
    }

    pub(super) fn overlay_dialog(&self) -> Option<Entity<Dialog>> {
        self.dialog.as_ref().map(|(d, _)| d.clone())
    }

    fn close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(LauncherEvent::Close);
    }

    /// 点浮层外面：关闭。菜单 / 对话框打开时，点击属于它们（它们的遮罩不吞事件冒泡），这里不处理。
    fn outside_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_none() && self.dialog.is_none() {
            self.close(window, cx);
        }
    }

    pub(super) fn focus_search(&self, window: &mut Window, cx: &App) {
        self.search.read(cx).focus(window);
    }

    /// 选中项滚动到可见。
    fn reveal_selected(&self) {
        let rows = logic::rows(self.model.sections());
        if let Some(i) = self
            .model
            .selected()
            .and_then(|id| logic::row_index_of(&rows, id))
        {
            self.scroll.scroll_to_item(i);
        }
    }

    // ---- 键盘 ----

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if self.menu.is_some() || self.dialog.is_some() || m.control || m.alt || m.platform {
            return;
        }
        match ev.keystroke.key.as_str() {
            "down" => {
                self.model.move_selection(1);
                self.reveal_selected();
                cx.notify();
                cx.stop_propagation();
            }
            "up" => {
                self.model.move_selection(-1);
                self.reveal_selected();
                cx.notify();
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // ---- 菜单 ----

    fn show_menu(
        &mut self,
        target: MenuTarget,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.is_some() || self.dialog.is_some() {
            return;
        }
        let items = match &target {
            MenuTarget::Profile(id) => match self.model.profile(*id) {
                Some(p) => logic::profile_menu(p, self.model.groups()),
                None => return,
            },
            MenuTarget::Group(_) => logic::group_menu(),
            MenuTarget::Saved => logic::saved_menu(),
        };
        let menu = cx.new(|cx| ContextMenu::new(items, at, window, cx));
        self._menu_sub =
            Some(
                cx.subscribe_in(&menu, window, |this, _m, ev: &MenuEvent, window, cx| {
                    let target = this.menu.take().map(|(_, t)| t);
                    this._menu_sub = None;
                    match (ev, target) {
                        (MenuEvent::Activate(id), Some(target)) => {
                            this.run_menu_action(&target, id, window, cx)
                        }
                        _ => this.focus_search(window, cx),
                    }
                    cx.notify();
                }),
            );
        self.menu = Some((menu, target));
        cx.notify();
    }

    fn run_menu_action(
        &mut self,
        target: &MenuTarget,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            MenuTarget::Profile(pid) => match logic::parse_profile_action(id) {
                Some(ProfileAction::Edit) => cx.emit(LauncherEvent::Edit(*pid)),
                Some(ProfileAction::Move(group)) => self.move_profile(*pid, group, window, cx),
                Some(ProfileAction::Delete) => self.confirm_delete_profile(*pid, window, cx),
                None => self.focus_search(window, cx),
            },
            MenuTarget::Group(group) => match logic::parse_group_action(id) {
                Some(GroupAction::Rename) => self.rename_group_dialog(group.clone(), window, cx),
                Some(GroupAction::Delete) => self.confirm_delete_group(group.clone(), window, cx),
                None => self.focus_search(window, cx),
            },
            MenuTarget::Saved => match logic::parse_saved_action(id) {
                Some(SavedAction::NewGroup) => self.new_group_dialog(window, cx),
                Some(SavedAction::ImportSshConfig) => cx.emit(LauncherEvent::ImportSshConfig),
                None => self.focus_search(window, cx),
            },
        }
    }

    // ---- 对话框 ----

    fn show_dialog(
        &mut self,
        spec: DialogSpec,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self._dialog_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, DialogEvent::Result(r): &DialogEvent, window, cx| {
                let pending = this.dialog.take().map(|(_, p)| p);
                this._dialog_sub = None;
                if let Some(p) = pending {
                    this.on_dialog_result(p, r, window, cx);
                }
                this.focus_search(window, cx);
                cx.notify();
            },
        ));
        self.dialog = Some((dialog, pending));
        cx.notify();
    }

    fn info_dialog(
        &mut self,
        title: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let spec = DialogSpec::new(title)
            .text(text)
            .button(ButtonSpec::primary("ok", label(Message::UiOk {})).cancel());
        self.show_dialog(spec, Pending::Info, window, cx);
    }

    fn error_dialog(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.info_dialog("操作没有成功", text, window, cx);
    }

    fn new_group_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = DialogSpec::new(label(Message::UiNewGroup2 {}))
            .input(InputSpec::new("name", label(Message::UiGroupName {})))
            .button(ButtonSpec::secondary("cancel", label(Message::UiCancel {})).cancel())
            .button(ButtonSpec::primary("ok", label(Message::UiCreate {})));
        self.show_dialog(spec, Pending::NewGroup, window, cx);
    }

    fn rename_group_dialog(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        let spec = DialogSpec::new(label(Message::UiRenameGroup {}))
            .input(InputSpec::new("name", label(Message::UiGroupName {})).initial(&group.name))
            .button(ButtonSpec::secondary("cancel", label(Message::UiCancel {})).cancel())
            .button(ButtonSpec::primary("ok", label(Message::UiRename2 {})));
        self.show_dialog(spec, Pending::RenameGroup(group), window, cx);
    }

    fn confirm_delete_group(&mut self, group: Group, window: &mut Window, cx: &mut Context<Self>) {
        let text = match self.model.delete_group_impact(&group) {
            Ok(impact) => logic::delete_group_message(&impact),
            Err(e) => return self.error_dialog(&core_error_text(&e), window, cx),
        };
        let spec = DialogSpec::new(&t(&Message::UiDeleteGroup {
            name: group.name.to_string(),
        }))
        .text(&text)
        .button(
            ButtonSpec::secondary("cancel", label(Message::UiCancel {}))
                .cancel()
                .default_button(),
        )
        .button(ButtonSpec::danger("delete", label(Message::UiDelete2 {})));
        self.show_dialog(spec, Pending::DeleteGroup(group), window, cx);
    }

    fn confirm_delete_profile(
        &mut self,
        id: ProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.model.profile(id).map(|p| p.name.clone()) else {
            return;
        };
        let spec = DialogSpec::new(&t(&Message::UiDeleteSession {
            name: name.to_string(),
        }))
        .text(logic::delete_profile_message())
        .button(
            ButtonSpec::secondary("cancel", label(Message::UiCancel {}))
                .cancel()
                .default_button(),
        )
        .button(ButtonSpec::danger("delete", label(Message::UiDelete2 {})));
        self.show_dialog(spec, Pending::DeleteProfile(id), window, cx);
    }

    fn on_dialog_result(
        &mut self,
        pending: Pending,
        r: &DialogResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let button = r.button.as_deref();
        match pending {
            Pending::NewGroup if button == Some("ok") => {
                let name = r.value("name").unwrap_or_default();
                if let Err(e) = self.model.create_group(name) {
                    self.error_dialog(&core_error_text(&e), window, cx);
                }
            }
            Pending::RenameGroup(group) if button == Some("ok") => {
                let name = r.value("name").unwrap_or_default();
                if let Err(e) = self.model.rename_group(&group, name) {
                    self.error_dialog(&core_error_text(&e), window, cx);
                }
            }
            Pending::DeleteGroup(group) if button == Some("delete") => {
                let core = self.core.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let result = core.delete_group(group.id).await;
                    this.update_in(cx, |this, window, cx| {
                        this.model.reload();
                        if let Err(e) = result {
                            this.error_dialog(&core_error_text(&e), window, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Pending::DeleteProfile(id) if button == Some("delete") => {
                let core = self.core.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let result = core.delete_profile(id).await;
                    this.update_in(cx, |this, window, cx| {
                        this.model.reload();
                        if let Err(e) = result {
                            this.error_dialog(&core_error_text(&e), window, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            _ => {}
        }
        self.model.reload();
    }

    fn move_profile(
        &mut self,
        id: ProfileId,
        group: Option<GroupId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.model.move_profile_to_group(id, group) {
            Ok(()) => self.focus_search(window, cx),
            Err(e) => self.error_dialog(&core_error_text(&e), window, cx),
        }
    }

    // ---- 绘制 ----

    fn render_action_row(
        &self,
        id: &'static str,
        glyph: &'static str,
        title: String,
        th: Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(ACTION_ROW_H))
            .px(px(16.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .hover(|s| s.bg(th.c("bg.rowHover")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    on_click(this, window, cx)
                }),
            )
            .child(
                div()
                    .w(px(20.0))
                    .flex()
                    .justify_center()
                    .text_color(th.c("text.secondary"))
                    .child(icon(glyph, 16.0, th.c("text.secondary"))),
            )
            .child(title)
    }

    fn small_button(&self, id: impl Into<gpui::ElementId>, th: Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(24.0))
            .min_w(px(24.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(number("radius", "sm")))
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .cursor_pointer()
            .hover(|s| s.bg(th.c("bg.tabHover")).text_color(th.c("text.primary")))
            .child(icon("ellipsis", 14.0, th.c("text.secondary")))
    }

    fn render_profile_row(
        &self,
        index: usize,
        p: &Profile,
        selected: bool,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let pid = p.id;
        let show_more = selected || self.hovered_profile == Some(pid);
        let swatch: Hsla = match p.color {
            Some(c) => rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into(),
            None => gpui::transparent_black(),
        };
        let more = self.small_button(("more", index), th).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.show_menu(MenuTarget::Profile(pid), ev.position, window, cx);
            }),
        );
        div()
            .id(("profile", index))
            .h(px(PROFILE_ROW_H))
            .flex_none()
            .px(px(16.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .when(selected, |d| d.bg(th.c("bg.tabSelected")))
            .hover(|s| s.bg(th.c("bg.rowHover")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                    this.model.select(Some(pid));
                    cx.emit(LauncherEvent::Connect(pid));
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.model.select(Some(pid));
                    this.show_menu(MenuTarget::Profile(pid), ev.position, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    if this.hovered_profile != Some(pid) {
                        this.hovered_profile = Some(pid);
                        cx.notify();
                    }
                } else if this.hovered_profile == Some(pid) {
                    this.hovered_profile = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .relative()
                    .top(px(-7.0))
                    .w(px(10.0))
                    .h(px(10.0))
                    .rounded(px(3.0))
                    .bg(swatch),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().truncate().child(p.name.clone()))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(number("font", "ui.caption")))
                            .text_color(th.c("text.tertiary"))
                            .child(logic::subtitle_text(p)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .min_w(px(24.0))
                    .when(show_more, |slot| slot.child(more)),
            )
            .into_any_element()
    }

    fn render_header_row(
        &self,
        index: usize,
        group: &Group,
        collapsed: bool,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let g = group.clone();
        let group_id = group.id;
        let title = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .when(self.model.query().trim().is_empty(), |d| {
                d.child(
                    div()
                        .id(("group-toggle", index))
                        .flex_none()
                        .size(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.model.toggle_group(group_id);
                                cx.notify();
                            }),
                        )
                        .child(icon(
                            if collapsed {
                                "chevron-right"
                            } else {
                                "chevron-down"
                            },
                            12.0,
                            th.c("text.secondary"),
                        )),
                )
            })
            .child(div().min_w_0().truncate().child(group.name.clone()));
        div()
            .id(("group", index))
            .h(px(HEADER_ROW_H))
            .flex_none()
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_between()
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .child(title)
            .child(self.small_button(("group-more", index), th).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.show_menu(MenuTarget::Group(g.clone()), ev.position, window, cx);
                }),
            ))
            .into_any_element()
    }
}

impl Render for LauncherView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Theme {
            appearance: appearance(window),
        };
        let rows = logic::rows(self.model.sections());
        let selected = self.model.selected();

        let mut list = div()
            .id("launcher-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col();
        if rows.is_empty() {
            let text = if self.model.has_any_profile() {
                logic::render_message(&Message::UiNoMatchingSessions {})
            } else {
                logic::render_message(&Message::UiNoSessions {})
            };
            list = list.child(
                div()
                    .h(px(72.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .text_color(th.c("text.tertiary"))
                    .child(text),
            );
        }
        for (i, row) in rows.iter().enumerate() {
            list = list.child(match row {
                LauncherRow::Header { group, collapsed } => {
                    self.render_header_row(i, group, *collapsed, th, cx)
                }
                LauncherRow::Profile(p) => {
                    self.render_profile_row(i, p, selected == Some(p.id), th, cx)
                }
                LauncherRow::EmptyGroup => div()
                    .h(px(28.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(th.c("text.tertiary"))
                    .child(logic::render_message(&Message::UiEmptyGroup {}))
                    .into_any_element(),
            });
        }

        let saved_header = div()
            .h(px(HEADER_ROW_H))
            .flex_none()
            .px(px(if self.embedded { 10.0 } else { 16.0 }))
            .flex()
            .items_center()
            .justify_between()
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .child(logic::render_message(&Message::UiSavedSessions {}))
            .child(self.small_button("saved-more", th).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.show_menu(MenuTarget::Saved, ev.position, window, cx);
                }),
            ));

        if self.embedded {
            log_state(self);
            return div()
                .id("sidebar-sessions")
                .key_context("Launcher")
                .track_focus(&self.root)
                .on_key_down(cx.listener(Self::on_key_down))
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .text_size(px(number("font", "ui.body")))
                .text_color(th.c("text.primary"))
                // 顶部搜索框 + ＋ 新建按钮，与「标签」页工具条等高对齐。
                .child(
                    div()
                        .h(px(40.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .mr(px(4.0))
                                .child(self.search.clone()),
                        )
                        .child(
                            div()
                                .id("sidebar-new-session")
                                .w(px(24.0))
                                .h(px(24.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(number("radius", "sm")))
                                .cursor_pointer()
                                .hover(|s| {
                                    s.bg(th.c("bg.rowHover")).text_color(th.c("text.primary"))
                                })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(LauncherEvent::NewSession);
                                    }),
                                )
                                .child(icon("plus", 16.0, th.c("text.secondary"))),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .border_t_1()
                        .border_color(th.c("border.subtle"))
                        .pt(px(4.0))
                        .flex()
                        .items_center()
                        .child(div().flex_1().min_w_0().child(self.render_action_row(
                            "launcher-local",
                            "square-terminal",
                            logic::render_message(&Message::UiLocalSession {}),
                            th,
                            |_this, _w, cx| cx.emit(LauncherEvent::OpenLocal),
                            cx,
                        )))
                        .child(
                            div()
                                .id("launcher-settings")
                                .mr(px(8.0))
                                .size(px(24.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(number("radius", "sm")))
                                .cursor_pointer()
                                .hover(|s| s.bg(th.c("bg.rowHover")))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(LauncherEvent::CustomizeShell);
                                    }),
                                )
                                .child(icon("sliders-horizontal", 15.0, th.c("text.secondary"))),
                        ),
                )
                .child(saved_header)
                .child(list);
        }

        let panel = div()
            .id("launcher-panel")
            .absolute()
            .left(px(8.0))
            .top(px(44.0))
            .w(px(PANEL_W))
            .max_h(px(PANEL_MAX_H))
            .flex()
            .flex_col()
            .py(px(8.0))
            .rounded(px(number("radius", "xl")))
            .border_1()
            .border_color(th.c("border.strong"))
            .bg(th.c("bg.popover"))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::rgba(0x00000055).into(),
                offset: gpui::point(px(0.0), px(6.0)),
                blur_radius: px(20.0),
                spread_radius: px(0.0),
            }])
            .text_size(px(number("font", "ui.body")))
            .text_color(th.c("text.primary"))
            // 浮层内的点击不能冒泡到外面的"点外部关闭"。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().child(self.render_action_row(
                        "launcher-local",
                        "square-terminal",
                        logic::render_message(&Message::UiLocalSession {}),
                        th,
                        |_this, _w, cx| cx.emit(LauncherEvent::OpenLocal),
                        cx,
                    )))
                    .child(
                        div()
                            .id("launcher-settings")
                            .mr(px(16.0))
                            .size(px(26.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(th.c("bg.rowHover")))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(LauncherEvent::CustomizeShell);
                                }),
                            )
                            .child(icon("sliders-horizontal", 16.0, th.c("text.secondary"))),
                    ),
            )
            .child(self.render_action_row(
                "launcher-new",
                "plus",
                format!("{}…", logic::render_message(&Message::UiNewSession {})),
                th,
                |_this, _w, cx| cx.emit(LauncherEvent::NewSession),
                cx,
            ))
            .child(
                div()
                    .px(px(16.0))
                    .pt(px(8.0))
                    .pb(px(12.0))
                    .child(self.search.clone()),
            )
            .child(saved_header)
            .child(list);

        log_state(self);
        div()
            .id("launcher")
            .key_context("Launcher")
            .track_focus(&self.root)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.outside_click(window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.outside_click(window, cx)),
            )
            .child(panel)
            .children(self.menu.as_ref().map(|(m, _)| m.clone()))
            .children(self.dialog.as_ref().map(|(d, _)| d.clone()))
    }
}

fn log_state(view: &LauncherView) {
    if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
        log(&format!(
            "launcher rows={} selected={:?}",
            logic::rows(view.model.sections()).len(),
            view.model.selected()
        ));
    }
}
