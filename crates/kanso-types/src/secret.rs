//! 凭据字符串包装。

use std::fmt;

/// 敏感字符串（密码、口令）。
///
/// `Debug` 与 `Display` 一律输出 `<redacted>`，避免凭据被日志或 panic 信息带出。
/// 刻意**不实现** `Serialize`：这样凭据无法被误写进导出文件或事件序列化（store-import.md §2）。
/// 需要明文时必须显式调用 [`SecretString::expose`]，便于审查调用点。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// 包装一段明文。
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 取出明文。仅在真正需要把凭据交给认证流程时调用，不得写入日志。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_hide_content() {
        let s = SecretString::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert!(!format!("{s}").contains("hunter2"));
        assert_eq!(format!("{s:?}"), "<redacted>");
        assert_eq!(s.expose(), "hunter2");
    }
}
