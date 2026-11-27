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
    // 之后的提示符紧接发出普通 A，不再额外插入空行。
    assert!(out.contains("\x1b]133;A\x07"), "{out:?}");
    assert!(!out.contains("\r\n\x1b]133;A\x07\r\n"), "{out:?}");
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
    // 保持用户原有的 PS1 样式不变，不注入 chip 序列。
    let vis = visible(&out);
    assert!(vis.contains('@'), "用户原有的 PS1 应保持不变：{vis:?}");
    assert!(!out.contains("\x1b]777;kanso-chip;"));

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

#[test]
fn ps1_rewritten_by_hooks_keeps_user_ps1_and_bmark() {
    require_zsh!();
    let fx = Fixture::new();
    // 模拟主题：每次 precmd 都重设 PS1；hook 里还故意执行 false，验证退出码不被它冲掉。
    fx.write(
        ".zshrc",
        "HISTSIZE=0\nmytheme() { false; PS1='THEME %~ %# ' }\nautoload -Uz add-zsh-hook\nadd-zsh-hook precmd mytheme\n",
    );
    fs::create_dir_all(fx.home.join("r")).unwrap();
    let out = Run::start(&fx, true, &[]).run(&["false", "cd r", "true"]);
    let vis = visible(&out);
    assert!(vis.contains("THEME ~/r % "), "{vis:?}");
    assert!(
        out.contains("\x1b]133;D;1\x07"),
        "退出码被用户 hook 冲掉：{out:?}"
    );
}
