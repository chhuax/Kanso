//! `--demo-input`：输入框演示窗口，用来自检和截图。
//!
//! 窗口里放几种输入框（普通、占位、密码、禁用、中文初始内容、只读、限长、长文本），
//! 每个输入框的事件（内容变化、回车、Esc、焦点）都写进日志，自动化脚本据此断言。

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, SharedString, Subscription,
    TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px,
    rgba, size,
};
use kanso_gpui::widgets::style::{Appearance, color};

use super::log;
use super::widgets::text_input::{self, TextInput, TextInputEvent};

struct DemoView {
    rows: Vec<(&'static str, Entity<TextInput>)>,
    last_event: SharedString,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

type Build = fn(TextInput) -> TextInput;

impl DemoView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let specs: Vec<(&'static str, Build)> = vec![
            ("普通", |i| i),
            ("占位文字", |i| i.placeholder("请输入服务器地址")),
            ("密码", |i| i.masked().with_text("hunter2")),
            ("禁用", |i| i.disabled().with_text("不可编辑的内容")),
            ("中文初始内容", |i| {
                i.with_text("你好，Kanso 终端 👨‍👩‍👧 emoji")
            }),
            ("只读", |i| {
                i.read_only().with_text("read-only, 可选择可复制")
            }),
            ("最多 8 个字符", |i| {
                i.max_len(8).placeholder("最多 8 个字符")
            }),
            ("长文本（水平滚动）", |i| {
                i.with_text(
                    "The quick brown fox jumps over the lazy dog, 然后又跳过了一只很懒的狗, 1234567890",
                )
            }),
        ];
        let mut rows = Vec::new();
        let mut subs = Vec::new();
        for (label, build) in specs {
            let input = cx.new(|cx| build(TextInput::new(window, cx)));
            subs.push(
                cx.subscribe(&input, move |this, _input, ev: &TextInputEvent, cx| {
                    let line = match ev {
                        TextInputEvent::Change(t) => format!("Change {t:?}"),
                        TextInputEvent::Submit(t) => format!("Submit {t:?}"),
                        TextInputEvent::Escape => "Escape".to_owned(),
                        TextInputEvent::Focus => "Focus".to_owned(),
                        TextInputEvent::Blur => "Blur".to_owned(),
                    };
                    log(&format!("input[{label}] {line}"));
                    this.last_event = format!("[{label}] {line}").into();
                    cx.notify();
                }),
            );
            rows.push((label, input));
        }
        let first = rows[0].1.clone();
        first.update(cx, |i, _| i.focus(window));
        DemoView {
            rows,
            last_event: "（无事件）".into(),
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }
}

impl Focusable for DemoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn c(name: &str, a: Appearance) -> gpui::Hsla {
    rgba(color(name, a).to_u32()).into()
}

impl Render for DemoView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let a = match window.appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            _ => Appearance::Dark,
        };
        let mut col = div()
            .size_full()
            .bg(c("bg.window", a))
            .text_color(c("text.primary", a))
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(15.))
                    .child("Kanso 输入框演示（Tab 切换，回车提交，Esc）"),
            );
        for (label, input) in &self.rows {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c("text.secondary", a))
                            .child(*label),
                    )
                    .child(input.clone()),
            );
        }
        col.child(
            div()
                .mt(px(4.))
                .text_size(px(11.))
                .text_color(c("text.secondary", a))
                .child(format!("最近事件：{}", self.last_event)),
        )
    }
}

/// 打开演示窗口，返回进程退出码。
pub(crate) fn run() -> i32 {
    Application::new().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(560.), px(720.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kanso 输入框演示".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DemoView::new(window, cx)),
        );
        match opened {
            Ok(_) => {
                log("demo-input：窗口已创建");
                cx.activate(true);
            }
            Err(e) => {
                log(&format!("创建窗口失败：{e}"));
                super::EXIT_CODE.store(1, std::sync::atomic::Ordering::SeqCst);
                cx.quit();
            }
        }
    });
    super::EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst)
}
