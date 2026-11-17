//! 终端查找栏（terminal.md §11）：浮在终端右上角的小面板，对照 mac 版 `Find/FindBarView`。
//!
//! 栏只负责控件与外观：输入框、区分大小写 / 正则开关、状态文字、上一个 / 下一个 / 关闭。
//! 搜索本身与状态（`kanso_gpui::app::find::FindState`）在 `TerminalView` 里，栏通过事件向它汇报用户操作，
//! 并由它回填开关与状态文字。

use gpui::{
    App, BoxShadow, Context, Entity, EventEmitter, Hsla, IntoElement, MouseButton, Render,
    SharedString, Stateful, Subscription, Window, div, point, prelude::*, px,
};
use kanso_gpui::widgets::style::number;

use super::display;
use super::shell::Theme;
use super::widgets::icons::icon;
use super::widgets::text_input::{TextInput, TextInputEvent};

/// 栏固定宽度（与 mac 版一致）。
pub(crate) const BAR_WIDTH: f32 = 360.0;

/// 栏向终端视图发出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FindBarEvent {
    /// 搜索文字变化（携带新文字）。
    Query(String),
    /// 切换“区分大小写”。
    ToggleCase,
    /// 切换“正则表达式”。
    ToggleRegex,
    /// 下一个（Enter）。
    Next,
    /// 上一个（Shift+Enter）。
    Prev,
    /// 关闭（Esc / 关闭按钮）。
    Close,
}

pub(crate) struct FindBar {
    input: Entity<TextInput>,
    case_sensitive: bool,
    regex: bool,
    status: SharedString,
    error: bool,
    _sub: Subscription,
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(window, cx).search().placeholder("查找"));
        let sub = cx.subscribe(&input, |_this, _input, ev: &TextInputEvent, cx| match ev {
            TextInputEvent::Change(t) => cx.emit(FindBarEvent::Query(t.to_string())),
            TextInputEvent::Submit(_) => cx.emit(FindBarEvent::Next),
            TextInputEvent::Escape => cx.emit(FindBarEvent::Close),
            _ => {}
        });
        FindBar {
            input,
            case_sensitive: false,
            regex: false,
            status: SharedString::default(),
            error: false,
            _sub: sub,
        }
    }

    /// 把键盘焦点交给输入框。
    pub(crate) fn focus_input(&self, window: &mut Window, cx: &App) {
        self.input.read(cx).focus(window);
    }

    /// 整体设置搜索文字（打开栏时恢复上次的文字）。
    pub(crate) fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_text(text, cx));
    }

    /// 由终端视图回填开关与状态文字。
    pub(crate) fn set_state(
        &mut self,
        case_sensitive: bool,
        regex: bool,
        status: String,
        error: bool,
        cx: &mut Context<Self>,
    ) {
        self.case_sensitive = case_sensitive;
        self.regex = regex;
        self.status = status.into();
        self.error = error;
        cx.notify();
    }
}

/// 栏上的小按钮底板：固定 24 像素高，悬停变底色。
fn button(id: &'static str, th: Theme, active: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(24.0))
        .min_w(px(24.0))
        .px(px(4.0))
        .rounded(px(number("radius", "md")))
        .cursor_pointer()
        .text_size(px(number("font", "ui.caption")))
        .text_color(if active {
            th.c("accent")
        } else {
            th.c("text.secondary")
        })
        .when(active, |d| d.bg(th.ca("accent", 38)))
        .hover(|d| d.bg(th.c("bg.rowHover")))
}

impl Render for FindBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Theme {
            appearance: display::window_appearance(window),
        };
        let secondary: Hsla = th.c("text.secondary");
        let status_color = if self.error {
            th.c("status.error")
        } else {
            secondary
        };

        div()
            .id("find-bar")
            .w(px(BAR_WIDTH))
            .flex()
            .items_center()
            .gap(px(4.0))
            .p(px(8.0))
            .rounded(px(number("radius", "lg")))
            .bg(th.c("bg.popover"))
            .border_1()
            .border_color(th.c("border.strong"))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.18),
                offset: point(px(0.0), px(2.0)),
                blur_radius: px(6.0),
                spread_radius: px(0.0),
            }])
            // Shift+Enter 是“上一个”：输入框自己会吞掉 Enter，所以在捕获阶段先看。
            .capture_key_down(cx.listener(|_this, ev: &gpui::KeyDownEvent, _window, cx| {
                let m = &ev.keystroke.modifiers;
                if ev.keystroke.key == "enter" && m.shift && !m.control && !m.alt && !m.platform {
                    cx.emit(FindBarEvent::Prev);
                    cx.stop_propagation();
                }
            }))
            // 点击栏不穿透到终端（否则会开始选择文字）。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(div().flex_1().min_w_0().child(self.input.clone()))
            .child(
                button("find-case", th, self.case_sensitive)
                    .child("Aa")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::ToggleCase))),
            )
            .child(
                button("find-regex", th, self.regex)
                    .child(".*")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::ToggleRegex))),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(56.0))
                    .text_right()
                    .whitespace_nowrap()
                    .text_size(px(number("font", "ui.caption")))
                    .text_color(status_color)
                    .child(self.status.clone()),
            )
            .child(
                button("find-prev", th, false)
                    .child(icon("chevron-up", 14.0, secondary))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Prev))),
            )
            .child(
                button("find-next", th, false)
                    .child(icon("chevron-down", 14.0, secondary))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Next))),
            )
            .child(
                button("find-close", th, false)
                    .child(icon("x", 14.0, secondary))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FindBarEvent::Close))),
            )
    }
}
