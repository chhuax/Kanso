//! VT 引擎封装、行元数据、帧快照（sans-IO）。
//!
//! 本 crate 是终端内核的基础部分：把会话输出字节喂给 alacritty_terminal 的 VT 解析与网格，
//! 在旁边维护“按绝对行号”的行元数据侧表，并把当前状态写成 `kanso_frame` 的帧快照。
//! 选区、搜索、命令跟踪推断（`track`）、AI CLI 识别（`agent`）与可见帧语义着色（`semantic`）也在这里。
//!
//! 输入编码（按键、鼠标、滚轮、粘贴 → 发往会话的字节）在 `input` 模块，见 `docs/input-encoding.md`。
//!
//! # 数据流
//!
//! ```text
//! 输出字节 ──feed(bytes, now_ms)──▶ OscScanner（切出 OSC 133/777/7/1337/9 的边界）
//!                                     │ 按边界分段
//!                                     ▼
//!                      vte::ansi::Processor ──▶ Core（Handler：转发给 Term，并维护侧表）
//!                                                 │            └─ 行元数据侧表（时间戳、提示符）
//!                                                 ▼
//!                                   alacritty Term：网格 + 回滚
//! take_replies / take_events ◀── 终端回应、OSC 事件
//! snapshot(reuse) ───────────────▶ kanso_frame::Frame（网格 + 侧表 + 光标 + 模式）
//! ```
//!
//! # 批次
//!
//! 一次 [`Terminal::feed`] 调用就是一个“批次”。清屏语义（terminal.md §3）按批次判断：
//! 同一批里 ED 2 与 ED 3 成对出现才是 `clear`；其它批次里单独的 ED 2 视为别的程序在重绘，
//! 会把屏幕推进回滚而不是抹掉。因此调用方应当**把一次读到的字节原样作为一批**喂入，
//! 不要把一次读取拆成多批，也不要把多次读取合并成一批。
//!
//! # 线程与所有权
//!
//! [`Terminal`] 是 `Send` 但不是 `Sync`：它由会话 actor 独占，所有变更方法都需要 `&mut self`。
//! 渲染侧只拿 [`Terminal::snapshot`] 生成的 `Frame`（不可变、可跨线程只读）。
//!
//! # sans-IO
//!
//! 本 crate 不读写网络与文件，也不读系统时钟：时间由调用方通过 `feed` 的 `now_ms`（Unix 毫秒）传入，
//! 同一次 `feed` 里写出的所有新行共用这个时间戳。vte 默认的“同步更新”缓冲会读 `Instant::now()`，
//! 所以被禁用（见 `handler::NoSync`）。
//!
//! # 已知限制（T4a）
//!
//! - DECSET 2026（同步更新）不缓冲：程序在同步更新中间画出的状态可能被渲染出来。
//!   需要时应在呈现层实现（同步期间 `is_dirty` 返回 false，并由调用方传入时钟做超时）。
//! - alacritty 不保存 SGR 5/6（闪烁），所以帧里的 `CELL_BLINK` 恒为 0。
//! - 软换行重排时，同一逻辑行在新宽度下的续行沿用该逻辑行的时间戳，提示符标记只留在第一行。

mod agent;
mod blocks;
mod handler;
mod input;
mod osc;
mod screen;
mod search;
mod selection;
mod semantic;
mod snapshot;
mod table;
mod track;
mod types;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_selsearch;

use alacritty_terminal::grid::Scroll;
use kanso_frame::Frame;
use kanso_types::geometry::GridSize;
use vte::ansi::Processor;

pub use crate::agent::detect_agent;
pub use crate::handler::DEFAULT_WORD_SEPARATORS;
use crate::handler::{Core, NoSync};
pub use crate::input::{
    KeyOptions, OptionAsMeta, Platform, ScrollAction, ShellFlavor, encode_key, encode_mouse,
    encode_paste, encode_scroll, quote_paths,
};
use crate::osc::OscScanner;
pub use crate::search::MAX_MATCHES;
pub use crate::track::{
    AGENT_IDLE_MS, CommandTracker, Effect, InputKind, MAX_COMMAND_CHARS, READ_DELAY_MS, Registered,
};
pub use crate::types::{
    CellRange, CwdReport, MouseMode, MouseOutcome, Overlays, SearchQuery, SearchStatus, TermConfig,
    TermEvent, TermModes, ThemeColors, kitty,
};
pub use kanso_types::geometry::AbsLine;

/// 终端内核。一个会话一个实例，由会话 actor 独占。
pub struct Terminal {
    core: Core,
    processor: Processor<NoSync>,
    scanner: OscScanner,
    dirty: bool,
    seq: u64,
    /// 终端正在处理一次左键拖动选择（见 `selection` 模块文档）。
    drag: bool,
    /// 搜索状态；`Some` 即搜索处于打开状态（见 `search` 模块文档）。
    search: Option<crate::search::SearchState>,
}

impl Terminal {
    /// 创建终端。
    pub fn new(config: TermConfig) -> Self {
        Self {
            core: Core::new(config),
            processor: Processor::new(),
            scanner: OscScanner::new(),
            dirty: true,
            seq: 0,
            drag: false,
            search: None,
        }
    }

    /// 喂入一批已解码为 UTF-8 的输出字节。
    ///
    /// **一次调用就是一个“批次”**，clear 语义按批次判断（见 crate 文档）。`now_ms` 是 Unix 毫秒，
    /// 本批里首次写出的行以它作为时间戳；调用方保证它不随系统时间回拨而乱序（内核不检查）。
    /// 半个 UTF-8 字符、被截断的转义序列都可以跨批：解析状态会保留到下一批。
    pub fn feed(&mut self, bytes: &[u8], now_ms: i64) {
        if bytes.is_empty() {
            return;
        }
        let core = &mut self.core;
        core.now_ms = now_ms;
        core.batch_ed2 = false;
        core.batch_ed3 = false;
        self.dirty = true;

        let mut rest = bytes;
        // 把字节流切在“相关 OSC 的结束位置”：OSC 133 A 必须记录在处理到它那一刻光标所在的行，
        // 所以要先让 vte 处理完这个 OSC 之前（含它）的全部字节，再处理 OSC 本身，然后继续。
        while let Some(end) = self.scanner.scan(rest) {
            self.processor.advance(core, &rest[..end]);
            core.drain_events();
            core.on_osc(self.scanner.done_ps, &self.scanner.body);
            rest = &rest[end..];
        }
        self.processor.advance(core, rest);
        core.drain_events();
        core.check_sync();
        core.prune_blocks();
        self.refresh_search();
    }

    /// 调整网格大小。窄于 2 列或矮于 1 行时夹到最小值。软换行会随之重排，行元数据跟着逻辑行走。
    pub fn resize(&mut self, size: GridSize) {
        self.core.resize(size);
        self.core.drain_events();
        self.core.check_sync();
        self.dirty = true;
        self.refresh_search();
    }

    /// 修改回滚上限；调小时立即丢弃超出的最老的行（terminal.md §15）。
    pub fn set_scrollback_lines(&mut self, lines: usize) {
        self.core.set_scrollback_lines(lines);
        self.core.check_sync();
        self.dirty = true;
        self.refresh_search();
    }

    /// 设置默认光标样式：程序没有用 DECSCUSR 指定样式时使用，DECSCUSR 0 也回到它（terminal.md §15）。
    /// 立即对帧生效；程序已经用 DECSCUSR 指定了非 0 样式时保持程序的选择。
    pub fn set_default_cursor(&mut self, shape: kanso_types::CursorShape, blinking: bool) {
        use vte::ansi::CursorShape as Shape;
        let shape = match shape {
            kanso_types::CursorShape::Block => Shape::Block,
            kanso_types::CursorShape::Underline => Shape::Underline,
            kanso_types::CursorShape::Bar => Shape::Beam,
        };
        self.core.set_default_cursor(shape, blinking);
        self.dirty = true;
    }

    /// 开关可见帧的语义着色，不影响原始网格、选区和复制文本。
    pub fn set_semantic_coloring(&mut self, enabled: bool) {
        if self.core.semantic_enabled != enabled {
            self.core.semantic_enabled = enabled;
            self.dirty = true;
        }
    }

    /// 终端自己产生的回应（DA、CPR、DSR、颜色查询应答、焦点通知等）。
    ///
    /// 调用方原样写回 PTY / SSH 通道。它们**不算用户输入**（terminal.md §5.2）：
    /// 不能据此判断用户在打字、不能开启 AI 轮次。
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.core.replies)
    }

    /// 取走累积的事件，按发生顺序排列。
    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut self.core.events)
    }

    /// 自上次 [`Terminal::snapshot`] 以来是否有变化。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 生成帧快照并清除脏标记。传入上一帧以复用缓冲区（调用方必须保证没有读者还在用它）。
    ///
    /// 内核自己持有的选区与搜索匹配会标到单元格上（`SELECTED` / `MATCH` / `MATCH_CURRENT`）。
    pub fn snapshot(&mut self, reuse: Option<Frame>) -> Frame {
        let selection = self.core.selection_cell_ranges();
        let (matches, current_match) = self.visible_matches();
        self.snapshot_with(
            reuse,
            &Overlays {
                selection: &selection,
                matches: &matches,
                current_match,
            },
        )
    }

    /// 同 [`Terminal::snapshot`]，但用调用方给的叠加层**替换**内核自己的选区与搜索标注
    /// （T4a 预留的底层入口；正常使用 `snapshot` 即可）。
    pub fn snapshot_with(&mut self, reuse: Option<Frame>, overlays: &Overlays<'_>) -> Frame {
        self.seq += 1;
        self.dirty = false;
        self.core.build_frame(reuse, overlays, self.seq)
    }

    /// 滚动显示区域：正数向上（看更早的内容），负数向下。夹在回滚范围内；全屏里没有回滚。
    pub fn scroll_display(&mut self, delta_lines: i32) {
        if self.core.scroll_display(Scroll::Delta(delta_lines)) {
            self.dirty = true;
        }
    }

    /// 回到最底部。
    pub fn scroll_to_bottom(&mut self) {
        if self.core.scroll_display(Scroll::Bottom) {
            self.dirty = true;
        }
    }

    /// ⌘K：清空屏幕、回滚和行元数据，光标回到左上角，显示行号从 1 重新开始。
    ///
    /// 全屏期间调用：只能清掉被盖住的主屏幕的回滚与元数据，主屏幕上已有的文字要到退出全屏后才会被
    /// 程序重绘覆盖（内核读不到被盖住的网格）。
    pub fn clear_buffer(&mut self) {
        self.core.clear_buffer();
        self.drag = false;
        self.dirty = true;
        self.refresh_search();
    }

    /// 设置键盘焦点：影响 `FRAME_FOCUSED` 与空心光标；程序开启了焦点上报时会产生回应（见 `take_replies`）。
    pub fn set_focused(&mut self, focused: bool) {
        if self.core.set_focused(focused) {
            self.dirty = true;
        }
    }

    /// 当前终端模式，供按键编码与 UI 使用。
    pub fn modes(&self) -> TermModes {
        self.core.modes()
    }

    /// 最近一次工作目录上报（含主机）。会话结束或重新连接时由调用方丢弃整个 `Terminal` 重建。
    pub fn cwd(&self) -> Option<&CwdReport> {
        self.core.cwd.as_ref()
    }

    /// 当前标题；从未设置或被重置时为 `None`。
    pub fn title(&self) -> Option<&str> {
        self.core.title.as_deref()
    }

    /// 按帧里 `CellExtra::hyperlink_id` 查 OSC 8 链接的 URL。
    /// 只保证对最近一次 `snapshot` 里出现过的 id 有效（旧 id 在链接表超限清空后会查不到）。
    pub fn hyperlink_url(&self, id: u32) -> Option<&str> {
        self.core.links.url(id)
    }

    /// 设置回应颜色查询用的主题色（主题变化时调用）。
    pub fn set_theme_colors(&mut self, theme: ThemeColors) {
        self.core.theme = theme;
    }

    /// 设置单元格像素尺寸，用于回应 `CSI 14 t`（文本区域像素大小）。默认 8x16。
    pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
        self.core.cell_px = (width, height);
    }

    /// 当前网格尺寸。
    pub fn size(&self) -> GridSize {
        GridSize {
            cols: self.core.cols as u16,
            rows: self.core.rows as u16,
        }
    }

    /// 光标所在行的绝对行号（全屏期间指向被盖住的主屏幕坐标系里的对应行）。
    pub fn cursor_line(&self) -> AbsLine {
        AbsLine(self.core.cursor_abs())
    }

    /// 行元数据侧表当前的行数（测试与诊断用）：恒不超过“回滚上限 + 屏幕行数”。
    pub fn line_meta_len(&self) -> usize {
        self.core.table.len()
    }
}
