//! 用户可见消息的扁平化表示。
//!
//! core 有 80 多个 `Message` 变体，逐个暴露会让 Swift 绑定巨大且每加一条消息都要改 FFI。
//! 这里统一成 `key + 命名参数表`：Swift 用 key 查本地化目录（docs/i18n.md），
//! 用参数名代入 Fluent / String Catalog 变量。

use kanso_types::message::{Endpoint, Message, MessageArg};

/// 用户可见消息：本地化 key 加命名参数（参数按 core 声明顺序）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMessage {
    /// 本地化 key，形如 `profile-host-empty`。
    pub key: String,
    /// 命名参数；没有参数为空。
    pub args: Vec<FfiMessageArg>,
}

/// 一个命名参数。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiMessageArg {
    /// 参数名（snake_case），即本地化文本里的变量名。
    pub name: String,
    /// 参数值。
    pub value: FfiArgValue,
}

/// 连接对象（经过跳板机时附带跳板链）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiEndpoint {
    /// 目标主机名或地址。
    pub host: String,
    /// 目标端口。
    pub port: u16,
    /// 从外到内经过的跳板机显示名；空表示直连。
    pub via: Vec<String>,
}

impl From<&Endpoint> for FfiEndpoint {
    fn from(e: &Endpoint) -> Self {
        FfiEndpoint {
            host: e.host.clone(),
            port: e.port,
            via: e.via.clone(),
        }
    }
}

impl From<FfiEndpoint> for Endpoint {
    fn from(e: FfiEndpoint) -> Self {
        Endpoint {
            host: e.host,
            port: e.port,
            via: e.via,
        }
    }
}

/// 消息参数值，与 core 的 `MessageArg` 一一对应。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiArgValue {
    /// 普通文本（文件名、路径、主机名等数据，不本地化）。
    Str {
        /// 文本。
        value: String,
    },
    /// 计数、退出码等整数，可据此做复数选择。
    Int {
        /// 整数值。
        value: i64,
    },
    /// 字节数，由平台格式化器显示成 `1.2 MB`。
    ByteSize {
        /// 字节数。
        bytes: u64,
    },
    /// 字符串列表，由平台的列表格式化器连接。
    StrList {
        /// 列表项。
        values: Vec<String>,
    },
    /// 选择器取值（Fluent select 的分支 key）。
    Select {
        /// 分支 key。
        key: String,
    },
    /// 失败原因：先用 `key`（`reason-*`）本地化，`detail` 仅 `reason-other` 有，为英文技术细节。
    Reason {
        /// 本地化子 key。
        key: String,
        /// 英文技术细节，不本地化。
        detail: Option<String>,
    },
    /// 连接对象，用 `endpoint-direct` / `endpoint-via` 子 key 渲染。
    Endpoint {
        /// 主机名或地址。
        host: String,
        /// 端口。
        port: u16,
        /// 经过的跳板机显示名；空表示直连。
        via: Vec<String>,
    },
}

impl From<&MessageArg> for FfiArgValue {
    fn from(a: &MessageArg) -> Self {
        match a {
            MessageArg::Str(s) => FfiArgValue::Str { value: s.clone() },
            MessageArg::Int(i) => FfiArgValue::Int { value: *i },
            MessageArg::ByteSize(b) => FfiArgValue::ByteSize { bytes: *b },
            MessageArg::StrList(v) => FfiArgValue::StrList { values: v.clone() },
            MessageArg::Select(k) => FfiArgValue::Select {
                key: (*k).to_owned(),
            },
            MessageArg::Reason(r) => FfiArgValue::Reason {
                key: r.key().to_owned(),
                detail: r.detail().map(str::to_owned),
            },
            MessageArg::Endpoint(e) => FfiArgValue::Endpoint {
                host: e.host.clone(),
                port: e.port,
                via: e.via.clone(),
            },
        }
    }
}

impl From<&Message> for FfiMessage {
    fn from(m: &Message) -> Self {
        FfiMessage {
            key: m.key().to_owned(),
            args: m
                .args()
                .iter()
                .map(|(name, value)| FfiMessageArg {
                    name: (*name).to_owned(),
                    value: value.into(),
                })
                .collect(),
        }
    }
}

impl From<&FfiArgValue> for kanso_l10n::ArgValue {
    fn from(v: &FfiArgValue) -> Self {
        use kanso_l10n::ArgValue;
        match v {
            FfiArgValue::Str { value } => ArgValue::Str(value.clone()),
            FfiArgValue::Int { value } => ArgValue::Int(*value),
            FfiArgValue::ByteSize { bytes } => ArgValue::ByteSize(*bytes),
            FfiArgValue::StrList { values } => ArgValue::StrList(values.clone()),
            FfiArgValue::Select { key } => ArgValue::Select(key.clone()),
            FfiArgValue::Reason { key, detail } => ArgValue::Reason {
                key: key.clone(),
                detail: detail.clone(),
            },
            FfiArgValue::Endpoint { host, port, via } => ArgValue::Endpoint {
                host: host.clone(),
                port: *port,
                via: via.clone(),
            },
        }
    }
}

/// 把 core 消息渲染成本地化文字。
///
/// `languages` 按偏好排序（Swift 传 `Bundle.main.preferredLocalizations`），协商规则见
/// `kanso_l10n::Lang::negotiate`。`ByteSize` / `StrList` 参数建议 Swift 先用系统格式化器转成
/// `Str` 再传入（Rust 侧只有兜底格式）。永不抛错、不 panic：key 不存在时返回 key 本身。
#[uniffi::export]
pub fn render_message(message: FfiMessage, languages: Vec<String>) -> String {
    let args: Vec<(String, kanso_l10n::ArgValue)> = message
        .args
        .iter()
        .map(|a| (a.name.clone(), (&a.value).into()))
        .collect();
    let languages: Vec<&str> = languages.iter().map(String::as_str).collect();
    kanso_l10n::render_key(&message.key, &args, &languages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::message::{ByteCount, Reason, SftpOp};
    use kanso_types::profile::AuthMethod;

    fn arg<'a>(m: &'a FfiMessage, name: &str) -> &'a FfiArgValue {
        &m.args
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("缺少参数 {name}"))
            .value
    }

    #[test]
    fn no_args_message() {
        let m = FfiMessage::from(&Message::ProfileHostEmpty {});
        assert_eq!(m.key, "profile-host-empty");
        assert!(m.args.is_empty());
    }

    #[test]
    fn str_select_and_reason_args() {
        let m = FfiMessage::from(&Message::SftpOpFailed {
            op: SftpOp::CreateFile,
            path: "/x y".into(),
            reason: Reason::Other {
                detail: "boom".into(),
            },
        });
        assert_eq!(m.key, "sftp-op-failed");
        // 参数顺序必须与 core 声明顺序一致。
        let names: Vec<_> = m.args.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["op", "path", "reason"]);
        assert_eq!(
            arg(&m, "op"),
            &FfiArgValue::Select {
                key: "create-file".into()
            }
        );
        assert_eq!(
            arg(&m, "path"),
            &FfiArgValue::Str {
                value: "/x y".into()
            }
        );
        assert_eq!(
            arg(&m, "reason"),
            &FfiArgValue::Reason {
                key: "reason-other".into(),
                detail: Some("boom".into())
            }
        );
        // 非 Other 的原因没有 detail。
        let m = FfiMessage::from(&Message::SftpOpFailed {
            op: SftpOp::List,
            path: String::new(),
            reason: Reason::TimedOut,
        });
        assert_eq!(
            arg(&m, "reason"),
            &FfiArgValue::Reason {
                key: "reason-timed-out".into(),
                detail: None
            }
        );
    }

    #[test]
    fn int_arg() {
        let m = FfiMessage::from(&Message::SessionExitedWithCode { code: 137 });
        assert_eq!(arg(&m, "code"), &FfiArgValue::Int { value: 137 });
    }

    #[test]
    fn byte_size_arg() {
        let m = FfiMessage::from(&Message::XferCompleted {
            name: "a.bin".into(),
            size: ByteCount(1536),
        });
        assert_eq!(arg(&m, "size"), &FfiArgValue::ByteSize { bytes: 1536 });
    }

    #[test]
    fn str_list_arg() {
        let m = FfiMessage::from(&Message::AuthAgentUnavailable {
            details: vec!["a".into(), "b".into()],
        });
        assert_eq!(
            arg(&m, "details"),
            &FfiArgValue::StrList {
                values: vec!["a".into(), "b".into()]
            }
        );
    }

    #[test]
    fn endpoint_arg_and_select() {
        let m = FfiMessage::from(&Message::AuthFailed {
            endpoint: Endpoint {
                host: "h".into(),
                port: 2222,
                via: vec!["jump".into()],
            },
            user: "u".into(),
            method: AuthMethod::PrivateKey,
        });
        assert_eq!(
            arg(&m, "endpoint"),
            &FfiArgValue::Endpoint {
                host: "h".into(),
                port: 2222,
                via: vec!["jump".into()]
            }
        );
        assert_eq!(
            arg(&m, "method"),
            &FfiArgValue::Select {
                key: "private-key".into()
            }
        );
    }

    #[test]
    fn render_message_round_trip_matches_direct_render() {
        let msg = Message::XferCompleted {
            name: "a.bin".into(),
            size: ByteCount(1536),
        };
        let via_ffi = render_message(FfiMessage::from(&msg), vec!["en".into()]);
        assert_eq!(via_ffi, kanso_l10n::render(&msg, &["en"]));
        let zh = render_message(FfiMessage::from(&msg), vec!["zh-Hans".into()]);
        assert_ne!(zh, via_ffi);
    }
}
