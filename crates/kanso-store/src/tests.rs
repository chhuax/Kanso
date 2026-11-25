//! 仓库层与导入导出的单元测试。数据库一律用内存库或临时目录，不碰用户真实环境。

use kanso_types::geometry::Rgb;
use kanso_types::message::Message;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, LineEnding, LocalProfile, Locale, Profile, ProfileKind,
    Snippet, SnippetKindScope, SnippetScope, SshProfile, SshTarget,
};
use kanso_types::{GroupId, ProfileId, SnippetId};
use serde_json::{Value, json};

use crate::{ImportError, MAX_SNIPPETS, Store, StoreError};

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn target(host: &str, jump: Option<ProfileId>) -> SshTarget {
    SshTarget {
        host: host.into(),
        port: 22,
        user: "root".into(),
        auth: AuthMethod::Password,
        private_key_path: None,
        jump,
        proxy_command: None,
        keepalive: kanso_types::profile::KeepaliveConfig::default(),
    }
}

fn ssh(name: &str, group: Option<GroupId>, jump: Option<ProfileId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: Some(Rgb { r: 1, g: 2, b: 255 }),
        group,
        kind: ProfileKind::Ssh(SshProfile {
            target: target("example.com", jump),
            encoding: EncodingLabel::new("gbk"),
            locale: Locale::parse("zh_CN.UTF-8"),
        }),
    }
}

fn local(name: &str, group: Option<GroupId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group,
        kind: ProfileKind::Local(LocalProfile {
            shell_command: "/bin/zsh -l".into(),
            start_dir: Some("/tmp".into()),
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

fn group(name: &str) -> Group {
    Group {
        id: GroupId::new(),
        name: name.into(),
    }
}

fn snippet(name: &str, scope: SnippetScope) -> Snippet {
    Snippet {
        id: SnippetId::new(),
        name: name.into(),
        text: "ls -l".into(),
        line_ending: LineEnding::CrLf,
        scope,
    }
}

#[test]
fn sender_scope_union_quick_deleted_moved_and_case_insensitive_sort() {
    use kanso_types::profile::SessionKind;
    let first_group = group("first");
    let second_group = group("second");
    let mut saved = ssh("saved", Some(first_group.id), None);
    let snippets = vec![
        snippet("Zulu", SnippetScope::Global),
        snippet("alpha", SnippetScope::Kind(SnippetKindScope::Ssh)),
        snippet("Beta", SnippetScope::Group(first_group.id)),
        snippet("beta", SnippetScope::Profile(saved.id)),
        snippet("local", SnippetScope::Kind(SnippetKindScope::Local)),
        snippet("other", SnippetScope::Group(second_group.id)),
    ];
    let visible = |profiles: &[Profile], profile, kind| {
        crate::visible_snippets(snippets.clone(), profiles, profile, kind)
            .into_iter()
            .map(|item| item.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        visible(&[saved.clone()], Some(saved.id), Some(SessionKind::Ssh)),
        ["alpha", "Beta", "beta", "Zulu"]
    );
    assert_eq!(
        visible(&[saved.clone()], None, Some(SessionKind::Ssh)),
        ["alpha", "Zulu"]
    );
    assert_eq!(
        visible(&[], Some(saved.id), Some(SessionKind::Ssh)),
        ["alpha", "Zulu"]
    );
    assert_eq!(visible(&[], None, None), ["Zulu"]);
    assert_eq!(visible(&[], None, Some(SessionKind::Sftp)), ["Zulu"]);
    saved.group = Some(second_group.id);
    assert_eq!(
        visible(&[saved.clone()], Some(saved.id), Some(SessionKind::Ssh)),
        ["alpha", "beta", "other", "Zulu"]
    );
}

#[test]
fn sender_legacy_sftp_conversion_missing_scope_and_text_roundtrip() {
    let mut store = store();
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("old.kanso");
    let sftp = SnippetId::new();
    let early = SnippetId::new();
    for version in 1..=3 {
        let doc = json!({"app": "Kanso", "formatVersion": version, "snippets": [
            {"id": sftp, "name": "sftp", "text": "  a\n\nb", "lineEnding": "none", "scope": {"kind":"kind", "ref":"sftp"}},
            {"id": early, "name": "early", "text": "echo old", "lineEnding": "crlf", "format":"hex"},
            {"id": SnippetId::new(), "name":"blank", "text":" \n\t", "lineEnding":"lf"}
        ]});
        std::fs::write(&input, serde_json::to_vec(&doc).unwrap()).unwrap();
        let report = store.import_file(&input, &ok).unwrap();
        assert_eq!(
            report.converted,
            [Message::ImportSnippetConverted {
                name: "sftp".into()
            }]
        );
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            (report.added, report.updated),
            if version == 1 { (2, 0) } else { (0, 2) }
        );
    }
    assert!(
        store
            .snippets()
            .unwrap()
            .iter()
            .all(|snippet| snippet.scope == SnippetScope::Global)
    );
    let exported = store.export(&dir.path().join("roundtrip"), "1").unwrap();
    let mut restored = Store::open_in_memory().unwrap();
    let report = restored.import_file(&exported, &ok).unwrap();
    assert!(report.converted.is_empty());
    assert_eq!(restored.snippets().unwrap(), store.snippets().unwrap());
}

/// 不做任何额外校验的 validate，用于不关心校验的导入测试。
fn ok(_: &Profile, _: &[Profile]) -> Result<(), Message> {
    Ok(())
}

// ---- Profile ----

#[test]
fn profile_roundtrip_all_kinds_and_stable_order() {
    let mut s = store();
    let a = ssh("b-ssh", None, None);
    let b = local("a-local", None);
    let c = Profile {
        id: ProfileId::new(),
        name: "sftp".into(),
        color: None,
        group: None,
        kind: ProfileKind::Sftp(target("files", None)),
    };
    for p in [&a, &b, &c] {
        s.upsert_profile(p).unwrap();
    }
    // 按插入顺序（sort_key），不是按名称。
    assert_eq!(s.profiles().unwrap(), vec![a.clone(), b.clone(), c.clone()]);
    assert_eq!(s.profile(b.id).unwrap(), Some(b.clone()));
    // 更新保留位置。
    let mut a2 = a.clone();
    a2.name = "renamed".into();
    s.upsert_profile(&a2).unwrap();
    assert_eq!(s.profiles().unwrap()[0], a2);
    assert_eq!(s.profiles().unwrap().len(), 3);
}

#[test]
fn profile_config_json_shape_is_locked() {
    // schema.md 约定 config 列是 ProfileKind 的 serde 序列化；锁定形状，防止 kanso-types 改字段名后
    // 悄悄让已有数据库读不出来。
    let mut s = store();
    let p = local("x", None);
    s.upsert_profile(&p).unwrap();
    let (kind, config): (String, String) = s
        .conn
        .query_row("SELECT kind, config FROM profiles", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "local");
    let v: Value = serde_json::from_str(&config).unwrap();
    assert_eq!(
        v,
        json!({"Local": {"shell_command": "/bin/zsh -l", "start_dir": "/tmp", "encoding": "", "locale": null}})
    );
}

#[test]
fn delete_profile_tombstones_and_removes_own_snippets_only() {
    let mut s = store();
    let p = local("p", None);
    let q = local("q", None);
    s.upsert_profile(&p).unwrap();
    s.upsert_profile(&q).unwrap();
    let own = snippet("own", SnippetScope::Profile(p.id));
    let other = snippet("other", SnippetScope::Profile(q.id));
    let global = snippet("global", SnippetScope::Global);
    for sn in [&own, &other, &global] {
        s.upsert_snippet(sn).unwrap();
    }
    assert!(s.delete_profile(p.id).unwrap());
    assert!(!s.delete_profile(p.id).unwrap(), "再删一次返回 false");
    assert_eq!(s.profiles().unwrap(), vec![q.clone()]);
    let names: Vec<_> = s.snippets().unwrap().into_iter().map(|x| x.name).collect();
    assert_eq!(names, vec!["other", "global"]);
    // 墓碑仍在表里。
    let n: i64 = s
        .conn
        .query_row("SELECT COUNT(*) FROM profiles WHERE deleted = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn deleted_profile_can_be_resurrected_by_upsert() {
    let mut s = store();
    let p = local("p", None);
    s.upsert_profile(&p).unwrap();
    s.delete_profile(p.id).unwrap();
    s.upsert_profile(&p).unwrap();
    assert_eq!(s.profiles().unwrap(), vec![p]);
}

// ---- 分组与一致性 ----

#[test]
fn group_crud_and_tombstone_filter() {
    let mut s = store();
    let g = group("g");
    s.upsert_group(&g).unwrap();
    assert_eq!(s.groups().unwrap(), vec![g.clone()]);
    assert_eq!(s.group(g.id).unwrap(), Some(g.clone()));
    let mut g2 = g.clone();
    g2.name = "g2".into();
    s.upsert_group(&g2).unwrap();
    assert_eq!(s.groups().unwrap(), vec![g2]);
    assert!(s.delete_group(g.id).unwrap().is_some());
    assert!(s.groups().unwrap().is_empty());
    assert_eq!(s.delete_group(g.id).unwrap(), None);
    assert_eq!(s.delete_impact(g.id).unwrap(), None);
}

#[test]
fn dangling_group_reference_reads_as_top_level() {
    let mut s = store();
    let missing = GroupId::new();
    let p = local("p", Some(missing));
    s.upsert_profile(&p).unwrap();
    assert_eq!(s.profiles().unwrap()[0].group, None);
    // 分组被删后同样回到顶层，且没回写：分组复活后引用恢复。
    let g = group("g");
    s.upsert_group(&g).unwrap();
    let q = local("q", Some(g.id));
    s.upsert_profile(&q).unwrap();
    assert_eq!(s.profile(q.id).unwrap().unwrap().group, Some(g.id));
    s.conn.execute("UPDATE groups SET deleted = 1", []).unwrap();
    assert_eq!(s.profile(q.id).unwrap().unwrap().group, None);
    s.conn.execute("UPDATE groups SET deleted = 0", []).unwrap();
    assert_eq!(s.profile(q.id).unwrap().unwrap().group, Some(g.id));
}

#[test]
fn dangling_snippet_scope_reads_as_global() {
    let mut s = store();
    let a = snippet("a", SnippetScope::Profile(ProfileId::new()));
    let b = snippet("b", SnippetScope::Group(GroupId::new()));
    let c = snippet("c", SnippetScope::Kind(SnippetKindScope::Ssh));
    for x in [&a, &b, &c] {
        s.upsert_snippet(x).unwrap();
    }
    let got = s.snippets().unwrap();
    assert_eq!(got[0].scope, SnippetScope::Global);
    assert_eq!(got[1].scope, SnippetScope::Global);
    assert_eq!(got[2].scope, SnippetScope::Kind(SnippetKindScope::Ssh));
}

/// 构造一个带多种命令的分组，返回 (分组, 组内 profile 数, 应被波及的命令数)。
fn populated_group(s: &mut Store) -> (Group, ProfileId) {
    let g = group("work");
    s.upsert_group(&g).unwrap();
    let in1 = local("in1", Some(g.id));
    let in2 = ssh("in2", Some(g.id), None);
    let out = local("out", None);
    for p in [&in1, &in2, &out] {
        s.upsert_profile(p).unwrap();
    }
    for sn in [
        snippet("scope-group", SnippetScope::Group(g.id)),
        snippet("scope-in1", SnippetScope::Profile(in1.id)),
        snippet("scope-in2", SnippetScope::Profile(in2.id)),
        snippet("scope-out", SnippetScope::Profile(out.id)),
        snippet("global", SnippetScope::Global),
        snippet("kind", SnippetKindScope::Local.into_scope()),
    ] {
        s.upsert_snippet(&sn).unwrap();
    }
    (g, out.id)
}

trait IntoScope {
    fn into_scope(self) -> SnippetScope;
}
impl IntoScope for SnippetKindScope {
    fn into_scope(self) -> SnippetScope {
        SnippetScope::Kind(self)
    }
}

#[test]
fn group_delete_impact_matches_actual_deletion() {
    let mut s = store();
    let (g, out_id) = populated_group(&mut s);
    let before_p = s.profiles().unwrap().len() as u32;
    let before_s = s.snippets().unwrap().len() as u32;
    let impact = s.delete_impact(g.id).unwrap().unwrap();
    assert_eq!((impact.profiles, impact.snippets), (2, 3));

    let deleted = s.delete_group(g.id).unwrap().unwrap();
    assert_eq!(deleted.len() as u32, impact.profiles);
    assert_eq!(
        before_p - s.profiles().unwrap().len() as u32,
        impact.profiles
    );
    assert_eq!(
        before_s - s.snippets().unwrap().len() as u32,
        impact.snippets
    );
    // 组外的 profile 与其命令、全局与按类型的命令都还在。
    assert_eq!(s.profiles().unwrap()[0].id, out_id);
    let names: Vec<_> = s.snippets().unwrap().into_iter().map(|x| x.name).collect();
    assert_eq!(names, vec!["scope-out", "global", "kind"]);
    assert!(s.groups().unwrap().is_empty());
}

// ---- Snippet ----

#[test]
fn snippet_limit_blocks_new_but_not_update() {
    let mut s = store();
    let mut first = None;
    for i in 0..MAX_SNIPPETS {
        let sn = snippet(&format!("s{i}"), SnippetScope::Global);
        s.upsert_snippet(&sn).unwrap();
        first.get_or_insert(sn);
    }
    let extra = snippet("extra", SnippetScope::Global);
    assert!(matches!(
        s.upsert_snippet(&extra),
        Err(StoreError::SnippetLimitReached)
    ));
    let mut upd = first.unwrap();
    upd.text = "changed".into();
    s.upsert_snippet(&upd).unwrap();
    assert_eq!(s.snippets().unwrap().len(), MAX_SNIPPETS);
    // 删一条后又能新增。
    s.delete_snippet(upd.id).unwrap();
    s.upsert_snippet(&extra).unwrap();
}

#[test]
fn delete_snippet_is_idempotent() {
    let mut s = store();
    let sn = snippet("x", SnippetScope::Global);
    s.upsert_snippet(&sn).unwrap();
    s.delete_snippet(sn.id).unwrap();
    s.delete_snippet(sn.id).unwrap();
    s.delete_snippet(SnippetId::new()).unwrap();
    assert!(s.snippets().unwrap().is_empty());
}

#[test]
fn snippet_roundtrip_all_scopes() {
    let mut s = store();
    let p = local("p", None);
    let g = group("g");
    s.upsert_profile(&p).unwrap();
    s.upsert_group(&g).unwrap();
    let all = vec![
        snippet("1", SnippetScope::Profile(p.id)),
        snippet("2", SnippetScope::Group(g.id)),
        snippet("3", SnippetScope::Kind(SnippetKindScope::Local)),
        snippet("4", SnippetScope::Kind(SnippetKindScope::Ssh)),
        snippet("5", SnippetScope::Global),
    ];
    for x in &all {
        s.upsert_snippet(x).unwrap();
    }
    assert_eq!(s.snippets().unwrap(), all);
}

// ---- 历史 ----

#[test]
fn history_filters() {
    let mut s = store();
    assert!(!s.record_history("", 1).unwrap());
    assert!(!s.record_history("   ", 1).unwrap());
    assert!(!s.record_history(" secret", 1).unwrap());
    assert!(s.record_history("ls", 1).unwrap());
    assert_eq!(s.history("", 10).unwrap().len(), 1);
}

#[test]
fn history_length_limit_counts_chars_not_bytes() {
    let mut s = store();
    // 500 个汉字 = 1500 字节，按字符数仍在限内。
    assert!(s.record_history(&"汉".repeat(500), 1).unwrap());
    assert!(!s.record_history(&"汉".repeat(501), 2).unwrap());
    assert!(s.record_history(&"a".repeat(500), 3).unwrap());
    assert!(!s.record_history(&"a".repeat(501), 4).unwrap());
}

#[test]
fn history_upsert_and_prefix_query() {
    let mut s = store();
    s.record_history("git status", 10).unwrap();
    s.record_history("git commit", 20).unwrap();
    s.record_history("Git big", 25).unwrap();
    s.record_history("ls", 30).unwrap();
    s.record_history("git status", 40).unwrap();
    let h = s.history("git", 10).unwrap();
    assert_eq!(
        h.iter().map(|e| e.command.as_str()).collect::<Vec<_>>(),
        vec!["git status", "git commit"],
        "区分大小写，最近使用在前"
    );
    assert_eq!(h[0].use_count, 2);
    assert_eq!(h[0].last_used_at, 40);
    assert_eq!(s.history("git", 1).unwrap().len(), 1);
    // 前缀里的 LIKE 通配符按字面处理。
    s.record_history("100%", 50).unwrap();
    assert_eq!(s.history("100%", 10).unwrap().len(), 1);
    assert_eq!(s.history("%", 10).unwrap().len(), 0);
}

#[test]
fn history_evicts_least_recently_used_not_oldest_recorded() {
    let mut s = store().with_history_capacity(3);
    s.record_history("old-but-popular", 1).unwrap();
    s.record_history("b", 2).unwrap();
    s.record_history("c", 3).unwrap();
    // 常用的旧命令被再次使用，刷新 last_used_at。
    s.record_history("old-but-popular", 4).unwrap();
    s.record_history("d", 5).unwrap();
    let cmds: Vec<String> = s
        .history("", 10)
        .unwrap()
        .into_iter()
        .map(|e| e.command)
        .collect();
    assert_eq!(cmds.len(), 3);
    assert!(cmds.contains(&"old-but-popular".to_owned()));
    assert!(!cmds.contains(&"b".to_owned()), "最久没用的 b 被淘汰");
    // 已有命令再次使用不触发淘汰。
    s.record_history("c", 6).unwrap();
    assert_eq!(s.history("", 10).unwrap().len(), 3);
}

#[test]
fn clear_history_only_clears_history() {
    let mut s = store();
    s.record_history("ls", 1).unwrap();
    s.upsert_profile(&local("p", None)).unwrap();
    s.clear_history().unwrap();
    assert!(s.history("", 10).unwrap().is_empty());
    assert_eq!(s.profiles().unwrap().len(), 1);
}

#[test]
fn history_hosts_share_one_lru_entry_and_clear_persisted_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    {
        let mut database = Store::open(&path).unwrap();
        for index in 0..5000 {
            database
                .record_history_on_host(&format!("command {index:04}"), "server-a", index)
                .unwrap();
        }
        database
            .record_history_on_host("command 0000", "server-b", 5001)
            .unwrap();
        database
            .record_history_on_host("command new", "server-b", 5002)
            .unwrap();
        let rows = database.completion_history("server-a").unwrap();
        assert_eq!(rows.len(), 5000);
        assert!(
            rows.iter()
                .any(|(entry, same_host)| entry.command == "command 0000" && *same_host)
        );
        assert!(
            !rows
                .iter()
                .any(|(entry, _)| entry.command == "command 0001")
        );
    }
    let mut database = Store::open(&path).unwrap();
    assert_eq!(
        database
            .completion_history("server-b")
            .unwrap()
            .iter()
            .filter(|(_, same_host)| *same_host)
            .count(),
        2
    );
    database.clear_history().unwrap();
    drop(database);
    assert!(
        Store::open(&path)
            .unwrap()
            .history("", 5001)
            .unwrap()
            .is_empty()
    );
}

// ---- 设置 ----

#[test]
fn completion_history_reuses_snapshot_until_a_history_write() {
    let mut database = store().with_history_capacity(2);
    database.record_history_on_host("ls", "a", 1).unwrap();
    assert!(database.completion_history("a").unwrap()[0].1);
    database
        .conn
        .execute_batch("ALTER TABLE history RENAME TO cached_history")
        .unwrap();
    for _ in 0..100 {
        assert!(database.completion_history("a").unwrap()[0].1);
        assert!(!database.completion_history("b").unwrap()[0].1);
    }
    database
        .conn
        .execute_batch("ALTER TABLE cached_history RENAME TO history")
        .unwrap();
    database.record_history_on_host("ls", "b", 2).unwrap();
    let snapshot = database.completion_history("b").unwrap();
    assert!(snapshot[0].1);
    assert_eq!(snapshot[0].0.use_count, 2);
    database.record_history("pwd", 3).unwrap();
    database.record_history("date", 4).unwrap();
    let snapshot = database.completion_history("a").unwrap();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.iter().all(|(entry, _)| entry.command != "ls"));
    database.clear_history().unwrap();
    assert!(database.completion_history("a").unwrap().is_empty());
}

#[test]
fn settings_roundtrip_and_reset_leaves_other_tables() {
    let mut s = store();
    assert_eq!(s.get_setting("k").unwrap(), None);
    s.set_setting("k", r#"{"v":1}"#).unwrap();
    s.set_setting("k", r#"{"v":2}"#).unwrap();
    assert_eq!(s.get_setting("k").unwrap().as_deref(), Some(r#"{"v":2}"#));
    assert!(matches!(
        s.set_setting("bad", "{not json"),
        Err(StoreError::InvalidData { .. })
    ));
    s.upsert_profile(&local("p", None)).unwrap();
    s.upsert_group(&group("g")).unwrap();
    s.upsert_snippet(&snippet("s", SnippetScope::Global))
        .unwrap();
    s.record_history("ls", 1).unwrap();
    s.reset_settings().unwrap();
    assert_eq!(s.get_setting("k").unwrap(), None);
    assert_eq!(s.profiles().unwrap().len(), 1);
    assert_eq!(s.groups().unwrap().len(), 1);
    assert_eq!(s.snippets().unwrap().len(), 1);
    assert_eq!(s.history("", 10).unwrap().len(), 1);
}

#[test]
fn open_file_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub/kanso.db");
    let p = local("p", None);
    {
        let mut s = Store::open(&path).unwrap();
        s.upsert_profile(&p).unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert_eq!(s.profiles().unwrap(), vec![p]);
}

// ---- 导出 / 导入 ----

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write(dir: &std::path::Path, name: &str, v: &Value) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, serde_json::to_vec(v).unwrap()).unwrap();
    p
}

#[test]
fn export_appends_extension_and_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    s.upsert_profile(&local("p", None)).unwrap();
    let out = s.export(&dir.path().join("backup"), "1.2.3").unwrap();
    assert_eq!(out, dir.path().join("backup.kanso"));
    assert!(out.exists());
    // 大小写不同的扩展名原样保留；其他扩展名追加。
    let out = s.export(&dir.path().join("B.KANSO"), "1").unwrap();
    assert_eq!(out, dir.path().join("B.KANSO"));
    let out = s.export(&dir.path().join("c.json"), "1").unwrap();
    assert_eq!(out, dir.path().join("c.json.kanso"));
    // 没有残留临时文件。
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    let v = read_json(&dir.path().join("backup.kanso"));
    assert_eq!(v["app"], "kanso-data");
    assert_eq!(v["formatVersion"], 1);
    assert_eq!(v["appVersion"], "1.2.3");
}

#[test]
fn export_failure_leaves_no_tmp_file() {
    let dir = tempfile::tempdir().unwrap();
    let s = store();
    // 目标目录不存在：创建临时文件就会失败。
    let r = s.export(&dir.path().join("nope/x.kanso"), "1");
    assert!(matches!(r, Err(StoreError::Io(_))));
}

#[test]
fn export_contains_no_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let mut p = ssh("srv", None, None);
    if let ProfileKind::Ssh(ref mut x) = p.kind {
        x.target.private_key_path = Some("/home/u/.ssh/id_ed25519".into());
    }
    s.upsert_profile(&p).unwrap();
    s.set_setting("theme", r#"{"name":"dark"}"#).unwrap();
    let out = s.export(&dir.path().join("x.kanso"), "1").unwrap();
    let text = std::fs::read_to_string(out).unwrap();
    for forbidden in ["passphrase", "secret", "\"password\":", "credential"] {
        assert!(!text.contains(forbidden), "导出不应含 {forbidden}");
    }
}

#[test]
fn export_import_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = store();
    let g = group("work");
    a.upsert_group(&g).unwrap();
    let jump = ssh("jump", Some(g.id), None);
    let target_p = ssh("target", Some(g.id), Some(jump.id));
    let loc = local("loc", None);
    for p in [&jump, &target_p, &loc] {
        a.upsert_profile(p).unwrap();
    }
    let sn = [
        snippet("g", SnippetScope::Group(g.id)),
        snippet("p", SnippetScope::Profile(loc.id)),
        snippet("k", SnippetScope::Kind(SnippetKindScope::Ssh)),
        snippet("glob", SnippetScope::Global),
    ];
    for x in &sn {
        a.upsert_snippet(x).unwrap();
    }
    a.set_setting("font", r#"{"v":1,"size":13}"#).unwrap();
    let out = a.export(&dir.path().join("all"), "1").unwrap();

    let mut b = store();
    let r = b.import_file(&out, &ok).unwrap();
    assert_eq!((r.added, r.updated), (1 + 3 + 4, 0));
    assert!(r.skipped.is_empty(), "{:?}", r.skipped);
    assert_eq!(b.groups().unwrap(), a.groups().unwrap());
    assert_eq!(b.profiles().unwrap(), a.profiles().unwrap());
    assert_eq!(b.snippets().unwrap(), a.snippets().unwrap());
    assert_eq!(
        b.get_setting("font")
            .unwrap()
            .as_deref()
            .map(|s| serde_json::from_str::<Value>(s).unwrap()),
        Some(json!({"v":1,"size":13}))
    );
    // 再导入一次：全是更新，数据不变。
    let r = b.import_file(&out, &ok).unwrap();
    assert_eq!((r.added, r.updated), (0, 1 + 3 + 4));
    assert_eq!(b.profiles().unwrap(), a.profiles().unwrap());
}

#[test]
fn import_rejects_wrong_extension_and_non_kanso() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let good = json!({"app":"kanso-data","formatVersion":1});
    let p = write(dir.path(), "a.json", &good);
    assert!(matches!(
        s.import_file(&p, &ok),
        Err(ImportError::WrongExtension)
    ));
    // 扩展名不区分大小写。
    let p = write(dir.path(), "a.KaNsO", &good);
    assert!(s.import_file(&p, &ok).is_ok());
    // 旧版应用标记走 1–3 版兼容路径。
    let p = write(
        dir.path(),
        "old.kanso",
        &json!({"app":"Kanso","formatVersion":1}),
    );
    assert!(s.import_file(&p, &ok).is_ok());
    let p = write(dir.path(), "other.kanso", &json!({"hello": 1}));
    assert!(matches!(
        s.import_file(&p, &ok),
        Err(ImportError::NotKansoFile)
    ));
    let p = write(dir.path(), "arr.kanso", &json!([1, 2]));
    assert!(matches!(
        s.import_file(&p, &ok),
        Err(ImportError::NotKansoFile)
    ));
    let p = dir.path().join("garbage.kanso");
    std::fs::write(&p, b"\xff\xfe not json").unwrap();
    assert!(matches!(
        s.import_file(&p, &ok),
        Err(ImportError::NotKansoFile)
    ));
    let p = write(dir.path(), "nover.kanso", &json!({"app":"kanso-data"}));
    assert!(matches!(
        s.import_file(&p, &ok),
        Err(ImportError::NotKansoFile)
    ));
    // 文件不存在：IO 错误。
    assert!(matches!(
        s.import_file(&dir.path().join("missing.kanso"), &ok),
        Err(ImportError::Io(_))
    ));
}

#[test]
fn import_rejects_newer_version_without_touching_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let p = write(
        dir.path(),
        "new.kanso",
        &json!({"app":"kanso-data","formatVersion":2,"groups":[{"id":GroupId::new(),"name":"x"}]}),
    );
    assert!(matches!(
        s.import_file(&p, &ok),
        Err(ImportError::VersionTooNew {
            version: 2,
            supported: 1
        })
    ));
    assert!(s.groups().unwrap().is_empty());
}

#[test]
fn import_skips_bad_entries_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let gid = GroupId::new();
    let good = ProfileId::new();
    let doc = json!({
        "app": "kanso-data", "formatVersion": 1,
        "groups": [{"id": gid, "name": "g"}, {"name": "no id"}],
        "profiles": [
            {"id": good, "name": "ok", "kind": "local", "config": {}},
            {"id": ProfileId::new(), "name": "serial one", "kind": "serial", "config": {}},
            {"id": ProfileId::new(), "name": "missing host", "kind": "ssh", "config": {"user": "u"}},
            {"id": ProfileId::new(), "name": "bad color", "kind": "local", "color": "red", "config": {}},
            {"name": "no id", "kind": "local"},
            {"id": ProfileId::new(), "name": "bad auth", "kind": "sftp", "config": {"host":"h","user":"u","auth":"magic"}},
            "not an object"
        ],
        "snippets": [
            {"id": SnippetId::new(), "name": "s", "text": "x", "lineEnding": "lf", "scope": {"kind": "global"}},
            {"id": SnippetId::new(), "name": "bad le", "text": "x", "lineEnding": "cr", "scope": {"kind": "global"}},
            {"id": SnippetId::new(), "name": "bad scope", "text": "x", "lineEnding": "lf", "scope": {"kind": "kind", "ref": "ftp"}}
        ]
    });
    let p = write(dir.path(), "x.kanso", &doc);
    let r = s.import_file(&p, &ok).unwrap();
    assert_eq!((r.added, r.updated), (3, 0));
    assert_eq!(r.skipped.len(), 9, "{:?}", r.skipped);
    assert!(r.skipped.iter().any(|m| matches!(
        m,
        Message::ImportSkippedUnsupportedKind { name, kind } if name == "serial one" && kind == "serial"
    )));
    assert_eq!(s.profiles().unwrap().len(), 1);
    assert_eq!(s.profiles().unwrap()[0].id, good);
    assert_eq!(s.groups().unwrap().len(), 1);
    assert_eq!(s.snippets().unwrap().len(), 1);
}

#[test]
fn import_same_id_replaces_local_and_never_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let keep = local("keep-local", None);
    let mine = local("mine", None);
    s.upsert_profile(&keep).unwrap();
    s.upsert_profile(&mine).unwrap();
    s.upsert_snippet(&snippet("local-only", SnippetScope::Global))
        .unwrap();
    let doc = json!({
        "app": "kanso-data", "formatVersion": 1,
        "profiles": [
            {"id": mine.id, "name": "from file", "kind": "local", "config": {"shellCommand": "fish"}},
            {"id": ProfileId::new(), "name": "new", "kind": "local", "config": {}}
        ]
    });
    let r = s
        .import_file(&write(dir.path(), "m.kanso", &doc), &ok)
        .unwrap();
    assert_eq!((r.added, r.updated), (1, 1));
    let ps = s.profiles().unwrap();
    assert_eq!(ps.len(), 3);
    assert_eq!(ps[0], keep);
    assert_eq!(ps[1].name, "from file");
    assert_eq!(s.snippets().unwrap().len(), 1, "本地独有的命令保留");
}

#[test]
fn import_ignores_credential_fields() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let id = ProfileId::new();
    let doc = json!({
        "app": "kanso-data", "formatVersion": 1,
        "profiles": [{"id": id, "name": "n", "kind": "ssh", "password": "hunter2",
            "config": {"host":"h","user":"u","password":"hunter2","passphrase":"x"}}]
    });
    let r = s
        .import_file(&write(dir.path(), "c.kanso", &doc), &ok)
        .unwrap();
    assert_eq!(r.added, 1);
    let cfg: String = s
        .conn
        .query_row("SELECT config FROM profiles", [], |r| r.get(0))
        .unwrap();
    assert!(!cfg.contains("hunter2") && !cfg.contains("passphrase"));
}

#[test]
fn import_fixes_references() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let jump_ok = ProfileId::new();
    let target_id = ProfileId::new();
    let dangling = ProfileId::new();
    let to_local = ProfileId::new();
    let missing_group = GroupId::new();
    let ssh_cfg = |jump: Option<ProfileId>| json!({"host":"h","user":"u","jump": jump});
    let doc = json!({
        "app": "kanso-data", "formatVersion": 1,
        "profiles": [
            // 跳板在同一个文件里、顺序在后面：也应保留。
            {"id": target_id, "name": "t", "kind": "ssh", "group": missing_group, "config": ssh_cfg(Some(jump_ok))},
            {"id": jump_ok, "name": "j", "kind": "ssh", "config": ssh_cfg(None)},
            {"id": dangling, "name": "d", "kind": "ssh", "config": ssh_cfg(Some(ProfileId::new()))},
            {"id": ProfileId::new(), "name": "loc", "kind": "local", "config": {}},
            {"id": to_local, "name": "tl", "kind": "ssh", "config": ssh_cfg(Some(ProfileId::new()))}
        ],
        "snippets": [
            {"id": SnippetId::new(), "name": "s1", "text": "x", "lineEnding": "none",
             "scope": {"kind": "profile", "ref": ProfileId::new()}},
            {"id": SnippetId::new(), "name": "s2", "text": "x", "lineEnding": "none",
             "scope": {"kind": "group", "ref": missing_group}},
            {"id": SnippetId::new(), "name": "s3", "text": "x", "lineEnding": "none",
             "scope": {"kind": "profile", "ref": target_id}}
        ]
    });
    let validate = |p: &Profile, all: &[Profile]| -> Result<(), Message> {
        // 模拟 core 的校验：跳板必须存在。
        if let ProfileKind::Ssh(s) = &p.kind
            && let Some(j) = s.target.jump
            && !all.iter().any(|x| x.id == j)
        {
            return Err(Message::JumpMissing {
                profile: j.to_string(),
            });
        }
        Ok(())
    };
    let r = s
        .import_file(&write(dir.path(), "r.kanso", &doc), &validate)
        .unwrap();
    assert!(r.skipped.is_empty(), "{:?}", r.skipped);
    let ps = s.profiles().unwrap();
    let get = |id: ProfileId| ps.iter().find(|p| p.id == id).unwrap().clone();
    assert_eq!(get(target_id).group, None);
    let jump_of = |p: &Profile| match &p.kind {
        ProfileKind::Ssh(s) => s.target.jump,
        _ => None,
    };
    assert_eq!(jump_of(&get(target_id)), Some(jump_ok));
    assert_eq!(jump_of(&get(dangling)), None);
    let scopes: Vec<_> = s.snippets().unwrap().into_iter().map(|x| x.scope).collect();
    assert_eq!(
        scopes,
        vec![
            SnippetScope::Global,
            SnippetScope::Global,
            SnippetScope::Profile(target_id)
        ]
    );
}

#[test]
fn import_skips_profiles_failing_validation_and_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let a = ProfileId::new();
    let b = ProfileId::new();
    let doc = json!({
        "app": "kanso-data", "formatVersion": 1,
        "profiles": [
            {"id": ProfileId::new(), "name": "empty host", "kind": "ssh", "config": {"host":"","user":"u"}},
            {"id": a, "name": "a", "kind": "ssh", "config": {"host":"h","user":"u","jump": b}},
            {"id": b, "name": "b", "kind": "ssh", "config": {"host":"h","user":"u","jump": a}},
            {"id": ProfileId::new(), "name": "fine", "kind": "local", "config": {}}
        ]
    });
    // 用与 core 相同形状的校验：主机为空、成环。
    fn validate(p: &Profile, all: &[Profile]) -> Result<(), Message> {
        if let ProfileKind::Ssh(s) = &p.kind {
            if s.target.host.is_empty() {
                return Err(Message::ProfileHostEmpty {});
            }
            let mut cur = s.target.jump;
            let mut hops = 0;
            while let Some(j) = cur {
                if j == p.id || hops > 8 {
                    return Err(Message::JumpCycle {
                        profile: p.name.clone(),
                    });
                }
                hops += 1;
                cur = all.iter().find(|x| x.id == j).and_then(|x| match &x.kind {
                    ProfileKind::Ssh(s) => s.target.jump,
                    _ => None,
                });
            }
        }
        Ok(())
    }
    let r = s
        .import_file(&write(dir.path(), "v.kanso", &doc), &validate)
        .unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(r.skipped.len(), 3);
    assert_eq!(s.profiles().unwrap()[0].name, "fine");
}

#[test]
fn import_respects_snippet_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    for i in 0..MAX_SNIPPETS - 1 {
        s.upsert_snippet(&snippet(&format!("s{i}"), SnippetScope::Global))
            .unwrap();
    }
    let mk = |n: &str| json!({"id": SnippetId::new(), "name": n, "text": "x", "lineEnding": "none", "scope": {"kind": "global"}});
    let doc = json!({"app":"kanso-data","formatVersion":1,"snippets":[mk("a"), mk("b")]});
    let r = s
        .import_file(&write(dir.path(), "l.kanso", &doc), &ok)
        .unwrap();
    assert_eq!((r.added, r.skipped.len()), (1, 1));
    assert_eq!(s.snippets().unwrap().len(), MAX_SNIPPETS);
}

#[test]
fn import_is_transactional() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    // 破坏 settings 表，让导入在写设置（最后一步）时失败；此前写入的分组与会话必须回滚。
    s.conn.execute("DROP TABLE settings", []).unwrap();
    let doc = json!({
        "app": "kanso-data", "formatVersion": 1,
        "groups": [{"id": GroupId::new(), "name": "g"}],
        "profiles": [{"id": ProfileId::new(), "name": "p", "kind": "local", "config": {}}],
        "settings": {"k": 1}
    });
    let r = s.import_file(&write(dir.path(), "t.kanso", &doc), &ok);
    assert!(matches!(r, Err(ImportError::Store(_))));
    assert!(s.groups().unwrap().is_empty());
    assert!(s.profiles().unwrap().is_empty());
}

#[test]
fn import_settings_overrides_same_key_and_keeps_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    s.set_setting("a", "1").unwrap();
    s.set_setting("b", "2").unwrap();
    let doc = json!({"app":"kanso-data","formatVersion":1,"settings":{"a": {"x": 1}, "c": "z"}});
    s.import_file(&write(dir.path(), "s.kanso", &doc), &ok)
        .unwrap();
    assert_eq!(s.get_setting("a").unwrap().as_deref(), Some(r#"{"x":1}"#));
    assert_eq!(s.get_setting("b").unwrap().as_deref(), Some("2"));
    assert_eq!(s.get_setting("c").unwrap().as_deref(), Some(r#""z""#));
}

#[test]
fn local_shell_defaults_stay_on_this_device() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    let key = crate::LOCAL_SHELL_DEFAULTS;
    let local = r#"{"shell_command":"local"}"#;
    s.set_setting(key, local).unwrap();
    let doc = json!({"app":"kanso-data","formatVersion":1,"settings":{(key): {"shell_command":"imported"}}});
    s.import_file(&write(dir.path(), "import.kanso", &doc), &ok)
        .unwrap();
    assert_eq!(s.get_setting(key).unwrap().as_deref(), Some(local));

    let path = s.export(&dir.path().join("export.kanso"), "1").unwrap();
    let exported: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(exported["settings"].get(key).is_none());
}

// ---- MCP 授权与审计（schema v3，spec/mcp.md §2.2、§6）----

mod mcp {
    use kanso_types::mcp::{MCP_AUDIT_RETENTION_MS, McpAccess, McpAuditEntry, McpAuditFilter};
    use kanso_types::{ProfileId, SessionId};

    use super::{group, local, ssh, store};

    fn entry(
        at_ms: i64,
        client: &str,
        profile: Option<ProfileId>,
        error: Option<&str>,
    ) -> McpAuditEntry {
        McpAuditEntry {
            at_ms,
            client: client.into(),
            mcp_session: "s1".into(),
            tool: "kanso_exec".into(),
            profile,
            session: Some(SessionId::from_raw(7)),
            args: "uname -a".into(),
            error_code: error.map(str::to_owned),
            duration_ms: 12,
            bytes: 34,
        }
    }

    #[test]
    fn enabled_defaults_off_and_roundtrips() {
        let mut s = store();
        assert!(!s.mcp_enabled().unwrap());
        s.set_mcp_enabled(true).unwrap();
        assert!(s.mcp_enabled().unwrap());
        s.set_mcp_enabled(false).unwrap();
        assert!(!s.mcp_enabled().unwrap());
    }

    #[test]
    fn access_defaults_off_set_and_clear() {
        let mut s = store();
        let p = ssh("web", None, None);
        s.upsert_profile(&p).unwrap();
        assert_eq!(s.mcp_access(p.id).unwrap(), McpAccess::Off);

        assert!(s.set_mcp_access(p.id, McpAccess::Full).unwrap());
        assert_eq!(s.mcp_access(p.id).unwrap(), McpAccess::Full);
        assert_eq!(s.mcp_accesses().unwrap(), vec![(p.id, McpAccess::Full)]);

        assert!(s.set_mcp_access(p.id, McpAccess::ReadOnly).unwrap());
        assert_eq!(s.mcp_access(p.id).unwrap(), McpAccess::ReadOnly);

        // Off 即删除记录，列表里不再出现。
        assert!(s.set_mcp_access(p.id, McpAccess::Off).unwrap());
        assert_eq!(s.mcp_access(p.id).unwrap(), McpAccess::Off);
        assert!(s.mcp_accesses().unwrap().is_empty());
    }

    #[test]
    fn access_for_unknown_profile_is_not_written() {
        let mut s = store();
        let ghost = ProfileId::new();
        assert!(!s.set_mcp_access(ghost, McpAccess::Full).unwrap());
        assert_eq!(s.mcp_access(ghost).unwrap(), McpAccess::Off);
    }

    #[test]
    fn deleting_profile_or_group_drops_grant_and_resurrection_starts_off() {
        let mut s = store();
        let p = ssh("web", None, None);
        s.upsert_profile(&p).unwrap();
        s.set_mcp_access(p.id, McpAccess::Full).unwrap();
        assert!(s.delete_profile(p.id).unwrap());
        assert_eq!(s.mcp_access(p.id).unwrap(), McpAccess::Off);
        // 墓碑复活（同 id 重新写入）后必须仍是关闭：授权记录已经物理删除。
        s.upsert_profile(&p).unwrap();
        assert_eq!(s.mcp_access(p.id).unwrap(), McpAccess::Off);

        let g = group("prod");
        s.upsert_group(&g).unwrap();
        let q = local("shell", Some(g.id));
        s.upsert_profile(&q).unwrap();
        s.set_mcp_access(q.id, McpAccess::ReadOnly).unwrap();
        s.delete_group(g.id).unwrap();
        assert_eq!(s.mcp_access(q.id).unwrap(), McpAccess::Off);
        assert!(s.mcp_accesses().unwrap().is_empty());
    }

    #[test]
    fn export_never_contains_mcp_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store();
        let p = ssh("web", None, None);
        s.upsert_profile(&p).unwrap();
        s.set_mcp_access(p.id, McpAccess::Full).unwrap();
        s.set_mcp_enabled(true).unwrap();
        s.append_mcp_audit(&[entry(1, "claude-code", Some(p.id), None)])
            .unwrap();
        let out = s.export(&dir.path().join("x"), "1").unwrap();
        let text = std::fs::read_to_string(out).unwrap().to_lowercase();
        assert!(!text.contains("mcp"), "export leaked MCP data: {text}");
        assert!(!text.contains("claude-code"));
    }

    #[test]
    fn audit_roundtrip_filter_and_paging() {
        let mut s = store();
        let p = ProfileId::new();
        s.append_mcp_audit(&[
            entry(1, "Claude-Code", Some(p), None),
            entry(2, "codex", None, Some("ACCESS_DENIED")),
            entry(3, "claude-code", Some(p), Some("TAB_BUSY")),
        ])
        .unwrap();

        let all = s.mcp_audit(&McpAuditFilter::default()).unwrap();
        assert_eq!(all.len(), 3);
        // 新的在前；字段原样往返。
        assert_eq!(
            all[0].entry,
            entry(3, "claude-code", Some(p), Some("TAB_BUSY"))
        );
        assert!(all[0].id > all[1].id && all[1].id > all[2].id);

        let claude = s
            .mcp_audit(&McpAuditFilter {
                client: Some("AuDe".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(claude.len(), 2);

        let errors = s
            .mcp_audit(&McpAuditFilter {
                errors_only: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(errors.len(), 2);

        let by_profile = s
            .mcp_audit(&McpAuditFilter {
                profile: Some(p),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_profile.len(), 2);

        let page = s
            .mcp_audit(&McpAuditFilter {
                before_id: Some(all[0].id),
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, all[1].id);

        let filtered_page = s
            .mcp_audit(&McpAuditFilter {
                client: Some("AuDe".into()),
                before_id: Some(all[0].id),
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(filtered_page.len(), 1);
        assert_eq!(filtered_page[0].id, all[2].id);

        s.clear_mcp_audit().unwrap();
        assert!(s.mcp_audit(&McpAuditFilter::default()).unwrap().is_empty());
    }

    #[test]
    fn audit_prune_by_age_then_by_count() {
        let mut s = store();
        let now = 100 * MCP_AUDIT_RETENTION_MS;
        s.append_mcp_audit(&[
            entry(now - MCP_AUDIT_RETENTION_MS - 1, "old", None, None),
            entry(now - MCP_AUDIT_RETENTION_MS, "edge", None, None),
            entry(now, "new", None, None),
        ])
        .unwrap();
        assert_eq!(s.prune_mcp_audit(now).unwrap(), 1);
        let left: Vec<String> = s
            .mcp_audit(&McpAuditFilter::default())
            .unwrap()
            .into_iter()
            .map(|r| r.entry.client)
            .collect();
        assert_eq!(left, vec!["new".to_owned(), "edge".to_owned()]);

        // 条数上限：按 id 保留最新的 10000 条，与 at 无关。
        let batch: Vec<McpAuditEntry> = (0..10_001)
            .map(|i| entry(now, &format!("c{i}"), None, None))
            .collect();
        s.append_mcp_audit(&batch).unwrap();
        let removed = s.prune_mcp_audit(now).unwrap();
        assert_eq!(removed, 2 + 1);
        let newest = s
            .mcp_audit(&McpAuditFilter {
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(newest[0].entry.client, "c10000");
    }
}
