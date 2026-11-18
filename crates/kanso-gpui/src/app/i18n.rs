//! 界面文字的语言切换（ui-shell.md §10 语言、architecture.md 多语言一节）。
//!
//! 界面文字与核心消息一样，全部走 `kanso-types` 的 `Message` + `locales/*.ftl`（由 `kanso-l10n` 渲染），
//! 本模块只管“用哪种语言”：设置里的语言选项、解析成 `kanso-l10n` 的 `Lang`、当前语言的全局状态，
//! 以及把 `Message` 渲染成界面文字的两个小函数（[`t`] 返回 `String`，[`label`] 返回缓存的 `&'static str`）。
//! 设置变化时由外壳调用 [`set_lang`]，各视图每次绘制 / 每次建菜单时重新取文字。
//!
//! 本模块不依赖 gpui，三个平台都能编译与测试。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

pub use kanso_l10n::Lang;
use kanso_types::message::Message;

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
            LanguageSetting::System => label(Message::UiFollowSystem {}),
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

/// 按指定语言渲染消息。
pub fn t_in(lang: Lang, message: &Message) -> String {
    kanso_l10n::render(message, &[lang.tag()])
}

/// 按当前语言渲染消息。
pub fn t(message: &Message) -> String {
    t_in(lang(), message)
}

/// 按指定语言取不带参数的界面文字，返回 `&'static str`（每种语言每条消息只渲染一次并缓存，
/// 条目数有限，缓存常驻）。让菜单标签、按钮文字这类要求 `&'static str` 的调用点不必改签名。
/// 带参数的消息请用 [`t`]。
pub fn label_in(lang: Lang, message: Message) -> &'static str {
    static CACHE: OnceLock<Mutex<HashMap<(Lang, &'static str), &'static str>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache
        .entry((lang, message.key()))
        .or_insert_with(|| Box::leak(t_in(lang, &message).into_boxed_str()))
}

/// 按当前语言取不带参数的界面文字（见 [`label_in`]）。
pub fn label(message: Message) -> &'static str {
    label_in(lang(), message)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn label_renders_per_language_and_is_cached() {
        let zh = label_in(Lang::ZhHans, Message::UiLanguage {});
        let en = label_in(Lang::En, Message::UiLanguage {});
        assert_eq!(zh, "语言");
        assert_eq!(en, "Language");
        // 第二次取到的是同一份缓存。
        assert!(std::ptr::eq(en, label_in(Lang::En, Message::UiLanguage {})));
    }

    #[test]
    fn t_substitutes_arguments() {
        let m = Message::UiAndMore { count: 3 };
        assert_eq!(t_in(Lang::ZhHans, &m), "…另外 3 个");
        assert_eq!(t_in(Lang::En, &m), "…and 3 more");
    }
}
