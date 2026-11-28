//! 右键菜单（GPUI 视图）。
//!
//! 菜单模型、键盘 / 悬停导航与定位翻转在库里的 `kanso_gpui::widgets::menu`（三平台可测），
//! 本文件只做 GPUI 接线：全窗口遮罩（点菜单外关闭且不穿透）、按层画面板、按键翻译、窗口失焦关闭。
//!
//! 用法：宿主视图在右键事件里创建 `ContextMenu`，订阅 `MenuEvent`，把它作为最上层子元素渲染，
//! 收到事件后丢弃实体并把焦点还给自己：
//!
//! ```ignore
//! let menu = cx.new(|cx| ContextMenu::new(items, ev.position, window, cx));
//! cx.subscribe(&menu, |this, _m, ev: &MenuEvent, cx| { /* Activate(id) / Dismiss */ }).detach();
//! // render：root.children(self.menu.clone())
//! ```
//!
//! 宿主的根元素必须铺满窗口且位于窗口原点（遮罩按窗口坐标定位）。

use gpui::{
    App, BoxShadow, Context, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, Window, deferred, div, point, prelude::*, px, rgba,
};
use kanso_gpui::widgets::menu::{
    ARROW_W, BADGE_GAP, BADGE_W, CHECK_W, ITEM_H, LevelLayout, MenuBadge, MenuBadgeTone, MenuItem,
    MenuKey, MenuOutcome, MenuState, PAD_X, PAD_Y, SEP_H,
};
use kanso_gpui::widgets::style::{Appearance, Rgba8, color, number};

/// 菜单向宿主发出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    /// 执行了某项（携带 `id`）。宿主应丢弃菜单并处理。
    Activate(String),
    /// 菜单被关闭（Esc、点菜单外、窗口失焦）。宿主应丢弃菜单。
    Dismiss,
}

/// 右键菜单实体。
pub struct ContextMenu {
    state: MenuState,
    anchor: Point<Pixels>,
    focus: FocusHandle,
    /// 已经发出过终结事件，之后的输入一律忽略。
    done: bool,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<MenuEvent> for ContextMenu {}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn hsla(c: Rgba8) -> Hsla {
    rgba(c.to_u32()).into()
}

impl ContextMenu {
    /// 在 `anchor`（窗口坐标，通常是鼠标位置）弹出菜单，并接管键盘焦点。
    pub fn new(
        items: Vec<MenuItem>,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let subs = vec![
            // 窗口失去激活（切到别的程序）时关闭。
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.finish(MenuEvent::Dismiss, cx);
                }
            }),
        ];
        let this = ContextMenu {
            state: MenuState::new(items),
            anchor,
            focus,
            done: false,
            _subs: subs,
        };
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            let vp = window.viewport_size();
            let ls = this.layout(vp.width.into(), vp.height.into());
            crate::ui::log(&format!(
                "menu open anchor=({:.0},{:.0}) window=({:.0},{:.0}) rect0={}",
                f32::from(anchor.x),
                f32::from(anchor.y),
                f32::from(vp.width),
                f32::from(vp.height),
                fmt_rect(&ls[0])
            ));
        }
        this
    }

    fn layout(&self, w: f32, h: f32) -> Vec<LevelLayout> {
        self.state
            .layout((self.anchor.x.into(), self.anchor.y.into()), (w, h))
    }

    fn finish(&mut self, ev: MenuEvent, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        crate::ui::log(&match &ev {
            MenuEvent::Activate(id) => format!("menu activate id={id}"),
            MenuEvent::Dismiss => "menu dismiss".to_owned(),
        });
        cx.emit(ev);
        cx.notify();
    }

    fn apply(&mut self, outcome: MenuOutcome, cx: &mut Context<Self>) {
        match outcome {
            MenuOutcome::None => cx.notify(),
            MenuOutcome::Close => self.finish(MenuEvent::Dismiss, cx),
            MenuOutcome::Activate(id) => self.finish(MenuEvent::Activate(id), cx),
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        let m = &ev.keystroke.modifiers;
        let key = match ev.keystroke.key.as_str() {
            "up" => Some(MenuKey::Up),
            "down" => Some(MenuKey::Down),
            "left" => Some(MenuKey::Left),
            "right" => Some(MenuKey::Right),
            "enter" => Some(MenuKey::Enter),
            "space" => Some(MenuKey::Space),
            "escape" => Some(MenuKey::Escape),
            "home" => Some(MenuKey::Home),
            "end" => Some(MenuKey::End),
            _ if !m.control && !m.alt && !m.platform => {
                ev.keystroke.key_char.as_deref().and_then(|s| {
                    let mut it = s.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) if !c.is_control() => Some(MenuKey::Char(c)),
                        _ => None,
                    }
                })
            }
            _ => None,
        };
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ui::log(&format!("menu key {} -> {key:?}", ev.keystroke.unparse()));
        }
        // 菜单打开期间键盘归菜单，不再往下传。
        cx.stop_propagation();
        if let Some(key) = key {
            let out = self.state.key(key);
            self.apply(out, cx);
        }
    }

    fn hover(&mut self, level: usize, row: usize, cx: &mut Context<Self>) {
        if !self.done && self.state.hover(level, Some(row)) {
            cx.notify();
        }
    }

    fn click(&mut self, level: usize, row: usize, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        let out = self.state.click(level, row);
        self.apply(out, cx);
    }
}

fn fmt_rect(l: &LevelLayout) -> String {
    format!(
        "({:.0},{:.0},{:.0}x{:.0})",
        l.rect.x, l.rect.y, l.rect.w, l.rect.h
    )
}

/// 菜单用到的颜色。
struct Palette {
    appearance: Appearance,
    bg: Hsla,
    border: Hsla,
    text: Hsla,
    text_dim: Hsla,
    text_disabled: Hsla,
    sep: Hsla,
    hl_bg: Hsla,
    hl_text: Hsla,
    badge_local_fg: Hsla,
    badge_local_bg: Hsla,
    badge_ssh_fg: Hsla,
    badge_ssh_bg: Hsla,
    badge_sftp_fg: Hsla,
    badge_sftp_bg: Hsla,
}

impl Palette {
    fn new(a: Appearance) -> Palette {
        let c = |n: &str| hsla(color(n, a));
        let ca = |n: &str, alpha: u8| hsla(color(n, a).with_alpha(alpha));
        Palette {
            appearance: a,
            bg: c("bg.popover"),
            border: c("border.strong"),
            text: c("text.primary"),
            text_dim: c("text.secondary"),
            text_disabled: c("text.tertiary"),
            sep: c("border.subtle"),
            hl_bg: c("accent"),
            hl_text: c("text.onAccent"),
            badge_local_fg: c("status.unreadDone"),
            badge_local_bg: ca("status.unreadDone", 0x26),
            badge_ssh_fg: c("accent"),
            badge_ssh_bg: ca("accent", 0x26),
            badge_sftp_fg: c("status.warning"),
            badge_sftp_bg: ca("status.warning", 0x26),
        }
    }
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = crate::ui::display::window_appearance(window);
        let pal = Palette::new(appearance);
        let vp = window.viewport_size();
        let layouts = self.layout(vp.width.into(), vp.height.into());
        let font = number("font", "ui.body");
        let radius = number("radius", "md");

        let mut overlay = div()
            .id("kanso-context-menu")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            // 点菜单外（任意键）：关闭，事件到此为止不穿透。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.finish(MenuEvent::Dismiss, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.finish(MenuEvent::Dismiss, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.finish(MenuEvent::Dismiss, cx);
                    cx.stop_propagation();
                }),
            );

        for (level, lay) in layouts.iter().enumerate() {
            let items = self.state.items_at(level);
            let hl = self.state.highlighted(level);
            let mut panel = div()
                .id(("menu-level", level))
                .absolute()
                .left(px(lay.rect.x))
                .top(px(lay.rect.y))
                .w(px(lay.rect.w))
                .h(px(lay.rect.h))
                .px(px(PAD_X))
                .py(px(PAD_Y))
                .flex()
                .flex_col()
                .bg(pal.bg)
                .border_1()
                .border_color(pal.border)
                .rounded(px(radius + 2.0))
                .text_size(px(font))
                .text_color(pal.text)
                .shadow(vec![BoxShadow {
                    color: gpui::black().opacity(0.28),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(14.),
                    spread_radius: px(0.),
                }])
                .occlude()
                // 点在面板内（含内边距）不关闭。
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation());
            for (row, item) in items.iter().enumerate() {
                panel = panel.child(render_row(level, row, item, hl == Some(row), &pal, cx));
            }
            overlay = overlay.child(panel);
        }
        deferred(overlay).with_priority(100)
    }
}

fn render_row(
    level: usize,
    row: usize,
    item: &MenuItem,
    highlighted: bool,
    pal: &Palette,
    cx: &mut Context<ContextMenu>,
) -> gpui::AnyElement {
    let id = ("menu-row", level * 1000 + row);
    match item {
        MenuItem::Separator => div()
            .id(id)
            .h(px(SEP_H))
            .flex()
            .items_center()
            .px(px(6.))
            .child(div().h(px(1.)).w_full().bg(pal.sep))
            .into_any_element(),
        MenuItem::Action {
            label,
            badge,
            shortcut,
            enabled,
            checked,
            ..
        } => row_base(id, level, row, *enabled, highlighted, pal, cx)
            .child(check_cell(*checked))
            .when_some(badge.as_ref(), |d, b| {
                d.child(badge_pill(b, highlighted, pal))
            })
            .child(div().flex_1().whitespace_nowrap().child(label.clone()))
            .when_some(shortcut.clone(), |d, s| {
                d.child(
                    div()
                        .flex_none()
                        .text_color(dim(*enabled, highlighted, pal))
                        .child(s),
                )
            })
            .child(div().w(px(ARROW_W)).flex_none())
            .into_any_element(),
        MenuItem::Submenu { label, enabled, .. } => {
            row_base(id, level, row, *enabled, highlighted, pal, cx)
                .child(check_cell(false))
                .child(div().flex_1().whitespace_nowrap().child(label.clone()))
                .child(
                    div()
                        .w(px(ARROW_W))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_color(dim(*enabled, highlighted, pal))
                        .child("›"),
                )
                .into_any_element()
        }
    }
}

fn dim(enabled: bool, highlighted: bool, pal: &Palette) -> Hsla {
    if !enabled {
        pal.text_disabled
    } else if highlighted {
        pal.hl_text.opacity(0.85)
    } else {
        pal.text_dim
    }
}

fn badge_pill(badge: &MenuBadge, highlighted: bool, pal: &Palette) -> gpui::Div {
    let (fg, bg) = if highlighted {
        (pal.hl_text, pal.hl_text.opacity(0.18))
    } else {
        match badge.tone {
            MenuBadgeTone::Local => (pal.badge_local_fg, pal.badge_local_bg),
            MenuBadgeTone::Ssh => (pal.badge_ssh_fg, pal.badge_ssh_bg),
            MenuBadgeTone::Sftp => (pal.badge_sftp_fg, pal.badge_sftp_bg),
            MenuBadgeTone::Custom(rgb24) => {
                // 亮色背景下适度压暗文字色保证对比度，深色背景保持原色。
                let fg_rgb = match pal.appearance {
                    Appearance::Light => {
                        let r = (((rgb24 >> 16) & 0xFF) * 3) / 4;
                        let g = (((rgb24 >> 8) & 0xFF) * 3) / 4;
                        let b = ((rgb24 & 0xFF) * 3) / 4;
                        (r << 16) | (g << 8) | b
                    }
                    Appearance::Dark => rgb24,
                };
                (
                    rgba((fg_rgb << 8) | 0xFF).into(),
                    rgba((rgb24 << 8) | 0x26).into(),
                )
            }
        }
    };
    div()
        .w(px(BADGE_W))
        .h(px(18.0))
        .mr(px(BADGE_GAP))
        .flex_none()
        .rounded(px(4.0))
        .bg(bg)
        .text_color(fg)
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .flex()
        .items_center()
        .justify_center()
        .child(badge.text.clone())
}

fn check_cell(checked: bool) -> gpui::Div {
    div()
        .w(px(CHECK_W))
        .flex_none()
        .flex()
        .items_center()
        .pl(px(4.))
        .child(if checked { "✓" } else { "" })
}

/// 可交互行的公共外壳：高度、配色、悬停与点击。
fn row_base(
    id: (&'static str, usize),
    level: usize,
    row: usize,
    enabled: bool,
    highlighted: bool,
    pal: &Palette,
    cx: &mut Context<ContextMenu>,
) -> gpui::Stateful<gpui::Div> {
    let (fg, bg) = if !enabled {
        (pal.text_disabled, None)
    } else if highlighted {
        (pal.hl_text, Some(pal.hl_bg))
    } else {
        (pal.text, None)
    };
    div()
        .id(id)
        .h(px(ITEM_H))
        .flex()
        .items_center()
        .px(px(2.))
        .rounded(px(4.))
        .text_color(fg)
        .when_some(bg, |d, c| d.bg(c))
        .on_mouse_move(cx.listener(move |this, _, _, cx| this.hover(level, row, cx)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                this.click(level, row, cx);
                cx.stop_propagation();
            }),
        )
}
