//! 凭据存储的宿主回调接口，以及把它适配成 core `SecretStore` 的 [`HostSecretStore`]。
//!
//! 明文凭据只在 `SecretString` 与 `String` 互转的那一刻出现在本模块；本模块不打任何日志。

use std::sync::Arc;

use kanso_secret::{SecretError, SecretKind, SecretRef, SecretStore};
use kanso_types::ProfileId;
use kanso_types::secret::SecretString;

/// 凭据存储的宿主实现（macOS：钥匙串）。
///
/// - **线程**：core 只在 `spawn_blocking` 的工作线程上调用，不是主线程；
/// - **阻塞**：允许阻塞（钥匙串可能弹授权框）；
/// - `account` 由 Rust 生成（`SecretRef::account()`，格式 `profile/<uuid>/password|key-passphrase`），
///   宿主不得解析，只当作不透明的键；`service` 由宿主自己决定（bundle id）；
/// - 宿主不得把 `secret` 写入任何日志。
#[uniffi::export(with_foreign)]
pub trait FfiSecretStore: Send + Sync {
    /// 读取凭据；不存在返回 `Ok(None)`。
    fn get(&self, account: String) -> Result<Option<String>, FfiSecretError>;

    /// 写入凭据，已存在则覆盖。
    fn set(&self, account: String, secret: String) -> Result<(), FfiSecretError>;

    /// 删除凭据；条目不存在视为成功。
    fn delete(&self, account: String) -> Result<(), FfiSecretError>;
}

/// 宿主凭据存储的错误，与 `kanso_secret::SecretError` 一一对应。`detail` 为英文技术信息，不得含凭据。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum FfiSecretError {
    /// 存储不可用（例如钥匙串被锁定、当前不允许交互）。
    Unavailable {
        /// 技术细节。
        detail: String,
    },
    /// 用户或系统拒绝访问。
    AccessDenied,
    /// 其他错误。
    Other {
        /// 技术细节。
        detail: String,
    },
}

impl std::fmt::Display for FfiSecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // UniFFI 要求 Error 类型实现 Debug + Display；detail 约定不含凭据。
        match self {
            FfiSecretError::Unavailable { detail } => write!(f, "unavailable: {detail}"),
            FfiSecretError::AccessDenied => f.write_str("access denied"),
            FfiSecretError::Other { detail } => write!(f, "other: {detail}"),
        }
    }
}

impl std::error::Error for FfiSecretError {}

impl From<FfiSecretError> for SecretError {
    fn from(e: FfiSecretError) -> Self {
        match e {
            FfiSecretError::Unavailable { detail } => SecretError::Unavailable { detail },
            FfiSecretError::AccessDenied => SecretError::AccessDenied,
            FfiSecretError::Other { detail } => SecretError::Other { detail },
        }
    }
}

/// 把宿主的 [`FfiSecretStore`] 适配成 core 的 [`SecretStore`]。
pub struct HostSecretStore {
    host: Arc<dyn FfiSecretStore>,
}

impl HostSecretStore {
    /// 包装宿主实现。
    pub fn new(host: Arc<dyn FfiSecretStore>) -> Self {
        Self { host }
    }
}

impl SecretStore for HostSecretStore {
    fn get(&self, r: &SecretRef) -> Result<Option<SecretString>, SecretError> {
        // 明文在此从宿主的 String 立刻装进 SecretString，之后 Debug / Display 都是脱敏的。
        Ok(self.host.get(r.account())?.map(SecretString::new))
    }

    fn set(&self, r: &SecretRef, value: &SecretString) -> Result<(), SecretError> {
        // 明文只在这次调用期间以 String 形式存在，交给宿主后即丢弃。
        Ok(self.host.set(r.account(), value.expose().to_owned())?)
    }

    fn delete(&self, r: &SecretRef) -> Result<(), SecretError> {
        Ok(self.host.delete(r.account())?)
    }

    fn delete_all_for(&self, profile: ProfileId) -> Result<(), SecretError> {
        // 两种都要尝试：第一个失败不能让另一个凭据残留；返回遇到的第一个错误。
        let mut first_err = None;
        for kind in [SecretKind::Password, SecretKind::KeyPassphrase] {
            if let Err(e) = self.delete(&SecretRef { profile, kind }) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// 记录调用的假宿主；`fail_on` 命中的 account 返回 `err`。
    #[derive(Default)]
    struct FakeHost {
        items: Mutex<HashMap<String, String>>,
        deleted: Mutex<Vec<String>>,
        fail_on: Mutex<Option<(String, FfiSecretError)>>,
    }

    impl FfiSecretStore for FakeHost {
        fn get(&self, account: String) -> Result<Option<String>, FfiSecretError> {
            self.check(&account)?;
            Ok(self.items.lock().unwrap().get(&account).cloned())
        }
        fn set(&self, account: String, secret: String) -> Result<(), FfiSecretError> {
            self.check(&account)?;
            self.items.lock().unwrap().insert(account, secret);
            Ok(())
        }
        fn delete(&self, account: String) -> Result<(), FfiSecretError> {
            self.deleted.lock().unwrap().push(account.clone());
            self.check(&account)?;
            self.items.lock().unwrap().remove(&account);
            Ok(())
        }
    }

    impl FakeHost {
        fn check(&self, account: &str) -> Result<(), FfiSecretError> {
            match &*self.fail_on.lock().unwrap() {
                Some((a, e)) if a == account || a == "*" => Err(e.clone()),
                _ => Ok(()),
            }
        }
    }

    fn pid() -> ProfileId {
        "0b6f4d2e-8c1a-4f3b-9a7d-5e2c1d3f4a6b".parse().unwrap()
    }

    fn pw() -> SecretRef {
        SecretRef {
            profile: pid(),
            kind: SecretKind::Password,
        }
    }

    fn kp() -> SecretRef {
        SecretRef {
            profile: pid(),
            kind: SecretKind::KeyPassphrase,
        }
    }

    fn setup() -> (Arc<FakeHost>, HostSecretStore) {
        let host = Arc::new(FakeHost::default());
        (host.clone(), HostSecretStore::new(host))
    }

    #[test]
    fn forwards_with_account_key() {
        let (host, s) = setup();
        assert_eq!(s.get(&pw()).unwrap(), None);
        s.set(&pw(), &SecretString::new("p1")).unwrap();
        assert_eq!(
            host.items
                .lock()
                .unwrap()
                .get("profile/0b6f4d2e-8c1a-4f3b-9a7d-5e2c1d3f4a6b/password")
                .map(String::as_str),
            Some("p1")
        );
        assert_eq!(s.get(&pw()).unwrap().unwrap().expose(), "p1");
        s.delete(&pw()).unwrap();
        assert_eq!(s.get(&pw()).unwrap(), None);
    }

    #[test]
    fn delete_all_for_deletes_both_kinds() {
        let (host, s) = setup();
        s.set(&pw(), &SecretString::new("a")).unwrap();
        s.set(&kp(), &SecretString::new("b")).unwrap();
        s.delete_all_for(pid()).unwrap();
        assert!(host.items.lock().unwrap().is_empty());
        assert_eq!(host.deleted.lock().unwrap().len(), 2);
    }

    #[test]
    fn delete_all_for_attempts_both_and_returns_first_error() {
        let (host, s) = setup();
        s.set(&kp(), &SecretString::new("b")).unwrap();
        *host.fail_on.lock().unwrap() = Some((pw().account(), FfiSecretError::AccessDenied));
        let err = s.delete_all_for(pid()).unwrap_err();
        assert_eq!(err, SecretError::AccessDenied);
        // Password 失败后 KeyPassphrase 仍被尝试并删除。
        assert_eq!(host.deleted.lock().unwrap().len(), 2);
        assert!(host.items.lock().unwrap().is_empty());
    }

    #[test]
    fn errors_are_mapped() {
        let (host, s) = setup();
        let cases = [
            (
                FfiSecretError::Unavailable { detail: "x".into() },
                SecretError::Unavailable { detail: "x".into() },
            ),
            (FfiSecretError::AccessDenied, SecretError::AccessDenied),
            (
                FfiSecretError::Other { detail: "y".into() },
                SecretError::Other { detail: "y".into() },
            ),
        ];
        for (ffi, want) in cases {
            *host.fail_on.lock().unwrap() = Some(("*".into(), ffi));
            assert_eq!(s.get(&pw()).unwrap_err(), want);
            assert_eq!(s.set(&pw(), &SecretString::new("z")).unwrap_err(), want);
            assert_eq!(s.delete(&pw()).unwrap_err(), want);
        }
    }
}
