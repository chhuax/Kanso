//! OpenSSH 配置导入（F6）的集成测试：真实 SQLite（临时目录）+ `MemorySecretStore`。
//!
//! 对应 store-import.md §7 与 §10 的验收用例。不读用户真实的 `~/.ssh`：HOME 指向临时目录，
//! 配置文件、`Include` 与默认私钥探测都在临时目录里。

use std::path::PathBuf;
use std::sync::Arc;

use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, Platform, SecretKind, SecretStore,
};
use kanso_secret::{MemorySecretStore, SecretRef};
use kanso_types::geometry::Rgb;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::{GroupId, Message, ProfileId};

struct Fx {
    tmp: tempfile::TempDir,
    core: Core,
    secrets: Arc<MemorySecretStore>,
    rt: tokio::runtime::Runtime,
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn ssh_dir(&self) -> PathBuf {
        self.home().join(".ssh")
    }

    /// 写主配置 `<HOME>/.ssh/config`，返回路径文本。
    fn config(&self, text: &str) -> String {
        std::fs::create_dir_all(self.ssh_dir()).unwrap();
        let p = self.ssh_dir().join("config");
        std::fs::write(&p, text).unwrap();
        p.to_string_lossy().into_owned()
    }

    /// 让默认私钥探测命中 `id_ed25519`。
    fn default_key(&self) -> String {
        std::fs::create_dir_all(self.ssh_dir()).unwrap();
        let p = self.ssh_dir().join("id_ed25519");
        std::fs::write(&p, "").unwrap();
        p.to_string_lossy().into_owned()
    }

    fn scan(&self, path: &str) -> Result<Vec<kanso_core::SshConfigCandidate>, CoreError> {
        self.scan_full(path).map(|s| s.candidates)
    }

    fn scan_full(&self, path: &str) -> Result<kanso_core::SshConfigScan, CoreError> {
        self.rt.block_on(self.core.scan_ssh_config(Some(path)))
    }

    /// 导入，显式传配置文件路径。
    fn import(
        &self,
        path: &str,
        aliases: &[&str],
        group: Option<GroupId>,
    ) -> Result<kanso_core::SshConfigImportReport, CoreError> {
        let aliases: Vec<String> = aliases.iter().map(|s| (*s).to_owned()).collect();
        self.rt
            .block_on(self.core.import_ssh_config(Some(path), &aliases, group))
    }

    fn profile(&self, name: &str) -> Option<Profile> {
        self.core.profiles().into_iter().find(|p| p.name == name)
    }

    fn target(&self, name: &str) -> SshTarget {
        match self
            .profile(name)
            .unwrap_or_else(|| panic!("无 {name}"))
            .kind
        {
            ProfileKind::Ssh(s) => s.target,
            _ => panic!("不是 SSH"),
        }
    }

    fn save_ssh(&self, name: &str, host: &str, port: u16, user: &str) -> Profile {
        let p = Profile {
            id: ProfileId::new(),
            name: name.into(),
            color: Some(Rgb { r: 1, g: 2, b: 3 }),
            group: None,
            kind: ProfileKind::Ssh(SshProfile {
                target: SshTarget {
                    host: host.into(),
                    port,
                    user: user.into(),
                    auth: AuthMethod::Password,
                    private_key_path: None,
                    jump: None,
                    proxy_command: None,
                    keepalive: kanso_types::profile::KeepaliveConfig::default(),
                },
                encoding: EncodingLabel::new("gbk"),
                locale: None,
            }),
        };
        self.core.save_profile(p.clone()).unwrap();
        p
    }

    fn save_group(&self, name: &str) -> GroupId {
        let id = GroupId::new();
        self.core
            .save_group(Group {
                id,
                name: name.into(),
            })
            .unwrap();
        id
    }
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::new());
    let mut platform = Platform::for_tests(Arc::new(CollectingEventSink::new()));
    platform.secrets = secrets.clone();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let core = Core::new_with_base_env(
        CoreConfig::new(tmp.path().join("data"), "9.9.9-test", "test.kanso"),
        platform,
        vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("USER".into(), "tester".into()),
        ],
    )
    .unwrap();
    Fx {
        tmp,
        core,
        secrets,
        rt: tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap(),
    }
}

fn has_warning(ws: &[Message], f: impl Fn(&Message) -> bool) -> bool {
    ws.iter().any(f)
}

#[test]
fn scan_handles_star_include_negation_and_multi_hop() {
    let fx = fx();
    std::fs::create_dir_all(fx.ssh_dir().join("config.d")).unwrap();
    std::fs::write(
        fx.ssh_dir().join("config.d/extra.conf"),
        "Host inc\n  HostName inc.example.com\n  Port 2022\n",
    )
    .unwrap();
    let path = fx.config(
        "Host *\n  User fallback\n\
         Include config.d/*\n\
         Host web web-alt\n  HostName web.example.com\n  Port 2200\n\
         Host !web *.wild\n  User wild\n\
         Host hopper\n  HostName hop.example.com\n  User admin\n  ProxyJump a,b\n\
         Host viaone\n  ProxyJump bastion\n\
         Host skip\n  ProxyJump none\n",
    );
    let c = fx.scan(&path).unwrap();
    let names: Vec<_> = c.iter().map(|c| c.alias.as_str()).collect();
    // 通配与否定模式不算别名；每个别名一个条目。
    assert_eq!(names, ["inc", "web", "web-alt", "hopper", "viaone", "skip"]);
    let by = |a: &str| c.iter().find(|c| c.alias == a).unwrap();
    // Host * 在前，给所有主机设默认用户；Include 的内容在引入处展开。
    assert_eq!(
        (
            by("inc").host.as_str(),
            by("inc").port,
            by("inc").user.as_str()
        ),
        ("inc.example.com", 2022, "fallback")
    );
    assert_eq!(
        (by("web").host.as_str(), by("web").port),
        ("web.example.com", 2200)
    );
    assert_eq!(by("web-alt").host, "web.example.com");
    // 没写 HostName 用别名本身。
    assert_eq!(by("viaone").host, "viaone");
    // 每个选项取第一次出现的值：前面的 Host * 压过后面块里的 User。
    assert_eq!(by("hopper").user, "fallback");
    assert_eq!(by("hopper").proxy_jump.as_deref(), Some("a,b"));
    assert!(has_warning(&by("hopper").warnings, |m| matches!(
        m,
        Message::SshConfigMultiHopIgnored { alias } if alias == "hopper"
    )));
    assert!(by("viaone").warnings.is_empty());
    assert_eq!(by("viaone").proxy_jump.as_deref(), Some("bastion"));
    // none 当作没有。
    assert_eq!(by("skip").proxy_jump, None);
    // 没有私钥时 identity_file 为空。
    assert_eq!(by("web").identity_file, None);
}

#[test]
fn scan_reports_explicit_and_default_identity_and_local_user() {
    let fx = fx();
    let key = fx.default_key();
    let path = fx.config("Host a\n  IdentityFile /keys/a\nHost b\n");
    let c = fx.scan(&path).unwrap();
    assert_eq!(c[0].identity_file.as_deref(), Some("/keys/a"));
    assert_eq!(c[1].identity_file.as_deref(), Some(key.as_str()));
    // 没写 User：本地用户名（来自基础环境快照）。
    assert_eq!(c[1].user, "tester");
}

#[test]
fn scan_default_path_and_unreadable_file() {
    let fx = fx();
    // 默认路径不存在 → User(ImportReadFailed)。
    let err = fx.rt.block_on(fx.core.scan_ssh_config(None)).unwrap_err();
    assert!(
        matches!(err, CoreError::User(Message::ImportReadFailed { .. })),
        "{err:?}"
    );
    fx.config("Host x\n");
    let c = fx.rt.block_on(fx.core.scan_ssh_config(None)).unwrap();
    assert_eq!(c.candidates.len(), 1);
    // 导入同样支持默认路径（path 为 None）。
    let r = fx
        .rt
        .block_on(fx.core.import_ssh_config(None, &["x".to_owned()], None));
    assert_eq!(r.unwrap().added, 1);
    // 显式路径是目录：同样不可读。
    let dir = fx.ssh_dir().to_string_lossy().into_owned();
    assert!(matches!(
        fx.scan(&dir),
        Err(CoreError::User(Message::ImportReadFailed { .. }))
    ));
    // 空配置文件是正常的“没有主机”，不是错误。
    let empty = fx.config("");
    assert!(fx.scan(&empty).unwrap().is_empty());
}

#[test]
fn existing_by_name_and_by_endpoint() {
    let fx = fx();
    let by_name = fx.save_ssh("alpha", "other.example.com", 22, "x");
    let by_ep = fx.save_ssh("renamed", "beta.example.com", 2200, "bob");
    // 主机相同但用户名不同：不算已保存。
    fx.save_ssh("near", "gamma.example.com", 22, "someone-else");
    let path = fx.config(
        "Host alpha\n  HostName alpha.example.com\n  User u\n\
         Host beta\n  HostName BETA.example.com\n  Port 2200\n  User bob\n\
         Host gamma\n  HostName gamma.example.com\n  User gu\n",
    );
    let c = fx.scan(&path).unwrap();
    assert_eq!(c[0].existing, Some(by_name.id));
    assert_eq!(c[1].existing, Some(by_ep.id));
    assert_eq!(c[2].existing, None);
}

#[test]
fn update_keeps_name_color_group_encoding_and_manual_jump() {
    let fx = fx();
    let g = fx.save_group("prod");
    let manual_jump = fx.save_ssh("manual-jump", "mj.example.com", 22, "mj");
    let mut p = fx.save_ssh("web", "old.example.com", 22, "old");
    p.group = Some(g);
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.target.jump = Some(manual_jump.id);
    }
    fx.core.save_profile(p.clone()).unwrap();
    fx.rt
        .block_on(
            fx.core
                .set_credential(p.id, SecretKind::Password, SecretString::new("pw")),
        )
        .unwrap();

    let path = fx.config(
        "Host web\n  HostName new.example.com\n  Port 2201\n  User newuser\n  ProxyJump other-jump\n\
         Host other-jump\n  HostName oj.example.com\n",
    );
    let r = fx.import(&path, &["web"], None).unwrap();
    assert_eq!((r.added, r.updated, r.jumps_added), (0, 1, 0));

    let got = fx.profile("web").unwrap();
    assert_eq!(got.id, p.id);
    assert_eq!(got.color, Some(Rgb { r: 1, g: 2, b: 3 }));
    assert_eq!(got.group, Some(g));
    let ProfileKind::Ssh(s) = got.kind else {
        panic!()
    };
    assert_eq!(s.encoding, EncodingLabel::new("gbk"));
    assert_eq!(
        (
            s.target.host.as_str(),
            s.target.port,
            s.target.user.as_str()
        ),
        ("new.example.com", 2201, "newuser")
    );
    // 手动设置的跳板机不被覆盖；没有额外导入 other-jump。
    assert_eq!(s.target.jump, Some(manual_jump.id));
    assert!(fx.profile("other-jump").is_none());
    // 凭据不动。
    assert!(
        fx.secrets
            .get(&SecretRef {
                profile: p.id,
                kind: SecretKind::Password
            })
            .unwrap()
            .is_some()
    );
    assert_eq!(fx.core.profiles().len(), 2);
}

#[test]
fn default_key_keeps_password_auth_but_explicit_key_changes_it() {
    let fx = fx();
    fx.default_key();
    fx.save_ssh("a", "a.example.com", 22, "u");
    fx.save_ssh("b", "b.example.com", 22, "u");
    let path = fx.config(
        "Host a\n  HostName a.example.com\n  User u\n\
         Host b\n  HostName b.example.com\n  User u\n  IdentityFile /keys/b\n",
    );
    let r = fx.import(&path, &["a", "b"], None).unwrap();
    assert_eq!((r.added, r.updated), (0, 2));
    // 只有默认私钥探测结果：保持密码认证。
    let a = fx.target("a");
    assert_eq!((a.auth, a.private_key_path), (AuthMethod::Password, None));
    // 显式写了私钥：改成私钥认证并设置路径。
    let b = fx.target("b");
    assert_eq!(
        (b.auth, b.private_key_path.as_deref()),
        (AuthMethod::PrivateKey, Some("/keys/b"))
    );
}

#[test]
fn new_profiles_get_name_group_and_auth_from_keys() {
    let fx = fx();
    let g = fx.save_group("imported");
    let path = fx.config("Host plain\n  User u\nHost keyed\n  User u\n  IdentityFile /keys/k\n");
    // 没有默认私钥、没有显式私钥 → 密码。
    let r = fx.import(&path, &["plain", "keyed"], Some(g)).unwrap();
    assert_eq!((r.added, r.updated, r.jumps_added), (2, 0, 0));
    let plain = fx.profile("plain").unwrap();
    assert_eq!(plain.group, Some(g));
    assert_eq!(fx.target("plain").auth, AuthMethod::Password);
    assert_eq!(fx.target("keyed").auth, AuthMethod::PrivateKey);
    assert_eq!(
        fx.target("keyed").private_key_path.as_deref(),
        Some("/keys/k")
    );

    // 有默认私钥、没有显式私钥 → 私钥（路径为探测到的）。
    let key = fx.default_key();
    let path = fx.config("Host fresh\n  User u\n");
    fx.import(&path, &["fresh"], None).unwrap();
    let t = fx.target("fresh");
    assert_eq!(
        (t.auth, t.private_key_path.as_deref()),
        (AuthMethod::PrivateKey, Some(key.as_str()))
    );
    assert_eq!(fx.profile("fresh").unwrap().group, None);
}

#[test]
fn single_hop_alias_jump_is_imported_even_if_not_selected() {
    let fx = fx();
    let path = fx.config(
        "Host target\n  HostName t.example.com\n  User u\n  ProxyJump bastion\n\
         Host bastion\n  HostName b.example.com\n  User ops\n  Port 2222\n",
    );
    let r = fx.import(&path, &["target"], None).unwrap();
    assert_eq!((r.added, r.updated, r.jumps_added), (1, 0, 1));
    let bastion = fx.profile("bastion").unwrap();
    assert_eq!(fx.target("target").jump, Some(bastion.id));
    assert_eq!(fx.target("bastion").port, 2222);

    // 再导一次：跳板机已存在，直接复用，不再新增。
    let r = fx.import(&path, &["target"], None).unwrap();
    assert_eq!((r.added, r.updated, r.jumps_added), (0, 1, 0));
    assert_eq!(fx.core.profiles().len(), 2);
}

#[test]
fn alias_jump_that_is_also_selected_counts_as_added_not_jump() {
    let fx = fx();
    let path = fx.config("Host target\n  User u\n  ProxyJump bastion\nHost bastion\n  User ops\n");
    let r = fx.import(&path, &["target", "bastion"], None).unwrap();
    assert_eq!((r.added, r.jumps_added), (2, 0));
    let b = fx.profile("bastion").unwrap();
    assert_eq!(fx.target("target").jump, Some(b.id));
}

#[test]
fn host_form_jump_reuses_saved_session_or_creates_one() {
    let fx = fx();
    let saved = fx.save_ssh("my-bastion", "bastion.example.com", 2200, "ops");
    let path = fx.config(
        "Host reuse\n  User u\n  ProxyJump ops@bastion.example.com:2200\n\
         Host fresh\n  User u\n  ProxyJump v6user@[::1]:2222\n\
         Host nouser\n  User u\n  ProxyJump plain.example.com\n",
    );
    let r = fx
        .import(&path, &["reuse", "fresh", "nouser"], None)
        .unwrap();
    assert_eq!((r.added, r.jumps_added), (3, 2));
    // 连接目标相同的已保存会话被复用，名称不变，数量不增。
    assert_eq!(fx.target("reuse").jump, Some(saved.id));
    // 没有则新建，名称用主机名（IPv6 写成 [addr]:port）。
    let v6 = fx.profile("::1").unwrap();
    assert_eq!(fx.target("fresh").jump, Some(v6.id));
    let t = fx.target("::1");
    assert_eq!((t.port, t.user.as_str()), (2222, "v6user"));
    // 没写用户 / 端口：按配置求值（本地用户、22）。
    let plain = fx.profile("plain.example.com").unwrap();
    assert_eq!(fx.target("nouser").jump, Some(plain.id));
    let t = fx.target("plain.example.com");
    assert_eq!((t.port, t.user.as_str()), (22, "tester"));
}

#[test]
fn multi_hop_is_ignored_with_warning_and_session_still_saved() {
    let fx = fx();
    let path = fx.config(
        "Host chain\n  User u\n  ProxyJump a.example.com,b.example.com\n\
         Host ok\n  User u\n",
    );
    let r = fx.import(&path, &["chain", "ok"], None).unwrap();
    assert_eq!((r.added, r.multi_hop_ignored, r.jumps_added), (2, 1, 0));
    assert!(has_warning(&r.warnings, |m| matches!(
        m,
        Message::SshConfigMultiHopIgnored { alias } if alias == "chain"
    )));
    assert_eq!(fx.target("chain").jump, None);
    // 不为多跳链新建任何跳板机。
    assert_eq!(fx.core.profiles().len(), 2);
}

#[test]
fn existing_jump_is_not_overwritten_even_when_multi_hop() {
    let fx = fx();
    let jump = fx.save_ssh("j", "j.example.com", 22, "j");
    let mut p = fx.save_ssh("web", "w.example.com", 22, "u");
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.target.jump = Some(jump.id);
    }
    fx.core.save_profile(p).unwrap();
    let path = fx.config("Host web\n  HostName w.example.com\n  User u\n  ProxyJump a,b\n");
    let r = fx.import(&path, &["web"], None).unwrap();
    assert_eq!((r.updated, r.multi_hop_ignored, r.jumps_added), (1, 0, 0));
    assert_eq!(fx.target("web").jump, Some(jump.id));
}

#[test]
fn jump_cycle_only_warns() {
    let fx = fx();
    let path = fx.config("Host a\n  User u\n  ProxyJump b\nHost b\n  User u\n  ProxyJump a\n");
    let r = fx.import(&path, &["a", "b"], None).unwrap();
    assert_eq!((r.added, r.jumps_added), (2, 0));
    let a = fx.profile("a").unwrap();
    let b = fx.profile("b").unwrap();
    assert_eq!(fx.target("a").jump, Some(b.id));
    // 成环的那条不设跳板，只告警。
    assert_eq!(fx.target("b").jump, None);
    assert!(has_warning(&r.warnings, |m| matches!(
        m,
        Message::JumpCycle { .. }
    )));
    assert_ne!(a.id, b.id);

    // 指向自己同样只是告警。
    let path = fx.config("Host self\n  User u\n  ProxyJump self\n");
    let r = fx.import(&path, &["self"], None).unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(fx.target("self").jump, None);
    assert!(has_warning(&r.warnings, |m| matches!(
        m,
        Message::JumpCycle { .. }
    )));
}

#[test]
fn proxy_jump_none_means_no_jump() {
    let fx = fx();
    let path = fx.config("Host n\n  User u\n  ProxyJump none\n");
    let r = fx.import(&path, &["n"], None).unwrap();
    assert_eq!((r.added, r.jumps_added, r.multi_hop_ignored), (1, 0, 0));
    assert_eq!(fx.target("n").jump, None);
    assert!(r.warnings.is_empty());
}

#[test]
fn missing_group_is_rejected_and_nothing_written() {
    let fx = fx();
    let path = fx.config("Host a\n  User u\n");
    let ghost = GroupId::new();
    assert_eq!(
        fx.import(&path, &["a"], Some(ghost)),
        Err(CoreError::NoSuchGroup(ghost))
    );
    assert!(fx.core.profiles().is_empty());
}

#[test]
fn unreadable_file_on_import_and_unknown_alias() {
    let fx = fx();
    let path = fx.config("Host a\n  User u\n");
    fx.scan(&path).unwrap();
    // 扫描之后文件被删：导入重新解析，报错而不是用旧结果。
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        fx.rt.block_on(
            fx.core
                .import_ssh_config(Some(&path), &["a".to_owned()], None)
        ),
        Err(CoreError::User(Message::ImportReadFailed { .. }))
    ));
    // 文件里没有的别名：告警并跳过，其余照常导入。
    let path = fx.config("Host a\n  User u\n");
    let r = fx.import(&path, &["ghost", "a", "A"], None).unwrap();
    assert_eq!(r.added, 1);
    assert!(has_warning(&r.warnings, |m| matches!(
        m,
        Message::ImportSkippedInvalid { name, .. } if name == "ghost"
    )));
}

#[test]
fn rescan_marks_imported_hosts_as_existing() {
    let fx = fx();
    let path = fx.config("Host a\n  User u\n");
    assert_eq!(fx.scan(&path).unwrap()[0].existing, None);
    fx.import(&path, &["a"], None).unwrap();
    let id = fx.profile("a").unwrap().id;
    assert_eq!(fx.scan(&path).unwrap()[0].existing, Some(id));
}

#[test]
fn file_level_warnings_go_to_scan_warnings_not_candidates() {
    let fx = fx();
    let path = fx.config("Host a\n  Port 99999\n  User u\nMatch host zzz\n  User m\n");
    let s = fx.scan_full(&path).unwrap();
    assert_eq!(s.candidates.len(), 1);
    assert!(has_warning(&s.warnings, |m| matches!(
        m,
        Message::SshConfigMatchUnsupported { .. }
    )));
    // 候选只带与自己相关的警告。
    assert!(has_warning(&s.candidates[0].warnings, |m| matches!(
        m,
        Message::SshConfigInvalidPort { alias, value } if alias == "a" && value == "99999"
    )));
    assert!(!has_warning(&s.candidates[0].warnings, |m| matches!(
        m,
        Message::SshConfigMatchUnsupported { .. }
    )));
    // 导入报告里同样给出文件级警告。
    let r = fx.import(&path, &["a"], None).unwrap();
    assert!(has_warning(&r.warnings, |m| matches!(
        m,
        Message::SshConfigMatchUnsupported { .. }
    )));
}
