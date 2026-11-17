//! 界面文字的语言切换（ui-shell.md §10 语言、architecture.md 多语言一节）。
//!
//! - 核心产生的消息（错误、警告、导入报告）由 `kanso-l10n` 按 `locales/*.ftl` 渲染，这里只决定用哪种语言；
//! - 界面自己的文字用 [`tr`]：源文是简体中文，英文译文在 [`super::i18n_en`] 的表里，
//!   表里没有的条目回落到源文（不会显示空白或 key）。简体中文下 [`tr`] 原样返回，零开销。
//! - 当前语言是进程内的全局状态：设置变化时由外壳调用 [`set_lang`]，各视图每次绘制 / 每次建菜单时重新取文字。
//!
//! 本模块不依赖 gpui，三个平台都能编译与测试。

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

pub use kanso_l10n::Lang;

use super::i18n_en::EN;

/// 设置里的语言选项。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguageSetting {
    /// 跟随系统（默认）。
    #[default]
    System,
    /// 简体中文。
    ZhHans,
    /// English。
    En,
}

impl LanguageSetting {
    /// 全部取值，界面按此顺序排列。
    pub const ALL: [LanguageSetting; 3] = [
        LanguageSetting::System,
        LanguageSetting::ZhHans,
        LanguageSetting::En,
    ];

    /// 设置表里的存储值（发布后不得改）。
    pub fn storage(self) -> &'static str {
        match self {
            LanguageSetting::System => "system",
            LanguageSetting::ZhHans => "zh-Hans",
            LanguageSetting::En => "en",
        }
    }

    /// 从存储值还原；不认识返回 `None`。
    pub fn from_storage(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.storage() == s)
    }

    /// 显示名。两种语言的名字永远用各自的母语写，方便误切后找回来。
    pub fn label(self) -> &'static str {
        match self {
            LanguageSetting::System => tr("跟随系统"),
            LanguageSetting::ZhHans => "简体中文",
            LanguageSetting::En => "English",
        }
    }

    /// 解析成实际语言；`system` 是系统的语言偏好（按偏好顺序），都不支持时回落英文。
    pub fn resolve(self, system: &[&str]) -> Lang {
        match self {
            LanguageSetting::System => Lang::negotiate(system),
            LanguageSetting::ZhHans => Lang::ZhHans,
            LanguageSetting::En => Lang::En,
        }
    }
}

/// 0 = 简体中文，1 = 英文。
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 切换当前界面语言。
pub fn set_lang(lang: Lang) {
    CURRENT.store(u8::from(lang == Lang::En), Ordering::Relaxed);
}

/// 当前界面语言。
pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::ZhHans
    }
}

/// 渲染核心消息用的语言列表（`kanso_l10n::render` 的 `languages` 参数）。
pub fn languages() -> [&'static str; 1] {
    [lang().tag()]
}

fn en_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| EN.iter().copied().collect())
}

/// 按指定语言取界面文字：简体中文原样返回源文，英文查表，没有译文就回落源文。
pub fn tr_in(lang: Lang, zh: &'static str) -> &'static str {
    match lang {
        Lang::ZhHans => zh,
        Lang::En => en_table().get(zh).copied().unwrap_or(zh),
    }
}

/// 按当前语言取界面文字。参数必须是字符串字面量（源文），这样译文表可以静态检查。
pub fn tr(zh: &'static str) -> &'static str {
    tr_in(lang(), zh)
}

/// 把模板里的 `{}` 依次换成参数（模板自己也要先翻译：`fmt(tr("共 {} 个"), &[&n])`）。
/// 参数比占位符少时多出的占位符原样保留，参数多时忽略多余的。
pub fn fmt(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        match args.next() {
            Some(a) => out.push_str(&a.to_string()),
            None => out.push_str("{}"),
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    #[test]
    fn storage_round_trip_and_resolve() {
        for s in LanguageSetting::ALL {
            assert_eq!(LanguageSetting::from_storage(s.storage()), Some(s));
        }
        assert_eq!(LanguageSetting::from_storage("fr"), None);
        assert_eq!(LanguageSetting::default(), LanguageSetting::System);
        assert_eq!(
            LanguageSetting::System.resolve(&["zh-CN", "en-US"]),
            Lang::ZhHans
        );
        assert_eq!(LanguageSetting::System.resolve(&["en-US"]), Lang::En);
        // 系统语言都不支持：回落英文。
        assert_eq!(LanguageSetting::System.resolve(&["ja-JP"]), Lang::En);
        assert_eq!(LanguageSetting::En.resolve(&["zh-CN"]), Lang::En);
        assert_eq!(LanguageSetting::ZhHans.resolve(&["en-US"]), Lang::ZhHans);
    }

    #[test]
    fn tr_in_falls_back_to_source() {
        assert_eq!(tr_in(Lang::ZhHans, "语言"), "语言");
        assert_eq!(tr_in(Lang::En, "语言"), "Language");
        // 表里没有：回落源文。
        assert_eq!(tr_in(Lang::En, "表里一定没有的句子"), "表里一定没有的句子");
    }

    #[test]
    fn fmt_substitutes_in_order() {
        assert_eq!(fmt("{} 台，{} 台已存在", &[&3, &1]), "3 台，1 台已存在");
        assert_eq!(fmt("无占位", &[&1]), "无占位");
        assert_eq!(fmt("{} 和 {}", &[&"a"]), "a 和 {}");
    }

    #[test]
    fn table_is_well_formed() {
        let mut seen = HashSet::new();
        for (zh, en) in EN {
            assert!(seen.insert(*zh), "译文表里重复的源文：{zh}");
            assert!(!en.trim().is_empty(), "空译文：{zh}");
            assert_eq!(
                zh.matches("{}").count(),
                en.matches("{}").count(),
                "占位符数量不一致：{zh}"
            );
        }
    }

    /// 递归收集 `src` 下所有 `.rs` 文件。
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// 从 `s` 开头解析一个字符串字面量（`s` 以 `"` 开头），返回内容。只处理 `\" \\ \n \t`。
    fn literal(s: &str) -> Option<String> {
        let mut chars = s.strip_prefix('"')?.chars();
        let mut out = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    }

    /// 安全网：源码里每个 `tr("…")` 的字面量都必须有英文译文（本模块与译文表自己除外）。
    #[test]
    fn every_tr_literal_has_translation() {
        let mut files = Vec::new();
        rust_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut missing = Vec::new();
        for f in files {
            if f.file_name()
                .is_some_and(|n| n == "i18n.rs" || n == "i18n_en.rs")
            {
                continue;
            }
            let src = std::fs::read_to_string(&f).unwrap();
            let mut from = 0;
            while let Some(i) = src[from..].find("tr(\"") {
                let at = from + i;
                from = at + 4;
                // 前一个字符不能是标识符的一部分（排除 `str(` 之类）。
                let prev = src[..at].chars().next_back();
                if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                if let Some(text) = literal(&src[at + 3..])
                    && !en_table().contains_key(text.as_str())
                {
                    missing.push(format!("{}: {text}", f.display()));
                }
            }
        }
        assert!(missing.is_empty(), "缺英文译文：\n{}", missing.join("\n"));
    }
}
