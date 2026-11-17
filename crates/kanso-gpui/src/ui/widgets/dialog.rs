//! 窗口内模态对话框（GPUI 视图）。
//!
//! 规格、焦点环与结果组装在库里的 `kanso_gpui::widgets::dialog`（三平台可测）；
//! 本文件做接线：遮罩 + 居中卡片、输入框（复用 `TextInput`）、按钮行、键盘（Enter / Esc / Tab 焦点陷阱）、
//! 打开时聚焦、关闭后把焦点还给打开前的元素。
//!
//! 用法：宿主窗口持有 `Option<Entity<Dialog>>`，把它作为根容器的最后一个子元素渲染（会铺满父容器）；
//! 订阅 [`DialogEvent::Result`]，收到后把它置为 `None`。
//!
//! 键盘：
//! - Tab / Shift-Tab 只在对话框内循环（输入框 → 按钮 → 回到第一个）；输入法组字时 Tab 与回车归输入法。
//! - 回车：焦点在按钮上触发该按钮，否则触发默认按钮；没有默认按钮（如只有危险按钮）时回车无效。
//! - Esc：触发取消按钮（没有则结果的 `button` 为 `None`），不带输入内容。取消组字的那个 Esc 不算。
//! - 点遮罩默认不关闭（与 mac sheet 一致），规格 `dismiss_on_scrim` 允许时按取消处理。

use gpui::{
    App, BoxShadow, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    KeyDownEvent, MouseButton, Subscription, Window, div, point, prelude::*, px, rgba,
};
use kanso_gpui::widgets::dialog::{
    BodyBlock, ButtonRole, DialogResult, DialogSpec, FocusRing, Slot, Trigger, enter_target,
    result_for_button, result_for_cancel,
};
use kanso_gpui::widgets::style::{Appearance, color};

use super::text_input::{TextInput, TextInputEvent};

/// 对话框向宿主发出的事件。
#[derive(Clone, Debug)]
pub enum DialogEvent {
    /// 用户作出了选择（焦点已还给打开前的元素）。
    Result(DialogResult),
}

/// 模态对话框。
pub struct Dialog {
    spec: DialogSpec,
    ring: FocusRing,
    inputs: Vec<Entity<TextInput>>,
    buttons: Vec<FocusHandle>,
    root: FocusHandle,
    prev_focus: Option<FocusHandle>,
    done: bool,
    appearance: Appearance,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DialogEvent> for Dialog {}

impl Focusable for Dialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root.clone()
    }
}

fn hsla(name: &str, a: Appearance) -> Hsla {
    rgba(color(name, a).to_u32()).into()
}

fn log_enabled() -> bool {
    crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed)
}

impl Dialog {
    /// 创建并聚焦（第一个输入框；没有则默认按钮）。必须在窗口上下文里创建。
    pub fn new(spec: DialogSpec, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prev_focus = window.focused(cx);
        let ring = FocusRing::new(&spec);
        let mut subs = Vec::new();
        let mut inputs = Vec::new();
        for inp in &spec.inputs {
            let (placeholder, secure, initial) =
                (inp.placeholder.clone(), inp.secure, inp.initial.clone());
            let input = cx.new(|cx| {
                let mut t = TextInput::new(window, cx).placeholder(placeholder);
                if secure {
                    t = t.masked();
                }
                t.with_text(&initial)
            });
            subs.push(cx.subscribe_in(
                &input,
                window,
                |this, _input, ev: &TextInputEvent, window, cx| match ev {
                    // 输入法组字期间输入框自己不发 Submit，这里不需要再判断组字。
                    TextInputEvent::Submit(_) => this.activate_enter(window, cx),
                    TextInputEvent::Escape => this.cancel(Trigger::Escape, window, cx),
                    _ => {}
                },
            ));
            inputs.push(input);
        }
        let buttons = spec.buttons.iter().map(|_| cx.focus_handle()).collect();
        let dialog = Dialog {
            spec,
            ring,
            inputs,
            buttons,
            root: cx.focus_handle(),
            prev_focus,
            done: false,
            appearance: Appearance::Dark,
            _subs: subs,
        };
        if log_enabled() {
            crate::ui::log(&format!(
                "dialog[{}] opened inputs={} buttons={} default={:?}",
                dialog.spec.title,
                dialog.spec.inputs.len(),
                dialog.spec.buttons.len(),
                dialog.spec.default_button()
            ));
        }
        let initial = dialog.ring.initial(&dialog.spec);
        dialog.focus_slot(initial, window, cx);
        // 首帧渲染之后再聚焦一次，保证焦点节点已进入调度树。
        cx.defer_in(window, move |this, window, cx| {
            if !this.done {
                this.focus_slot(initial, window, cx);
            }
        });
        dialog
    }

    fn focus_slot(&self, slot: Option<Slot>, window: &mut Window, cx: &mut App) {
        match slot {
            Some(Slot::Input(i)) => {
                if let Some(input) = self.inputs.get(i) {
                    input.read(cx).focus(window);
                }
            }
            Some(Slot::Button(i)) => {
                if let Some(h) = self.buttons.get(i) {
                    window.focus(h);
                }
            }
            None => window.focus(&self.root),
        }
        if log_enabled() {
            crate::ui::log(&format!("dialog[{}] focus -> {slot:?}", self.spec.title));
        }
    }

    fn current_slot(&self, window: &Window, cx: &App) -> Option<Slot> {
        self.ring.slots().iter().copied().find(|s| match *s {
            Slot::Input(i) => self.inputs[i].read(cx).focus_handle(cx).is_focused(window),
            Slot::Button(i) => self.buttons[i].is_focused(window),
        })
    }

    fn composing(&self, cx: &App) -> bool {
        self.inputs.iter().any(|i| i.read(cx).is_composing())
    }

    fn finish(&mut self, result: DialogResult, window: &mut Window, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        if log_enabled() {
            crate::ui::log(&format!(
                "dialog[{}] result {}",
                self.spec.title,
                result.log_line()
            ));
        }
        // 明文用完即清。
        for input in &self.inputs {
            input.update(cx, |t, cx| t.set_text("", cx));
        }
        if let Some(prev) = self.prev_focus.take() {
            window.focus(&prev);
        }
        cx.emit(DialogEvent::Result(result));
        cx.notify();
    }

    fn finish_button(
        &mut self,
        button: usize,
        trigger: Trigger,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let texts: Vec<String> = self
            .inputs
            .iter()
            .map(|i| i.read(cx).text().to_owned())
            .collect();
        let result = result_for_button(&self.spec, button, trigger, &texts);
        self.finish(result, window, cx);
    }

    fn cancel(&mut self, trigger: Trigger, window: &mut Window, cx: &mut Context<Self>) {
        let result = result_for_cancel(&self.spec, trigger);
        self.finish(result, window, cx);
    }

    fn activate_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.current_slot(window, cx);
        if let Some(b) = enter_target(&self.spec, focused) {
            self.finish_button(b, Trigger::Enter, window, cx);
        }
    }

    /// 捕获阶段：Tab 焦点陷阱、回车、空格（先于输入框）。
    fn on_capture_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if self.done || m.control || m.alt || m.platform || self.composing(cx) {
            return;
        }
        match ev.keystroke.key.as_str() {
            "tab" => {
                let cur = self.current_slot(window, cx);
                let next = if m.shift {
                    self.ring.prev(cur)
                } else {
                    self.ring.next(cur)
                };
                self.focus_slot(next, window, cx);
                cx.stop_propagation();
            }
            "enter" => {
                self.activate_enter(window, cx);
                cx.stop_propagation();
            }
            "space" if !m.shift => {
                // 空格只对获得焦点的按钮生效；输入框里照常输入空格。
                if let Some(Slot::Button(i)) = self.current_slot(window, cx) {
                    self.finish_button(i, Trigger::Enter, window, cx);
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }

    /// 冒泡阶段：焦点不在输入框时的 Esc（输入框里的 Esc 走它自己的事件，已处理取消组字）。
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if self.done || m.control || m.alt || m.platform {
            return;
        }
        if ev.keystroke.key == "escape"
            && !matches!(self.current_slot(window, cx), Some(Slot::Input(_)))
        {
            self.cancel(Trigger::Escape, window, cx);
            cx.stop_propagation();
        }
    }

    fn on_scrim_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.spec.dismiss_on_scrim {
            self.cancel(Trigger::Scrim, window, cx);
        } else {
            // 点遮罩不关闭，也不让焦点溜到外面。
            let cur = self
                .current_slot(window, cx)
                .or_else(|| self.ring.initial(&self.spec));
            self.focus_slot(cur, window, cx);
        }
    }

    fn render_button(
        &self,
        i: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let a = self.appearance;
        let spec = &self.spec.buttons[i];
        let handle = self.buttons[i].clone();
        let focused = handle.is_focused(window);
        let (bg, fg, border): (Hsla, Hsla, Hsla) = match spec.role {
            ButtonRole::Primary => (
                hsla("accent", a),
                hsla("text.onAccent", a),
                hsla("accent", a),
            ),
            ButtonRole::Danger => (
                hsla("status.error", a),
                hsla("text.onAccent", a),
                hsla("status.error", a),
            ),
            ButtonRole::Secondary => (
                hsla("bg.buttonSecondary", a),
                hsla("text.primary", a),
                hsla("border.strong", a),
            ),
        };
        let ring: Hsla = rgba(color("accent", a).with_alpha(0x80).to_u32()).into();
        div()
            .id(("dialog-button", i))
            .track_focus(&handle)
            .cursor(CursorStyle::PointingHand)
            .min_w(px(84.))
            .h(px(30.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(px(13.))
            .hover(|s| s.opacity(0.88))
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: ring,
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(2.),
                }])
            })
            // 键盘激活（回车 / 空格）在捕获阶段的按下事件里处理；GPUI 另会在抬起时补一个键盘 click，
            // 此处忽略，否则回车触发默认按钮后，抬起事件会再误点新获得焦点的元素。
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                if !ev.is_keyboard() {
                    this.finish_button(i, Trigger::Click, window, cx);
                }
            }))
            .child(spec.label.clone())
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.appearance = crate::ui::display::window_appearance(window);
        let a = self.appearance;

        let mut card = div()
            .w(px(440.))
            .max_w_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(hsla("border.strong", a))
            .bg(hsla("bg.dialog", a))
            .text_color(hsla("text.primary", a))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000066).into(),
                offset: point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
            }])
            // 卡片内的点击不能冒泡到遮罩。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.spec.title.clone()),
            );

        for block in &self.spec.body {
            card = match block {
                BodyBlock::Text(t) => card.child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(19.))
                        .text_color(hsla("text.secondary", a))
                        .child(t.clone()),
                ),
                BodyBlock::Code(t) => card.child(
                    div()
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(hsla("border.card", a))
                        .bg(hsla("bg.card", a))
                        .font_family("Cascadia Mono")
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .child(t.clone()),
                ),
            };
        }

        for (i, inp) in self.spec.inputs.iter().enumerate() {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(hsla("text.secondary", a))
                            .child(inp.label.clone()),
                    )
                    .child(self.inputs[i].clone()),
            );
        }

        let mut row = div().mt(px(4.)).flex().flex_row().justify_end().gap(px(8.));
        for i in 0..self.spec.buttons.len() {
            row = row.child(self.render_button(i, window, cx));
        }
        card = card.child(row);

        div()
            .id("kanso-dialog")
            .key_context("KansoDialog")
            .track_focus(&self.root)
            .capture_key_down(cx.listener(Self::on_capture_key))
            .on_key_down(cx.listener(Self::on_key))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla("bg.scrim", a))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.on_scrim_down(window, cx)),
            )
            .child(card)
    }
}
