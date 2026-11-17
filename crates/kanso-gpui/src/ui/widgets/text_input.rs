//! 单行文本输入框（GPUI 视图 + 自绘元素）。
//!
//! 编辑状态、选区、撤销、掩码、滚动与配色都在库里的 `kanso_gpui::widgets`（三平台可测），
//! 本文件只做 GPUI 接线：按键动作、鼠标选区、输入法 `EntityInputHandler`、文字整形与绘制、焦点与光标闪烁。
//!
//! 文件结构与 `EntityInputHandler` / 自绘元素（`prepaint` 整形文字、`paint` 里 `handle_input`）的骨架
//! 改编自 gpui 0.2.2 的 `examples/input.rs`（Apache-2.0，Copyright Zed Industries, Inc.），已修改：
//! 编辑逻辑换成 `EditState`，增加按词移动、撤销、密码掩码、水平滚动、光标闪烁、占位文字、禁用 / 只读、
//! 事件、焦点环、Tab 切换焦点，以及终端视图里处理过的输入法细节（取消组字后的 Esc 只吞一次）。
//! 许可证全文见 `crates/kanso-gpui/LICENSE-APACHE`，来源说明见 `crates/kanso-gpui/third_party/README.md`。

use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, BoxShadow, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hsla, KeyBinding, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine,
    SharedString, Style, Subscription, TextRun, UTF16Selection, UnderlineStyle, Window, div, fill,
    point, prelude::*, px, relative, rgba, size,
};
use kanso_gpui::widgets::blink::{Blink, HALF_PERIOD};
use kanso_gpui::widgets::edit::{Display, EditState};
use kanso_gpui::widgets::scroll::follow_cursor;
use kanso_gpui::widgets::style::{Appearance, FieldStyle, Rgba8};

/// 键盘上下文名，快捷键绑定用它限定作用域。
pub const KEY_CONTEXT: &str = "KansoTextInput";

/// 组字被取消后多长时间内的无修饰 Esc 视为“取消组字的那个键”（与终端视图一致）。
const ESC_GRACE: Duration = Duration::from_millis(100);
/// 光标宽度（逻辑像素）。
const CARET_W: f32 = 1.0;
/// 文字行高（逻辑像素）。
const LINE_HEIGHT: f32 = 18.0;

mod act {
    use gpui::actions;
    actions!(
        kanso_text_input,
        [
            Backspace,
            Delete,
            BackspaceWord,
            DeleteWord,
            Left,
            Right,
            SelectLeft,
            SelectRight,
            WordLeft,
            WordRight,
            SelectWordLeft,
            SelectWordRight,
            Home,
            End,
            SelectHome,
            SelectEnd,
            SelectAll,
            Copy,
            Cut,
            Paste,
            Undo,
            Redo,
        ]
    );
}
use act::*;

/// 注册输入框的快捷键。应用启动时调用一次。
pub fn bind_keys(cx: &mut App) {
    let c = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("shift-backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("ctrl-backspace", BackspaceWord, c),
        KeyBinding::new("ctrl-delete", DeleteWord, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("ctrl-left", WordLeft, c),
        KeyBinding::new("ctrl-right", WordRight, c),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, c),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-insert", Copy, c),
        KeyBinding::new("ctrl-x", Cut, c),
        KeyBinding::new("shift-delete", Cut, c),
        KeyBinding::new("ctrl-v", Paste, c),
        KeyBinding::new("shift-insert", Paste, c),
        KeyBinding::new("ctrl-z", Undo, c),
        KeyBinding::new("ctrl-y", Redo, c),
        KeyBinding::new("ctrl-shift-z", Redo, c),
    ]);
}

/// 输入框向外发出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputEvent {
    /// 内容变化（携带新内容；组字期间的预编辑串也算内容）。
    Change(SharedString),
    /// 按下回车（携带当前内容）。
    Submit(SharedString),
    /// 按下 Esc（取消组字的那个 Esc 已被吞掉，不会发出）。
    Escape,
    /// 获得焦点。
    Focus,
    /// 失去焦点。
    Blur,
}

/// 单行输入框。
pub struct TextInput {
    focus: FocusHandle,
    state: EditState,
    placeholder: SharedString,
    search: bool,
    disabled: bool,
    appearance: Appearance,
    scroll_x: f32,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// 最近一次绘制用的字体、字号与文字色，输入法回调里据此现场整形。
    last_font: Option<(gpui::Font, Pixels, Hsla)>,
    is_selecting: bool,
    blink: Blink,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn hsla(c: Rgba8) -> Hsla {
    rgba(c.to_u32()).into()
}

impl TextInput {
    /// 创建输入框。必须在窗口上下文里创建（需要订阅焦点事件）。
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        let subs = vec![
            cx.on_focus(&focus, window, |this, _window, cx| this.on_focus(cx)),
            cx.on_blur(&focus, window, |this, _window, cx| this.on_blur(cx)),
            // 窗口失活时丢弃组字，不上屏，避免输入法随后又上屏一次造成重复。
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.state.cancel_composition();
                    cx.notify();
                }
            }),
        ];
        TextInput {
            focus,
            state: EditState::default(),
            placeholder: SharedString::default(),
            search: false,
            disabled: false,
            appearance: Appearance::Dark,
            scroll_x: 0.0,
            last_layout: None,
            last_bounds: None,
            last_font: None,
            is_selecting: false,
            blink: Blink::default(),
            _subs: subs,
        }
    }

    // ---- 构建器 ----

    /// 搜索输入在文字前显示放大镜，与 Mac 的路径筛选框一致。
    pub fn search(mut self) -> Self {
        self.search = true;
        self
    }

    /// 初始内容（光标在末尾）。
    pub fn with_text(mut self, text: &str) -> Self {
        self.state.set_text(text);
        self
    }

    /// 占位文字。
    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// 密码模式：显示圆点，禁止复制 / 剪切。
    pub fn masked(mut self) -> Self {
        self.state.masked = true;
        self
    }

    /// 禁用：不可聚焦、不可编辑、不可选择。
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self.focus = self.focus.tab_stop(false);
        self
    }

    /// 只读：可聚焦、可选择、可复制，不可修改。
    pub fn read_only(mut self) -> Self {
        self.state.read_only = true;
        self
    }

    /// 最大字符数。
    pub fn max_len(mut self, n: usize) -> Self {
        self.state.max_len = Some(n);
        self
    }

    // ---- 运行时接口 ----

    /// 当前内容。
    #[allow(dead_code)] // 控件公开接口，演示窗口暂未用到
    pub fn text(&self) -> &str {
        self.state.text()
    }

    /// 整体设置内容（光标到末尾，清空撤销历史）。
    #[allow(dead_code)]
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let before = self.state.text().to_owned();
        self.state.set_text(text);
        self.scroll_x = 0.0;
        if before != self.state.text() {
            cx.emit(TextInputEvent::Change(self.state.text().to_owned().into()));
        }
        cx.notify();
    }

    /// 切换禁用状态。
    #[allow(dead_code)]
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        self.focus = self.focus.clone().tab_stop(!disabled);
        cx.notify();
    }

    /// 是否正在输入法组字（对话框据此决定回车 / Tab 是否归输入法）。
    pub fn is_composing(&self) -> bool {
        self.state.is_composing()
    }

    /// 把焦点交给输入框。
    pub fn focus(&self, window: &mut Window) {
        if !self.disabled {
            self.focus.focus(window);
        }
    }

    // ---- 焦点与闪烁 ----

    fn on_focus(&mut self, cx: &mut Context<Self>) {
        self.restart_blink(cx);
        cx.emit(TextInputEvent::Focus);
        cx.notify();
    }

    fn on_blur(&mut self, cx: &mut Context<Self>) {
        self.blink.stop();
        self.is_selecting = false;
        self.state.cancel_composition();
        cx.emit(TextInputEvent::Blur);
        cx.notify();
    }

    /// 光标回到常亮并重新计时（有输入、移动光标、获得焦点时）。
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        let epoch = self.blink.reset();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HALF_PERIOD).await;
                let alive = this.update(cx, |t, cx| {
                    let live = t.blink.tick(epoch);
                    if live {
                        cx.notify();
                    }
                    live
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// 一次状态变更之后的收尾：发 `Change`、光标常亮、重绘。
    fn touched(&mut self, changed: bool, cx: &mut Context<Self>) {
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ui::log(&format!(
                "state changed={changed} cursor={} sel={:?} marked={:?} len={}",
                self.state.cursor(),
                self.state.selection(),
                self.state.marked_range(),
                self.state.text().len()
            ));
        }
        if changed {
            cx.emit(TextInputEvent::Change(self.state.text().to_owned().into()));
        }
        self.restart_blink(cx);
        cx.notify();
    }

    // ---- 动作 ----

    fn mv(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditState)) {
        if self.disabled {
            return;
        }
        f(&mut self.state);
        self.touched(false, cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditState) -> bool) {
        if self.disabled {
            return;
        }
        let changed = f(&mut self.state);
        self.touched(changed, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_left(false));
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_right(false));
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_left(true));
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_right(true));
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_word_left(false));
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_word_right(false));
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_word_left(true));
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_word_right(true));
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_home(false));
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_end(false));
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_home(true));
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_end(true));
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.select_all());
    }
    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.backspace());
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.delete());
    }
    fn backspace_word(&mut self, _: &BackspaceWord, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.backspace_word());
    }
    fn delete_word(&mut self, _: &DeleteWord, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.delete_word());
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.undo());
    }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |s| s.redo());
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ui::log(&format!("copy -> {:?}", self.state.copy_text()));
        }
        if let Some(t) = self.state.copy_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        if let Some(t) = self.state.cut() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
            self.touched(true, cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let clip = cx.read_from_clipboard().and_then(|item| item.text());
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ui::log(&format!("paste <- {clip:?}"));
        }
        if let Some(t) = clip {
            let changed = self.state.paste(&t);
            self.touched(changed, cx);
        }
    }

    /// Esc / Enter / Tab 不是编辑动作，走原始按键事件。
    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let m = &ev.keystroke.modifiers;
        if crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ui::log(&format!(
                "key {} char={:?} composing={}",
                ev.keystroke.unparse(),
                ev.keystroke.key_char,
                self.state.is_composing()
            ));
        }
        match ev.keystroke.key.as_str() {
            "escape" if !m.control && !m.alt && !m.platform => {
                // 输入法用这个 Esc 取消了组字：键归输入法，不当作“关闭”。只吞一次。
                if self.state.take_cancel_escape(ESC_GRACE) {
                    cx.stop_propagation();
                } else if !self.state.is_composing() {
                    cx.emit(TextInputEvent::Escape);
                }
            }
            "enter" if !m.control && !m.alt && !m.platform && !self.state.is_composing() => {
                cx.emit(TextInputEvent::Submit(self.state.text().to_owned().into()));
                cx.stop_propagation();
            }
            "tab" if !m.control && !m.alt && !m.platform && !self.state.is_composing() => {
                if m.shift {
                    window.focus_prev();
                } else {
                    window.focus_next();
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    // ---- 鼠标 ----

    /// 窗口坐标 → 内容字节偏移。
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        let disp = self.state.display();
        let x = position.x - bounds.left() + px(self.scroll_x);
        let d = line.closest_index_for_x(x).min(disp.text.len());
        disp.to_content(d)
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        self.focus.focus(window);
        if self.state.is_composing() {
            return;
        }
        let idx = self.index_for_position(ev.position);
        match ev.click_count {
            0 | 1 => {
                self.state.set_cursor(idx, ev.modifiers.shift);
                self.is_selecting = true;
            }
            2 => self.state.select_word_at(idx),
            _ => self.state.select_all(),
        }
        self.touched(false, cx);
    }

    // ---- 绘制辅助 ----

    fn field_style(&self) -> FieldStyle {
        FieldStyle::new(self.appearance)
    }

    /// 用给定字体整形当前要显示的文字（内容或占位符）。返回整形结果与展示映射。
    fn shape(
        &self,
        font: &gpui::Font,
        font_size: Pixels,
        color: Hsla,
        window: &Window,
    ) -> (ShapedLine, Display) {
        let disp = self.state.display();
        let st = self.field_style();
        let (shown, text_color) = if self.state.is_empty() {
            (self.placeholder.to_string(), hsla(st.placeholder))
        } else {
            (disp.text.clone(), color)
        };
        let base = TextRun {
            len: shown.len(),
            font: font.clone(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked = self
            .state
            .marked_range()
            .map(|m| disp.to_display(m.start)..disp.to_display(m.end))
            .filter(|m| m.end <= shown.len() && !self.state.is_empty());
        let runs: Vec<TextRun> = match marked {
            Some(m) => [
                TextRun {
                    len: m.start,
                    ..base.clone()
                },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle {
                        color: Some(base.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..base.clone()
                },
                TextRun {
                    len: shown.len() - m.end,
                    ..base
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![base],
        };
        let line = window
            .text_system()
            .shape_line(shown.into(), font_size, &runs, None);
        (line, disp)
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.state.text_for_range_utf16(range_utf16);
        *actual_range = Some(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.state.selected_range_utf16(),
            reversed: self.state.is_reversed(),
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.state.marked_range_utf16()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // 与终端视图一致：丢弃而不是留在文本里，避免输入法随后又上屏造成重复。
        self.state.cancel_composition();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let changed = self.state.ime_replace(range_utf16, new_text);
        self.touched(changed, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let changed = self
            .state
            .ime_mark(range_utf16, new_text, new_selected_range_utf16);
        self.touched(changed, cx);
    }

    /// 候选框定位：现场整形当前文字，返回 `range_utf16` 对应的矩形（扣掉水平滚动）。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (font, size, color) = self.last_font.clone()?;
        let (line, disp) = self.shape(&font, size, color, window);
        let r = self.state.range_from_utf16(&range_utf16);
        let x0 = line.x_for_index(disp.to_display(r.start));
        let x1 = line.x_for_index(disp.to_display(r.end));
        let cursor_x = line.x_for_index(disp.to_display(self.state.cursor()));
        let scroll = follow_cursor(
            self.scroll_x,
            f32::from(cursor_x),
            f32::from(line.x_for_index(line.len())),
            f32::from(element_bounds.size.width),
            CARET_W,
        );
        let left = element_bounds.left() - px(scroll);
        Some(Bounds::from_corners(
            point(left + x0, element_bounds.top()),
            point(left + x1, element_bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.last_bounds?.localize(&point)?;
        let idx = self.index_for_position(point);
        Some(self.state.range_to_utf16(&(idx..idx)).start)
    }
}

/// 自绘元素：整形文字、画选区 / 预编辑下划线 / 光标，并注册输入法处理器。
struct TextElement {
    input: Entity<TextInput>,
}

struct Prepaint {
    line: ShapedLine,
    scroll: f32,
    selection: Option<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let (line, disp) = input.shape(&style.font(), font_size, style.color, window);
        let fs = input.field_style();
        let empty = input.state.is_empty();

        let cursor_x = if empty {
            px(0.)
        } else {
            line.x_for_index(disp.to_display(input.state.cursor()))
        };
        let text_w = if empty {
            px(0.)
        } else {
            line.x_for_index(line.len())
        };
        // 没有焦点时从头显示（和多数输入框一致）；获得焦点后再跟随光标。
        let focused = input.focus.is_focused(window);
        let scroll = if focused {
            follow_cursor(
                input.scroll_x,
                f32::from(cursor_x),
                f32::from(text_w),
                f32::from(bounds.size.width),
                CARET_W,
            )
        } else {
            0.0
        };

        let left = bounds.left() - px(scroll);
        let selection = (input.state.has_selection() && !input.state.is_composing()).then(|| {
            let sel = input.state.selection();
            fill(
                Bounds::from_corners(
                    point(
                        left + line.x_for_index(disp.to_display(sel.start)),
                        bounds.top(),
                    ),
                    point(
                        left + line.x_for_index(disp.to_display(sel.end)),
                        bounds.bottom(),
                    ),
                ),
                hsla(fs.selection),
            )
        });
        let caret = (focused
            && !input.disabled
            && input.blink.visible()
            && (!input.state.has_selection() || input.state.is_composing()))
        .then(|| {
            fill(
                Bounds::new(
                    point(left + cursor_x, bounds.top()),
                    size(px(CARET_W), bounds.size.height),
                ),
                hsla(fs.caret),
            )
        });
        Prepaint {
            line,
            scroll,
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, disabled) = {
            let i = self.input.read(cx);
            (i.focus.clone(), i.disabled)
        };
        if !disabled {
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.input.clone()),
                cx,
            );
        }

        // 拖动选区：鼠标移出输入框后仍然跟随（窗口级监听，只在拖动中起作用）。
        let input = self.input.clone();
        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |i, cx| {
                    if i.is_selecting {
                        let idx = i.index_for_position(ev.position);
                        i.state.set_cursor(idx, true);
                        cx.notify();
                    }
                });
            }
        });
        let input = self.input.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |i, _| i.is_selecting = false);
            }
        });

        let line_height = window.line_height();
        let origin = point(bounds.origin.x - px(prepaint.scroll), bounds.origin.y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(sel) = prepaint.selection.take() {
                window.paint_quad(sel);
            }
            prepaint.line.paint(origin, line_height, window, cx).ok();
            if let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = prepaint.line.clone();
        let scroll = prepaint.scroll;
        self.input.update(cx, |i, _| {
            i.last_layout = Some(line);
            i.last_bounds = Some(bounds);
            i.scroll_x = scroll;
            i.last_font = Some((style.font(), font_size, style.color));
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.appearance = crate::ui::display::window_appearance(window);
        let st = self.field_style();
        let focused = self.focus.is_focused(window) && !self.disabled;
        let (bg, border, text) = if self.disabled {
            (st.disabled_bg, st.disabled_border, st.disabled_text)
        } else if focused {
            (st.bg, st.border_focus, st.text)
        } else {
            (st.bg, st.border, st.text)
        };

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .cursor(if self.disabled {
                CursorStyle::OperationNotAllowed
            } else {
                CursorStyle::IBeam
            })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::backspace_word))
            .on_action(cx.listener(Self::delete_word))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .w_full()
            .h(px(st.height))
            .px(px(st.padding_x))
            .flex()
            .items_center()
            .rounded(px(st.radius))
            .border_1()
            .border_color(hsla(border))
            .bg(hsla(bg))
            .text_size(px(st.font_size))
            .line_height(px(LINE_HEIGHT))
            .text_color(hsla(text))
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: hsla(st.ring),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(st.ring_width),
                }])
            })
            .when(self.search, |d| {
                d.gap(px(8.0))
                    .child(super::icons::icon("search", 14.0, hsla(st.placeholder)))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(TextElement { input: cx.entity() }),
            )
    }
}
