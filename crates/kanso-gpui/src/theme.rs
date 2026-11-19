//! 终端配色。
//!
//! 颜色来源是 `design/terminal-themes/*.json`（与 macOS 前端共用同一份），编译时嵌入。
//! 帧里的颜色只存标签与索引（docs/frame-abi.md §4），切换主题只需要换 [`Palette`]。

use kanso_core::terminal_themes::TerminalTheme;

/// 8 位 RGB 颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb8 {
    /// 红。
    pub r: u8,
    /// 绿。
    pub g: u8,
    /// 蓝。
    pub b: u8,
}

impl Rgb8 {
    /// 构造。
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb8 { r, g, b }
    }

    /// 解析 `#RRGGBB`（`#` 可省略）。
    pub fn from_hex(s: &str) -> Option<Rgb8> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.is_ascii() {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Rgb8::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// `0xRRGGBB`。
    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// 与 Mac 终端一致，在 sRGB 分量上插值，再四舍五入到 8 位。
    pub fn mixed(self, other: Rgb8, amount: f32) -> Rgb8 {
        let lerp =
            |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
        Rgb8::new(
            lerp(self.r, other.r),
            lerp(self.g, other.g),
            lerp(self.b, other.b),
        )
    }

    /// 向 `other` 混合：`t` 为 0..=255，0 得到自己，255 得到 `other`。
    pub fn mix(self, other: Rgb8, t: u8) -> Rgb8 {
        let t = u16::from(t);
        let m = |a: u8, b: u8| ((u16::from(a) * (255 - t) + u16::from(b) * t + 127) / 255) as u8;
        Rgb8::new(m(self.r, other.r), m(self.g, other.g), m(self.b, other.b))
    }
}

/// 带不透明度的叠加色（0..=255）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    /// 颜色。
    pub rgb: Rgb8,
    /// 不透明度。
    pub alpha: u8,
}

/// 终端装饰色（时间戳栏、命令分隔线、选中的命令块），取自 `design/tokens.json` 的 `terminal.*` token，
/// 随主题深浅切换；主题 JSON 里没有这些项，按背景明暗选一组。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decorations {
    /// 提示符标签的底色和描边。
    pub chip_background: Rgb8,
    /// 提示符标签描边。
    pub chip_border: Rgb8,
    /// 时间戳 / 行号栏文字（`terminal.gutterText`）。
    pub gutter_text: Rgb8,
    /// 栏内光标所在行的高亮底色（`terminal.gutterCursorLine`）。
    pub gutter_cursor_line: Overlay,
    /// 命令分隔线与栏边线（`terminal.commandSeparator`）。
    pub command_separator: Overlay,
    /// 选中命令块的整行底色（`terminal.blockSelection`）。
    pub block_selection: Rgb8,
}

impl Decorations {
    fn for_background(background: Rgb8) -> Decorations {
        let dark =
            u16::from(background.r) + u16::from(background.g) + u16::from(background.b) < 3 * 128;
        if dark {
            Decorations {
                chip_background: Rgb8::new(0x12, 0x12, 0x14),
                chip_border: Rgb8::new(0x2A, 0x2A, 0x2C),
                gutter_text: Rgb8::new(0x8E, 0x94, 0x9B),
                gutter_cursor_line: Overlay {
                    rgb: Rgb8::new(0x62, 0xB0, 0xD2),
                    alpha: 0x26,
                },
                command_separator: Overlay {
                    rgb: Rgb8::new(0xEC, 0xED, 0xEF),
                    alpha: 0x26,
                },
                block_selection: Rgb8::new(0x20, 0x34, 0x3E),
            }
        } else {
            Decorations {
                chip_background: Rgb8::new(0xF1, 0xF1, 0xEC),
                chip_border: Rgb8::new(0xD5, 0xD5, 0xCF),
                gutter_text: Rgb8::new(0x6A, 0x6F, 0x76),
                gutter_cursor_line: Overlay {
                    rgb: Rgb8::new(0x2A, 0x6A, 0x8A),
                    alpha: 0x1F,
                },
                command_separator: Overlay {
                    rgb: Rgb8::new(0x1B, 0x1D, 0x20),
                    alpha: 0x26,
                },
                block_selection: Rgb8::new(0xDF, 0xEB, 0xEF),
            }
        }
    }
}

/// 一套终端配色。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    /// 默认前景。
    pub foreground: Rgb8,
    /// 默认背景。
    pub background: Rgb8,
    /// 光标色。
    pub cursor: Rgb8,
    /// 块状光标下文字的颜色。
    pub cursor_text: Rgb8,
    /// 选区背景。
    pub selection_background: Rgb8,
    /// ANSI 16 色。
    pub ansi: [Rgb8; 16],
    /// 语义着色类别色（下标即类别号，docs/semantic-color.md）。
    pub semantic: Vec<Rgb8>,
    /// 时间戳栏、命令分隔线与命令块的装饰色。
    pub decorations: Decorations,
}

const DARK_JSON: &str = include_str!("../../../design/terminal-themes/kanso-dark.json");
const LIGHT_JSON: &str = include_str!("../../../design/terminal-themes/kanso-light.json");

impl Palette {
    /// 从主题 JSON 解析。
    pub fn from_json(json: &str) -> Result<Palette, String> {
        TerminalTheme::parse(String::new(), json).map(|t| Self::from_theme(&t))
    }

    /// 将共享读取器的颜色转换为 GPUI 渲染色。
    pub fn from_theme(t: &TerminalTheme) -> Palette {
        let rgb = |c: u32| Rgb8::new((c >> 16) as u8, (c >> 8) as u8, c as u8);
        let background = rgb(t.background);
        Palette {
            foreground: rgb(t.foreground),
            background,
            decorations: Decorations::for_background(background),
            cursor: rgb(t.cursor),
            cursor_text: rgb(t.cursor_text),
            selection_background: rgb(t.selection_background),
            ansi: t.ansi.map(rgb),
            semantic: t.semantic.into_iter().map(rgb).collect(),
        }
    }

    /// Kanso Dark。
    pub fn kanso_dark() -> Palette {
        Palette::from_json(DARK_JSON).expect("内置深色主题必须合法")
    }

    /// Kanso Light。
    pub fn kanso_light() -> Palette {
        Palette::from_json(LIGHT_JSON).expect("内置浅色主题必须合法")
    }

    /// 256 色调色板：0..16 取主题 ANSI，16..232 为 6×6×6 色块，232..256 为灰阶（xterm 约定）。
    pub fn indexed(&self, i: u8) -> Rgb8 {
        match i {
            0..=15 => self.ansi[usize::from(i)],
            16..=231 => {
                let n = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                Rgb8::new(level(n / 36), level((n / 6) % 6), level(n % 6))
            }
            232..=255 => {
                let v = 8 + 10 * (i - 232);
                Rgb8::new(v, v, v)
            }
        }
    }

    /// 语义类别色；类别号越界时退回默认前景。
    pub fn semantic_color(&self, class: u8) -> Rgb8 {
        self.semantic
            .get(usize::from(class))
            .copied()
            .unwrap_or(self.foreground)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_themes_parse() {
        let d = Palette::kanso_dark();
        assert_eq!(d.background, Rgb8::new(0x05, 0x05, 0x05));
        assert_eq!(d.ansi[1], Rgb8::new(0xE5, 0x64, 0x5A));
        assert_eq!(d.semantic.len(), 13);
        let l = Palette::kanso_light();
        assert_ne!(l.background, d.background);
    }

    #[test]
    fn decorations_follow_the_background() {
        let d = Palette::kanso_dark().decorations;
        let l = Palette::kanso_light().decorations;
        assert_eq!(d.block_selection, Rgb8::new(0x20, 0x34, 0x3E));
        assert_eq!(l.block_selection, Rgb8::new(0xDF, 0xEB, 0xEF));
        assert_eq!(d.chip_background, Rgb8::new(0x12, 0x12, 0x14));
        assert_eq!(d.chip_border, Rgb8::new(0x2A, 0x2A, 0x2C));
        assert_eq!(l.chip_background, Rgb8::new(0xF1, 0xF1, 0xEC));
        assert_eq!(l.chip_border, Rgb8::new(0xD5, 0xD5, 0xCF));
        assert_ne!(d.gutter_text, l.gutter_text);
        assert_eq!(d.command_separator.alpha, 0x26);
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(Rgb8::from_hex("#0B6A8A"), Some(Rgb8::new(0x0B, 0x6A, 0x8A)));
        assert_eq!(Rgb8::from_hex("0b6a8a"), Some(Rgb8::new(0x0B, 0x6A, 0x8A)));
        assert_eq!(Rgb8::from_hex("#12345"), None);
        assert_eq!(Rgb8::from_hex("#GGGGGG"), None);
        assert_eq!(Rgb8::from_hex("#é12345"), None);
        assert_eq!(Rgb8::new(1, 2, 3).to_u32(), 0x010203);
    }

    #[test]
    fn xterm_cube_and_gray() {
        let p = Palette::kanso_dark();
        assert_eq!(p.indexed(16), Rgb8::new(0, 0, 0));
        assert_eq!(p.indexed(231), Rgb8::new(255, 255, 255));
        assert_eq!(p.indexed(196), Rgb8::new(255, 0, 0));
        assert_eq!(p.indexed(232), Rgb8::new(8, 8, 8));
        assert_eq!(p.indexed(255), Rgb8::new(238, 238, 238));
        assert_eq!(p.indexed(2), p.ansi[2]);
    }

    #[test]
    fn mix_endpoints() {
        let a = Rgb8::new(0, 100, 200);
        let b = Rgb8::new(255, 0, 0);
        assert_eq!(a.mix(b, 0), a);
        assert_eq!(a.mix(b, 255), b);
    }

    #[test]
    fn semantic_out_of_range_falls_back() {
        let p = Palette::kanso_dark();
        assert_eq!(p.semantic_color(200), p.foreground);
        assert_eq!(p.semantic_color(0), p.semantic[0]);
    }

    #[test]
    fn rejects_bad_ansi_count() {
        let bad = r##"{"foreground":"#000000","background":"#000000","cursor":"#000000","cursorText":"#000000","selectionBackground":"#000000","ansi":["#000000"],"semantic":[]}"##;
        assert!(Palette::from_json(bad).is_err());
    }
}
