//! 与本机 `ssh -G` 对照的集成测试。
//!
//! 约束：必须用 `-F` 指定临时配置（否则会读系统与用户真实配置）；所有 `Include` 写绝对路径
//! （相对路径会以真实 `~/.ssh` 为基准）；没有 `ssh` 可执行文件时跳过。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use kanso_sshconfig::{Env, StdFs, resolve, scan};

fn ssh_g(conf: &Path, host: &str) -> Option<HashMap<String, Vec<String>>> {
    let out = Command::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(conf)
        .arg(host)
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "ssh -G 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((k, v)) = line.split_once(' ') {
            map.entry(k.to_string()).or_default().push(v.to_string());
        }
    }
    Some(map)
}

fn first(m: &HashMap<String, Vec<String>>, k: &str) -> Option<String> {
    m.get(k).and_then(|v| v.first()).cloned()
}

/// 用空配置取 ssh 认定的本地用户名，避免自己猜。
fn local_user(dir: &Path) -> Option<String> {
    let empty = dir.join("empty");
    std::fs::write(&empty, "").unwrap();
    first(&ssh_g(&empty, "zzz-probe")?, "user")
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn compare(conf: &Path, env: &Env, extra_hosts: &[&str]) {
    let mut hosts: Vec<String> = scan(conf, &StdFs, env)
        .hosts
        .into_iter()
        .map(|h| h.alias)
        .collect();
    hosts.extend(extra_hosts.iter().map(|s| s.to_string()));
    assert!(!hosts.is_empty());
    for h in &hosts {
        let ours = resolve(h, conf, &StdFs, env);
        let g = ssh_g(conf, h).unwrap();
        assert_eq!(
            Some(ours.host_name.clone()),
            first(&g, "hostname"),
            "hostname of {h}"
        );
        assert_eq!(
            Some(ours.port.to_string()),
            first(&g, "port"),
            "port of {h}"
        );
        assert_eq!(Some(ours.user.clone()), first(&g, "user"), "user of {h}");
        assert_eq!(ours.proxy_jump, first(&g, "proxyjump"), "proxyjump of {h}");
        // 没有显式 IdentityFile 时 ssh 会列出默认列表，只比较显式写了的情形。
        if let Some(id) = &ours.identity_file {
            assert_eq!(
                Some(id.to_string_lossy().into_owned()),
                first(&g, "identityfile"),
                "identityfile of {h}"
            );
        }
    }
}

fn setup() -> Option<(tempfile::TempDir, Env)> {
    if Command::new("ssh").arg("-V").output().is_err() {
        eprintln!("跳过：本机没有 ssh 可执行文件");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let user = local_user(dir.path())?;
    let env = Env {
        // home 指向临时目录：默认私钥探测不会碰到真实 ~/.ssh。
        home: dir.path().join("home"),
        local_user: user,
        local_host: "localhost".into(),
        local_uid: 0,
    };
    Some((dir, env))
}

#[test]
fn acceptance_config_matches_ssh() {
    let Some((dir, env)) = setup() else { return };
    let d = dir.path();
    let cd = d.join("config.d");
    write(
        &cd.join("10-db.conf"),
        "Host db\n  HostName db.internal\n  Port 5432\n",
    );
    write(&cd.join("05-web.conf"), "Host web1 web2\n  User deploy\n");
    write(
        &cd.join("20-late.conf"),
        "Host db\n  User ignored-late\n  Port 1\n",
    );
    let key = d.join("keys").join("id_x");
    let conf = d.join("config");
    write(
        &conf,
        &format!(
            "Host *\n  User defuser\n  ServerAliveInterval 30\n\
             Include {inc}\n\
             Host web1\n  HostName=w1.example.com\n  Port = 2222\n  IdentityFile {key}\n  IdentityFile /second\n\
             Host *.example.com !secret.example.com\n  User wild\n\
             Host secret.example.com\n  HostName 10.0.0.9\n\
             Host app\n  ProxyJump j1,bob@j2:2200,j3\n  HostName %h.corp\n\
             Host solo\n  ProxyJump none\n\
             Host nouser\n  User \"quoted user\"\n\
             Host Mixed\n  hostname CaseTest\n",
            inc = cd.join("*.conf").display(),
            key = key.display(),
        ),
    );
    compare(
        &conf,
        &env,
        &[
            "w1.example.com",
            "other.example.com",
            "secret.example.com",
            "unlisted",
        ],
    );
}

#[test]
fn include_scoping_matches_ssh() {
    let Some((dir, env)) = setup() else { return };
    let d = dir.path();
    let inc = d.join("inc");
    write(&inc, "Host b\n  User fromb\n  Port 2002\n");
    let conf = d.join("config");
    // Host 块内引入带 Host 的文件，之后的行属于外层块；顶层引入后的行对所有主机生效。
    write(
        &conf,
        &format!(
            "Host a\n  Include {i}\n  Port 1111\n\
             Host c\n  Include {i}\n  User cuser\n\
             Include {i}\n  HostName global.example\n",
            i = inc.display()
        ),
    );
    compare(&conf, &env, &["b", "zzz"]);
}

#[test]
fn std_fs_glob_is_sorted_and_literal_dir() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["b.conf", "a.conf", "c.txt", ".hidden.conf"] {
        write(&dir.path().join(n), "");
    }
    use kanso_sshconfig::ConfigFs;
    let got: Vec<PathBuf> = StdFs.glob(&dir.path().join("*.conf"));
    assert_eq!(
        got,
        vec![dir.path().join("a.conf"), dir.path().join("b.conf")]
    );
}
