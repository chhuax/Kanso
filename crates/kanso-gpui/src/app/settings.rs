//! 显示设置模型（`ui-shell.md` §10），与 mac 版 `apple/Kanso/App/Settings/` 同一套存储约定。
//!
//! 存储：核心的设置表（`Core::setting` / `Core::set_setting`，SQLite，随 `.kanso` 导出），
//! 每个分区一个 key，值是 JSON 对象：
//! - `display`：终端显示（字体、字号、行距、光标、回滚、语义着色、最低对比度）+ 界面字体与字号；
//! - `appearance`：外观模式与浅色 / 深色各自的终端配色 id；
//! - `timeline`：时间戳 / 行号栏模式；
//! - `mouse`：中键 / 右键行为与选中即复制（两个平台同一格式）。
//!
//! 兼容规则与 mac 一致：写入带 `"v"`；未知字段忽略；缺字段或类型不对用默认值；数值夹进范围。
//! 因此 mac 与 Windows 读写同一份数据互不破坏（Windows 多出的 `uiFontFamily` / `uiFontSize`
//! 对 mac 是未知字段，被忽略）。
//!
//! 本模块不依赖 gpui，三个平台都能编译与测试；GPUI 一侧见 `ui/settings.rs`。

use crate::app::i18n::label;
use kanso_types::display::{CursorShape, TerminalOptions};
use kanso_types::message::Message;
use serde_json::{Map, Value, json};

use super::i18n::LanguageSetting;
use super::keybindings::KeybindingSettings;
use crate::theme::Palette;

/// 当前写入的版本号。
pub const VERSION: u32 = 1;

/// 设置分区，对应核心设置表的 key（发布后不得改名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// 终端与界面显示。
    Display,
    /// 外观。
    Appearance,
    /// 时间戳 / 行号栏。
    Timeline,
    /// 鼠标：中键、右键、选中即复制。
    Mouse,
    /// 快捷键改绑（Windows / Linux 专用 key，见 [`crate::app::keybindings`]）。
    Keybindings,
}

impl Section {
    /// 全部分区。
    pub const ALL: [Section; 5] = [
        Section::Display,
        Section::Appearance,
        Section::Timeline,
        Section::Mouse,
        Section::Keybindings,
    ];

    /// 设置表里的 key。
    pub fn key(self) -> &'static str {
        match self {
            Section::Display => "display",
            Section::Appearance => "appearance",
            Section::Timeline => "timeline",
            Section::Mouse => "mouse",
            Section::Keybindings => crate::app::keybindings::SETTING_KEY,
        }
    }
}

// ---- 取值辅助：键缺失或类型不对都返回默认值 ----

fn get_str(m: &Map<String, Value>, k: &str, d: &str) -> String {
    m.get(k)
        .and_then(Value::as_str)
        .map_or_else(|| d.to_owned(), str::to_owned)
}

fn get_bool(m: &Map<String, Value>, k: &str, d: bool) -> bool {
    m.get(k).and_then(Value::as_bool).unwrap_or(d)
}

fn get_f64(m: &Map<String, Value>, k: &str, d: f64, range: (f64, f64)) -> f64 {
    match m.get(k).and_then(Value::as_f64) {
        Some(v) if v.is_finite() => v.clamp(range.0, range.1),
        _ => d,
    }
}

fn parse_object(json: Option<&str>) -> Map<String, Value> {
    json.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

// ---- 光标形状 ----

/// 默认光标形状（设置里的存储值）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShapeSetting {
    /// 块状。
    Block,
    /// 下划线。
    Underline,
    /// 竖线（默认，2026-11-16 产品决定）。
    #[default]
    Bar,
}

impl CursorShapeSetting {
    /// 全部取值，界面按此顺序排列。
    pub const ALL: [CursorShapeSetting; 3] = [
        CursorShapeSetting::Block,
        CursorShapeSetting::Underline,
        CursorShapeSetting::Bar,
    ];

    fn storage(self) -> &'static str {
        match self {
            CursorShapeSetting::Block => "block",
            CursorShapeSetting::Underline => "underline",
            CursorShapeSetting::Bar => "bar",
        }
    }

    fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。
    pub fn label(self) -> &'static str {
        match self {
            CursorShapeSetting::Block => label(Message::UiBlock {}),
            CursorShapeSetting::Underline => label(Message::UiUnderline {}),
            CursorShapeSetting::Bar => label(Message::UiBar {}),
        }
    }

    fn to_core(self) -> CursorShape {
        match self {
            CursorShapeSetting::Block => CursorShape::Block,
            CursorShapeSetting::Underline => CursorShape::Underline,
            CursorShapeSetting::Bar => CursorShape::Bar,
        }
    }
}

// ---- 显示 ----

/// 终端与界面显示设置。
#[derive(Clone, Debug, PartialEq)]
pub struct DisplaySettings {
    /// 终端字体族；空串 = 平台默认等宽字体。
    pub font_family: String,
    /// 终端字号。
    pub font_size: f64,
    /// 行距倍数。
    pub line_height: f64,
    /// 默认光标形状。
    pub cursor_shape: CursorShapeSetting,
    /// 光标是否闪烁。
    pub cursor_blink: bool,
    /// 回滚行数。
    pub scrollback_lines: u32,
    /// 最低对比度保护开关（本平台渲染层尚未实现，只做存取，保证与 mac 往返不丢）。
    pub minimum_contrast_enabled: bool,
    /// 最低对比度比值。
    pub minimum_contrast_ratio: f64,
    /// 语义着色。
    pub semantic_coloring: bool,
    /// 选中即复制（与 mac 版共用字段，保证往返不丢）。
    pub copy_on_select: bool,
    /// 界面字体族；空串 = 平台默认界面字体。
    pub ui_font_family: String,
    /// 界面字号。
    pub ui_font_size: f64,
}

impl DisplaySettings {
    /// 终端字号范围。
    pub const FONT_SIZE_RANGE: (f64, f64) = (9.0, 36.0);
    /// 默认终端字号。
    pub const DEFAULT_FONT_SIZE: f64 = 14.0;
    /// 行距范围。
    pub const LINE_HEIGHT_RANGE: (f64, f64) = (1.0, 2.0);
    /// 行距步长。
    pub const LINE_HEIGHT_STEP: f64 = 0.05;
    /// 回滚行数范围。
    pub const SCROLLBACK_RANGE: (u32, u32) = (1_000, 1_000_000);
    /// 对比度范围。
    pub const CONTRAST_RANGE: (f64, f64) = (1.0, 21.0);
    /// 界面字号范围。
    pub const UI_FONT_SIZE_RANGE: (f64, f64) = (11.0, 20.0);
    /// 默认界面字号（设计 token `font.ui.body`）。
    pub const DEFAULT_UI_FONT_SIZE: f64 = 13.0;

    /// 解码（宽松）。
    pub fn decode(json: Option<&str>) -> DisplaySettings {
        let m = parse_object(json);
        let d = DisplaySettings::default();
        let lines = m
            .get("scrollbackLines")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .map_or(d.scrollback_lines, |v| {
                v.clamp(
                    f64::from(Self::SCROLLBACK_RANGE.0),
                    f64::from(Self::SCROLLBACK_RANGE.1),
                ) as u32
            });
        DisplaySettings {
            font_family: get_str(&m, "fontFamily", &d.font_family),
            font_size: get_f64(&m, "fontSize", d.font_size, Self::FONT_SIZE_RANGE),
            line_height: get_f64(&m, "lineHeight", d.line_height, Self::LINE_HEIGHT_RANGE),
            cursor_shape: CursorShapeSetting::from_storage(&get_str(&m, "cursorShape", ""))
                .unwrap_or(d.cursor_shape),
            cursor_blink: get_bool(&m, "cursorBlink", d.cursor_blink),
            scrollback_lines: lines,
            minimum_contrast_enabled: get_bool(
                &m,
                "minimumContrastEnabled",
                d.minimum_contrast_enabled,
            ),
            minimum_contrast_ratio: get_f64(
                &m,
                "minimumContrastRatio",
                d.minimum_contrast_ratio,
                Self::CONTRAST_RANGE,
            ),
            semantic_coloring: get_bool(&m, "semanticColoring", d.semantic_coloring),
            copy_on_select: get_bool(&m, "copyOnSelect", d.copy_on_select),
            ui_font_family: get_str(&m, "uiFontFamily", &d.ui_font_family),
            ui_font_size: get_f64(&m, "uiFontSize", d.ui_font_size, Self::UI_FONT_SIZE_RANGE),
        }
    }

    /// 编码（键序固定：serde_json 的 Map 默认按键排序）。
    pub fn encode(&self) -> String {
        json!({
            "v": VERSION,
            "fontFamily": self.font_family,
            "fontSize": self.font_size,
            "lineHeight": self.line_height,
            "cursorShape": self.cursor_shape.storage(),
            "cursorBlink": self.cursor_blink,
            "scrollbackLines": self.scrollback_lines,
            "minimumContrastEnabled": self.minimum_contrast_enabled,
            "minimumContrastRatio": self.minimum_contrast_ratio,
            "semanticColoring": self.semantic_coloring,
            "copyOnSelect": self.copy_on_select,
            "uiFontFamily": self.ui_font_family,
            "uiFontSize": self.ui_font_size,
        })
        .to_string()
    }

    /// 下发给核心的终端选项。
    pub fn terminal_options(&self) -> TerminalOptions {
        TerminalOptions {
            scrollback_lines: self.scrollback_lines,
            cursor_shape: self.cursor_shape.to_core(),
            cursor_blink: self.cursor_blink,
            semantic_coloring: self.semantic_coloring,
        }
    }

    /// 字号加减一档（快捷键 Ctrl+= / Ctrl+-，步长 1，夹进范围）。
    pub fn zoomed(&self, delta: f64) -> f64 {
        (self.font_size + delta)
            .round()
            .clamp(Self::FONT_SIZE_RANGE.0, Self::FONT_SIZE_RANGE.1)
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        DisplaySettings {
            font_family: String::new(),
            font_size: Self::DEFAULT_FONT_SIZE,
            line_height: 1.2,
            cursor_shape: CursorShapeSetting::Bar,
            cursor_blink: true,
            scrollback_lines: TerminalOptions::DEFAULT_SCROLLBACK_LINES,
            minimum_contrast_enabled: true,
            minimum_contrast_ratio: 4.5,
            semantic_coloring: true,
            copy_on_select: false,
            ui_font_family: String::new(),
            ui_font_size: Self::DEFAULT_UI_FONT_SIZE,
        }
    }
}

/// 显示设置中的数值字段。文本提交与步进按钮使用相同的范围。
#[derive(Clone, Copy, Debug)]
pub enum NumericSetting {
    /// 终端字号。
    FontSize,
    /// 终端行距倍数。
    LineHeight,
    /// 回滚行数。
    Scrollback,
    /// 最低对比度。
    Contrast,
    /// 界面字号。
    UiFontSize,
}

impl NumericSetting {
    /// 与设置窗口的输入框顺序一致。
    pub const ALL: [Self; 5] = [
        Self::FontSize,
        Self::LineHeight,
        Self::Scrollback,
        Self::Contrast,
        Self::UiFontSize,
    ];

    /// 下限、上限和按钮步长。用户可以直接输入范围内的中间值。
    pub fn range_step(self) -> (f64, f64, f64) {
        let ((lo, hi), step) = match self {
            Self::FontSize => (DisplaySettings::FONT_SIZE_RANGE, 1.0),
            Self::LineHeight => (
                DisplaySettings::LINE_HEIGHT_RANGE,
                DisplaySettings::LINE_HEIGHT_STEP,
            ),
            Self::Scrollback => (
                (
                    f64::from(DisplaySettings::SCROLLBACK_RANGE.0),
                    f64::from(DisplaySettings::SCROLLBACK_RANGE.1),
                ),
                1000.0,
            ),
            Self::Contrast => (DisplaySettings::CONTRAST_RANGE, 0.5),
            Self::UiFontSize => (DisplaySettings::UI_FONT_SIZE_RANGE, 1.0),
        };
        (lo, hi, step)
    }

    /// 当前值。
    pub fn value(self, d: &DisplaySettings) -> f64 {
        match self {
            Self::FontSize => d.font_size,
            Self::LineHeight => d.line_height,
            Self::Scrollback => f64::from(d.scrollback_lines),
            Self::Contrast => d.minimum_contrast_ratio,
            Self::UiFontSize => d.ui_font_size,
        }
    }

    /// 按 Mac 设置的范围写回，显示精度与实际值独立。
    pub fn set(self, d: &mut DisplaySettings, value: f64) {
        let (lo, hi, _) = self.range_step();
        let v = value.clamp(lo, hi);
        match self {
            Self::FontSize => d.font_size = v,
            Self::LineHeight => d.line_height = v,
            Self::Scrollback => d.scrollback_lines = v as u32,
            Self::Contrast => d.minimum_contrast_ratio = v,
            Self::UiFontSize => d.ui_font_size = v.round(),
        }
    }

    /// 回车或失焦提交。无效数字保留原值。
    pub fn commit(self, text: &str, d: &mut DisplaySettings) {
        if let Ok(value) = text.trim().parse::<f64>()
            && value.is_finite()
        {
            self.set(d, value);
        }
    }

    /// 对应 Mac 数字框的小数位数。
    pub fn text(self, d: &DisplaySettings) -> String {
        let value = self.value(d);
        match self {
            Self::LineHeight => format!("{value:.2}"),
            Self::Contrast => format!("{value:.1}"),
            _ => format!("{value:.0}"),
        }
    }
}

/// 把行距规整到步长（避免 1.2000000000000002 这类浮点毛刺写进存储）。
pub fn snap_line_height(v: f64) -> f64 {
    let (lo, hi) = DisplaySettings::LINE_HEIGHT_RANGE;
    // 先放大成整数再除，避免 24 * 0.05 = 1.2000000000000002。
    let steps_per_unit = (1.0 / DisplaySettings::LINE_HEIGHT_STEP).round();
    ((v * steps_per_unit).round() / steps_per_unit).clamp(lo, hi)
}

// ---- 外观 ----

/// 外观模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    /// 跟随系统。
    #[default]
    System,
    /// 浅色。
    Light,
    /// 深色。
    Dark,
}

impl ThemeMode {
    /// 全部取值。
    pub const ALL: [ThemeMode; 3] = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];

    pub(super) fn storage(self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }

    pub(super) fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::System => label(Message::UiFollowSystem {}),
            ThemeMode::Light => label(Message::UiLight {}),
            ThemeMode::Dark => label(Message::UiDark {}),
        }
    }

    /// 实际是否用深色：显式选择优先，跟随系统时看系统当前是否深色。
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            ThemeMode::System => system_dark,
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
        }
    }
}

/// 标签栏位置（ui-shell.md §2.5）。存储值与 mac 版相同（`left` / `top`），`.kanso` 导出跨平台可读。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabBarPosition {
    /// 左侧竖向标签栏。
    #[default]
    Left,
    /// 每个分屏顶部各一条横向标签条。
    Top,
}

impl TabBarPosition {
    /// 全部取值，顺序即设置页上的显示顺序。
    pub const ALL: [TabBarPosition; 2] = [TabBarPosition::Left, TabBarPosition::Top];

    pub(super) fn storage(self) -> &'static str {
        match self {
            TabBarPosition::Left => "left",
            TabBarPosition::Top => "top",
        }
    }

    pub(super) fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。
    pub fn label(self) -> &'static str {
        match self {
            TabBarPosition::Left => label(Message::UiTabBarLeft {}),
            TabBarPosition::Top => label(Message::UiTabBarTop {}),
        }
    }
}

/// 内置终端配色：`(id, 显示名)`。id 与 mac 版一致。
pub const BUILTIN_THEMES: [(&str, &str); 2] =
    [("kanso-light", "Kanso Light"), ("kanso-dark", "Kanso Dark")];

/// 外观设置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceSettings {
    /// 外观模式。
    pub mode: ThemeMode,
    /// 浅色外观用的终端配色 id（未知 id 原样保留，使用处回落默认）。
    pub light_theme: String,
    /// 深色外观用的终端配色 id。
    pub dark_theme: String,
    /// 界面语言（Windows / Linux 在设置里切换；mac 版跟随系统，忽略这个字段）。
    pub language: LanguageSetting,
    /// 标签栏位置。
    pub tab_bar_position: TabBarPosition,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        AppearanceSettings {
            mode: ThemeMode::System,
            light_theme: "kanso-light".to_owned(),
            dark_theme: "kanso-dark".to_owned(),
            language: LanguageSetting::System,
            tab_bar_position: TabBarPosition::Left,
        }
    }
}

impl AppearanceSettings {
    /// 解码（宽松）。
    pub fn decode(json: Option<&str>) -> AppearanceSettings {
        let m = parse_object(json);
        let d = AppearanceSettings::default();
        AppearanceSettings {
            mode: ThemeMode::from_storage(&get_str(&m, "mode", "")).unwrap_or(d.mode),
            light_theme: get_str(&m, "lightTheme", &d.light_theme),
            dark_theme: get_str(&m, "darkTheme", &d.dark_theme),
            language: LanguageSetting::from_storage(&get_str(&m, "language", ""))
                .unwrap_or(d.language),
            // 旧数据没有这个字段；未知值（更新版本新增的位置）同样回落到左侧。
            tab_bar_position: TabBarPosition::from_storage(&get_str(&m, "tabBarPosition", ""))
                .unwrap_or(d.tab_bar_position),
        }
    }

    /// 编码。
    pub fn encode(&self) -> String {
        json!({
            "v": VERSION,
            "mode": self.mode.storage(),
            "lightTheme": self.light_theme,
            "darkTheme": self.dark_theme,
            "language": self.language.storage(),
            "tabBarPosition": self.tab_bar_position.storage(),
        })
        .to_string()
    }

    /// 当前外观下的终端配色。
    pub fn terminal_palette(
        &self,
        dark: bool,
        themes: &[kanso_core::terminal_themes::TerminalTheme],
    ) -> Palette {
        let id = if dark {
            &self.dark_theme
        } else {
            &self.light_theme
        };
        if let Some(theme) = themes.iter().find(|t| t.id == *id) {
            return Palette::from_theme(theme);
        }
        match id.as_str() {
            "kanso-light" => Palette::kanso_light(),
            "kanso-dark" => Palette::kanso_dark(),
            // 未知 id：回落到该外观的默认配色。
            _ if dark => Palette::kanso_dark(),
            _ => Palette::kanso_light(),
        }
    }
}

// ---- 时间轴 ----

/// 时间戳 / 行号栏模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GutterMode {
    /// 关闭（默认）。
    #[default]
    Off,
    /// 只显示行号。
    LineNumbers,
    /// 只显示时间。
    Timestamps,
    /// 两者都显示。
    Both,
}

impl GutterMode {
    /// 全部取值。
    pub const ALL: [GutterMode; 4] = [
        GutterMode::Off,
        GutterMode::LineNumbers,
        GutterMode::Timestamps,
        GutterMode::Both,
    ];

    pub(super) fn storage(self) -> &'static str {
        match self {
            GutterMode::Off => "off",
            GutterMode::LineNumbers => "lineNumbers",
            GutterMode::Timestamps => "timestamps",
            GutterMode::Both => "both",
        }
    }

    pub(super) fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。
    pub fn label(self) -> &'static str {
        match self {
            GutterMode::Off => label(Message::UiOff {}),
            GutterMode::LineNumbers => label(Message::UiLineNumbersOnly {}),
            GutterMode::Timestamps => label(Message::UiTimestampsOnly {}),
            GutterMode::Both => label(Message::UiTimestampsAndLineNumbers {}),
        }
    }
}

/// 时间轴设置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineSettings {
    /// 当前模式。
    pub mode: GutterMode,
    /// 重新打开时恢复的模式（不会是 `Off`）。
    pub last_visible: GutterMode,
}

impl Default for TimelineSettings {
    fn default() -> Self {
        TimelineSettings {
            mode: GutterMode::Off,
            last_visible: GutterMode::Both,
        }
    }
}

impl TimelineSettings {
    /// 解码（宽松）。
    pub fn decode(json: Option<&str>) -> TimelineSettings {
        let m = parse_object(json);
        let d = TimelineSettings::default();
        let last = GutterMode::from_storage(&get_str(&m, "lastVisible", ""))
            .filter(|g| *g != GutterMode::Off)
            .unwrap_or(d.last_visible);
        TimelineSettings {
            mode: GutterMode::from_storage(&get_str(&m, "mode", "")).unwrap_or(d.mode),
            last_visible: last,
        }
    }

    /// 编码。
    pub fn encode(&self) -> String {
        json!({
            "v": VERSION,
            "mode": self.mode.storage(),
            "lastVisible": self.last_visible.storage(),
        })
        .to_string()
    }

    /// 设置模式；非 `Off` 的模式同时记为"可恢复模式"。
    pub fn set_mode(&mut self, mode: GutterMode) {
        self.mode = mode;
        if mode != GutterMode::Off {
            self.last_visible = mode;
        }
    }
}

// ---- 鼠标 ----

/// 中键行为（terminal.md §8.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MiddleClick {
    /// 粘贴剪贴板内容（默认）。
    #[default]
    Paste,
    /// 无动作（给滚轮容易误按中键的用户）。
    None,
}

impl MiddleClick {
    /// 全部取值，界面按此顺序排列。
    pub const ALL: [MiddleClick; 2] = [MiddleClick::Paste, MiddleClick::None];

    fn storage(self) -> &'static str {
        match self {
            MiddleClick::Paste => "paste",
            MiddleClick::None => "none",
        }
    }

    fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。
    pub fn label(self) -> &'static str {
        match self {
            MiddleClick::Paste => label(Message::UiMiddleClickPaste {}),
            MiddleClick::None => label(Message::UiMiddleClickNone {}),
        }
    }
}

/// 右键行为（terminal.md §8.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RightClick {
    /// 弹出菜单（默认）。
    #[default]
    Menu,
    /// 有选区就复制，没有就粘贴。
    CopyOrPaste,
}

impl RightClick {
    /// 全部取值，界面按此顺序排列。
    pub const ALL: [RightClick; 2] = [RightClick::Menu, RightClick::CopyOrPaste];

    fn storage(self) -> &'static str {
        match self {
            RightClick::Menu => "menu",
            RightClick::CopyOrPaste => "copyOrPaste",
        }
    }

    fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。
    pub fn label(self) -> &'static str {
        match self {
            RightClick::Menu => label(Message::UiRightClickMenu {}),
            RightClick::CopyOrPaste => label(Message::UiRightClickCopyOrPaste {}),
        }
    }
}

/// 鼠标设置：存放在设置表的 `mouse` 分区，与 mac 版同一格式
/// `{"v":1,"middleClick":"paste","rightClick":"menu","copyOnSelect":false}`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseSettings {
    /// 中键行为。
    pub middle_click: MiddleClick,
    /// 右键行为。
    pub right_click: RightClick,
    /// 选中即复制。
    pub copy_on_select: bool,
}

impl MouseSettings {
    /// 解码（宽松）：缺字段、未知取值、类型不对都用默认值。
    pub fn decode(json: Option<&str>) -> MouseSettings {
        let m = parse_object(json);
        let d = MouseSettings::default();
        MouseSettings {
            middle_click: MiddleClick::from_storage(&get_str(&m, "middleClick", ""))
                .unwrap_or(d.middle_click),
            right_click: RightClick::from_storage(&get_str(&m, "rightClick", ""))
                .unwrap_or(d.right_click),
            copy_on_select: get_bool(&m, "copyOnSelect", d.copy_on_select),
        }
    }

    /// 编码。
    pub fn encode(&self) -> String {
        json!({
            "v": VERSION,
            "middleClick": self.middle_click.storage(),
            "rightClick": self.right_click.storage(),
            "copyOnSelect": self.copy_on_select,
        })
        .to_string()
    }

    /// 迁移旧版散落在别处的两个字段，只在 `mouse` 分区不存在时由 [`SettingsStore::load`] 调用：
    /// - Windows 旧版把右键模式存在 `windows.panels` 的 `right_click_menu` 里；
    /// - 1.0.7 把选中即复制存在 `display` 分区的 `copyOnSelect` 里（两个平台都写过）。
    ///
    /// 返回需要写回的设置；两个旧字段都不存在（或不是布尔）时返回 `None`，保持默认、不写。
    fn migrate_legacy(
        panels_json: Option<&str>,
        display_json: Option<&str>,
    ) -> Option<MouseSettings> {
        let right_menu = parse_object(panels_json)
            .get("right_click_menu")
            .and_then(Value::as_bool);
        let copy_on_select = parse_object(display_json)
            .get("copyOnSelect")
            .and_then(Value::as_bool);
        if right_menu.is_none() && copy_on_select.is_none() {
            return None;
        }
        let d = MouseSettings::default();
        Some(MouseSettings {
            right_click: match right_menu {
                Some(false) => RightClick::CopyOrPaste,
                Some(true) => RightClick::Menu,
                None => d.right_click,
            },
            copy_on_select: copy_on_select.unwrap_or(d.copy_on_select),
            ..d
        })
    }
}

/// 一次点击该由终端做什么（右键 / 中键共用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickAction {
    /// 交给程序（鼠标上报）。
    Report,
    /// 弹出右键菜单。
    Menu,
    /// 复制选区。
    Copy,
    /// 粘贴。
    Paste,
    /// 什么都不做。
    Nothing,
}

/// 右键决策。程序开启了鼠标上报且没按 Shift 时交给程序；Shift 绕过上报，由终端按设置处理。
pub fn right_click_action(
    mode: RightClick,
    reporting: bool,
    shift: bool,
    has_selection: bool,
) -> ClickAction {
    if reporting && !shift {
        return ClickAction::Report;
    }
    match mode {
        RightClick::Menu => ClickAction::Menu,
        RightClick::CopyOrPaste if has_selection => ClickAction::Copy,
        RightClick::CopyOrPaste => ClickAction::Paste,
    }
}

/// 中键决策，上报规则同 [`right_click_action`]。
pub fn middle_click_action(mode: MiddleClick, reporting: bool, shift: bool) -> ClickAction {
    if reporting && !shift {
        return ClickAction::Report;
    }
    match mode {
        MiddleClick::Paste => ClickAction::Paste,
        MiddleClick::None => ClickAction::Nothing,
    }
}

/// 左键松开时是否触发“选中即复制”：开关开启、这次按下由终端自己处理选择
/// （按下时没有被鼠标上报接管），并且确实有非空选区（单击不产生选区时不复制）。
pub fn should_copy_on_select(enabled: bool, local_selection: bool, has_selection: bool) -> bool {
    enabled && local_selection && has_selection
}

// ---- 持久化后端与存储 ----

/// 设置的持久化后端（`Core` 实现它；测试用内存实现）。
pub trait SettingsBackend {
    /// 读分区 JSON。
    fn setting(&self, key: &str) -> Option<String>;
    /// 写分区 JSON。
    fn set_setting(&self, key: &str, json: String) -> Result<(), String>;
    /// 清空全部设置（只动设置表，不动会话 / 分组 / Sender 命令 / 历史）。
    fn reset_settings(&self) -> Result<(), String>;
    /// 下发终端选项（回滚、光标、语义着色）。
    fn set_terminal_options(&self, options: TerminalOptions);
}

impl SettingsBackend for kanso_core::Core {
    fn setting(&self, key: &str) -> Option<String> {
        kanso_core::Core::setting(self, key)
    }
    fn set_setting(&self, key: &str, json: String) -> Result<(), String> {
        kanso_core::Core::set_setting(self, key, json).map_err(|e| e.to_string())
    }
    fn reset_settings(&self) -> Result<(), String> {
        kanso_core::Core::reset_settings(self).map_err(|e| e.to_string())
    }
    fn set_terminal_options(&self, options: TerminalOptions) {
        kanso_core::Core::set_terminal_options(self, options);
    }
}

impl<T: SettingsBackend + ?Sized> SettingsBackend for std::sync::Arc<T> {
    fn setting(&self, key: &str) -> Option<String> {
        (**self).setting(key)
    }
    fn set_setting(&self, key: &str, json: String) -> Result<(), String> {
        (**self).set_setting(key, json)
    }
    fn reset_settings(&self) -> Result<(), String> {
        (**self).reset_settings()
    }
    fn set_terminal_options(&self, options: TerminalOptions) {
        (**self).set_terminal_options(options);
    }
}

/// 全部设置的内存镜像 + 持久化。修改方法返回受影响的分区（值没变返回 `None`）。
pub struct SettingsStore<B> {
    backend: B,
    display: DisplaySettings,
    appearance: AppearanceSettings,
    timeline: TimelineSettings,
    mouse: MouseSettings,
    keybindings: KeybindingSettings,
}

impl<B: SettingsBackend> SettingsStore<B> {
    /// 从后端载入，并把终端选项下发给核心（核心的默认值未必与用户保存的一致）。
    pub fn load(backend: B) -> Self {
        let display = DisplaySettings::decode(backend.setting(Section::Display.key()).as_deref());
        let appearance =
            AppearanceSettings::decode(backend.setting(Section::Appearance.key()).as_deref());
        let timeline =
            TimelineSettings::decode(backend.setting(Section::Timeline.key()).as_deref());
        let keybindings =
            KeybindingSettings::decode(backend.setting(Section::Keybindings.key()).as_deref());
        let mouse = match backend.setting(Section::Mouse.key()) {
            Some(json) => MouseSettings::decode(Some(&json)),
            // 没有 mouse 分区：从旧的 windows.panels.right_click_menu 与 display.copyOnSelect
            // 迁移一次并写回；写回之后该分区存在，旧字段就不再被读取。
            None => {
                match MouseSettings::migrate_legacy(
                    backend.setting("windows.panels").as_deref(),
                    backend.setting(Section::Display.key()).as_deref(),
                ) {
                    Some(migrated) => {
                        if let Err(e) = backend.set_setting(Section::Mouse.key(), migrated.encode())
                        {
                            eprintln!("迁移鼠标设置失败：{e}");
                        }
                        migrated
                    }
                    None => MouseSettings::default(),
                }
            }
        };
        backend.set_terminal_options(display.terminal_options());
        SettingsStore {
            backend,
            display,
            appearance,
            timeline,
            mouse,
            keybindings,
        }
    }

    /// 快捷键覆盖。
    pub fn keybindings(&self) -> &KeybindingSettings {
        &self.keybindings
    }

    /// 修改快捷键覆盖。
    pub fn update_keybindings(
        &mut self,
        change: impl FnOnce(&mut KeybindingSettings),
    ) -> Option<Section> {
        let mut new = self.keybindings.clone();
        change(&mut new);
        if new == self.keybindings {
            return None;
        }
        self.keybindings = new;
        self.persist(Section::Keybindings, self.keybindings.encode());
        Some(Section::Keybindings)
    }

    /// “恢复默认设置”：清空设置表，内存里的各分区回到默认值，并把默认终端选项下发给核心。
    /// 失败时什么都不改。只动设置，不动会话 / 分组 / Sender 命令 / 历史（store-import.md §9）。
    pub fn reset_to_defaults(&mut self) -> Result<(), String> {
        self.backend.reset_settings()?;
        self.display = DisplaySettings::default();
        self.appearance = AppearanceSettings::default();
        self.timeline = TimelineSettings::default();
        self.mouse = MouseSettings::default();
        self.keybindings = KeybindingSettings::default();
        self.backend
            .set_terminal_options(self.display.terminal_options());
        Ok(())
    }

    /// 显示设置。
    pub fn display(&self) -> &DisplaySettings {
        &self.display
    }

    /// 外观设置。
    pub fn appearance(&self) -> &AppearanceSettings {
        &self.appearance
    }

    /// 时间轴设置。
    pub fn timeline(&self) -> &TimelineSettings {
        &self.timeline
    }

    /// 鼠标设置。
    pub fn mouse(&self) -> &MouseSettings {
        &self.mouse
    }

    /// 修改鼠标设置。
    pub fn update_mouse(&mut self, change: impl FnOnce(&mut MouseSettings)) -> Option<Section> {
        let mut new = self.mouse;
        change(&mut new);
        if new == self.mouse {
            return None;
        }
        self.mouse = new;
        self.persist(Section::Mouse, self.mouse.encode());
        Some(Section::Mouse)
    }

    /// 修改显示设置。变化时写回、下发终端选项。
    pub fn update_display(&mut self, change: impl FnOnce(&mut DisplaySettings)) -> Option<Section> {
        let mut new = self.display.clone();
        change(&mut new);
        if new == self.display {
            return None;
        }
        self.display = new;
        self.persist(Section::Display, self.display.encode());
        self.backend
            .set_terminal_options(self.display.terminal_options());
        Some(Section::Display)
    }

    /// 修改外观设置。
    pub fn update_appearance(
        &mut self,
        change: impl FnOnce(&mut AppearanceSettings),
    ) -> Option<Section> {
        let mut new = self.appearance.clone();
        change(&mut new);
        if new == self.appearance {
            return None;
        }
        self.appearance = new;
        self.persist(Section::Appearance, self.appearance.encode());
        Some(Section::Appearance)
    }

    /// 修改时间轴设置。
    pub fn update_timeline(
        &mut self,
        change: impl FnOnce(&mut TimelineSettings),
    ) -> Option<Section> {
        let mut new = self.timeline;
        change(&mut new);
        if new == self.timeline {
            return None;
        }
        self.timeline = new;
        self.persist(Section::Timeline, self.timeline.encode());
        Some(Section::Timeline)
    }

    /// 写失败只记在返回前的日志里：内存值照常生效，下次修改会再试（与 mac 版一致）。
    fn persist(&self, section: Section, json: String) {
        if let Err(e) = self.backend.set_setting(section.key(), json) {
            eprintln!("保存设置 {} 失败：{e}", section.key());
        }
    }
}

// ---- 字体 ----

/// 判断一个字体是不是等宽：几个宽度差异很大的字符步进相同。
/// `advances` 依次是 `i`、`m`、`W`、`.` 的步进（任何一个取不到就判为非等宽）。
pub fn is_monospace(advances: &[f32]) -> bool {
    match advances.first() {
        Some(first) if *first > 0.0 => advances.iter().all(|a| (a - first).abs() <= 0.01 * first),
        _ => false,
    }
}

/// 过滤出可显示的字体族名：去掉空名、系统内部名（以 `.` 开头）、竖排副本（以 `@` 开头），去重并排序。
pub fn clean_font_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = names
        .into_iter()
        .filter(|n| {
            let t = n.trim();
            !t.is_empty() && !t.starts_with('.') && !t.starts_with('@')
        })
        .collect();
    v.sort_by_key(|n| n.to_lowercase());
    v.dedup();
    v
}

/// 在已安装字体里挑一个 Nerd Font 做图标回退：优先 `Symbols Nerd Font Mono`，
/// 其次任意名字含 "Nerd Font" 且以 "Mono" 结尾的，再次含 "Nerd Font" 的。
pub fn pick_nerd_fallback(installed: &[String]) -> Option<String> {
    let has = |n: &&String| n.contains("Nerd Font");
    installed
        .iter()
        .find(|n| n.as_str() == "Symbols Nerd Font Mono")
        .or_else(|| installed.iter().filter(has).find(|n| n.ends_with("Mono")))
        .or_else(|| installed.iter().find(has))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Mem {
        map: RefCell<HashMap<String, String>>,
        options: RefCell<Vec<TerminalOptions>>,
    }

    impl SettingsBackend for &Mem {
        fn setting(&self, key: &str) -> Option<String> {
            self.map.borrow().get(key).cloned()
        }
        fn set_setting(&self, key: &str, json: String) -> Result<(), String> {
            self.map.borrow_mut().insert(key.to_owned(), json);
            Ok(())
        }
        fn reset_settings(&self) -> Result<(), String> {
            self.map.borrow_mut().clear();
            Ok(())
        }
        fn set_terminal_options(&self, options: TerminalOptions) {
            self.options.borrow_mut().push(options);
        }
    }

    #[test]
    fn numeric_text_commits_nonpreset_values_and_restores_invalid_input() {
        let mut d = DisplaySettings::default();
        NumericSetting::Scrollback.commit("12345", &mut d);
        NumericSetting::LineHeight.commit("1.23", &mut d);
        NumericSetting::Contrast.commit("6.7", &mut d);
        NumericSetting::Contrast.commit("NaN", &mut d);
        assert_eq!(d.scrollback_lines, 12345);
        assert_eq!(NumericSetting::LineHeight.text(&d), "1.23");
        assert_eq!(d.minimum_contrast_ratio, 6.7);
        NumericSetting::Scrollback.commit("8", &mut d);
        assert_eq!(d.scrollback_lines, 1000);
    }

    #[test]
    fn defaults_match_spec() {
        let d = DisplaySettings::default();
        assert_eq!(d.font_size, 14.0);
        assert_eq!(d.line_height, 1.2);
        assert_eq!(d.cursor_shape, CursorShapeSetting::Bar);
        assert!(d.cursor_blink);
        assert_eq!(d.scrollback_lines, 10_000);
        assert_eq!(AppearanceSettings::default().mode, ThemeMode::System);
        assert_eq!(TimelineSettings::default().mode, GutterMode::Off);
    }

    #[test]
    fn display_round_trip() {
        let d = DisplaySettings {
            font_family: "JetBrains Mono".into(),
            font_size: 17.0,
            line_height: 1.35,
            cursor_shape: CursorShapeSetting::Block,
            cursor_blink: false,
            scrollback_lines: 50_000,
            copy_on_select: true,
            ui_font_family: "Segoe UI".into(),
            ..DisplaySettings::default()
        };
        assert_eq!(DisplaySettings::decode(Some(&d.encode())), d);
    }

    #[test]
    fn decode_is_lenient() {
        // 缺字段、类型不对、未知字段、越界值：都不失败。
        let d = DisplaySettings::decode(Some(
            r#"{"v":9,"fontSize":"big","lineHeight":99,"cursorShape":"star","scrollbackLines":5,"future":1,"cursorBlink":3}"#,
        ));
        assert_eq!(d.font_size, 14.0);
        assert_eq!(d.line_height, 2.0);
        assert_eq!(d.cursor_shape, CursorShapeSetting::Bar);
        assert_eq!(d.scrollback_lines, 1_000);
        assert!(d.cursor_blink);
        // 不是对象 / 不是 JSON / 没有：全默认。
        assert_eq!(
            DisplaySettings::decode(Some("[1]")),
            DisplaySettings::default()
        );
        assert_eq!(
            DisplaySettings::decode(Some("{{")),
            DisplaySettings::default()
        );
        assert_eq!(DisplaySettings::decode(None), DisplaySettings::default());
    }

    #[test]
    fn reads_mac_written_display_json() {
        // mac 版写出的分区没有 uiFontFamily / uiFontSize。
        let d = DisplaySettings::decode(Some(
            r#"{"cursorBlink":false,"cursorShape":"underline","fontFamily":"Menlo","fontSize":16,"lineHeight":1.25,"minimumContrastEnabled":false,"minimumContrastRatio":7,"scrollbackLines":20000,"semanticColoring":false,"v":1}"#,
        ));
        assert_eq!(d.font_family, "Menlo");
        assert_eq!(d.font_size, 16.0);
        assert_eq!(d.cursor_shape, CursorShapeSetting::Underline);
        assert!(!d.semantic_coloring);
        assert_eq!(d.ui_font_size, 13.0);
    }

    #[test]
    fn appearance_and_timeline_codecs() {
        let a = AppearanceSettings::decode(Some(
            r#"{"mode":"dark","lightTheme":"kanso-dark","darkTheme":"future-theme"}"#,
        ));
        assert_eq!(a.mode, ThemeMode::Dark);
        assert_eq!(AppearanceSettings::decode(Some(&a.encode())), a);
        // 未知配色 id 保留，但取色时回落到该外观的默认。
        assert_eq!(a.dark_theme, "future-theme");
        assert_eq!(a.terminal_palette(true, &[]), Palette::kanso_dark());
        assert_eq!(a.terminal_palette(false, &[]), Palette::kanso_dark());
        assert_eq!(
            AppearanceSettings::decode(Some(r#"{"mode":"x"}"#)).mode,
            ThemeMode::System
        );
        // 语言：缺省与不认识的值都是“跟随系统”，写入后能读回。
        assert_eq!(a.language, LanguageSetting::System);
        assert_eq!(
            AppearanceSettings::decode(Some(r#"{"language":"fr"}"#)).language,
            LanguageSetting::System
        );
        let en = AppearanceSettings {
            language: LanguageSetting::En,
            ..AppearanceSettings::default()
        };
        assert_eq!(AppearanceSettings::decode(Some(&en.encode())), en);
        // 标签栏位置：缺省与不认识的值都是左侧，写入的字符串与 mac 版一致。
        assert_eq!(a.tab_bar_position, TabBarPosition::Left);
        assert_eq!(
            AppearanceSettings::decode(Some(r#"{"tabBarPosition":"bottom"}"#)).tab_bar_position,
            TabBarPosition::Left
        );
        let top = AppearanceSettings {
            tab_bar_position: TabBarPosition::Top,
            ..AppearanceSettings::default()
        };
        assert!(top.encode().contains(r#""tabBarPosition":"top""#));
        assert_eq!(AppearanceSettings::decode(Some(&top.encode())), top);

        let t = TimelineSettings::decode(Some(r#"{"mode":"timestamps","lastVisible":"off"}"#));
        assert_eq!(t.mode, GutterMode::Timestamps);
        assert_eq!(t.last_visible, GutterMode::Both);
        let mut t = TimelineSettings::default();
        t.set_mode(GutterMode::LineNumbers);
        t.set_mode(GutterMode::Off);
        assert_eq!(t.last_visible, GutterMode::LineNumbers);
        assert_eq!(TimelineSettings::decode(Some(&t.encode())), t);
    }

    #[test]
    fn theme_mode_resolves_dark() {
        assert!(ThemeMode::System.is_dark(true));
        assert!(!ThemeMode::System.is_dark(false));
        assert!(!ThemeMode::Light.is_dark(true));
        assert!(ThemeMode::Dark.is_dark(false));
    }

    #[test]
    fn store_persists_and_pushes_options() {
        let mem = Mem::default();
        let mut s = SettingsStore::load(&mem);
        assert_eq!(mem.options.borrow().len(), 1); // 载入即下发
        assert_eq!(
            s.update_display(|d| d.font_size = 18.0),
            Some(Section::Display)
        );
        assert_eq!(s.update_display(|d| d.font_size = 18.0), None); // 没变不写
        assert_eq!(mem.options.borrow().len(), 2);
        assert!(mem.map.borrow()["display"].contains("\"fontSize\":18"));
        assert_eq!(
            s.update_appearance(|a| a.mode = ThemeMode::Light),
            Some(Section::Appearance)
        );
        s.update_timeline(|t| t.set_mode(GutterMode::Both));

        // 重启：新 store 读到同样的值。
        let s2 = SettingsStore::load(&mem);
        assert_eq!(s2.display().font_size, 18.0);
        assert_eq!(s2.appearance().mode, ThemeMode::Light);
        assert_eq!(s2.timeline().mode, GutterMode::Both);
    }

    #[test]
    fn zoom_clamps_and_snaps() {
        let mut d = DisplaySettings::default();
        assert_eq!(d.zoomed(1.0), 15.0);
        d.font_size = 36.0;
        assert_eq!(d.zoomed(1.0), 36.0);
        d.font_size = 9.0;
        assert_eq!(d.zoomed(-1.0), 9.0);
        assert_eq!(snap_line_height(1.1 + 0.1), 1.2);
        assert!((snap_line_height(1.23) - 1.25).abs() < 1e-9);
        assert_eq!(snap_line_height(5.0), 2.0);
    }

    #[test]
    fn monospace_detection() {
        assert!(is_monospace(&[8.4, 8.4, 8.4, 8.4]));
        assert!(!is_monospace(&[3.0, 11.0, 14.0, 4.0]));
        assert!(!is_monospace(&[]));
        assert!(!is_monospace(&[0.0, 0.0]));
    }

    #[test]
    fn font_name_cleaning_and_nerd_pick() {
        let v = clean_font_names(
            [
                "Zed",
                ".SystemUIFont",
                "@Vertical",
                "arial",
                "",
                "Zed",
                "Cascadia",
            ]
            .map(String::from),
        );
        assert_eq!(v, ["arial", "Cascadia", "Zed"]);
        let installed = ["Arial", "FiraCode Nerd Font", "Hack Nerd Font Mono"].map(String::from);
        assert_eq!(
            pick_nerd_fallback(&installed).as_deref(),
            Some("Hack Nerd Font Mono")
        );
        let with_sym = ["Symbols Nerd Font Mono", "Hack Nerd Font Mono"].map(String::from);
        assert_eq!(
            pick_nerd_fallback(&with_sym).as_deref(),
            Some("Symbols Nerd Font Mono")
        );
        assert_eq!(pick_nerd_fallback(&["Arial".to_owned()]), None);
    }

    #[test]
    fn keybindings_persist_and_reset_restores_everything() {
        let mem = Mem::default();
        let mut store = SettingsStore::load(&mem);
        let section = store.update_keybindings(|k| {
            k.overrides.insert("find".into(), "ctrl+f".into());
        });
        assert_eq!(section, Some(Section::Keybindings));
        assert!(mem.map.borrow().contains_key("keybindings-win"));
        // 值没变：不写、不通知。
        assert_eq!(
            store.update_keybindings(|k| {
                k.overrides.insert("find".into(), "ctrl+f".into());
            }),
            None
        );
        store.update_display(|d| d.font_size = 20.0);
        let reloaded = SettingsStore::load(&mem);
        assert_eq!(reloaded.keybindings().overrides.len(), 1);
        store.reset_to_defaults().unwrap();
        assert!(mem.map.borrow().is_empty());
        assert!(store.keybindings().overrides.is_empty());
        assert_eq!(
            store.display().font_size,
            DisplaySettings::default().font_size
        );
    }

    #[test]
    fn mouse_decode_is_lenient_and_round_trips() {
        assert_eq!(MouseSettings::decode(None), MouseSettings::default());
        assert_eq!(MouseSettings::decode(Some("{}")), MouseSettings::default());
        // 未知枚举值回落默认；类型不对的字段回落默认，其余字段照常读。
        let m = MouseSettings::decode(Some(
            r#"{"v":3,"middleClick":"scroll","rightClick":7,"copyOnSelect":true,"future":1}"#,
        ));
        assert_eq!(m.middle_click, MiddleClick::Paste);
        assert_eq!(m.right_click, RightClick::Menu);
        assert!(m.copy_on_select);
        let m = MouseSettings {
            middle_click: MiddleClick::None,
            right_click: RightClick::CopyOrPaste,
            copy_on_select: true,
        };
        assert_eq!(MouseSettings::decode(Some(&m.encode())), m);
        let v: Value = serde_json::from_str(&m.encode()).unwrap();
        assert_eq!(
            v,
            json!({"v":1,"middleClick":"none","rightClick":"copyOrPaste","copyOnSelect":true})
        );
    }

    #[test]
    fn mouse_migrates_legacy_right_click_once() {
        // 无 mouse + 旧字段 false -> 复制或粘贴，并写回。
        let mem = Mem::default();
        mem.map.borrow_mut().insert(
            "windows.panels".into(),
            r#"{"right_click_menu":false}"#.into(),
        );
        let s = SettingsStore::load(&mem);
        assert_eq!(s.mouse().right_click, RightClick::CopyOrPaste);
        assert!(mem.map.borrow()["mouse"].contains("copyOrPaste"));

        // 无 mouse + 无旧字段 -> 默认 Menu。
        let mem = Mem::default();
        mem.map.borrow_mut().insert(
            "windows.panels".into(),
            r#"{"sidebar_visible":true}"#.into(),
        );
        let s = SettingsStore::load(&mem);
        assert_eq!(s.mouse().right_click, RightClick::Menu);
        let mem = Mem::default();
        assert_eq!(
            SettingsStore::load(&mem).mouse().right_click,
            RightClick::Menu
        );

        // 已有 mouse：忽略旧字段。
        let mem = Mem::default();
        mem.map.borrow_mut().insert(
            "windows.panels".into(),
            r#"{"right_click_menu":false}"#.into(),
        );
        mem.map.borrow_mut().insert(
            "mouse".into(),
            r#"{"v":1,"middleClick":"none","rightClick":"menu","copyOnSelect":false}"#.into(),
        );
        let s = SettingsStore::load(&mem);
        assert_eq!(s.mouse().right_click, RightClick::Menu);
        assert_eq!(s.mouse().middle_click, MiddleClick::None);
    }

    #[test]
    fn mouse_migrates_copy_on_select_from_display() {
        // 1.0.7 存在 display.copyOnSelect 里的开关要带过来，右键保持默认。
        let mem = Mem::default();
        mem.map
            .borrow_mut()
            .insert("display".into(), r#"{"v":1,"copyOnSelect":true}"#.into());
        let s = SettingsStore::load(&mem);
        assert!(s.mouse().copy_on_select);
        assert_eq!(s.mouse().right_click, RightClick::Menu);
        assert!(mem.map.borrow()["mouse"].contains(r#""copyOnSelect":true"#));

        // 两个旧字段同时存在：都迁移。
        let mem = Mem::default();
        mem.map
            .borrow_mut()
            .insert("display".into(), r#"{"copyOnSelect":true}"#.into());
        mem.map.borrow_mut().insert(
            "windows.panels".into(),
            r#"{"right_click_menu":false}"#.into(),
        );
        let s = SettingsStore::load(&mem);
        assert!(s.mouse().copy_on_select);
        assert_eq!(s.mouse().right_click, RightClick::CopyOrPaste);
    }

    #[test]
    fn mouse_update_persists_and_reset_restores() {
        let mem = Mem::default();
        let mut s = SettingsStore::load(&mem);
        assert_eq!(
            s.update_mouse(|m| m.copy_on_select = true),
            Some(Section::Mouse)
        );
        assert_eq!(s.update_mouse(|m| m.copy_on_select = true), None);
        assert!(SettingsStore::load(&mem).mouse().copy_on_select);
        s.reset_to_defaults().unwrap();
        assert_eq!(*s.mouse(), MouseSettings::default());
    }

    #[test]
    fn click_actions_follow_spec() {
        use ClickAction::*;
        // 上报且没按 Shift：交给程序，不看设置。
        assert_eq!(
            right_click_action(RightClick::Menu, true, false, true),
            Report
        );
        assert_eq!(middle_click_action(MiddleClick::None, true, false), Report);
        // Shift 绕过上报。
        assert_eq!(
            right_click_action(RightClick::Menu, true, true, false),
            Menu
        );
        assert_eq!(middle_click_action(MiddleClick::Paste, true, true), Paste);
        assert_eq!(middle_click_action(MiddleClick::None, true, true), Nothing);
        // 非上报。
        let cp = RightClick::CopyOrPaste;
        assert_eq!(right_click_action(cp, false, false, true), Copy);
        assert_eq!(right_click_action(cp, false, false, false), Paste);
        assert_eq!(
            right_click_action(RightClick::Menu, false, false, true),
            Menu
        );
        assert_eq!(middle_click_action(MiddleClick::Paste, false, false), Paste);
        assert_eq!(
            middle_click_action(MiddleClick::None, false, false),
            Nothing
        );
    }

    #[test]
    fn copy_on_select_needs_local_nonempty_selection() {
        assert!(should_copy_on_select(true, true, true));
        assert!(!should_copy_on_select(false, true, true));
        assert!(!should_copy_on_select(true, false, true)); // 按下时被上报接管
        assert!(!should_copy_on_select(true, true, false)); // 单击没有产生选区
    }
}
