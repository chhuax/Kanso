//! `--demo-dialog`：模态对话框演示窗口，用来自检和截图。
//!
//! 三个按钮分别打开 主机密钥确认 / 口令输入 / 危险删除确认；
//! 对话框结果（按了哪个按钮、各输入框内容，密码只记长度）在 `--log-input` 下写进日志，自动化脚本据此断言。
//! 演示按钮可用 Tab 聚焦、回车 / 空格打开，用来验证“关闭后焦点还给打开前的元素”。

use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, Hsla, KeyDownEvent,
    SharedString, Subscription, TitlebarOptions, Window, WindowAppearance, WindowBounds,
    WindowOptions, div, prelude::*, px, rgba, size,
};
use kanso_gpui::widgets::dialog::{ButtonSpec, DialogSpec, InputSpec};
use kanso_gpui::widgets::style::{Appearance, color};

use super::log;
use super::widgets::dialog::{Dialog, DialogEvent};
use super::widgets::text_input;

/// 三种演示对话框。
#[derive(Clone, Copy)]
enum Kind {
    HostKey,
    Passphrase,
    Delete,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::HostKey, Kind::Passphrase, Kind::Delete];

    fn label(self) -> &'static str {
        match self {
            Kind::HostKey => "主机密钥确认",
            Kind::Passphrase => "口令输入",
            Kind::Delete => "危险删除确认",
        }
    }

    fn spec(self) -> DialogSpec {
        match self {
            Kind::HostKey => DialogSpec::new("未知主机密钥")
                .text("无法确认主机 example.com:22 的真实性。\n该主机的 ssh-ed25519 密钥指纹为：")
                .code("SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s")
                .text("确定要继续连接吗？接受后密钥会写入 known_hosts。")
                .button(ButtonSpec::secondary("reject", "拒绝").cancel())
                .button(ButtonSpec::primary("accept", "信任并继续")),
            Kind::Passphrase => DialogSpec::new("需要私钥口令")
                .text("私钥 C:\\Users\\me\\.ssh\\id_ed25519 已加密，请输入口令。")
                .input(InputSpec::secure("passphrase", "口令").placeholder("私钥口令"))
                .input(InputSpec::new("note", "备注（可选）"))
                .button(ButtonSpec::secondary("cancel", "取消").cancel())
                .button(ButtonSpec::primary("ok", "确定")),
            Kind::Delete => DialogSpec::new("删除 3 个文件？")
                .text("将永久删除以下内容，此操作无法撤销：")
                .code("/var/log/app/old-1.log\n/var/log/app/old-2.log\n/var/log/app/old-3.log")
                .button(
                    ButtonSpec::secondary("cancel", "取消")
                        .cancel()
                        .default_button(),
                )
                .button(ButtonSpec::danger("delete", "删除")),
        }
    }
}

struct DemoView {
    openers: Vec<FocusHandle>,
    dialog: Option<(Kind, Entity<Dialog>)>,
    last_result: SharedString,
    focus: FocusHandle,
    _sub: Option<Subscription>,
}

impl DemoView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let openers: Vec<FocusHandle> = Kind::ALL
            .iter()
            .map(|_| cx.focus_handle().tab_stop(true))
            .collect();
        window.focus(&openers[0]);
        DemoView {
            openers,
            dialog: None,
            last_result: "（尚无结果）".into(),
            focus: cx.focus_handle(),
            _sub: None,
        }
    }

    fn open(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let spec = kind.spec();
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self._sub = Some(
            cx.subscribe(&dialog, move |this, _d, ev: &DialogEvent, cx| {
                let DialogEvent::Result(r) = ev;
                let line = r.log_line();
                if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
                    log(&format!("demo-dialog[{}] 结果 {line}", kind.label()));
                }
                this.last_result = format!("{}：{line}", kind.label()).into();
                this.dialog = None;
                this._sub = None;
                cx.notify();
            }),
        );
        self.dialog = Some((kind, dialog));
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        if matches!(ev.keystroke.key.as_str(), "enter" | "space")
            && let Some(i) = self.openers.iter().position(|h| h.is_focused(window))
        {
            self.open(Kind::ALL[i], window, cx);
        }
    }
}

impl Focusable for DemoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn c(name: &str, a: Appearance) -> Hsla {
    rgba(color(name, a).to_u32()).into()
}

impl Render for DemoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let a = match window.appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            _ => Appearance::Dark,
        };
        let mut row = div().flex().flex_row().gap(px(12.));
        for (i, kind) in Kind::ALL.iter().copied().enumerate() {
            let focused = self.openers[i].is_focused(window);
            row = row.child(
                div()
                    .id(("demo-open", i))
                    .track_focus(&self.openers[i])
                    .px(px(14.))
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if focused {
                        c("accent", a)
                    } else {
                        c("border.strong", a)
                    })
                    .bg(c("bg.buttonSecondary", a))
                    .text_size(px(13.))
                    .on_click(cx.listener(move |this, _, window, cx| this.open(kind, window, cx)))
                    .child(kind.label()),
            );
        }
        div()
            .relative()
            .track_focus(&self.focus)
            .size_full()
            .bg(c("bg.window", a))
            .text_color(c("text.primary", a))
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .size_full()
                    .p(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .child("Kanso 对话框演示（Tab 选按钮，回车 / 空格打开）"),
                    )
                    .child(row)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c("text.secondary", a))
                            .child(format!("最近结果：{}", self.last_result)),
                    ),
            )
            .children(self.dialog.as_ref().map(|(_, d)| d.clone()))
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
        let bounds = Bounds::centered(None, size(px(640.), px(420.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kanso 对话框演示".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| DemoView::new(window, cx)),
        );
        match opened {
            Ok(_) => {
                log("demo-dialog：窗口已创建");
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
