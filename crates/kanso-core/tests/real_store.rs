//! 真实 core 的存储相关集成测试（T8）：真实 SQLite（临时目录）+ `MemorySecretStore`。
//!
//! 覆盖 store-import.md §10 中与存储相关的验收用例。不碰用户真实环境：数据目录是临时目录，
//! 凭据在内存里，导入导出文件都写在临时目录下。

use std::sync::Arc;

// 只有本地 shell 相关的测试（仅 Unix）用到。
#[cfg(unix)]
use kanso_core::OpenTarget;
use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, Platform, SecretKind, SecretStore,
};
use kanso_secret::{MemorySecretStore, SecretError, SecretRef};
use kanso_types::geometry::Rgb;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, LineEnding, Profile, ProfileKind, Snippet, SnippetScope,
    SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
#[cfg(unix)]
use kanso_types::{GridSize, profile::LocalProfile};
use kanso_types::{GroupId, Message, ProfileId, SnippetId};

#[cfg(unix)]
const GRID: GridSize = GridSize { cols: 80, rows: 24 };

struct Fx {
    tmp: tempfile::TempDir,
    core: Core,
    secrets: Arc<MemorySecretStore>,
    rt: tokio::runtime::Runtime,
}

fn config(tmp: &tempfile::TempDir) -> CoreConfig {
    let mut c = CoreConfig::new(tmp.path().join("data"), "9.9.9-test", "test.kanso");
    c.default_shell = Some("/bin/sh -i".to_owned());
    c
}

fn core_with(tmp: &tempfile::TempDir, secrets: Arc<dyn SecretStore>) -> Result<Core, CoreError> {
    let sink = Arc::new(CollectingEventSink::new());
    let mut platform = Platform::for_tests(sink);
    platform.secrets = secrets;
    // 本地会话的子进程环境显式给定，不读开发者真实的 HOME / 配置。
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    Core::new_with_base_env(
        config(tmp),
        platform,
        vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("HOME".into(), home.to_string_lossy().into_owned()),
        ],
    )
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let core = core_with(&tmp, secrets.clone()).unwrap();
    Fx {
        tmp,
        core,
        secrets,
        rt: tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap(),
    }
}

fn ssh_profile(name: &str, group: Option<GroupId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: Some(Rgb {
            r: 10,
            g: 20,
            b: 30,
        }),
        group,
        kind: ProfileKind::Ssh(SshProfile {
            target: SshTarget {
                host: "example.com".into(),
                port: 22,
                user: "root".into(),
                auth: AuthMethod::Password,
                private_key_path: None,
                jump: None,
                proxy_command: None,
                keepalive: kanso_types::profile::KeepaliveConfig::default(),
            },
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

#[cfg(unix)]
fn local_profile(name: &str, shell: &str, start_dir: Option<String>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group: None,
        kind: ProfileKind::Local(LocalProfile {
            shell_command: shell.into(),
            start_dir,
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

fn snippet(name: &str, scope: SnippetScope) -> Snippet {
    Snippet {
        id: SnippetId::new(),
        name: name.into(),
        text: "echo hi".into(),
        line_ending: LineEnding::Lf,
        scope,
    }
}

fn secret_ref(profile: ProfileId, kind: SecretKind) -> SecretRef {
    SecretRef { profile, kind }
}

fn has_secret(fx: &Fx, profile: ProfileId, kind: SecretKind) -> bool {
    fx.secrets
        .get(&secret_ref(profile, kind))
        .unwrap()
        .is_some()
}

fn set_secret(fx: &Fx, p: ProfileId, kind: SecretKind, v: &str) {
    fx.rt
        .block_on(fx.core.set_credential(p, kind, SecretString::new(v)))
        .unwrap();
}

#[test]
fn save_and_read_back_profile_group_snippet() {
    let fx = fx();
    let g = Group {
        id: GroupId::new(),
        name: "prod".into(),
    };
    fx.core.save_group(g.clone()).unwrap();
    let p = ssh_profile("web", Some(g.id));
    fx.core.save_profile(p.clone()).unwrap();
    let s = snippet("hello", SnippetScope::Group(g.id));
    fx.core.save_snippet(s.clone()).unwrap();
    assert_eq!(fx.core.groups(), vec![g]);
    assert_eq!(fx.core.profiles(), vec![p]);
    assert_eq!(fx.core.snippets(), vec![s.clone()]);
    fx.core.delete_snippet(s.id).unwrap();
    fx.core.delete_snippet(s.id).unwrap();
    assert!(fx.core.snippets().is_empty());
}

#[test]
fn data_persists_across_core_restart_in_db_file() {
    let tmp = tempfile::tempdir().unwrap();
    let p = ssh_profile("persist", None);
    {
        let core = core_with(&tmp, Arc::new(MemorySecretStore::new())).unwrap();
        core.save_profile(p.clone()).unwrap();
        core.shutdown();
    }
    assert!(tmp.path().join("data/kanso.db").is_file());
    let core = core_with(&tmp, Arc::new(MemorySecretStore::new())).unwrap();
    assert_eq!(core.profiles(), vec![p]);
}

#[test]
fn new_fails_with_internal_when_database_is_unusable() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    std::fs::write(
        tmp.path().join("data/kanso.db"),
        b"this is definitely not a sqlite database, just text padding.....",
    )
    .unwrap();
    let r = core_with(&tmp, Arc::new(MemorySecretStore::new()));
    assert!(
        matches!(r, Err(CoreError::Internal { .. })),
        "{:?}",
        r.err()
    );
}

#[test]
fn save_profile_validation_errors() {
    let fx = fx();
    let mut p = ssh_profile("a", None);
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.target.host = "  ".into();
    }
    assert_eq!(
        fx.core.save_profile(p),
        Err(CoreError::User(Message::ProfileHostEmpty {}))
    );
    let mut p = ssh_profile("b", None);
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.target.user = String::new();
    }
    assert_eq!(
        fx.core.save_profile(p),
        Err(CoreError::User(Message::ProfileUserEmpty {}))
    );
    // 跳板引用不存在：必须报错，不能悄悄改成直连。
    let mut p = ssh_profile("c", None);
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.target.jump = Some(ProfileId::new());
    }
    assert!(matches!(
        fx.core.save_profile(p),
        Err(CoreError::User(Message::JumpMissing { .. }))
    ));
    // 自己当自己的跳板：成环。
    let mut p = ssh_profile("d", None);
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.target.jump = Some(p.id);
    }
    assert!(matches!(
        fx.core.save_profile(p),
        Err(CoreError::User(Message::JumpCycle { .. }))
    ));
    // 分组不存在。
    let ghost = GroupId::new();
    assert_eq!(
        fx.core.save_profile(ssh_profile("e", Some(ghost))),
        Err(CoreError::NoSuchGroup(ghost))
    );
    assert!(fx.core.profiles().is_empty());
}

#[test]
fn delete_profile_removes_credentials_and_own_snippets_only() {
    let fx = fx();
    let p = ssh_profile("p", None);
    let q = ssh_profile("q", None);
    fx.core.save_profile(p.clone()).unwrap();
    fx.core.save_profile(q.clone()).unwrap();
    set_secret(&fx, p.id, SecretKind::Password, "pw-p");
    set_secret(&fx, p.id, SecretKind::KeyPassphrase, "pp-p");
    set_secret(&fx, q.id, SecretKind::Password, "pw-q");
    let own = snippet("own", SnippetScope::Profile(p.id));
    let other = snippet("other", SnippetScope::Profile(q.id));
    let global = snippet("global", SnippetScope::Global);
    for s in [&own, &other, &global] {
        fx.core.save_snippet(s.clone()).unwrap();
    }

    fx.rt.block_on(fx.core.delete_profile(p.id)).unwrap();

    assert!(!has_secret(&fx, p.id, SecretKind::Password));
    assert!(!has_secret(&fx, p.id, SecretKind::KeyPassphrase));
    assert!(
        has_secret(&fx, q.id, SecretKind::Password),
        "别的会话的凭据不受影响"
    );
    assert_eq!(fx.core.profiles(), vec![q]);
    let names: Vec<_> = fx.core.snippets().into_iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        vec!["other", "global"],
        "只属于它的命令消失，全局命令不受影响"
    );
    assert_eq!(
        fx.rt.block_on(fx.core.delete_profile(p.id)),
        Err(CoreError::NoSuchProfile(p.id))
    );
}

#[test]
fn group_impact_matches_delete_group_and_credentials_go() {
    let fx = fx();
    let g = Group {
        id: GroupId::new(),
        name: "g".into(),
    };
    fx.core.save_group(g.clone()).unwrap();
    let a = ssh_profile("a", Some(g.id));
    let b = ssh_profile("b", Some(g.id));
    let outside = ssh_profile("outside", None);
    for p in [&a, &b, &outside] {
        fx.core.save_profile(p.clone()).unwrap();
        set_secret(&fx, p.id, SecretKind::Password, "pw");
    }
    for s in [
        snippet("g", SnippetScope::Group(g.id)),
        snippet("a", SnippetScope::Profile(a.id)),
        snippet("b", SnippetScope::Profile(b.id)),
        snippet("out", SnippetScope::Profile(outside.id)),
        snippet("global", SnippetScope::Global),
    ] {
        fx.core.save_snippet(s).unwrap();
    }
    let impact = fx.core.group_delete_impact(g.id).unwrap();
    assert_eq!((impact.profiles, impact.snippets), (2, 3));
    let (pb, sb) = (fx.core.profiles().len(), fx.core.snippets().len());

    fx.rt.block_on(fx.core.delete_group(g.id)).unwrap();

    assert_eq!(pb - fx.core.profiles().len(), impact.profiles as usize);
    assert_eq!(sb - fx.core.snippets().len(), impact.snippets as usize);
    assert!(!has_secret(&fx, a.id, SecretKind::Password));
    assert!(!has_secret(&fx, b.id, SecretKind::Password));
    assert!(has_secret(&fx, outside.id, SecretKind::Password));
    assert!(fx.core.groups().is_empty());
    let ghost = GroupId::new();
    assert_eq!(
        fx.core.group_delete_impact(ghost),
        Err(CoreError::NoSuchGroup(ghost))
    );
    assert_eq!(
        fx.rt.block_on(fx.core.delete_group(g.id)),
        Err(CoreError::NoSuchGroup(g.id))
    );
}

/// 所有写操作都失败的凭据存储，用于验证"数据库先删、凭据失败时报错"。
struct FailingSecrets;

impl SecretStore for FailingSecrets {
    fn get(&self, _: &SecretRef) -> Result<Option<SecretString>, SecretError> {
        Ok(None)
    }
    fn set(&self, _: &SecretRef, _: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::AccessDenied)
    }
    fn delete(&self, _: &SecretRef) -> Result<(), SecretError> {
        Err(SecretError::AccessDenied)
    }
    fn delete_all_for(&self, _: ProfileId) -> Result<(), SecretError> {
        Err(SecretError::Unavailable {
            detail: "locked".into(),
        })
    }
}

#[test]
fn secret_store_failures_map_to_user_error_and_db_delete_still_applies() {
    let tmp = tempfile::tempdir().unwrap();
    let core = core_with(&tmp, Arc::new(FailingSecrets)).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let p = ssh_profile("p", None);
    core.save_profile(p.clone()).unwrap();
    let r = rt.block_on(core.set_credential(p.id, SecretKind::Password, SecretString::new("x")));
    assert!(matches!(
        r,
        Err(CoreError::User(Message::SecretStoreFailed { .. }))
    ));
    let r = rt.block_on(core.delete_profile(p.id));
    assert!(matches!(
        r,
        Err(CoreError::User(Message::SecretStoreFailed { .. }))
    ));
    assert!(core.profiles().is_empty(), "数据库删除已生效");

    let g = Group {
        id: GroupId::new(),
        name: "g".into(),
    };
    core.save_group(g.clone()).unwrap();
    core.save_profile(ssh_profile("m", Some(g.id))).unwrap();
    let r = rt.block_on(core.delete_group(g.id));
    assert!(matches!(
        r,
        Err(CoreError::User(Message::SecretStoreFailed { .. }))
    ));
    assert!(core.groups().is_empty() && core.profiles().is_empty());
}

#[test]
fn set_credential_requires_existing_profile() {
    let fx = fx();
    let ghost = ProfileId::new();
    assert_eq!(
        fx.rt.block_on(
            fx.core
                .set_credential(ghost, SecretKind::Password, SecretString::new("x"))
        ),
        Err(CoreError::NoSuchProfile(ghost))
    );
    assert!(!has_secret(&fx, ghost, SecretKind::Password));
    let p = ssh_profile("p", None);
    fx.core.save_profile(p.clone()).unwrap();
    set_secret(&fx, p.id, SecretKind::Password, "pw");
    assert!(has_secret(&fx, p.id, SecretKind::Password));
}

#[test]
fn snippet_limit_is_enforced() {
    let fx = fx();
    for i in 0..1000 {
        fx.core
            .save_snippet(snippet(&format!("s{i}"), SnippetScope::Global))
            .unwrap();
    }
    let r = fx.core.save_snippet(snippet("extra", SnippetScope::Global));
    assert_eq!(
        r,
        Err(CoreError::User(Message::SnippetLimitReached {
            limit: 1000
        }))
    );
    assert_eq!(fx.core.snippets().len(), 1000);
}

#[test]
fn clear_history_succeeds() {
    let fx = fx();
    let saved = snippet("keep", SnippetScope::Global);
    fx.core.save_snippet(saved.clone()).unwrap();
    let mut database = kanso_store::Store::open(&fx.tmp.path().join("data/kanso.db")).unwrap();
    database
        .record_history_on_host("git status", "local", 42)
        .unwrap();
    assert_eq!(fx.core.clear_history(), Ok(()));
    assert!(database.history("", 5000).unwrap().is_empty());
    assert_eq!(fx.core.snippets(), vec![saved]);
}

// ---- 设置 ----

#[test]
fn settings_roundtrip_persist_and_reset_keeps_user_data() {
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let profile = ssh_profile("keep-me", None);
    {
        let core = core_with(&tmp, secrets.clone()).unwrap();
        assert_eq!(core.setting("display"), None);
        core.set_setting("display", r#"{"v":1,"fontSize":15}"#.into())
            .unwrap();
        core.save_profile(profile.clone()).unwrap();
        // 非法 JSON 在写入时就被拒绝，不会进入导出文件。
        assert!(matches!(
            core.set_setting("display", "{not json".into()),
            Err(CoreError::Internal { .. })
        ));
        core.shutdown();
    }
    // 重启后设置还在。
    let core = core_with(&tmp, secrets).unwrap();
    assert_eq!(
        core.setting("display").as_deref(),
        Some(r#"{"v":1,"fontSize":15}"#)
    );
    core.reset_settings().unwrap();
    assert_eq!(core.setting("display"), None);
    // 恢复默认设置不动会话配置（store-import.md §9）。
    assert_eq!(core.profiles().len(), 1);
    assert_eq!(core.profiles()[0].name, "keep-me");
}

#[test]
fn settings_are_exported_in_kanso_file() {
    let fx = fx();
    fx.core
        .set_setting("appearance", r#"{"v":1,"mode":"dark"}"#.into())
        .unwrap();
    let path = fx.tmp.path().join("export");
    fx.rt
        .block_on(fx.core.export_data(path.to_str().unwrap()))
        .unwrap();
    // 导出会自动补 `.kanso` 扩展名。
    let text = std::fs::read_to_string(fx.tmp.path().join("export.kanso")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["settings"]["appearance"]["mode"], "dark");
}

#[cfg(unix)]
fn make_pipe(path: &std::path::Path) {
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
fn assert_store_readable_while_blocked(core: &Core, release_io: impl FnOnce()) {
    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = scope.spawn(move || sender.send(core.groups()).unwrap());
        let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
        release_io();
        reader.join().unwrap();
        assert!(result.is_ok(), "文件 IO 阻塞期间不能占用 store 锁");
    });
}

#[test]
#[cfg(unix)]
fn import_file_io_does_not_hold_store_lock() {
    use std::io::Write;
    let fixture = fx();
    let path = fixture.tmp.path().join("blocked.kanso");
    make_pipe(&path);
    std::thread::scope(|scope| {
        let import = scope.spawn(|| {
            fixture
                .rt
                .block_on(fixture.core.import_data(path.to_str().unwrap()))
        });
        let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert_store_readable_while_blocked(&fixture.core, move || {
            writer
                .write_all(br#"{"app":"kanso-data","formatVersion":1}"#)
                .unwrap();
        });
        assert_eq!(import.join().unwrap().unwrap().added, 0);
    });
}

#[test]
#[cfg(unix)]
fn export_file_io_does_not_hold_store_lock() {
    use std::io::Read;
    let fixture = fx();
    fixture
        .core
        .set_setting(
            "large",
            serde_json::to_string(&"x".repeat(1024 * 1024)).unwrap(),
        )
        .unwrap();
    let path = fixture.tmp.path().join("blocked.kanso");
    make_pipe(&path.with_extension("kanso.tmp"));
    std::thread::scope(|scope| {
        let export = scope.spawn(|| {
            fixture
                .rt
                .block_on(fixture.core.export_data(path.to_str().unwrap()))
        });
        let mut reader = std::fs::File::open(path.with_extension("kanso.tmp")).unwrap();
        assert_store_readable_while_blocked(&fixture.core, move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            assert!(bytes.len() > 1024 * 1024);
        });
        assert!(matches!(
            export.join().unwrap(),
            Ok(()) | Err(CoreError::User(Message::ExportWriteFailed { .. }))
        ));
    });
}

#[test]
#[cfg(unix)]
fn ssh_config_file_io_does_not_hold_store_lock() {
    use std::io::Write;
    let fixture = fx();
    let path = fixture.tmp.path().join("ssh_config");
    make_pipe(&path);
    let second_path = path.clone();
    std::thread::scope(|scope| {
        let import = scope.spawn(|| {
            fixture.rt.block_on(fixture.core.import_ssh_config(
                Some(path.to_str().unwrap()),
                &["example".to_owned()],
                None,
            ))
        });
        let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert_store_readable_while_blocked(&fixture.core, move || {
            let config = b"Host example\n HostName example.com\n User test\n";
            std::fs::remove_file(&second_path).unwrap();
            std::fs::write(&second_path, config).unwrap();
            writer.write_all(config).unwrap();
        });
        assert_eq!(import.join().unwrap().unwrap().added, 1);
    });
}

// ---- 导入导出 ----

#[test]
fn export_contains_no_credentials() {
    let fx = fx();
    let p = ssh_profile("srv", None);
    fx.core.save_profile(p.clone()).unwrap();
    set_secret(&fx, p.id, SecretKind::Password, "hunter2-PASSWORD");
    set_secret(
        &fx,
        p.id,
        SecretKind::KeyPassphrase,
        "correct-horse-PASSPHRASE",
    );
    let path = fx.tmp.path().join("export");
    fx.rt
        .block_on(fx.core.export_data(path.to_str().unwrap()))
        .unwrap();
    let text = std::fs::read_to_string(fx.tmp.path().join("export.kanso")).unwrap();
    assert!(text.contains("srv"));
    for forbidden in ["hunter2", "correct-horse", "PASSWORD", "PASSPHRASE"] {
        assert!(!text.contains(forbidden), "导出文件不应含 {forbidden}");
    }
    // 键名里也不能有凭据字段。
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    fn keys(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, x) in m {
                    out.push(k.to_lowercase());
                    keys(x, out);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
            _ => {}
        }
    }
    let mut ks = Vec::new();
    keys(&v, &mut ks);
    assert!(
        !ks.iter()
            .any(|k| k.contains("password") || k.contains("passphrase") || k.contains("secret"))
    );
}

#[test]
fn import_export_roundtrip_between_cores_keeps_local_credentials() {
    let a = fx();
    let g = Group {
        id: GroupId::new(),
        name: "g".into(),
    };
    a.core.save_group(g.clone()).unwrap();
    let p = ssh_profile("p", Some(g.id));
    a.core.save_profile(p.clone()).unwrap();
    a.core
        .save_snippet(snippet("s", SnippetScope::Profile(p.id)))
        .unwrap();
    let file = a.tmp.path().join("a.kanso");
    a.rt.block_on(a.core.export_data(file.to_str().unwrap()))
        .unwrap();

    // b 里已有同 id 的会话（名字不同）并保存了凭据：导入替换配置、保留凭据，且不删除本地数据。
    let b = fx();
    let mut old = p.clone();
    old.name = "old name".into();
    old.group = None;
    b.core.save_profile(old).unwrap();
    set_secret(&b, p.id, SecretKind::Password, "local-pw");
    let extra = ssh_profile("local only", None);
    b.core.save_profile(extra.clone()).unwrap();

    let report =
        b.rt.block_on(b.core.import_data(file.to_str().unwrap()))
            .unwrap();
    assert_eq!((report.added, report.updated), (2, 1)); // 分组 + 命令新增，会话更新
    assert!(report.skipped.is_empty());
    let names: Vec<_> = b.core.profiles().into_iter().map(|x| x.name).collect();
    assert_eq!(names, vec!["p", "local only"]);
    assert_eq!(b.core.profiles()[0].group, Some(g.id));
    assert!(has_secret(&b, p.id, SecretKind::Password));
}

#[test]
fn import_rejects_bad_files() {
    let fx = fx();
    let dir = fx.tmp.path();
    let imp = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        fx.rt.block_on(fx.core.import_data(p.to_str().unwrap()))
    };
    assert_eq!(
        imp("a.json", r#"{"app":"kanso-data","formatVersion":1}"#),
        Err(CoreError::User(Message::ImportWrongExtension {}))
    );
    assert_eq!(
        imp("b.kanso", r#"{"app":"someone-else","formatVersion":1}"#),
        Err(CoreError::User(Message::ImportNotKansoFile {}))
    );
    // 版本比当前程序认识的新：拒绝并说明，且不改动任何数据。
    assert_eq!(
        imp(
            "c.kanso",
            r#"{"app":"kanso-data","formatVersion":99,"groups":[{"id":"6f6bbd6e-6f7d-4a0b-9d65-1d2f3c0f1a11","name":"x"}]}"#
        ),
        Err(CoreError::User(Message::ImportVersionTooNew {
            version: 99,
            supported: 1
        }))
    );
    assert!(fx.core.groups().is_empty());
    assert!(matches!(
        fx.rt.block_on(
            fx.core
                .import_data(dir.join("missing.kanso").to_str().unwrap())
        ),
        Err(CoreError::User(Message::ImportReadFailed { .. }))
    ));
}

#[test]
fn import_with_unsupported_kind_imports_the_rest() {
    let fx = fx();
    let good = ProfileId::new();
    let body = serde_json::json!({
        "app": "kanso-data", "formatVersion": 1,
        "profiles": [
            {"id": ProfileId::new(), "name": "old serial", "kind": "serial", "config": {}},
            {"id": good, "name": "fine", "kind": "local", "config": {"shellCommand": "/bin/sh"}},
            {"id": ProfileId::new(), "name": "no host", "kind": "ssh", "config": {"host": "", "user": "u"}}
        ]
    });
    let path = fx.tmp.path().join("mixed.kanso");
    std::fs::write(&path, body.to_string()).unwrap();
    let report = fx
        .rt
        .block_on(fx.core.import_data(path.to_str().unwrap()))
        .unwrap();
    assert_eq!((report.added, report.updated), (1, 0));
    assert_eq!(report.skipped.len(), 2);
    assert!(report.skipped.iter().any(|m| matches!(
        m,
        Message::ImportSkippedUnsupportedKind { name, kind } if name == "old serial" && kind == "serial"
    )));
    // 校验失败（主机为空）走和手工保存相同的规则，被跳过并说明。
    assert!(
        report
            .skipped
            .iter()
            .any(|m| matches!(m, Message::ImportSkippedInvalid { name, .. } if name == "no host"))
    );
    assert_eq!(fx.core.profiles()[0].id, good);
}

// ---- 按 Profile 打开会话 ----

#[cfg(unix)]
#[test]
fn open_session_from_profile() {
    let fx = fx();
    let ghost = ProfileId::new();
    assert_eq!(
        fx.core.open_session(OpenTarget::Profile(ghost), GRID),
        Err(CoreError::NoSuchProfile(ghost))
    );
    // 跳板引用已失效（被引用的 Profile 之后被删除）的 SSH Profile：打开时报 JumpMissing，
    // 不能悄悄改成直连。不能用普通 SSH Profile 测：它会真的发起网络连接，
    // 而本测试不得连接外部主机（SSH 打开流程见 real_ssh.rs）。
    let jump = ssh_profile("jump", None);
    fx.core.save_profile(jump.clone()).unwrap();
    let mut remote = ssh_profile("remote", None);
    if let ProfileKind::Ssh(s) = &mut remote.kind {
        s.target.jump = Some(jump.id);
    }
    fx.core.save_profile(remote.clone()).unwrap();
    fx.rt.block_on(fx.core.delete_profile(jump.id)).unwrap();
    assert!(matches!(
        fx.core.open_session(OpenTarget::Profile(remote.id), GRID),
        Err(CoreError::User(Message::JumpMissing { .. }))
    ));

    let dir = fx.tmp.path().join("work");
    std::fs::create_dir_all(&dir).unwrap();
    let lp = local_profile(
        "My Shell",
        "/bin/sh -i",
        Some(dir.to_string_lossy().into_owned()),
    );
    fx.core.save_profile(lp.clone()).unwrap();
    let id = fx
        .core
        .open_session(OpenTarget::Profile(lp.id), GRID)
        .unwrap();
    let info = fx.core.session_info(id).unwrap();
    assert_eq!(info.profile, Some(lp.id));
    assert_eq!(info.name, "My Shell");
    // 复制标签：副本属于同一个 Profile。
    let dup = fx
        .core
        .open_session(OpenTarget::Duplicate(id), GRID)
        .unwrap();
    assert_eq!(fx.core.session_info(dup).unwrap().profile, Some(lp.id));
    // 默认 shell 打开的会话没有 Profile。
    let plain = fx
        .core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap();
    assert_eq!(fx.core.session_info(plain).unwrap().profile, None);
    fx.core.close_session(id);
    fx.core.close_session(dup);
    fx.core.close_session(plain);
    fx.core.shutdown();
}

#[cfg(unix)]
#[test]
fn open_session_from_profile_with_missing_shell_reports_spawn_failure() {
    let fx = fx();
    let lp = local_profile("broken", "/definitely/not/a/shell", None);
    fx.core.save_profile(lp.clone()).unwrap();
    let r = fx.core.open_session(OpenTarget::Profile(lp.id), GRID);
    assert!(matches!(
        r,
        Err(CoreError::User(Message::SessionSpawnFailed { .. }))
    ));
    fx.core.shutdown();
}
