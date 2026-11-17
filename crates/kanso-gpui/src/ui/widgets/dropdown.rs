//! 下拉选择框（GPUI 视图）。
//!
//! 选项模型、过滤、键盘导航、键入跳转与浮层定位都在库里的 `kanso_gpui::widgets::dropdown`（三平台可测），
//! 本文件只做 GPUI 接线：
//! - 收起态是一个可聚焦的按钮（当前值 + 箭头），外观与输入框一致（取自同一份 `FieldStyle`）；
//! - 展开态是 `deferred` + `anchored` 的整窗浮层：底层一块透明的遮罩吞掉点击（点外部关闭、不穿透），
//!   遮罩上按定位结果摆放菜单；菜单里是 `uniform_list`（虚拟化，长列表也流畅）；
//! - 可过滤模式在菜单顶部放一个 [`TextInput`]（复用输入框，支持输入法），按键仍冒泡到本控件处理上下 / 翻页 / Esc；
//! - 窗口失活时关闭。

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, BoxShadow, Context, CursorStyle, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window, anchored,
    canvas, deferred, div, point, prelude::*, px, rgba, uniform_list,
};
use kanso_gpui::widgets::dropdown::{
    self as logic, DropdownState, Item, MENU_PADDING, ROW_HEIGHT, Rect, RowKind,
};
use kanso_gpui::widgets::style::{Appearance, FieldStyle, Rgba8, color};

use super::text_input::{TextInput, TextInputEvent};

/// 下拉框向外发出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropdownEvent {
    /// 用户选了不同的值（携带选项序号；程序 `set_selected` 不触发）。
    Change(usize),
    /// 浮层展开。
    Open,
    /// 浮层收起（无论是否改值）。
    Close,
}

/// 下拉选择框。
pub struct Dropdown {
    focus: FocusHandle,
    state: DropdownState,
    placeholder: SharedString,
    /// 过滤模式：`Some(过滤框占位文字)`。
    filter_placeholder: Option<SharedString>,
    filter_input: Entity<TextInput>,
    disabled: bool,
    appearance: Appearance,
    /// 按钮的窗口坐标（绘制时由 `canvas` 回填，打开浮层时据此定位）。
    button_bounds: Rc<Cell<Bounds<Pixels>>>,
    scroll: UniformListScrollHandle,
    /// 一屏能放几行（PageUp / PageDown 用，渲染时更新）。
    page_rows: usize,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DropdownEvent> for Dropdown {}

impl Focusable for Dropdown {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn hsla(c: Rgba8) -> Hsla {
    rgba(c.to_u32()).into()
}

fn now_ms() -> u64 {
    crate::ui::START
        .get()
        .map_or(0, |s| s.elapsed().as_millis() as u64)
}

impl Dropdown {
    /// 创建下拉框。必须在窗口上下文里创建（需要订阅焦点 / 窗口激活）。
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_input = cx.new(|cx| TextInput::new(window, cx));
        let mut subs = vec![
            // 窗口失焦关闭浮层。
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() && this.state.is_open() {
                    this.close(window, cx);
                }
            }),
        ];
        subs.push(cx.subscribe_in(
            &filter_input,
            window,
            |this, _input, ev: &TextInputEvent, window, cx| match ev {
                TextInputEvent::Change(t) => {
                    if this.state.is_open() {
                        let row = this.state.set_filter(t);
                        this.reveal(row, ScrollStrategy::Top);
                        cx.notify();
                    }
                }
                TextInputEvent::Submit(_) => this.accept(window, cx),
                TextInputEvent::Escape => this.close(window, cx),
                _ => {}
            },
        ));
        Dropdown {
            focus: cx.focus_handle().tab_stop(true),
            state: DropdownState::default(),
            placeholder: "请选择".into(),
            filter_placeholder: None,
            filter_input,
            disabled: false,
            appearance: Appearance::Dark,
            button_bounds: Rc::new(Cell::new(Bounds::default())),
            scroll: UniformListScrollHandle::new(),
            page_rows: 8,
            _subs: subs,
        }
    }

    // ---- 构建器 ----

    /// 条目列表。
    pub fn items(mut self, items: Vec<Item>) -> Self {
        self.state.set_items(items);
        self
    }

    /// 初始选中项（选项序号）。
    pub fn selected(mut self, opt: usize) -> Self {
        self.state.set_selected(Some(opt));
        self
    }

    /// 没有选中值时按钮上显示的文字。
    #[allow(dead_code)] // 控件公开接口，演示窗口暂未用到
    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// 打开过滤模式：浮层顶部带一个过滤输入框（占位文字为参数）。
    pub fn filterable(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.filter_placeholder = Some(placeholder.into());
        self
    }

    /// 禁用：不可聚焦、不可展开。
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self.focus = self.focus.tab_stop(false);
        self
    }

    // ---- 运行时接口 ----

    /// 当前选中的选项序号。
    #[allow(dead_code)]
    pub fn selected_index(&self) -> Option<usize> {
        self.state.selected()
    }

    /// 当前选中项的文字。
    #[allow(dead_code)]
    pub fn selected_label(&self) -> Option<&str> {
        self.state.selected_label()
    }

    /// 程序设置选中项（不触发事件）。
    #[allow(dead_code)]
    pub fn set_selected(&mut self, opt: Option<usize>, cx: &mut Context<Self>) {
        self.state.set_selected(opt);
        cx.notify();
    }

    /// 替换条目（浮层会收起，选中值越界则清空）。
    #[allow(dead_code)]
    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        self.state.set_items(items);
        cx.notify();
    }

    /// 切换禁用状态。
    #[allow(dead_code)]
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        self.focus = self.focus.clone().tab_stop(!disabled);
        if disabled {
            self.state.close();
        }
        cx.notify();
    }

    /// 把焦点交给下拉框。
    #[allow(dead_code)]
    pub fn focus(&self, window: &mut Window) {
        if !self.disabled {
            self.focus.focus(window);
        }
    }

    // ---- 展开 / 收起 / 选中 ----

    fn log(&self, msg: &str) {
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ui::log(&format!("dropdown {msg}"));
        }
    }

    fn reveal(&self, row: Option<usize>, strategy: ScrollStrategy) {
        if let Some(r) = row {
            self.scroll.scroll_to_item(r, strategy);
        }
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled || self.state.option_count() == 0 || self.state.is_open() {
            return;
        }
        let row = self.state.open();
        self.reveal(row, ScrollStrategy::Center);
        if self.filter_placeholder.is_some() {
            self.filter_input.update(cx, |i, cx| i.set_text("", cx));
            self.filter_input.update(cx, |i, _| i.focus(window));
        } else {
            self.focus.focus(window);
        }
        self.log(&format!("open highlight={:?}", self.state.highlight()));
        cx.emit(DropdownEvent::Open);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.is_open() {
            return;
        }
        self.state.close();
        self.focus.focus(window);
        self.log("close");
        cx.emit(DropdownEvent::Close);
        cx.notify();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.is_open() {
            return;
        }
        let before = self.state.selected();
        let chosen = self.state.accept();
        self.focus.focus(window);
        if let Some(opt) = chosen
            && before != Some(opt)
        {
            self.log(&format!(
                "change index={opt} label={:?}",
                self.state.option_label(opt)
            ));
            cx.emit(DropdownEvent::Change(opt));
        }
        self.log("close");
        cx.emit(DropdownEvent::Close);
        cx.notify();
    }

    /// 收起态直接改值后的通知。
    fn changed(&mut self, new: Option<usize>, cx: &mut Context<Self>) {
        if let Some(opt) = new {
            self.log(&format!(
                "change index={opt} label={:?}",
                self.state.option_label(opt)
            ));
            cx.emit(DropdownEvent::Change(opt));
        }
        cx.notify();
    }

    // ---- 键盘 ----

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let m = &ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let filterable = self.filter_placeholder.is_some();
        let plain = !m.control && !m.alt && !m.platform;
        if self.state.is_open() {
            let mut handled = true;
            match key {
                "escape" if plain => self.close(window, cx),
                "enter" if plain => self.accept(window, cx),
                "space" if plain && !filterable => self.accept(window, cx),
                "down" => self.nav(cx, |s| s.move_highlight(1), ScrollStrategy::Bottom),
                "up" => self.nav(cx, |s| s.move_highlight(-1), ScrollStrategy::Top),
                "pagedown" => {
                    let n = self.page_rows;
                    self.nav(cx, move |s| s.page(true, n), ScrollStrategy::Bottom);
                }
                "pageup" => {
                    let n = self.page_rows;
                    self.nav(cx, move |s| s.page(false, n), ScrollStrategy::Top);
                }
                "home" if !filterable => self.nav(cx, |s| s.highlight_first(), ScrollStrategy::Top),
                "end" if !filterable => {
                    self.nav(cx, |s| s.highlight_last(), ScrollStrategy::Bottom)
                }
                "tab" if plain || m.shift => {
                    self.close(window, cx);
                    if m.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                }
                _ => {
                    handled = false;
                    if !filterable
                        && plain
                        && let Some(ch) = single_char(ev)
                    {
                        let row = self.state.type_ahead(ch, now_ms());
                        self.reveal(row, ScrollStrategy::Center);
                        cx.notify();
                        handled = true;
                    }
                }
            }
            if handled {
                cx.stop_propagation();
            }
            return;
        }
        // 收起态。
        match key {
            "space" | "enter" if plain => self.open(window, cx),
            "tab" if plain || m.shift => {
                if m.shift {
                    window.focus_prev();
                } else {
                    window.focus_next();
                }
            }
            "down" | "up" if m.alt => self.open(window, cx),
            "down" if plain => {
                let new = self.state.step_selected(1);
                self.changed(new, cx);
            }
            "up" if plain => {
                let new = self.state.step_selected(-1);
                self.changed(new, cx);
            }
            _ => {
                if plain && let Some(ch) = single_char(ev) {
                    let new = self.state.type_ahead_closed(ch, now_ms());
                    self.changed(new, cx);
                } else {
                    return;
                }
            }
        }
        cx.stop_propagation();
    }

    fn nav(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut DropdownState) -> Option<usize>,
        strategy: ScrollStrategy,
    ) {
        let row = f(&mut self.state);
        self.reveal(row, strategy);
        cx.notify();
    }

    // ---- 渲染 ----

    fn render_row(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let a = self.appearance;
        let Some(row) = self.state.rows().get(i).cloned() else {
            return div().into_any_element();
        };
        let label: SharedString = self.state.row_label(i).to_owned().into();
        let mut outer = div().w_full().h(px(ROW_HEIGHT)).relative();
        if row.separator_before {
            outer = outer.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(2.))
                    .right(px(2.))
                    .h(px(1.))
                    .bg(hsla(color("border.subtle", a))),
            );
        }
        match row.kind {
            RowKind::Header(_) => outer
                .child(
                    div()
                        .size_full()
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .text_size(px(11.))
                        .text_color(hsla(color("text.secondary", a)))
                        .child(label),
                )
                .into_any_element(),
            RowKind::Option(opt) => {
                let highlighted = self.state.highlight() == Some(i);
                let selected = self.state.selected() == Some(opt);
                outer
                    .id(ElementId::NamedInteger("dd-row".into(), i as u64))
                    .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _w, cx| {
                        if this.state.hover(i) {
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.state.hover(i);
                        this.accept(window, cx);
                    }))
                    .child(
                        div()
                            .size_full()
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(4.))
                            .when(highlighted, |d| d.bg(hsla(color("bg.rowHover", a))))
                            .child(
                                div()
                                    .w(px(14.))
                                    .flex_none()
                                    .text_size(px(12.))
                                    .when(selected, |d| d.child("✓")),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(label)),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_menu(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let a = self.appearance;
        let filterable = self.filter_placeholder.is_some();
        let vp = window.viewport_size();
        let bb = self.button_bounds.get();
        let button = Rect {
            x: f32::from(bb.origin.x),
            y: f32::from(bb.origin.y),
            w: f32::from(bb.size.width),
            h: f32::from(bb.size.height),
        };
        let rows = self.state.rows().len();
        // +2：菜单自己的 1px 边框上下各一条。
        let content_h = logic::content_height(rows, filterable) + 2.0;
        let desired_w = logic::estimate_menu_width(&self.state, 13.0);
        let placement = logic::place(
            button,
            (f32::from(vp.width), f32::from(vp.height)),
            content_h,
            desired_w,
        );
        let r = placement.rect;
        let list_h = r.h
            - MENU_PADDING * 2.0
            - 2.0
            - if filterable {
                logic::FILTER_AREA_HEIGHT
            } else {
                0.0
            };
        self.page_rows = ((list_h / ROW_HEIGHT).floor() as usize).max(1);

        let list: AnyElement = if rows == 0 {
            div()
                .h(px(ROW_HEIGHT))
                .px(px(8.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(hsla(color("text.tertiary", a)))
                .child("无匹配项")
                .into_any_element()
        } else {
            uniform_list(
                "dropdown-list",
                rows,
                cx.processor(|this, range: Range<usize>, _w, cx| {
                    range.map(|i| this.render_row(i, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .size_full()
            .into_any_element()
        };

        let menu = div()
            .absolute()
            .left(px(r.x))
            .top(px(r.y))
            .w(px(r.w))
            .h(px(r.h))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .p(px(MENU_PADDING))
            .rounded(px(FieldStyle::new(a).radius))
            .border_1()
            .border_color(hsla(color("border.strong", a)))
            .bg(hsla(color("bg.popover", a)))
            .shadow(vec![BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.28),
                offset: point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
            }])
            .text_size(px(13.))
            .text_color(hsla(color("text.primary", a)))
            .when(filterable, |d| {
                d.child(
                    div()
                        .flex_none()
                        .pb(px(6.))
                        .child(self.filter_input.clone()),
                )
            })
            .child(div().flex_1().min_h_0().child(list));

        // 整窗透明遮罩：吞掉点击（点外部关闭，不穿透到下面的控件）。
        let backdrop = div()
            .relative()
            .w(vp.width)
            .h(vp.height)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.close(window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, window, cx| this.close(window, cx)),
            )
            .child(menu);
        deferred(
            anchored()
                .position(point(px(0.), px(0.)))
                .snap_to_window()
                .child(backdrop),
        )
        .with_priority(1)
        .into_any_element()
    }
}

/// 按键携带的单个可打印字符（用于键入跳转）。
fn single_char(ev: &KeyDownEvent) -> Option<char> {
    let s = ev.keystroke.key_char.as_deref()?;
    let mut it = s.chars();
    let c = it.next()?;
    (it.next().is_none() && !c.is_control() && !c.is_whitespace()).then_some(c)
}

impl Render for Dropdown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.appearance = crate::ui::display::window_appearance(window);
        let st = FieldStyle::new(self.appearance);
        let open = self.state.is_open();
        let focused = self.focus.is_focused(window) && !self.disabled;
        let (bg, border, text) = if self.disabled {
            (st.disabled_bg, st.disabled_border, st.disabled_text)
        } else if focused || open {
            (st.bg, st.border_focus, st.text)
        } else {
            (st.bg, st.border, st.text)
        };
        let (label, label_color) = match self.state.selected_label() {
            Some(l) => (SharedString::from(l.to_owned()), text),
            None => (self.placeholder.clone(), st.placeholder),
        };
        let cell = self.button_bounds.clone();
        let button = div()
            .id("dropdown-button")
            .relative()
            .w_full()
            .h(px(st.height))
            .px(px(st.padding_x))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(st.radius))
            .border_1()
            .border_color(hsla(border))
            .bg(hsla(bg))
            .text_size(px(st.font_size))
            .text_color(hsla(label_color))
            .cursor(if self.disabled {
                CursorStyle::OperationNotAllowed
            } else {
                CursorStyle::PointingHand
            })
            .when(focused && !open, |d| {
                d.shadow(vec![BoxShadow {
                    color: hsla(st.ring),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(st.ring_width),
                }])
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if this.disabled {
                        return;
                    }
                    this.focus.focus(window);
                    if this.state.is_open() {
                        this.close(window, cx);
                    } else {
                        this.open(window, cx);
                    }
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(hsla(if self.disabled {
                        st.disabled_text
                    } else {
                        color("text.secondary", self.appearance)
                    }))
                    .child("▾"),
            );

        div()
            .key_context("KansoDropdown")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .w_full()
            .child(
                // 记录整个控件（= 按钮）的窗口坐标；显式 top / left，否则绝对定位按静态位置落到内边距里。
                canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(button)
            .when(open, |d| d.child(self.render_menu(window, cx)))
    }
}
