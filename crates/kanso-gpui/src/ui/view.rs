//! 终端视图实体：持有会话，处理输入，实现 GPUI 的输入法接口。

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EntityInputHandler, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollWheelEvent, Subscription, UTF16Selection, Window, div,
    point, prelude::*, px, rgb, size,
};
use kanso_core::{Core, CoreError};
use kanso_frame::{CommandBlock, Frame};
use kanso_types::input::{
    Key, KeyEvent, KeyEventKind, Modifiers, MouseButton as KButton, MouseEvent, MouseEventKind,
    PasteContent,
};
use kanso_types::{CellSide, GridSize, SessionId, ViewportPoint};

use kanso_gpui::app::blocks::{BlockMenu, BlockPart, BlockSelection, has_command, menu_for};
use kanso_gpui::app::find::{FindResult, FindState, Throttle, TimerAction};
use kanso_gpui::app::links::{LinkHit, is_openable, url_at};
use kanso_gpui::app::settings::{
    ClickAction, GutterMode, MouseSettings, middle_click_action, right_click_action,
    should_copy_on_select,
};
use kanso_gpui::contrast::ContrastGuard;
use kanso_gpui::ime::ImeState;
use kanso_gpui::keys::{KeyAction, KeyInput, classify_key};
use kanso_gpui::metrics::{cell_at, grid_size, scroll_lines};
use kanso_gpui::render::{
    LiveChips, RenderGrid, build_render_grid_for, hyperlink_at, line_texts, logical_screen_text,
};
use kanso_gpui::theme::Palette;

use super::decor::{self, DecorInput};
use super::element::{TerminalElement, base_font};
use super::filer_drag::FilerDrag;
use super::find_bar::{FindBar, FindBarEvent};
use super::selftest::SelfTest;
use super::{LOG_INPUT, log};

/// 终端区域四周的内边距（逻辑像素）。
pub(crate) const PAD: f32 = 6.0;

/// 组字被取消后多长时间内的无修饰 Esc 视为“取消组字的那个键”。
const IME_ESC_GRACE: Duration = Duration::from_millis(100);

/// 字体度量（逻辑像素）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub font_size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// 时间戳 / 行号栏的字号与单元格宽（比正文小一档）。
    pub gutter_font_size: f32,
    pub gutter_cell_w: f32,
}

/// 量字体：等宽字体用 `m` 的步进作为单元格宽；单元格高 = 字体自然行高（ascent + descent 向上取整）× 行距倍数，
/// 字形在单元格内垂直居中（`ShapedLine::paint` 按行高平分额外空间）。字体、字号与行距取自显示设置。
pub(crate) fn measure(window: &Window) -> Metrics {
    let font_size = super::display::terminal_font_size();
    let ts = window.text_system();
    let font_id = ts.resolve_font(&base_font());
    let fs = px(font_size);
    let cell_w = ts
        .advance(font_id, fs, 'm')
        .map(|s| f32::from(s.width))
        .unwrap_or(font_size * 0.6);
    let gutter_font_size = (font_size * 0.85).round().max(9.0);
    let gutter_cell_w = ts
        .advance(font_id, px(gutter_font_size), 'm')
        .map(|s| f32::from(s.width))
        .unwrap_or(gutter_font_size * 0.6);
    let (asc, desc) = (
        f32::from(ts.ascent(font_id, fs)),
        f32::from(ts.descent(font_id, fs)),
    );
    log(&format!(
        "measure：ascent {asc:.2} descent {desc:.2} 字号 {font_size}"
    ));
    // descent 在 GPUI 里是负数（基线以下），取绝对值。
    let natural = (asc + desc.abs()).ceil();
    let cell_h = (natural * super::display::line_height()).round();
    Metrics {
        font_size,
        cell_w,
        cell_h: cell_h.max(font_size),
        gutter_font_size,
        gutter_cell_w,
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

/// 终端视图向外壳报告的事件。
pub(crate) enum TerminalEvent {
    /// 终端阶段的自检结束（`true` 为通过）。
    SelfTestFinished(bool),
    /// 用户在终端上右键（菜单模式、程序没有开启鼠标上报）：外壳在 `position`（窗口坐标）弹出菜单。
    ContextMenu {
        session: SessionId,
        position: Point<Pixels>,
    },
    /// 需要在状态栏上短暂提示一句话（例如“已复制”）。
    Notice(String),
}

pub(crate) struct TerminalView {
    pub core: Arc<Core>,
    pub session: SessionId,
    pub focus: FocusHandle,
    pub palette: Arc<Palette>,
    pub frame: Option<Frame>,
    pub grid: Option<Arc<RenderGrid>>,
    /// 整屏逻辑行（只在自检时维护）。
    pub screen: Vec<String>,
    pub ime: ImeState,
    pub metrics: Metrics,
    /// 当前行列数（已通知会话的）。
    pub size: GridSize,
    /// 网格左上角在窗口里的位置（上一次绘制时记录）。
    pub origin: Option<Point<Pixels>>,
    pub first_painted: bool,
    pub selftest: Option<SelfTest>,
    /// 是否维护整屏文本 `screen`（自检用）。
    pub track_screen: bool,
    left_down: bool,
    /// 中键 / 右键行为与选中即复制（terminal.md §8.1、§8.2）。由外壳统一下发。
    pub mouse: MouseSettings,
    /// 当前这次左键按下是否由终端自己处理选择（按下时没有被鼠标上报接管）。
    /// 在按下时记录而不是松开时再判断：拖动途中程序可能切换鼠标上报，松开时的状态不代表这次选择的归属。
    local_select: bool,
    /// 时间戳 / 行号栏模式（由外壳按全局设置与 AI 会话的临时设置下发）。
    pub gutter_mode: GutterMode,
    /// 最低对比度保护（关闭为 `None`）。
    contrast: Option<ContrastGuard>,
    /// 查找状态（terminal.md §11）与查找栏。
    pub find: FindState,
    find_bar: Option<Entity<FindBar>>,
    find_sub: Option<Subscription>,
    /// 命令块选中状态（command-blocks.md §2）。
    pub block_sel: BlockSelection,
    /// 右键菜单对应的块（复制命令 / 输出作用于它）。
    menu_block: Option<u64>,
    menu_block_menu: Option<BlockMenu>,
    /// 悬停的链接与所在单元格。
    hover: Option<LinkHit>,
    hover_cell: Option<(u16, u16)>,
    ctrl_down: bool,
    /// 左键按下时所在的单元格（判断是否开始拖选）。
    down_cell: Option<(u16, u16)>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    /// 创建终端视图。核心事件由外壳（`ShellView`）统一接收，按会话把“有新帧”转给对应视图的
    /// [`pull_frame`](Self::pull_frame)；焦点也由外壳分配。
    pub(crate) fn new(
        core: Arc<Core>,
        session: SessionId,
        selftest: Option<Duration>,
        track_screen: bool,
        dark: bool,
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

        let st = selftest.map(SelfTest::new);
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
            palette: Arc::new(palette_for(dark)),
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
            track_screen,
            left_down: false,
            mouse: MouseSettings::default(),
            local_select: false,
            gutter_mode: GutterMode::Off,
            contrast: super::display::min_contrast().map(ContrastGuard::new),
            find: FindState::default(),
            find_bar: None,
            find_sub: None,
            block_sel: BlockSelection::default(),
            menu_block: None,
            menu_block_menu: None,
            hover: None,
            hover_cell: None,
            ctrl_down: false,
            down_cell: None,
        }
    }

    /// 栏宽（逻辑像素）；关闭为 0。正文区从它右侧开始。
    pub(crate) fn gutter_width(&self) -> f32 {
        decor::gutter_width(self.gutter_mode, self.metrics.gutter_cell_w)
    }

    /// 换时间戳 / 行号栏模式。栏宽变化会改变列数，由元素下一次绘制时通知会话。
    pub(crate) fn set_gutter_mode(&mut self, mode: GutterMode, cx: &mut Context<Self>) {
        if self.gutter_mode != mode {
            self.gutter_mode = mode;
            cx.notify();
        }
    }

    /// 装饰绘制需要的数据快照；还没有帧时为 `None`。
    pub(crate) fn decor_input(&self) -> Option<DecorInput> {
        Some(DecorInput {
            grid: self.grid.clone()?,
            palette: self.palette.clone(),
            metrics: self.metrics,
            gutter_mode: self.gutter_mode,
            gutter_w: self.gutter_width(),
            blocks: self.blocks().to_vec(),
            selected_block: self.block_sel.selected(),
            hover: self.hover.clone(),
            tz_offset_min: super::tz::local_offset_minutes(),
        })
    }

    fn blocks(&self) -> &[CommandBlock] {
        self.frame
            .as_ref()
            .map_or(&[], |f| f.command_blocks.as_slice())
    }

    /// 用当前帧、配色与对比度保护重建渲染数据。
    fn rebuild_grid(&mut self) {
        if let Some(frame) = &self.frame
            && let Ok(view) = frame.view()
        {
            self.grid = Some(Arc::new(build_render_grid_for(
                &view,
                &self.palette,
                self.contrast.as_mut(),
                LiveChips::from_blocks(&frame.command_blocks),
                &frame.command_blocks,
            )));
        }
    }

    /// 最低对比度保护的设置变了；返回是否有变化。
    pub(crate) fn set_min_contrast(&mut self, ratio: Option<f64>) -> bool {
        if self.contrast.as_ref().map(ContrastGuard::ratio) == ratio {
            return false;
        }
        self.contrast = ratio.map(ContrastGuard::new);
        self.rebuild_grid();
        true
    }

    /// 窗口激活状态变化：失焦丢弃组字，不上屏（checklist 场景 5）。
    pub(crate) fn on_window_activation(&mut self, active: bool, cx: &mut Context<Self>) {
        if !active {
            self.ime.cancel();
        }
        cx.notify();
    }

    /// 换终端配色并重建渲染数据。
    pub(crate) fn set_palette(&mut self, palette: Palette, cx: &mut Context<Self>) {
        if *self.palette == palette {
            return;
        }
        self.palette = Arc::new(palette);
        self.rebuild_grid();
        cx.notify();
    }

    /// 显示设置里的字体 / 字号 / 行距变了：重新量字体，并把新的单元格像素尺寸告知会话。
    /// 行列数由绘制时的元素按新度量重新计算（变了会走 `apply_resize`）。
    pub(crate) fn apply_display(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.sync_find_bar(cx);
        if self.set_min_contrast(super::display::min_contrast()) {
            cx.notify();
        }
        let m = measure(window);
        if (m.font_size, m.cell_w, m.cell_h, m.gutter_cell_w)
            == (
                self.metrics.font_size,
                self.metrics.cell_w,
                self.metrics.cell_h,
                self.metrics.gutter_cell_w,
            )
        {
            return;
        }
        self.metrics = m;
        let scale = window.scale_factor();
        let (w, h) = ((m.cell_w * scale) as u16, (m.cell_h * scale) as u16);
        log(&format!(
            "字体度量更新：字号 {} 单元格 {:.2}x{:.2}",
            m.font_size, m.cell_w, m.cell_h
        ));
        if let Err(e) = self.core.resize(self.session, self.size, w, h) {
            log(&format!("resize 失败：{e}"));
        }
        cx.notify();
    }

    // ---- 核心事件与帧 ----

    pub(crate) fn pull_frame(&mut self, cx: &mut Context<Self>) {
        let Some(frame) = self.core.take_frame(self.session, &mut self.frame) else {
            return;
        };
        match frame.view() {
            Ok(view) => {
                self.grid = Some(Arc::new(build_render_grid_for(
                    &view,
                    &self.palette,
                    self.contrast.as_mut(),
                    LiveChips::from_blocks(&frame.command_blocks),
                    &frame.command_blocks,
                )));
                if self.track_screen || self.selftest.is_some() {
                    self.screen = logical_screen_text(&view);
                }
            }
            Err(e) => log(&format!("帧解析失败：{e}")),
        }
        self.frame = Some(frame);
        // 进入全屏程序：块选中取消，退出后不恢复。
        if self.grid.as_ref().is_some_and(|g| g.alt_screen) {
            self.block_sel.on_alt_screen(true);
        }
        // 内容变了，悬停的链接位置可能已经不对；下一次鼠标移动时重新识别。
        if self.hover.take().is_some() {
            self.hover_cell = None;
        }
        // 新输出到来：搜索计数随之更新（节流）。
        if self.find.wants_refresh() {
            self.schedule_find_refresh(cx);
        }
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
        self.block_sel.clear();
        if let Err(e) = self.core.key(self.session, event) {
            log(&format!("key 失败：{e}"));
        }
    }

    pub(crate) fn send_text(&mut self, text: &str) {
        if LOG_INPUT.load(Ordering::Relaxed) {
            log(&format!("textInput {text:?}"));
        }
        self.snap_to_bottom();
        self.block_sel.clear();
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

    /// 复制：有普通选区复制选区；没有选区但选中了命令块，复制整块（command-blocks.md §3）。
    pub(crate) fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.core.selection_text(self.session) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            let _ = self.core.clear_selection(self.session);
        } else if let Some(id) = self.block_sel.selected() {
            self.copy_block_part(id, BlockPart::All, cx);
        }
    }

    /// 选中即复制：只复制文本选区并保留高亮，不回落命令块。
    /// 不能复用 `copy`：它复制后清掉选区，用户刚拖完选区就消失，看不出选了什么，也没法接着扩选。
    fn copy_selection_keep(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self
            .core
            .selection_text(self.session)
            .filter(|t| !t.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn copy_block_part(&mut self, id: u64, part: BlockPart, cx: &mut Context<Self>) {
        match self
            .core
            .command_block_text(self.session, id, part as u8)
            .filter(|t| !t.is_empty())
        {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.emit(TerminalEvent::Notice(
                    kanso_gpui::app::launcher::render_message(
                        &kanso_types::message::Message::UiCopied {},
                    ),
                ));
            }
            None => log(&format!("块 {id} 没有可复制的内容")),
        }
    }

    /// 右键菜单的“复制命令 / 复制输出”：作用于右键点中的块。
    pub(crate) fn copy_menu_block(&mut self, part: BlockPart, cx: &mut Context<Self>) {
        if let Some(id) = self.menu_block {
            self.copy_block_part(id, part, cx);
        }
    }

    /// 右键菜单里块相关项（点在有命令的块内才有）。
    pub(crate) fn menu_block_menu(&self) -> Option<BlockMenu> {
        self.menu_block_menu
    }

    /// 跳到上一个 / 下一个命令块并选中它（command-blocks.md §4）。
    pub(crate) fn jump_block(&mut self, forward: bool, cx: &mut Context<Self>) {
        match self.core.jump_command_block(self.session, forward) {
            Ok(Some(id)) => {
                self.block_sel.select(id);
                cx.notify();
            }
            Ok(None) => {}
            Err(e) => log(&format!("jump_command_block 失败：{e}")),
        }
    }

    // ---- 查找（terminal.md §11）----

    /// 打开查找栏并聚焦；已打开就重新聚焦。有上次的文字时恢复并立即搜索，高亮随之回来。
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.open = true;
        if self.find_bar.is_none() {
            let bar = cx.new(|cx| FindBar::new(window, cx));
            self.find_sub = Some(cx.subscribe_in(
                &bar,
                window,
                |this, _bar, ev: &FindBarEvent, window, cx| this.on_find_event(ev, window, cx),
            ));
            let text = self.find.text.clone();
            bar.update(cx, |b, cx| b.set_text(&text, cx));
            self.find_bar = Some(bar);
            self.find_search(cx);
        }
        if let Some(bar) = &self.find_bar {
            bar.read(cx).focus_input(window, cx);
        }
        self.sync_find_bar(cx);
        cx.notify();
    }

    /// 关闭查找栏：清高亮、停止随输出刷新、焦点还给终端。
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.core.search_close(self.session);
        self.find.close();
        self.find_bar = None;
        self.find_sub = None;
        self.focus.focus(window);
        cx.notify();
    }

    /// 查找下一个 / 上一个；栏没开时先打开。文字为空什么都不做。
    pub(crate) fn find_step(
        &mut self,
        backwards: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.find.open {
            self.open_find(window, cx);
        }
        if self.find.text.is_empty() {
            return;
        }
        let r = self.core.search_next(self.session, backwards);
        self.apply_search(r);
        self.sync_find_bar(cx);
        cx.notify();
    }

    fn on_find_event(&mut self, ev: &FindBarEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            FindBarEvent::Query(t) => {
                if *t != self.find.text {
                    self.find.text = t.clone();
                    self.find_search(cx);
                }
            }
            FindBarEvent::ToggleCase => {
                self.find.case_sensitive = !self.find.case_sensitive;
                self.find_search(cx);
            }
            FindBarEvent::ToggleRegex => {
                self.find.regex = !self.find.regex;
                self.find_search(cx);
            }
            FindBarEvent::Next => self.find_step(false, window, cx),
            FindBarEvent::Prev => self.find_step(true, window, cx),
            FindBarEvent::Close => self.close_find(window, cx),
        }
    }

    /// 用当前文字与开关搜索；文字为空则关闭搜索并清空状态（不显示“无结果”）。
    fn find_search(&mut self, cx: &mut Context<Self>) {
        match self.find.query() {
            None => {
                self.core.search_close(self.session);
                self.find.clear_result();
            }
            Some(q) => {
                let r = self.core.search(self.session, q);
                self.apply_search(r);
            }
        }
        self.sync_find_bar(cx);
        cx.notify();
    }

    /// 匹配计数及正则错误来自同一份终端搜索状态。
    fn apply_search(&mut self, r: Result<kanso_core::SearchStatus, CoreError>) {
        self.find.result = match r {
            Ok(s) => s.into(),
            Err(_) => FindResult::Idle,
        };
    }

    fn sync_find_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(bar) = &self.find_bar {
            let (case, regex, text, err) = (
                self.find.case_sensitive,
                self.find.regex,
                self.find.status_text(),
                self.find.is_error(),
            );
            bar.update(cx, |b, cx| b.set_state(case, regex, text, err, cx));
        }
    }

    /// 新输出到来：节流后读一次搜索状态，保证最后一次输出之后的计数是准确的。
    fn schedule_find_refresh(&mut self, cx: &mut Context<Self>) {
        if let Throttle::RunNow { timer_ms } = self.find.refresher.request() {
            self.refresh_find_status(cx);
            self.arm_find_timer(timer_ms, cx);
        }
    }

    fn refresh_find_status(&mut self, cx: &mut Context<Self>) {
        let r = self.core.search_status(self.session);
        self.apply_search(r);
        self.sync_find_bar(cx);
    }

    fn arm_find_timer(&mut self, ms: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(ms))
                .await;
            let _ = this.update(cx, |t, cx| match t.find.refresher.on_timer() {
                TimerAction::Idle => {}
                TimerAction::RunAndRearm { timer_ms } => {
                    if t.find.wants_refresh() {
                        t.refresh_find_status(cx);
                    }
                    t.arm_find_timer(timer_ms, cx);
                }
            });
        })
        .detach();
    }

    /// 自检：直接设置搜索文字与正则开关并搜索。
    pub(crate) fn selftest_query(&mut self, text: &str, regex: bool, cx: &mut Context<Self>) {
        self.find.text = text.to_owned();
        self.find.regex = regex;
        self.find_search(cx);
    }

    /// 自检：当前屏幕上识别到的第一个 URL。
    pub(crate) fn selftest_find_url(&self) -> Option<String> {
        let view = self.frame.as_ref()?.view().ok()?;
        let lines = line_texts(&view);
        (0..lines.len()).find_map(|r| {
            (0..lines[r].chars.len()).find_map(|c| url_at(&lines, r, c).map(|h| h.url))
        })
    }

    // ---- 链接（terminal.md §10）----

    /// 指针下的链接：先看 OSC 8 超链接，再从屏幕文字里识别 URL。
    fn link_at(&self, pos: Point<Pixels>) -> Option<LinkHit> {
        let p = self.grid_point(pos);
        let frame = self.frame.as_ref()?;
        let view = frame.view().ok()?;
        if let Some(id) = hyperlink_at(&view, p.row, p.col) {
            let url = self.core.hyperlink_url(self.session, id)?;
            let same = |c: u16| hyperlink_at(&view, p.row, c) == Some(id);
            let (mut c0, mut c1) = (p.col, p.col + 1);
            while c0 > 0 && same(c0 - 1) {
                c0 -= 1;
            }
            while c1 < view.header().cols && same(c1) {
                c1 += 1;
            }
            return Some(LinkHit {
                url,
                segments: vec![(p.row, c0, c1)],
            });
        }
        url_at(&line_texts(&view), usize::from(p.row), usize::from(p.col))
    }

    /// Ctrl+点击：指针下有链接就用系统默认程序打开，返回是否处理了这次点击。
    fn try_open_link(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(link) = self.link_at(pos) else {
            return false;
        };
        if is_openable(&link.url) {
            log(&format!("打开链接 {}", link.url));
            cx.open_url(&link.url);
        } else {
            log(&format!("拒绝打开不受信任协议的链接 {}", link.url));
            cx.emit(TerminalEvent::Notice(
                kanso_gpui::app::launcher::render_message(
                    &kanso_types::message::Message::UiLinkUnsupported {},
                ),
            ));
        }
        true
    }

    fn update_hover(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let ctrl = ev.modifiers.control;
        if ctrl != self.ctrl_down {
            self.ctrl_down = ctrl;
            if self.hover.is_some() {
                cx.notify();
            }
        }
        let p = self.grid_point(ev.position);
        let cell = (p.row, p.col);
        if self.hover_cell == Some(cell) {
            return;
        }
        self.hover_cell = Some(cell);
        let blocked = self.mouse_reporting() && !ev.modifiers.shift;
        let new = if blocked {
            None
        } else {
            self.link_at(ev.position)
        };
        if new != self.hover {
            self.hover = new;
            cx.notify();
        }
    }

    // ---- 命令块点击（command-blocks.md §2）----

    fn block_at_row(&self, row: u16) -> Option<&CommandBlock> {
        decor::block_for_row(self.grid.as_deref()?, self.blocks(), row)
    }

    fn click_block(&mut self, pos: Point<Pixels>, click_count: usize, cx: &mut Context<Self>) {
        let row = self.grid_point(pos).row;
        let hit = self.block_at_row(row).cloned();
        if self.block_sel.click(hit.as_ref(), click_count) {
            cx.notify();
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.core.has_selection(self.session)
    }

    pub(crate) fn select_all(&mut self) {
        if let Err(e) = self.core.select_all(self.session) {
            log(&format!("select_all 失败：{e}"));
        }
    }

    pub(crate) fn clear_buffer(&mut self) {
        self.block_sel.clear();
        if let Err(e) = self.core.clear_buffer(self.session) {
            log(&format!("clear_buffer 失败：{e}"));
        }
    }

    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(content) = super::clipboard::read_terminal_paste(cx) {
            self.snap_to_bottom();
            self.block_sel.clear();
            if let Err(e) = self.core.paste(self.session, content) {
                log(&format!("paste 失败：{e}"));
            }
        }
    }

    /// 文件落到终端（外部文件或从 Filer 拖来的条目）：向这个终端插入路径，聚焦它；不自动执行（filer.md §7.5）。
    /// 转义、AI CLI 会话保持原样粘贴路径等规则都在核心的 `PasteContent::Files` 里。
    pub(crate) fn drop_paths(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        window.focus(&self.focus);
        self.snap_to_bottom();
        self.block_sel.clear();
        if let Err(e) = self.core.paste(self.session, PasteContent::Files(paths)) {
            log(&format!("拖入路径失败：{e}"));
        }
        cx.notify();
    }

    /// 从辅助 Filer 拖来的条目落下。远端条目要等本机暂存副本准备完成，没完成就拒绝（并取消准备）。
    fn drop_filer(&mut self, d: &FilerDrag, window: &mut Window, cx: &mut Context<Self>) {
        use kanso_gpui::app::drag_out::DropOutcome;
        match d.prep.on_drop() {
            DropOutcome::Insert(paths) => self.drop_paths(paths, window, cx),
            DropOutcome::NotReady => {
                cx.emit(TerminalEvent::Notice(kanso_gpui::app::filer_text::msg(
                    kanso_types::message::Message::FilerDragNotReady {},
                )))
            }
            DropOutcome::Failed(why) => cx.emit(TerminalEvent::Notice(
                kanso_gpui::app::filer_text::msg(kanso_types::message::Message::FilerDragFailed {
                    name: d.label(),
                    why,
                }),
            )),
            DropOutcome::Ignore => {}
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
                // 输入法用这个 Esc 取消了组字：键归输入法，不发给 shell。
                // 只吞 Esc，且只在组字被取消（没有上屏）时；其他键一律正常发送。
                let plain = !event.mods.ctrl && !event.mods.alt();
                if plain && event.key == Key::Escape && self.ime.take_cancel_escape(IME_ESC_GRACE) {
                    if LOG_INPUT.load(Ordering::Relaxed) {
                        log("吞掉取消组字的 Esc");
                    }
                } else {
                    self.send_key(event);
                }
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
                // 程序没有接管鼠标（或按住 Shift 绕过）时，终端自己处理：Ctrl+点击开链接，单击选 / 取消命令块。
                let reporting = self.mouse_reporting() && !ev.modifiers.shift;
                if !reporting {
                    if ev.modifiers.control && self.try_open_link(ev.position, cx) {
                        return;
                    }
                    self.click_block(ev.position, ev.click_count, cx);
                }
                let p = self.grid_point(ev.position);
                self.down_cell = Some((p.row, p.col));
                self.left_down = true;
                self.local_select = !reporting;
                self.send_mouse(
                    MouseEventKind::Down,
                    KButton::Left,
                    ev.position,
                    &ev.modifiers,
                    ev.click_count,
                );
            }
            MouseButton::Right => {
                let action = right_click_action(
                    self.mouse.right_click,
                    self.mouse_reporting(),
                    ev.modifiers.shift,
                    self.has_selection(),
                );
                if action == ClickAction::Report {
                    self.send_mouse(
                        MouseEventKind::Down,
                        KButton::Right,
                        ev.position,
                        &ev.modifiers,
                        ev.click_count,
                    );
                } else if action == ClickAction::Menu {
                    // 右键点在有命令的块内时，菜单里多“复制命令 / 复制输出”。
                    let row = self.grid_point(ev.position).row;
                    let hit = self.block_at_row(row).cloned();
                    self.menu_block = hit.as_ref().filter(|b| has_command(b)).map(|b| b.id);
                    self.menu_block_menu = menu_for(hit.as_ref());
                    // 菜单模式：没有选区时先选中指针下的单词（VS Code 的做法），再让外壳弹菜单。
                    if !self.has_selection() {
                        let at = self.grid_point(ev.position);
                        let _ = self.core.select_word_at(self.session, at);
                    }
                    cx.emit(TerminalEvent::ContextMenu {
                        session: self.session,
                        position: ev.position,
                    });
                } else if action == ClickAction::Copy {
                    // “复制或粘贴”模式（terminal.md §8.1）：有选区就复制，没有就粘贴。
                    self.copy(cx);
                } else {
                    self.paste(cx);
                }
            }
            MouseButton::Middle => {
                match middle_click_action(
                    self.mouse.middle_click,
                    self.mouse_reporting(),
                    ev.modifiers.shift,
                ) {
                    ClickAction::Report => self.send_mouse(
                        MouseEventKind::Down,
                        KButton::Middle,
                        ev.position,
                        &ev.modifiers,
                        ev.click_count,
                    ),
                    ClickAction::Paste => self.paste(cx),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
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
                    // 选区由 core 维护，必须在 Up 送达之后再查（Up 才会结束这次拖动选择）。
                    let local = std::mem::take(&mut self.local_select);
                    if should_copy_on_select(self.mouse.copy_on_select, local, self.has_selection())
                    {
                        self.copy_selection_keep(cx);
                    }
                }
            }
            MouseButton::Right | MouseButton::Middle
                if self.mouse_reporting() && !ev.modifiers.shift =>
            {
                self.send_mouse(
                    MouseEventKind::Up,
                    if ev.button == MouseButton::Right {
                        KButton::Right
                    } else {
                        KButton::Middle
                    },
                    ev.position,
                    &ev.modifiers,
                    ev.click_count,
                );
            }
            _ => {}
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.left_down && ev.pressed_button == Some(MouseButton::Left) {
            // 开始拖动文本选区：取消块选中（同一个单元格内的抖动不算）。
            let p = self.grid_point(ev.position);
            if self.down_cell.is_some_and(|c| c != (p.row, p.col)) && self.block_sel.clear() {
                cx.notify();
            }
            self.send_mouse(
                MouseEventKind::Drag,
                KButton::Left,
                ev.position,
                &ev.modifiers,
                1,
            );
        } else {
            self.update_hover(ev, cx);
            if self.mouse_reporting() && !ev.modifiers.shift {
                let button = match ev.pressed_button {
                    Some(MouseButton::Right) => KButton::Right,
                    Some(MouseButton::Middle) => KButton::Middle,
                    _ => KButton::None,
                };
                self.send_mouse(
                    if button == KButton::None {
                        MouseEventKind::Move
                    } else {
                        MouseEventKind::Drag
                    },
                    button,
                    ev.position,
                    &ev.modifiers,
                    0,
                );
            }
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
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = self.find_bar.clone();
        div()
            .size_full()
            .relative()
            .bg(rgb(self.palette.background.to_u32()))
            .when(self.hover.is_some() && self.ctrl_down, |d| {
                d.cursor_pointer()
            })
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                let paths = paths
                    .paths()
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                this.drop_paths(paths, window, cx);
            }))
            .on_drop(cx.listener(|this, d: &FilerDrag, window, cx| this.drop_filer(d, window, cx)))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_2().border_color(rgb(0x3b82f6)))
            .drag_over::<FilerDrag>(|s, _, _, _| s.border_2().border_color(rgb(0x3b82f6)))
            .child(TerminalElement { view: cx.entity() })
            // 查找栏浮在右上角。
            .children(bar.map(|b| div().absolute().top(px(8.0)).right(px(8.0)).child(b)))
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
                element_bounds.origin.x + px(self.gutter_width() + PAD + col as f32 * m.cell_w),
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

/// 深色 / 浅色对应的终端配色。
pub(crate) fn palette_for(dark: bool) -> Palette {
    if dark {
        Palette::kanso_dark()
    } else {
        Palette::kanso_light()
    }
}

/// 供元素计算行列数。
pub(crate) fn compute_grid(
    width: f32,
    height: f32,
    m: &Metrics,
    gutter_w: f32,
) -> Option<GridSize> {
    grid_size(
        width - gutter_w - 2.0 * PAD,
        height - 2.0 * PAD,
        m.cell_w,
        m.cell_h,
    )
}
