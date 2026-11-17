//! 主窗口工具栏和命令搜索。结果保留标签和会话的 ID，打开时再交给原来的动作处理。
use super::shell::{ShellView, Theme};
use super::widgets::{
    icons::icon,
    text_input::{TextInput, TextInputEvent},
};
use gpui::{
    Context, Entity, KeyDownEvent, MouseButton, MouseDownEvent, ScrollHandle, Subscription, Window,
    div, point, prelude::*, px,
};
use kanso_gpui::app::launcher::{render_message, subtitle_text};
use kanso_gpui::app::menus::{app_menu, decode};
use kanso_gpui::app::shortcuts::AppCommand;
use kanso_gpui::widgets::menu::MenuItem;
use kanso_types::{ProfileId, SessionId, message::Message};

pub(super) const HEIGHT: f32 = 34.0;
const CAPTION_BUTTON_W: f32 = 44.0;

/// 让 GPUI 把这三个区域交给 Windows 处理，沿用系统的贴靠、最大化和关闭确认。
pub(super) fn window_buttons(th: Theme, window: &Window) -> impl IntoElement {
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

#[derive(Clone)]
enum Target {
    Tab(SessionId),
    Profile(ProfileId),
    Command(AppCommand),
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
    selected: usize,
    scroll: ScrollHandle,
}

impl Palette {
    fn filter(&mut self, query: &str) {
        let q = query.trim().to_lowercase();
        self.visible = self
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| {
                q.is_empty()
                    || hit.title.to_lowercase().contains(&q)
                    || hit.detail.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
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
        menu_hits(app_menu(self.app_menu_state()), &mut hits);
        let input = cx.new(|cx| {
            TextInput::new(window, cx)
                .search()
                .placeholder(Message::UiSearch {})
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
            input,
            _subscription: subscription,
            visible: (0..hits.len()).collect(),
            hits,
            selected: 0,
            scroll: ScrollHandle::new(),
        });
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
        }
    }

    fn palette_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn render_toolbar(
        &self,
        th: Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        let controls = if window.is_fullscreen() {
            0.0
        } else {
            3.0 * CAPTION_BUTTON_W
        };
        let search_w = (width - controls - 86.).clamp(0., 240.);
        let search_x = ((width - search_w) * 0.5)
            .min(width - controls - 40. - search_w)
            .max(40.);
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
                    .window_control_area(gpui::WindowControlArea::Drag),
            )
            .child(button("sidebar-toggle", "panel-left").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, w, cx| {
                    this.run_command(AppCommand::ToggleSidebar, w, cx)
                }),
            ))
            .child(
                div()
                    .id("toolbar-search")
                    .occlude()
                    .absolute()
                    .left(px(search_x))
                    .top(px(6.))
                    .w(px(search_w))
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
            .child(button("app-menu", "menu").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, w, cx| {
                    this.open_app_menu(point(ev.position.x, px(HEIGHT)), w, cx)
                }),
            ))
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
        for (row, &index) in p.visible.iter().enumerate() {
            let hit = &p.hits[index];
            let symbol = match hit.target {
                Target::Tab(_) => "square-terminal",
                Target::Profile(_) => "server",
                Target::Command(_) => "menu",
            };
            list = list.child(
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
                    .when(row == p.selected, |s| s.bg(th.c("bg.tabSelected")))
                    .hover(|s| s.bg(th.c("bg.rowHover")))
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
                .capture_key_down(cx.listener(Self::palette_key))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, w, cx| this.dismiss_palette(w, cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }
}
