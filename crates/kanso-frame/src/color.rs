//! 颜色的 `u32` 编码：高 8 位标签，低 24 位负载。
//!
//! 颜色不预先解析成具体像素值，切换主题只需更换调色板（architecture §5、§11）。

use crate::layout::{
    COLOR_TAG_DEFAULT, COLOR_TAG_INDEXED, COLOR_TAG_RGB, COLOR_TAG_SEMANTIC, COLOR_TAG_SHIFT,
};
use kanso_types::geometry::{Rgb, TermColor};

/// 解码后的颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodedColor {
    /// 默认色，前景还是背景由所在字段决定。
    Default,
    /// 调色板索引。
    Indexed(u8),
    /// 直接指定的 RGB。
    Rgb(Rgb),
    /// 语义着色类别，渲染器按当前主题映射成颜色（terminal.md §7）。
    Semantic(u8),
}

/// 颜色编解码的命名空间，不含状态。
pub struct Color;

impl Color {
    /// 把终端颜色编码为 `u32`。`Default` 编码为 0，所以零初始化的单元格就是默认色。
    pub fn encode(color: TermColor) -> u32 {
        match color {
            TermColor::Default => COLOR_TAG_DEFAULT << COLOR_TAG_SHIFT,
            TermColor::Indexed(i) => (COLOR_TAG_INDEXED << COLOR_TAG_SHIFT) | u32::from(i),
            TermColor::Rgb(Rgb { r, g, b }) => {
                (COLOR_TAG_RGB << COLOR_TAG_SHIFT)
                    | (u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b)
            }
        }
    }

    /// 编码语义着色类别。只应用于 `fg`，且只在程序没设前景色时由 core 使用。
    pub fn semantic(class: u8) -> u32 {
        (COLOR_TAG_SEMANTIC << COLOR_TAG_SHIFT) | u32::from(class)
    }

    /// 解码。未知标签按默认色处理：这样旧读取方遇到新版本追加的标签时退化为默认色而不是出错。
    pub fn decode(value: u32) -> DecodedColor {
        let payload = value & 0x00FF_FFFF;
        match value >> COLOR_TAG_SHIFT {
            COLOR_TAG_INDEXED => DecodedColor::Indexed(payload as u8),
            COLOR_TAG_RGB => DecodedColor::Rgb(Rgb {
                r: (payload >> 16) as u8,
                g: (payload >> 8) as u8,
                b: payload as u8,
            }),
            COLOR_TAG_SEMANTIC => DecodedColor::Semantic(payload as u8),
            _ => DecodedColor::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for c in [
            TermColor::Default,
            TermColor::Indexed(0),
            TermColor::Indexed(255),
            TermColor::Rgb(Rgb { r: 1, g: 2, b: 3 }),
            TermColor::Rgb(Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
        ] {
            let d = Color::decode(Color::encode(c));
            let expect = match c {
                TermColor::Default => DecodedColor::Default,
                TermColor::Indexed(i) => DecodedColor::Indexed(i),
                TermColor::Rgb(r) => DecodedColor::Rgb(r),
            };
            assert_eq!(d, expect);
        }
        for class in [0u8, 1, 255] {
            assert_eq!(
                Color::decode(Color::semantic(class)),
                DecodedColor::Semantic(class)
            );
        }
    }

    #[test]
    fn default_is_zero_and_unknown_tag_is_default() {
        assert_eq!(Color::encode(TermColor::Default), 0);
        assert_eq!(Color::decode(0x7F00_0001), DecodedColor::Default);
    }
}
