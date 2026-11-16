//! 文本层小工具：行切分、通配匹配、`~` 与 `%` 展开。

use std::path::{Path, PathBuf};

/// 一行配置切分出的关键字（原样大小写）与词列表。
pub(crate) struct Line {
    pub keyword: String,
    pub words: Vec<String>,
}

/// 把一行切成关键字和值词；空行与整行注释返回 `None`。
///
/// 行为依据 `ssh -G` 黑盒实验：
/// - 关键字以空白或 `=` 结束，`=` 两侧可以有空白；
/// - 词由空白分隔，双引号可以把空白括进词里，引号段与非引号段可以拼接（`a"b c"d` 是一个词）；
/// - 以未加引号的 `#` **开头**的词开始行内注释（直到行尾），词中间的 `#`（`foo#bar`）属于值。
///   这比“只有整行注释才算注释”更接近 ssh 的真实行为：`User bob # 备注` 里的备注会被 ssh 丢掉。
pub(crate) fn split_line(line: &str) -> Option<Line> {
    let line = line.trim_start_matches('\u{feff}').trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let kw_end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line[..kw_end].to_string();
    let mut rest = line[kw_end..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    Some(Line {
        keyword,
        words: split_words(rest),
    })
}

fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.peek() {
            None | Some('#') => break,
            _ => {}
        }
        let mut word = String::new();
        let mut in_quote = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                in_quote = !in_quote;
            } else if c.is_whitespace() && !in_quote {
                break;
            } else {
                word.push(c);
            }
            chars.next();
        }
        words.push(word);
    }
    words
}

/// 通配匹配：`*` 任意长度（含空），`?` 恰好一个字符。
///
/// 用迭代回溯而不是递归，避免 `a*a*a*…` 之类的模式造成指数级耗时。
pub(crate) fn wildcard_match(pattern: &str, text: &str, ignore_case: bool) -> bool {
    let fold = |c: char| {
        if ignore_case {
            c.to_lowercase().next().unwrap_or(c)
        } else {
            c
        }
    };
    let p: Vec<char> = pattern.chars().map(fold).collect();
    let t: Vec<char> = text.chars().map(fold).collect();
    let (mut pi, mut ti) = (0, 0);
    // 最近一个 `*` 的位置，以及它当前吞到的文本位置。
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 字符串里是否含通配符。
pub(crate) fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// 展开开头的 `~`（仅 `~` 与 `~/…`；`~user` 形式不支持，原样返回）。
pub(crate) fn expand_tilde(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        home.to_path_buf()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}

/// `%` 展开：`lookup` 对支持的 token 返回替换值。
///
/// 返回展开结果和未识别的 token 字符。未识别（包含结尾孤立的 `%`）的 token 原样保留，
/// 由调用方转成警告；ssh 对这种情况是直接报错退出，我们不能因为一个坏值丢掉整个主机。
pub(crate) fn expand_percent(
    s: &str,
    lookup: &dyn Fn(char) -> Option<String>,
) -> (String, Vec<char>) {
    let mut out = String::new();
    let mut bad = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(t) => match lookup(t) {
                Some(v) => out.push_str(&v),
                None => {
                    out.push('%');
                    out.push(t);
                    bad.push(t);
                }
            },
            None => {
                out.push('%');
                bad.push('%');
            }
        }
    }
    (out, bad)
}
