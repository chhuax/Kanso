//! design token 与终端配色的读取、校验（`check-tokens`）和 Apple 端代码生成（`gen-tokens`）。
//!
//! 格式见 `docs/design-tokens.md`。校验函数一律“收集全部问题再返回”，
//! 让作者一次看到所有错误，而不是改一个报一个。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Map, Value, json};

use crate::color::{Rgba, contrast_ratio};

/// 外观：与 Asset Catalog 的 Any / Dark 外观对应。
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

/// 解析后的 `design/tokens.json`。各 map 用 `BTreeMap`，保证遍历与生成顺序稳定。
#[derive(Debug, Default)]
struct Tokens {
    colors: BTreeMap<String, ColorToken>,
    spacing: BTreeMap<String, f64>,
    radius: BTreeMap<String, f64>,
    font: BTreeMap<String, f64>,
    contrast: Vec<ContrastRule>,
}

/// 解析后的终端配色。
#[derive(Debug)]
struct TerminalTheme {
    foreground: Rgba,
    background: Rgba,
    cursor: Rgba,
    cursor_text: Rgba,
    selection_background: Rgba,
    ansi: Vec<Rgba>,
    semantic: Vec<Rgba>,
}

/// 终端配色 Swift 文件在仓库内的相对路径（入库，由 `gen-tokens` 生成，`check-tokens` 校验）。
const THEMES_SWIFT: &str = "apple/Packages/TerminalKit/Sources/TerminalKit/TerminalThemes.swift";

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

fn parse_group(
    file: &str,
    root: &Map<String, Value>,
    group: &str,
    errors: &mut Vec<String>,
) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Some(obj) = root.get(group).and_then(Value::as_object) else {
        errors.push(format!("{file}：缺少对象 `{group}`"));
        return out;
    };
    for (name, v) in obj {
        if !valid_token_name(name) {
            errors.push(format!("{file}：{group}.{name} 名字不合法"));
            continue;
        }
        match v.as_f64() {
            Some(n) if n.is_finite() && n > 0.0 => {
                out.insert(name.clone(), n);
            }
            _ => errors.push(format!("{file}：{group}.{name} 必须是正数")),
        }
    }
    out
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

    t.spacing = parse_group(file, root, "spacing", errors);
    t.radius = parse_group(file, root, "radius", errors);
    t.font = parse_group(file, root, "font", errors);

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
    Some(TerminalTheme {
        foreground: foreground?,
        background: background?,
        cursor: cursor?,
        cursor_text: cursor_text?,
        selection_background: selection_background?,
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

    if let Some(t) = &tokens {
        let refs: Vec<_> = themes
            .iter()
            .map(|(a, n, th)| (*a, n.as_str(), th))
            .collect();
        // 终端配色缺失时对应错误已记录，此处只检查能检查的部分。
        errors.extend(check_tokens("design/tokens.json", t, &refs));

        // 入库的 TerminalThemes.swift 必须与 JSON 一致：包内生成物入库是为了让单独的
        // `swift build` 不依赖 xtask，代价就是要在这里兜底，防止改了 JSON 忘记重新生成。
        if errors.is_empty() {
            let light = themes.iter().find(|(a, _, _)| *a == Appearance::Light);
            let dark = themes.iter().find(|(a, _, _)| *a == Appearance::Dark);
            if let (Some((_, _, l)), Some((_, _, d))) = (light, dark) {
                let expected = render_terminal_themes(t, l, d);
                let path = root.join(THEMES_SWIFT);
                match fs::read_to_string(&path) {
                    Ok(actual) if actual == expected => {}
                    Ok(_) => errors.push(format!(
                        "{THEMES_SWIFT} 与 design/ 下的配色不一致，请运行 `cargo xtask gen-tokens` 并提交"
                    )),
                    Err(e) => errors.push(format!(
                        "{THEMES_SWIFT} 无法读取（{e}），请运行 `cargo xtask gen-tokens`"
                    )),
                }
            }
        }
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

/// token 名转 Swift 标识符：按 `.` 拆开，首段保持原样，其余各段首字母大写后拼接。
/// `bg.tabSelected` 得到 `bgTabSelected`。
fn swift_ident(name: &str) -> String {
    let mut out = String::new();
    for (i, seg) in name.split('.').enumerate() {
        if i == 0 {
            out.push_str(seg);
        } else {
            let mut cs = seg.chars();
            if let Some(f) = cs.next() {
                out.push(f.to_ascii_uppercase());
                out.extend(cs);
            }
        }
    }
    out
}

/// 颜色集名字：token 名把 `.` 换成 `_`。
fn colorset_name(name: &str) -> String {
    name.replace('.', "_")
}

fn color_json(c: Rgba) -> Value {
    let f = |v: u8| format!("{:.4}", f64::from(v) / 255.0);
    json!({
        "color-space": "srgb",
        "components": {
            "alpha": f(c.a),
            "blue": f(c.b),
            "green": f(c.g),
            "red": f(c.r),
        }
    })
}

fn colorset_contents(t: &ColorToken) -> Value {
    json!({
        "colors": [
            { "color": color_json(t.light), "idiom": "universal" },
            {
                "appearances": [{ "appearance": "luminosity", "value": "dark" }],
                "color": color_json(t.dark),
                "idiom": "universal"
            }
        ],
        "info": { "author": "xcode", "version": 1 }
    })
}

fn swift_enum(out: &mut String, name: &str, map: &BTreeMap<String, f64>, errors: &mut Vec<String>) {
    out.push_str(&format!("    enum {name} {{\n"));
    let mut seen = BTreeMap::new();
    for (k, v) in map {
        let id = swift_ident(k);
        if let Some(prev) = seen.insert(id.clone(), k) {
            errors.push(format!(
                "{name}.{k} 与 {prev} 生成了相同的 Swift 标识符 {id}"
            ));
        }
        out.push_str(&format!("        static let {id}: CGFloat = {v}\n"));
    }
    out.push_str("    }\n");
}

fn render_swift(t: &Tokens, errors: &mut Vec<String>) -> String {
    let mut s = String::from(
        "// 由 cargo xtask gen-tokens 生成，勿手改。\n// 来源：design/tokens.json\n\nimport CoreGraphics\n\nenum DesignTokens {\n",
    );
    swift_enum(&mut s, "Spacing", &t.spacing, errors);
    s.push('\n');
    swift_enum(&mut s, "Radius", &t.radius, errors);
    s.push('\n');
    swift_enum(&mut s, "Font", &t.font, errors);
    s.push_str(
        "\n    /// 颜色集名字，用于 `Color(name)` / `NSColor(named:)`。\n    enum Color {\n",
    );
    let mut seen = BTreeMap::new();
    for k in t.colors.keys() {
        let id = swift_ident(k);
        if let Some(prev) = seen.insert(id.clone(), k) {
            errors.push(format!(
                "Color.{k} 与 {prev} 生成了相同的 Swift 标识符 {id}"
            ));
        }
        s.push_str(&format!(
            "        static let {id} = \"{}\"\n",
            colorset_name(k)
        ));
    }
    s.push_str("    }\n}\n");
    s
}

fn swift_rgb(c: Rgba) -> String {
    format!("RGB8(hex: 0x{:02X}{:02X}{:02X})", c.r, c.g, c.b)
}

fn swift_rgba(c: Rgba) -> String {
    format!("RGBA8(r: {}, g: {}, b: {}, a: {})", c.r, c.g, c.b, c.a)
}

/// 生成 `TerminalThemes.swift`：浅色对应 `kanso-light.json`，深色对应 `kanso-dark.json`。
///
/// 界面 token（`terminal.gutterText` 等）也一并烘焙进来，而不是运行时从 Asset Catalog 取：
/// TerminalKit 是独立的 SwiftPM 包，拿不到 App 的 Asset Catalog，而且离屏渲染要能按指定外观取色，
/// 不依赖 `NSAppearance` 的动态颜色解析。调用方须保证三个 token 存在（`parse_tokens` 之后由
/// `check_tokens` 校验 `terminal.gutterText`，其余两个缺失时这里直接 panic 以免悄悄生成错误文件）。
fn render_terminal_themes(t: &Tokens, light: &TerminalTheme, dark: &TerminalTheme) -> String {
    let token = |name: &str, a: Appearance| -> Rgba {
        t.colors
            .get(name)
            .unwrap_or_else(|| panic!("缺少 color token `{name}`"))
            .get(a)
    };
    let mut s = String::from(
        "// 由 cargo xtask gen-tokens 生成，勿手改。\n\
         // 来源：design/terminal-themes/*.json 与 design/tokens.json 的 terminal.* token。\n\
         // 入库是为了让单独的 `swift build` 不依赖 xtask；cargo xtask check-tokens 会校验它与来源一致。\n\n\
         extension TerminalPalette {\n",
    );
    for (ident, label, a, th) in [
        ("kansoLight", "kanso-light.json", Appearance::Light, light),
        ("kansoDark", "kanso-dark.json", Appearance::Dark, dark),
    ] {
        s.push_str(&format!(
            "    /// 数值来自 design/terminal-themes/{label}。\n"
        ));
        s.push_str(&format!(
            "    public static let {ident} = TerminalPalette(\n"
        ));
        s.push_str(&format!(
            "        foreground: {},\n",
            swift_rgb(th.foreground)
        ));
        s.push_str(&format!(
            "        background: {},\n",
            swift_rgb(th.background)
        ));
        s.push_str(&format!("        cursor: {},\n", swift_rgb(th.cursor)));
        s.push_str(&format!(
            "        cursorText: {},\n",
            swift_rgb(th.cursor_text)
        ));
        s.push_str(&format!(
            "        selectionBackground: {},\n",
            swift_rgb(th.selection_background)
        ));
        for (name, list) in [("ansi", &th.ansi), ("semantic", &th.semantic)] {
            s.push_str(&format!("        {name}: [\n"));
            for row in list.chunks(8) {
                let items: Vec<String> = row.iter().map(|c| swift_rgb(*c)).collect();
                s.push_str(&format!("            {},\n", items.join(", ")));
            }
            s.push_str("        ],\n");
        }
        s.push_str(&format!(
            "        gutterText: {},\n",
            swift_rgba(token("terminal.gutterText", a))
        ));
        s.push_str(&format!(
            "        gutterCursorLine: {},\n",
            swift_rgba(token("terminal.gutterCursorLine", a))
        ));
        s.push_str(&format!(
            "        commandSeparator: {},\n",
            swift_rgba(token("terminal.commandSeparator", a))
        ));
        s.push_str(&format!(
            "        chipBackground: {},\n        chipBorder: {}\n",
            swift_rgba(token("terminal.chipBackground", a)),
            swift_rgba(token("terminal.chipBorder", a))
        ));
        s.push_str("    )\n");
        if ident == "kansoLight" {
            s.push('\n');
        }
    }
    s.push_str("}\n");
    s
}

fn write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, content)
}

/// `cargo xtask gen-tokens [--out <dir>]`。
pub fn generate(args: &[String]) -> ExitCode {
    let root = repo_root();
    let mut out = root.join("apple/Kanso/Generated");
    let mut themes_out = root.join(THEMES_SWIFT);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => match it.next() {
                Some(p) => out = PathBuf::from(p),
                None => {
                    eprintln!("--out 需要一个目录参数");
                    return ExitCode::FAILURE;
                }
            },
            "--themes-out" => match it.next() {
                Some(p) => themes_out = PathBuf::from(p),
                None => {
                    eprintln!("--themes-out 需要一个文件路径参数");
                    return ExitCode::FAILURE;
                }
            },
            other => {
                eprintln!("未知参数：{other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let mut errors = vec![];
    let tokens = read_json(&root.join("design/tokens.json"), &mut errors)
        .map(|v| parse_tokens("design/tokens.json", &v, &mut errors));
    let Some(tokens) = tokens.filter(|_| errors.is_empty()) else {
        for e in &errors {
            eprintln!("  - {e}");
        }
        eprintln!("gen-tokens：tokens.json 无效，请先运行 cargo xtask check-tokens");
        return ExitCode::FAILURE;
    };

    let swift = render_swift(&tokens, &mut errors);
    let mut theme_files = vec![];
    for name in ["kanso-light", "kanso-dark"] {
        let rel = format!("design/terminal-themes/{name}.json");
        let theme = read_json(&root.join(&rel), &mut errors)
            .and_then(|v| parse_theme(&rel, &v, &mut errors));
        theme_files.push(theme);
    }
    for key in [
        "terminal.gutterText",
        "terminal.gutterCursorLine",
        "terminal.commandSeparator",
        "terminal.chipBackground",
        "terminal.chipBorder",
    ] {
        if !tokens.colors.contains_key(key) {
            errors.push(format!("design/tokens.json 缺少 color token `{key}`"));
        }
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("  - {e}");
        }
        return ExitCode::FAILURE;
    }

    let result = (|| -> std::io::Result<()> {
        // 先清掉旧的 Colors.xcassets，避免已删除的 token 留下过期颜色集。
        let assets = out.join("Colors.xcassets");
        if assets.exists() {
            fs::remove_dir_all(&assets)?;
        }
        let pretty =
            |v: &Value| serde_json::to_string_pretty(v).expect("序列化 Value 不会失败") + "\n";
        write(
            &assets.join("Contents.json"),
            &pretty(&json!({ "info": { "author": "xcode", "version": 1 } })),
        )?;
        for (name, tok) in &tokens.colors {
            let p = assets
                .join(format!("{}.colorset", colorset_name(name)))
                .join("Contents.json");
            write(&p, &pretty(&colorset_contents(tok)))?;
        }
        write(&out.join("DesignTokens.swift"), &swift)?;
        // 上面已确认两份配色都解析成功。
        if let [Some(light), Some(dark)] = theme_files.as_slice() {
            write(&themes_out, &render_terminal_themes(&tokens, light, dark))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            eprintln!("gen-tokens：已生成到 {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gen-tokens：写入失败（{e}）");
            ExitCode::FAILURE
        }
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
    fn swift_names() {
        assert_eq!(swift_ident("bg.tabSelected"), "bgTabSelected");
        assert_eq!(swift_ident("tabRow.height"), "tabRowHeight");
        assert_eq!(colorset_name("bg.tabSelected"), "bg_tabSelected");
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
