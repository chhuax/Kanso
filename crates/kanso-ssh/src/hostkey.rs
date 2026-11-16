//! 主机密钥策略（ssh.md §3）：相当于 OpenSSH 的 `StrictHostKeyChecking=accept-new`。
//!
//! 策略判断在这里做，[`HostKeyStore`] 只负责存取（trait 文档的职责划分）：
//! - 未知主机：`add` 后继续；
//! - 已知且密钥匹配：继续；
//! - 同算法密钥变了：**拒绝连接、不打开任何通道**，返回带结构化信息的 [`SshError::HostKeyChanged`]；
//! - 新算法：当作首次见到，`add` 后继续；
//! - `lookup` / `add` 出错：连接失败，不能在没记录成功的情况下默默放行。
//!
//! 用户接受变更后由调用方调用 [`accept_changed_key`] 再重连。

use std::sync::Arc;

use kanso_types::message::Endpoint;
use russh::keys::{HashAlg, PublicKey, PublicKeyBase64};

use crate::error::{HostKeyChange, SshError};
use crate::{HostKeyStore, HostKeyStoreError, KnownHostsFile};

/// 主机密钥吊销查询（`known_hosts` 的 `@revoked` 行）。
///
/// 单独成 trait 是因为 [`HostKeyStore`] 的签名不能表达"吊销"。握手流程在策略判断之前先问它，
/// 被吊销的密钥直接拒绝且没有"接受"选项。没有吊销概念的存储（如 MAS 版数据库）不需要实现。
pub trait RevokedKeys: Send + Sync + 'static {
    /// 该主机的这把密钥（SSH wire 格式 blob）是否已被吊销。
    fn is_revoked(&self, host: &str, port: u16, key: &[u8]) -> Result<bool, HostKeyStoreError>;
}

impl RevokedKeys for KnownHostsFile {
    fn is_revoked(&self, host: &str, port: u16, key: &[u8]) -> Result<bool, HostKeyStoreError> {
        KnownHostsFile::is_revoked(self, host, port, key)
    }
}

/// 公钥的 SHA256 指纹：`SHA256:` + 无填充 base64，与 `ssh-keygen -lf` 输出一致。
pub fn fingerprint_sha256(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 密钥类型名（`ssh-ed25519`、`ssh-rsa`、`ecdsa-sha2-nistp256` 等）。
///
/// 取的是**密钥**的类型而不是签名算法：RSA 密钥无论用 rsa-sha2-256 还是 ssh-rsa 签名，
/// 记录里都写 `ssh-rsa`，否则同一把密钥会因签名哈希不同被当成"新算法"。
pub(crate) fn key_type_name(key: &PublicKey) -> String {
    key.algorithm().as_str().to_owned()
}

/// 策略判断的结果（成功的两种）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// 已记录且匹配。
    Known,
    /// 首次见到（或这是该主机新算法的密钥），已经写入存储。
    Recorded,
}

/// 对服务器出示的主机密钥执行 accept-new 策略。
///
/// 全部是阻塞 IO（文件读写），调用方必须在 `spawn_blocking` 里调用（`HostKeyStore` 的约定）。
pub(crate) fn evaluate(
    store: &dyn HostKeyStore,
    revoked: Option<&dyn RevokedKeys>,
    endpoint: &Endpoint,
    key: &PublicKey,
) -> Result<Verdict, SshError> {
    let host = endpoint.host.as_str();
    let port = endpoint.port;
    let key_type = key_type_name(key);
    let blob = key.public_key_bytes();
    let store_err = |write: bool, error: HostKeyStoreError| SshError::HostKeyStore {
        endpoint: endpoint.clone(),
        write,
        error,
    };

    // 吊销检查必须先于"匹配"判断：被吊销的密钥哪怕也记录在普通行里也不能放行。
    if let Some(r) = revoked
        && r.is_revoked(host, port, &blob)
            .map_err(|e| store_err(false, e))?
    {
        return Err(SshError::HostKeyRevoked {
            endpoint: endpoint.clone(),
            key_type,
            fingerprint: fingerprint_sha256(key),
        });
    }

    let known = store.lookup(host, port).map_err(|e| store_err(false, e))?;
    if known
        .iter()
        .any(|k| k.key_type == key_type && k.key == blob)
    {
        return Ok(Verdict::Known);
    }
    if let Some(old) = known.iter().find(|k| k.key_type == key_type) {
        return Err(SshError::HostKeyChanged {
            endpoint: endpoint.clone(),
            details: Box::new(HostKeyChange {
                key_type,
                key: blob,
                fingerprint: fingerprint_sha256(key),
                known_hosts: old.location.clone(),
            }),
        });
    }
    // 没有同算法记录：未知主机，或这是该主机新算法的密钥，两者都记录后继续（ssh.md §3）。
    store
        .add(host, port, &key_type, &blob)
        .map_err(|e| store_err(true, e))?;
    Ok(Verdict::Recorded)
}

/// 用户接受主机密钥变更后调用：删除该主机的**所有**旧记录再写入新密钥，
/// 效果等同 `ssh-keygen -R` 之后首次连接（ssh.md §3）。
///
/// 调用方用 [`SshError::HostKeyChanged`] 里的 `key_type`、`key` 传参，然后重新发起连接；
/// 本函数不做连接。内部在 `spawn_blocking` 里调用 `replace_all`（trait 要求不阻塞 async 线程）。
pub async fn accept_changed_key(
    store: Arc<dyn HostKeyStore>,
    host: &str,
    port: u16,
    key_type: &str,
    key: &[u8],
) -> Result<(), SshError> {
    let (h, t, k) = (host.to_owned(), key_type.to_owned(), key.to_vec());
    let result = tokio::task::spawn_blocking(move || store.replace_all(&h, port, &t, &k)).await;
    let endpoint = Endpoint {
        host: host.to_owned(),
        port,
        via: Vec::new(),
    };
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(SshError::HostKeyStore {
            endpoint,
            write: true,
            error,
        }),
        // 工作线程 panic：当作写入失败，而不是让 panic 传到调用方。
        Err(e) => Err(SshError::HostKeyStore {
            endpoint,
            write: true,
            error: HostKeyStoreError::Other {
                detail: format!("replace_all task failed: {e}"),
            },
        }),
    }
}
