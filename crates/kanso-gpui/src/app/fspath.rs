//! 文件浏览用的路径纯函数（docs/spec/filer.md §3、§5.3）：拼接、上一级、直接子项名称校验、
//! 路径栏输入的解析（相对路径以当前浏览目录为基准）、面包屑。
//!
//! 两种风格：
//! - [`PathStyle::Posix`]：远端 SFTP 与 Unix 本地路径，分隔符 `/`；Windows OpenSSH 的 `/C:/dir` 形式也按它处理；
//! - [`PathStyle::Windows`]：本机 Windows 路径，分隔符 `\`；空字符串表示虚拟根“此电脑”（盘符列表）。
//!
//! 本文件不碰文件系统，三个平台都能编译与测试。

use crate::filer::parent_path;

/// 路径风格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    /// `/` 分隔（远端、Unix 本地）。
    Posix,
    /// `\` 分隔、盘符开头（本机 Windows）。
    Windows,
}

impl PathStyle {
    /// 本机路径的风格。
    pub fn native() -> PathStyle {
        if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Posix
        }
    }

    /// 分隔符。
    pub fn sep(self) -> char {
        match self {
            PathStyle::Posix => '/',
            PathStyle::Windows => '\\',
        }
    }
}

/// Windows 本地的虚拟根（“此电脑”，盘符列表）。
pub const THIS_PC: &str = "";

/// “此电脑”这类虚拟位置：不是真实目录，不能新建、删除、接收复制。
pub fn is_virtual_root(style: PathStyle, path: &str) -> bool {
    style == PathStyle::Windows && path.is_empty()
}

fn is_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// 把 Windows 路径里的 `/` 统一成 `\`。
fn win_norm(path: &str) -> String {
    path.replace('/', "\\")
}

/// 路径是不是文件系统根（`/`、`C:\`、`/C:/`）。
pub fn is_root(style: PathStyle, path: &str) -> bool {
    match style {
        PathStyle::Posix => path == "/",
        PathStyle::Windows => {
            let p = win_norm(path);
            is_drive_prefix(&p) && p.len() <= 3 && p[2..].trim_matches('\\').is_empty()
        }
    }
}

/// 上一级目录。Posix 根停在 `/`；Windows 盘符根的上一级是“此电脑”（空串），再向上仍是它。
pub fn parent(style: PathStyle, path: &str) -> String {
    match style {
        PathStyle::Posix => parent_path(path),
        PathStyle::Windows => {
            if path.is_empty() || is_root(style, path) {
                return THIS_PC.to_owned();
            }
            let p = win_norm(path);
            let trimmed = p.trim_end_matches('\\');
            match trimmed.rfind('\\') {
                None => THIS_PC.to_owned(),
                Some(i) => {
                    let head = &trimmed[..i];
                    if is_drive_prefix(head) && head.len() == 2 {
                        format!("{head}\\")
                    } else if head.is_empty() {
                        THIS_PC.to_owned()
                    } else {
                        head.to_owned()
                    }
                }
            }
        }
    }
}

/// 在目录 `base` 下拼出名称为 `name` 的子项路径。
pub fn join(style: PathStyle, base: &str, name: &str) -> String {
    let sep = style.sep();
    if base.is_empty() {
        return match style {
            PathStyle::Posix => format!("/{name}"),
            PathStyle::Windows => name.to_owned(),
        };
    }
    if base.ends_with(sep) {
        format!("{base}{name}")
    } else {
        format!("{base}{sep}{name}")
    }
}

/// 路径的最后一段名称（根返回路径本身）。
pub fn file_name(style: PathStyle, path: &str) -> String {
    let p = match style {
        PathStyle::Posix => path.to_owned(),
        PathStyle::Windows => win_norm(path),
    };
    let sep = style.sep();
    let trimmed = p.trim_end_matches(sep);
    if trimmed.is_empty() {
        return p;
    }
    trimmed
        .rsplit(sep)
        .next()
        .map_or_else(|| trimmed.to_owned(), str::to_owned)
}

/// 名称校验失败的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// 空名称。
    Empty,
    /// `.` 或 `..`。
    Reserved,
    /// 含目录分隔符（不是当前目录的直接子项）。
    HasSeparator,
    /// 含系统不允许的字符。
    InvalidChar(char),
}

/// 校验并整理新建 / 重命名的名称：去掉首尾空白，必须是当前目录的一个直接子项。
pub fn validate_child_name(style: PathStyle, raw: &str) -> Result<String, NameError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(NameError::Reserved);
    }
    if name.contains('/') || (style == PathStyle::Windows && name.contains('\\')) {
        return Err(NameError::HasSeparator);
    }
    if let Some(c) = name.chars().find(|c| *c == '\0' || c.is_control()) {
        return Err(NameError::InvalidChar(c));
    }
    if style == PathStyle::Windows {
        if let Some(c) = name.chars().find(|c| "<>:\"|?*".contains(*c)) {
            return Err(NameError::InvalidChar(c));
        }
        if name.ends_with('.') {
            return Err(NameError::InvalidChar('.'));
        }
    }
    Ok(name.to_owned())
}

/// 归一化：去掉 `.`、折叠 `..` 与重复分隔符。`..` 不会越过根。
fn normalize_posix(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

fn normalize_windows(path: &str) -> String {
    let p = win_norm(path);
    let (drive, rest) = if is_drive_prefix(&p) {
        (p[..2].to_ascii_uppercase(), &p[2..])
    } else {
        (String::new(), &p[..])
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in rest.split('\\') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    if drive.is_empty() {
        return parts.join("\\");
    }
    if parts.is_empty() {
        format!("{drive}\\")
    } else {
        format!("{drive}\\{}", parts.join("\\"))
    }
}

/// 解析路径栏里输入的文字（filer.md §3）：
/// - 空输入回到当前目录；
/// - 绝对路径保持原意（只做归一化）；
/// - 本地支持 `~` 家目录缩写（`home` 非空时）；
/// - 相对路径以当前浏览目录为基准，而不是应用启动位置。
///
/// Windows 的虚拟根（当前目录为空串）下，相对输入只能是盘符，否则原样返回由读取目录报错。
pub fn resolve_input(style: PathStyle, current: &str, input: &str, home: Option<&str>) -> String {
    let input = input.trim();
    if input.is_empty() {
        return current.to_owned();
    }
    let input = match (home, input) {
        (Some(h), "~") => h.to_owned(),
        (Some(h), s) if s.starts_with("~/") || s.starts_with("~\\") => {
            format!(
                "{}{}{}",
                h.trim_end_matches(['/', '\\']),
                style.sep(),
                &s[2..]
            )
        }
        _ => input.to_owned(),
    };
    match style {
        PathStyle::Posix => {
            if input.starts_with('/') {
                normalize_posix(&input)
            } else {
                normalize_posix(&format!("{}/{input}", current.trim_end_matches('/')))
            }
        }
        PathStyle::Windows => {
            let p = win_norm(&input);
            if is_drive_prefix(&p) || p.starts_with("\\\\") {
                return normalize_windows(&p);
            }
            if current.is_empty() {
                return normalize_windows(&p);
            }
            if p.starts_with('\\') {
                // 当前盘符的根。
                let cur = win_norm(current);
                let drive = &cur[..2.min(cur.len())];
                return normalize_windows(&format!("{drive}{p}"));
            }
            normalize_windows(&format!("{}\\{p}", current.trim_end_matches('\\')))
        }
    }
}

/// 路径栏的一段面包屑。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    /// 显示文字。
    pub label: String,
    /// 点击后进入的路径。
    pub path: String,
}

/// 把路径拆成可点击的面包屑（filer.md D2）。
pub fn breadcrumbs(style: PathStyle, path: &str) -> Vec<Crumb> {
    let mut out = Vec::new();
    match style {
        PathStyle::Posix => {
            out.push(Crumb {
                label: "/".to_owned(),
                path: "/".to_owned(),
            });
            let mut acc = String::new();
            for seg in path.split('/').filter(|s| !s.is_empty()) {
                acc.push('/');
                acc.push_str(seg);
                // OpenSSH on Windows 的盘符段要带尾部斜杠才是目录根。
                let target = if is_drive_prefix(seg) && seg.len() == 2 && out.len() == 1 {
                    format!("{acc}/")
                } else {
                    acc.clone()
                };
                out.push(Crumb {
                    label: seg.to_owned(),
                    path: target,
                });
            }
        }
        PathStyle::Windows => {
            out.push(Crumb {
                label: crate::app::filer_text::t("ui-filer-this-pc"),
                path: THIS_PC.to_owned(),
            });
            let p = win_norm(path);
            let mut acc = String::new();
            for (i, seg) in p.split('\\').filter(|s| !s.is_empty()).enumerate() {
                if i == 0 {
                    acc = seg.to_owned();
                    let target = if is_drive_prefix(seg) {
                        format!("{acc}\\")
                    } else {
                        acc.clone()
                    };
                    out.push(Crumb {
                        label: seg.to_owned(),
                        path: target,
                    });
                } else {
                    acc = format!("{}\\{seg}", acc.trim_end_matches('\\'));
                    out.push(Crumb {
                        label: seg.to_owned(),
                        path: acc.clone(),
                    });
                }
            }
        }
    }
    out
}

/// 隐藏项（按名称以点开头判断；规格 D2：隐藏文件开关默认显示）。
pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;
    use PathStyle::{Posix, Windows};

    #[test]
    fn parent_stops_at_roots() {
        assert_eq!(parent(Posix, "/a/b"), "/a");
        assert_eq!(parent(Posix, "/a"), "/");
        assert_eq!(parent(Posix, "/"), "/");
        assert_eq!(parent(Windows, r"C:\a\b"), r"C:\a");
        assert_eq!(parent(Windows, r"C:\a"), r"C:\");
        // 盘符根的上一级是“此电脑”，再向上仍停在这里。
        assert_eq!(parent(Windows, r"C:\"), THIS_PC);
        assert_eq!(parent(Windows, THIS_PC), THIS_PC);
        assert_eq!(parent(Windows, "C:/a"), r"C:\");
    }

    #[test]
    fn join_and_file_name() {
        assert_eq!(join(Posix, "/a", "b"), "/a/b");
        assert_eq!(join(Posix, "/", "b"), "/b");
        assert_eq!(join(Windows, r"C:\", "b"), r"C:\b");
        assert_eq!(join(Windows, r"C:\a", "b"), r"C:\a\b");
        assert_eq!(file_name(Posix, "/a/b.txt"), "b.txt");
        assert_eq!(file_name(Windows, r"C:\a\b.txt"), "b.txt");
        assert_eq!(file_name(Posix, "/"), "/");
    }

    #[test]
    fn child_names_must_be_direct_children() {
        assert_eq!(validate_child_name(Posix, "  新建 "), Ok("新建".into()));
        assert_eq!(validate_child_name(Posix, "  "), Err(NameError::Empty));
        assert_eq!(validate_child_name(Posix, ".."), Err(NameError::Reserved));
        assert_eq!(validate_child_name(Posix, "."), Err(NameError::Reserved));
        assert_eq!(
            validate_child_name(Posix, "a/b"),
            Err(NameError::HasSeparator)
        );
        // 远端 Posix 文件名里的反斜杠是合法字符。
        assert!(validate_child_name(Posix, r"a\b").is_ok());
        assert_eq!(
            validate_child_name(Windows, r"a\b"),
            Err(NameError::HasSeparator)
        );
        assert_eq!(
            validate_child_name(Windows, "a:b"),
            Err(NameError::InvalidChar(':'))
        );
        assert_eq!(
            validate_child_name(Windows, "a?"),
            Err(NameError::InvalidChar('?'))
        );
        assert!(validate_child_name(Posix, "a\u{0}b").is_err());
    }

    #[test]
    fn relative_input_is_based_on_the_current_directory() {
        assert_eq!(
            resolve_input(Posix, "/srv/app", "logs", None),
            "/srv/app/logs"
        );
        assert_eq!(resolve_input(Posix, "/srv/app", "../etc", None), "/srv/etc");
        assert_eq!(
            resolve_input(Posix, "/srv/app", "/var//log/", None),
            "/var/log"
        );
        assert_eq!(resolve_input(Posix, "/srv/app", "", None), "/srv/app");
        // `..` 越不过根。
        assert_eq!(resolve_input(Posix, "/", "../..", None), "/");
        assert_eq!(
            resolve_input(Posix, "/x", "~/docs", Some("/home/me")),
            "/home/me/docs"
        );
        assert_eq!(
            resolve_input(Posix, "/x", "~", Some("/home/me")),
            "/home/me"
        );
        // 没有家目录信息（远端）时 `~` 当普通名称。
        assert_eq!(resolve_input(Posix, "/x", "~", None), "/x/~");
    }

    #[test]
    fn windows_input_variants() {
        assert_eq!(resolve_input(Windows, r"C:\a", "b", None), r"C:\a\b");
        assert_eq!(resolve_input(Windows, r"C:\a", r"..\b", None), r"C:\b");
        assert_eq!(resolve_input(Windows, r"C:\a", r"\x", None), r"C:\x");
        assert_eq!(resolve_input(Windows, r"C:\a", "d:", None), r"D:\");
        assert_eq!(resolve_input(Windows, r"C:\a", "D:/x/y", None), r"D:\x\y");
        assert_eq!(resolve_input(Windows, THIS_PC, "c:", None), r"C:\");
        assert_eq!(
            resolve_input(Windows, r"C:\a", r"~\z", Some(r"C:\Users\me")),
            r"C:\Users\me\z"
        );
    }

    #[test]
    fn breadcrumbs_lead_back_to_every_ancestor() {
        let b = breadcrumbs(Posix, "/a/b");
        assert_eq!(
            b.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["/", "/a", "/a/b"]
        );
        let b = breadcrumbs(Windows, r"C:\a\b");
        assert_eq!(
            b.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["", r"C:\", r"C:\a", r"C:\a\b"]
        );
        assert_eq!(b[0].label, "此电脑");
        let b = breadcrumbs(Posix, "/C:/dir");
        assert_eq!(b[1].path, "/C:/");
        assert_eq!(breadcrumbs(Windows, THIS_PC).len(), 1);
    }

    #[test]
    fn virtual_root_and_roots() {
        assert!(is_virtual_root(Windows, ""));
        assert!(!is_virtual_root(Posix, ""));
        assert!(is_root(Windows, r"C:\"));
        assert!(is_root(Windows, "C:"));
        assert!(!is_root(Windows, r"C:\a"));
        assert!(is_root(Posix, "/"));
        assert!(is_hidden_name(".git"));
        assert!(!is_hidden_name("a"));
    }
}
