//! zsh 提示符集成 shim 的端到端测试：真实启动 `zsh -l -i`，HOME / 数据目录全部在临时目录里，
//! 不读写用户真实的 `~/.zshrc`、`~/.zsh_history` 等文件。本机没有 zsh 时整组测试跳过。

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_pty::{
    CommandSpec, EnvOptions, EnvSnapshot, PtySession, SpawnConfig, ZshIntegration, build_env,
    install_zsh_shim,
};
use kanso_types::GridSize;

fn zsh_path() -> Option<&'static str> {
    [
        "/bin/zsh",
        "/usr/bin/zsh",
        "/usr/local/bin/zsh",
        "/opt/homebrew/bin/zsh",
    ]
    .into_iter()
    .find(|p| Path::new(p).exists())
}

/// macOS 一定带 `en_US.UTF-8`；Linux 不一定生成了它，按 local-shell.md §4 用 `C.UTF-8`。
#[cfg(target_os = "macos")]
const TEST_LANG: &str = "en_US.UTF-8";
#[cfg(not(target_os = "macos"))]
const TEST_LANG: &str = "C.UTF-8";

/// 用户没设置 HISTFILE 时 zsh 启动后的值：macOS 的 /etc/zshrc 会设成 `${ZDOTDIR:-$HOME}/.zsh_history`，
/// Linux 发行版的 zsh 一般不设（为空）。shim 只把前者改回原位置，不替用户新设 HISTFILE。
fn default_histfile(home: &Path) -> String {
    if cfg!(target_os = "macos") {
        format!("{}/.zsh_history", home.display())
    } else {
        String::new()
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        // 规范化：macOS 的临时目录在 /var -> /private/var 之下，而 zsh 的 $PWD 与系统 cwd 查询都是真实路径。
        let root = fs::canonicalize(tmp.path()).unwrap();
        let home = root.join("home");
        let data = root.join("data");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&data).unwrap();
        // shim 从 PWD 向上读 .git/HEAD，不走 git 命令。TMPDIR 落在外层仓库内时，
        // 在临时根放一个无法识别的 .git，挡住向上查到外层分支（如 wp/…）。
        let ceiling = root.join(".git");
        fs::create_dir_all(&ceiling).unwrap();
        fs::write(ceiling.join("HEAD"), "not-a-git-repository\n").unwrap();
        Fixture {
            _tmp: tmp,
            home,
            data,
        }
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn shim_dir(&self) -> PathBuf {
        self.data.join("shell-integration/zsh")
    }
}

/// 驱动一个交互式 zsh：后台线程收集输出，按提示符节拍发送命令。
struct Run {
    session: PtySession,
    buf: Arc<Mutex<Vec<u8>>>,
    /// 用来数"提示符出现了几次"的标记：集成开启时是 OSC 133 B，关闭时是测试自己设置的 PS1 文本。
    marker: &'static str,
}

impl Run {
    fn start(fx: &Fixture, prompt_enabled: bool, extra_env: &[(&str, &str)]) -> Run {
        let shim = install_zsh_shim(&fx.data).unwrap();
        let mut pairs: Vec<(String, String)> = vec![
            ("HOME".into(), fx.home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("LANG".into(), TEST_LANG.into()),
        ];
        pairs.extend(
            extra_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let base = EnvSnapshot::from_pairs(pairs);
        let changes = build_env(
            &base,
            &EnvOptions {
                term_program_version: "0.0.0-test".into(),
                locale: None,
                zsh: Some(ZshIntegration {
                    shim_dir: shim,
                    prompt_enabled,
                }),
            },
        );
        let session = PtySession::spawn(SpawnConfig {
            command: CommandSpec {
                program: zsh_path().unwrap().into(),
                args: vec!["-l".into(), "-i".into()],
            },
            env: base.apply(&changes),
            cwd: fx.home.clone(),
            size: GridSize {
                cols: 120,
                rows: 40,
            },
        })
        .unwrap();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let mut reader = session.reader().unwrap();
        let b2 = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                b2.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        let marker = if prompt_enabled {
            "\x1b]133;B\x07"
        } else {
            "KTPROMPT> "
        };
        Run {
            session,
            buf,
            marker,
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()).into_owned()
    }

    fn wait_until(&self, what: &str, f: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if f(&self.text()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("等待超时：{what}\n输出：{:?}", self.text());
    }

    /// 等到出现第 `n` 个提示符后返回。
    fn wait_prompt(&self, n: usize) {
        self.wait_until("提示符", |t| t.matches(self.marker).count() >= n);
    }

    /// 依次执行命令（每条等到下一个提示符出现再发下一条），最后 exit 并等 zsh 退出，返回全部输出。
    fn run(mut self, cmds: &[&str]) -> String {
        self.wait_prompt(1);
        for (i, c) in cmds.iter().enumerate() {
            self.session
                .writer()
                .write_all(format!("{c}\n").as_bytes())
                .unwrap();
            self.wait_prompt(i + 2);
        }
        self.session.writer().write_all(b"exit\n").unwrap();
        let st = self.session.wait().unwrap();
        assert!(st.success(), "zsh 非正常退出：{st:?}");
        // 退出后再给读线程一点时间把尾部输出收完。
        std::thread::sleep(Duration::from_millis(200));
        self.text()
    }
}

/// 去掉 CSI、OSC 和图标，只留可见文本与两行布局；图标单独断言。
fn visible(s: &str) -> String {
    visible_raw(s)
        .replace("\u{e0a0} ", "")
        .replace("\u{f07b} ", "")
        .replace("\u{e718} ", "")
}

fn visible_raw(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\x1b' && i + 1 < b.len() {
            match b[i + 1] {
                '[' => {
                    i += 2;
                    while i < b.len() && !('@'..='~').contains(&b[i]) {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                ']' => {
                    i += 2;
                    while i < b.len() && b[i] != '\x07' {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                // 两字符的 ESC 序列，如 zle 按 terminfo 的 smkx/rmkx 输出的 `ESC =` / `ESC >`
                // （Linux 的 xterm-256color 带有，macOS 上一般不出现）。
                '=' | '>' => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn directory_request(path: &Path) -> Vec<u8> {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut request = String::from("\x1b[9001~");
    for byte in path.as_os_str().as_bytes() {
        write!(request, "{byte:02x}").unwrap();
    }
    request.push('\n');
    request.into_bytes()
}

#[test]
fn directory_widget_preserves_input_cursor_history_and_updates_prompt() {
    if zsh_path().is_none() {
        return;
    }
    let fx = Fixture::new();
    fx.write(
        ".zshrc",
        "PS1='> '\nHISTFILE=$HOME/history\nHISTSIZE=100\nSAVEHIST=100\n\
         chpwd() { print -rn -- \"$PWD\" > \"$HOME/hook\"; }\n",
    );
    let target = fx.home.join("space '\" 中文\ntrailing\n");
    fs::create_dir(&target).unwrap();
    let mut run = Run::start(&fx, true, &[]);
    run.wait_prompt(1);
    run.session
        .writer()
        .write_all(b"print -r -- __KEPT__TAIL\x02\x02\x02\x02")
        .unwrap();
    run.wait_until("半行输入", |text| text.contains("__KEPT__TAIL"));
    let before = run.text();
    run.session
        .writer()
        .write_all(&directory_request(&target))
        .unwrap();
    run.wait_prompt(2);
    let changed = run.text();
    let refresh = &changed[before.len()..];
    assert!(refresh.contains("\x1b]7;file://"));
    assert!(refresh.contains("%20%27%22%20%E4%B8%AD%E6%96%87%0Atrailing%0A"));
    assert!(visible_raw(refresh).contains("\u{f07b}"));
    assert!(visible_raw(refresh).contains("中文?trailing?"));
    assert!(!refresh.contains("\x1b]133;A"));
    assert!(!refresh.contains("\x1b]133;C"));
    assert_eq!(
        fs::read(fx.home.join("hook")).unwrap(),
        target.as_os_str().as_encoded_bytes()
    );
    run.session.writer().write_all(b"MIDDLE\x05\n").unwrap();
    run.wait_prompt(3);
    assert!(visible(&run.text()).contains("__KEPT__MIDDLETAIL"));
    run.session.writer().write_all(b"exit\n").unwrap();
    assert!(run.session.wait().unwrap().success());
    let history = fs::read_to_string(fx.home.join("history")).unwrap();
    assert!(history.contains("print -r -- __KEPT__MIDDLETAIL"));
    assert!(!history.contains("9001"));
    assert!(!history.contains("cd --"));
    assert!(!history.contains("space"));
}

#[test]
fn directory_widget_missing_path_is_silent_and_preserves_state() {
    if zsh_path().is_none() {
        return;
    }
    let fx = Fixture::new();
    fx.write(
        ".zshrc",
        "PS1='> '\n\
         __test_probe() { print -rn -- \"__STATE__${PWD}|${BUFFER}|${CURSOR}|${HISTCMD}__END__\"; }\n\
         zle -N __test_probe\nbindkey '^X' __test_probe\n\
         chpwd() { print -rn -- changed > \"$HOME/hook\"; }\n",
    );
    let mut run = Run::start(&fx, true, &[]);
    run.wait_prompt(1);
    run.session.writer().write_all(b"half input\x18").unwrap();
    run.wait_until("初始状态", |text| text.contains("__END__"));
    let before = run.text();
    let state = &before[before.find("__STATE__").unwrap()..];
    let mut request = directory_request(&fx.home.join("missing"));
    request.push(0x18);
    run.session.writer().write_all(&request).unwrap();
    run.wait_until("失败后的状态", |text| {
        text.matches("__END__").count() == 2
    });
    let after = run.text();
    assert_eq!(&after[before.len()..], state);
    assert!(!fx.home.join("hook").exists());
    run.session.writer().write_all(b"\x01\x0bexit\n").unwrap();
    assert!(run.session.wait().unwrap().success());
}

macro_rules! require_zsh {
    () => {
        if zsh_path().is_none() {
            eprintln!("跳过：本机没有 zsh");
            return;
        }
    };
}

const BASE_ZSHRC: &str = "PS1='%n@%m %1~ %# '\nHISTSIZE=100\nSAVEHIST=100\n";

#[test]
fn shim_loads_user_files_and_emits_osc_sequences() {
    require_zsh!();
    let fx = Fixture::new();
    fx.write(".zshenv", "export KT_ENV=env_ok\n");
    fx.write(".zprofile", "export KT_PROFILE=profile_ok\n");
    fx.write(".zshrc", &format!("{BASE_ZSHRC}KT_RC=rc_ok\n"));
    let dir = fx.home.join("proj/中 文");
    fs::create_dir_all(&dir).unwrap();

    let out = Run::start(&fx, true, &[]).run(&[
        "echo \"VARS:$KT_ENV:$KT_PROFILE:$KT_RC:$HISTFILE:${ZDOTDIR-unset}\"",
        "false",
        &format!("cd '{}'", dir.display()),
        "true",
    ]);

    // 用户文件都生效；HISTFILE 在临时 HOME 的原位置；用户的 ZDOTDIR 本来没设置，退回未设置。
    let want = format!(
        "VARS:env_ok:profile_ok:rc_ok:{}:unset",
        default_histfile(&fx.home)
    );
    assert!(out.contains(&want), "{out:?}");

    // 第一个提示符：kanso=first，之前没有 D；其后没有 kanso=first 的 A。
    let first = out
        .find("\x1b]133;A;kanso=first\x07")
        .expect("缺少 first 标记");
    assert_eq!(out.matches("kanso=first").count(), 1);
    assert!(
        out.find("\x1b]133;D").unwrap() > first,
        "第一个提示符之前不应有 D"
    );
    // 之后的提示符：两个空行（tty 把 \n 转成 \r\n）夹着普通的 A。
    assert!(out.contains("\r\n\x1b]133;A\x07\r\n"), "{out:?}");
    // 退出码：false → 1，true → 0。
    assert!(out.contains("\x1b]133;D;1\x07"));
    assert!(out.contains("\x1b]133;D;0\x07"));
    // C 在命令执行前输出。
    assert!(out.contains("\x1b]133;C\x07"));
    // OSC 7：主目录与带空格、中文的目录（百分号编码）。
    assert!(out.contains("\x1b]7;file://"));
    assert!(out.contains("/proj/%E4%B8%AD%20%E6%96%87\x07"), "{out:?}");
    // 序列顺序：同一个提示符里 D → 7 → A。
    let d = out.find("\x1b]133;D;1\x07").unwrap();
    let seven = out[d..].find("\x1b]7;").unwrap() + d;
    let a = out[seven..].find("\x1b]133;A\x07").unwrap() + seven;
    assert!(d < seven && seven < a);
    // 提示符：目录用 ~ 缩写，用户名与主机名被去掉，`%#` 保留。
    let vis = visible(&out);
    assert!(vis.contains("~ \r\n"), "{vis:?}");
    assert!(vis.contains("~/proj/中 文 \r\n"), "{vis:?}");
    // 默认提示符只剩 `%#` 时，普通用户的输入行不显示 `%`，命令紧跟在换行后。
    assert!(vis.contains("~/proj/中 文 \r\nt"), "{vis:?}");
    assert!(out.contains("\x1b]777;kanso-chip;start\x07"));
    assert!(out.contains("\x1b]777;kanso-chip;end\x07"));
    assert!(!vis.contains('@'), "用户名@主机名没有去掉：{vis:?}");

    // shim 目录里没有生成历史文件；历史写在临时 HOME 的原位置。
    assert!(!fx.shim_dir().join(".zsh_history").exists());
    // 历史文件里含中文路径时 zsh 会写入元字符转义，不一定是合法 UTF-8，所以按字节读取。
    // 只有 HISTFILE 默认有值的平台（macOS）才会写历史。
    if !default_histfile(&fx.home).is_empty() {
        let hist =
            String::from_utf8_lossy(&fs::read(fx.home.join(".zsh_history")).unwrap_or_default())
                .into_owned();
        assert!(hist.contains("false"), "历史没有写到原位置：{hist:?}");
    }
}

#[test]
fn user_zdotdir_set_in_zshenv_is_honored() {
    require_zsh!();
    let fx = Fixture::new();
    fx.write(".zshenv", "export ZDOTDIR=$HOME/.config/zsh\nKT_ENV=1\n");
    fx.write(".config/zsh/.zprofile", "KT_PROFILE=zdot_profile\n");
    fx.write(
        ".config/zsh/.zshrc",
        &format!("{BASE_ZSHRC}KT_RC=zdot_rc\nHISTFILE=$ZDOTDIR/hist\n"),
    );
    fx.write(".config/zsh/.zlogout", "echo LOGOUT_RAN\n");
    let out = Run::start(&fx, true, &[])
        .run(&["echo \"V:$KT_ENV:$KT_PROFILE:$KT_RC:$HISTFILE:$ZDOTDIR\""]);
    let z = format!("{}/.config/zsh", fx.home.display());
    assert!(
        out.contains(&format!("V:1:zdot_profile:zdot_rc:{z}/hist:{z}")),
        "{out:?}"
    );
    assert!(out.contains("LOGOUT_RAN"));
    assert!(!fx.shim_dir().join("hist").exists());
}

#[test]
fn zdotdir_from_environment_is_preserved() {
    require_zsh!();
    let fx = Fixture::new();
    let z = fx.home.join("zdot");
    fs::create_dir_all(&z).unwrap();
    fs::write(z.join(".zshrc"), format!("{BASE_ZSHRC}KT_RC=env_zdot\n")).unwrap();
    // 家目录里放一个不该被加载的 .zshrc，证明用的是 ZDOTDIR。
    fx.write(".zshrc", "KT_RC=wrong\n");
    let out = Run::start(&fx, true, &[("ZDOTDIR", z.to_str().unwrap())])
        .run(&["echo \"V:$KT_RC:$ZDOTDIR\""]);
    assert!(
        out.contains(&format!("V:env_zdot:{}", z.display())),
        "{out:?}"
    );
}

#[test]
fn integration_can_be_disabled() {
    require_zsh!();
    let fx = Fixture::new();
    fx.write(".zshenv", "export KT_ENV=env_ok\n");
    fx.write(
        ".zshrc",
        "PS1='KTPROMPT> '\nHISTSIZE=100\nSAVEHIST=100\nKT_RC=rc_ok\n",
    );
    let out = Run::start(&fx, false, &[]).run(&["echo \"V:$KT_ENV:$KT_RC:$HISTFILE\""]);
    assert!(
        out.contains(&format!("V:env_ok:rc_ok:{}\r", default_histfile(&fx.home))),
        "{out:?}"
    );
    // 关闭后没有任何 OSC 133 / OSC 7，提示符也保持用户原样。
    assert!(!out.contains("\x1b]133;"), "{out:?}");
    assert!(!out.contains("\x1b]7;"));
    assert!(!fx.shim_dir().join(".zsh_history").exists());
}

#[test]
fn history_defaults_do_not_leak_into_shim_dir() {
    require_zsh!();
    // 用户没有自己设置 HISTFILE（macOS 的 /etc/zshrc 会设成 ${ZDOTDIR:-$HOME}/.zsh_history）。
    let fx = Fixture::new();
    fx.write(".zshrc", "PS1='%# '\nHISTSIZE=100\nSAVEHIST=100\n");
    Run::start(&fx, true, &[]).run(&["echo marker_cmd"]);
    assert!(!fx.shim_dir().join(".zsh_history").exists());
}

fn git_head(root: &Path, rel: &str, head: &str) {
    let d = root.join(rel);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("HEAD"), head).unwrap();
}

fn branch_scenario(promptsubst: bool) {
    let fx = Fixture::new();
    let zshrc = format!(
        "{BASE_ZSHRC}{}",
        if promptsubst {
            "setopt promptsubst\n"
        } else {
            ""
        }
    );
    fx.write(".zshrc", &zshrc);
    let h = &fx.home;
    // 普通仓库。
    git_head(h, "plain/.git", "ref: refs/heads/main\n");
    fs::create_dir_all(h.join("plain/src/deep")).unwrap();
    // worktree：.git 是文件，gitdir 用相对路径（相对 .git 文件所在目录）。
    git_head(
        h,
        "main-repo/.git/worktrees/wt",
        "ref: refs/heads/feature/x\n",
    );
    fs::create_dir_all(h.join("wt/sub")).unwrap();
    fs::write(
        h.join("wt/.git"),
        "gitdir: ../main-repo/.git/worktrees/wt\n",
    )
    .unwrap();
    // submodule 风格：绝对路径的 gitdir。
    git_head(
        h,
        "main-repo/.git/modules/sm",
        "ref: refs/heads/sm-branch\n",
    );
    fs::create_dir_all(h.join("sm")).unwrap();
    fs::write(
        h.join("sm/.git"),
        format!("gitdir: {}/main-repo/.git/modules/sm\n", h.display()),
    )
    .unwrap();
    // detached HEAD。
    git_head(
        h,
        "detached/.git",
        "0123456789abcdef0123456789abcdef01234567\n",
    );
    // 不认识的内容：不显示。
    git_head(h, "garbage/.git", "something else\n");
    // 注入：分支名里有 % 和命令替换。
    let flag = h.join("pwned-flag");
    git_head(
        h,
        "evil/.git",
        &format!("ref: refs/heads/evil%b%d$(touch {})\n", flag.display()),
    );
    // 分支名含控制字符（ESC）：不应原样进入输出。
    git_head(h, "esc/.git", "ref: refs/heads/a\u{1b}[31mb\n");

    let cmds: Vec<String> = [
        "plain",
        "plain/src/deep",
        "wt/sub",
        "sm",
        "detached",
        "garbage",
        "evil",
        "esc",
    ]
    .iter()
    .map(|d| format!("cd '{}/{d}'", h.display()))
    .collect();
    let refs: Vec<&str> = cmds.iter().map(String::as_str).collect();
    let out = Run::start(&fx, true, &[]).run(&refs);
    let vis = visible(&out);

    assert!(vis.contains("~/plain   main \r\n"), "{vis:?}");
    assert!(
        visible_raw(&out).contains("\u{f07b} ~/plain   \u{e0a0} main \r\n"),
        "{out:?}"
    );
    assert!(vis.contains("~/plain/src/deep   main \r\n"), "{vis:?}");
    assert!(vis.contains("~/wt/sub   feature/x \r\n"), "{vis:?}");
    assert!(vis.contains("~/sm   sm-branch \r\n"), "{vis:?}");
    assert!(vis.contains("~/detached   0123456 \r\n"), "{vis:?}");
    assert!(vis.contains("~/garbage \r\n"), "{vis:?}");
    assert!(
        vis.contains(&format!(
            "~/evil   evil%b%d$(touch {}) \r\n",
            flag.display()
        )),
        "{vis:?}"
    );
    // 分支名里的 ESC 被丢弃，红色 SGR 不会出现在输出里（视觉文本里 "a[31mb" 被当成普通字符）。
    assert!(vis.contains("~/esc   a[31mb \r\n"), "{vis:?}");
    assert!(!out.contains("a\x1b[31mb"));
    assert!(
        !flag.exists(),
        "分支名里的命令替换被执行了（promptsubst={promptsubst}）"
    );
}

#[test]
fn git_branch_display_and_injection_safety() {
    require_zsh!();
    branch_scenario(false);
}

#[test]
fn git_branch_injection_safety_with_promptsubst() {
    require_zsh!();
    branch_scenario(true);
}

#[test]
fn node_chip_requires_project_and_command_and_caches_directory_and_path() {
    use std::os::unix::fs::PermissionsExt;

    require_zsh!();
    let fx = Fixture::new();
    fx.write(".zshrc", "setopt promptsubst\nPS1='%~ %# '\nHISTSIZE=0\n");
    fx.write("project/package.json", "{}");
    fx.write("project/sub/keep", "");
    fx.write("plain/keep", "");
    fx.write("empty-bin/keep", "");
    git_head(&fx.home, "project/nested/.git", "ref: refs/heads/nested\n");
    fx.write("project/nested/sub/keep", "");
    fs::write(fx.home.parent().unwrap().join("package.json"), "{}").unwrap();
    for (bin, version) in [("bin1", "v26.13.0"), ("bin2", "v26.14.0")] {
        let file = format!("{bin}/node");
        fx.write(
            &file,
            &format!(
                "#!/bin/sh\nprintf 'call\\n' >> \"$HOME/node-calls\"\nprintf '{version}\\n'\n"
            ),
        );
        fs::set_permissions(fx.home.join(file), fs::Permissions::from_mode(0o755)).unwrap();
    }
    // PATH 要在 shell 启动后再设：macOS 的 /etc/zprofile 会用 path_helper 把系统目录排到前面，
    // 机器上装了真 node（如 GitHub CI 的 /usr/local/bin/node）时会盖住测试用的假 node。
    // 第一条命令在 $HOME 执行，没有 package.json，不会触发 node 查询。
    let out = Run::start(&fx, true, &[]).run(&[
        "PATH=$HOME/bin1:/usr/bin:/bin",
        "cd project/sub",
        ":",
        "PATH=\"$HOME/bin2:$PATH\"",
        ":",
        "cd ../../plain",
        "cd ../project/nested/sub",
        "cd ~",
        "PATH=$HOME/empty-bin; cd project",
    ]);
    let prompts: Vec<_> = out.split("\x1b]133;B\x07").take(10).collect();
    for (index, prompt) in prompts.iter().enumerate() {
        assert_eq!(
            prompt.contains('\u{e718}'),
            (2..=5).contains(&index),
            "prompt {index}: {prompt:?}"
        );
    }
    assert!(out.contains("v26.13.0"));
    assert!(out.contains("v26.14.0"));
    assert_eq!(
        fs::read_to_string(fx.home.join("node-calls")).unwrap(),
        "call\ncall\n"
    );
}

#[test]
fn prompt_merging_rules() {
    require_zsh!();
    let fx = Fixture::new();
    fx.write(".zshrc", "PS1='%n@%m %~ %# '\nHISTSIZE=0\n");
    let out = Run::start(&fx, true, &[]).run(&[]);
    let vis = visible(&out);
    assert!(vis.contains("~ \r\n"), "{vis:?}");
    assert!(!vis.contains('@'), "用户名与主机名没有去掉：{vis:?}");

    // 用户用 $PWD 的原表达式保留在输入行，分支只放在标签行。
    let fx = Fixture::new();
    fx.write(
        ".zshrc",
        "setopt promptsubst\nPS1='[$PWD] %# '\nHISTSIZE=0\n",
    );
    git_head(&fx.home, "r/.git", "ref: refs/heads/dev\n");
    let out = Run::start(&fx, true, &[]).run(&["cd r"]);
    let vis = visible(&out);
    assert!(
        vis.contains(&format!("[{}] % ", fx.home.display())),
        "{vis:?}"
    );
    assert!(
        vis.contains(&format!("~/r   dev \r\n[{}/r] % ", fx.home.display())),
        "{vis:?}"
    );

    // oh-my-zsh robbyrussell 一类主题用 %c（目录最后一段）：也要去掉，否则目录重复显示。
    let fx = Fixture::new();
    fx.write(".zshrc", "PS1='%c ➜ '\nHISTSIZE=0\n");
    fs::create_dir_all(fx.home.join("repo")).unwrap();
    let out = Run::start(&fx, true, &[]).run(&["cd repo"]);
    let vis = visible(&out);
    assert!(vis.contains("~/repo \r\n➜ "), "{vis:?}");
    assert!(!vis.contains("repo repo"), "目录重复显示：{vis:?}");
}

#[test]
fn long_directory_is_truncated_to_whole_segments() {
    require_zsh!();
    let fx = Fixture::new();
    fx.write(".zshrc", "PS1='%# '\nHISTSIZE=0\n");
    let deep = fx.home.join("aaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbb/cccccccccccccccccccc/dddddddddd/eeeeeeeeeeeeeeee");
    fs::create_dir_all(&deep).unwrap();
    let out = Run::start(&fx, true, &[]).run(&[&format!("cd '{}'", deep.display())]);
    let vis = visible(&out);
    assert!(
        vis.contains(".../cccccccccccccccccccc/dddddddddd/eeeeeeeeeeeeeeee \r\n"),
        "{vis:?}"
    );
    // 120 列终端阈值 60 字符：只保留尾部完整的路径段，不会出现被截断一半的段，也不会保留 bbb 段。
    // 只检查提示符标签行（以 ` \r\n` 结尾）；zsh 回显的 `cd '…'` 命令本身含完整路径，不能拿来断言。
    assert!(
        !vis.contains("bbbbbbbbbbbbbbbbbbbb/cccccccccccccccccccc/dddddddddd/eeeeeeeeeeeeeeee \r\n"),
        "{vis:?}"
    );
}

#[test]
fn ps1_rewritten_by_hooks_is_recomputed_without_duplication() {
    require_zsh!();
    let fx = Fixture::new();
    // 模拟主题：每次 precmd 都重设 PS1；hook 里还故意执行 false，验证退出码不被它冲掉。
    fx.write(
        ".zshrc",
        "HISTSIZE=0\nmytheme() { false; PS1='THEME %n@%m %~ %# ' }\nautoload -Uz add-zsh-hook\nadd-zsh-hook precmd mytheme\n",
    );
    git_head(&fx.home, "r/.git", "ref: refs/heads/dev\n");
    let out = Run::start(&fx, true, &[]).run(&["false", "cd r", "true"]);
    let vis = visible(&out);
    assert!(vis.contains("~/r   dev \r\nTHEME % "), "{vis:?}");
    assert_eq!(vis.matches("~/r ~/r").count(), 0, "目录重复叠加：{vis:?}");
    assert!(
        out.contains("\x1b]133;D;1\x07"),
        "退出码被用户 hook 冲掉：{out:?}"
    );

    // 一次性改写：在我们的 PS1 之前叠加 `(venv) `，目录与分支不能重复。
    let fx = Fixture::new();
    fx.write(".zshrc", "PS1='%~ %# '\nHISTSIZE=0\n");
    git_head(&fx.home, "r/.git", "ref: refs/heads/dev\n");
    let out = Run::start(&fx, true, &[]).run(&["cd r", "PS1=\"(venv) $PS1\"", "cd .."]);
    let vis = visible(&out);
    assert!(vis.contains("~ \r\n(venv) e"), "{vis:?}");
    assert!(!vis.contains("(venv) ~"), "{vis:?}");
}

#[test]
fn transient_prompt_compacts_chips_on_command_submit() {
    use std::os::unix::fs::PermissionsExt;

    require_zsh!();
    for promptsubst in [false, true] {
        let fx = Fixture::new();
        let rc = if promptsubst {
            "setopt promptsubst\nPS1='%~ %# '\nHISTSIZE=0\n"
        } else {
            "PS1='%~ %# '\nHISTSIZE=0\n"
        };
        fx.write(".zshrc", rc);
        fx.write("repo/package.json", "{}");
        git_head(&fx.home, "repo/.git", "ref: refs/heads/main\n");
        fx.write("bin/node", "#!/bin/sh\nprintf 'v26.13.0\\n'\n");
        fs::set_permissions(fx.home.join("bin/node"), fs::Permissions::from_mode(0o755)).unwrap();

        let out =
            Run::start(&fx, true, &[]).run(&["PATH=$HOME/bin:/usr/bin:/bin", "cd repo", "echo ok"]);
        let vis = visible_raw(&out);
        assert!(
            vis.contains("~/repo git:(main) node:v26.13.0\r\n"),
            "提交命令后未重绘为紧凑纯文本历史行（promptsubst={promptsubst}）：{vis:?}"
        );
    }
}
