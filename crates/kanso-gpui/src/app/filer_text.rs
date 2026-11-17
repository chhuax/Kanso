//! 文件面板与传输界面的文字：按 key 查 `locales/*.ftl`（key 以 `ui-filer-` / `ui-xfer-` 开头）。
//!
//! 与 `launcher::render_message` 同一套本地化渲染（kanso-l10n），目前界面只有简体中文。
//! key 缺失时 debug 构建会 panic（单元测试因此能发现漏写），release 返回 key 本身。

use kanso_l10n::ArgValue;

/// 界面语言（目前只有简体中文）。
const LANGUAGES: &[&str] = &["zh-Hans"];

/// 无参数文字。
pub fn t(key: &str) -> String {
    kanso_l10n::render_key(key, &[], LANGUAGES)
}

/// 带命名参数的文字（参数一律按普通文本代入）。
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let args: Vec<(String, ArgValue)> = args
        .iter()
        .map(|(k, v)| ((*k).to_owned(), ArgValue::Str((*v).to_owned())))
        .collect();
    kanso_l10n::render_key(key, &args, LANGUAGES)
}

/// 带一个整数参数 `count` 的文字（Fluent 复数选择用）。
pub fn tn(key: &str, count: usize) -> String {
    kanso_l10n::render_key(
        key,
        &[("count".to_owned(), ArgValue::Int(count as i64))],
        LANGUAGES,
    )
}
