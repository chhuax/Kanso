//! 把 core 产生的 [`Message`](kanso_types::message::Message) 渲染成本地化文字。
//!
//! `locales/*.ftl` 是 core 消息的唯一来源，这里用 `fluent-bundle` 在运行时渲染：
//! Fluent 的 select 表达式、复数和 `reason-*` / `endpoint-*` 子 key 嵌套没法忠实映射成
//! 平台自己的字符串目录，所以由 Rust 统一渲染，Swift 与以后的 GPUI 版共用同一个实现
//! （决定与理由见 docs/architecture.md §10）。
//!
//! 渲染不会 panic：本 crate 经 FFI 被调用，release 下 panic 穿过 FFI 会直接崩溃。
//! 出错时返回尽量完整的文本，debug 构建用 `debug_assert!` 暴露问题，release 下记日志。

mod lang;

use std::fmt::Write as _;
use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use kanso_types::message::{Message, MessageArg};

pub use lang::Lang;

/// 编入二进制的目录。路径相对本文件；这样 App 不需要在运行时找资源文件。
const EN_FTL: &str = include_str!("../../../locales/en.ftl");
const ZH_HANS_FTL: &str = include_str!("../../../locales/zh-Hans.ftl");

/// 渲染所需的参数值，与 [`MessageArg`] 对应，但全部是拥有所有权的值，且 `Select` / `Reason`
/// 用字符串表示，方便从 FFI 的扁平结构直接构造。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgValue {
    /// 普通文本，原样代入。
    Str(String),
    /// 整数，作为数字传给 Fluent，复数选择依赖它。
    Int(i64),
    /// 字节数。平台应先用系统格式化器转成 [`ArgValue::Str`]；
    /// 这里只提供兜底格式（`1.2 MB`，十进制单位，与 macOS `.file` 风格一致）。
    ByteSize(u64),
    /// 列表。平台应先用系统列表格式化器转成 [`ArgValue::Str`]；兜底用 `, ` 连接。
    StrList(Vec<String>),
    /// Fluent 选择器的取值，作为字符串代入。
    Select(String),
    /// 失败原因：`key` 为 `reason-*` 子 key，`detail` 仅 `reason-other` 使用。
    Reason {
        /// 子 key。
        key: String,
        /// 英文技术细节，不本地化。
        detail: Option<String>,
    },
    /// 连接对象，先渲染成一个字符串再代入。
    Endpoint {
        /// 主机名或地址。
        host: String,
        /// 端口。
        port: u16,
        /// 经过的跳板机显示名；空表示直连。
        via: Vec<String>,
    },
}

impl From<&MessageArg> for ArgValue {
    fn from(a: &MessageArg) -> Self {
        match a {
            MessageArg::Str(s) => ArgValue::Str(s.clone()),
            MessageArg::Int(i) => ArgValue::Int(*i),
            MessageArg::ByteSize(b) => ArgValue::ByteSize(*b),
            MessageArg::StrList(v) => ArgValue::StrList(v.clone()),
            MessageArg::Select(k) => ArgValue::Select((*k).to_owned()),
            MessageArg::Reason(r) => ArgValue::Reason {
                key: r.key().to_owned(),
                detail: r.detail().map(str::to_owned),
            },
            MessageArg::Endpoint(e) => ArgValue::Endpoint {
                host: e.host.clone(),
                port: e.port,
                via: e.via.clone(),
            },
        }
    }
}

/// 渲染 [`Message`]。
///
/// `languages` 是按偏好排序的语言标识（如 `Bundle.main.preferredLocalizations`），
/// 协商规则见 [`Lang::negotiate`]。永不 panic；出错时返回尽量完整的文本。
pub fn render(message: &Message, languages: &[&str]) -> String {
    let args: Vec<(String, ArgValue)> = message
        .args()
        .iter()
        .map(|(name, value)| ((*name).to_owned(), ArgValue::from(value)))
        .collect();
    render_key(message.key(), &args, languages)
}

/// 面向 FFI 的底层渲染：按 key 与已转换的命名参数渲染。
///
/// key 不存在时返回 key 本身；参数缺失等错误返回尽量完整的文本。永不 panic。
pub fn render_key(key: &str, args: &[(String, ArgValue)], languages: &[&str]) -> String {
    match try_render_key(key, args, Lang::negotiate(languages)) {
        Ok(text) => text,
        Err(failure) => {
            // debug 构建让开发期立刻发现 .ftl 与 CATALOG 不一致；release 只记日志，
            // 因为 panic 穿过 FFI 会让 App 崩溃，而用户看到不完整的文字远好于崩溃。
            tracing::error!(key, errors = ?failure.errors, "message render failed");
            debug_assert!(false, "渲染 {key} 失败：{:?}", failure.errors);
            failure.text
        }
    }
}

/// 渲染失败时的结果：仍带有尽量完整的文本，便于调用方回退。
#[derive(Debug)]
pub(crate) struct RenderFailure {
    pub(crate) text: String,
    pub(crate) errors: Vec<String>,
}

/// 渲染并返回错误明细，不触发 `debug_assert!`（供测试检查错误路径）。
pub(crate) fn try_render_key(
    key: &str,
    args: &[(String, ArgValue)],
    lang: Lang,
) -> Result<String, RenderFailure> {
    let bundle = bundle(lang);
    let mut errors = Vec::new();
    let Some(message) = bundle.get_message(key) else {
        errors.push(format!("unknown message key: {key}"));
        return Err(RenderFailure {
            text: key.to_owned(),
            errors,
        });
    };
    let Some(pattern) = message.value() else {
        errors.push(format!("message has no value: {key}"));
        return Err(RenderFailure {
            text: key.to_owned(),
            errors,
        });
    };

    // 先把需要二次渲染的参数（Reason / Endpoint）变成字符串，再交给 Fluent。
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        let v = to_fluent_value(bundle, value, &mut errors);
        fluent_args.set(name.as_str(), v);
    }
    let mut fluent_errors = Vec::new();
    let text = bundle
        .format_pattern(pattern, Some(&fluent_args), &mut fluent_errors)
        .into_owned();
    errors.extend(fluent_errors.iter().map(|e| e.to_string()));
    if errors.is_empty() {
        Ok(text)
    } else {
        Err(RenderFailure { text, errors })
    }
}

fn to_fluent_value(
    bundle: &'static Bundle,
    value: &ArgValue,
    errors: &mut Vec<String>,
) -> FluentValue<'static> {
    match value {
        ArgValue::Str(s) | ArgValue::Select(s) => FluentValue::from(s.clone()),
        ArgValue::Int(i) => FluentValue::from(*i),
        ArgValue::ByteSize(b) => FluentValue::from(fallback_byte_size(*b)),
        ArgValue::StrList(v) => FluentValue::from(v.join(", ")),
        ArgValue::Reason { key, detail } => {
            let mut sub = FluentArgs::new();
            if let Some(d) = detail {
                sub.set("detail", d.clone());
            }
            FluentValue::from(render_sub(bundle, key, &sub, errors))
        }
        ArgValue::Endpoint { host, port, via } => {
            let host_port = kanso_types::message::Endpoint {
                host: host.clone(),
                port: *port,
                via: via.clone(),
            }
            .host_port();
            let mut sub = FluentArgs::new();
            sub.set("host_port", host_port);
            let key = if via.is_empty() {
                "endpoint-direct"
            } else {
                // 跳板链用列表格式连接：与 StrList 的兜底一致。平台没有为嵌套在
                // Endpoint 里的列表单独提供格式化入口，这是有意的取舍。
                sub.set("via", via.join(", "));
                "endpoint-via"
            };
            FluentValue::from(render_sub(bundle, key, &sub, errors))
        }
    }
}

/// 渲染子 key；失败时记录错误并用 key 本身代替，保证外层消息仍可读。
fn render_sub(bundle: &Bundle, key: &str, args: &FluentArgs, errors: &mut Vec<String>) -> String {
    let Some(pattern) = bundle.get_message(key).and_then(|m| m.value()) else {
        errors.push(format!("unknown sub key: {key}"));
        return key.to_owned();
    };
    let mut fluent_errors = Vec::new();
    let text = bundle
        .format_pattern(pattern, Some(args), &mut fluent_errors)
        .into_owned();
    errors.extend(fluent_errors.iter().map(|e| format!("{key}: {e}")));
    text
}

/// 兜底的字节数格式：十进制单位，最多一位小数（`1.2 MB`、`512 B`）。
fn fallback_byte_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let mut s = String::new();
    if unit == 0 {
        let _ = write!(s, "{bytes} B");
    } else {
        let one_decimal = format!("{value:.1}");
        let trimmed = one_decimal.strip_suffix(".0").unwrap_or(&one_decimal);
        let _ = write!(s, "{trimmed} {}", UNITS[unit]);
    }
    s
}

/// 并发版 bundle：内部的 `IntlLangMemoizer` 用 `Mutex` 保护，满足 `Sync`，
/// 才能放进全局 `OnceLock` 被多个线程同时渲染。非 concurrent 版是 `!Sync`，不能换。
type Bundle = FluentBundle<FluentResource>;

/// 构建一种语言的 bundle，并返回构建过程的错误（解析错误、key 重复）。
pub(crate) fn build_bundle(lang: Lang) -> (Bundle, Vec<String>) {
    let source = match lang {
        Lang::En => EN_FTL,
        Lang::ZhHans => ZH_HANS_FTL,
    };
    let mut errors = Vec::new();
    let resource = FluentResource::try_new(source.to_owned()).unwrap_or_else(|(res, errs)| {
        errors.extend(errs.iter().map(|e| format!("{e:?}")));
        res
    });
    let langid = lang
        .tag()
        .parse()
        .expect("Lang::tag 返回的都是合法的语言标识");
    let mut bundle = FluentBundle::new_concurrent(vec![langid]);
    // 必须关闭双向隔离：默认会在每个变量两侧插入 U+2068 / U+2069，
    // 这些不可见字符复制到终端、日志或剪贴板里会变成乱码。
    // Kanso 暂不支持从右到左的语言（architecture §10），关闭没有排版上的损失。
    bundle.set_use_isolating(false);
    if let Err(errs) = bundle.add_resource(resource) {
        errors.extend(errs.iter().map(|e| e.to_string()));
    }
    (bundle, errors)
}

/// 取某语言的缓存 bundle；首次使用时构建，之后复用。
fn bundle(lang: Lang) -> &'static Bundle {
    static EN: OnceLock<Bundle> = OnceLock::new();
    static ZH_HANS: OnceLock<Bundle> = OnceLock::new();
    let cell = match lang {
        Lang::En => &EN,
        Lang::ZhHans => &ZH_HANS,
    };
    cell.get_or_init(|| {
        let (bundle, errors) = build_bundle(lang);
        if !errors.is_empty() {
            // 目录是编译期嵌入的，出错只能是开发期的 .ftl 问题；check-locales 与单元测试会拦截。
            tracing::error!(lang = lang.tag(), ?errors, "locale catalog has errors");
            debug_assert!(false, "目录 {} 有错误：{errors:?}", lang.tag());
        }
        bundle
    })
}

#[cfg(test)]
mod tests;
