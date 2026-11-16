//! 用户可见消息：key + 命名参数。
//!
//! 规则：core 不拼用户可见的句子，只返回 [`Message`]；各平台 UI 用 key 查本地化目录
//! （`locales/<lang>.ftl`，Fluent）。日志另走英文，不用 `Message`。
//!
//! key 命名规则：`<领域>-<名称>`，全小写 kebab-case，只含 `[a-z0-9-]`，以字母开头；
//! 参数名 snake_case。[`CATALOG`] 与 [`AUX_KEYS`] 供多语言校验工具检查各语言目录。

mod args;
mod messages;

pub use args::{
    AUX_KEYS, AlgorithmCategory, ArgKind, ByteCount, Endpoint, IntoArg, MessageArg, MessageSpec,
    ParamSpec, Reason, SelectKey, SftpOp,
};
pub use messages::{CATALOG, Message};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::AuthMethod;
    use crate::transfer::XferProtocol;
    use std::collections::{HashMap, HashSet};
    use std::io;

    fn valid_key(k: &str) -> bool {
        k.starts_with(|c: char| c.is_ascii_lowercase())
            && k.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !k.ends_with('-')
            && !k.contains("--")
    }

    fn valid_param(p: &str) -> bool {
        p.starts_with(|c: char| c.is_ascii_lowercase())
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    #[test]
    fn catalog_keys_unique_and_valid() {
        let mut seen = HashSet::new();
        for spec in CATALOG {
            assert!(valid_key(spec.key), "非法 key：{}", spec.key);
            assert!(seen.insert(spec.key), "重复 key：{}", spec.key);
        }
    }

    #[test]
    fn catalog_params_unique_snake_case_and_consistent() {
        let mut kinds: HashMap<&str, ArgKind> = HashMap::new();
        for spec in CATALOG {
            let mut names = HashSet::new();
            for p in spec.params {
                assert!(valid_param(p.name), "{} 的参数名非法：{}", spec.key, p.name);
                assert!(names.insert(p.name), "{} 参数重名：{}", spec.key, p.name);
                if let Some(prev) = kinds.insert(p.name, p.kind) {
                    assert_eq!(prev, p.kind, "参数 {} 在 {} 中类型不一致", p.name, spec.key);
                }
            }
        }
    }

    #[test]
    fn message_key_and_args_match_catalog() {
        let ep = Endpoint {
            host: "h".into(),
            port: 22,
            via: vec![],
        };
        let samples = [
            Message::ProfileHostEmpty {},
            Message::SshConnectFailed {
                endpoint: ep.clone(),
                reason: Reason::TimedOut,
            },
            Message::AuthFailed {
                endpoint: ep.clone(),
                user: "u".into(),
                method: AuthMethod::Agent,
            },
            Message::XferCompleted {
                name: "a".into(),
                size: ByteCount(5),
            },
            Message::SshConfigImportReport {
                added: 1,
                updated: 2,
                jumps_added: 3,
                multi_hop_ignored: 4,
            },
        ];
        for m in samples {
            let spec = CATALOG
                .iter()
                .find(|s| s.key == m.key())
                .unwrap_or_else(|| panic!("CATALOG 缺少 {}", m.key()));
            let args = m.args();
            let names: Vec<_> = args.iter().map(|(n, _)| *n).collect();
            let spec_names: Vec<_> = spec.params.iter().map(|p| p.name).collect();
            assert_eq!(names, spec_names);
        }
        assert_eq!(
            Message::AuthFailed {
                endpoint: ep,
                user: "u".into(),
                method: AuthMethod::Agent
            }
            .args()[2]
                .1,
            MessageArg::Select("agent")
        );
    }

    #[test]
    fn aux_and_reason_keys_unique_and_valid() {
        let mut seen = HashSet::new();
        for k in AUX_KEYS {
            assert!(valid_key(k), "非法 key：{k}");
            assert!(seen.insert(*k), "重复 key：{k}");
        }
        for k in Reason::ALL_KEYS {
            assert!(valid_key(k));
            assert!(AUX_KEYS.contains(k), "AUX_KEYS 缺少 {k}");
        }
        assert!(AUX_KEYS.contains(&"endpoint-direct") && AUX_KEYS.contains(&"endpoint-via"));
        let reasons = [
            Reason::NotFound,
            Reason::PermissionDenied,
            Reason::AlreadyExists,
            Reason::TimedOut,
            Reason::ConnectionRefused,
            Reason::ConnectionReset,
            Reason::HostUnreachable,
            Reason::NameResolution,
            Reason::Other { detail: "x".into() },
        ];
        let keys: Vec<_> = reasons.iter().map(Reason::key).collect();
        assert_eq!(keys, Reason::ALL_KEYS);
    }

    fn check_select<T: SelectKey>(all: &[T]) {
        let keys: Vec<_> = all.iter().map(|v| v.select_key()).collect();
        assert_eq!(keys, T::ALL_KEYS);
        assert!(T::ALL_KEYS.iter().all(|k| valid_key(k)));
    }

    #[test]
    fn select_keys_consistent() {
        use AlgorithmCategory::*;
        check_select(&[Kex, HostKey, Cipher, Mac, Compression]);
        check_select(&[
            AuthMethod::Password,
            AuthMethod::PrivateKey,
            AuthMethod::Agent,
        ]);
        check_select(&[
            SftpOp::List,
            SftpOp::Stat,
            SftpOp::Mkdir,
            SftpOp::Remove,
            SftpOp::Rename,
            SftpOp::CreateFile,
            SftpOp::Open,
        ]);
        check_select(&[
            XferProtocol::Sftp,
            XferProtocol::Zmodem,
            XferProtocol::Xmodem,
        ]);
    }

    #[test]
    fn reason_from_io_error() {
        let r = |k| Reason::from(&io::Error::from(k));
        assert_eq!(r(io::ErrorKind::NotFound), Reason::NotFound);
        assert_eq!(r(io::ErrorKind::PermissionDenied), Reason::PermissionDenied);
        assert_eq!(r(io::ErrorKind::AlreadyExists), Reason::AlreadyExists);
        assert_eq!(r(io::ErrorKind::TimedOut), Reason::TimedOut);
        assert_eq!(
            r(io::ErrorKind::ConnectionRefused),
            Reason::ConnectionRefused
        );
        assert_eq!(r(io::ErrorKind::ConnectionReset), Reason::ConnectionReset);
        assert_eq!(r(io::ErrorKind::HostUnreachable), Reason::HostUnreachable);
        assert!(matches!(r(io::ErrorKind::BrokenPipe), Reason::Other { .. }));
    }

    #[test]
    fn endpoint_host_port() {
        let ep = |h: &str| Endpoint {
            host: h.into(),
            port: 22,
            via: vec![],
        };
        assert_eq!(ep("10.0.0.1").host_port(), "10.0.0.1:22");
        assert_eq!(ep("example.com").host_port(), "example.com:22");
        assert_eq!(ep("::1").host_port(), "[::1]:22");
    }

    #[test]
    fn message_serde_roundtrip() {
        let m = Message::SftpOpFailed {
            op: SftpOp::Mkdir,
            path: "/x".into(),
            reason: Reason::Other { detail: "d".into() },
        };
        let back: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }
}
