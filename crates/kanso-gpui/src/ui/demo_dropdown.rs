//! `--demo-dropdown`：下拉选择框演示窗口，用来自检和截图。
//!
//! 四个下拉框：短列表（编码，带分组与分隔线）、300 项可过滤长列表（模拟字体名，含中文名）、
//! 禁用的、靠窗口底部的（测试向上翻转）。选中值写日志（`--log-input` 下），自动化脚本据此断言。

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, KeyDownEvent, SharedString,
    Subscription, TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions, div,
    prelude::*, px, rgba, size,
};
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::{Appearance, color};

use super::log;
use super::widgets::dropdown::{Dropdown, DropdownEvent};

struct DemoView {
    rows: Vec<(&'static str, Entity<Dropdown>)>,
    last_event: SharedString,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

fn encodings() -> Vec<Item> {
    vec![
        Item::header("Unicode"),
        Item::option("UTF-8"),
        Item::option("UTF-16 LE"),
        Item::option("UTF-16 BE"),
        Item::Separator,
        Item::header("中文"),
        Item::option("GBK"),
        Item::option("GB18030"),
        Item::option("Big5"),
        Item::Separator,
        Item::header("其他"),
        Item::option("Shift_JIS"),
        Item::option("EUC-KR"),
        Item::option("ISO-8859-1"),
        Item::option("Windows-1252"),
    ]
}

/// 300 个模拟字体名：真实常见字体 + 中文名 + 编号填充。
fn fonts() -> Vec<Item> {
    let base = [
        "Cascadia Code",
        "Cascadia Mono",
        "Consolas",
        "Courier New",
        "JetBrains Mono",
        "Fira Code",
        "Source Code Pro",
        "Segoe UI",
        "Arial",
        "Calibri",
        "微软雅黑",
        "宋体",
        "黑体",
        "楷体",
        "仿宋",
        "思源黑体",
        "思源宋体",
        "霞鹜文楷",
        "苹方-简",
        "等线",
        "Noto Sans CJK SC",
        "Sarasa Mono SC",
    ];
    let mut v: Vec<String> = base.iter().map(|s| (*s).to_owned()).collect();
    let mut i = 1;
    while v.len() < 300 {
        v.push(match i % 3 {
            0 => format!("Mono Test {i:03}"),
            1 => format!("测试字体 {i:03}"),
            _ => format!("Sample Sans {i:03}"),
        });
        i += 1;
    }
    v.sort_by_key(|s| s.to_lowercase());
    v.into_iter().map(Item::Option).collect()
}

impl DemoView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let specs: Vec<(&'static str, Entity<Dropdown>)> = vec![
            (
                "编码（短列表，分组）",
                cx.new(|cx| Dropdown::new(window, cx).items(encodings()).selected(0)),
            ),
            (
                "字体（300 项，可过滤）",
                cx.new(|cx| {
                    Dropdown::new(window, cx)
                        .items(fonts())
                        .filterable("搜索字体")
                        .selected(0)
                }),
            ),
            (
                "禁用",
                cx.new(|cx| {
                    Dropdown::new(window, cx)
                        .items(encodings())
                        .selected(0)
                        .disabled()
                }),
            ),
            (
                "靠窗口底部（向上展开）",
                cx.new(|cx| Dropdown::new(window, cx).items(encodings()).selected(4)),
            ),
        ];
        let mut subs = Vec::new();
        for (label, dd) in &specs {
            let label = *label;
            subs.push(cx.subscribe(dd, move |this, dd, ev: &DropdownEvent, cx| {
                let line = match ev {
                    DropdownEvent::Change(i) => {
                        let name = dd.read(cx).selected_label().unwrap_or("").to_owned();
                        format!("Change {i} {name:?}")
                    }
                    DropdownEvent::Open => "Open".to_owned(),
                    DropdownEvent::Close => "Close".to_owned(),
                };
                log(&format!("dropdown[{label}] {line}"));
                this.last_event = format!("[{label}] {line}").into();
                cx.notify();
            }));
        }
        let first = specs[0].1.clone();
        first.update(cx, |d, _| d.focus(window));
        DemoView {
            rows: specs,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let a = match window.appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            _ => Appearance::Dark,
        };
        let labeled = |i: usize| {
            let (label, dd) = &self.rows[i];
            div()
                .w(px(260.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c("text.secondary", a))
                        .child(*label),
                )
                .child(dd.clone())
        };
        div()
            .size_full()
            .bg(c("bg.window", a))
            .text_color(c("text.primary", a))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                // 下拉框把 Tab 冒泡上来，由这里的焦点链统一处理（顺序 = 行顺序，跳过禁用项）。
                let handles: Vec<FocusHandle> = this
                    .rows
                    .iter()
                    .map(|(_, dd)| dd.read(cx).focus_handle(cx))
                    .collect();
                if super::widgets::focus_chain::handle_tab(&handles, ev, window) {
                    cx.stop_propagation();
                }
            }))
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .text_size(px(15.))
                    .child("Kanso 下拉框演示（Tab 切换，Space / Enter / Alt+↓ 展开）"),
            )
            .child(labeled(0))
            .child(labeled(1))
            .child(labeled(2))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(c("text.secondary", a))
                    .child(format!("最近事件：{}", self.last_event)),
            )
            .child(div().flex_1())
            .child(labeled(3))
    }
}

/// 打开演示窗口，返回进程退出码。
pub(crate) fn run() -> i32 {
    Application::new().run(|cx: &mut App| {
        super::widgets::text_input::bind_keys(cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(560.), px(560.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kanso 下拉框演示".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DemoView::new(window, cx)),
        );
        match opened {
            Ok(_) => {
                log("demo-dropdown：窗口已创建");
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
