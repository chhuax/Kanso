//! 系统凭据库的真实往返测试（Windows 凭据管理器 / Linux Secret Service）。
//!
//! 这些测试会读写真实的系统凭据库，所以默认 `#[ignore]`，需要显式运行：
//!
//! ```sh
//! cargo test -p kanso-secret --test system_store -- --ignored
//! ```
//!
//! 运行环境要求：Windows 需要交互式登录会话（纯 SSH 登录没有凭据会话，`CredWrite` 会返回
//! `ERROR_NO_SUCH_LOGON_SESSION`）；Linux 需要一个正在运行的 D-Bus 会话总线和已解锁的
//! Secret Service（如 gnome-keyring）。所有条目使用随机的 service 名，测试结束时清理。
#![cfg(any(windows, target_os = "linux"))]

use kanso_secret::{PlatformSecretStore, SecretError, SecretKind, SecretRef, SecretStore};
use kanso_types::ProfileId;
use kanso_types::secret::SecretString;

fn unique_service() -> String {
    format!("kanso-test-{}", ProfileId::new())
}

fn r(profile: ProfileId, kind: SecretKind) -> SecretRef {
    SecretRef { profile, kind }
}

/// 测试结束（含断言失败时）清理该 service 下本次用到的条目。
struct Cleanup<'a> {
    store: &'a PlatformSecretStore,
    profiles: Vec<ProfileId>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for p in &self.profiles {
            let _ = self.store.delete_all_for(*p);
        }
    }
}

#[test]
#[ignore = "读写真实系统凭据库"]
fn roundtrip_set_get_overwrite_delete() {
    let store = PlatformSecretStore::new(unique_service());
    let p = ProfileId::new();
    let _guard = Cleanup {
        store: &store,
        profiles: vec![p],
    };
    let k = r(p, SecretKind::Password);

    assert_eq!(store.get(&k).unwrap(), None, "新条目应不存在");
    store.set(&k, &SecretString::new("first")).unwrap();
    assert_eq!(store.get(&k).unwrap().unwrap().expose(), "first");
    store.set(&k, &SecretString::new("second")).unwrap();
    assert_eq!(store.get(&k).unwrap().unwrap().expose(), "second");
    store.delete(&k).unwrap();
    assert_eq!(store.get(&k).unwrap(), None);
    // 删除不存在的条目也成功。
    store.delete(&k).unwrap();
}

#[test]
#[ignore = "读写真实系统凭据库"]
fn values_roundtrip_exactly() {
    let store = PlatformSecretStore::new(unique_service());
    let p = ProfileId::new();
    let _guard = Cleanup {
        store: &store,
        profiles: vec![p],
    };
    let k = r(p, SecretKind::KeyPassphrase);
    let cases = [
        "密码🔑with spaces & symbols \"'\\",
        "line1\nline2\ttab",
        // 接近凭据管理器 2560 字节上限的长值。
        &"a".repeat(2500),
    ];
    for v in cases {
        store.set(&k, &SecretString::new(v)).unwrap();
        assert_eq!(store.get(&k).unwrap().unwrap().expose(), v);
    }
    // 空密码也要能存、能读回。
    store.set(&k, &SecretString::new("")).unwrap();
    assert_eq!(store.get(&k).unwrap().unwrap().expose(), "");
}

#[test]
#[ignore = "读写真实系统凭据库"]
fn kinds_and_profiles_are_independent_and_delete_all_for_is_scoped() {
    let store = PlatformSecretStore::new(unique_service());
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let _guard = Cleanup {
        store: &store,
        profiles: vec![a, b],
    };
    for p in [a, b] {
        store
            .set(&r(p, SecretKind::Password), &SecretString::new("pw"))
            .unwrap();
        store
            .set(&r(p, SecretKind::KeyPassphrase), &SecretString::new("pp"))
            .unwrap();
    }
    assert_eq!(
        store
            .get(&r(a, SecretKind::Password))
            .unwrap()
            .unwrap()
            .expose(),
        "pw"
    );
    assert_eq!(
        store
            .get(&r(a, SecretKind::KeyPassphrase))
            .unwrap()
            .unwrap()
            .expose(),
        "pp"
    );

    store.delete_all_for(a).unwrap();
    assert_eq!(store.get(&r(a, SecretKind::Password)).unwrap(), None);
    assert_eq!(store.get(&r(a, SecretKind::KeyPassphrase)).unwrap(), None);
    assert!(store.get(&r(b, SecretKind::Password)).unwrap().is_some());
    assert!(
        store
            .get(&r(b, SecretKind::KeyPassphrase))
            .unwrap()
            .is_some()
    );
}

#[test]
#[ignore = "读写真实系统凭据库"]
fn different_services_do_not_see_each_other() {
    let s1 = PlatformSecretStore::new(unique_service());
    let s2 = PlatformSecretStore::new(unique_service());
    let p = ProfileId::new();
    let _g1 = Cleanup {
        store: &s1,
        profiles: vec![p],
    };
    let _g2 = Cleanup {
        store: &s2,
        profiles: vec![p],
    };
    let k = r(p, SecretKind::Password);
    s1.set(&k, &SecretString::new("one")).unwrap();
    assert_eq!(s2.get(&k).unwrap(), None);
    s2.set(&k, &SecretString::new("two")).unwrap();
    assert_eq!(s1.get(&k).unwrap().unwrap().expose(), "one");
    assert_eq!(s2.get(&k).unwrap().unwrap().expose(), "two");
}

#[test]
#[ignore = "读写真实系统凭据库"]
fn concurrent_sets_do_not_corrupt() {
    use std::sync::Arc;
    let store = Arc::new(PlatformSecretStore::new(unique_service()));
    let p = ProfileId::new();
    let _guard = Cleanup {
        store: &store,
        profiles: vec![p],
    };
    let k = r(p, SecretKind::Password);
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.set(&k, &SecretString::new(format!("v{i}"))).unwrap())
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let got = store.get(&k).unwrap().unwrap();
    assert!(["v0", "v1", "v2", "v3"].contains(&got.expose()));
}

/// 没有 D-Bus 会话 / Secret Service 的环境下必须返回 `Unavailable`，不能 panic 也不能挂起。
/// 仅当 `KANSO_EXPECT_NO_SECRET_SERVICE=1` 时执行（由测试脚本在无 dbus 的环境里设置）。
#[cfg(target_os = "linux")]
#[test]
#[ignore = "需要在没有 D-Bus 会话的环境里运行"]
fn unavailable_without_secret_service() {
    if std::env::var("KANSO_EXPECT_NO_SECRET_SERVICE").as_deref() != Ok("1") {
        eprintln!("skipped: KANSO_EXPECT_NO_SECRET_SERVICE != 1");
        return;
    }
    let store = PlatformSecretStore::new(unique_service());
    let k = r(ProfileId::new(), SecretKind::Password);
    assert!(matches!(
        store.get(&k),
        Err(SecretError::Unavailable { .. })
    ));
    assert!(matches!(
        store.set(&k, &SecretString::new("x")),
        Err(SecretError::Unavailable { .. })
    ));
    assert!(matches!(
        store.delete(&k),
        Err(SecretError::Unavailable { .. })
    ));
}

/// 当前会话没有凭据会话（纯 SSH 登录）时应返回 `Unavailable`，而不是 panic 或其他错误。
/// 仅当 `KANSO_EXPECT_NO_LOGON_SESSION=1` 时执行。
#[cfg(windows)]
#[test]
#[ignore = "需要在没有交互式登录会话的环境里运行"]
fn unavailable_without_logon_session() {
    if std::env::var("KANSO_EXPECT_NO_LOGON_SESSION").as_deref() != Ok("1") {
        eprintln!("skipped: KANSO_EXPECT_NO_LOGON_SESSION != 1");
        return;
    }
    let store = PlatformSecretStore::new(unique_service());
    let k = r(ProfileId::new(), SecretKind::Password);
    let err = store.set(&k, &SecretString::new("x")).unwrap_err();
    assert!(matches!(err, SecretError::Unavailable { .. }), "{err:?}");
}
