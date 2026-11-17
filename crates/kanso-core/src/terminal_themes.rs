//! 用户终端主题目录。两个前端都使用同一份版本、颜色与文件标识规则。

use std::{fs, path::Path};

use serde::Deserialize;

/// 渲染所需的 sRGB 颜色以 0xRRGGBB 表示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTheme {
    /// 持久化选择使用的稳定标识。
    pub id: String,
    /// 主题文件给出的显示名称。
    pub name: String,
    /// 默认文字色。
    pub foreground: u32,
    /// 默认底色。
    pub background: u32,
    /// 光标色。
    pub cursor: u32,
    /// 块状光标上的文字色。
    pub cursor_text: u32,
    /// 选区底色。
    pub selection_background: u32,
    /// 标准与高亮 ANSI 色，依次共 16 项。
    pub ansi: [u32; 16],
    /// 按帧内类别编号排列的 13 个语义色。
    pub semantic: [u32; 13],
}

#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Hex(u32);

impl TryFrom<String> for Hex {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let digits = s
            .strip_prefix('#')
            .filter(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("颜色必须为 #RRGGBB")?;
        Ok(Self(
            u32::from_str_radix(digits, 16).expect("已校验六位十六进制数"),
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThemeFile {
    version: u32,
    name: String,
    foreground: Hex,
    background: Hex,
    cursor: Hex,
    cursor_text: Hex,
    selection_background: Hex,
    ansi: [Hex; 16],
    semantic: [Hex; 13],
}

impl TerminalTheme {
    /// 解析与内置配色同格式的 v1 文件，也接受 Windows 编辑器写入的 UTF-8 BOM。
    pub fn parse(id: String, json: &str) -> Result<Self, String> {
        let t: ThemeFile =
            serde_json::from_str(json.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
        if t.version != 1 || t.name.trim().is_empty() {
            return Err("主题版本必须为 1，名称不能为空".into());
        }
        Ok(Self {
            id,
            name: t.name.trim().to_owned(),
            foreground: t.foreground.0,
            background: t.background.0,
            cursor: t.cursor.0,
            cursor_text: t.cursor_text.0,
            selection_background: t.selection_background.0,
            ansi: t.ansi.map(|v| v.0),
            semantic: t.semantic.map(|v| v.0),
        })
    }
}

/// 扫描目录的第一层。内置主题使用独立标识；用户文件用文件名识别，修改显示名不会丢失选择。
pub fn load_terminal_themes(directory: &Path) -> Vec<TerminalTheme> {
    let files = match fs::create_dir_all(directory).and_then(|()| fs::read_dir(directory)) {
        Ok(files) => files,
        Err(error) => {
            tracing::warn!(?directory, %error, "无法读取终端配色目录");
            return Vec::new();
        }
    };
    let mut paths = files
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("json"))
                && p.is_file()
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut themes = Vec::new();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let id = format!("file:{stem}");
        let parsed = fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|json| TerminalTheme::parse(id, &json));
        match parsed {
            Ok(theme) => {
                if !themes
                    .iter()
                    .any(|existing: &TerminalTheme| existing.id == theme.id)
                {
                    themes.push(theme);
                }
            }
            Err(error) => tracing::warn!(?path, %error, "忽略无效终端配色文件"),
        }
    }
    themes.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    themes
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = include_str!("../../../design/terminal-themes/kanso-dark.json");

    #[test]
    fn discover_update_remove_and_restore_by_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("themes");
        assert!(load_terminal_themes(&dir).is_empty());
        let path = dir.join("青色.JSON");
        fs::write(&path, format!("\u{feff}{SAMPLE}")).unwrap();
        fs::write(dir.join("bad.json"), "{").unwrap();
        let theme = load_terminal_themes(&dir).pop().unwrap();
        assert_eq!(theme.id, "file:青色");
        assert_eq!(theme.ansi[1], 0xe5645a);
        fs::write(&path, SAMPLE.replace("Kanso Dark", "青色")).unwrap();
        let changed = load_terminal_themes(&dir);
        assert_eq!(
            (changed[0].id.as_str(), changed[0].name.as_str()),
            ("file:青色", "青色")
        );
        fs::remove_file(&path).unwrap();
        assert!(load_terminal_themes(&dir).is_empty());
        fs::write(&path, SAMPLE).unwrap();
        assert_eq!(load_terminal_themes(&dir), [theme]);
    }

    #[test]
    fn rejects_incomplete_or_unknown_theme_files() {
        for (key, value) in [
            ("version", serde_json::json!(2)),
            ("name", serde_json::json!("  ")),
            ("cursor", serde_json::json!("#+12345")),
            ("foreground", serde_json::json!("112233")),
            ("semantic", serde_json::json!([])),
            ("ansi", serde_json::json!(["#FFFFFF"])),
        ] {
            let mut json: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
            json[key] = value;
            assert!(
                TerminalTheme::parse(String::new(), &json.to_string()).is_err(),
                "{key}"
            );
        }
    }
}
