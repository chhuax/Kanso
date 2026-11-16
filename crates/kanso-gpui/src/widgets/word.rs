//! 词边界：Ctrl+←/→ 按词移动、双击选词、Ctrl+Backspace / Delete 按词删除。
//!
//! 基于 Unicode UAX #29 的词切分。含字母或数字的片段算“词”，纯空白与标点片段在移动时被跳过。
//! 中日韩文字没有词典，切分结果是逐字，所以 Ctrl+方向键在中文里等于逐字移动，这是可接受的退化。
//! 所有偏移都是 UTF-8 字节偏移，且总落在字符边界上。

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

fn is_word(seg: &str) -> bool {
    seg.chars().any(char::is_alphanumeric)
}

/// 光标左侧最近一个词的起点；左边没有词时返回 0。
pub fn prev_word_start(s: &str, offset: usize) -> usize {
    s.split_word_bound_indices()
        .rev()
        .find(|(i, seg)| *i < offset && is_word(seg))
        .map_or(0, |(i, _)| i)
}

/// 光标右侧下一个词的起点（Windows 习惯：落在下一个词的开头）；右边没有词时返回末尾。
pub fn next_word_start(s: &str, offset: usize) -> usize {
    s.split_word_bound_indices()
        .find(|(i, seg)| *i > offset && is_word(seg))
        .map_or(s.len(), |(i, _)| i)
}

/// `offset` 所在的词（或空白 / 标点片段）的范围，用于双击选词。`offset` 在末尾时取最后一个片段。
pub fn word_range_at(s: &str, offset: usize) -> Range<usize> {
    if s.is_empty() {
        return 0..0;
    }
    let mut last = 0..0;
    for (i, seg) in s.split_word_bound_indices() {
        let r = i..i + seg.len();
        if offset < r.end {
            return r;
        }
        last = r;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_right_lands_on_next_word_start() {
        let s = "hello  world, foo";
        assert_eq!(next_word_start(s, 0), 7);
        assert_eq!(next_word_start(s, 7), 14);
        assert_eq!(next_word_start(s, 14), s.len());
        assert_eq!(next_word_start(s, s.len()), s.len());
    }

    #[test]
    fn move_left_lands_on_word_start() {
        let s = "hello  world, foo";
        assert_eq!(prev_word_start(s, s.len()), 14);
        assert_eq!(prev_word_start(s, 14), 7);
        assert_eq!(prev_word_start(s, 9), 7);
        assert_eq!(prev_word_start(s, 7), 0);
        assert_eq!(prev_word_start(s, 0), 0);
    }

    #[test]
    fn punctuation_is_skipped() {
        // UAX #29 不在 “a.b” 的点处切开（MidNumLet），整体算一个词。
        assert_eq!(next_word_start("a.b", 0), 3);
        assert_eq!(next_word_start("foo, bar", 3), 5);
    }

    #[test]
    fn double_click_selects_word() {
        let s = "hello world";
        assert_eq!(word_range_at(s, 2), 0..5);
        assert_eq!(word_range_at(s, 5), 5..6);
        assert_eq!(word_range_at(s, 6), 6..11);
        assert_eq!(word_range_at(s, 11), 6..11);
        assert_eq!(word_range_at("", 0), 0..0);
    }

    #[test]
    fn cjk_stays_on_char_boundaries() {
        let s = "你好 world";
        let n = next_word_start(s, 0);
        assert!(s.is_char_boundary(n));
        let p = prev_word_start(s, s.len());
        assert_eq!(p, 7);
        assert!(s.is_char_boundary(word_range_at(s, 1).start));
    }
}
