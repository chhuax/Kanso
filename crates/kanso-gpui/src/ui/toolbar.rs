//! 主窗口工具栏和命令搜索。结果保留标签和会话的 ID，打开时再交给原来的动作处理。
use super::shell::{ShellView, Theme};
use super::widgets::{
    icons::icon,
    text_input::{TextInput, TextInputEvent},
};
use gpui::{
    Context, Entity, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ScrollHandle,
    Subscription, Window, div, point, prelude::*, px,
};
use kanso_gpui::app::launcher::{render_message, subtitle_text};
use kanso_gpui::app::menus::{app_menu, decode};
use kanso_gpui::app::shortcuts::AppCommand;
use kanso_gpui::widgets::menu::MenuItem;
use kanso_types::{ProfileId, SessionId, message::Message};

pub(super) const HEIGHT: f32 = 34.0;
const CAPTION_BUTTON_W: f32 = 44.0;

pub(super) fn custom_decorations(window: &Window) -> bool {
    cfg!(windows)
        || matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        )
}

/// Windows 使用系统命中区域；Linux 将相同按钮接到 GPUI 窗口动作。
pub(super) fn window_buttons(
    th: Theme,
    window: &Window,
    cx: &mut Context<ShellView>,
) -> impl IntoElement {
    use gpui::WindowControlArea::{Close, Max, Min};
    let button = |id, area, symbol, close| {
        div()
            .id(id)
            .w(px(CAPTION_BUTTON_W))
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .window_control_area(area)
            .when(cfg!(target_os = "linux"), |button| {
                button.on_click(cx.listener(move |this, _, window, cx| match area {
                    Min => window.minimize_window(),
                    Max => window.zoom_window(),
                    Close => this.run_command(AppCommand::Quit, window, cx),
                    _ => unreachable!("窗口按钮只有最小化、最大化与关闭"),
                }))
            })
            .hover(|s| {
                s.bg(if close {
                    th.c("status.error")
                } else {
                    th.c("bg.rowHover")
                })
            })
            .child(icon(symbol, 14., th.c("text.primary")))
    };
    div()
        .absolute()
        .top_0()
        .right_0()
        .flex()
        .bg(th.c("bg.window"))
        .child(button("caption-min", Min, "minus", false))
        .child(button(
            "caption-max",
            Max,
            if window.is_maximized() {
                "copy"
            } else {
                "square"
            },
            false,
        ))
        .child(button("caption-close", Close, "x", true))
}

/// Linux 客户端装饰只补窗口边缘的命中区，缩放仍由窗口管理器执行。
pub(super) fn resize_borders(window: &Window) -> Vec<gpui::AnyElement> {
    use gpui::{CursorStyle as C, ResizeEdge as E};
    let w = f32::from(window.viewport_size().width);
    let h = f32::from(window.viewport_size().height);
    let border = 4.;
    [
        (E::Top, border, 0., w - 2. * border, border, C::ResizeUpDown),
        (
            E::Bottom,
            border,
            h - border,
            w - 2. * border,
            border,
            C::ResizeUpDown,
        ),
        (
            E::Left,
            0.,
            border,
            border,
            h - 2. * border,
            C::ResizeLeftRight,
        ),
        (
            E::Right,
            w - border,
            border,
            border,
            h - 2. * border,
            C::ResizeLeftRight,
        ),
        (E::TopLeft, 0., 0., border, border, C::ResizeUpLeftDownRight),
        (
            E::TopRight,
            w - border,
            0.,
            border,
            border,
            C::ResizeUpRightDownLeft,
        ),
        (
            E::BottomLeft,
            0.,
            h - border,
            border,
            border,
            C::ResizeUpRightDownLeft,
        ),
        (
            E::BottomRight,
            w - border,
            h - border,
            border,
            border,
            C::ResizeUpLeftDownRight,
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (edge, x, y, width, height, cursor))| {
        div()
            .id(("window-resize", id))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .h(px(height))
            .occlude()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
            .into_any_element()
    })
    .collect()
}

#[derive(Clone)]
enum Target {
    Tab(SessionId),
    Profile(ProfileId),
    Command(AppCommand),
    File(SessionId, std::path::PathBuf),
}

impl Target {
    fn group(&self) -> usize {
        match self {
            Self::Tab(_) => 0,
            Self::Profile(_) => 1,
            Self::Command(_) => 2,
            Self::File(..) => 3,
        }
    }
}

struct Hit {
    title: String,
    detail: String,
    target: Target,
}

pub(super) struct Palette {
    input: Entity<TextInput>,
    _subscription: Subscription,
    hits: Vec<Hit>,
    visible: Vec<usize>,
    pub(super) selected: usize,
    scroll: ScrollHandle,
}

impl Palette {
    fn filter(&mut self, query: &str) {
        let q = query.trim().to_lowercase();
        let mut ranked = Vec::new();
        for (index, hit) in self.hits.iter().enumerate() {
            let score = if q.is_empty() {
                Some(0)
            } else {
                [hit.title.as_str(), hit.detail.as_str()]
                    .into_iter()
                    .filter_map(|text| kanso_gpui::app::launcher::command_search_score(text, &q))
                    .min()
            };
            if let Some(score) = score {
                ranked.push((hit.target.group(), score, index));
            }
        }
        ranked.sort_by(|a, b| {
            (a.0, a.1)
                .cmp(&(b.0, b.1))
                .then_with(|| self.hits[a.2].title.cmp(&self.hits[b.2].title))
        });
        self.visible = ranked.into_iter().map(|(_, _, index)| index).collect();
        self.selected = 0;
        self.scroll.set_offset(point(px(0.), px(0.)));
    }
}

fn menu_hits(items: Vec<MenuItem>, out: &mut Vec<Hit>) {
    for item in items {
        match item {
            MenuItem::Action {
                id,
                label,
                shortcut,
                enabled: true,
                ..
            } => {
                if let Some(cmd) = decode(&id) {
                    out.push(Hit {
                        title: label,
                        detail: shortcut.unwrap_or_default(),
                        target: Target::Command(cmd),
                    });
                }
            }
            MenuItem::Submenu {
                enabled: true,
                items,
                ..
            } => menu_hits(items, out),
            _ => {}
        }
    }
}

impl ShellView {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.ctl.workspace();
        let local_session = ws.active_session().filter(|id| {
            ws.item(*id)
                .is_some_and(|item| item.kind == kanso_gpui::app::tab::TabKind::Local)
        });
        let mut hits = Vec::new();
        for id in ws.sidebar_order() {
            if let Some(item) = ws.item(id) {
                let title = self.ctl.tab_title(id).unwrap_or_else(|| item.name.clone());
                let detail = if title != item.name {
                    item.name.clone()
                } else {
                    item.cwd.clone().unwrap_or_default()
                };
                hits.push(Hit {
                    title,
                    detail,
                    target: Target::Tab(id),
                });
            }
        }
        for p in self.core.profiles() {
            let detail = subtitle_text(&p);
            hits.push(Hit {
                title: p.name,
                detail,
                target: Target::Profile(p.id),
            });
        }
        menu_hits(app_menu(self.app_menu_state(cx)), &mut hits);
        let input = cx.new(|cx| {
            TextInput::new(window, cx)
                .search()
                .placeholder(Message::UiPaletteHint {})
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Change(q) => {
                    if let Some(p) = &mut this.palette {
                        p.filter(q);
                        cx.notify();
                    }
                }
                TextInputEvent::Escape => this.dismiss_palette(window, cx),
                TextInputEvent::Submit(_) => this.choose_result(window, cx),
                _ => {}
            },
        );
        input.read(cx).focus(window);
        self.palette = Some(Palette {
            input: input.clone(),
            _subscription: subscription,
            visible: (0..hits.len()).collect(),
            hits,
            selected: 0,
            scroll: ScrollHandle::new(),
        });
        if let Some(session) = local_session {
            let core = self.core.clone();
            cx.spawn(async move |this, cx| {
                let Ok(dir) = core.working_directory(session).await else {
                    return;
                };
                let files = cx
                    .background_executor()
                    .spawn(async move {
                        std::fs::read_dir(dir)
                            .into_iter()
                            .flatten()
                            .filter_map(Result::ok)
                            .map(|entry| Hit {
                                title: entry.file_name().to_string_lossy().into_owned(),
                                detail: String::new(),
                                target: Target::File(session, entry.path()),
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let Some(p) = this.palette.as_mut().filter(|p| p.input == input) else {
                        return;
                    };
                    let previous = p.visible.get(p.selected).copied();
                    let query = input.read(cx).text().to_owned();
                    p.hits.extend(files);
                    p.filter(&query);
                    if let Some(position) =
                        previous.and_then(|id| p.visible.iter().position(|index| *index == id))
                    {
                        p.selected = position;
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn dismiss_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.sync_focus(window, cx);
        cx.notify();
    }

    fn choose_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.palette else { return };
        let Some(&index) = p.visible.get(p.selected) else {
            return;
        };
        let target = p.hits[index].target.clone();
        self.dismiss_palette(window, cx);
        match target {
            Target::Tab(id) => self.activate_session(id, window, cx),
            Target::Profile(id) => self.connect_profile(id, window, cx),
            Target::Command(cmd) => self.run_command(cmd, window, cx),
            Target::File(sid, path) => self.reveal_palette_file(sid, &path, window, cx),
        }
    }

    pub(super) fn palette_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(p) = &mut self.palette else { return };
        let modifiers = &ev.keystroke.modifiers;
        if p.input.read(cx).is_composing()
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
        {
            return;
        }
        match ev.keystroke.key.as_str() {
            "tab" if modifiers.shift => p.selected = p.selected.saturating_sub(1),
            "up" => p.selected = p.selected.saturating_sub(1),
            "down" | "tab" => p.selected = (p.selected + 1).min(p.visible.len().saturating_sub(1)),
            _ => return,
        }
        p.scroll.scroll_to_item(p.selected);
        cx.stop_propagation();
        cx.notify();
    }

    /// 顶部菜单栏的一级项：子菜单在按钮下方弹出，动作项直接执行命令。
    fn open_top_menu_item(
        &mut self,
        index: usize,
        x: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = app_menu(self.app_menu_state(cx));
        match items.get(index) {
            Some(MenuItem::Submenu { items, .. }) => {
                self.open_menu(items.clone(), point(x, px(HEIGHT)), window, cx);
            }
            Some(MenuItem::Action { id, .. }) => {
                if let Some(command) = decode(id) {
                    self.run_command(command, window, cx);
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_toolbar(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let controls = if window.is_fullscreen() || !custom_decorations(window) {
            0.0
        } else {
            3.0 * CAPTION_BUTTON_W
        };
        let button = |id, name| {
            div()
                .id(id)
                .occlude()
                .flex_none()
                .w(px(28.))
                .h(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .cursor_pointer()
                .hover(|s| s.bg(th.c("bg.rowHover")))
                .child(icon(name, 16., th.c("text.secondary")))
        };
        // 把应用菜单的一级项铺在左侧，替代原先右上角的汉堡按钮。
        let mut menu_bar = div().flex().flex_none().items_center().gap(px(2.));
        for (index, item) in app_menu(self.app_menu_state(cx)).iter().enumerate() {
            if matches!(item, MenuItem::Separator) {
                continue;
            }
            menu_bar = menu_bar.child(
                div()
                    .id(("app-menu-bar", index))
                    .occlude()
                    .flex_none()
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(th.c("text.secondary"))
                    .hover(|s| s.bg(th.c("bg.rowHover")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, w, cx| {
                            this.open_top_menu_item(index, ev.position.x, w, cx);
                        }),
                    )
                    .child(item.label().to_owned()),
            );
        }
        div()
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(HEIGHT))
            .pl(px(8.))
            .pr(px(controls + 4.))
            .flex()
            .items_center()
            .justify_between()
            .bg(th.c("bg.window"))
            .child(
                div()
                    .id("caption-drag")
                    .absolute()
                    .inset_0()
                    .window_control_area(gpui::WindowControlArea::Drag)
                    .when(cfg!(target_os = "linux"), |drag| {
                        drag.on_mouse_down(MouseButton::Left, |event, window, _| {
                            if event.click_count == 2 {
                                window.zoom_window();
                            } else {
                                window.start_window_move();
                            }
                        })
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    // 顶部标签条模式下没有左侧竖栏，这个开关的图标会误导，隐藏；菜单与快捷键仍可切换标签条。
                    .when(!self.tab_bar_top, |bar| {
                        bar.child(button("sidebar-toggle", "panel-left").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, w, cx| {
                                this.run_command(AppCommand::ToggleSidebar, w, cx)
                            }),
                        ))
                    })
                    .child(menu_bar),
            )
            .child(
                div()
                    .id("toolbar-search")
                    .occlude()
                    .flex_1()
                    .max_w(px(240.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(th.c("border.field"))
                    .bg(th.c("bg.field"))
                    .text_color(th.c("text.tertiary"))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, w, cx| {
                            this.run_command(AppCommand::OpenPalette, w, cx)
                        }),
                    )
                    .child(icon("search", 12., th.c("text.tertiary")))
                    .child(render_message(&Message::UiSearch {})),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .child(self.git_status_item(th, cx))
                    .child(self.monitor_status_item(th, cx)),
            )
    }

    pub(super) fn render_palette(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let p = self.palette.as_ref()?;
        let mut list = div()
            .id("palette-results")
            .track_scroll(&p.scroll)
            .overflow_y_scroll()
            .min_h_0()
            .flex()
            .flex_col()
            .px(px(6.))
            .py(px(4.));
        let mut last_group = None;
        for (row, &index) in p.visible.iter().enumerate() {
            let hit = &p.hits[index];
            let group = hit.target.group();
            let header = (last_group != Some(group)).then(|| {
                let message = [
                    Message::UiPaletteTabs {},
                    Message::UiPaletteSessions {},
                    Message::UiPaletteCommands {},
                    Message::UiFiles {},
                ];
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(11.))
                    .text_color(th.c("text.secondary"))
                    .child(render_message(&message[group]))
            });
            last_group = Some(group);
            let symbol = match hit.target {
                Target::Tab(_) => "square-terminal",
                Target::Profile(_) => "server",
                Target::Command(_) => "menu",
                Target::File(..) => "file",
            };
            list = list.child(
                div().children(header).child(
                    div()
                        .id(("result", row))
                        .h(px(32.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(8.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .when(row == p.selected, |s| s.bg(th.c("bg.rowHover")))
                        .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                            if let Some(palette) = &mut this.palette
                                && palette.selected != row
                            {
                                palette.selected = row;
                                cx.notify();
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, w, cx| {
                                cx.stop_propagation();
                                if let Some(p) = &mut this.palette {
                                    p.selected = row;
                                }
                                this.choose_result(w, cx);
                            }),
                        )
                        .child(icon(symbol, 14., th.c("text.secondary")))
                        .child(div().min_w_0().flex_1().truncate().child(hit.title.clone()))
                        .child(
                            div()
                                .max_w(px(160.))
                                .truncate()
                                .text_color(th.c("text.secondary"))
                                .child(hit.detail.clone()),
                        ),
                ),
            );
        }
        if p.visible.is_empty() {
            list = list.child(
                div()
                    .p(px(10.))
                    .text_color(th.c("text.secondary"))
                    .child(render_message(&Message::UiSearchEmpty {})),
            );
        }
        let panel = div()
            .id("palette-panel")
            .mt(px(HEIGHT))
            .w(px(
                (f32::from(window.viewport_size().width) - 16.).clamp(0., 480.)
            ))
            .max_h(px(
                (f32::from(window.viewport_size().height) - HEIGHT - 8.).clamp(0., 380.)
            ))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(8.))
            .shadow_lg()
            .border_1()
            .border_color(th.c("border.field"))
            .bg(th.c("bg.popover"))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().p(px(6.)).flex_none().child(p.input.clone()))
            .child(list);
        Some(
            div()
                .id("command-palette")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .items_start()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, w, cx| this.dismiss_palette(w, cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }
}
