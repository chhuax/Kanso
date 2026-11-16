//! 终端视图实体：持有会话，处理输入，实现 GPUI 的输入法接口。

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, Bounds, ClipboardItem, Context, EntityInputHandler, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent,
    UTF16Selection, Window, div, point, prelude::*, px, rgb, size,
};
use kanso_core::Core;
use kanso_frame::Frame;
use kanso_types::input::{
    Key, KeyEvent, KeyEventKind, Modifiers, MouseButton as KButton, MouseEvent, MouseEventKind,
    PasteContent,
};
use kanso_types::{CellSide, CoreEvent, GridSize, SessionId, ViewportPoint};

use kanso_gpui::ime::ImeState;
use kanso_gpui::keys::{KeyAction, KeyInput, classify_key};
use kanso_gpui::metrics::{cell_at, grid_size, scroll_lines};
use kanso_gpui::render::{RenderGrid, build_render_grid, screen_text};
use kanso_gpui::theme::Palette;

use super::element::{TerminalElement, base_font};
use super::selftest::SelfTest;
use super::{EXIT_CODE, LOG_INPUT, log, memory};

/// 终端区域四周的内边距（逻辑像素）。
pub(crate) const PAD: f32 = 6.0;

/// 组字结束后多长时间内的无修饰按键视为“结束组字的那个键”。
const IME_KEY_GRACE: Duration = Duration::from_millis(100);

/// 字体度量（逻辑像素）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub font_size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
}

/// 量字体：等宽字体用 `m` 的步进作为单元格宽，行高取 ascent + descent 向上取整。
pub(crate) fn measure(window: &Window) -> Metrics {
    let font_size = 14.0_f32;
    let ts = window.text_system();
    let font_id = ts.resolve_font(&base_font());
    let fs = px(font_size);
    let cell_w = ts
        .advance(font_id, fs, 'm')
        .map(|s| f32::from(s.width))
        .unwrap_or(font_size * 0.6);
    let cell_h = f32::from(ts.ascent(font_id, fs) + ts.descent(font_id, fs)).ceil();
    Metrics {
        font_size,
        cell_w,
        cell_h: cell_h.max(font_size),
    }
}

fn kmods(m: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        ctrl: m.control,
        alt_left: m.alt,
        alt_right: false,
        meta: m.platform,
    }
}

pub(crate) struct TerminalView {
    pub core: Arc<Core>,
    pub session: SessionId,
    pub focus: FocusHandle,
    pub palette: Arc<Palette>,
    pub frame: Option<Frame>,
    pub grid: Option<Arc<RenderGrid>>,
    /// 整屏文本（只在自检时维护）。
    pub screen: Vec<String>,
    pub ime: ImeState,
    pub metrics: Metrics,
    /// 当前行列数（已通知会话的）。
    pub size: GridSize,
    /// 网格左上角在窗口里的位置（上一次绘制时记录）。
    pub origin: Option<Point<Pixels>>,
    pub first_painted: bool,
    pub selftest: Option<SelfTest>,
    left_down: bool,
}

impl TerminalView {
    pub(crate) fn new(
        core: Arc<Core>,
        session: SessionId,
        mut rx: UnboundedReceiver<CoreEvent>,
        selftest: bool,
        timeout_secs: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let metrics = measure(window);
        log(&format!(
            "字体度量：字号 {} 单元格 {:.2}x{:.2} 逻辑像素，缩放 {}",
            metrics.font_size,
            metrics.cell_w,
            metrics.cell_h,
            window.scale_factor()
        ));
        let focus = cx.focus_handle();
        window.focus(&focus);
        let _ = core.focus(session, true);

        // 核心事件 → UI 线程。核心保证每帧最多唤醒一次（TerminalDirty 合并）。
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                if this.update(cx, |t, cx| t.on_core_event(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            let _ = this.core.focus(this.session, active);
            if !active {
                // 失焦丢弃组字，不上屏（checklist 场景 5）。
                this.ime.cancel();
            }
            cx.notify();
        })
        .detach();

        let st = selftest.then(|| SelfTest::new(Duration::from_secs(timeout_secs)));
        if st.is_some() {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    match this.update(cx, |t, cx| t.selftest_tick(cx)) {
                        Ok(true) => {}
                        _ => break,
                    }
                }
            })
            .detach();
        }

        TerminalView {
            core,
            session,
            focus,
            palette: Arc::new(Palette::kanso_dark()),
            frame: None,
            grid: None,
            screen: Vec::new(),
            ime: ImeState::default(),
            metrics,
            size: GridSize {
                cols: 100,
                rows: 30,
            },
            origin: None,
            first_painted: false,
            selftest: st,
            left_down: false,
        }
    }

    // ---- 核心事件与帧 ----

    fn on_core_event(&mut self, ev: CoreEvent, cx: &mut Context<Self>) {
        match ev {
            CoreEvent::TerminalDirty { session } if session == self.session => self.pull_frame(cx),
            CoreEvent::SessionStateChanged { session, state } if session == self.session => {
                log(&format!("会话状态：{state:?}"));
                if let kanso_types::event::SessionState::Closed(reason) = state {
                    log(&format!("会话已关闭（{reason:?}），退出应用"));
                    if self.selftest.is_some() {
                        EXIT_CODE.store(1, Ordering::SeqCst);
                    }
                    cx.quit();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn pull_frame(&mut self, cx: &mut Context<Self>) {
        let Some(frame) = self.core.take_frame(self.session, &mut self.frame) else {
            return;
        };
        match frame.view() {
            Ok(view) => {
                self.grid = Some(Arc::new(build_render_grid(&view, &self.palette)));
                if self.selftest.is_some() {
                    self.screen = screen_text(&view);
                }
            }
            Err(e) => log(&format!("帧解析失败：{e}")),
        }
        self.frame = Some(frame);
        cx.notify();
    }

    pub(crate) fn apply_resize(&mut self, size: GridSize, scale: f32, cx: &mut Context<Self>) {
        self.size = size;
        let w = (self.metrics.cell_w * scale) as u16;
        let h = (self.metrics.cell_h * scale) as u16;
        log(&format!(
            "调整大小：{}x{} 单元格 {w}x{h} 物理像素",
            size.cols, size.rows
        ));
        if let Err(e) = self.core.resize(self.session, size, w, h) {
            log(&format!("resize 失败：{e}"));
        }
        cx.notify();
    }

    // ---- 向会话发送 ----

    /// 输入前先回到底部（正在看回滚历史时）。
    fn snap_to_bottom(&mut self) {
        let offset = self.grid.as_ref().map_or(0, |g| g.display_offset);
        if offset > 0 {
            let at = ViewportPoint {
                col: 0,
                row: 0,
                side: CellSide::Left,
            };
            let _ = self
                .core
                .scroll(self.session, -(offset as f32), at, Modifiers::NONE);
        }
    }

    pub(crate) fn send_key(&mut self, event: KeyEvent) {
        if LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!(
                "key {:?} mods={:?} text={:?}",
                event.key, event.mods, event.text
            ));
        }
        self.snap_to_bottom();
        if let Err(e) = self.core.key(self.session, event) {
            log(&format!("key 失败：{e}"));
        }
    }

    pub(crate) fn send_text(&mut self, text: &str) {
        if LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!("textInput {text:?}"));
        }
        self.snap_to_bottom();
        if let Err(e) = self.core.text_input(self.session, text) {
            log(&format!("text_input 失败：{e}"));
        }
    }

    pub(crate) fn send_enter(&mut self) {
        self.send_key(KeyEvent {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
            kind: KeyEventKind::Press,
        });
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.core.selection_text(self.session) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            let _ = self.core.clear_selection(self.session);
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            self.snap_to_bottom();
            if let Err(e) = self.core.paste(self.session, PasteContent::Text(text)) {
                log(&format!("paste 失败：{e}"));
            }
        }
    }

    // ---- 输入法（纯状态逻辑在 kanso_gpui::ime，这里只接线）----

    pub(crate) fn ime_mark(
        &mut self,
        text: &str,
        caret: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        if LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!("ime mark {text:?} caret={caret:?}"));
        }
        self.ime.replace_and_mark(text, caret);
        cx.notify();
    }

    pub(crate) fn ime_commit(&mut self, text: &str, cx: &mut Context<Self>) {
        if LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!("ime commit {text:?}"));
        }
        if let Some(t) = self.ime.commit(text) {
            self.send_text(&t);
        }
        cx.notify();
    }

    // ---- 事件处理 ----

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let input = KeyInput {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            is_held: ev.is_held,
        };
        match classify_key(&input) {
            KeyAction::Send(event) => {
                // 输入法用这个键结束了组字（Esc 取消等）：键归输入法，不发给 shell。
                let plain = !event.mods.ctrl && !event.mods.alt();
                if plain && self.ime.ended_within(IME_KEY_GRACE) {
                    if LOG_INPUT.load(Ordering::Relaxed) {
                        log(&format!("吞掉结束组字的按键 {:?}", event.key));
                    }
                } else {
                    self.send_key(event);
                }
                cx.stop_propagation();
            }
            KeyAction::Copy => {
                self.copy(cx);
                cx.stop_propagation();
            }
            KeyAction::Paste => {
                self.paste(cx);
                cx.stop_propagation();
            }
            KeyAction::PassThrough => {}
        }
    }

    fn grid_point(&self, pos: Point<Pixels>) -> ViewportPoint {
        let o = self.origin.unwrap_or_else(|| point(px(PAD), px(PAD)));
        cell_at(
            f32::from(pos.x) - f32::from(o.x),
            f32::from(pos.y) - f32::from(o.y),
            self.metrics.cell_w,
            self.metrics.cell_h,
            self.size,
        )
    }

    fn mouse_reporting(&self) -> bool {
        self.grid.as_ref().is_some_and(|g| g.mouse_reporting)
    }

    fn send_mouse(
        &mut self,
        kind: MouseEventKind,
        button: KButton,
        pos: Point<Pixels>,
        mods: &gpui::Modifiers,
        click_count: usize,
    ) {
        let event = MouseEvent {
            kind,
            button,
            point: self.grid_point(pos),
            mods: kmods(mods),
            click_count: click_count.min(255) as u8,
        };
        if let Err(e) = self.core.mouse(self.session, event) {
            log(&format!("mouse 失败：{e}"));
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        match ev.button {
            MouseButton::Left => {
                self.left_down = true;
                self.send_mouse(
                    MouseEventKind::Down,
                    KButton::Left,
                    ev.position,
                    &ev.modifiers,
                    ev.click_count,
                );
            }
            MouseButton::Right => {
                if self.mouse_reporting() && !ev.modifiers.shift {
                    self.send_mouse(
                        MouseEventKind::Down,
                        KButton::Right,
                        ev.position,
                        &ev.modifiers,
                        ev.click_count,
                    );
                } else if self.core.has_selection(self.session) {
                    // “复制或粘贴”模式（terminal.md §8.1）：有选区就复制，没有就粘贴。
                    self.copy(cx);
                } else {
                    self.paste(cx);
                }
            }
            _ => {}
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        match ev.button {
            MouseButton::Left => {
                if self.left_down {
                    self.left_down = false;
                    self.send_mouse(
                        MouseEventKind::Up,
                        KButton::Left,
                        ev.position,
                        &ev.modifiers,
                        ev.click_count,
                    );
                }
            }
            MouseButton::Right if self.mouse_reporting() => {
                self.send_mouse(
                    MouseEventKind::Up,
                    KButton::Right,
                    ev.position,
                    &ev.modifiers,
                    ev.click_count,
                );
            }
            _ => {}
        }
    }

    fn on_mouse_move(
        &mut self,
        ev: &MouseMoveEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if self.left_down && ev.pressed_button == Some(MouseButton::Left) {
            self.send_mouse(
                MouseEventKind::Drag,
                KButton::Left,
                ev.position,
                &ev.modifiers,
                1,
            );
        } else if self.mouse_reporting() {
            self.send_mouse(
                MouseEventKind::Move,
                KButton::None,
                ev.position,
                &ev.modifiers,
                0,
            );
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let dy = ev.delta.pixel_delta(px(self.metrics.cell_h)).y.into();
        let lines = scroll_lines(dy, self.metrics.cell_h);
        if lines == 0.0 {
            return;
        }
        let at = self.grid_point(ev.position);
        if let Err(e) = self
            .core
            .scroll(self.session, lines, at, kmods(&ev.modifiers))
        {
            log(&format!("scroll 失败：{e}"));
        }
    }

    pub(crate) fn memory_line(&self) -> String {
        memory::describe()
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(self.palette.background.to_u32()))
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TerminalElement { view: cx.entity() })
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.ime.text_for_range(range);
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
            range: self.ime.selected_range(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.ime.marked_range()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime.cancel();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_commit(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_mark(new_text, new_selected_range, cx);
    }

    /// 候选框定位：返回 `range` 对应文字在窗口里的矩形。组字时 `range` 落在预编辑串内，
    /// 矩形从光标所在单元格起算；没有组字时就是光标单元格。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (cursor_col, cursor_row) = self.grid.as_ref().map_or((0, 0), |g| g.cursor_cell);
        let cols = self.size.cols;
        let start_col = match self.ime.marked_text() {
            Some(text) => {
                kanso_gpui::ime::layout_preedit(
                    text,
                    self.ime.selected_range().start,
                    cursor_col,
                    cols,
                )
                .start_col
            }
            None => cursor_col,
        };
        let before = self.ime.cells_before(range_utf16.start);
        let width = self
            .ime
            .cells_before(range_utf16.end)
            .saturating_sub(before)
            .max(1);
        let m = self.metrics;
        let col = (usize::from(start_col) + before).min(usize::from(cols.saturating_sub(1)));
        Some(Bounds::new(
            point(
                element_bounds.origin.x + px(PAD + col as f32 * m.cell_w),
                element_bounds.origin.y + px(PAD + f32::from(cursor_row) * m.cell_h),
            ),
            size(px(width as f32 * m.cell_w), px(m.cell_h)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// 供元素计算行列数。
pub(crate) fn compute_grid(width: f32, height: f32, m: &Metrics) -> Option<GridSize> {
    grid_size(width - 2.0 * PAD, height - 2.0 * PAD, m.cell_w, m.cell_h)
}
