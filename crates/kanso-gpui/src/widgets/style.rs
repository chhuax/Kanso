//! 输入框的配色与尺寸，取自 `design/tokens.json`（与 macOS 前端同一份来源，编译时嵌入）。
//!
//! 取值对应关系：背景 `bg.field`、边框 `border.field`、获得焦点时边框换成 `accent`（token 说明里的约定）、
//! 文字 `text.primary`、占位符与禁用文字 `text.tertiary`、高度 `field.height`、水平内边距 `field.paddingX`、
//! 圆角 `radius.md`、字号 `font.ui.body`。禁用态背景用 `bg.panel` / 边框用 `border.subtle`，不额外造色。

use std::sync::OnceLock;

use serde_json::Value;

const TOKENS_JSON: &str = include_str!("../../../../design/tokens.json");

/// 外观。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Appearance {
    /// 浅色。
    Light,
    /// 深色。
    #[default]
    Dark,
}

/// 带透明度的 8 位 RGBA。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    /// 红。
    pub r: u8,
    /// 绿。
    pub g: u8,
    /// 蓝。
    pub b: u8,
    /// 不透明度（255 为不透明）。
    pub a: u8,
}

impl Rgba8 {
    /// 解析 `#RRGGBB` 或 `#RRGGBBAA`。
    pub fn from_hex(s: &str) -> Option<Rgba8> {
        let s = s.strip_prefix('#')?;
        if !s.is_ascii() {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        match s.len() {
            6 => Some(Rgba8 {
                r: (v >> 16) as u8,
                g: (v >> 8) as u8,
                b: v as u8,
                a: 255,
            }),
            8 => Some(Rgba8 {
                r: (v >> 24) as u8,
                g: (v >> 16) as u8,
                b: (v >> 8) as u8,
                a: v as u8,
            }),
            _ => None,
        }
    }

    /// `0xRRGGBBAA`。
    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 24) | ((self.g as u32) << 16) | ((self.b as u32) << 8) | self.a as u32
    }

    /// 换一个不透明度。
    pub const fn with_alpha(self, a: u8) -> Rgba8 {
        Rgba8 { a, ..self }
    }
}

fn tokens() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(TOKENS_JSON).expect("design/tokens.json 必须是合法 JSON"))
}

/// 取颜色 token；token 不存在或格式不对时 panic（内嵌文件由 `cargo xtask check-tokens` 校验，不会发生）。
pub fn color(name: &str, appearance: Appearance) -> Rgba8 {
    let key = match appearance {
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    };
    tokens()["color"][name][key]
        .as_str()
        .and_then(Rgba8::from_hex)
        .unwrap_or_else(|| panic!("缺少颜色 token {name}.{key}"))
}

/// 取数值 token（`spacing` / `radius` / `font` 三组之一）。
pub fn number(group: &str, name: &str) -> f32 {
    tokens()[group][name]
        .as_f64()
        .unwrap_or_else(|| panic!("缺少数值 token {group}.{name}")) as f32
}

/// 输入框的完整样式。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldStyle {
    /// 背景。
    pub bg: Rgba8,
    /// 边框。
    pub border: Rgba8,
    /// 获得焦点时的边框。
    pub border_focus: Rgba8,
    /// 焦点环（边框外一圈的半透明强调色）。
    pub ring: Rgba8,
    /// 文字。
    pub text: Rgba8,
    /// 占位文字。
    pub placeholder: Rgba8,
    /// 选区底色。
    pub selection: Rgba8,
    /// 光标。
    pub caret: Rgba8,
    /// 禁用态背景。
    pub disabled_bg: Rgba8,
    /// 禁用态边框。
    pub disabled_border: Rgba8,
    /// 禁用态文字。
    pub disabled_text: Rgba8,
    /// 校验失败的边框（预留给表单）。
    pub error: Rgba8,
    /// 控件高度（pt）。
    pub height: f32,
    /// 水平内边距（pt）。
    pub padding_x: f32,
    /// 圆角（pt）。
    pub radius: f32,
    /// 字号（pt）。
    pub font_size: f32,
    /// 焦点环宽度（pt）。
    pub ring_width: f32,
}

impl FieldStyle {
    /// 取指定外观的样式。
    pub fn new(appearance: Appearance) -> FieldStyle {
        let c = |n: &str| color(n, appearance);
        let accent = c("accent");
        FieldStyle {
            bg: c("bg.field"),
            border: c("border.field"),
            border_focus: accent,
            ring: accent.with_alpha(0x4D),
            text: c("text.primary"),
            placeholder: c("text.tertiary"),
            selection: accent.with_alpha(0x55),
            caret: c("text.primary"),
            disabled_bg: c("bg.panel"),
            disabled_border: c("border.subtle"),
            disabled_text: c("text.tertiary"),
            error: c("status.error"),
            height: number("spacing", "field.height"),
            padding_x: number("spacing", "field.paddingX"),
            radius: number("radius", "md"),
            font_size: number("font", "ui.body"),
            ring_width: 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_and_without_alpha() {
        assert_eq!(
            Rgba8::from_hex("#0A7C98"),
            Some(Rgba8 {
                r: 0x0A,
                g: 0x7C,
                b: 0x98,
                a: 255
            })
        );
        assert_eq!(
            Rgba8::from_hex("#00000080").map(|c| (c.r, c.a)),
            Some((0, 0x80))
        );
        assert_eq!(Rgba8::from_hex("0A7C98"), None);
        assert_eq!(Rgba8::from_hex("#12345"), None);
        assert_eq!(Rgba8::from_hex("#é12345"), None);
        assert_eq!(Rgba8::from_hex("#0A7C98").unwrap().to_u32(), 0x0A7C98FF);
    }

    #[test]
    fn field_style_matches_tokens() {
        let l = FieldStyle::new(Appearance::Light);
        assert_eq!(l.bg, Rgba8::from_hex("#FFFFFF").unwrap());
        assert_eq!(l.border, Rgba8::from_hex("#D2D2CC").unwrap());
        assert_eq!(l.border_focus, Rgba8::from_hex("#0A7C98").unwrap());
        assert_eq!(l.height, 30.0);
        assert_eq!(l.padding_x, 10.0);
        assert_eq!(l.radius, 6.0);
        assert_eq!(l.font_size, 13.0);
        let d = FieldStyle::new(Appearance::Dark);
        assert_eq!(d.bg, Rgba8::from_hex("#0F0F0F").unwrap());
        assert_eq!(d.border_focus, Rgba8::from_hex("#15B8E0").unwrap());
        assert_ne!(l.text, d.text);
    }

    #[test]
    fn every_style_color_is_defined_in_both_appearances() {
        for a in [Appearance::Light, Appearance::Dark] {
            let s = FieldStyle::new(a);
            assert_eq!(s.ring.a, 0x4D);
            assert_eq!(s.bg.a, 255);
        }
    }
}
