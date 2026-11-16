//! UTF-8 locale 的自动决策（local-shell.md §4）。

use crate::command::Platform;
use crate::env::EnvSnapshot;
use kanso_types::profile::Locale;

/// 系统语言与地区设置，由调用方（Swift 一侧读 `Locale.preferredLanguages` 与 `Locale.current`）传入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemLocaleInfo {
    /// 首选语言标识，如 `zh-Hans-CN`、`en`、`pt-BR`、`en_US@calendar=gregorian`。
    pub preferred_language: String,
    /// 用户在系统设置里选的地区代码，如 `CN`；没有就是 `None`。
    pub region: Option<String>,
}

/// macOS 上各语言的"常用地区"（local-shell.md §4）。
const COMMON_REGIONS: &[(&str, &str)] = &[
    ("en", "US"),
    ("zh", "CN"),
    ("ja", "JP"),
    ("ko", "KR"),
    ("de", "DE"),
    ("fr", "FR"),
    ("es", "ES"),
    ("it", "IT"),
    ("pt", "BR"),
    ("ru", "RU"),
    ("nl", "NL"),
    ("sv", "SE"),
    ("pl", "PL"),
    ("tr", "TR"),
];

/// 兜底 locale：macOS 一定自带。
const FALLBACK_MACOS: &str = "en_US.UTF-8";

/// 按 local-shell.md §4 的四步顺序决定要设置的 `LANG`；返回 `None` 表示不设置。
///
/// 1. `explicit`（配置里明确填写的 locale）合规就直接用；不合规（含空串）视为没填，继续往下，
///    不把不合规的值写进环境变量（它可能被拼进命令，所以复用 [`Locale::parse`] 的校验）。
/// 2. 会话编码不是 UTF-8（`session_is_utf8 == false`，如 GBK）：不设置，只有用户知道该用哪个 GBK locale。
/// 3. 环境里已有非空的 `LC_ALL` / `LC_CTYPE` / `LANG`：不动。
/// 4. 自动选：macOS 按系统语言和地区推算候选，取第一个 `locale_exists` 为真的，都没有用
///    `en_US.UTF-8`；Linux / BSD 用 `C.UTF-8`；Windows 不设置。
///
/// `locale_exists` 判断"系统是否带有某个 locale 名"，真实实现见 [`system_locale_exists`]；
/// 作为闭包注入是为了测试不依赖本机安装了哪些 locale。
pub fn decide_locale(
    explicit: Option<&str>,
    session_is_utf8: bool,
    env: &EnvSnapshot,
    platform: Platform,
    system: &SystemLocaleInfo,
    locale_exists: impl Fn(&str) -> bool,
) -> Option<String> {
    if let Some(l) = explicit.and_then(Locale::parse) {
        return Some(l.as_str().to_owned());
    }
    if !session_is_utf8 {
        return None;
    }
    if ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .any(|k| env.get_non_empty(k).is_some())
    {
        return None;
    }
    match platform {
        Platform::MacOs => Some(macos_locale(system, locale_exists)),
        Platform::Linux | Platform::Bsd => Some("C.UTF-8".to_owned()),
        Platform::Windows => None,
    }
}

fn macos_locale(system: &SystemLocaleInfo, locale_exists: impl Fn(&str) -> bool) -> String {
    let (lang, tag_region) = parse_language_tag(&system.preferred_language);
    if let Some(lang) = lang {
        let mut regions: Vec<String> = Vec::new();
        // 候选顺序：用户所在地区 → 语言标识自带的地区（如 en-GB）→ 该语言的常用地区。
        // 中间一项是对规格的小扩展：规格只写了前后两项，但 `en-GB` 这类标识里的地区
        // 明显比"常用地区"更贴近用户，且仍然要经过"系统带有这个 locale"的检查。
        if let Some(r) = system.region.as_deref().and_then(normalize_region) {
            regions.push(r);
        }
        if let Some(r) = tag_region {
            regions.push(r);
        }
        if let Some((_, r)) = COMMON_REGIONS.iter().find(|(l, _)| *l == lang) {
            regions.push((*r).to_owned());
        }
        for r in regions {
            let name = format!("{lang}_{r}.UTF-8");
            // 名称会被写进环境变量，再过一遍 Locale 校验作为兜底。
            if Locale::parse(&name).is_some() && locale_exists(&name) {
                return name;
            }
        }
    }
    FALLBACK_MACOS.to_owned()
}

/// 拆出语言与（可选的）地区：去掉 `@` 后的修饰、`.` 后的编码，丢掉书写体系。
/// `zh-Hans-CN` → (zh, CN)；`zh-Hans` → (zh, None)；`en_US@x=y` → (en, US)。
fn parse_language_tag(tag: &str) -> (Option<String>, Option<String>) {
    let tag = tag.split('@').next().unwrap_or("");
    let tag = tag.split('.').next().unwrap_or("");
    let mut parts = tag.split(['-', '_']).filter(|p| !p.is_empty());
    let lang = parts
        .next()
        .filter(|l| (2..=3).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(str::to_ascii_lowercase);
    let region = parts.find_map(normalize_region);
    (lang, region)
}

/// 只接受 2 个字母（`CN`）或 3 个数字（`419`）的地区码，其余（如 4 字母的书写体系 `Hans`）丢弃。
fn normalize_region(r: &str) -> Option<String> {
    let ok = (r.len() == 2 && r.bytes().all(|b| b.is_ascii_alphabetic()))
        || (r.len() == 3 && r.bytes().all(|b| b.is_ascii_digit()));
    ok.then(|| r.to_ascii_uppercase())
}

/// 真实的"系统是否带有这个 locale"：检查 `/usr/share/locale/<名称>` 目录是否存在（macOS）。
pub fn system_locale_exists(name: &str) -> bool {
    // 名称来自 `Locale::parse` 校验过的候选，不含路径分隔符，不会逃出目录。
    std::path::Path::new("/usr/share/locale")
        .join(name)
        .is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(p: &[(&str, &str)]) -> EnvSnapshot {
        EnvSnapshot::from_pairs(p.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn sys(lang: &str, region: Option<&str>) -> SystemLocaleInfo {
        SystemLocaleInfo {
            preferred_language: lang.into(),
            region: region.map(Into::into),
        }
    }

    fn all(_: &str) -> bool {
        true
    }

    fn only(list: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n| list.contains(&n)
    }

    #[test]
    fn explicit_wins_even_for_non_utf8_session_and_existing_env() {
        let e = env(&[("LANG", "C")]);
        let r = decide_locale(
            Some("zh_CN.GBK"),
            false,
            &e,
            Platform::MacOs,
            &sys("en", None),
            all,
        );
        assert_eq!(r.as_deref(), Some("zh_CN.GBK"));
    }

    #[test]
    fn invalid_explicit_is_ignored() {
        let r = decide_locale(
            Some("en US;rm"),
            true,
            &env(&[]),
            Platform::Linux,
            &sys("en", None),
            all,
        );
        assert_eq!(r.as_deref(), Some("C.UTF-8"));
        let r = decide_locale(
            Some(""),
            true,
            &env(&[]),
            Platform::Linux,
            &sys("en", None),
            all,
        );
        assert_eq!(r.as_deref(), Some("C.UTF-8"));
    }

    #[test]
    fn non_utf8_session_not_set() {
        let r = decide_locale(
            None,
            false,
            &env(&[]),
            Platform::MacOs,
            &sys("zh-Hans-CN", None),
            all,
        );
        assert_eq!(r, None);
    }

    #[test]
    fn existing_variables_untouched() {
        for k in ["LC_ALL", "LC_CTYPE", "LANG"] {
            let e = env(&[(k, "en_GB.UTF-8")]);
            let r = decide_locale(
                None,
                true,
                &e,
                Platform::MacOs,
                &sys("zh-Hans-CN", None),
                all,
            );
            assert_eq!(r, None, "{k}");
        }
        // 空值视同没有。
        let e = env(&[("LANG", "")]);
        let r = decide_locale(
            None,
            true,
            &e,
            Platform::MacOs,
            &sys("zh-Hans-CN", None),
            all,
        );
        assert_eq!(r.as_deref(), Some("zh_CN.UTF-8"));
    }

    #[test]
    fn macos_candidates() {
        let m = |s: SystemLocaleInfo, exists: &'static [&'static str]| {
            decide_locale(None, true, &env(&[]), Platform::MacOs, &s, only(exists))
        };
        // 书写体系被去掉，语言 + 常用地区。
        assert_eq!(
            m(sys("zh-Hans-CN", None), &["zh_CN.UTF-8"]).as_deref(),
            Some("zh_CN.UTF-8")
        );
        assert_eq!(
            m(sys("zh-Hans", None), &["zh_CN.UTF-8"]).as_deref(),
            Some("zh_CN.UTF-8")
        );
        // 英文 + 地区 CN：没有 en_CN，退回 en_US。
        assert_eq!(
            m(sys("en", Some("CN")), &["en_US.UTF-8", "zh_CN.UTF-8"]).as_deref(),
            Some("en_US.UTF-8")
        );
        // 用户地区优先于常用地区。
        assert_eq!(
            m(sys("en", Some("GB")), &["en_GB.UTF-8", "en_US.UTF-8"]).as_deref(),
            Some("en_GB.UTF-8")
        );
        // 去掉 @ 修饰。
        assert_eq!(
            m(sys("de@calendar=gregorian", None), &["de_DE.UTF-8"]).as_deref(),
            Some("de_DE.UTF-8")
        );
        // 语言标识自带的地区。
        assert_eq!(
            m(sys("pt-PT", None), &["pt_PT.UTF-8", "pt_BR.UTF-8"]).as_deref(),
            Some("pt_PT.UTF-8")
        );
        // 都不存在 / 不认识的语言 / 空语言：en_US.UTF-8。
        assert_eq!(
            m(sys("zh-Hans-CN", None), &[]).as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(
            m(sys("xx", None), &["xx_XX.UTF-8"]).as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(m(sys("", None), &[]).as_deref(), Some("en_US.UTF-8"));
        // 恶意输入不会产生非法名称。
        assert_eq!(
            m(sys("zh;rm -rf", Some("C N")), &[]).as_deref(),
            Some("en_US.UTF-8")
        );
    }

    #[test]
    fn other_platforms() {
        let s = sys("zh-Hans-CN", None);
        let d = |p| decide_locale(None, true, &env(&[]), p, &s, all);
        assert_eq!(d(Platform::Linux).as_deref(), Some("C.UTF-8"));
        assert_eq!(d(Platform::Bsd).as_deref(), Some("C.UTF-8"));
        assert_eq!(d(Platform::Windows), None);
    }
}
