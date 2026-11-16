//! 结构化错误（ssh.md §11）。
//!
//! 每个变体都带 [`Endpoint`]（单跳时 `via` 为空，结构上为跳板链保留），
//! 通过 [`SshError::to_message`] 映射到 `kanso-types` 里**已有**的消息 key；
//! 面向用户的句子由各平台按 key 本地化，这里的 `Display` 只是英文技术描述，只进日志。

use std::fmt;

use kanso_types::message::{AlgorithmCategory, Endpoint, Message, Reason};
use kanso_types::profile::AuthMethod as ProfileAuthMethod;

use crate::{HostKeyLocation, HostKeyStoreError};

/// 主机密钥变更的详情，见 [`SshError::HostKeyChanged`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyChange {
    /// 新密钥的类型，如 `ssh-ed25519`。
    pub key_type: String,
    /// 新密钥的 SSH wire 格式 blob，供 [`crate::accept_changed_key`] 使用。
    pub key: Vec<u8>,
    /// 新密钥的 SHA256 指纹，格式与 `ssh-keygen -lf` 一致（`SHA256:` + 无填充 base64）。
    pub fingerprint: String,
    /// 旧记录的位置；没有文件概念的存储为 `None`。
    pub known_hosts: Option<HostKeyLocation>,
}

/// kanso-ssh 的错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshError {
    /// TCP 连接、握手或其他传输层失败。
    ConnectFailed {
        /// 连接对象。
        endpoint: Endpoint,
        /// 原因。
        reason: Reason,
    },
    /// 算法协商失败。
    NegotiationFailed {
        /// 连接对象。
        endpoint: Endpoint,
        /// 哪一类算法没有共同项。
        category: AlgorithmCategory,
        /// 服务器提供的该类算法（已去掉 `ext-info-*`、`kex-strict-*` 伪算法）。
        server_offers: Vec<String>,
    },
    /// 同算法的主机密钥与记录不一致：连接已拒绝、没有打开任何通道。
    ///
    /// 调用方让用户确认后，用 [`crate::accept_changed_key`] 写入新密钥再重连。
    HostKeyChanged {
        /// 连接对象。
        endpoint: Endpoint,
        /// 变更详情。装箱是为了让 `Result<_, SshError>` 保持小（这是少见的冷路径）。
        details: Box<HostKeyChange>,
    },
    /// 密钥被 `@revoked` 记录吊销：直接拒绝，没有"接受"选项。
    HostKeyRevoked {
        /// 连接对象。
        endpoint: Endpoint,
        /// 密钥类型。
        key_type: String,
        /// SHA256 指纹。
        fingerprint: String,
    },
    /// 主机密钥存储读写失败（`lookup` 出错、`add` / `replace_all` 写入失败）。连接因此中止。
    HostKeyStore {
        /// 连接对象。
        endpoint: Endpoint,
        /// `true` 表示写入失败（`add` / `replace_all`），`false` 表示读取（`lookup`）失败。
        write: bool,
        /// 存储给出的错误。
        error: HostKeyStoreError,
    },
    /// 私钥文件无法读取或无法解析。
    KeyFileUnreadable {
        /// 连接对象。
        endpoint: Endpoint,
        /// 用户配置的路径（未展开 `~`）。
        path: String,
        /// 原因。
        reason: Reason,
    },
    /// 私钥口令错误，解密失败。
    KeyDecryptFailed {
        /// 连接对象。
        endpoint: Endpoint,
        /// 私钥路径。
        path: String,
    },
    /// 凭据已被服务器接受，但还需要另一个因素，且服务器没有提供本客户端支持的方式。
    AuthPartialSuccess {
        /// 连接对象。
        endpoint: Endpoint,
        /// 登录用户。
        user: String,
        /// 服务器还愿意接受的方法名。
        methods: Vec<String>,
    },
    /// 认证失败（密码方式）。
    AuthFailed {
        /// 连接对象。
        endpoint: Endpoint,
        /// 登录用户。
        user: String,
        /// 所用方式。
        method: ProfileAuthMethod,
    },
    /// 服务器拒绝了该私钥。
    AuthKeyRejected {
        /// 连接对象。
        endpoint: Endpoint,
        /// 登录用户。
        user: String,
        /// 私钥路径。
        path: String,
    },
    /// ssh-agent 里没有被服务器接受的身份。
    AuthAgentNoIdentity {
        /// 连接对象。
        endpoint: Endpoint,
        /// 登录用户。
        user: String,
    },
    /// 无法连上 ssh-agent。
    AuthAgentUnavailable {
        /// 连接对象。
        endpoint: Endpoint,
        /// 各种尝试方式的失败原因（英文技术信息）。
        details: Vec<String>,
    },
    /// 用户取消认证，含 180 秒无应答超时。
    AuthCancelled {
        /// 连接对象。
        endpoint: Endpoint,
    },
    /// keyboard-interactive 轮数超过上限。
    AuthTooManyRounds {
        /// 连接对象。
        endpoint: Endpoint,
        /// 轮数上限。
        max_rounds: u32,
    },
    /// 服务器拒绝了 keyboard-interactive 的应答。
    AuthResponseRejected {
        /// 连接对象。
        endpoint: Endpoint,
    },
    /// keepalive 无响应，判定连接已死。
    KeepaliveTimeout {
        /// 连接对象。
        endpoint: Endpoint,
    },
    /// 已建立的连接意外中断（对端关闭、网络错误、写失败）。
    ConnectionLost {
        /// 连接对象。
        endpoint: Endpoint,
    },
    /// 共享的连接已经断开，不会偷偷重连。
    SharedConnectionClosed {
        /// 连接对象。
        endpoint: Endpoint,
    },
    /// 打开通道或通道请求（PTY、shell、exec）被服务器拒绝。
    ChannelFailed {
        /// 连接对象。
        endpoint: Endpoint,
        /// 英文技术细节。
        detail: String,
    },
    /// 跳板机打不开到下一跳的 `direct-tcpip` 通道（ssh.md §5）。
    JumpUnreachable {
        /// 跳板机的显示名。
        jump: String,
        /// 下一跳（`via` 含该跳板机）。
        target: Endpoint,
        /// 原因。
        reason: Reason,
    },
    /// ProxyCommand 启动失败或在握手完成前退出（ssh.md §5）。
    ProxyCommandFailed {
        /// 连接对象（目标主机）。
        endpoint: Endpoint,
        /// 用户配置的命令模板（未做 `%` 替换）。
        command: String,
        /// 原因；`Other` 里带退出码与 stderr 摘要（英文技术信息）。
        reason: Reason,
    },
}

impl SshError {
    /// 出错的连接对象。
    pub fn endpoint(&self) -> &Endpoint {
        match self {
            SshError::ConnectFailed { endpoint, .. }
            | SshError::NegotiationFailed { endpoint, .. }
            | SshError::HostKeyChanged { endpoint, .. }
            | SshError::HostKeyRevoked { endpoint, .. }
            | SshError::HostKeyStore { endpoint, .. }
            | SshError::KeyFileUnreadable { endpoint, .. }
            | SshError::KeyDecryptFailed { endpoint, .. }
            | SshError::AuthPartialSuccess { endpoint, .. }
            | SshError::AuthFailed { endpoint, .. }
            | SshError::AuthKeyRejected { endpoint, .. }
            | SshError::AuthAgentNoIdentity { endpoint, .. }
            | SshError::AuthAgentUnavailable { endpoint, .. }
            | SshError::AuthCancelled { endpoint }
            | SshError::AuthTooManyRounds { endpoint, .. }
            | SshError::AuthResponseRejected { endpoint }
            | SshError::KeepaliveTimeout { endpoint }
            | SshError::ConnectionLost { endpoint }
            | SshError::SharedConnectionClosed { endpoint }
            | SshError::ChannelFailed { endpoint, .. }
            | SshError::ProxyCommandFailed { endpoint, .. } => endpoint,
            // 跳板不可达时对象是"下一跳"：用户要连的是它。
            SshError::JumpUnreachable { target, .. } => target,
        }
    }

    /// 映射成面向用户的消息，只使用 `kanso-types` 里已有的 key。
    ///
    /// 没有专门 key 的几种情形借用 `ssh-connect-failed` + `Reason::Other`（原因文字为英文技术信息）：
    /// 主机密钥被吊销、known_hosts 读取失败、通道请求被拒。
    pub fn to_message(&self) -> Message {
        match self {
            SshError::ConnectFailed { endpoint, reason } => Message::SshConnectFailed {
                endpoint: endpoint.clone(),
                reason: reason.clone(),
            },
            SshError::NegotiationFailed {
                endpoint,
                category,
                server_offers,
            } => Message::SshNegotiationFailed {
                endpoint: endpoint.clone(),
                category: *category,
                server_offers: server_offers.clone(),
            },
            SshError::HostKeyChanged { endpoint, details } => Message::HostKeyChanged {
                endpoint: endpoint.clone(),
                key_type: details.key_type.clone(),
                fingerprint: details.fingerprint.clone(),
            },
            SshError::HostKeyRevoked {
                endpoint,
                key_type,
                fingerprint,
            } => Message::SshConnectFailed {
                endpoint: endpoint.clone(),
                reason: Reason::Other {
                    detail: format!("host key revoked ({key_type} {fingerprint})"),
                },
            },
            SshError::HostKeyStore {
                endpoint,
                write,
                error,
            } => {
                let (path, detail) = match error {
                    HostKeyStoreError::Io { path, detail } => (path.clone(), detail.clone()),
                    HostKeyStoreError::Other { detail } => (String::new(), detail.clone()),
                };
                if *write {
                    Message::HostKeyStoreWriteFailed {
                        path,
                        reason: Reason::Other { detail },
                    }
                } else {
                    Message::SshConnectFailed {
                        endpoint: endpoint.clone(),
                        reason: Reason::Other {
                            detail: format!("reading known hosts failed: {detail}"),
                        },
                    }
                }
            }
            SshError::KeyFileUnreadable { path, reason, .. } => Message::SshKeyFileUnreadable {
                path: path.clone(),
                reason: reason.clone(),
            },
            SshError::KeyDecryptFailed { path, .. } => {
                Message::SshKeyDecryptFailed { path: path.clone() }
            }
            SshError::AuthPartialSuccess {
                endpoint,
                user,
                methods,
            } => Message::AuthPartialSuccess {
                endpoint: endpoint.clone(),
                user: user.clone(),
                methods: methods.clone(),
            },
            SshError::AuthFailed {
                endpoint,
                user,
                method,
            } => Message::AuthFailed {
                endpoint: endpoint.clone(),
                user: user.clone(),
                method: *method,
            },
            SshError::AuthKeyRejected {
                endpoint,
                user,
                path,
            } => Message::AuthKeyRejected {
                endpoint: endpoint.clone(),
                user: user.clone(),
                path: path.clone(),
            },
            SshError::AuthAgentNoIdentity { endpoint, user } => Message::AuthAgentNoIdentity {
                endpoint: endpoint.clone(),
                user: user.clone(),
            },
            SshError::AuthAgentUnavailable { details, .. } => Message::AuthAgentUnavailable {
                details: details.clone(),
            },
            SshError::AuthCancelled { endpoint } => Message::AuthCancelled {
                endpoint: endpoint.clone(),
            },
            SshError::AuthTooManyRounds {
                endpoint,
                max_rounds,
            } => Message::AuthTooManyRounds {
                endpoint: endpoint.clone(),
                max_rounds: i64::from(*max_rounds),
            },
            SshError::AuthResponseRejected { endpoint } => Message::AuthResponseRejected {
                endpoint: endpoint.clone(),
            },
            SshError::KeepaliveTimeout { endpoint } => Message::SshKeepaliveTimeout {
                endpoint: endpoint.clone(),
            },
            SshError::ConnectionLost { endpoint } => Message::SshConnectionLost {
                endpoint: endpoint.clone(),
            },
            SshError::SharedConnectionClosed { endpoint } => Message::SshSharedConnectionClosed {
                endpoint: endpoint.clone(),
            },
            SshError::ChannelFailed { endpoint, detail } => Message::SshConnectFailed {
                endpoint: endpoint.clone(),
                reason: Reason::Other {
                    detail: detail.clone(),
                },
            },
            SshError::JumpUnreachable {
                jump,
                target,
                reason,
            } => Message::JumpUnreachable {
                jump: jump.clone(),
                target: target.clone(),
                reason: reason.clone(),
            },
            SshError::ProxyCommandFailed {
                command, reason, ..
            } => Message::SshProxyCommandFailed {
                command: command.clone(),
                reason: reason.clone(),
            },
        }
    }
}

impl fmt::Display for SshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ep = self.endpoint().host_port();
        match self {
            SshError::ConnectFailed { reason, .. } => write!(f, "{ep}: connect failed: {reason:?}"),
            SshError::NegotiationFailed {
                category,
                server_offers,
                ..
            } => write!(
                f,
                "{ep}: no common {category:?} algorithm; server offers {server_offers:?}"
            ),
            SshError::HostKeyChanged { details, .. } => write!(
                f,
                "{ep}: host key changed ({} {}), recorded at {:?}",
                details.key_type, details.fingerprint, details.known_hosts
            ),
            SshError::HostKeyRevoked {
                key_type,
                fingerprint,
                ..
            } => write!(f, "{ep}: host key revoked ({key_type} {fingerprint})"),
            SshError::HostKeyStore { write, error, .. } => write!(
                f,
                "{ep}: known hosts {} failed: {error}",
                if *write { "write" } else { "read" }
            ),
            SshError::KeyFileUnreadable { path, reason, .. } => {
                write!(f, "{ep}: cannot read key {path}: {reason:?}")
            }
            SshError::KeyDecryptFailed { path, .. } => {
                write!(f, "{ep}: cannot decrypt key {path}")
            }
            SshError::AuthPartialSuccess { user, methods, .. } => write!(
                f,
                "{ep}: credentials accepted for {user}, server still requires one of {methods:?}"
            ),
            SshError::AuthFailed { user, method, .. } => {
                write!(f, "{ep}: authentication failed for {user} ({method:?})")
            }
            SshError::AuthKeyRejected { user, path, .. } => {
                write!(f, "{ep}: key {path} rejected for {user}")
            }
            SshError::AuthAgentNoIdentity { user, .. } => {
                write!(f, "{ep}: no agent identity accepted for {user}")
            }
            SshError::AuthAgentUnavailable { details, .. } => {
                write!(f, "{ep}: ssh-agent unavailable: {details:?}")
            }
            SshError::AuthCancelled { .. } => write!(f, "{ep}: authentication cancelled"),
            SshError::AuthTooManyRounds { max_rounds, .. } => {
                write!(
                    f,
                    "{ep}: more than {max_rounds} keyboard-interactive rounds"
                )
            }
            SshError::AuthResponseRejected { .. } => {
                write!(f, "{ep}: server rejected keyboard-interactive responses")
            }
            SshError::KeepaliveTimeout { .. } => write!(f, "{ep}: keepalive timeout"),
            SshError::ConnectionLost { .. } => write!(f, "{ep}: connection lost"),
            SshError::SharedConnectionClosed { .. } => {
                write!(f, "{ep}: shared connection already closed")
            }
            SshError::ChannelFailed { detail, .. } => write!(f, "{ep}: channel failed: {detail}"),
            SshError::JumpUnreachable { jump, reason, .. } => {
                write!(f, "{ep}: jump host {jump} cannot reach it: {reason:?}")
            }
            SshError::ProxyCommandFailed {
                command, reason, ..
            } => write!(f, "{ep}: proxy command {command:?} failed: {reason:?}"),
        }
    }
}

impl std::error::Error for SshError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep() -> Endpoint {
        Endpoint {
            host: "h".into(),
            port: 22,
            via: vec![],
        }
    }

    #[test]
    fn partial_success_maps_to_dedicated_message_not_auth_failed() {
        let e = SshError::AuthPartialSuccess {
            endpoint: ep(),
            user: "u".into(),
            methods: vec!["publickey".into()],
        };
        assert_eq!(e.to_message().key(), "auth-partial-success");
    }

    #[test]
    fn every_variant_has_endpoint_and_message() {
        let all = vec![
            SshError::ConnectFailed {
                endpoint: ep(),
                reason: Reason::TimedOut,
            },
            SshError::NegotiationFailed {
                endpoint: ep(),
                category: AlgorithmCategory::Kex,
                server_offers: vec!["a".into()],
            },
            SshError::HostKeyChanged {
                endpoint: ep(),
                details: Box::new(HostKeyChange {
                    key_type: "ssh-ed25519".into(),
                    key: vec![1],
                    fingerprint: "SHA256:x".into(),
                    known_hosts: None,
                }),
            },
            SshError::HostKeyRevoked {
                endpoint: ep(),
                key_type: "ssh-ed25519".into(),
                fingerprint: "SHA256:x".into(),
            },
            SshError::HostKeyStore {
                endpoint: ep(),
                write: true,
                error: HostKeyStoreError::Other { detail: "d".into() },
            },
            SshError::HostKeyStore {
                endpoint: ep(),
                write: false,
                error: HostKeyStoreError::Other { detail: "d".into() },
            },
            SshError::KeyFileUnreadable {
                endpoint: ep(),
                path: "p".into(),
                reason: Reason::NotFound,
            },
            SshError::KeyDecryptFailed {
                endpoint: ep(),
                path: "p".into(),
            },
            SshError::AuthFailed {
                endpoint: ep(),
                user: "u".into(),
                method: ProfileAuthMethod::Password,
            },
            SshError::AuthKeyRejected {
                endpoint: ep(),
                user: "u".into(),
                path: "p".into(),
            },
            SshError::AuthAgentNoIdentity {
                endpoint: ep(),
                user: "u".into(),
            },
            SshError::AuthAgentUnavailable {
                endpoint: ep(),
                details: vec![],
            },
            SshError::AuthCancelled { endpoint: ep() },
            SshError::AuthTooManyRounds {
                endpoint: ep(),
                max_rounds: 16,
            },
            SshError::AuthResponseRejected { endpoint: ep() },
            SshError::KeepaliveTimeout { endpoint: ep() },
            SshError::ConnectionLost { endpoint: ep() },
            SshError::SharedConnectionClosed { endpoint: ep() },
            SshError::ChannelFailed {
                endpoint: ep(),
                detail: "d".into(),
            },
            SshError::JumpUnreachable {
                jump: "j".into(),
                target: ep(),
                reason: Reason::ConnectionRefused,
            },
            SshError::ProxyCommandFailed {
                endpoint: ep(),
                command: "nc %h %p".into(),
                reason: Reason::Other { detail: "d".into() },
            },
        ];
        for e in all {
            assert_eq!(e.endpoint(), &ep());
            assert!(!e.to_message().key().is_empty());
            assert!(!e.to_string().is_empty());
        }
    }
}
