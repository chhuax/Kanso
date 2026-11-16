//! SecretStore trait 及各平台实现。
//!
//! 本 crate 只定义凭据存取的接口与测试用的内存实现；真实的 Keychain 实现由 Swift 宿主提供
//! （T8 负责），启动时以 `Arc<dyn SecretStore>` 注入 core。

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use kanso_types::ProfileId;
use kanso_types::secret::SecretString;

/// 凭据种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// 登录密码。
    Password,
    /// 私钥口令。
    KeyPassphrase,
}

/// 一条凭据的定位：属于哪个会话配置、是哪一类凭据。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SecretRef {
    /// 所属会话配置。
    pub profile: ProfileId,
    /// 凭据种类。
    pub kind: SecretKind,
}

impl SecretRef {
    /// 钥匙串条目的 account 名，格式为 `profile/<uuid>/password` 或
    /// `profile/<uuid>/key-passphrase`（uuid 为带连字符的小写形式）。
    ///
    /// **格式一旦发布不能改**：改了之后用户已保存的密码会全部找不到。
    /// service 名由宿主决定（使用 bundle id），不在这里体现。
    pub fn account(&self) -> String {
        let kind = match self.kind {
            SecretKind::Password => "password",
            SecretKind::KeyPassphrase => "key-passphrase",
        };
        format!("profile/{}/{}", self.profile, kind)
    }
}

/// 凭据存取错误。`detail` 为英文技术信息，只进日志，不展示给用户。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretError {
    /// 凭据存储不可用（例如钥匙串被锁定或服务不可达）。
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

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::Unavailable { detail } => write!(f, "secret store unavailable: {detail}"),
            SecretError::AccessDenied => f.write_str("secret store access denied"),
            SecretError::Other { detail } => write!(f, "secret store error: {detail}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// 凭据存储。
///
/// - 实现者：Swift 宿主（Keychain，T8）；测试与 mock core 用 [`MemorySecretStore`]。
/// - 调用者：core 的连接流程与会话管理，一律在 `spawn_blocking` 的工作线程里调用，
///   不会在 UI 线程或 async 执行器线程上调用。
/// - 可以阻塞：钥匙串访问可能弹授权框，允许阻塞较长时间。
/// - 出错处理：**`get` 出错时调用方当作“没保存过凭据”**，继续流程并在连接时向用户询问
///   （store-import.md §2），不能因此让连接失败；`set` / `delete` 出错只记日志并提示用户，
///   不影响当前会话。
pub trait SecretStore: Send + Sync + 'static {
    /// 读取凭据；不存在返回 `Ok(None)`。
    fn get(&self, r: &SecretRef) -> Result<Option<SecretString>, SecretError>;

    /// 写入凭据，已存在则覆盖。
    fn set(&self, r: &SecretRef, value: &SecretString) -> Result<(), SecretError>;

    /// 删除凭据；条目不存在视为成功。
    fn delete(&self, r: &SecretRef) -> Result<(), SecretError>;

    /// 删除某个会话配置的全部凭据（密码与私钥口令）。删除会话时连带调用（store-import.md §3）。
    fn delete_all_for(&self, profile: ProfileId) -> Result<(), SecretError>;
}

/// 基于内存的 [`SecretStore`]，仅供测试和 mock core 使用，进程退出即丢失。
#[derive(Default)]
pub struct MemorySecretStore {
    items: Mutex<HashMap<SecretRef, SecretString>>,
}

impl MemorySecretStore {
    /// 创建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SecretRef, SecretString>> {
        // 测试用实现：即使其他线程 panic 导致中毒，数据本身仍然可用。
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, r: &SecretRef) -> Result<Option<SecretString>, SecretError> {
        Ok(self.lock().get(r).cloned())
    }

    fn set(&self, r: &SecretRef, value: &SecretString) -> Result<(), SecretError> {
        self.lock().insert(*r, value.clone());
        Ok(())
    }

    fn delete(&self, r: &SecretRef) -> Result<(), SecretError> {
        self.lock().remove(r);
        Ok(())
    }

    fn delete_all_for(&self, profile: ProfileId) -> Result<(), SecretError> {
        self.lock().retain(|k, _| k.profile != profile);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn pid() -> ProfileId {
        "0b6f4d2e-8c1a-4f3b-9a7d-5e2c1d3f4a6b".parse().unwrap()
    }

    fn r(profile: ProfileId, kind: SecretKind) -> SecretRef {
        SecretRef { profile, kind }
    }

    #[test]
    fn account_format_is_stable() {
        assert_eq!(
            r(pid(), SecretKind::Password).account(),
            "profile/0b6f4d2e-8c1a-4f3b-9a7d-5e2c1d3f4a6b/password"
        );
        assert_eq!(
            r(pid(), SecretKind::KeyPassphrase).account(),
            "profile/0b6f4d2e-8c1a-4f3b-9a7d-5e2c1d3f4a6b/key-passphrase"
        );
    }

    #[test]
    fn memory_set_get_delete() {
        let s = MemorySecretStore::new();
        let k = r(pid(), SecretKind::Password);
        assert_eq!(s.get(&k).unwrap(), None);
        s.set(&k, &SecretString::new("a")).unwrap();
        assert_eq!(s.get(&k).unwrap().unwrap().expose(), "a");
        s.set(&k, &SecretString::new("b")).unwrap();
        assert_eq!(s.get(&k).unwrap().unwrap().expose(), "b");
        s.delete(&k).unwrap();
        assert_eq!(s.get(&k).unwrap(), None);
        // 删除不存在的条目也成功。
        s.delete(&k).unwrap();
    }

    #[test]
    fn delete_all_for_only_removes_that_profile() {
        let s = MemorySecretStore::new();
        let a = pid();
        let b = ProfileId::new();
        for p in [a, b] {
            s.set(&r(p, SecretKind::Password), &SecretString::new("x"))
                .unwrap();
            s.set(&r(p, SecretKind::KeyPassphrase), &SecretString::new("y"))
                .unwrap();
        }
        s.delete_all_for(a).unwrap();
        assert_eq!(s.get(&r(a, SecretKind::Password)).unwrap(), None);
        assert_eq!(s.get(&r(a, SecretKind::KeyPassphrase)).unwrap(), None);
        assert!(s.get(&r(b, SecretKind::Password)).unwrap().is_some());
        assert!(s.get(&r(b, SecretKind::KeyPassphrase)).unwrap().is_some());
    }

    #[test]
    fn trait_is_object_safe_and_thread_safe() {
        fn assert_send_sync<T: Send + Sync + ?Sized>(_: &T) {}
        let s: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        assert_send_sync(&s);
    }
}
