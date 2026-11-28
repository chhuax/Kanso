//! design token 与终端配色的读取、校验（`check-tokens`）。
//!
//! 格式见 `docs/design-tokens.md`。校验函数一律“收集全部问题再返回”，
//! 让作者一次看到所有错误，而不是改一个报一个。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(test)]
use serde_json::json;
use serde_json::{Map, Value};

use crate::color::{Rgba, contrast_ratio};

/// 外观：浅色与深色两套配色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    const ALL: [Appearance; 2] = [Appearance::Light, Appearance::Dark];

    fn name(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }
}

/// 一个颜色 token 的两套取值。
#[derive(Clone, Copy, Debug)]
struct ColorToken {
    light: Rgba,
    dark: Rgba,
}

impl ColorToken {
    fn get(&self, a: Appearance) -> Rgba {
        match a {
            Appearance::Light => self.light,
            Appearance::Dark => self.dark,
        }
    }
}

/// 一条对比度要求。
#[derive(Clone, Debug)]
struct ContrastRule {
    fg: String,
    bg: String,
    min: f64,
}

/// 解析后的 `design/tokens.json`。各 map 用 `BTreeMap`，保证遍历顺序稳定。
#[derive(Debug, Default)]
struct Tokens {
    colors: BTreeMap<String, ColorToken>,
    contrast: Vec<ContrastRule>,
}

/// 解析后的终端配色。
#[derive(Debug)]
struct TerminalTheme {
    foreground: Rgba,
    background: Rgba,
    ansi: Vec<Rgba>,
    semantic: Vec<Rgba>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 位于仓库根目录之下")
        .to_path_buf()
}

/// token 名：用 `.` 分层，每层以小写字母开头，其后为字母或数字（camelCase）。
fn valid_token_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|seg| {
            seg.starts_with(|c: char| c.is_ascii_lowercase())
                && seg.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn read_json(path: &Path, errors: &mut Vec<String>) -> Option<Value> {
    let name = path.display();
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            errors.push(format!("{name}：无法读取（{e}）"));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            errors.push(format!("{name}：JSON 解析失败（{e}）"));
            None
        }
    }
}

fn parse_color(
    file: &str,
    what: &str,
    v: Option<&Value>,
    errors: &mut Vec<String>,
) -> Option<Rgba> {
    match v.and_then(Value::as_str) {
        Some(s) => match Rgba::parse(s) {
            Some(c) => Some(c),
            None => {
                errors.push(format!(
                    "{file}：{what} 颜色写法非法：{s:?}（应为 #RRGGBB 或 #RRGGBBAA）"
                ));
                None
            }
        },
        None => {
            errors.push(format!("{file}：{what} 缺失或不是字符串"));
            None
        }
    }
}

fn check_group(file: &str, root: &Map<String, Value>, group: &str, errors: &mut Vec<String>) {
    let Some(obj) = root.get(group).and_then(Value::as_object) else {
        errors.push(format!("{file}：缺少对象 `{group}`"));
        return;
    };
    for (name, v) in obj {
        if !valid_token_name(name) {
            errors.push(format!("{file}：{group}.{name} 名字不合法"));
            continue;
        }
        match v.as_f64() {
            Some(n) if n.is_finite() && n > 0.0 => {}
            _ => errors.push(format!("{file}：{group}.{name} 必须是正数")),
        }
    }
}

fn parse_tokens(file: &str, v: &Value, errors: &mut Vec<String>) -> Tokens {
    let mut t = Tokens::default();
    let Some(root) = v.as_object() else {
        errors.push(format!("{file}：顶层必须是对象"));
        return t;
    };
    if root.get("version").and_then(Value::as_i64) != Some(1) {
        errors.push(format!("{file}：version 必须为 1"));
    }

    match root.get("color").and_then(Value::as_object) {
        None => errors.push(format!("{file}：缺少对象 `color`")),
        Some(colors) => {
            for (name, entry) in colors {
                if !valid_token_name(name) {
                    errors.push(format!("{file}：color.{name} 名字不合法"));
                    continue;
                }
                let light = parse_color(
                    file,
                    &format!("color.{name}.light"),
                    entry.get("light"),
                    errors,
                );
                let dark = parse_color(
                    file,
                    &format!("color.{name}.dark"),
                    entry.get("dark"),
                    errors,
                );
                if entry
                    .get("note")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    errors.push(format!("{file}：color.{name} 缺少 note"));
                }
                if let (Some(light), Some(dark)) = (light, dark) {
                    t.colors.insert(name.clone(), ColorToken { light, dark });
                }
            }
        }
    }

    check_group(file, root, "spacing", errors);
    check_group(file, root, "radius", errors);
    check_group(file, root, "font", errors);

    match root.get("contrast").and_then(Value::as_array) {
        None => errors.push(format!("{file}：缺少数组 `contrast`")),
        Some(list) => {
            for (i, rule) in list.iter().enumerate() {
                let fg = rule.get("fg").and_then(Value::as_str);
                let bg = rule.get("bg").and_then(Value::as_str);
                let min = rule.get("min").and_then(Value::as_f64);
                match (fg, bg, min) {
                    (Some(fg), Some(bg), Some(min)) if min >= 1.0 => {
                        t.contrast.push(ContrastRule {
                            fg: fg.into(),
                            bg: bg.into(),
                            min,
                        })
                    }
                    _ => errors.push(format!(
                        "{file}：contrast[{i}] 需要 fg、bg（字符串）与 min（不小于 1 的数）"
                    )),
                }
            }
        }
    }
    t
}

fn parse_theme(file: &str, v: &Value, errors: &mut Vec<String>) -> Option<TerminalTheme> {
    let Some(root) = v.as_object() else {
        errors.push(format!("{file}：顶层必须是对象"));
        return None;
    };
    let before = errors.len();
    if root.get("version").and_then(Value::as_i64) != Some(1) {
        errors.push(format!("{file}：version 必须为 1"));
    }
    if root
        .get("name")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        errors.push(format!("{file}：缺少 name"));
    }
    // 终端配色的颜色一律 #RRGGBB：终端里没有“叠在什么上面”的概念，半透明没有意义。
    let solid = |key: &str, errors: &mut Vec<String>| {
        let c = parse_color(file, key, root.get(key), errors)?;
        if !c.is_opaque() {
            errors.push(format!("{file}：{key} 不能带 alpha"));
        }
        Some(c)
    };
    let foreground = solid("foreground", errors);
    let background = solid("background", errors);
    let cursor = solid("cursor", errors);
    let cursor_text = solid("cursorText", errors);
    let selection_background = solid("selectionBackground", errors);

    let list = |key: &str, len: usize, errors: &mut Vec<String>| -> Vec<Rgba> {
        let Some(arr) = root.get(key).and_then(Value::as_array) else {
            errors.push(format!("{file}：缺少数组 `{key}`"));
            return vec![];
        };
        if arr.len() != len {
            errors.push(format!(
                "{file}：{key} 必须恰好 {len} 项，实际 {}",
                arr.len()
            ));
        }
        let mut out = vec![];
        for (i, item) in arr.iter().enumerate() {
            if let Some(c) = parse_color(file, &format!("{key}[{i}]"), Some(item), errors) {
                if !c.is_opaque() {
                    errors.push(format!("{file}：{key}[{i}] 不能带 alpha"));
                }
                out.push(c);
            }
        }
        out
    };
    let ansi = list("ansi", 16, errors);
    let semantic = list("semantic", 13, errors);

    if errors.len() > before {
        return None;
    }
    let _ = (cursor?, cursor_text?, selection_background?);
    Some(TerminalTheme {
        foreground: foreground?,
        background: background?,
        ansi,
        semantic,
    })
}

/// 终端配色自身的检查（terminal.md §7）。
fn check_theme(file: &str, t: &TerminalTheme) -> Vec<String> {
    let mut errors = vec![];
    let ratio = contrast_ratio(t.foreground, t.background);
    if ratio < 7.0 {
        errors.push(format!(
            "{file}：foreground 对 background 对比度 {ratio:.2}，要求 >= 7.0"
        ));
    }
    for (i, c) in t.semantic.iter().enumerate() {
        let ratio = contrast_ratio(*c, t.background);
        if ratio < 4.5 {
            errors.push(format!(
                "{file}：semantic[{i}] 对 background 对比度 {ratio:.2}，要求 >= 4.5"
            ));
        }
    }
    for (i, c) in t.ansi.iter().enumerate() {
        if *c == t.background {
            errors.push(format!("{file}：ansi[{i}] 与 background 完全相同"));
        }
    }
    errors
}

/// 校验 token 间的引用与对比度。`themes` 为 (外观, 文件名, 配色)。
fn check_tokens(
    file: &str,
    t: &Tokens,
    themes: &[(Appearance, &str, &TerminalTheme)],
) -> Vec<String> {
    let mut errors = vec![];
    for rule in &t.contrast {
        let (Some(fg), Some(bg)) = (t.colors.get(&rule.fg), t.colors.get(&rule.bg)) else {
            for n in [&rule.fg, &rule.bg] {
                if !t.colors.contains_key(n) {
                    errors.push(format!("{file}：contrast 引用了不存在的 color token `{n}`"));
                }
            }
            continue;
        };
        for a in Appearance::ALL {
            let (f, b) = (fg.get(a), bg.get(a));
            if !b.is_opaque() {
                errors.push(format!(
                    "{file}：contrast 的 bg `{}`（{}）必须是不透明色",
                    rule.bg,
                    a.name()
                ));
                continue;
            }
            let ratio = contrast_ratio(f.over(b), b);
            if ratio < rule.min {
                errors.push(format!(
                    "{file}：`{}` 对 `{}`（{}）对比度 {ratio:.2}，要求 >= {}",
                    rule.fg,
                    rule.bg,
                    a.name(),
                    rule.min
                ));
            }
        }
    }

    if let Some(chip) = t.colors.get("terminal.chipBackground") {
        for (appearance, theme_file, theme) in themes {
            let background = chip.get(*appearance).over(theme.background);
            for foreground in [theme.foreground, theme.ansi[2]] {
                let ratio = contrast_ratio(foreground, background);
                if ratio < 4.5 {
                    errors.push(format!(
                        "{file}：{theme_file} 的标签文字对 `terminal.chipBackground` 对比度 {ratio:.2}，要求 >= 4.5"
                    ));
                }
            }
        }
    }

    // 时间戳栏文字显示在终端背景上，所以要对终端配色的背景而不是 chrome 的 bg token 检查。
    match t.colors.get("terminal.gutterText") {
        None => errors.push(format!("{file}：缺少 color token `terminal.gutterText`")),
        Some(g) => {
            for (a, theme_file, theme) in themes {
                let ratio = contrast_ratio(g.get(*a).over(theme.background), theme.background);
                if ratio < 4.5 {
                    errors.push(format!(
                        "{file}：`terminal.gutterText`（{}）对 {theme_file} 的 background 对比度 {ratio:.2}，要求 >= 4.5",
                        a.name()
                    ));
                }
            }
        }
    }
    errors
}

/// 随程序附带的配色（`design/terminal-themes/bundled/*.json`，由 kanso-core 嵌入）。
/// 它们不生成代码、不参与外观 token 校验，但对比度要求与内置配色相同；目录缺失视为错误，
/// 防止改名或挪位置后这项检查悄悄失效。
fn check_bundled_themes(root: &Path) -> Vec<String> {
    let dir = "design/terminal-themes/bundled";
    let mut errors = vec![];
    let mut paths = match fs::read_dir(root.join(dir)) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect::<Vec<_>>(),
        Err(e) => return vec![format!("{dir} 无法读取（{e}）")],
    };
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let rel = format!("{dir}/{name}");
        if let Some(v) = read_json(&path, &mut errors)
            && let Some(theme) = parse_theme(&rel, &v, &mut errors)
        {
            errors.extend(check_theme(&rel, &theme));
        }
    }
    errors
}

/// `cargo xtask check-tokens`。
pub fn check() -> ExitCode {
    let root = repo_root();
    let mut errors = vec![];
    let tokens_path = root.join("design/tokens.json");
    let tokens = read_json(&tokens_path, &mut errors)
        .map(|v| parse_tokens("design/tokens.json", &v, &mut errors));

    let mut themes = vec![];
    for (a, name) in [
        (Appearance::Light, "kanso-light"),
        (Appearance::Dark, "kanso-dark"),
    ] {
        let rel = format!("design/terminal-themes/{name}.json");
        let Some(v) = read_json(&root.join(&rel), &mut errors) else {
            continue;
        };
        if let Some(theme) = parse_theme(&rel, &v, &mut errors) {
            errors.extend(check_theme(&rel, &theme));
            themes.push((a, rel, theme));
        }
    }
    errors.extend(check_bundled_themes(&root));

    if let Some(t) = &tokens {
        let refs: Vec<_> = themes
            .iter()
            .map(|(a, n, th)| (*a, n.as_str(), th))
            .collect();
        // 终端配色缺失时对应错误已记录，此处只检查能检查的部分。
        errors.extend(check_tokens("design/tokens.json", t, &refs));
    }

    if errors.is_empty() {
        eprintln!("check-tokens：通过");
        ExitCode::SUCCESS
    } else {
        eprintln!("check-tokens：发现 {} 个问题", errors.len());
        for e in &errors {
            eprintln!("  - {e}");
        }
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_colors_meet_contrast_in_both_themes() {
        for source in [
            include_str!("../../design/terminal-themes/kanso-light.json"),
            include_str!("../../design/terminal-themes/kanso-dark.json"),
        ] {
            let value = serde_json::from_str(source).unwrap();
            let mut errors = Vec::new();
            let theme = parse_theme("theme", &value, &mut errors).unwrap();
            assert!(errors.is_empty());
            assert_eq!(theme.semantic.len(), 13);
            for color in theme.semantic {
                assert!(contrast_ratio(color, theme.background) >= 4.5);
            }
        }
    }

    #[test]
    fn names() {
        assert!(valid_token_name("bg.tabSelected"));
        assert!(valid_token_name("xxs"));
        assert!(!valid_token_name("Bg.x"));
        assert!(!valid_token_name("bg..x"));
        assert!(!valid_token_name("bg-x"));
        assert!(!valid_token_name(""));
    }

    #[test]
    fn contrast_rule_failure_and_translucent_bg() {
        let v = json!({
            "version": 1,
            "color": {
                "a": { "light": "#777777", "dark": "#777777", "note": "x" },
                "b": { "light": "#FFFFFF", "dark": "#FFFFFF80", "note": "x" }
            },
            "spacing": {}, "radius": {}, "font": {},
            "contrast": [ { "fg": "a", "bg": "b", "min": 4.5 } ]
        });
        let mut errs = vec![];
        let t = parse_tokens("t", &v, &mut errs);
        // 空分组合法，不应有解析错误。
        assert!(errs.is_empty(), "{errs:?}");
        let errs = check_tokens("t", &t, &[]);
        assert!(
            errs.iter()
                .any(|e| e.contains("4.48") && e.contains("light")),
            "{errs:?}"
        );
        assert!(errs.iter().any(|e| e.contains("不透明")), "{errs:?}");
    }

    #[test]
    fn missing_dark_is_reported() {
        let v = json!({
            "version": 1,
            "color": { "a": { "light": "#000000", "note": "x" } },
            "spacing": { "sm": -1 }, "radius": {}, "font": {}, "contrast": []
        });
        let mut errs = vec![];
        parse_tokens("t", &v, &mut errs);
        assert!(errs.iter().any(|e| e.contains("color.a.dark")));
        assert!(errs.iter().any(|e| e.contains("spacing.sm")));
    }
}
