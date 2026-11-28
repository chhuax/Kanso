//! macOS 的 [`SecretStore`]：系统钥匙串（Keychain），直接调用 Security.framework
//! 通用密码接口（`get_generic_password` / `set_generic_password` / `delete_generic_password`）。
//!
//! - `kSecAttrService` 为宿主 bundle id（如 `com.chhuax.kanso`），`kSecAttrAccount` 为
//!   `SecretRef::account` 或 `mcp/client-token`，与原 Swift `KeychainSecretStore` 键格式完全一致，
//!   老用户升级后已保存的 SSH 密码、私钥口令与 MCP 客户端令牌可无缝继承。
//! - 明文只在 `String` / `Vec<u8>` 里短暂存在；错误时先清零缓冲，且不在日志或 `detail` 中包含凭据。

use kanso_types::ProfileId;
use kanso_types::secret::SecretString;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use crate::{SecretError, SecretKind, SecretRef, SecretStore};

/// MCP 客户端令牌的 account，与 Swift `McpClientToken` 及 Windows / Linux 实现保持一致。
const MCP_TOKEN_ACCOUNT: &str = "mcp/client-token";

/// macOS `errSecItemNotFound`。
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
/// macOS `errSecUserCanceled`。
const ERR_SEC_USER_CANCELED: i32 = -128;
/// macOS `errSecAuthFailed`。
const ERR_SEC_AUTH_FAILED: i32 = -25293;
/// macOS `errSecInteractionNotAllowed`。
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

/// 基于 macOS Keychain 的凭据存储。
#[derive(Clone, Debug)]
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// 创建存储。`service` 写入 `kSecAttrService`，宿主传 bundle id（如 `com.chhuax.kanso`）。
    /// **发布后不能改**，否则已保存的凭据会找不到。
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// 读取独立于会话凭据的 MCP 客户端令牌。
    pub fn mcp_token(&self) -> Result<Option<SecretString>, SecretError> {
        self.get_account(MCP_TOKEN_ACCOUNT)
    }

    /// 保存 MCP 客户端令牌，重新生成时覆盖同一个条目。
    pub fn set_mcp_token(&self, value: &SecretString) -> Result<(), SecretError> {
        self.set_account(MCP_TOKEN_ACCOUNT, value)
    }

    fn get_account(&self, account: &str) -> Result<Option<SecretString>, SecretError> {
        match get_generic_password(&self.service, account) {
            Ok(bytes) => decode(bytes).map(Some),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(err) => Err(map_error("get", err)),
        }
    }

    fn set_account(&self, account: &str, value: &SecretString) -> Result<(), SecretError> {
        let bytes = value.expose().as_bytes();
        set_generic_password(&self.service, account, bytes).map_err(|err| map_error("set", err))
    }

    fn delete_account(&self, account: &str) -> Result<(), SecretError> {
        match delete_generic_password(&self.service, account) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(err) => Err(map_error("delete", err)),
        }
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, r: &SecretRef) -> Result<Option<SecretString>, SecretError> {
        self.get_account(&r.account())
    }

    fn set(&self, r: &SecretRef, value: &SecretString) -> Result<(), SecretError> {
        self.set_account(&r.account(), value)
    }

    fn delete(&self, r: &SecretRef) -> Result<(), SecretError> {
        self.delete_account(&r.account())
    }

    fn delete_all_for(&self, profile: ProfileId) -> Result<(), SecretError> {
        for kind in [SecretKind::Password, SecretKind::KeyPassphrase] {
            self.delete(&SecretRef { profile, kind })?;
        }
        Ok(())
    }
}

/// 字节转凭据；非法 UTF-8 先清零再报错。
fn decode(bytes: Vec<u8>) -> Result<SecretString, SecretError> {
    match String::from_utf8(bytes) {
        Ok(text) => Ok(SecretString::new(text)),
        Err(err) => {
            let mut bytes = err.into_bytes();
            wipe(&mut bytes);
            Err(SecretError::Other {
                detail: "keychain item is not valid UTF-8".to_owned(),
            })
        }
    }
}

fn wipe(buf: &mut [u8]) {
    for b in buf {
        // SAFETY 无需 unsafe：使用普通清零循环并在返回前写入。
        *b = 0;
    }
}

fn map_error(op: &str, err: security_framework::base::Error) -> SecretError {
    let code = err.code();
    match code {
        ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED => SecretError::AccessDenied,
        ERR_SEC_INTERACTION_NOT_ALLOWED => SecretError::Unavailable {
            detail: format!("Keychain {op} interaction not allowed ({code})"),
        },
        _ => SecretError::Other {
            detail: format!("Keychain {op} failed ({code}): {err}"),
        },
    }
}
