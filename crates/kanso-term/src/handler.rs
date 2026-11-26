//! `Core`：包在 alacritty `Term` 外面的 vte `Handler`。
//!
//! 它**把 vte 的每一个 `Handler` 方法都转发给 `Term`**，只在需要的地方先后插入侧表维护逻辑：
//! 行滚动（回滚推进、区域滚动）、ED 清屏语义、写入时间戳、全屏切换、软换行重排。
//! 没有被转发的方法会被 trait 的默认空实现静默吞掉，所以转发清单必须完整；
//! 升级 alacritty_terminal / vte 时的检查项见 workspace `Cargo.toml` 的注释，
//! `tests::forwarding_matches_bare_term` 用一批覆盖所有方法的序列与裸 `Term` 对拍。
//!
//! # 侧表为什么要“镜像”alacritty 的行移动
//!
//! alacritty 不告诉我们“滚出去了几行”（`scroll_up_relative` 是私有的，回滚满了以后历史行数也不再变化，
//! 无法从外部观察）。所以侧表在每个会移动行的入口按 alacritty 的同一套条件做同样的移动。
//! 这些条件（谁会触发滚动、`n` 怎么夹、哪些区域进回滚）都抄自 alacritty_terminal 0.25.1 的
//! `Term::{linefeed, wrapline, put_tab, scroll_up, scroll_down, insert_blank_lines, delete_lines,
//! reverse_index, clear_screen}` 与 `Grid::{scroll_up, scroll_down, clear_viewport}`。
//! `Core::check_sync` 在每批结束时核对侧表行数与网格是否一致，不一致时自愈并计数（测试里要求为 0）。

use std::collections::HashMap;
use std::ops::Range;
use std::sync::mpsc;
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, GridCell, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Flags, Hyperlink as CellLink};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use kanso_types::geometry::{AbsLine, GridSize, Rgb};
use unicode_width::UnicodeWidthChar;
use vte::ansi::cursor_icon::CursorIcon;
use vte::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Handler, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys, NamedPrivateMode,
    PrivateMode, Rgb as VteRgb, ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
    Timeout,
};

use crate::osc::{self, Osc133};
use crate::table::{LineTable, ROW_FIRST, ROW_PROMPT, ROW_WRITTEN};
use crate::types::{CwdReport, MouseMode, TermConfig, TermEvent, TermModes, ThemeColors, kitty};

// alacritty_terminal 0.25.1 的最高位未使用；随网格单元格一起滚动和重排。
pub(crate) const CHIP: Flags = Flags::from_bits_retain(1 << 15);

/// 禁用 vte 的“同步更新（DECSET 2026）”缓冲。
///
/// vte 默认的 `StdSyncHandler` 用 `Instant::now()` 计时，违反 sans-IO（内核不读系统时钟）。
/// `pending_timeout` 恒为 false 后，vte 不再缓冲，2026 序列只当作普通私有模式交给 `Term`（它会忽略）。
/// 代价：同步更新期间的中间状态可能被渲染（可能闪烁）；见 crate 文档“已知限制”。
#[derive(Default)]
pub(crate) struct NoSync;

impl Timeout for NoSync {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// `Term` 的事件出口。用 mpsc 而不是 `Rc<RefCell>`，使 `Terminal` 是 `Send` 但不是 `Sync`。
pub(crate) struct EventSink(pub(crate) mpsc::Sender<Event>);

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

pub(crate) struct Dims {
    pub(crate) cols: usize,
    pub(crate) rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// OSC 8 链接的 id 表：帧里的 `hyperlink_id` 是 u32，URL 由 `hyperlink_url` 按 id 查。
///
/// id 在快照时才分配，并且永不复用（旧帧里的 id 要么查到原来的 URL，要么查不到）。
/// 表超过上限时整体清空（id 计数继续增长），下一帧会为可见单元格重新分配，所以内存有界。
#[derive(Default)]
pub(crate) struct Links {
    ids: HashMap<CellLink, u32>,
    urls: HashMap<u32, String>,
    next: u32,
}

const MAX_LINKS: usize = 16 * 1024;

impl Links {
    pub fn begin_frame(&mut self) {
        if self.ids.len() > MAX_LINKS {
            self.ids.clear();
            self.urls.clear();
        }
    }

    pub fn intern(&mut self, link: &CellLink) -> u32 {
        if let Some(&id) = self.ids.get(link) {
            return id;
        }
        self.next += 1;
        self.ids.insert(link.clone(), self.next);
        self.urls.insert(self.next, link.uri().to_owned());
        self.next
    }

    pub fn url(&self, id: u32) -> Option<&str> {
        self.urls.get(&id).map(String::as_str)
    }
}

/// 用来在重排前后识别同一个逻辑行的内容指纹：非空格字符数 + 第一个与最后一个非空格字符。
/// 重排只改变换行位置，不改变字符，所以同一个逻辑行前后指纹相同。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    n: u32,
    first: char,
    last: char,
}

impl Key {
    fn is_blank(&self) -> bool {
        self.n == 0
    }
}

impl Key {
    /// 重排前的 `self` 是否被截成了重排后的 `new`（除了精确相等之外的另一种“同一个逻辑行”）。
    ///
    /// 要容忍两个边界：网格最老的那个逻辑行可能被回滚上限截掉开头
    /// （保留的末尾字符相同、字符数变少），最新的那个可能被变矮的屏幕裁掉结尾（开头相同、字符数变少）。
    fn partial(&self, new: &Key, is_top: bool, is_bottom: bool) -> bool {
        new.n > 0
            && new.n <= self.n
            && ((is_bottom && new.first == self.first) || (is_top && new.last == self.last))
    }
}

/// 只给最后这么多个逻辑行算指纹：对齐只需要一小段尾部，而且进入全屏时要把它存下来，必须便宜。
const TAIL: usize = 128;

/// 扫描网格：返回每个逻辑行占几行（从最老到最新），以及最后 `TAIL` 个逻辑行的指纹。
fn scan_grid(
    grid: &alacritty_terminal::Grid<alacritty_terminal::term::cell::Cell>,
) -> (Vec<usize>, Vec<Key>) {
    let hist = grid.history_size();
    let total = grid.total_lines();
    let last = Column(grid.columns() - 1);
    let line = |i: usize| Line(i as i32 - hist as i32);
    let mut counts = Vec::new();
    let mut run = 0usize;
    for i in 0..total {
        run += 1;
        if !grid[line(i)][last].flags.contains(Flags::WRAPLINE) {
            counts.push(run);
            run = 0;
        }
    }
    if run > 0 {
        counts.push(run);
    }
    let mut keys = Vec::new();
    let mut end = total;
    for &c in counts.iter().rev().take(TAIL) {
        let mut key = Key {
            n: 0,
            first: ' ',
            last: ' ',
        };
        for i in end - c..end {
            let row = &grid[line(i)];
            for col in 0..grid.columns() {
                let ch = row[Column(col)].c;
                if ch != ' ' && ch != '\t' {
                    if key.n == 0 {
                        key.first = ch;
                    }
                    key.last = ch;
                    key.n += 1;
                }
            }
        }
        keys.push(key);
        end -= c;
    }
    keys.reverse();
    (counts, keys)
}

/// 在重排前后的指纹尾部之间找对齐偏移：新逻辑行 `j` 对应旧逻辑行 `j + s`。
/// 返回 `(s, 锚点)`，锚点是在该偏移下匹配上的、最靠下的非空新逻辑行下标；没有任何匹配返回 `None`。
///
/// 候选偏移来自“最靠下的几个非空新逻辑行”与旧尾部中相同指纹的行；每个候选按整段尾部的匹配数打分，
/// 并列时优先取光标锚点给出的 `hint`（它在大多数情形下是对的，只是不保证）。
fn choose_shift(
    old_keys: &[Key],
    n_old: usize,
    new_keys: &[Key],
    n_new: usize,
    hint: Option<isize>,
) -> Option<(isize, usize)> {
    let old_base = n_old - old_keys.len();
    let new_base = n_new - new_keys.len();
    let old_at = |j: isize| -> Option<&Key> {
        let j = usize::try_from(j).ok()?;
        old_keys.get(j.checked_sub(old_base)?)
    };
    let score = |s: isize| -> (usize, Option<usize>) {
        let mut n = 0;
        let mut anchor = None;
        for (k, key) in new_keys.iter().enumerate() {
            if key.is_blank() {
                continue;
            }
            let j = (new_base + k) as isize;
            if old_at(j + s)
                .is_some_and(|o| o == key || o.partial(key, j == 0, new_base + k == n_new - 1))
            {
                n += 1;
                anchor = Some(new_base + k);
            }
        }
        (n, anchor)
    };
    let mut candidates: Vec<isize> = hint.into_iter().collect();
    let mut tried = 0;
    for (k, key) in new_keys.iter().enumerate().rev() {
        if key.is_blank() {
            continue;
        }
        tried += 1;
        if tried > 8 {
            break;
        }
        for (o, ok) in old_keys.iter().enumerate().rev() {
            if ok == key {
                let s = (old_base + o) as isize - (new_base + k) as isize;
                if !candidates.contains(&s) {
                    candidates.push(s);
                }
            }
        }
    }
    let mut best: Option<(usize, isize, usize)> = None;
    for s in candidates {
        let (n, anchor) = score(s);
        if let Some(anchor) = anchor
            && best.is_none_or(|(bn, _, _)| n > bn)
        {
            best = Some((n, s, anchor));
        }
    }
    best.map(|(_, s, a)| (s, a))
}

/// 在 `choose_shift` 给出的整体偏移 `s` 基础上，逐个逻辑行对齐尾部：返回每个新逻辑行对应的旧逻辑行。
///
/// 只靠一个整体偏移不够：alacritty 重排时会在光标附近增删空行（光标行号被夹在屏幕边缘时
/// 尤其明显），偏移会在中途变化。所以非空行按“内容指纹相同”顺序配对，空行依次吃掉旧的空行，
/// 多出来的空行没有来源（元数据为空）。尾部之外的更早的行没有指纹，沿用尾部最上面一个配对的偏移。
fn align(
    old_keys: &[Key],
    n_old: usize,
    new_keys: &[Key],
    n_new: usize,
    s: isize,
) -> Vec<Option<usize>> {
    let old_base = n_old - old_keys.len();
    let new_base = n_new - new_keys.len();
    let mut map = vec![None; n_new];
    let mut p = (new_base as isize + s - 2).max(old_base as isize) as usize;
    for (k, key) in new_keys.iter().enumerate() {
        if p >= n_old {
            break;
        }
        let j = new_base + k;
        if key.is_blank() {
            if old_keys[p - old_base].is_blank() {
                map[j] = Some(p);
                p += 1;
            }
        } else if let Some(q) = expected_match(old_keys, old_base, n_old, key, j, n_new, s, p)
            .or_else(|| (p..n_old).find(|&q| old_keys[q - old_base] == *key))
        {
            map[j] = Some(q);
            p = q + 1;
        } else {
            // 精确匹配不上时，下一个没被消费的旧逻辑行如果开头相同、字符数相差不多，也认作同一行：
            // 重排会丢掉恰好落在边缘的宽字符（光标所在行尤其常见）。
            let o = &old_keys[p - old_base];
            if o.first == key.first && o.n.abs_diff(key.n) <= 3 {
                map[j] = Some(p);
                p += 1;
            }
        }
    }
    let top_shift = map
        .iter()
        .enumerate()
        .find_map(|(j, o)| o.map(|o| o as isize - j as isize))
        .unwrap_or(s);
    for (j, slot) in map.iter_mut().enumerate().take(new_base) {
        *slot = usize::try_from(j as isize + top_shift)
            .ok()
            .filter(|&o| o < n_old);
    }
    map
}

/// 整体偏移 `s` 预测的位置上的旧逻辑行是否就是 `key`（精确相等，或首尾被截断的近似相等）。
#[allow(clippy::too_many_arguments)]
fn expected_match(
    old_keys: &[Key],
    old_base: usize,
    n_old: usize,
    key: &Key,
    j: usize,
    n_new: usize,
    s: isize,
    p: usize,
) -> Option<usize> {
    let q = usize::try_from(j as isize + s).ok()?;
    if q < p || q >= n_old || q < old_base {
        return None;
    }
    let o = &old_keys[q - old_base];
    (o == key || o.partial(key, j == 0, j == n_new - 1)).then_some(q)
}

/// 事件队列上限，防止调用方不取事件时被恶意输出（标题刷屏等）撑爆内存。
const MAX_EVENTS: usize = 4096;
/// 表示“没有缓存”的屏幕行号。
const NO_LINE: i32 = -1;

/// 默认的选词分隔字符（双击选词），见 `Terminal::set_word_separators`。
///
/// 空白、引号、括号、`|`、`,`、`;`、`:`、框线字符 `│` 作为分隔；路径里常见的 `/ . - _ ~`、
/// 以及 `= @ # % + ?` 等**不**作为分隔，这样双击能选中整个路径、URL 或 `KEY=value`。
/// `:` 作为分隔是为了在 `file.rs:12:3` 里只选中文件名。
pub const DEFAULT_WORD_SEPARATORS: &str = " \t,;:|│`'\"()[]{}<>";

pub(crate) fn alacritty_config(scrollback: usize) -> Config {
    alacritty_config_with(scrollback, DEFAULT_WORD_SEPARATORS)
}

pub(crate) fn alacritty_config_with(scrollback: usize, separators: &str) -> Config {
    Config {
        scrolling_history: scrollback,
        // `set_options` 会整体替换配置，所以分隔字符必须每次随回滚上限一起传入，否则会被改回默认值。
        semantic_escape_chars: separators.to_owned(),
        // 内核要支持 kitty 键盘协议（terminal.md §9）：由程序主动开启，模式位由 modes() 暴露。
        kitty_keyboard: true,
        // 只允许程序写剪贴板（OSC 52 store）；读剪贴板会把用户剪贴板内容泄露给远端程序。
        osc52: Osc52::OnlyCopy,
        ..Config::default()
    }
}

pub(crate) struct Core {
    chip_active: bool,
    pub semantic: crate::semantic::SemanticColors,
    pub semantic_enabled: bool,
    pub blocks: crate::blocks::Blocks,
    pub term: Term<EventSink>,
    rx: mpsc::Receiver<Event>,
    pub table: LineTable,
    pub links: Links,
    pub rows: usize,
    pub cols: usize,
    /// 滚动区域（相对屏幕顶的行），镜像 `Term::scroll_region`（它是私有的）。
    region: Range<usize>,
    pub cap: usize,
    /// 是否在全屏（alternate screen）。全屏期间侧表冻结：它属于被盖住的主屏幕。
    pub alt: bool,
    /// 全屏期间发生过 resize：主屏幕被（看不见地）重排了，退出全屏时要重建侧表。
    alt_resized: bool,
    /// 进入全屏时主屏幕光标所在行的绝对行号：退出全屏重建侧表时用它作锚点。
    alt_cursor_abs: u64,
    alt_block_columns: crate::blocks::ReflowColumns,
    /// 进入全屏时主屏幕最后 `TAIL` 个逻辑行的内容指纹。
    alt_keys: Vec<Key>,
    pub now_ms: i64,
    /// 已知“写过内容”的那一行的绝对行号（热路径缓存，见 `stamp`）。
    stamped_line: i32,
    pub batch_ed2: bool,
    pub batch_ed3: bool,
    /// 显示行号 = abs - base + 1。
    pub line_number_base: u64,
    pub events: Vec<TermEvent>,
    pub replies: Vec<u8>,
    pub cwd: Option<CwdReport>,
    pub title: Option<String>,
    pub theme: ThemeColors,
    pub cell_px: (u16, u16),
    pub focused: bool,
    /// 侧表与网格对不上而触发自愈的次数；正常情况恒为 0，测试断言它。
    pub resync_count: u32,
    #[cfg(test)]
    pub full_remaps: u32,
    #[cfg(test)]
    pub search_rows: std::cell::Cell<usize>,
    /// 选词分隔字符，见 `alacritty_config_with`。
    pub separators: String,
    /// 程序没有用 DECSCUSR 指定样式时的默认光标样式（显示设置，terminal.md §15）。
    /// 必须每次随 `set_options` 一起传入：`set_options` 整体替换配置，漏传会被改回 alacritty 的默认值。
    pub default_cursor: CursorStyle,
    /// 历史内容被“非追加方式”改动的计数（ED 3、⌘K、RIS、重排、调回滚上限）。
    /// 搜索靠它判断已搜索过的回滚行是否还可信：追加新行不会改变它，只需重搜屏幕附近的行。
    pub epoch: u64,
    /// 整屏清除（`clear` 的 ED 2 + ED 3 成对、⌘K、RIS）发生的次数，只增不减。
    /// 与 `epoch` 不同：单独的 ED 3 或调回滚上限不算，因为那时屏幕上的内容还在；
    /// MCP 的 `kanso_terminal_run` 据此判断"命令执行期间是否清过屏"（spec/mcp.md §4.2）。
    pub clears: u64,
}

impl Core {
    pub fn new(config: TermConfig) -> Self {
        let cols = usize::from(config.size.cols).max(2);
        let rows = usize::from(config.size.rows).max(1);
        let (tx, rx) = mpsc::channel();
        let term = Term::new(
            alacritty_config(config.scrollback_lines),
            &Dims { cols, rows },
            EventSink(tx),
        );
        Self {
            semantic: crate::semantic::SemanticColors::default(),
            chip_active: false,
            semantic_enabled: true,
            blocks: crate::blocks::Blocks::default(),
            term,
            rx,
            table: LineTable::new(rows),
            links: Links::default(),
            rows,
            cols,
            region: 0..rows,
            cap: config.scrollback_lines,
            alt: false,
            alt_resized: false,
            alt_cursor_abs: 0,
            alt_block_columns: crate::blocks::ReflowColumns::default(),
            alt_keys: Vec::new(),
            now_ms: 0,
            stamped_line: NO_LINE,
            batch_ed2: false,
            batch_ed3: false,
            line_number_base: 0,
            events: Vec::new(),
            replies: Vec::new(),
            cwd: None,
            title: None,
            theme: ThemeColors::default(),
            cell_px: (8, 16),
            focused: false,
            resync_count: 0,
            #[cfg(test)]
            full_remaps: 0,
            #[cfg(test)]
            search_rows: std::cell::Cell::new(0),
            separators: DEFAULT_WORD_SEPARATORS.to_owned(),
            default_cursor: CursorStyle::default(),
            epoch: 0,
            clears: 0,
        }
    }

    // ---------------------------------------------------------------- 基础

    #[inline]
    fn cursor_line(&self) -> usize {
        self.term.grid().cursor.point.line.0 as usize
    }

    /// 光标所在行的绝对行号（主屏幕）。
    pub fn cursor_abs(&self) -> u64 {
        self.table.screen_top_abs() + self.cursor_line() as u64
    }

    fn push_event(&mut self, ev: TermEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(ev);
    }

    pub fn modes(&self) -> TermModes {
        let m = *self.term.mode();
        let mouse = if m.contains(TermMode::MOUSE_MOTION) {
            MouseMode::Motion
        } else if m.contains(TermMode::MOUSE_DRAG) {
            MouseMode::Drag
        } else if m.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseMode::Click
        } else {
            MouseMode::None
        };
        let mut k = 0;
        for (bit, flag) in [
            (TermMode::DISAMBIGUATE_ESC_CODES, kitty::DISAMBIGUATE),
            (TermMode::REPORT_EVENT_TYPES, kitty::REPORT_EVENT_TYPES),
            (
                TermMode::REPORT_ALTERNATE_KEYS,
                kitty::REPORT_ALTERNATE_KEYS,
            ),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, kitty::REPORT_ALL_KEYS),
            (
                TermMode::REPORT_ASSOCIATED_TEXT,
                kitty::REPORT_ASSOCIATED_TEXT,
            ),
        ] {
            if m.contains(bit) {
                k |= flag;
            }
        }
        TermModes {
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            app_cursor: m.contains(TermMode::APP_CURSOR),
            app_keypad: m.contains(TermMode::APP_KEYPAD),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse,
            mouse_sgr: m.contains(TermMode::SGR_MOUSE),
            mouse_utf8: m.contains(TermMode::UTF8_MOUSE),
            focus_reporting: m.contains(TermMode::FOCUS_IN_OUT),
            alternate_scroll: m.contains(TermMode::ALTERNATE_SCROLL),
            line_wrap: m.contains(TermMode::LINE_WRAP),
            cursor_visible: m.contains(TermMode::SHOW_CURSOR),
            line_feed_new_line: m.contains(TermMode::LINE_FEED_NEW_LINE),
            kitty_keyboard: k,
        }
    }

    // ---------------------------------------------------------------- 事件与回应

    /// 取走 `Term` 产生的事件，转成回应字节与 `TermEvent`。
    pub fn drain_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle_event(ev);
        }
    }

    fn handle_event(&mut self, ev: Event) {
        match ev {
            // DA / DSR / CPR / DECRQM 等：终端自己的回应，原样写回，不算用户输入（terminal.md §5.2）。
            Event::PtyWrite(s) => self.replies.extend_from_slice(s.as_bytes()),
            Event::Title(t) => {
                self.title = Some(t);
                self.push_event(TermEvent::TitleChanged);
            }
            Event::ResetTitle => {
                self.title = None;
                self.push_event(TermEvent::TitleChanged);
            }
            Event::Bell => {
                if self.events.last() != Some(&TermEvent::Bell) {
                    self.push_event(TermEvent::Bell);
                }
            }
            Event::ClipboardStore(_, text) => self.push_event(TermEvent::ClipboardStore(text)),
            Event::ColorRequest(idx, fmt) => {
                let c = self.color_at(idx);
                self.replies.extend_from_slice(fmt(c).as_bytes());
            }
            Event::TextAreaSizeRequest(fmt) => {
                let ws = WindowSize {
                    num_lines: self.rows as u16,
                    num_cols: self.cols as u16,
                    cell_width: self.cell_px.0,
                    cell_height: self.cell_px.1,
                };
                self.replies.extend_from_slice(fmt(ws).as_bytes());
            }
            // 剪贴板读取被配置禁用；其余只是给 UI 的提示，帧快照每次都重新读取状态。
            _ => {}
        }
    }

    /// 颜色查询的应答值：程序用 OSC 4/10/11 设置过的优先，否则用主题色 / xterm 默认调色板。
    fn color_at(&self, idx: usize) -> VteRgb {
        if idx < 269
            && let Some(c) = self.term.colors()[idx]
        {
            return c;
        }
        let t = |c: Rgb| VteRgb {
            r: c.r,
            g: c.g,
            b: c.b,
        };
        match idx {
            0..=15 => t(self.theme.ansi[idx]),
            16..=231 => {
                let i = idx - 16;
                let lv = |n: usize| if n == 0 { 0 } else { (55 + 40 * n) as u8 };
                VteRgb {
                    r: lv(i / 36),
                    g: lv(i / 6 % 6),
                    b: lv(i % 6),
                }
            }
            232..=255 => {
                let v = (8 + 10 * (idx - 232)) as u8;
                VteRgb { r: v, g: v, b: v }
            }
            257 | 268 => t(self.theme.background),
            259..=266 => t(self.theme.ansi[idx - 259]),
            _ => t(self.theme.foreground),
        }
    }

    // ---------------------------------------------------------------- OSC（扫描器派发）

    pub fn on_osc(&mut self, ps: u32, body: &[u8]) {
        match ps {
            777 => match body {
                b"kanso-chip;start" => self.chip_active = true,
                b"kanso-chip;end" => self.end_chip(),
                _ => {}
            },
            // 全屏程序里的 OSC 133 不属于我们的 shell 集成，忽略（侧表此时也冻结着）。
            133 if !self.alt => {
                if let Some(o) = osc::parse_133(body) {
                    self.on_133(o);
                }
            }
            7 => self.report_cwd(osc::parse_osc7(body)),
            1337 => self.report_cwd(osc::parse_osc1337(body)),
            9 => self.report_cwd(osc::parse_osc9(body)),
            _ => {}
        }
    }

    fn end_chip(&mut self) {
        self.chip_active = false;
        self.term.grid_mut().cursor.template.flags.remove(CHIP);
    }

    fn report_cwd(&mut self, r: Option<CwdReport>) {
        if let Some(r) = r {
            self.cwd = Some(r.clone());
            self.push_event(TermEvent::CwdChanged(r));
        }
    }

    fn on_133(&mut self, o: Osc133) {
        if matches!(o, Osc133::PromptStart { .. }) {
            self.end_chip();
        }
        if self.alt {
            return;
        }
        self.on_block_marker(&o);
        let abs = self.cursor_abs();
        match o {
            Osc133::PromptStart { first } => {
                let Some(m) = self.table.get_mut(abs) else {
                    return;
                };
                // zsh 重绘提示符会在同一行重复发 A：同一行只记一次，否则分隔线会叠加变亮（terminal.md §4）。
                if m.flags & ROW_PROMPT != 0 {
                    if first {
                        m.flags |= ROW_FIRST;
                    }
                    return;
                }
                m.flags |= ROW_PROMPT | if first { ROW_FIRST } else { 0 };
                self.push_event(TermEvent::PromptStart {
                    line: AbsLine(abs),
                    first,
                });
            }
            Osc133::InputStart => self.push_event(TermEvent::InputStart { line: AbsLine(abs) }),
            Osc133::CommandStart => self.push_event(TermEvent::CommandStart { line: AbsLine(abs) }),
            Osc133::CommandFinished { exit_code } => {
                self.push_event(TermEvent::CommandFinished { exit_code })
            }
        }
    }

    // ---------------------------------------------------------------- 写入时间戳

    /// 给光标所在行记“首次写出时间”。必须在 `Term::input` 之后调用：此时光标行就是刚写入的那一行
    /// （换行发生在写入之前；写满最后一列后光标留在原行等待下一次换行）。
    ///
    /// `stamped_line` 缓存“最近确认过已写过”的屏幕行号，使同一行里的连续字符只比较一个整数
    /// （这是逐字符的热路径，terminal.md §17 要求吞吐下降不超过 10%）。凡是会让屏幕行与侧表行的
    /// 对应关系改变的操作——任何滚动（包括全屏换行：光标行号不变但下面的行换了）、重置、重排——
    /// 都必须把它清成 `NO_LINE`，否则会把时间戳记到错误的行上。
    #[inline]
    fn stamp(&mut self) {
        let line = self.term.grid().cursor.point.line.0;
        if line != self.stamped_line {
            self.stamp_slow(line);
        }
    }

    #[cold]
    fn stamp_slow(&mut self, line: i32) {
        let abs = self.table.screen_top_abs() + line as u64;
        if let Some(m) = self.table.get_mut(abs) {
            if m.flags & ROW_WRITTEN == 0 {
                m.flags |= ROW_WRITTEN;
                m.ts_ms = self.now_ms;
            }
            self.stamped_line = line;
        }
    }

    // ---------------------------------------------------------------- 行滚动镜像

    /// 镜像 `Term::scroll_up_relative`：`origin` 起到区域末尾的区域上滚 `lines` 行。
    fn table_scroll_up(&mut self, origin: usize, lines: usize) {
        if self.alt {
            return;
        }
        self.stamped_line = NO_LINE;
        // 热路径：整屏区域上滚一行（普通输出的每个换行），只在表尾追加。
        if origin == 0 && self.region.start == 0 && self.region.end == self.rows && lines == 1 {
            self.table.push_line(self.cap);
            return;
        }
        // alacritty 把 lines 夹到“整个滚动区域”的大小，而不是 origin..end。
        let lines = lines.min(self.region.end - self.region.start);
        self.table
            .scroll_up(origin, self.region.end, lines, self.cap);
    }

    /// 镜像 `Term::scroll_down_relative`。
    fn table_scroll_down(&mut self, origin: usize, lines: usize) {
        if self.alt {
            return;
        }
        let lines = lines
            .min(self.region.end - self.region.start)
            .min(self.region.end - origin);
        self.table.scroll_down(origin, self.region.end, lines);
        self.stamped_line = NO_LINE;
    }

    // ---------------------------------------------------------------- 清除

    /// ED 2 会推进回滚的行数：屏幕上最后一个非空行之前的行数（镜像 `Grid::clear_viewport`）。
    fn positions_to_push(&self) -> usize {
        let grid = self.term.grid();
        for r in (0..self.rows).rev() {
            let row = &grid[Line(r as i32)];
            if (0..self.cols).any(|c| !row[Column(c)].is_empty()) {
                return r + 1;
            }
        }
        // 屏幕全空时 alacritty 往回数会一直数到网格最顶端：有回滚时停在回滚第一行（得 0），
        // 没有回滚时数到屏幕第 0 行就没有更多的格子了，得到的是 1——于是会白白推一个空行进回滚。
        // 这是它的行为，侧表必须照做，否则行数对不上。
        if grid.history_size() == 0 { 1 } else { 0 }
    }

    /// `clear` 完成：显示行号从光标所在行重新从 1 开始。AbsLine 本身不回退，只移动基准。
    fn finish_clear(&mut self) {
        self.clears += 1;
        self.blocks.clear();
        self.line_number_base = self.cursor_abs();
        self.batch_ed2 = false;
        self.batch_ed3 = false;
    }

    fn clear_screen_main(&mut self, mode: ClearMode) {
        let c = self.cursor_line();
        let region_full = self.region == (0..self.rows);
        match mode {
            ClearMode::Below => {
                Handler::clear_screen(&mut self.term, mode);
                // 光标行本身只擦一部分，保留它的元数据；其下整行被擦掉。
                self.table.reset_screen_rows(c + 1, self.rows);
            }
            ClearMode::Above => {
                Handler::clear_screen(&mut self.term, mode);
                // 与 alacritty 一致：它只在 cursor.line > 1 时才整行清除上方各行。
                if c > 1 {
                    self.table.reset_screen_rows(0, c);
                }
            }
            ClearMode::All => {
                if self.batch_ed3 || !region_full {
                    // 直接擦掉而不是推进回滚：
                    // - 同一批里 ED 3 已经先清掉了回滚（macOS 的 `clear` 是先 3 后 2），
                    //   再让 alacritty 把屏幕推进回滚就会把刚清掉的内容又放回来；
                    // - 带滚动区域时按普通擦除处理（terminal.md §3）。
                    self.term.grid_mut().reset_region(..);
                    // 这条路径绕过了 `Term::clear_screen`，选区不会被它清掉：屏幕上的内容没了，
                    // 落在屏幕里的选区必须清除；已在回滚里的部分内容没变，保留。
                    let screen = Line(0)..Line(self.rows as i32);
                    self.term.selection = self
                        .term
                        .selection
                        .take()
                        .filter(|s| !s.intersects_range(screen.clone()));
                    self.table.reset_screen_rows(0, self.rows);
                    if self.batch_ed3 && region_full {
                        self.finish_clear();
                    }
                } else {
                    // 只有 ED 2：alacritty 的 `clear_viewport` 会把屏幕内容推进回滚而不是抹掉，
                    // 正是 terminal.md §3 要求的语义，所以交给它做，侧表同步推进。
                    // 必须在这一刻立即执行（top 会在同一批里先清屏再画第一帧，事后补做会抹掉第一帧）。
                    let p = self.positions_to_push();
                    // `Term::clear_screen(All)` 无条件清掉选区；但这里屏幕只是被推进回滚、内容还在，
                    // 选区应当跟着内容走。`clear_viewport` 就是整屏区域上滚 `p` 行，所以用同样的
                    // rotate 把选区补回去（与 `Term::scroll_up_relative` 对选区的处理一致）。
                    let saved_sel = self.term.selection.take();
                    Handler::clear_screen(&mut self.term, mode);
                    let region = Line(0)..Line(self.rows as i32);
                    self.term.selection =
                        saved_sel.and_then(|s| s.rotate(&self.term, &region, p as i32));
                    self.table.push_screen(p, self.cap);
                    // “先 2 后 3”的 clear：ED 3 到来时再清回滚，两次各自立即执行，天然得到 clear 的效果。
                    self.batch_ed2 = true;
                }
            }
            ClearMode::Saved => {
                Handler::clear_screen(&mut self.term, mode);
                self.epoch += 1;
                self.table.clear_history();
                self.batch_ed3 = true;
                if self.batch_ed2 && region_full {
                    self.finish_clear();
                }
            }
        }
        self.stamped_line = NO_LINE;
    }

    /// ⌘K：清空屏幕、回滚和行元数据，光标回到左上角。
    pub fn clear_buffer(&mut self) {
        self.clears += 1;
        self.blocks.clear();
        self.drain_events();
        // 先清回滚：primary 即使被全屏盖住，`set_options` 也会对它生效（`grid_mut` 则只能碰到当前屏幕）。
        self.apply_scrollback(0);
        self.apply_scrollback(self.cap);
        self.term.grid_mut().reset_region(..);
        // 内容全部抹掉（绕过了 `Term` 的清除路径），选区与已搜索的历史都作废。
        self.term.selection = None;
        self.epoch += 1;
        Handler::goto(&mut self.term, 0, 0);
        self.table.clear_history();
        self.table.reset_screen_rows(0, self.table.screen_rows());
        self.stamped_line = NO_LINE;
        self.batch_ed2 = false;
        self.batch_ed3 = false;
        self.line_number_base = self.table.screen_top_abs();
    }

    /// 修改 alacritty 的回滚上限。`set_options` 会重发标题事件，这里吞掉它，避免误报 `TitleChanged`。
    fn apply_scrollback(&mut self, lines: usize) {
        let mut config = alacritty_config_with(lines, &self.separators);
        config.default_cursor_style = self.default_cursor;
        self.term.set_options(config);
        while let Ok(ev) = self.rx.try_recv() {
            if !matches!(ev, Event::Title(_) | Event::ResetTitle) {
                self.handle_event(ev);
            }
        }
    }

    /// 把 `self.separators` 应用到 `Term`（`set_options` 是唯一的设置途径）。
    pub fn set_word_separators(&mut self) {
        self.drain_events();
        self.apply_scrollback(self.cap);
    }

    /// 修改默认光标样式。已经被 DECSCUSR 指定过样式的程序不受影响；DECSCUSR 0 会回到这里设置的样式。
    pub fn set_default_cursor(&mut self, shape: CursorShape, blinking: bool) {
        self.drain_events();
        self.default_cursor = CursorStyle { shape, blinking };
        self.apply_scrollback(self.cap);
    }

    pub fn set_scrollback_lines(&mut self, lines: usize) {
        self.drain_events();
        self.cap = lines;
        self.epoch += 1;
        self.apply_scrollback(lines);
        // 调小时立即丢弃最老的行（terminal.md §15）；全屏期间表属于被盖住的主屏幕，同样处理。
        self.table.trim_history(lines);
        self.prune_blocks();
    }

    pub fn scroll_display(&mut self, scroll: Scroll) -> bool {
        if self.has_folded() {
            let (_, before, max_off) = self.viewport_map();
            let next = match scroll {
                Scroll::Delta(d) => {
                    (before as i64 + i64::from(d)).clamp(0, max_off as i64) as usize
                }
                Scroll::PageUp => before.saturating_add(self.rows).min(max_off),
                Scroll::PageDown => before.saturating_sub(self.rows),
                Scroll::Top => max_off,
                Scroll::Bottom => 0,
            };
            self.blocks.fold_offset = next;
            return next != before;
        }
        let before = self.term.grid().display_offset();
        self.term.scroll_display(scroll);
        self.drain_events();
        self.term.grid().display_offset() != before
    }

    // ---------------------------------------------------------------- 全屏切换

    fn sync_wrapped_from_grid(&mut self) {
        let hist = self.table.hist();
        let last = Column(self.cols - 1);
        let grid = self.term.grid();
        self.table.sync_wrapped(|i| {
            grid[Line(i as i32 - hist as i32)][last]
                .flags
                .contains(Flags::WRAPLINE)
        });
    }

    fn enter_alt(&mut self, mode: PrivateMode) {
        // 进入前把主屏幕的软换行分组记进侧表：全屏期间主网格读不到，
        // 而全屏里发生 resize 时要靠它重建逻辑行。
        self.sync_wrapped_from_grid();
        self.alt_cursor_abs = self.cursor_abs();
        self.alt_block_columns =
            crate::blocks::ReflowColumns::capture(self.term.grid(), self.table.first_abs());
        self.alt_keys = scan_grid(self.term.grid()).1;
        Handler::set_private_mode(&mut self.term, mode);
        self.alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.alt_resized = false;
        self.stamped_line = NO_LINE;
    }

    fn leave_alt(&mut self, mode: PrivateMode) {
        Handler::unset_private_mode(&mut self.term, mode);
        self.alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.stamped_line = NO_LINE;
        if !self.alt && self.alt_resized {
            self.alt_resized = false;
            // 全屏期间主网格被（看不见地）重排过：用进入时记下的内容指纹和光标行对齐。
            // 回滚满了又缩窄时，指纹只覆盖最后 TAIL 个逻辑行，更早的行按同一偏移推算。
            let old = self.table.groups();
            let old_idx = self.alt_cursor_abs.saturating_sub(self.table.first_abs()) as usize;
            let old_cg = old
                .partition_point(|g| g.start <= old_idx)
                .saturating_sub(1);
            let keys = std::mem::take(&mut self.alt_keys);
            let columns = std::mem::take(&mut self.alt_block_columns);
            self.remap_with(
                old,
                &keys,
                Some((old_cg as isize, self.alt_cursor_abs)),
                columns,
            );
        }
    }

    // ---------------------------------------------------------------- resize

    pub fn resize(&mut self, size: GridSize) {
        let cols = usize::from(size.cols).max(2);
        let rows = usize::from(size.rows).max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.drain_events();
        if cols == self.cols {
            if !self.alt || !self.alt_resized {
                let cursor = if self.alt {
                    self.alt_cursor_abs
                        .saturating_sub(self.table.screen_top_abs()) as usize
                } else {
                    self.cursor_line()
                };
                self.table.resize_rows(rows, cursor, self.cap);
                if self.alt {
                    self.alt_keys.clear();
                }
                self.prune_blocks();
            }
            self.term.resize(Dims { cols, rows });
            self.rows = rows;
            self.region = 0..rows;
            self.stamped_line = NO_LINE;
            return;
        }
        self.epoch += 1;
        if self.alt {
            self.term.resize(Dims { cols, rows });
            self.alt_resized = true;
        } else {
            self.sync_wrapped_from_grid();
            let old_groups = self.table.groups();
            let columns =
                crate::blocks::ReflowColumns::capture(self.term.grid(), self.table.first_abs());
            let (_, old_keys) = scan_grid(self.term.grid());
            let old_idx = self.table.hist() + self.cursor_line();
            let old_cg = old_groups
                .partition_point(|g| g.start <= old_idx)
                .saturating_sub(1);
            let old_abs = self.table.first_abs() + old_idx as u64;
            self.term.resize(Dims { cols, rows });
            self.cols = cols;
            self.rows = rows;
            self.region = 0..rows;
            self.remap_with(
                old_groups,
                &old_keys,
                Some((old_cg as isize, old_abs)),
                columns,
            );
            self.stamped_line = NO_LINE;
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.region = 0..rows;
        self.stamped_line = NO_LINE;
    }

    /// 重排后重建侧表：把旧逻辑行的元数据搬到新网格的逻辑行上。
    ///
    /// 难点是“新逻辑行 k 对应旧逻辑行几号”。重排保持逻辑行的顺序，但两端都可能丢行：
    /// 回滚满了时 `shrink_columns` 从队首截掉最老的行；变矮时 `shrink_lines` 裁掉光标以下的行；
    /// 变高时又会在队尾补空行。最直观的锚点——光标——并不可靠：alacritty 在光标靠近屏幕顶部、
    /// 其上方的行被拆成更多行时，会把光标行号夹在 0，光标就指到了另一个逻辑行上。
    ///
    /// 所以以**内容指纹**对齐：重排不改变一个逻辑行里的字符，按逻辑行统计“非空格字符数 + 首尾字符”
    /// 即可在重排前后识别同一个逻辑行（见 [`choose_shift`]）。找不到任何匹配（例如全是空行）
    /// 才退回到光标锚点，再退回到从队首对齐。
    fn remap_with(
        &mut self,
        old: Vec<crate::table::Group>,
        old_keys: &[Key],
        cursor_fallback: Option<(isize, u64)>,
        old_columns: crate::blocks::ReflowColumns,
    ) {
        #[cfg(test)]
        {
            self.full_remaps += 1;
        }
        let grid = self.term.grid();
        let cursor_idx = grid.history_size() + grid.cursor.point.line.0 as usize;
        let (counts, new_keys) = scan_grid(grid);
        let mut cursor_group = 0;
        let mut acc = 0;
        for (g, &c) in counts.iter().enumerate() {
            if cursor_idx < acc + c {
                cursor_group = g;
                break;
            }
            acc += c;
        }
        let hint = cursor_fallback.map(|(old_cg, _)| old_cg - cursor_group as isize);
        let (map, first_abs) =
            match choose_shift(old_keys, old.len(), &new_keys, counts.len(), hint) {
                Some((s, anchor)) => {
                    let map = align(old_keys, old.len(), &new_keys, counts.len(), s);
                    // 保持最靠下的、对上了的那个逻辑行的绝对行号不变。
                    let j = map.iter().rposition(Option::is_some).unwrap_or(anchor);
                    let o = map[j].unwrap_or((anchor as isize + s) as usize);
                    let rows_before: usize = counts[..j].iter().sum();
                    let old_row = old[o].start as u64;
                    (
                        map,
                        (self.table.first_abs() + old_row).saturating_sub(rows_before as u64),
                    )
                }
                None => {
                    let (delta, first_abs) = match cursor_fallback {
                        Some((old_cg, old_abs)) => (
                            old_cg - cursor_group as isize,
                            old_abs.saturating_sub(cursor_idx as u64),
                        ),
                        None => (0, self.table.first_abs()),
                    };
                    let map = (0..counts.len())
                        .map(|k| usize::try_from(k as isize + delta).ok())
                        .collect();
                    (map, first_abs)
                }
            };
        // 只有尾部有指纹；尾部之外的行没有来源时按“空”处理（它们要么匹配上了，要么早就被挤出了回滚）。
        let new_base = counts.len() - new_keys.len();
        let has_text: Vec<bool> = (0..counts.len())
            .map(|k| k >= new_base && !new_keys[k - new_base].is_blank())
            .collect();
        let new_columns = crate::blocks::ReflowColumns::capture(self.term.grid(), first_abs);
        self.blocks.remap(
            &old,
            self.table.first_abs(),
            (&old_columns, &new_columns),
            &counts,
            &map,
            first_abs,
        );
        self.table
            .remap(&old, &counts, &map, &has_text, first_abs, self.rows);
        self.prune_blocks();
    }

    /// 每批结束后核对侧表与网格行数。不一致说明镜像逻辑漏了某种行移动：自愈并计数。
    pub fn check_sync(&mut self) {
        if self.alt {
            return;
        }
        let total = self.term.grid().total_lines();
        if self.table.heal(total) {
            self.resync_count += 1;
            self.stamped_line = NO_LINE;
        }
    }

    pub fn set_focused(&mut self, focused: bool) -> bool {
        if self.focused == focused {
            return false;
        }
        self.focused = focused;
        self.term.is_focused = focused;
        if self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
            // 焦点通知是终端发给程序的回应，不算用户输入（terminal.md §5.2）。
            self.replies
                .extend_from_slice(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        true
    }

    // ---------------------------------------------------------------- 输入的慢路径

    #[cold]
    fn input_slow(&mut self, c: char) {
        if !self.alt {
            let width = c.width();
            let wrap_mode = self.term.mode().contains(TermMode::LINE_WRAP);
            let cur = &self.term.grid().cursor;
            // 镜像 `Term::input`：需要换行的情形有两种——上一个字符写满了最后一列（input_needs_wrap），
            // 或者这是个宽字符而光标在最后一列。零宽 / 无宽度的字符不会触发换行。
            let wraps = match width {
                None | Some(0) => false,
                Some(w) => {
                    wrap_mode
                        && (cur.input_needs_wrap || (w == 2 && cur.point.column.0 + 1 >= self.cols))
                }
            };
            // `wrapline` 在 `line + 1 >= region.end` 时调用 `linefeed`，
            // 后者只有 `line + 1 == region.end` 才真正滚动；光标在区域下方时只是下移。
            if wraps && cur.point.line.0 as usize + 1 == self.region.end {
                self.table_scroll_up(self.region.start, 1);
            }
        }
        Handler::input(&mut self.term, c);
        if !self.alt {
            self.stamp();
        }
    }
}

/// 逐个转发的 `Handler` 方法。清单必须与 vte `ansi::Handler` 的方法一一对应（已手写覆盖的除外）。
macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {
        $(
            #[inline]
            fn $name(&mut self $(, $arg: $ty)*) {
                Handler::$name(&mut self.term, $($arg),*)
            }
        )*
    };
}

impl Handler for Core {
    // ---- 手写：需要维护侧表 / 清除语义 / 全屏状态的方法 ----

    #[inline]
    fn input(&mut self, c: char) {
        self.term
            .grid_mut()
            .cursor
            .template
            .flags
            .set(CHIP, self.chip_active);
        // 热路径：普通 ASCII 且上一个字符没写满最后一列——不可能换行，也就不可能滚动，
        // 转发后只需要记一下写入时间。其余情形（行尾、宽字符、非 ASCII）走慢路径做精确预测。
        if self.alt || self.term.grid().cursor.input_needs_wrap || !c.is_ascii() {
            return self.input_slow(c);
        }
        Handler::input(&mut self.term, c);
        self.stamp();
    }

    fn linefeed(&mut self) {
        self.end_chip();
        if !self.alt && self.cursor_line() + 1 == self.region.end {
            self.table_scroll_up(self.region.start, 1);
        }
        Handler::linefeed(&mut self.term);
    }

    fn newline(&mut self) {
        self.end_chip();
        // `Term::newline` 在内部直接调自己的 `linefeed`，不会经过本类型的 `linefeed`，必须单独镜像。
        if !self.alt && self.cursor_line() + 1 == self.region.end {
            self.table_scroll_up(self.region.start, 1);
        }
        Handler::newline(&mut self.term);
    }

    fn put_tab(&mut self, count: u16) {
        // 在最后一列等待换行时，制表符等同于换行（可能触发滚动）。
        let cur = &self.term.grid().cursor;
        if !self.alt
            && cur.input_needs_wrap
            && self.term.mode().contains(TermMode::LINE_WRAP)
            && cur.point.line.0 as usize + 1 == self.region.end
        {
            self.table_scroll_up(self.region.start, 1);
        }
        Handler::put_tab(&mut self.term, count);
    }

    fn scroll_up(&mut self, lines: usize) {
        self.table_scroll_up(self.region.start, lines);
        Handler::scroll_up(&mut self.term, lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.table_scroll_down(self.region.start, lines);
        Handler::scroll_down(&mut self.term, lines);
    }

    fn insert_blank_lines(&mut self, lines: usize) {
        let origin = self.cursor_line();
        if self.region.contains(&origin) {
            self.table_scroll_down(origin, lines);
        }
        Handler::insert_blank_lines(&mut self.term, lines);
    }

    fn delete_lines(&mut self, lines: usize) {
        let origin = self.cursor_line();
        let clamped = lines.min(self.rows - origin);
        if clamped > 0 && self.region.contains(&origin) {
            self.table_scroll_up(origin, clamped);
        }
        Handler::delete_lines(&mut self.term, lines);
    }

    fn reverse_index(&mut self) {
        if self.cursor_line() == self.region.start {
            self.table_scroll_down(self.region.start, 1);
        }
        Handler::reverse_index(&mut self.term);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        if self.alt {
            // 全屏没有回滚：ED 2 就是普通擦除，ED 3 也没有可清的（alacritty 对备用屏幕同样如此）。
            Handler::clear_screen(&mut self.term, mode);
        } else {
            self.clear_screen_main(mode);
        }
    }

    fn reset_state(&mut self) {
        self.end_chip();
        self.clears += 1;
        self.blocks.clear();
        Handler::reset_state(&mut self.term);
        self.epoch += 1;
        // RIS 会清空回滚并（若在全屏）回到主屏幕。
        self.alt = false;
        self.alt_resized = false;
        self.region = 0..self.rows;
        self.table.reset_all(self.rows);
        self.stamped_line = NO_LINE;
        self.line_number_base = self.table.screen_top_abs();
        self.batch_ed2 = false;
        self.batch_ed3 = false;
    }

    fn decaln(&mut self) {
        Handler::decaln(&mut self.term);
        if !self.alt {
            self.table.mark_screen_written(self.now_ms);
        }
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        // 镜像 `Term::set_scrolling_region`（非法区域被忽略）。
        let bottom_v = bottom.unwrap_or(self.rows);
        if top < bottom_v {
            self.region = top.saturating_sub(1).min(self.rows)..bottom_v.min(self.rows);
        }
        Handler::set_scrolling_region(&mut self.term, top, bottom);
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        match mode {
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor) if !self.alt => {
                self.enter_alt(mode)
            }
            PrivateMode::Named(NamedPrivateMode::ColumnMode) => {
                Handler::set_private_mode(&mut self.term, mode);
                self.after_deccolm();
            }
            _ => Handler::set_private_mode(&mut self.term, mode),
        }
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        match mode {
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor) if self.alt => {
                self.leave_alt(mode)
            }
            PrivateMode::Named(NamedPrivateMode::ColumnMode) => {
                Handler::unset_private_mode(&mut self.term, mode);
                self.after_deccolm();
            }
            _ => Handler::unset_private_mode(&mut self.term, mode),
        }
    }

    // ---- 纯转发 ----
    forward! {
        set_title(t: Option<String>);
        set_cursor_style(s: Option<CursorStyle>);
        set_cursor_shape(s: CursorShape);
        goto(line: i32, col: usize);
        goto_line(line: i32);
        goto_col(col: usize);
        insert_blank(n: usize);
        move_up(n: usize);
        move_down(n: usize);
        identify_terminal(i: Option<char>);
        device_status(n: usize);
        move_forward(n: usize);
        move_backward(n: usize);
        move_down_and_cr(n: usize);
        move_up_and_cr(n: usize);
        backspace();
        carriage_return();
        bell();
        substitute();
        set_horizontal_tabstop();
        erase_chars(n: usize);
        delete_chars(n: usize);
        move_backward_tabs(n: u16);
        move_forward_tabs(n: u16);
        save_cursor_position();
        restore_cursor_position();
        clear_line(m: LineClearMode);
        clear_tabs(m: TabulationClearMode);
        set_tabs(n: u16);
        terminal_attribute(a: Attr);
        set_mode(m: Mode);
        unset_mode(m: Mode);
        report_mode(m: Mode);
        report_private_mode(m: PrivateMode);
        set_keypad_application_mode();
        unset_keypad_application_mode();
        set_active_charset(c: CharsetIndex);
        configure_charset(i: CharsetIndex, c: StandardCharset);
        set_color(i: usize, c: VteRgb);
        dynamic_color_sequence(p: String, i: usize, t: &str);
        reset_color(i: usize);
        clipboard_store(c: u8, d: &[u8]);
        clipboard_load(c: u8, t: &str);
        push_title();
        pop_title();
        text_area_size_pixels();
        text_area_size_chars();
        set_hyperlink(h: Option<Hyperlink>);
        set_mouse_cursor_icon(i: CursorIcon);
        report_keyboard_mode();
        push_keyboard_mode(m: KeyboardModes);
        pop_keyboard_modes(n: u16);
        set_keyboard_mode(m: KeyboardModes, b: KeyboardModesApplyBehavior);
        set_modify_other_keys(m: ModifyOtherKeys);
        report_modify_other_keys();
        set_scp(c: ScpCharPath, u: ScpUpdateMode);
    }
}

impl Core {
    /// DECCOLM 会清屏并重置滚动区域（`Term::deccolm`），不推进回滚。
    fn after_deccolm(&mut self) {
        self.region = 0..self.rows;
        if !self.alt {
            self.table.reset_screen_rows(0, self.rows);
            self.stamped_line = NO_LINE;
        }
    }
}
