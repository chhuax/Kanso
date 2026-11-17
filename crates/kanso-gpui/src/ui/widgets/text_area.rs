//! 多行文本输入框（GPUI 视图 + 自绘元素），供 Sender 的命令框使用。
//!
//! 编辑状态是 `EditState` 的多行模式（`multiline = true`），行几何与纵向滚动在
//! `kanso_gpui::widgets::lines`（三平台可测）。本文件只做 GPUI 接线：按键动作、鼠标选区、
//! 输入法 `EntityInputHandler`、逐行整形与绘制、焦点与光标闪烁。
//!
//! 结构与输入法接线同 `text_input.rs`（同样改编自 gpui 0.2.2 的 `examples/input.rs`，Apache-2.0，
//! Copyright Zed Industries, Inc.，见 `third_party/README.md`），区别：文字按行整形、纵向滚动、
//! 回车 / Shift+回车分别发出 `Submit` / 插入换行（输入法组字中的回车归输入法）。

use std::ops::Range;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{
    App, Bounds, BoxShadow, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hsla, KeyBinding, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ScrollWheelEvent,
    ShapedLine, SharedString, Style, Subscription, TextRun, UTF16Selection, UnderlineStyle, Window,
    div, fill, point, prelude::*, px, relative, rgba, size,
};
use kanso_gpui::app::sender::{EnterAction, enter_action};
use kanso_gpui::widgets::blink::{Blink, HALF_PERIOD};
use kanso_gpui::widgets::edit::EditState;
use kanso_gpui::widgets::lines::{clamp_scroll, follow_line, line_at_y, line_of, line_ranges};
use kanso_gpui::widgets::scroll::follow_cursor;
use kanso_gpui::widgets::style::{Appearance, FieldStyle, Rgba8};

/// 键盘上下文名。
pub const KEY_CONTEXT: &str = "KansoTextArea";

const ESC_GRACE: Duration = Duration::from_millis(100);
const CARET_W: f32 = 1.0;
/// 行高（逻辑像素）。
pub const LINE_HEIGHT: f32 = 18.0;
/// 内边距。
const PAD: f32 = 6.0;

mod act {
    use gpui::actions;
    actions!(
        kanso_text_area,
        [
            Backspace,
            Delete,
            BackspaceWord,
            DeleteWord,
            Left,
            Right,
            Up,
            Down,
            SelectLeft,
            SelectRight,
            SelectUp,
            SelectDown,
            WordLeft,
            WordRight,
            SelectWordLeft,
            SelectWordRight,
            Home,
            End,
            SelectHome,
            SelectEnd,
            DocStart,
            DocEnd,
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

/// 注册多行输入框的快捷键。应用启动时调用一次。
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
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("ctrl-left", WordLeft, c),
        KeyBinding::new("ctrl-right", WordRight, c),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, c),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("ctrl-home", DocStart, c),
        KeyBinding::new("ctrl-end", DocEnd, c),
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

/// 向外发出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextAreaEvent {
    /// 内容变化。
    Change(SharedString),
    /// 回车（不含 Shift，且不在组字中）。
    Submit(SharedString),
    /// Esc（取消组字的那个 Esc 已被吞掉）。
    Escape,
    /// 获得焦点。
    Focus,
    /// 失去焦点。
    Blur,
}

/// 多行输入框。
pub struct TextArea {
    focus: FocusHandle,
    state: EditState,
    placeholder: SharedString,
    appearance: Appearance,
    scroll_x: f32,
    scroll_y: f32,
    /// 下一次绘制要把光标滚进可视区。
    follow: bool,
    last_lines: Vec<ShapedLine>,
    last_ranges: Vec<Range<usize>>,
    last_bounds: Option<Bounds<Pixels>>,
    last_font: Option<(gpui::Font, Pixels, Hsla)>,
    is_selecting: bool,
    blink: Blink,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TextAreaEvent> for TextArea {}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn hsla(c: Rgba8) -> Hsla {
    rgba(c.to_u32()).into()
}

impl TextArea {
    /// 创建输入框（需要窗口上下文以订阅焦点事件）。
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        let subs = vec![
            cx.on_focus(&focus, window, |this, _w, cx| {
                this.restart_blink(cx);
                cx.emit(TextAreaEvent::Focus);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |this, _w, cx| {
                this.blink.stop();
                this.is_selecting = false;
                this.state.cancel_composition();
                cx.emit(TextAreaEvent::Blur);
                cx.notify();
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.state.cancel_composition();
                    cx.notify();
                }
            }),
        ];
        let state = EditState {
            multiline: true,
            ..EditState::default()
        };
        TextArea {
            focus,
            state,
            placeholder: SharedString::default(),
            appearance: Appearance::Dark,
            scroll_x: 0.0,
            scroll_y: 0.0,
            follow: false,
            last_lines: Vec::new(),
            last_ranges: Vec::new(),
            last_bounds: None,
            last_font: None,
            is_selecting: false,
            blink: Blink::default(),
            _subs: subs,
        }
    }

    /// 占位文字。
    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// 当前内容。
    pub fn text(&self) -> &str {
        self.state.text()
    }

    /// 整体设置内容（光标到末尾，清空撤销历史）。内容变化时发出 `Change`。
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let before = self.state.text().to_owned();
        self.state.set_text(text);
        self.follow = true;
        if before != self.state.text() {
            cx.emit(TextAreaEvent::Change(self.state.text().to_owned().into()));
        }
        cx.notify();
    }

    /// 是否正在输入法组字。
    pub fn is_composing(&self) -> bool {
        self.state.is_composing()
    }

    /// 把焦点交给输入框。
    pub fn focus(&self, window: &mut Window) {
        self.focus.focus(window);
    }

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

    fn touched(&mut self, changed: bool, cx: &mut Context<Self>) {
        if changed {
            cx.emit(TextAreaEvent::Change(self.state.text().to_owned().into()));
        }
        self.follow = true;
        self.restart_blink(cx);
        cx.notify();
    }

    fn mv(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditState)) {
        f(&mut self.state);
        self.touched(false, cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditState) -> bool) {
        let changed = f(&mut self.state);
        self.touched(changed, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_left(false));
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_right(false));
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_up(false));
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_down(false));
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_left(true));
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_right(true));
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_up(true));
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.move_down(true));
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
    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| s.set_cursor(0, false));
    }
    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.mv(cx, |s| {
            let n = s.text().len();
            s.set_cursor(n, false)
        });
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
        if let Some(t) = self.state.copy_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.state.cut() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
            self.touched(true, cx);
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let changed = self.state.paste(&t);
            self.touched(changed, cx);
        }
    }

    /// Esc / Enter / Tab 走原始按键事件。
    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if crate::ui::LOG_INPUT.load(Ordering::Relaxed) {
            crate::ui::log(&format!(
                "textarea key {} composing={}",
                ev.keystroke.unparse(),
                self.state.is_composing()
            ));
        }
        match ev.keystroke.key.as_str() {
            "escape" if !m.control && !m.alt && !m.platform => {
                if self.state.take_cancel_escape(ESC_GRACE) {
                    cx.stop_propagation();
                } else if !self.state.is_composing() {
                    cx.emit(TextAreaEvent::Escape);
                }
            }
            "enter" if !m.control && !m.alt && !m.platform => {
                match enter_action(m.shift, self.state.is_composing()) {
                    EnterAction::Send => {
                        cx.emit(TextAreaEvent::Submit(self.state.text().to_owned().into()));
                        cx.stop_propagation();
                    }
                    EnterAction::Newline => {
                        let changed = self.state.insert("\n");
                        self.touched(changed, cx);
                        cx.stop_propagation();
                    }
                    EnterAction::InputMethod => {}
                }
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

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let delta = ev.delta.pixel_delta(window.line_height());
        let total = self.last_ranges.len() as f32 * LINE_HEIGHT;
        let next = clamp_scroll(
            self.scroll_y - f32::from(delta.y),
            total,
            f32::from(bounds.size.height),
        );
        if next != self.scroll_y {
            self.scroll_y = next;
            cx.notify();
            cx.stop_propagation();
        }
    }

    // ---- 鼠标 ----

    /// 窗口坐标 → 内容字节偏移。
    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        if self.last_lines.is_empty() {
            return 0;
        }
        let y = f32::from(position.y - bounds.top()) + self.scroll_y;
        let row = line_at_y(y, LINE_HEIGHT, self.last_lines.len());
        let x = position.x - bounds.left() + px(self.scroll_x);
        let range = &self.last_ranges[row];
        let local = self.last_lines[row]
            .closest_index_for_x(x)
            .min(range.end - range.start);
        range.start + local
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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

    fn field_style(&self) -> FieldStyle {
        FieldStyle::new(self.appearance)
    }
}

/// 整形一行文字；`marked` 是这一行内的组字范围（画下划线）。
fn shape_row(
    shown: &str,
    marked: Option<Range<usize>>,
    font: &gpui::Font,
    font_size: Pixels,
    color: Hsla,
    window: &Window,
) -> ShapedLine {
    let base = TextRun {
        len: shown.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs: Vec<TextRun> = match marked.filter(|m| m.end <= shown.len() && m.start < m.end) {
        Some(m) => [
            TextRun {
                len: m.start,
                ..base.clone()
            },
            TextRun {
                len: m.end - m.start,
                underline: Some(UnderlineStyle {
                    color: Some(color),
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
    window
        .text_system()
        .shape_line(shown.to_owned().into(), font_size, &runs, None)
}

/// 组字范围落在某一行内的部分（行内偏移）。
fn marked_in_line(marked: Option<&Range<usize>>, line: &Range<usize>) -> Option<Range<usize>> {
    let m = marked?;
    let start = m.start.max(line.start);
    let end = m.end.min(line.end);
    (start < end).then(|| start - line.start..end - line.start)
}

impl EntityInputHandler for TextArea {
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
        let changed = self
            .state
            .ime_mark(range_utf16, new_text, new_selected_range_utf16);
        self.touched(changed, cx);
    }

    /// 候选框定位：现场整形范围起点所在的行。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (font, font_size, color) = self.last_font.clone()?;
        let r = self.state.range_from_utf16(&range_utf16);
        let ranges = line_ranges(self.state.text());
        let row = line_of(&ranges, r.start);
        let line_range = ranges[row].clone();
        let marked = self.state.marked_range();
        let shaped = shape_row(
            &self.state.text()[line_range.clone()],
            marked_in_line(marked.as_ref(), &line_range),
            &font,
            font_size,
            color,
            window,
        );
        let x0 = shaped.x_for_index(r.start.saturating_sub(line_range.start));
        let x1 = shaped.x_for_index(r.end.min(line_range.end).saturating_sub(line_range.start));
        let left = element_bounds.left() - px(self.scroll_x);
        let top = element_bounds.top() + px(row as f32 * LINE_HEIGHT - self.scroll_y);
        Some(Bounds::from_corners(
            point(left + x0, top),
            point(left + x1.max(x0 + px(1.0)), top + px(LINE_HEIGHT)),
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

struct AreaElement {
    input: Entity<TextArea>,
}

struct Prepaint {
    lines: Vec<ShapedLine>,
    ranges: Vec<Range<usize>>,
    scroll_x: f32,
    scroll_y: f32,
    selections: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for AreaElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AreaElement {
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
        style.size.height = relative(1.).into();
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
        let font = style.font();
        let fs = input.field_style();
        let empty = input.state.is_empty();
        let text = input.state.text().to_owned();
        let ranges = line_ranges(&text);
        let marked = input.state.marked_range();

        let lines: Vec<ShapedLine> = if empty {
            vec![shape_row(
                &input.placeholder,
                None,
                &font,
                font_size,
                hsla(fs.placeholder),
                window,
            )]
        } else {
            ranges
                .iter()
                .map(|r| {
                    shape_row(
                        &text[r.clone()],
                        marked_in_line(marked.as_ref(), r),
                        &font,
                        font_size,
                        style.color,
                        window,
                    )
                })
                .collect()
        };

        let cursor = input.state.cursor();
        let cursor_row = line_of(&ranges, cursor);
        let cursor_x = if empty {
            px(0.)
        } else {
            lines[cursor_row].x_for_index(cursor - ranges[cursor_row].start)
        };
        let max_w = if empty {
            0.0
        } else {
            lines
                .iter()
                .map(|l| f32::from(l.x_for_index(l.len())))
                .fold(0.0, f32::max)
        };

        let view_h = f32::from(bounds.size.height);
        let view_w = f32::from(bounds.size.width);
        let total_h = ranges.len() as f32 * LINE_HEIGHT;
        let mut scroll_y = clamp_scroll(input.scroll_y, total_h, view_h);
        let mut scroll_x = input.scroll_x;
        if input.follow {
            scroll_y = follow_line(scroll_y, cursor_row, LINE_HEIGHT, ranges.len(), view_h);
            scroll_x = follow_cursor(scroll_x, f32::from(cursor_x), max_w, view_w, CARET_W);
        } else {
            scroll_x = scroll_x.clamp(0.0, (max_w + CARET_W - view_w).max(0.0));
        }

        let left = bounds.left() - px(scroll_x);
        let row_top = |row: usize| bounds.top() + px(row as f32 * LINE_HEIGHT - scroll_y);

        let mut selections = Vec::new();
        if input.state.has_selection() && !input.state.is_composing() && !empty {
            let sel = input.state.selection();
            for (i, r) in ranges.iter().enumerate() {
                let start = sel.start.max(r.start);
                let end = sel.end.min(r.end);
                // 选区跨过换行符时，这一行多画一小块表示换行。
                let covers_newline = sel.end > r.end && sel.start <= r.end;
                if start > end || (start == end && !covers_newline) {
                    continue;
                }
                let x0 = lines[i].x_for_index(start - r.start);
                let mut x1 = lines[i].x_for_index(end - r.start);
                if covers_newline {
                    x1 += px(4.0);
                }
                selections.push(fill(
                    Bounds::from_corners(
                        point(left + x0, row_top(i)),
                        point(left + x1, row_top(i) + px(LINE_HEIGHT)),
                    ),
                    hsla(fs.selection),
                ));
            }
        }
        let focused = input.focus.is_focused(window);
        let caret = (focused
            && input.blink.visible()
            && (!input.state.has_selection() || input.state.is_composing()))
        .then(|| {
            fill(
                Bounds::new(
                    point(left + cursor_x, row_top(cursor_row)),
                    size(px(CARET_W), px(LINE_HEIGHT)),
                ),
                hsla(fs.caret),
            )
        });
        Prepaint {
            lines,
            ranges,
            scroll_x,
            scroll_y,
            selections,
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
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

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

        let empty = self.input.read(cx).state.is_empty();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for sel in prepaint.selections.drain(..) {
                window.paint_quad(sel);
            }
            let first = (prepaint.scroll_y / LINE_HEIGHT) as usize;
            let visible = (f32::from(bounds.size.height) / LINE_HEIGHT) as usize + 2;
            for (i, line) in prepaint.lines.iter().enumerate().skip(first).take(visible) {
                let origin = point(
                    bounds.origin.x - px(prepaint.scroll_x),
                    bounds.origin.y + px(i as f32 * LINE_HEIGHT - prepaint.scroll_y),
                );
                line.paint(origin, px(LINE_HEIGHT), window, cx).ok();
            }
            if let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let lines = std::mem::take(&mut prepaint.lines);
        let ranges = std::mem::take(&mut prepaint.ranges);
        let (sx, sy) = (prepaint.scroll_x, prepaint.scroll_y);
        self.input.update(cx, |i, _| {
            // 空内容时 `lines` 里是占位符，点击定位仍按单行空文本处理。
            i.last_lines = lines;
            i.last_ranges = if empty { vec![0..0] } else { ranges };
            i.last_bounds = Some(bounds);
            i.scroll_x = sx;
            i.scroll_y = sy;
            i.follow = false;
            i.last_font = Some((style.font(), font_size, style.color));
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.appearance = crate::ui::display::window_appearance(window);
        let st = self.field_style();
        let focused = self.focus.is_focused(window);
        let border = if focused { st.border_focus } else { st.border };

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::backspace_word))
            .on_action(cx.listener(Self::delete_word))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .size_full()
            .p(px(PAD))
            .rounded(px(st.radius))
            .border_1()
            .border_color(hsla(border))
            .bg(hsla(st.bg))
            .text_size(px(st.font_size))
            .line_height(px(LINE_HEIGHT))
            .text_color(hsla(st.text))
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: hsla(st.ring),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(st.ring_width),
                }])
            })
            .child(
                div()
                    .size_full()
                    .overflow_hidden()
                    .child(AreaElement { input: cx.entity() }),
            )
    }
}
