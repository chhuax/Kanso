//! 语言协商：把系统给出的语言标识归并到已编入的目录语言。

/// 已编入二进制的目录语言。新增语言时同步修改 [`Lang::ALL`]、[`Lang::negotiate_one`]
/// 与 `lib.rs` 里的资源表。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    /// 英文，同时是回退语言。
    En,
    /// 简体中文。
    ZhHans,
}

impl Lang {
    /// 全部支持的语言。
    pub const ALL: [Lang; 2] = [Lang::En, Lang::ZhHans];

    /// 回退语言：`languages` 里没有任何受支持的项时使用。
    pub const FALLBACK: Lang = Lang::En;

    /// 目录文件名使用的语言标签，同时作为 Fluent bundle 的 locale（决定复数规则）。
    pub const fn tag(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhHans => "zh-Hans",
        }
    }

    /// 按偏好顺序取第一个受支持的语言；全都不支持时回退英文。
    ///
    /// 不支持的项（例如 `zh-Hant`）要跳过而不是中断：用户偏好列表里后面
    /// 可能还有受支持的语言；全部落空才回退英文。
    pub fn negotiate(languages: &[&str]) -> Lang {
        languages
            .iter()
            .find_map(|l| Self::negotiate_one(l))
            .unwrap_or(Self::FALLBACK)
    }

    /// 归并单个语言标识；不支持返回 `None`。
    ///
    /// 同时接受 BCP 47（`zh-Hans-CN`）与 POSIX 风格（`zh_CN`、`en_US.UTF-8`）写法，
    /// 大小写不敏感。中文只有明确指向简体的写法才归到简体：繁体（`Hant` 或
    /// `TW`/`HK`/`MO` 地区）不能显示成简体，所以视为不支持。
    fn negotiate_one(tag: &str) -> Option<Lang> {
        // 去掉 POSIX 的 `.UTF-8` / `@modifier` 后缀。
        let base = tag.split(['.', '@']).next().unwrap_or("");
        let lower = base.replace('_', "-").to_ascii_lowercase();
        let mut parts = lower.split('-');
        match parts.next()? {
            "en" => Some(Lang::En),
            "zh" => {
                let rest: Vec<&str> = parts.collect();
                let traditional = rest
                    .iter()
                    .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"));
                if traditional {
                    None
                } else {
                    // `zh`、`zh-Hans`、`zh-CN`、`zh-SG` 等；其余脚本（如 `Latn`）不是中文文字，不支持。
                    let other_script = rest.iter().any(|p| p.len() == 4 && *p != "hans");
                    (!other_script).then_some(Lang::ZhHans)
                }
            }
            _ => None,
        }
    }
}
