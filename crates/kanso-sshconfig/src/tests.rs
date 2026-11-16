//! 单元测试：全部使用内存文件系统，不碰真实 `~/.ssh`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::text::{split_line, wildcard_match};
use crate::*;

#[derive(Default)]
struct MemFs(BTreeMap<PathBuf, String>);

impl MemFs {
    fn with(files: &[(&str, &str)]) -> Self {
        MemFs(
            files
                .iter()
                .map(|(p, c)| (PathBuf::from(p), c.to_string()))
                .collect(),
        )
    }
}

impl ConfigFs for MemFs {
    fn read_to_string(&self, path: &Path) -> Option<String> {
        self.0.get(path).cloned()
    }
    fn glob(&self, pattern: &Path) -> Vec<PathBuf> {
        let name = pattern.file_name().unwrap().to_string_lossy().into_owned();
        // 故意返回倒序，验证调用方自己排序。
        self.0
            .keys()
            .rev()
            .filter(|k| {
                k.parent() == pattern.parent()
                    && wildcard_match(&name, &k.file_name().unwrap().to_string_lossy(), false)
            })
            .cloned()
            .collect()
    }
    fn exists(&self, path: &Path) -> bool {
        self.0.contains_key(path)
    }
}

fn env() -> Env {
    Env {
        home: PathBuf::from("/home/u"),
        local_user: "me".into(),
        local_host: "mac.example.com".into(),
        local_uid: 501,
    }
}

const MAIN: &str = "/home/u/.ssh/config";

fn scan_one(files: &[(&str, &str)]) -> ScanResult {
    scan(Path::new(MAIN), &MemFs::with(files), &env())
}

fn res(cfg: &str, host: &str) -> Resolved {
    resolve(host, Path::new(MAIN), &MemFs::with(&[(MAIN, cfg)]), &env())
}

fn names(r: &ScanResult) -> Vec<&str> {
    r.hosts.iter().map(|h| h.alias.as_str()).collect()
}

#[test]
fn lexer_forms() {
    let l = split_line("  HostName=  a.b  ").unwrap();
    assert_eq!(
        (l.keyword.as_str(), l.words),
        ("HostName", vec!["a.b".to_string()])
    );
    let l = split_line("User = bob").unwrap();
    assert_eq!(l.words, vec!["bob"]);
    let l = split_line("Host\ta  \"b c\" d").unwrap();
    assert_eq!(l.words, vec!["a", "b c", "d"]);
    // 引号段与非引号段拼接。
    assert_eq!(split_line("User a\"b c\"d").unwrap().words, vec!["ab cd"]);
    // 词内的 # 属于值，词首的 # 开始行内注释。
    assert_eq!(
        split_line("HostName foo#bar").unwrap().words,
        vec!["foo#bar"]
    );
    assert_eq!(split_line("User bob # 备注").unwrap().words, vec!["bob"]);
    assert!(split_line("   # comment").is_none());
    assert!(split_line("   ").is_none());
}

#[test]
fn keywords_case_insensitive_and_crlf() {
    let r = res(
        "host a\r\n  HOSTNAME h.example\r\n  pOrT 2200\r\n  uSeR bob\r\n",
        "a",
    );
    assert_eq!(r.host_name, "h.example");
    assert_eq!(r.port, 2200);
    assert_eq!(r.user, "bob");
}

#[test]
fn defaults() {
    let r = res("Host a\n", "a");
    assert_eq!(r.host_name, "a");
    assert_eq!(r.port, 22);
    assert_eq!(r.user, "me");
    assert_eq!(r.identity_file, None);
    assert_eq!(r.default_identity, None);
    assert_eq!(r.proxy_jump, None);
    assert!(r.warnings.is_empty());
}

#[test]
fn default_identity_probe_order() {
    let fs = MemFs::with(&[
        (MAIN, "Host a\n"),
        ("/home/u/.ssh/id_rsa", ""),
        ("/home/u/.ssh/id_ecdsa", ""),
    ]);
    let r = resolve("a", Path::new(MAIN), &fs, &env());
    assert_eq!(
        r.default_identity,
        Some(PathBuf::from("/home/u/.ssh/id_ecdsa"))
    );
    // 显式写了 IdentityFile 时不探测。
    let fs = MemFs::with(&[
        (MAIN, "Host a\n IdentityFile ~/k\n"),
        ("/home/u/.ssh/id_rsa", ""),
    ]);
    let r = resolve("a", Path::new(MAIN), &fs, &env());
    assert_eq!(r.identity_file, Some(PathBuf::from("/home/u/k")));
    assert_eq!(r.default_identity, None);
}

#[test]
fn first_value_wins_and_host_star_default() {
    let cfg = "Host *\n  User star\n  Port 2222\nHost a\n  User a\n  Port 1\nHost a\n User again\n";
    let r = res(cfg, "a");
    assert_eq!((r.user.as_str(), r.port), ("star", 2222));
    // 把 Host * 放最后，具体块优先。
    let cfg = "Host a\n  User a\nHost *\n  User star\n  Port 2222\n";
    let r = res(cfg, "a");
    assert_eq!((r.user.as_str(), r.port), ("a", 2222));
}

#[test]
fn leading_lines_apply_to_all() {
    let r = res("User top\nHost a\n User a\n", "a");
    assert_eq!(r.user, "top");
}

#[test]
fn patterns_and_negation() {
    let cfg = "Host web-* !web-secret\n User w\n";
    assert_eq!(res(cfg, "web-1").user, "w");
    assert_eq!(res(cfg, "WEB-1").user, "w");
    assert_eq!(res(cfg, "web-secret").user, "me");
    assert_eq!(res(cfg, "db").user, "me");
    // 否定模式即使排在前面也一票否决。
    let cfg = "Host !a *\n User x\n";
    assert_eq!(res(cfg, "a").user, "me");
    assert_eq!(res(cfg, "b").user, "x");
    // 只有否定模式不选中任何主机。
    assert_eq!(res("Host !a\n User x\n", "b").user, "me");
    // ? 匹配单个字符。
    assert_eq!(res("Host h?\n User q\n", "h1").user, "q");
    assert_eq!(res("Host h?\n User q\n", "h12").user, "me");
}

#[test]
fn alias_listing_and_dedup() {
    let r = scan_one(&[(
        MAIN,
        "Host a b*\n Port 1\nHost *\nHost !c d\nHost A\n User late\nHost e\n",
    )]);
    assert_eq!(names(&r), ["a", "d", "e"]);
    // 同一别名的多个块合并。
    let a = &r.hosts[0];
    assert_eq!(a.port, 1);
    assert_eq!(a.user, "late");
}

#[test]
fn include_in_block_belongs_to_block() {
    let r = scan_one(&[
        (MAIN, "Host a\n Include /inc/x\nHost b\n"),
        ("/inc/x", "User fromx\nPort 7\n"),
    ]);
    assert_eq!(r.hosts[0].user, "fromx");
    assert_eq!(r.hosts[0].port, 7);
    // b 没有被 a 块里的内容污染。
    assert_eq!(r.hosts[1].user, "me");
}

#[test]
fn include_with_host_then_outer_state_restored() {
    // ssh -G 实验：被引入文件里的 Host 开新块，但回到外层后仍属于外层原来的块。
    let r = scan_one(&[
        (
            MAIN,
            "Host a\n Include /inc/x\n Port 1111\nHost *\n HostName zz\n",
        ),
        ("/inc/x", "Host b\n User fromb\n"),
    ]);
    assert_eq!(names(&r), ["a", "b"]);
    let (a, b) = (&r.hosts[0], &r.hosts[1]);
    assert_eq!(a.port, 1111);
    assert_eq!(a.user, "me");
    assert_eq!(a.host_name, "zz");
    // b 只在 a 块里被引入，求值 b 时那个 Include 不会被读取。
    assert_eq!(b.user, "me");
    assert_eq!(b.port, 22);
}

#[test]
fn top_level_include_new_block_and_restore() {
    let r = scan_one(&[
        (MAIN, "Include /inc/x\nPort 1111\nHost c\n User cu\n"),
        ("/inc/x", "Host b\n User fromb\n Port 2002\n"),
    ]);
    let get = |n: &str| r.hosts.iter().find(|h| h.alias == n).unwrap().clone();
    assert_eq!((get("b").user, get("b").port), ("fromb".to_string(), 2002));
    // 其它主机：Include 之后回到全局作用域，Port 1111 对所有主机生效。
    assert_eq!(get("c").port, 1111);
    assert_eq!(res_with(r_files(), "zzz").port, 1111);
}

fn r_files() -> MemFs {
    MemFs::with(&[
        (MAIN, "Include /inc/x\nPort 1111\nHost c\n User cu\n"),
        ("/inc/x", "Host b\n User fromb\n Port 2002\n"),
    ])
}

fn res_with(fs: MemFs, host: &str) -> Resolved {
    resolve(host, Path::new(MAIN), &fs, &env())
}

#[test]
fn include_relative_tilde_and_glob_order() {
    let fs = MemFs::with(&[
        (
            MAIN,
            "Include config.d/*\nInclude ~/.ssh/extra \"/inc/with space\"\n",
        ),
        ("/home/u/.ssh/config.d/10-b", "Host b\n User b10\n"),
        ("/home/u/.ssh/config.d/05-a", "Host a\n User a05\n"),
        ("/home/u/.ssh/config.d/20-c", "Host c\n"),
        ("/home/u/.ssh/extra", "Host e\n"),
        ("/inc/with space", "Host s\n"),
    ]);
    let r = scan(Path::new(MAIN), &fs, &env());
    assert_eq!(names(&r), ["a", "b", "c", "e", "s"]);
    assert!(r.warnings.is_empty());
}

#[test]
fn include_missing_file_skipped() {
    let r = scan_one(&[(MAIN, "Include /nope\nInclude /nope/*\nHost a\n")]);
    assert_eq!(names(&r), ["a"]);
    assert!(r.warnings.is_empty());
}

#[test]
fn include_depth_limit() {
    // 主文件第 0 层；第 16 层可读，第 17 层放弃。
    let mut files = vec![(MAIN.to_string(), "Include /d/1\n".to_string())];
    for n in 1..=20 {
        files.push((
            format!("/d/{n}"),
            format!("Host h{n}\nInclude /d/{}\n", n + 1),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let r = scan_one(&refs);
    assert_eq!(r.hosts.len(), 16);
    assert_eq!(r.hosts.last().unwrap().alias, "h16");
    assert_eq!(
        r.warnings,
        vec![Warning::IncludeTooDeep {
            file: PathBuf::from("/d/16"),
            include: PathBuf::from("/d/17"),
        }]
    );
}

#[test]
fn token_expansion() {
    let cfg = "Host a\n HostName %h.example.com\n User u-%h-%n-%p-%u-%d\n IdentityFile %d/.ssh/%r_%h_%n_%p\n";
    let r = res(cfg, "a");
    assert_eq!(r.host_name, "a.example.com");
    assert_eq!(r.user, "u-a.example.com-a-22-me-/home/u");
    assert_eq!(
        r.identity_file,
        Some(PathBuf::from(
            "/home/u/.ssh/u-a.example.com-a-22-me-/home/u_a.example.com_a_22"
        ))
    );
    assert!(r.warnings.is_empty());
    let r = res("Host a\n User %%x\n", "a");
    assert_eq!(r.user, "%x");
    let r = res("Host a\n IdentityFile ~/k\n", "a");
    assert_eq!(r.identity_file, Some(PathBuf::from("/home/u/k")));
}

#[test]
fn unknown_tokens_kept_with_warning() {
    // HostName 只支持 %h；User 不支持 %r。
    let r = res("Host a\n HostName %p\n User %r\n", "a");
    assert_eq!(r.host_name, "%p");
    assert_eq!(r.user, "%r");
    assert_eq!(r.warnings.len(), 2);
    assert!(matches!(
        r.warnings[0],
        Warning::UnknownToken {
            option: "HostName",
            token: 'p',
            ..
        }
    ));
    let r = res("Host a\n User x%\n", "a");
    assert_eq!(r.user, "x%");
    assert_eq!(r.warnings.len(), 1);
}

#[test]
fn invalid_port() {
    for bad in ["0", "65536", "abc", "-1"] {
        let r = res(&format!("Host a\n Port {bad}\n"), "a");
        assert_eq!(r.port, 22, "{bad}");
        assert_eq!(r.warnings, vec![Warning::InvalidPort { value: bad.into() }]);
    }
    assert_eq!(res("Host a\n Port 65535\n", "a").port, 65535);
}

#[test]
fn match_block_selects_nothing_and_warns_once() {
    let cfg = "Host a\n User a\nMatch host a\n Port 1\nMatch all\n Port 2\nHost b\n";
    let r = scan_one(&[(MAIN, cfg)]);
    assert_eq!(r.hosts[0].port, 22);
    assert_eq!(
        r.warnings,
        vec![Warning::MatchUnsupported {
            file: PathBuf::from(MAIN)
        }]
    );
    // 之后的 Host 重新开始选中。
    assert_eq!(names(&r), ["a", "b"]);
}

#[test]
fn proxy_jump() {
    assert_eq!(
        res("Host a\n ProxyJump j1,u@j2:22\n", "a")
            .proxy_jump
            .as_deref(),
        Some("j1,u@j2:22")
    );
    // none 也占用“第一次出现”，后面的 Host * 不能覆盖。
    let cfg = "Host a\n ProxyJump none\nHost *\n ProxyJump g\n";
    assert_eq!(res(cfg, "a").proxy_jump, None);
    assert_eq!(res(cfg, "b").proxy_jump.as_deref(), Some("g"));
}

#[test]
fn unreadable_main_file() {
    let r = scan_one(&[]);
    assert!(r.hosts.is_empty());
}

#[test]
fn identity_file_first_only() {
    let r = res("Host a\n IdentityFile /k1\n IdentityFile /k2\n", "a");
    assert_eq!(r.identity_file, Some(PathBuf::from("/k1")));
}

#[test]
fn wildcard_matcher() {
    assert!(wildcard_match("*", "", false));
    assert!(wildcard_match("a*b*c", "aXXbYYc", false));
    assert!(!wildcard_match("a*b*c", "aXXbYY", false));
    assert!(wildcard_match("A?C", "abc", true));
    assert!(!wildcard_match("A?C", "abc", false));
    assert!(!wildcard_match("a?", "a", false));
}

#[test]
fn host_name_lowercased() {
    assert_eq!(
        res("Host Mixed\n HostName Foo.Example\n", "Mixed").host_name,
        "foo.example"
    );
    assert_eq!(res("Host Mixed\n", "Mixed").host_name, "mixed");
}
