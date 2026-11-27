use std::collections::BTreeMap;
use std::sync::LazyLock;

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use regex::Regex;

const NONE: u8 = u8::MAX;
const MARGIN: i32 = 4;
const MAX_CELLS: usize = 4096;

static RULES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?P<string>"[^"\r\n]*"|'[^'\r\n]*')|"#,
        r#"(?P<path>(?:[A-Za-z]:\\|~/|\.{1,2}/|/|\b[\w.-]+/)[^\s"'<>]+)|"#,
        r"(?P<filename>\b[\w-][\w.-]*\.(?:rs|swift|js|jsx|ts|tsx|mjs|json|toml|yaml|yml|log|txt|md|sh|py|go|java|kt|c|h|cpp|hpp|rb|sql|html|css|scss|xml|lock|conf|ini|csv|pdf|png|jpg|jpeg|gif|svg|zip|tar|gz|tgz|dmg)\b)|",
        r#"(?P<address>\b(?:https?|ssh|ftp)://[^\s<>"']+|\b[\w.+-]+@[\w.-]+\.[A-Za-z]{2,}\b|\b(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\b|\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b|(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}\b|[0-9A-Fa-f:]*::[0-9A-Fa-f:]*|\b(?:[a-zA-Z0-9-]+\.)+[a-zA-Z]{2,}\b|\b[a-zA-Z][a-zA-Z0-9]*-[0-9]+\b)|"#,
        r"(?P<identity>[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}|[0-9a-fA-F]{7,64})|",
        r"(?P<time>\b[0-9]{4}-[0-9]{2}-[0-9]{2}(?:[T ][0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z?)?\b|\b[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?\b|\bv?[0-9]+\.[0-9]+\.[0-9]+(?:[-+][\w.-]+)?\b)|",
        r"(?P<permission>[-dlcbps][-rwxstST]{9})|",
        r"(?P<option>\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*\b|\b[A-Z_][A-Z0-9_]*=|--[A-Za-z][\w-]*\b|-[A-Za-z]\b)|",
        r"(?P<key>\b[A-Za-z_][\w.-]*[=:])|",
        r"(?P<positive>\b(?:INFO|DEBUG|TRACE|OK|SUCCESS|PASS|PASSED|added|new\ file)\b)|",
        r"(?P<negative>\b(?:ERROR|FATAL|FAIL|FAILED|PANIC|deleted|conflict)\b)|",
        r"(?P<warning>\b(?:WARN|WARNING|modified|renamed)\b)|",
        r"(?P<number>\b0x[0-9a-fA-F]+\b|\b[0-9]+(?:\.[0-9]+)?(?:%|[KMGTPE]i?B|[numk]?s|[KMGT]B)?\b%?)"
    ))
    .expect("语义着色规则有效")
});

const CLASSES: [(&str, u8); 13] = [
    ("path", 0),
    ("string", 1),
    ("number", 2),
    ("time", 3),
    ("identity", 4),
    ("address", 5),
    ("option", 6),
    ("key", 7),
    ("permission", 8),
    ("positive", 9),
    ("negative", 10),
    ("warning", 11),
    ("filename", 12),
];

struct CachedLine {
    text: String,
    spans: Vec<(usize, usize, u8)>,
}

#[derive(Default)]
pub(crate) struct SemanticColors {
    cache: BTreeMap<u64, CachedLine>,
    #[cfg(test)]
    calculations: usize,
}

fn recognize(text: &str) -> Vec<(usize, usize, u8)> {
    let trimmed = text.trim();
    let permission = text.split_whitespace().next().is_some_and(|word| {
        word.len() == 10 && word.bytes().all(|byte| b"-dlcbpsrwxstST".contains(&byte))
    });
    let status = if (text.starts_with('+') && !text.starts_with("+++"))
        || text.starts_with("A  ")
        || text.starts_with("?? ")
    {
        Some(9)
    } else if (text.starts_with('-')
        && !text.starts_with("--")
        && text
            .split_whitespace()
            .next()
            .is_some_and(|word| word.len() != 2)
        && !permission)
        || text.starts_with("D  ")
    {
        Some(10)
    } else if text.starts_with(" M ") || text.starts_with("M  ") {
        Some(11)
    } else {
        None
    };
    if let Some(class) = status {
        return vec![(0, text.len(), class)];
    }
    if trimmed.split_whitespace().count() > 1
        && trimmed.split_whitespace().all(|word| {
            word.bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
        })
        && !trimmed
            .split_whitespace()
            .any(|word| matches!(word, "INFO" | "ERROR" | "WARN" | "WARNING"))
    {
        return vec![(0, text.len(), 7)];
    }
    RULES
        .captures_iter(text)
        .filter_map(|capture| {
            CLASSES.iter().find_map(|&(name, mut class)| {
                capture.name(name).and_then(|matched| {
                    if class == 4
                        && (text[..matched.start()]
                            .chars()
                            .next_back()
                            .is_some_and(char::is_alphanumeric)
                            || text[matched.end()..]
                                .chars()
                                .next()
                                .is_some_and(char::is_alphanumeric))
                    {
                        return None;
                    }
                    if class == 4 && matched.as_str().bytes().all(|byte| byte.is_ascii_digit()) {
                        class = 2;
                    }
                    Some((matched.start(), matched.end(), class))
                })
            })
        })
        .collect()
}

impl SemanticColors {
    pub fn frame(&mut self, grid: &Grid<Cell>, first_abs: u64) -> Vec<u8> {
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let offset = grid.display_offset() as i32;
        let top = -offset;
        let bottom = top + rows as i32;
        let lower = (top - MARGIN).max(-(grid.history_size() as i32));
        let upper = (bottom + MARGIN).min(rows as i32);
        let wraps = |line: i32| {
            grid[Line(line)][Column(cols - 1)]
                .flags
                .contains(Flags::WRAPLINE)
        };
        let mut line = top;
        while line > lower && wraps(line - 1) {
            line -= 1;
        }
        let mut colors = vec![NONE; rows * cols];
        let mut retained = BTreeMap::new();
        while line < bottom {
            let start = line;
            let incomplete_start =
                start == lower && start > -(grid.history_size() as i32) && wraps(start - 1);
            while line + 1 < upper && wraps(line) {
                line += 1;
            }
            let end = line;
            line += 1;
            if incomplete_start || wraps(end) || (end - start + 1) as usize * cols > MAX_CELLS {
                continue;
            }
            let abs = (first_abs as i64 + i64::from(start - top)) as u64;
            if (start..=end).contains(&grid.cursor.point.line.0) {
                if let Some(cached) = self.cache.remove(&abs) {
                    retained.insert(abs, cached);
                }
                continue;
            }
            let mut text = String::new();
            let mut positions = Vec::new();
            for physical in start..=end {
                let row = &grid[Line(physical)];
                for column in 0..cols {
                    let cell = &row[Column(column)];
                    if cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    let byte = text.len();
                    text.push(cell.c);
                    if let Some(extra) = cell.zerowidth() {
                        text.extend(extra);
                    }
                    if (top..bottom).contains(&physical) {
                        positions.push((byte, (physical - top) as usize * cols + column));
                    }
                }
            }
            if text.trim_end().is_empty() {
                continue;
            }
            let cached = match self.cache.remove(&abs) {
                Some(cached) if cached.text == text => cached,
                _ => {
                    #[cfg(test)]
                    {
                        self.calculations += 1;
                    }
                    let spans = recognize(text.trim_end());
                    CachedLine { text, spans }
                }
            };
            let mut span_index = 0;
            for (byte, position) in positions {
                while span_index < cached.spans.len() && cached.spans[span_index].1 <= byte {
                    span_index += 1;
                }
                if let Some(&(from, to, class)) = cached.spans.get(span_index)
                    && (from..to).contains(&byte)
                {
                    colors[position] = class;
                }
            }
            retained.insert(abs, cached);
        }
        self.cache = retained;
        colors
    }

    /// 折叠模式下的视口语义着色：按 `viewport` 给出的每一可见行绝对行号映射网格行。
    pub fn frame_viewport(
        &mut self,
        grid: &Grid<Cell>,
        screen_top: u64,
        viewport: &[Option<u64>],
    ) -> Vec<u8> {
        let cols = grid.columns();
        let rows = viewport.len();
        let min_line = -(grid.history_size() as i32);
        let max_line = grid.screen_lines() as i32 - 1;
        let wraps = |line: i32| {
            grid[Line(line)][Column(cols - 1)]
                .flags
                .contains(Flags::WRAPLINE)
        };
        let mut colors = vec![NONE; rows * cols];
        let mut retained = BTreeMap::new();
        let mut row_by_physical = BTreeMap::new();
        for (r, slot) in viewport.iter().enumerate() {
            if let Some(abs) = *slot {
                let p = (abs as i64 - screen_top as i64) as i32;
                if (min_line..=max_line).contains(&p) {
                    row_by_physical.insert(p, r);
                }
            }
        }
        let mut visited_starts = BTreeMap::new();
        for &physical in row_by_physical.keys() {
            let mut start = physical;
            let lower = (physical - MARGIN).max(min_line);
            while start > lower && wraps(start - 1) {
                start -= 1;
            }
            if visited_starts.contains_key(&start) {
                continue;
            }
            visited_starts.insert(start, ());
            let incomplete_start = start == lower && start > min_line && wraps(start - 1);
            let mut end = physical;
            let upper = (physical + MARGIN).min(max_line);
            while end < upper && wraps(end) {
                end += 1;
            }
            if incomplete_start || wraps(end) || (end - start + 1) as usize * cols > MAX_CELLS {
                continue;
            }
            let abs = (screen_top as i64 + i64::from(start)) as u64;
            if (start..=end).contains(&grid.cursor.point.line.0) {
                if let Some(cached) = self.cache.remove(&abs) {
                    retained.insert(abs, cached);
                }
                continue;
            }
            let mut text = String::new();
            let mut positions = Vec::new();
            for p in start..=end {
                let row = &grid[Line(p)];
                let vis_row = row_by_physical.get(&p).copied();
                for column in 0..cols {
                    let cell = &row[Column(column)];
                    if cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    let byte = text.len();
                    text.push(cell.c);
                    if let Some(extra) = cell.zerowidth() {
                        text.extend(extra);
                    }
                    if let Some(vr) = vis_row {
                        positions.push((byte, vr * cols + column));
                    }
                }
            }
            if text.trim_end().is_empty() {
                continue;
            }
            let cached = match self.cache.remove(&abs) {
                Some(cached) if cached.text == text => cached,
                _ => {
                    #[cfg(test)]
                    {
                        self.calculations += 1;
                    }
                    let spans = recognize(text.trim_end());
                    CachedLine { text, spans }
                }
            };
            let mut span_index = 0;
            for (byte, position) in positions {
                while span_index < cached.spans.len() && cached.spans[span_index].1 <= byte {
                    span_index += 1;
                }
                if let Some(&(from, to, class)) = cached.spans.get(span_index)
                    && (from..to).contains(&byte)
                {
                    colors[position] = class;
                }
            }
            retained.insert(abs, cached);
        }
        self.cache = retained;
        colors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TermConfig, Terminal};
    use kanso_frame::{Color, DecodedColor};
    use kanso_types::geometry::GridSize;

    fn terminal(cols: u16, rows: u16) -> Terminal {
        Terminal::new(TermConfig {
            size: GridSize { cols, rows },
            scrollback_lines: 1000,
        })
    }

    fn category(terminal: &mut Terminal, row: u16, col: u16) -> DecodedColor {
        Color::decode(terminal.snapshot(None).view().unwrap().row(row)[usize::from(col)].fg)
    }

    #[test]
    fn categories_have_positive_and_negative_examples() {
        for (class, positives, negative) in [
            (
                0,
                vec!["/srv/日志/app.log", "./src/main.rs", r"C:\Users\app.txt"],
                "filename",
            ),
            (1, vec!["\"hello 世界\"", "'quoted text'"], "unquoted"),
            (2, vec!["42", "12.5", "64MiB", "75%", "0xff"], "forty-two"),
            (
                3,
                vec![
                    "2026-11-15",
                    "2026-11-15T12:34:56Z",
                    "12:34:56.123",
                    "v1.2.3",
                ],
                "yesterday",
            ),
            (
                4,
                vec!["550e8400-e29b-41d4-a716-446655440000", "a1b2c3d4"],
                "12345678",
            ),
            (
                5,
                vec![
                    "192.168.1.2",
                    "2001:db8::1",
                    "aa:bb:cc:dd:ee:ff",
                    "host-01",
                    "example.com",
                    "https://example.com/a",
                    "a@example.com",
                ],
                "localhost",
            ),
            (
                6,
                vec!["--verbose", "-x", "$HOME", "${PATH}", "PATH="],
                "verbose",
            ),
            (
                7,
                vec!["worker=", "status:", "NAME SIZE OWNER"],
                "ordinary prose",
            ),
            (8, vec!["-rwxr-xr--", "drwxr-xr-x"], "rwx"),
            (
                9,
                vec!["SUCCESS", "INFO", "+added line", "?? new.rs", "A  new.rs"],
                "unsuccessful",
            ),
            (
                10,
                vec!["ERROR", "FATAL", "-removed line", "D  old.rs"],
                "errorless",
            ),
            (11, vec!["WARN", "WARNING", " M main.rs"], "warningly"),
            (
                12,
                vec![
                    "report.json",
                    "restart-yoncode.py",
                    "mr657-阶段复核收缩说明.md",
                    "README.zh-CN.md",
                    "index.html",
                    "tsconfig.node.json",
                ],
                "filename",
            ),
        ] {
            for positive in positives {
                assert!(
                    recognize(positive)
                        .iter()
                        .any(|&(_, _, found)| found == class),
                    "{positive}: {class}"
                );
            }
            assert!(
                !recognize(negative)
                    .iter()
                    .any(|&(_, _, found)| found == class),
                "{negative}: {class}"
            );
        }
    }

    #[test]
    fn hashes_use_word_boundaries_and_plain_hyphenated_words_stay_plain() {
        let branch = "iuap-zx-daily-filter-web-picker-01a1bfd4";
        assert_eq!(recognize(branch), vec![(branch.len() - 8, branch.len(), 4)]);
        for separator in ['-', '_', '.'] {
            let text = format!("prefix{separator}01a1bfd4");
            assert_eq!(recognize(&text), vec![(7, 15, 4)], "{text}");
        }
        for text in [
            "iuap-zx-dsh-usage-host",
            "claude-import",
            "prefix01a1bfd4",
            "01a1bfd4suffix",
        ] {
            assert!(recognize(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn filenames_and_directory_paths_have_distinct_categories() {
        for text in ["restart-yoncode.py", "mr657-阶段复核收缩说明.md"] {
            assert_eq!(recognize(text), vec![(0, text.len(), 12)], "{text}");
            let path = format!("./{text}");
            assert_eq!(recognize(&path), vec![(0, path.len(), 0)], "{path}");
        }
    }

    #[test]
    fn prompt_range_preserves_shell_colors_after_execution_and_reflow() {
        let mut term = terminal(80, 8);
        term.feed(
            concat!(
                "\x1b]133;A\x07/srv/app 01a1bfd4 \x1b[32mhost-01\x1b[0m\r\n",
                "ERROR \x1b]133;B\x07echo restart-yoncode.py\r\n\x1b]133;C\x07",
                "/srv/app restart-yoncode.py\r\n\x1b]133;D;0\x07",
                "\x1b]133;A\x07$ \x1b]133;B\x07"
            )
            .as_bytes(),
            0,
        );
        for col in [0, 9] {
            assert_eq!(category(&mut term, 0, col), DecodedColor::Default);
        }
        assert_eq!(category(&mut term, 0, 18), DecodedColor::Indexed(2));
        assert_eq!(category(&mut term, 1, 0), DecodedColor::Default);
        assert_eq!(category(&mut term, 1, 11), DecodedColor::Semantic(12));
        assert_eq!(category(&mut term, 2, 0), DecodedColor::Semantic(0));
        assert_eq!(category(&mut term, 2, 9), DecodedColor::Semantic(12));
        term.resize(GridSize { cols: 40, rows: 8 });
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Default);
        assert_eq!(category(&mut term, 1, 0), DecodedColor::Default);
        assert_eq!(category(&mut term, 2, 0), DecodedColor::Semantic(0));
    }

    #[test]
    fn first_frame_preserves_ansi_and_skips_editing_logical_line() {
        let mut term = terminal(40, 5);
        term.feed(b"\x1b[31mERROR\x1b[39m 42\r\n$HOME", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Indexed(1));
        assert_eq!(category(&mut term, 0, 6), DecodedColor::Semantic(2));
        assert_eq!(category(&mut term, 1, 0), DecodedColor::Default);
        term.feed(b"\r\n", 0);
        assert_eq!(category(&mut term, 1, 0), DecodedColor::Semantic(6));
        term.set_semantic_coloring(false);
        assert_eq!(category(&mut term, 0, 6), DecodedColor::Default);
        term.set_semantic_coloring(true);
        assert_eq!(category(&mut term, 0, 6), DecodedColor::Semantic(2));
    }

    #[test]
    fn wide_characters_and_soft_wraps_keep_column_alignment() {
        let mut term = terminal(12, 6);
        term.feed("中文 /srv/日志/app.log".as_bytes(), 0);
        assert_eq!(category(&mut term, 0, 5), DecodedColor::Default);
        term.feed(b"\r\n", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Default);
        assert_eq!(category(&mut term, 0, 5), DecodedColor::Semantic(0));
        assert_eq!(category(&mut term, 1, 2), DecodedColor::Semantic(0));
        term.resize(GridSize { cols: 20, rows: 6 });
        assert_eq!(category(&mut term, 0, 5), DecodedColor::Semantic(0));
    }

    #[test]
    fn unchanged_lines_are_cached_and_changes_recomputed() {
        let mut term = terminal(30, 4);
        term.feed(b"42\r\n", 0);
        term.snapshot(None);
        let count = term.core.semantic.calculations;
        term.snapshot(None);
        term.set_focused(true);
        term.snapshot(None);
        assert_eq!(term.core.semantic.calculations, count);
        term.feed(b"\x1b[1;1H", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Default);
        term.feed(b"\x1b[2;1H", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Semantic(2));
        assert_eq!(term.core.semantic.calculations, count);
        term.feed(b"\x1b[1;1HERROR\x1b[3;1H", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Semantic(10));
        assert!(term.core.semantic.calculations > count);
    }

    #[test]
    fn fullscreen_and_overlong_lines_are_not_colored() {
        let mut term = terminal(120, 40);
        term.feed(b"ERROR\r\n", 0);
        term.feed(b"\x1b[?1049hERROR\r\n", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Default);
        term.feed(b"\x1b[?1049l", 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Semantic(10));
        term.feed(format!("{}\r\n", "1".repeat(5000)).as_bytes(), 0);
        assert_eq!(category(&mut term, 0, 0), DecodedColor::Default);
        assert!(term.core.semantic.cache.len() <= 40);
    }

    #[test]
    fn scrolling_only_classifies_visible_lines() {
        let mut term = terminal(30, 4);
        for number in 0..100 {
            term.feed(format!("{number} ERROR\r\n").as_bytes(), 0);
        }
        assert_eq!(term.core.semantic.calculations, 0);
        assert_eq!(category(&mut term, 0, 3), DecodedColor::Semantic(10));
        let count = term.core.semantic.calculations;
        assert_eq!(count, 3);
        term.scroll_display(50);
        assert_eq!(category(&mut term, 0, 3), DecodedColor::Semantic(10));
        assert_eq!(term.core.semantic.calculations - count, 4);
        assert_eq!(term.core.semantic.cache.len(), 4);
        term.snapshot(None);
        assert_eq!(term.core.semantic.calculations - count, 4);
        term.scroll_to_bottom();
        assert_eq!(category(&mut term, 0, 3), DecodedColor::Semantic(10));
        assert_eq!(term.core.semantic.cache.len(), 3);
    }

    #[test]
    fn snapshots_do_not_change_copy_or_explicit_rgb() {
        let mut term = terminal(40, 4);
        term.feed(
            "中文 42 /srv/app\r\n\x1b[38;2;0;0;0mERROR\x1b[0m\r\n".as_bytes(),
            0,
        );
        term.select_all();
        let before = term.selection_text();
        term.snapshot(None);
        assert_eq!(term.selection_text(), before);
        term.set_semantic_coloring(false);
        term.snapshot(None);
        assert_eq!(term.selection_text(), before);
        assert_eq!(
            category(&mut term, 1, 0),
            DecodedColor::Rgb(kanso_types::geometry::Rgb { r: 0, g: 0, b: 0 })
        );
    }
}
