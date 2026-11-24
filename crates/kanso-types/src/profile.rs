//! 会话配置（Profile）、分组与快捷片段。
//!
//! 本模块的类型**不含任何密码或口令**：凭据由 kanso-secret 单独保管，
//! 配置与导出文件里只出现“认证方式”和私钥文件路径（store-import.md §2）。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::geometry::Rgb;
use crate::ids::{GroupId, ProfileId, SnippetId};

/// 会话种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionKind {
    /// 本地 shell。
    Local,
    /// SSH 终端。
    Ssh,
    /// 纯 SFTP 文件会话（无终端）。
    Sftp,
}

/// SSH 认证方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthMethod {
    /// 密码认证（含 keyboard-interactive 的口令回退）。
    #[default]
    Password,
    /// 私钥文件认证。
    PrivateKey,
    /// 通过 ssh-agent 认证。
    Agent,
}

/// 终端字符编码标签的规范化存储。
///
/// 这里只做“去首尾空格 + 转小写”；空串表示 UTF-8。标签按 WHATWG 编码标准解析、
/// 未知标签回退 UTF-8 都是 kanso-encoding 的职责（encoding-transfer.md §1.1），
/// 本类型不校验标签是否存在。反序列化同样走规范化。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct EncodingLabel(String);

impl EncodingLabel {
    /// 规范化后构造。
    pub fn new(label: &str) -> Self {
        Self(label.trim().to_ascii_lowercase())
    }

    /// 是否表示 UTF-8（空串）。显式写成 `utf-8` 的标签不算，交由 kanso-encoding 解析。
    pub fn is_utf8(&self) -> bool {
        self.0.is_empty()
    }

    /// 规范化后的标签文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for EncodingLabel {
    fn from(s: String) -> Self {
        Self::new(&s)
    }
}

impl From<EncodingLabel> for String {
    fn from(l: EncodingLabel) -> Self {
        l.0
    }
}

/// locale 字符串非法。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidLocale;

impl fmt::Display for InvalidLocale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid locale")
    }
}

impl std::error::Error for InvalidLocale {}

/// 经过校验的 locale 名（如 `zh_CN.UTF-8`）。
///
/// 它会被写进子进程环境变量或远端命令，所以只允许 ASCII 字母数字和 `_ - . @`，
/// 长度 1..=64，杜绝注入（local-shell.md §4、ssh.md §6）。反序列化非法值会失败。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Locale(String);

impl Locale {
    /// 校验并构造；非法返回 `None`。
    pub fn parse(s: &str) -> Option<Locale> {
        let ok = (1..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'@'));
        ok.then(|| Locale(s.to_owned()))
    }

    /// locale 文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Locale {
    type Error = InvalidLocale;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Locale::parse(&s).ok_or(InvalidLocale)
    }
}

impl From<Locale> for String {
    fn from(l: Locale) -> Self {
        l.0
    }
}

/// 本地 shell 会话配置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProfile {
    /// 启动命令；空表示使用默认 shell。
    pub shell_command: String,
    /// 起始目录；`None` 表示用户主目录。
    pub start_dir: Option<String>,
    /// 终端编码。
    pub encoding: EncodingLabel,
    /// 覆盖子进程 locale；`None` 表示沿用系统。
    pub locale: Option<Locale>,
}

/// SSH 保活配置（ssh.md §2.4）。
///
/// 两套保活机制：协议层 keepalive（由 russh 实现）与空闲时向 shell 发字符串（由会话层实现）。
/// 默认值保持规格现状：协议层开、30 秒、3 次无回应；空闲字符串关、3600 秒、`\r`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepaliveConfig {
    /// 是否启用 SSH 协议层 keepalive（默认开）。
    pub enabled: bool,
    /// 协议层 keepalive 间隔秒数（默认 30，范围 1..=86400）。
    pub interval_secs: u16,
    /// 连续多少次无回应判定断线（默认 3）。
    pub max_misses: u16,
    /// 是否在空闲时向 shell 发送保活字符串（默认关）。
    pub idle_enabled: bool,
    /// 网络空闲多少秒后发送一次（默认 3600，范围 1..=86400）。
    pub idle_interval_secs: u16,
    /// 空闲时发送的字符串，支持 \r \n \t \\ 转义（默认 "\r"）。
    pub idle_string: String,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            max_misses: 3,
            idle_enabled: false,
            idle_interval_secs: 3600,
            idle_string: "\\r".to_owned(),
        }
    }
}

impl KeepaliveConfig {
    /// 把空闲保活字符串里的 `\r` `\n` `\t` `\\` 展开成字节。
    ///
    /// 其他转义序列按字面处理（保留反斜杠），非 ASCII 字符按 UTF-8 原样输出。
    pub fn decoded_idle_string(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.idle_string.len());
        let mut chars = self.idle_string.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('r') => out.push(b'\r'),
                    Some('n') => out.push(b'\n'),
                    Some('t') => out.push(b'\t'),
                    Some('\\') => out.push(b'\\'),
                    Some(other) => {
                        out.push(b'\\');
                        let mut buf = [0; 4];
                        out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                    }
                    None => out.push(b'\\'),
                }
            } else {
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        out
    }
}

/// SSH 连接目标，终端会话与 SFTP 会话共用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshTarget {
    /// 主机名或地址。
    pub host: String,
    /// 端口，默认 22（见 [`DEFAULT_SSH_PORT`]）。
    pub port: u16,
    /// 用户名。
    pub user: String,
    /// 认证方式。
    pub auth: AuthMethod,
    /// 私钥文件路径，仅 `AuthMethod::PrivateKey` 使用。
    pub private_key_path: Option<String>,
    /// 跳板机：引用另一个 SSH Profile。与 `proxy_command` 二选一。
    pub jump: Option<ProfileId>,
    /// 代理命令。与 `jump` 二选一，且仅 Direct 渠道生效（ssh.md §5）。
    pub proxy_command: Option<String>,
    /// 保活配置；缺省时用 [`KeepaliveConfig::default`]（兼容旧配置与导出文件）。
    #[serde(default)]
    pub keepalive: KeepaliveConfig,
}

/// SSH 默认端口。
pub const DEFAULT_SSH_PORT: u16 = 22;

/// SSH 终端会话配置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProfile {
    /// 连接目标。
    pub target: SshTarget,
    /// 终端编码。
    pub encoding: EncodingLabel,
    /// 远端 locale；`None` 表示不设置。
    pub locale: Option<Locale>,
}

/// Profile 的具体类型与其专属配置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileKind {
    /// 本地 shell。
    Local(LocalProfile),
    /// SSH 终端。
    Ssh(SshProfile),
    /// 纯 SFTP。
    Sftp(SshTarget),
}

impl ProfileKind {
    /// 对应的会话种类。
    pub fn kind(&self) -> SessionKind {
        match self {
            ProfileKind::Local(_) => SessionKind::Local,
            ProfileKind::Ssh(_) => SessionKind::Ssh,
            ProfileKind::Sftp(_) => SessionKind::Sftp,
        }
    }
}

/// 一条会话配置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// 持久化 id。
    pub id: ProfileId,
    /// 显示名。
    pub name: String,
    /// 标签颜色；`None` 表示不着色。
    pub color: Option<Rgb>,
    /// 所属分组；`None` 表示未分组。
    pub group: Option<GroupId>,
    /// 种类与专属配置。
    pub kind: ProfileKind,
}

/// 分组，只有一层，不嵌套。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// 持久化 id。
    pub id: GroupId,
    /// 显示名。
    pub name: String,
}

/// 快捷片段发送时追加的行尾。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineEnding {
    /// 不追加（只粘贴文本，不执行）。
    None,
    /// 追加 `\n`。
    Lf,
    /// 追加 `\r\n`。
    CrLf,
}

/// 快捷片段按会话种类划分的作用域（不含 SFTP，它没有终端）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnippetKindScope {
    /// 所有本地会话。
    Local,
    /// 所有 SSH 会话。
    Ssh,
}

/// 快捷片段的作用域。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnippetScope {
    /// 仅限某个 Profile。
    Profile(ProfileId),
    /// 某个分组内的所有 Profile。
    Group(GroupId),
    /// 某一类会话。
    Kind(SnippetKindScope),
    /// 全局可用。
    Global,
}

/// 快捷片段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// 持久化 id。
    pub id: SnippetId,
    /// 显示名。
    pub name: String,
    /// 要发送的文本。
    pub text: String,
    /// 发送时追加的行尾。
    pub line_ending: LineEnding,
    /// 作用域。
    pub scope: SnippetScope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_parse() {
        assert!(Locale::parse("zh_CN.UTF-8").is_some());
        assert!(Locale::parse("de_DE@euro").is_some());
        assert!(Locale::parse("").is_none());
        assert!(Locale::parse("en US").is_none());
        assert!(Locale::parse("en;rm").is_none());
        assert!(Locale::parse(&"a".repeat(64)).is_some());
        assert!(Locale::parse(&"a".repeat(65)).is_none());
    }

    #[test]
    fn locale_deserialize_validates() {
        assert!(serde_json::from_str::<Locale>("\"en_US.UTF-8\"").is_ok());
        assert!(serde_json::from_str::<Locale>("\"en US\"").is_err());
        assert!(serde_json::from_str::<Locale>("\"\"").is_err());
    }

    #[test]
    fn encoding_label_normalizes() {
        assert_eq!(EncodingLabel::new("  GBK ").as_str(), "gbk");
        assert!(EncodingLabel::new("").is_utf8());
        assert!(!EncodingLabel::new("gbk").is_utf8());
        let l: EncodingLabel = serde_json::from_str("\" Big5 \"").unwrap();
        assert_eq!(l.as_str(), "big5");
    }

    #[test]
    fn profile_kind_and_auth_default() {
        assert_eq!(AuthMethod::default(), AuthMethod::Password);
        let k = ProfileKind::Local(LocalProfile {
            shell_command: String::new(),
            start_dir: None,
            encoding: EncodingLabel::default(),
            locale: None,
        });
        assert_eq!(k.kind(), SessionKind::Local);
    }

    #[test]
    fn keepalive_default() {
        let k = KeepaliveConfig::default();
        assert!(k.enabled);
        assert_eq!(k.interval_secs, 30);
        assert_eq!(k.max_misses, 3);
        assert!(!k.idle_enabled);
        assert_eq!(k.idle_interval_secs, 3600);
        assert_eq!(k.idle_string, "\\r");
        assert_eq!(k.decoded_idle_string(), vec![0x0d]);
    }

    #[test]
    fn keepalive_missing_field_falls_back_to_default() {
        let json = r#"{"host":"h","port":22,"user":"u","auth":"Password","private_key_path":null,"jump":null,"proxy_command":null}"#;
        let t: SshTarget = serde_json::from_str(json).unwrap();
        assert_eq!(t.keepalive, KeepaliveConfig::default());
    }

    #[test]
    fn keepalive_decodes_escapes() {
        let mut k = KeepaliveConfig {
            idle_string: r"a\r\n\t\\b".to_owned(),
            ..Default::default()
        };
        assert_eq!(k.decoded_idle_string(), b"a\r\n\t\\b".to_vec());
        // 未知转义按字面保留，非 ASCII 按 UTF-8 输出。
        k.idle_string = r"\q中".to_owned();
        let mut expected = b"\\q".to_vec();
        expected.extend_from_slice("中".as_bytes());
        assert_eq!(k.decoded_idle_string(), expected);
    }
}
