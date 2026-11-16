//! 对外的数据类型：事件、模式、主题色、快照叠加层。

use kanso_types::geometry::{AbsLine, CellPoint, GridSize, Rgb};

/// 终端配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermConfig {
    /// 初始网格尺寸。列数小于 2、行数小于 1 时会被夹到最小值（宽字符至少要 2 列）。
    pub size: GridSize,
    /// 回滚行数上限（不含屏幕）。
    pub scrollback_lines: usize,
}

/// 程序上报的工作目录（OSC 7 / 1337 / 9;9）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CwdReport {
    /// 主机名；上报里没有（或为空）时为 `None`。调用方用它区分“本机路径”和“远端路径”。
    pub host: Option<String>,
    /// 路径。OSC 7 已做百分号解码（解码失败时是原样）。
    pub path: String,
}

/// `Terminal::take_events` 产生的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// 标题变化；新值用 `Terminal::title()` 读取（`None` 表示被重置）。
    TitleChanged,
    /// 收到一次工作目录上报。**每次上报都会产生**（不与上一次去重）：terminal.md §12 要求
    /// 把它同时当作“提示符出现”的信号。
    CwdChanged(CwdReport),
    /// 响铃（同一批里连续的响铃合并为一个）。
    Bell,
    /// OSC 52 写剪贴板请求（已解码为文本）。
    ClipboardStore(String),
    /// OSC 133 A：提示符开始。同一行重复的 A 不会再产生事件。
    PromptStart {
        /// 提示符所在行。
        line: AbsLine,
        /// 是否带 `kanso=first`。
        first: bool,
    },
    /// OSC 133 B：提示符结束，用户开始输入命令。
    InputStart {
        /// 光标所在行。
        line: AbsLine,
    },
    /// OSC 133 C：命令开始执行（输出从这里开始）。
    CommandStart {
        /// 光标所在行。
        line: AbsLine,
    },
    /// OSC 133 D：命令结束。
    CommandFinished {
        /// 退出码；上报里没有时为 `None`。
        exit_code: Option<i32>,
    },
}

/// 鼠标上报模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    /// 未开启。
    None,
    /// 只上报点击（DECSET 1000）。
    Click,
    /// 点击加按下拖动（1002）。
    Drag,
    /// 所有移动（1003）。
    Motion,
}

/// kitty 键盘协议的当前标志位（与协议里的位定义一致）。
pub mod kitty {
    /// 消除转义码歧义。
    pub const DISAMBIGUATE: u8 = 1 << 0;
    /// 上报事件类型（按下/重复/释放）。
    pub const REPORT_EVENT_TYPES: u8 = 1 << 1;
    /// 上报备用键。
    pub const REPORT_ALTERNATE_KEYS: u8 = 1 << 2;
    /// 所有按键都用转义码上报。
    pub const REPORT_ALL_KEYS: u8 = 1 << 3;
    /// 上报关联文本。
    pub const REPORT_ASSOCIATED_TEXT: u8 = 1 << 4;
}

/// 供按键编码与 UI 使用的终端模式快照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermModes {
    /// 全屏（alternate screen）。
    pub alt_screen: bool,
    /// DECCKM：方向键发 `ESC O` 前缀。
    pub app_cursor: bool,
    /// DECKPAM：应用小键盘。
    pub app_keypad: bool,
    /// 粘贴时需要用 `ESC [200~` 包裹。
    pub bracketed_paste: bool,
    /// 鼠标上报模式。
    pub mouse: MouseMode,
    /// 鼠标坐标用 SGR 编码（1006）。
    pub mouse_sgr: bool,
    /// 鼠标坐标用 UTF-8 编码（1005）。
    pub mouse_utf8: bool,
    /// 焦点上报（1004）。
    pub focus_reporting: bool,
    /// 全屏里滚轮转换成方向键（1007）。
    pub alternate_scroll: bool,
    /// 自动换行。
    pub line_wrap: bool,
    /// 光标是否可见（DECTCEM）。
    pub cursor_visible: bool,
    /// LNM：回车换行模式。
    pub line_feed_new_line: bool,
    /// kitty 键盘协议标志位，见 [`kitty`]。
    pub kitty_keyboard: u8,
}

impl TermModes {
    /// 是否有任何鼠标上报模式开启。
    pub fn mouse_reporting(&self) -> bool {
        self.mouse != MouseMode::None
    }
}

/// 回应终端颜色查询（OSC 4 / 10 / 11 / 12）时使用的主题色。
///
/// 内核不知道 UI 当前的主题，所以由调用方在主题变化时设置；程序自己用 OSC 4/10/11 改过的颜色优先。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeColors {
    /// 默认前景。
    pub foreground: Rgb,
    /// 默认背景。
    pub background: Rgb,
    /// 前 16 个 ANSI 颜色。
    pub ansi: [Rgb; 16],
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

impl Default for ThemeColors {
    /// xterm 风格的默认值，只在调用方没有设置主题时用来回应查询。
    fn default() -> Self {
        Self {
            foreground: rgb(0xe5, 0xe5, 0xe5),
            background: rgb(0x00, 0x00, 0x00),
            ansi: [
                rgb(0, 0, 0),
                rgb(205, 0, 0),
                rgb(0, 205, 0),
                rgb(205, 205, 0),
                rgb(0, 0, 238),
                rgb(205, 0, 205),
                rgb(0, 205, 205),
                rgb(229, 229, 229),
                rgb(127, 127, 127),
                rgb(255, 0, 0),
                rgb(0, 255, 0),
                rgb(255, 255, 0),
                rgb(92, 92, 255),
                rgb(255, 0, 255),
                rgb(0, 255, 255),
                rgb(255, 255, 255),
            ],
        }
    }
}

/// 以绝对行号表示的闭区间（按字符流顺序：首行从 `start.col` 起，末行到 `end.col` 止，中间整行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    /// 起点（含）。
    pub start: CellPoint,
    /// 终点（含）。
    pub end: CellPoint,
}

/// 快照时叠加到单元格上的标注来源：选区与搜索匹配。
///
/// T4a 里内核自己不产生它们，只负责在生成帧时把区间标到单元格上（`CELL_SELECTED` /
/// `CELL_MATCH` / `CELL_MATCH_CURRENT`），后续的选区、搜索模块持有区间并传入。
/// 矩形选区等形态由选区模块在传入前拆成逐行区间。
#[derive(Clone, Copy, Debug, Default)]
pub struct Overlays<'a> {
    /// 选区。
    pub selection: &'a [CellRange],
    /// 所有搜索匹配。
    pub matches: &'a [CellRange],
    /// 当前匹配项（同时也应出现在 `matches` 里，帧里会同时带 `MATCH` 与 `MATCH_CURRENT`）。
    pub current_match: Option<CellRange>,
}

/// [`crate::Terminal::handle_mouse`] 的处理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseOutcome {
    /// 终端没有需要调用方响应的动作（选区变化靠 `is_dirty` / 下一帧体现）。
    None,
    /// 程序开启了鼠标上报：这些字节原样写回会话。
    Report(Vec<u8>),
    /// 一次拖动选择结束（松开鼠标，或丢失松开事件后被后续事件结束）。
    ///
    /// UI 在“选中即复制”开启且 `has_selection` 为真时复制 `Terminal::selection_text()`
    /// （terminal.md §8.2）；单击没有拖动时 `has_selection` 为假，不应复制。
    SelectionFinished {
        /// 结束时是否有非空选区。
        has_selection: bool,
    },
}

/// 搜索请求（terminal.md §11）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// 查询文本。`regex` 为假时按字面量匹配（内部转义），为空表示不搜索。
    pub text: String,
    /// 是否区分大小写。
    pub case_sensitive: bool,
    /// 是否把 `text` 当正则表达式（Rust `regex` 语法，不支持环视与反向引用）。
    pub regex: bool,
}

/// 搜索状态，供 UI 显示“第几个 / 共几个”。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchStatus {
    /// 匹配总数（达到上限时为上限值，同时 `truncated` 为真）。
    pub total: u32,
    /// 当前匹配项的序号，从 1 开始；没有匹配时为 `None`。
    pub current: Option<u32>,
    /// 匹配数超过上限（10 万）而被截断：`total` 是下限而不是精确值。
    pub truncated: bool,
    /// 正则表达式无法编译（此时没有任何匹配与高亮）。
    pub invalid_regex: bool,
}
