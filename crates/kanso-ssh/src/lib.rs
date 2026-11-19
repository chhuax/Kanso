//! SSH 连接、算法表、主机密钥、认证、跳板链、共享 transport、SFTP 与流水线传输。
//!
//! 模块：
//! - [`algorithms`]：算法表与旧算法判定（ssh.md §2）；
//! - [`hostkey`] / [`known_hosts`]：主机密钥策略与 `known_hosts` 读写（§3）；
//! - [`auth`] / [`interaction`]：认证与向用户提问的接口（§4）；
//! - [`connection`] / [`channel`]：连接、共享与 shell / exec 通道（§2.4、§6）；
//! - [`chain`] / [`proxy`]：跳板链与 ProxyCommand（§5）；
//! - [`cwd`]：远端工作目录查询（§10）；
//! - [`sftp`]：SFTP 客户端与流水线传输引擎（§7–§9）；
//! - [`error`]：结构化错误与到 `kanso-types` 消息的映射（§11）。
//!
//! 本库是 async（tokio），不创建运行时。

use std::fmt;
use std::sync::Mutex;

pub mod algorithms;
pub mod auth;
pub mod chain;
pub mod channel;
pub mod connection;
pub mod cwd;
pub mod error;
pub mod hostkey;
pub mod interaction;
pub mod known_hosts;
pub mod proxy;
pub mod sftp;

pub use algorithms::{NegotiatedAlgorithms, is_legacy};
pub use auth::{AuthMethod, MAX_KBD_ROUNDS};
pub use chain::{ChainConnected, HopParams, connect_chain};
pub use channel::{
    ChannelChunk, ChannelReader, ChannelWriter, ExecChannel, PtyRequest, ShellChannel,
};
pub use connection::{
    CloseReason, ConnectParams, Connected, Connection, KEEPALIVE_INTERVAL, KEEPALIVE_MAX,
    RememberedCredential, connect, connect_over,
};
pub use cwd::{CWD_TIMEOUT, CwdError, query_working_directory};
pub use error::{HostKeyChange, SshError};
pub use hostkey::{RevokedKeys, accept_changed_key, fingerprint_sha256};
pub use interaction::{Interaction, KbdRound, PROMPT_TIMEOUT};
pub use known_hosts::KnownHostsFile;
pub use sftp::SftpClient;

// 进程内 russh server 驱动的集成测试（ssh.md §12）。
#[cfg(test)]
mod itests;

/// 进程内 russh 测试服务器。本库自己的集成测试与启用 `test-server` feature 的下游测试共用同一份实现，
/// 避免在 core / session 里再复制一份服务器脚本。不属于公开 API，产品构建不会编译它。
#[cfg(any(test, feature = "test-server"))]
#[doc(hidden)]
#[path = "itests/server.rs"]
#[allow(clippy::unwrap_used, clippy::missing_panics_doc, missing_docs)]
pub mod test_server;

/// 进程内 SFTP v3 测试服务器（映射到临时目录），由 [`test_server`] 的 `sftp` 子系统使用。
#[cfg(any(test, feature = "test-server"))]
#[doc(hidden)]
#[path = "itests/sftp_server.rs"]
#[allow(clippy::unwrap_used, clippy::missing_panics_doc, missing_docs)]
pub mod test_sftp_server;

/// 主机密钥在存储文件中的位置，用于在“密钥已变更”提示里告诉用户去哪里看。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyLocation {
    /// 文件路径。
    pub path: String,
    /// 行号，从 1 开始。**注释行和空行也计数**，与用户用编辑器打开文件看到的一致
    /// （ssh.md §3）。
    pub line: u32,
}

/// 已记录的一条主机密钥。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownHostKey {
    /// 密钥算法名，例如 `ssh-ed25519`。
    pub key_type: String,
    /// SSH wire 格式的公钥 blob。
    pub key: Vec<u8>,
    /// 记录所在位置；没有文件概念的实现（如数据库、内存）为 `None`。
    pub location: Option<HostKeyLocation>,
}

/// 主机密钥存储错误。`detail` 为英文技术信息，只进日志。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostKeyStoreError {
    /// 读写文件失败。
    Io {
        /// 出错的文件路径。
        path: String,
        /// 技术细节。
        detail: String,
    },
    /// 其他错误。
    Other {
        /// 技术细节。
        detail: String,
    },
}

impl fmt::Display for HostKeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostKeyStoreError::Io { path, detail } => {
                write!(f, "host key store io error at {path}: {detail}")
            }
            HostKeyStoreError::Other { detail } => write!(f, "host key store error: {detail}"),
        }
    }
}

impl std::error::Error for HostKeyStoreError {}

/// 主机密钥存储。
///
/// - 实现者：Direct 版是 Rust 内置的 `~/.ssh/known_hosts` 读写（T6）；MAS 版由 core 用数据库实现。
/// - 调用者：kanso-ssh 的握手流程，在 `spawn_blocking` 的工作线程里调用，可以做文件 IO。
/// - 职责边界：**策略判断（accept-new、同算法密钥变更拒绝）由 kanso-ssh 自己做，
///   store 只负责存取**，不要在实现里加策略。
/// - 出错处理：`lookup` 出错时调用方让本次连接失败并提示；`add` / `replace_all` 出错时
///   同样中止连接，不能在没有记录成功的情况下默默放行。
pub trait HostKeyStore: Send + Sync + 'static {
    /// 返回该主机已记录的全部密钥（可能有多种算法）。
    ///
    /// 存储不存在或无法解析时返回空列表而不是错误，调用方当作未知主机（ssh.md §3）。
    /// 非 22 端口如何写成 `[host]:port` 是实现自己的事。
    fn lookup(&self, host: &str, port: u16) -> Result<Vec<KnownHostKey>, HostKeyStoreError>;

    /// 追加一条记录：首次见到该主机，或见到该主机新算法的密钥时调用。
    ///
    /// 幂等：同主机、同算法、同密钥已存在时什么也不做（并发打开同一主机的多个标签时，
    /// 两次握手可能都判定为"未知主机"）。同主机同算法但密钥不同属于"密钥变更"，
    /// 调用方必须先拒绝连接、经用户确认后走 [`HostKeyStore::replace_all`]，不得调用 `add`；
    /// 实现遇到这种调用时可以返回 `Other` 错误，但不能覆盖旧记录。
    fn add(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        key: &[u8],
    ) -> Result<(), HostKeyStoreError>;

    /// 用户接受密钥变更后调用：删除该主机**所有**旧记录再写入新密钥，
    /// 效果等同 `ssh-keygen -R` 之后首次连接。
    fn replace_all(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        key: &[u8],
    ) -> Result<(), HostKeyStoreError>;
}

/// 基于内存的 [`HostKeyStore`]，仅供测试使用。记录不带 `location`。
#[derive(Default)]
pub struct MemoryHostKeyStore {
    entries: Mutex<Vec<(String, u16, KnownHostKey)>>,
}

impl MemoryHostKeyStore {
    /// 创建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, u16, KnownHostKey)>> {
        // 测试用实现：中毒后数据仍可用。
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn entry(host: &str, port: u16, key_type: &str, key: &[u8]) -> (String, u16, KnownHostKey) {
        (
            host.to_owned(),
            port,
            KnownHostKey {
                key_type: key_type.to_owned(),
                key: key.to_vec(),
                location: None,
            },
        )
    }
}

impl HostKeyStore for MemoryHostKeyStore {
    fn lookup(&self, host: &str, port: u16) -> Result<Vec<KnownHostKey>, HostKeyStoreError> {
        Ok(self
            .lock()
            .iter()
            .filter(|(h, p, _)| h == host && *p == port)
            .map(|(_, _, k)| k.clone())
            .collect())
    }

    fn add(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        key: &[u8],
    ) -> Result<(), HostKeyStoreError> {
        let mut g = self.lock();
        let existing = g
            .iter()
            .find(|(h, p, k)| h == host && *p == port && k.key_type == key_type);
        match existing {
            Some((_, _, k)) if k.key == key => Ok(()),
            Some(_) => Err(HostKeyStoreError::Other {
                detail: format!("add() called for changed {key_type} key of {host}:{port}"),
            }),
            None => {
                g.push(Self::entry(host, port, key_type, key));
                Ok(())
            }
        }
    }

    fn replace_all(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        key: &[u8],
    ) -> Result<(), HostKeyStoreError> {
        let mut g = self.lock();
        g.retain(|(h, p, _)| !(h == host && *p == port));
        g.push(Self::entry(host, port, key_type, key));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn add_two_algorithms_then_lookup() {
        let s = MemoryHostKeyStore::new();
        s.add("h", 22, "ssh-ed25519", &[1]).unwrap();
        s.add("h", 22, "ssh-rsa", &[2]).unwrap();
        s.add("other", 22, "ssh-rsa", &[3]).unwrap();
        let got = s.lookup("h", 22).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].key_type, "ssh-ed25519");
        assert_eq!(got[1].key, vec![2]);
        assert!(s.lookup("h", 2222).unwrap().is_empty());
    }

    #[test]
    fn add_is_idempotent_and_refuses_changed_key() {
        let s = MemoryHostKeyStore::new();
        s.add("h", 22, "ssh-ed25519", &[1]).unwrap();
        s.add("h", 22, "ssh-ed25519", &[1]).unwrap();
        assert_eq!(s.lookup("h", 22).unwrap().len(), 1);
        assert!(s.add("h", 22, "ssh-ed25519", &[9]).is_err());
        assert_eq!(s.lookup("h", 22).unwrap()[0].key, vec![1]);
    }

    #[test]
    fn replace_all_leaves_single_entry() {
        let s = MemoryHostKeyStore::new();
        s.add("h", 22, "ssh-ed25519", &[1]).unwrap();
        s.add("h", 22, "ssh-rsa", &[2]).unwrap();
        s.add("other", 22, "ssh-rsa", &[3]).unwrap();
        s.replace_all("h", 22, "ssh-ed25519", &[9]).unwrap();
        let got = s.lookup("h", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, vec![9]);
        assert_eq!(s.lookup("other", 22).unwrap().len(), 1);
    }

    #[test]
    fn trait_is_object_safe_and_thread_safe() {
        fn assert_send_sync<T: Send + Sync + ?Sized>(_: &T) {}
        let s: Arc<dyn HostKeyStore> = Arc::new(MemoryHostKeyStore::new());
        assert_send_sync(&s);
    }
}
