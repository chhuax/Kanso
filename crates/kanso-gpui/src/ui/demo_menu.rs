//! `--demo-menu`：右键菜单演示窗口，用来自检和截图。
//!
//! 在窗口任意位置右键弹出菜单（带分隔线、禁用项、勾选项、快捷键提示与子菜单）；
//! 四角各有一块提示区，在那里右键可以看到向内翻转。选中项写进日志（`menu activate id=...`）。

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    SharedString, TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions, div,
    prelude::*, px, rgba, size,
};
use kanso_gpui::widgets::menu::MenuItem;
use kanso_gpui::widgets::style::{Appearance, color};

use super::log;
use super::widgets::menu::{ContextMenu, MenuEvent};

struct DemoView {
    menu: Option<Entity<ContextMenu>>,
    wrap: bool,
    last: SharedString,
    focus: FocusHandle,
}

fn items(wrap: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::action("copy", "复制").shortcut("Ctrl+Shift+C"),
        MenuItem::action("paste", "粘贴").shortcut("Ctrl+Shift+V"),
        MenuItem::action("select-all", "全选").shortcut("Ctrl+Shift+A"),
        MenuItem::separator(),
        MenuItem::action("clear", "清屏").disabled(),
        MenuItem::action("wrap", "自动换行").checked(wrap),
        MenuItem::separator(),
        MenuItem::submenu(
            "分屏",
            vec![
                MenuItem::action("split-right", "向右分屏").shortcut("Ctrl+D"),
                MenuItem::action("split-down", "向下分屏").shortcut("Ctrl+Shift+D"),
                MenuItem::separator(),
                MenuItem::action("split-close", "关闭分屏").disabled(),
            ],
        ),
        MenuItem::submenu(
            "Tab 操作",
            vec![MenuItem::action("tab-rename", "Rename Tab")],
        ),
    ]
}

impl DemoView {
    fn open(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let wrap = self.wrap;
        let menu = cx.new(|cx| ContextMenu::new(items(wrap), ev.position, window, cx));
        cx.subscribe_in(&menu, window, |this, _m, ev: &MenuEvent, window, cx| {
            match ev {
                MenuEvent::Activate(id) => {
                    if id == "wrap" {
                        this.wrap = !this.wrap;
                    }
                    this.last = format!("选中 {id}").into();
                }
                MenuEvent::Dismiss => this.last = "已关闭".into(),
            }
            this.menu = None;
            window.focus(&this.focus);
            cx.notify();
        })
        .detach();
        self.menu = Some(menu);
        cx.notify();
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
        let corner = |label: &'static str| {
            div()
                .absolute()
                .w(px(150.))
                .h(px(60.))
                .border_1()
                .border_color(c("border.subtle", a))
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(c("text.secondary", a))
                .child(label)
        };
        div()
            .size_full()
            .relative()
            .bg(c("bg.window", a))
            .text_color(c("text.primary", a))
            .track_focus(&self.focus)
            .on_mouse_down(MouseButton::Right, cx.listener(Self::open))
            .child(
                div()
                    .absolute()
                    .top(px(90.))
                    .left(px(20.))
                    .text_size(px(13.))
                    .child("在窗口任意位置右键弹出菜单；四角区域用来测试翻转"),
            )
            .child(
                div()
                    .absolute()
                    .top(px(116.))
                    .left(px(20.))
                    .text_size(px(11.))
                    .text_color(c("text.secondary", a))
                    .child(format!(
                        "最近：{}（自动换行：{}）",
                        self.last,
                        if self.wrap { "开" } else { "关" }
                    )),
            )
            .child(corner("左上").top(px(8.)).left(px(8.)))
            .child(corner("右上").top(px(8.)).right(px(8.)))
            .child(corner("左下").bottom(px(8.)).left(px(8.)))
            .child(corner("右下").bottom(px(8.)).right(px(8.)))
            .children(self.menu.clone())
    }
}

/// 打开演示窗口，返回进程退出码。
pub(crate) fn run() -> i32 {
    Application::new().run(|cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(640.), px(480.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kanso 右键菜单演示".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| DemoView {
                    menu: None,
                    wrap: true,
                    last: "（无）".into(),
                    focus: cx.focus_handle(),
                });
                window.focus(&view.focus_handle(cx));
                view
            },
        );
        match opened {
            Ok(_) => {
                log("demo-menu：窗口已创建");
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
