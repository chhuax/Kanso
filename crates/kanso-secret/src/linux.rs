//! Linux 的 [`SecretStore`]：freedesktop Secret Service（GNOME Keyring、KeePassXC、KWallet 的
//! Secret Service 接口等）。
//!
//! 规范：<https://specifications.freedesktop.org/secret-service-spec/latest/>。
//!
//! - 条目存在**默认集合**（别名 `default`，通常是“登录”钥匙环）里，属性为 `service` 与 `account`，
//!   与 `secret-tool lookup service <service> account <account>` 互通，也和 macOS 钥匙串的
//!   `kSecAttrService` / `kSecAttrAccount` 一一对应。
//! - 不回退到 `session` 集合（`get_any_collection` 会这么做）：那是只存在于内存里的集合，
//!   写进去的密码在登出后会静默丢失。没有默认集合就返回 `Unavailable`。
//! - 不会自己创建集合：创建集合要弹出用户界面，且会替用户决定钥匙环的密码，交给桌面环境。
//! - 集合被锁定时会请求解锁（可能弹出系统的密码框，阻塞到用户处理完，trait 契约允许）。
//!   用户取消 → `AccessDenied`；无法连接 D-Bus 会话、没有 Secret Service 服务 → `Unavailable`。
//! - 每次操作新建一个连接（含 DH 会话协商），不缓存：凭据读写频率很低，换来的是服务重启、
//!   会话总线变化后不会留下失效的连接。
//! - 传输加密使用规范里的 `dh-ietf1024-sha256-aes128-cbc-pkcs7`，秘密不会以明文经过 D-Bus。
//! - 明文只在 `String` / `Vec<u8>` 里短暂存在；本模块不打印任何东西，错误的 `detail` 不含凭据。

use std::collections::HashMap;

use kanso_types::ProfileId;
use kanso_types::secret::SecretString;
use secret_service::EncryptionType;
use secret_service::blocking::{Collection, Item, SecretService};

use crate::{SecretError, SecretKind, SecretRef, SecretStore};

/// 条目的 content type。
const CONTENT_TYPE: &str = "text/plain";

/// 基于 Secret Service 的凭据存储。
#[derive(Clone, Debug)]
pub struct SecretServiceStore {
    service: String,
}

impl SecretServiceStore {
    /// 创建存储。`service` 写入条目的 `service` 属性，用于隔离不同 App / 发行渠道的条目，
    /// 宿主传 bundle id（如 `com.chhuax.kanso`）。**发布后不能改**，否则已保存的凭据会找不到。
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// 定位条目的查询属性。
    fn attributes<'a>(&'a self, account: &'a str) -> HashMap<&'a str, &'a str> {
        HashMap::from([("service", self.service.as_str()), ("account", account)])
    }

    /// 条目在钥匙环管理界面（Seahorse 等）里显示的名字。
    fn label(&self, account: &str) -> String {
        format!("{} {}", self.service, account)
    }

    /// 连接服务并取得已解锁的默认集合，把它交给 `f`。
    fn with_collection<T>(
        &self,
        f: impl FnOnce(&Collection<'_>) -> Result<T, secret_service::Error>,
    ) -> Result<T, SecretError> {
        let ss = SecretService::connect(EncryptionType::Dh).map_err(map_connect_error)?;
        let collection = ss.get_default_collection().map_err(map_error)?;
        collection.ensure_unlocked().map_err(map_error)?;
        f(&collection).map_err(map_error)
    }
}

impl SecretStore for SecretServiceStore {
    fn get(&self, r: &SecretRef) -> Result<Option<SecretString>, SecretError> {
        let account = r.account();
        self.with_collection(|c| {
            let items = c.search_items(self.attributes(&account))?;
            match items.first() {
                None => Ok(None),
                Some(item) => read_item(item).map(Some),
            }
        })
        .and_then(|bytes| bytes.map(decode).transpose())
    }

    fn set(&self, r: &SecretRef, value: &SecretString) -> Result<(), SecretError> {
        let account = r.account();
        self.with_collection(|c| {
            // replace = true：同属性的旧条目被原子地覆盖，不会出现“先删后加失败丢旧凭据”。
            c.create_item(
                &self.label(&account),
                self.attributes(&account),
                value.expose().as_bytes(),
                true,
                CONTENT_TYPE,
            )
            .map(|_| ())
        })
    }

    fn delete(&self, r: &SecretRef) -> Result<(), SecretError> {
        let account = r.account();
        self.with_collection(|c| {
            for item in c.search_items(self.attributes(&account))? {
                item.delete()?;
            }
            Ok(())
        })
    }

    fn delete_all_for(&self, profile: ProfileId) -> Result<(), SecretError> {
        for kind in [SecretKind::Password, SecretKind::KeyPassphrase] {
            self.delete(&SecretRef { profile, kind })?;
        }
        Ok(())
    }
}

/// 取出条目的明文字节（先确保条目已解锁）。
fn read_item(item: &Item<'_>) -> Result<Vec<u8>, secret_service::Error> {
    item.ensure_unlocked()?;
    item.get_secret()
}

/// 字节转凭据。非法 UTF-8 说明条目不是我们写的（或已损坏），不能静默丢字节。
fn decode(bytes: Vec<u8>) -> Result<SecretString, SecretError> {
    match String::from_utf8(bytes) {
        Ok(text) => Ok(SecretString::new(text)),
        Err(e) => {
            let mut bytes = e.into_bytes();
            wipe(&mut bytes);
            Err(SecretError::Other {
                detail: "secret service item is not valid UTF-8".to_owned(),
            })
        }
    }
}

/// 尽力清零。`black_box` 防止编译器把对即将释放内存的写入当作死代码删掉。
fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(&*bytes);
}

/// 连接阶段的任何失败都意味着“用不了 Secret Service”（没有会话总线、没有服务、
/// 会话协商失败），统一报 `Unavailable`；只有本地加密实现的错误算 `Other`。
fn map_connect_error(e: secret_service::Error) -> SecretError {
    match e {
        secret_service::Error::Crypto(_) => map_error(e),
        secret_service::Error::Prompt => SecretError::AccessDenied,
        other => SecretError::Unavailable {
            detail: format!("secret service connect failed: {other}"),
        },
    }
}

/// `secret_service::Error` 到 [`SecretError`] 的映射（与 macOS 约定一致）：
/// - 用户关掉授权对话框 → `AccessDenied`（重试没有意义，core 据此提示用户）；
/// - 钥匙环仍被锁定、服务不可用、没有默认集合、授权中途连接断开 → `Unavailable`（环境问题，稍后可能恢复）；
/// - D-Bus 层的权限拒绝 → `AccessDenied`；其余 → `Other`，只带英文描述。
fn map_error(e: secret_service::Error) -> SecretError {
    use secret_service::Error as E;
    match &e {
        E::Prompt => SecretError::AccessDenied,
        E::Locked => SecretError::Unavailable {
            detail: "secret service collection is locked".to_owned(),
        },
        E::NoResult => SecretError::Unavailable {
            detail: "secret service has no default collection".to_owned(),
        },
        E::PromptDisconnected | E::Unavailable => SecretError::Unavailable {
            detail: e.to_string(),
        },
        E::ZbusFdo(zbus::fdo::Error::AccessDenied(_)) => SecretError::AccessDenied,
        E::ZbusFdo(zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NoReply(_)) => {
            SecretError::Unavailable {
                detail: e.to_string(),
            }
        }
        _ => SecretError::Other {
            detail: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_use_service_and_account() {
        let s = SecretServiceStore::new("com.chhuax.kanso");
        let a = s.attributes("profile/x/password");
        assert_eq!(a.len(), 2);
        assert_eq!(a["service"], "com.chhuax.kanso");
        assert_eq!(a["account"], "profile/x/password");
    }

    #[test]
    fn label_contains_service_and_account() {
        let s = SecretServiceStore::new("svc");
        assert_eq!(s.label("acc"), "svc acc");
    }

    #[test]
    fn decode_rejects_invalid_utf8() {
        assert_eq!(decode(b"ok".to_vec()).unwrap().expose(), "ok");
        assert!(matches!(
            decode(vec![0xff, 0xfe]),
            Err(SecretError::Other { .. })
        ));
        // 空密码合法。
        assert_eq!(decode(Vec::new()).unwrap().expose(), "");
    }

    #[test]
    fn error_mapping() {
        use secret_service::Error as E;
        assert_eq!(map_error(E::Prompt), SecretError::AccessDenied);
        assert!(matches!(
            map_error(E::Locked),
            SecretError::Unavailable { .. }
        ));
        assert!(matches!(
            map_error(E::NoResult),
            SecretError::Unavailable { .. }
        ));
        assert!(matches!(
            map_error(E::Unavailable),
            SecretError::Unavailable { .. }
        ));
        assert!(matches!(
            map_error(E::PromptDisconnected),
            SecretError::Unavailable { .. }
        ));
        assert!(matches!(
            map_error(E::Crypto("x")),
            SecretError::Other { .. }
        ));
        assert_eq!(
            map_error(E::ZbusFdo(zbus::fdo::Error::AccessDenied("no".into()))),
            SecretError::AccessDenied
        );
        assert!(matches!(
            map_connect_error(E::Locked),
            SecretError::Unavailable { .. }
        ));
    }

    #[test]
    fn errors_never_contain_secret() {
        // detail 只来自库的错误描述；这里确认格式化不会带出任何调用方数据。
        let e = map_error(secret_service::Error::Locked);
        assert!(!e.to_string().contains("hunter2"));
    }
}
