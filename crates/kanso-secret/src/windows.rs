//! Windows 的 [`SecretStore`]：凭据管理器（Credential Manager），直接调用 `CredReadW` /
//! `CredWriteW` / `CredDeleteW`。
//!
//! 文档：<https://learn.microsoft.com/windows/win32/api/wincred/>。
//!
//! - 条目类型 `CRED_TYPE_GENERIC`，`TargetName` = `<service>/<account>`（例如
//!   `com.chhuax.kanso/profile/<uuid>/password`），对应 macOS 的 `kSecAttrService` + `kSecAttrAccount`。
//!   `UserName` 设为 account，便于在“凭据管理器”界面里辨认。
//! - 持久性 `CRED_PERSIST_LOCAL_MACHINE`：随用户配置文件保存，但**不漫游**到域内其他机器，
//!   对应 macOS 的 `ThisDeviceOnly`（服务器密码不跨设备同步）。
//! - 值按 UTF-8 字节写入（与 Keychain 实现一致）；读到非法 UTF-8 报 `Other`，不静默丢字节。
//!   凭据管理器单条 blob 上限 2560 字节（`CRED_MAX_CREDENTIAL_BLOB_SIZE`），超出报 `Other`，
//!   不会截断。
//! - `CredWriteW` 对同一 `TargetName` + 类型是覆盖写（原子），因此 `set` 不需要先删再加。
//! - 无登录会话（例如纯 SSH 登录、服务进程，`ERROR_NO_SUCH_LOGON_SESSION`）、凭据管理服务不可用 →
//!   `Unavailable`；`ERROR_ACCESS_DENIED` / `ERROR_CANCELLED` → `AccessDenied`。
//! - 明文只出现在调用方的 `SecretString` 和这里短暂的缓冲区里；系统返回的 blob 在 `CredFree` 之前
//!   先清零。本模块不打印任何东西。

// 本模块必须调用 Win32 API，是整个 crate 里唯一放开 unsafe 的地方。
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;

use kanso_types::ProfileId;
use kanso_types::secret::SecretString;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BAD_USERNAME, ERROR_BUSY, ERROR_CANCELLED,
    ERROR_NO_SUCH_LOGON_SESSION, ERROR_NOT_FOUND, ERROR_SERVICE_NOT_ACTIVE, GetLastError,
};
use windows_sys::Win32::Security::Credentials::{
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
    CredDeleteW, CredFree, CredReadW, CredWriteW,
};

use crate::{SecretError, SecretKind, SecretRef, SecretStore};

/// 进程内写操作（写 / 删）串行化。凭据管理器自身是线程安全的，这里是防御性的：
/// 实测中曾在全新登录会话的首次并发写入里出现过一次“写入返回成功但读不到”的现象（未能复现），
/// 凭据读写频率很低，串行化没有可感知的代价。读不加锁。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn write_guard() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 基于凭据管理器的凭据存储。
#[derive(Clone, Debug)]
pub struct CredentialManagerStore {
    service: String,
}

impl CredentialManagerStore {
    /// 创建存储。`service` 作为 `TargetName` 的前缀，用于隔离不同 App / 发行渠道的条目，
    /// 宿主传 bundle id（如 `com.chhuax.kanso`）。**发布后不能改**，否则已保存的凭据会找不到。
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn target(&self, account: &str) -> String {
        format!("{}/{}", self.service, account)
    }
}

impl SecretStore for CredentialManagerStore {
    fn get(&self, r: &SecretRef) -> Result<Option<SecretString>, SecretError> {
        let target = wide(&self.target(&r.account()));
        let mut cred: *mut CREDENTIALW = ptr::null_mut();
        // SAFETY: target 以 NUL 结尾且在调用期间存活；cred 是有效的输出指针。
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
        if ok == 0 {
            // 必须在任何其他 Win32 调用之前取错误码。
            let code = unsafe { GetLastError() };
            return if code == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(map_error("get", code))
            };
        }
        // SAFETY: CredReadW 成功后 cred 指向系统分配的 CREDENTIALW，直到 CredFree 前有效。
        let blob = unsafe { take_blob(cred) };
        unsafe { CredFree(cred as *const c_void) };
        decode(blob).map(Some)
    }

    fn set(&self, r: &SecretRef, value: &SecretString) -> Result<(), SecretError> {
        let account = r.account();
        let mut target = wide(&self.target(&account));
        let mut user = wide(&account);
        let mut blob = value.expose().as_bytes().to_vec();
        let _guard = write_guard();
        let result = write(&mut target, &mut user, &mut blob);
        wipe(&mut blob);
        result
    }

    fn delete(&self, r: &SecretRef) -> Result<(), SecretError> {
        let target = wide(&self.target(&r.account()));
        let _guard = write_guard();
        // SAFETY: target 以 NUL 结尾且在调用期间存活。
        let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok != 0 {
            return Ok(());
        }
        let code = unsafe { GetLastError() };
        // 契约：条目不存在视为成功。
        if code == ERROR_NOT_FOUND {
            Ok(())
        } else {
            Err(map_error("delete", code))
        }
    }

    fn delete_all_for(&self, profile: ProfileId) -> Result<(), SecretError> {
        for kind in [SecretKind::Password, SecretKind::KeyPassphrase] {
            self.delete(&SecretRef { profile, kind })?;
        }
        Ok(())
    }
}

fn write(target: &mut [u16], user: &mut [u16], blob: &mut [u8]) -> Result<(), SecretError> {
    if blob.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(SecretError::Other {
            detail: format!(
                "credential manager: secret is {} bytes, limit is {CRED_MAX_CREDENTIAL_BLOB_SIZE}",
                blob.len()
            ),
        });
    }
    // SAFETY: CREDENTIALW 是纯 C 结构体，全零（空指针）是合法的初始值。
    let mut cred: CREDENTIALW = unsafe { std::mem::zeroed() };
    cred.Type = CRED_TYPE_GENERIC;
    cred.TargetName = target.as_mut_ptr();
    cred.UserName = user.as_mut_ptr();
    cred.Persist = CRED_PERSIST_LOCAL_MACHINE;
    cred.CredentialBlobSize = blob.len() as u32;
    // 空密码：blob 指针保持为空、长度为 0，这是合法的。
    if !blob.is_empty() {
        cred.CredentialBlob = blob.as_mut_ptr();
    }
    // SAFETY: cred 的所有指针都指向调用期间存活的缓冲区；系统会复制其内容。
    let ok = unsafe { CredWriteW(&cred, 0) };
    if ok != 0 {
        Ok(())
    } else {
        Err(map_error("set", unsafe { GetLastError() }))
    }
}

/// 复制出 blob 并就地清零系统缓冲区。
///
/// # Safety
/// `cred` 必须是 `CredReadW` 成功返回、尚未 `CredFree` 的指针。
unsafe fn take_blob(cred: *mut CREDENTIALW) -> Vec<u8> {
    // SAFETY: 见函数的安全约定。
    let c = unsafe { &*cred };
    let len = c.CredentialBlobSize as usize;
    if len == 0 || c.CredentialBlob.is_null() {
        return Vec::new();
    }
    // SAFETY: 系统保证 CredentialBlob 指向 CredentialBlobSize 个字节。
    let src = unsafe { std::slice::from_raw_parts_mut(c.CredentialBlob, len) };
    let out = src.to_vec();
    wipe(src);
    out
}

fn decode(bytes: Vec<u8>) -> Result<SecretString, SecretError> {
    match String::from_utf8(bytes) {
        Ok(text) => Ok(SecretString::new(text)),
        Err(e) => {
            let mut bytes = e.into_bytes();
            wipe(&mut bytes);
            Err(SecretError::Other {
                detail: "credential manager item is not valid UTF-8".to_owned(),
            })
        }
    }
}

/// 尽力清零。`black_box` 防止编译器把对即将释放内存的写入当作死代码删掉。
fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(&*bytes);
}

/// UTF-16、以 NUL 结尾。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Win32 错误码到 [`SecretError`] 的映射（与 macOS 约定一致）：
/// - 用户取消 / 被拒绝 → `AccessDenied`；
/// - 没有登录会话、凭据服务不可用或忙 → `Unavailable`（环境问题，换到交互式登录会话后可能恢复）；
/// - 其余 → `Other`，带英文描述与数字码，便于排查。
fn map_error(op: &str, code: u32) -> SecretError {
    let detail = format!(
        "credential manager {op} failed: {} (Win32 error {code})",
        describe(code)
    );
    match code {
        ERROR_ACCESS_DENIED | ERROR_CANCELLED => SecretError::AccessDenied,
        ERROR_NO_SUCH_LOGON_SESSION | ERROR_SERVICE_NOT_ACTIVE | ERROR_BUSY => {
            SecretError::Unavailable { detail }
        }
        // 没有可用的凭据存储（例如用户配置文件不可用）也属于环境问题。
        ERROR_BAD_USERNAME => SecretError::Unavailable { detail },
        _ => SecretError::Other { detail },
    }
}

/// 常见错误码的英文描述（避免依赖随系统语言本地化的 `FormatMessage`）；
/// 数字码始终附在后面，以数字码为准排查。
fn describe(code: u32) -> &'static str {
    match code {
        ERROR_ACCESS_DENIED => "access denied",
        ERROR_CANCELLED => "cancelled",
        ERROR_NO_SUCH_LOGON_SESSION => "no such logon session",
        ERROR_SERVICE_NOT_ACTIVE => "service not active",
        ERROR_BUSY => "busy",
        ERROR_BAD_USERNAME => "bad user name",
        _ => "unexpected error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_service_slash_account() {
        let s = CredentialManagerStore::new("com.chhuax.kanso");
        assert_eq!(
            s.target("profile/x/password"),
            "com.chhuax.kanso/profile/x/password"
        );
    }

    #[test]
    fn wide_is_nul_terminated_utf16() {
        assert_eq!(wide("a中"), vec![0x61, 0x4e2d, 0]);
        assert_eq!(wide(""), vec![0]);
    }

    #[test]
    fn decode_rejects_invalid_utf8() {
        assert_eq!(decode("密码".as_bytes().to_vec()).unwrap().expose(), "密码");
        assert!(matches!(
            decode(vec![0xff, 0xfe]),
            Err(SecretError::Other { .. })
        ));
        assert_eq!(decode(Vec::new()).unwrap().expose(), "");
    }

    #[test]
    fn error_mapping() {
        assert_eq!(
            map_error("get", ERROR_ACCESS_DENIED),
            SecretError::AccessDenied
        );
        assert_eq!(map_error("get", ERROR_CANCELLED), SecretError::AccessDenied);
        for code in [
            ERROR_NO_SUCH_LOGON_SESSION,
            ERROR_SERVICE_NOT_ACTIVE,
            ERROR_BUSY,
        ] {
            assert!(matches!(
                map_error("set", code),
                SecretError::Unavailable { .. }
            ));
        }
        match map_error("delete", 1) {
            SecretError::Other { detail } => assert!(detail.contains("Win32 error 1")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn oversize_secret_is_rejected_without_calling_system() {
        let s = CredentialManagerStore::new("kanso-test-oversize");
        let r = SecretRef {
            profile: ProfileId::new(),
            kind: SecretKind::Password,
        };
        let big = SecretString::new("x".repeat(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize + 1));
        assert!(matches!(s.set(&r, &big), Err(SecretError::Other { .. })));
    }
}
