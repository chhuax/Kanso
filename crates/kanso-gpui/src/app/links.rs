//! 链接（terminal.md §10）的纯逻辑：从屏幕文字里识别 URL、把悬停位置映射到链接，以及打开前的安全检查。
//!
//! 输出里的普通 URL 由这里识别（只在悬停时对相关的行做一次扫描，不随输出维护）；
//! OSC 8 超链接由核心按单元格给出 id，再用 `Core::hyperlink_url` 取地址，不经过这里。

/// 一行屏幕文字：每个单元格一个字符，宽字符的占位格用 `'\0'`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineText {
    /// 按列排列的字符。
    pub chars: Vec<char>,
    /// 本行软换行到下一行（URL 可以跨行）。
    pub wrapped: bool,
}

/// 命中的链接。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkHit {
    /// 地址。
    pub url: String,
    /// 覆盖的区段 `(行, 起始列, 结束列（不含）)`，软换行的链接有多段。
    pub segments: Vec<(u16, u16, u16)>,
}

impl LinkHit {
    /// 某个单元格是否在链接内。
    pub fn contains(&self, row: u16, col: u16) -> bool {
        self.segments
            .iter()
            .any(|&(r, c0, c1)| r == row && c0 <= col && col < c1)
    }
}

/// 自动识别的协议（只认明确的 `scheme://`）。
const SCHEMES: &[&str] = &["http", "https", "ftp", "ftps"];

/// 允许直接打开的协议：避免文字或 OSC 8 里的 `file:` / `ms-*:` 这类会执行本地程序的地址交给系统。
const OPENABLE: &[&str] = &["http", "https", "ftp", "ftps", "mailto"];

/// 能否交给系统默认程序打开。
pub fn is_openable(url: &str) -> bool {
    match url.split_once(':') {
        Some((scheme, rest)) => {
            !rest.is_empty()
                && OPENABLE
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(scheme.trim()))
        }
        None => false,
    }
}

fn is_url_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '\'' | '`' | '\\')
}

fn opener(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        _ => '{',
    }
}

/// 某个下标处是否以 `scheme://` 开头（协议名不区分大小写），返回 `://` 之后的下标。
fn scheme_at(chars: &[char], i: usize) -> Option<usize> {
    SCHEMES.iter().find_map(|s| {
        let end = i + s.len();
        let head_ok = chars.get(i..end).is_some_and(|h| {
            h.iter()
                .zip(s.chars())
                .all(|(a, b)| a.to_ascii_lowercase() == b)
        });
        let sep_ok = chars.get(end..end + 3) == Some(&[':', '/', '/'][..]);
        (head_ok && sep_ok).then_some(end + 3)
    })
}

/// 在一段连续文字里找出所有 URL，返回 `(起, 止)` 字符下标（止不含）。
fn scan(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let Some(body) = boundary.then(|| scheme_at(chars, i)).flatten() else {
            i += 1;
            continue;
        };
        let mut end = body;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        // 去掉句末标点，以及没有配对的右括号（“(见 http://x.com/a)”）。
        while end > body {
            let c = chars[end - 1];
            let trim = match c {
                '.' | ',' | ';' | ':' | '!' | '?' => true,
                ')' | ']' | '}' => {
                    let o = opener(c);
                    let opens = chars[i..end].iter().filter(|&&x| x == o).count();
                    let closes = chars[i..end].iter().filter(|&&x| x == c).count();
                    closes > opens
                }
                _ => false,
            };
            if !trim {
                break;
            }
            end -= 1;
        }
        if end > body {
            out.push((i, end));
            i = end;
        } else {
            i = body;
        }
    }
    out
}

/// 以 `(row, col)` 为悬停位置，在 `lines`（按行号排列的视口文字）里找链接。
/// 软换行的相邻行会合并成一段再识别。
pub fn url_at(lines: &[LineText], row: usize, col: usize) -> Option<LinkHit> {
    if row >= lines.len() {
        return None;
    }
    let mut first = row;
    while first > 0 && lines[first - 1].wrapped {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < lines.len() && lines[last].wrapped {
        last += 1;
    }

    // 合并：每个条目 (字符, 行, 列, 宽度)；占位格不进条目，但并入前一个字符的宽度。
    let mut chars = Vec::new();
    let mut pos: Vec<(u16, u16, u16)> = Vec::new();
    for (r, line) in lines.iter().enumerate().take(last + 1).skip(first) {
        for (c, &ch) in line.chars.iter().enumerate() {
            if ch == '\0' {
                if let Some(p) = pos.last_mut()
                    && usize::from(p.0) == r
                {
                    p.2 += 1;
                }
                continue;
            }
            chars.push(ch);
            pos.push((r as u16, c as u16, 1));
        }
    }

    let hit = pos.iter().position(|&(r, c, w)| {
        usize::from(r) == row && usize::from(c) <= col && col < usize::from(c + w)
    })?;
    let (start, end) = scan(&chars)
        .into_iter()
        .find(|&(s, e)| s <= hit && hit < e)?;

    let mut segments: Vec<(u16, u16, u16)> = Vec::new();
    for &(r, c, w) in &pos[start..end] {
        match segments.last_mut() {
            Some(seg) if seg.0 == r && seg.2 == c => seg.2 = c + w,
            _ => segments.push((r, c, c + w)),
        }
    }
    Some(LinkHit {
        url: chars[start..end].iter().collect(),
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str, wrapped: bool) -> LineText {
        LineText {
            chars: s.chars().collect(),
            wrapped,
        }
    }

    fn url(s: &str, col: usize) -> Option<String> {
        url_at(&[line(s, false)], 0, col).map(|h| h.url)
    }

    #[test]
    fn finds_plain_urls() {
        let s = "see https://example.com/a?b=1#c now";
        assert_eq!(url(s, 4).as_deref(), Some("https://example.com/a?b=1#c"));
        assert_eq!(url(s, 20).as_deref(), Some("https://example.com/a?b=1#c"));
        assert_eq!(url(s, 30).as_deref(), Some("https://example.com/a?b=1#c"));
        // 前后的文字不是链接。
        assert_eq!(url(s, 0), None);
        assert_eq!(url(s, s.len() - 1), None);
    }

    #[test]
    fn trims_trailing_punctuation_and_unbalanced_parens() {
        assert_eq!(
            url("go to http://a.com/x.", 8).as_deref(),
            Some("http://a.com/x")
        );
        assert_eq!(
            url("(see http://a.com/x)", 8).as_deref(),
            Some("http://a.com/x")
        );
        // 配对的括号保留（维基百科风格）。
        assert_eq!(
            url("http://a.com/w/Foo_(bar)", 3).as_deref(),
            Some("http://a.com/w/Foo_(bar)")
        );
        assert_eq!(
            url("\"http://a.com\", next", 3).as_deref(),
            Some("http://a.com")
        );
    }

    #[test]
    fn scheme_is_case_insensitive_and_needs_a_boundary() {
        assert_eq!(url("HTTP://A.COM/x", 2).as_deref(), Some("HTTP://A.COM/x"));
        assert_eq!(url("xhttp://a.com", 3), None);
        // 只有协议、没有地址：不算。
        assert_eq!(url("http://", 2), None);
        // 不认识的协议不自动识别。
        assert_eq!(url("file:///etc/passwd", 3), None);
    }

    #[test]
    fn two_urls_on_one_line() {
        let s = "http://a.com http://b.com";
        assert_eq!(url(s, 2).as_deref(), Some("http://a.com"));
        assert_eq!(url(s, 15).as_deref(), Some("http://b.com"));
        // 两者之间的空格不属于任何一个。
        assert_eq!(url(s, 12), None);
    }

    #[test]
    fn soft_wrapped_url_spans_rows() {
        let lines = [
            line("open http://exam", true),
            line("ple.com/path ok", false),
        ];
        let h = url_at(&lines, 1, 3).unwrap();
        assert_eq!(h.url, "http://example.com/path");
        assert_eq!(h.segments, vec![(0, 5, 16), (1, 0, 12)]);
        assert!(h.contains(0, 5));
        assert!(h.contains(1, 11));
        assert!(!h.contains(1, 12));
        assert!(!h.contains(0, 4));
        // 从第一段悬停同样命中。
        assert_eq!(url_at(&lines, 0, 8).unwrap().url, h.url);
    }

    #[test]
    fn rows_that_are_not_wrapped_do_not_join() {
        let lines = [line("http://exam", false), line("ple.com", false)];
        assert_eq!(url_at(&lines, 0, 2).unwrap().url, "http://exam");
        assert_eq!(url_at(&lines, 1, 2), None);
    }

    #[test]
    fn wide_characters_shift_columns_but_not_urls() {
        // “中”占两列（第二列是占位 \0），URL 从第 3 列开始。
        let mut chars: Vec<char> = vec!['中', '\0', ' '];
        chars.extend("http://a.com".chars());
        let lines = [LineText {
            chars,
            wrapped: false,
        }];
        let h = url_at(&lines, 0, 5).unwrap();
        assert_eq!(h.url, "http://a.com");
        assert_eq!(h.segments, vec![(0, 3, 15)]);
        // 悬停在宽字符的占位格上：不是链接。
        assert_eq!(url_at(&lines, 0, 1), None);
        // 悬停位置越界。
        assert_eq!(url_at(&lines, 0, 99), None);
        assert_eq!(url_at(&lines, 3, 0), None);
    }

    #[test]
    fn only_safe_schemes_are_openable() {
        assert!(is_openable("https://example.com"));
        assert!(is_openable("HTTP://example.com"));
        assert!(is_openable("mailto:a@b.com"));
        assert!(is_openable("ftp://host/file"));
        assert!(!is_openable("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_openable("ms-msdt:/id PCWDiagnostic"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("notaurl"));
        assert!(!is_openable("http:"));
        assert!(!is_openable(""));
    }
}
