//! 会话集成测试的公共设施。
//!
//! 隔离约定：`HOME`、数据目录全部是临时目录，子进程环境用 `ManagerConfig::base_env` 显式给定
//! （不继承本进程环境，更不读开发者真实的 `~/.zshrc` 等文件）。测试用 shell 是 `/bin/sh -i`
//! （带参数所以不会被自动加 `-l`，也就不读 `/etc/profile`、`~/.profile`）。

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use kanso_frame::{CELL_WIDE_SPACER, Frame};
use kanso_session::{
    EnvSnapshot, LocalSession, ManagerConfig, OpenRequest, SessionHost, SessionManager,
};
use kanso_types::event::SessionState;
use kanso_types::{CoreEvent, GridSize, SessionId};

pub const TIMEOUT: Duration = Duration::from_secs(20);

/// 记录全部事件与后台命令结束通知，并支持按条件等待。
#[derive(Default)]
pub struct Recorder {
    events: Mutex<Vec<CoreEvent>>,
    cond: Condvar,
    pub finished: Mutex<Vec<(SessionId, Option<i32>)>>,
    /// AI 轮次结束的后台通知。
    pub agent_done: Mutex<Vec<(SessionId, kanso_types::agent::AgentKind)>>,
}

impl SessionHost for Recorder {
    fn emit(&self, event: CoreEvent) {
        self.events.lock().unwrap().push(event);
        self.cond.notify_all();
    }

    fn command_finished_in_background(&self, session: SessionId, exit_code: Option<i32>) {
        self.finished.lock().unwrap().push((session, exit_code));
    }

    fn agent_turn_done_in_background(
        &self,
        session: SessionId,
        agent: kanso_types::agent::AgentKind,
    ) {
        self.agent_done.lock().unwrap().push((session, agent));
    }
}

impl Recorder {
    pub fn all(&self) -> Vec<CoreEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn count(&self, pred: impl Fn(&CoreEvent) -> bool) -> usize {
        self.all().iter().filter(|e| pred(e)).count()
    }

    /// 等到事件序列里（任意位置）出现满足条件的事件。
    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&CoreEvent) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.events.lock().unwrap();
        loop {
            if guard.iter().any(&pred) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            guard = self.cond.wait_timeout(guard, deadline - now).unwrap().0;
        }
    }
}

pub fn is_closed(e: &CoreEvent) -> bool {
    matches!(
        e,
        CoreEvent::SessionStateChanged {
            state: SessionState::Closed(_),
            ..
        }
    )
}

pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub home: PathBuf,
    pub rec: Arc<Recorder>,
    pub mgr: SessionManager,
}

pub fn fixture(shell: &str, integration: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    // zsh 集成从 PWD 向上读 .git；TMPDIR 在外层仓库内时，用无法识别的 .git 挡住外层分支。
    let ceiling = tmp.path().join(".git");
    std::fs::create_dir_all(&ceiling).unwrap();
    std::fs::write(ceiling.join("HEAD"), "not-a-git-repository\n").unwrap();
    let rec = Arc::new(Recorder::default());
    #[cfg(windows)]
    let path = std::env::var("PATH").unwrap_or_default();
    #[cfg(not(windows))]
    let path = "/usr/bin:/bin:/usr/sbin:/sbin".to_owned();
    #[allow(unused_mut)]
    let mut env = EnvSnapshot::from_pairs([
        ("PATH", path),
        ("HOME", home.to_string_lossy().into_owned()),
        ("USER", "kanso-test".to_owned()),
    ]);
    // Windows 的 cmd.exe / PowerShell 需要系统变量才能正常启动。
    #[cfg(windows)]
    for key in ["SystemRoot", "ComSpec", "windir", "TEMP", "TMP"] {
        if let Ok(v) = std::env::var(key) {
            env = env.apply(&[kanso_pty::EnvChange::set(key, v)]);
        }
    }
    let mgr = SessionManager::new(
        ManagerConfig {
            data_dir: tmp.path().join("data"),
            app_version: "0.0.0-test".to_owned(),
            preferred_language: "en".to_owned(),
            region: None,
            default_shell: Some(shell.to_owned()),
            shell_integration: integration,
            base_env: Some(env),
        },
        Arc::clone(&rec) as Arc<dyn SessionHost>,
    );
    Fixture {
        tmp,
        home,
        rec,
        mgr,
    }
}

pub fn sh_fixture() -> Fixture {
    fixture("/bin/sh -i", false)
}

pub const GRID: GridSize = GridSize { cols: 80, rows: 24 };

impl Fixture {
    pub fn open(&self) -> (SessionId, Arc<LocalSession>) {
        let id = self.mgr.open(OpenRequest::DefaultLocalShell, GRID).unwrap();
        (id, self.mgr.get(id).unwrap())
    }
}

/// 把帧里所有行拼成文本（行之间用换行，空单元格当空格，宽字符占位格跳过）。
pub fn frame_text(frame: &Frame) -> String {
    let view = frame.view().unwrap();
    let mut out = String::new();
    for r in 0..view.header().rows {
        for cell in view.row(r) {
            if cell.flags & CELL_WIDE_SPACER != 0 {
                continue;
            }
            match view.extra(cell).and_then(|e| view.grapheme(e)) {
                Some(g) => out.push_str(g),
                None => out.push(if cell.ch == 0 {
                    ' '
                } else {
                    char::from_u32(cell.ch).unwrap_or('?')
                }),
            }
        }
        out.push('\n');
    }
    out
}

/// 轮询取帧，直到屏幕文本满足条件。`take_frame` 没有新帧时返回 `None`，屏幕文本沿用上一次取到的。
pub struct Screen {
    session: Arc<LocalSession>,
    reuse: Option<Frame>,
    pub text: String,
}

impl Screen {
    pub fn new(session: &Arc<LocalSession>) -> Screen {
        Screen {
            session: Arc::clone(session),
            reuse: None,
            text: String::new(),
        }
    }

    pub fn refresh(&mut self) {
        if let Some(f) = self.session.take_frame(&mut self.reuse) {
            self.text = frame_text(&f);
            self.reuse = Some(f);
        }
    }

    pub fn wait_for(&mut self, timeout: Duration, pred: impl Fn(&str) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.refresh();
            if pred(&self.text) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_contains(&mut self, needle: &str) -> bool {
        self.wait_for(TIMEOUT, |t| t.contains(needle))
    }
}

/// 进程是否还存在（`kill -0`）。用外部命令而不是 libc，因为本 crate 禁止 unsafe。
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
