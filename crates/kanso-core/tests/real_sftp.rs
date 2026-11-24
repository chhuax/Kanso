//! 真实 core 的 SFTP 集成测试（T7b）：纯 SFTP 会话、文件操作、传输与事件。
//!
//! 服务器是进程内的 russh 测试服务器 + SFTP 子系统（`kanso_ssh::test_server` / `test_sftp_server`），
//! 远端文件系统是临时目录，本地文件一律放临时目录。只连 127.0.0.1；HOME、数据目录、known_hosts
//! 都是临时目录，不读写真实的 `~/.ssh`，不碰真实钥匙串，不连 ssh-agent。
//!
//! 异步方法在一个**没有 IO / 时间驱动**的 current_thread 运行时上驱动，模拟 FFI 侧执行器
//! （那里没有 tokio 计时器）：core 必须自己把工作切到它的运行时，否则这里会 panic。

#![cfg(unix)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_core::{
    Core, CoreConfig, CoreError, EventSink, OpenTarget, Platform, SearchQuery, SecretKind,
};
use kanso_secret::{MemorySecretStore, SecretRef, SecretStore};
use kanso_ssh::test_server::{Policy, ServerConfig, TestServer};
use kanso_ssh::test_sftp_server::SftpServerConfig;
use kanso_ssh::{HostKeyStore, KnownHostsFile};
use kanso_types::event::{CloseReason, SessionState, TransferDirection, TransferOutcome};
use kanso_types::fs::DirEntry;
use kanso_types::message::{Message, Reason};
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Profile, ProfileKind, SessionKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::transfer::ConflictPolicy;
use kanso_types::transfer::XferProtocol;
use kanso_types::{
    CellSide, CoreEvent, GridSize, Key, KeyEvent, KeyEventKind, Modifiers, PasteContent, ProfileId,
    SessionId, TransferId, ViewportPoint,
};

const TIMEOUT: Duration = Duration::from_secs(30);
const GRID: GridSize = GridSize { cols: 80, rows: 24 };
const HOME: &str = "/home/tester";

/// 带接收时间的事件收集器：验证进度事件的最小间隔需要时间戳。
#[derive(Default)]
struct TimedSink(Mutex<Vec<(Instant, CoreEvent)>>);

impl EventSink for TimedSink {
    fn on_event(&self, event: CoreEvent) {
        self.0.lock().unwrap().push((Instant::now(), event));
    }
}

struct Env {
    // 字段顺序即 drop 顺序：先关 core（断开会话），再停服务器与运行时。
    core: Core,
    sink: Arc<TimedSink>,
    log: Mutex<Vec<(Instant, CoreEvent)>>,
    servers: Vec<TestServer>,
    /// 与 `servers` 一一对应的远端文件系统根（临时目录）。
    roots: Vec<tempfile::TempDir>,
    secrets: Arc<MemorySecretStore>,
    rt: tokio::runtime::Runtime,
    /// 本地侧（下载目标 / 上传源）。
    local: tempfile::TempDir,
    _tmp: tempfile::TempDir,
}

fn target(port: u16) -> SshTarget {
    SshTarget {
        host: "127.0.0.1".into(),
        port,
        user: "tester".into(),
        auth: AuthMethod::Password,
        private_key_path: None,
        jump: None,
        proxy_command: None,
        keepalive: kanso_types::profile::KeepaliveConfig::default(),
    }
}

fn ssh_profile(name: &str, t: SshTarget) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group: None,
        kind: ProfileKind::Ssh(SshProfile {
            target: t,
            encoding: EncodingLabel::new(""),
            locale: None,
        }),
    }
}

fn sftp_profile(name: &str, t: SshTarget) -> Profile {
    Profile {
        kind: ProfileKind::Sftp(t),
        ..ssh_profile(name, target(0))
    }
}

/// 可复现的伪随机内容：不同偏移内容不同，才能发现块错位 / 乱序。
fn pattern(len: usize) -> Vec<u8> {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn key_event() -> KeyEvent {
    KeyEvent {
        key: Key::Character("a".into()),
        mods: Modifiers::NONE,
        text: Some("a".into()),
        kind: KeyEventKind::Press,
    }
}

impl Env {
    fn new() -> Env {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let sink = Arc::new(TimedSink::default());
        let secrets = Arc::new(MemorySecretStore::new());
        let known_hosts = Arc::new(KnownHostsFile::new(tmp.path().join("known_hosts")));
        let mut platform = Platform::for_tests(sink.clone());
        platform.secrets = secrets.clone();
        platform.host_keys = Some(known_hosts as Arc<dyn HostKeyStore>);
        let core = Core::new_with_base_env(
            CoreConfig::new(tmp.path().join("data"), "0.0.0-test", "test.kanso"),
            platform,
            vec![("HOME".into(), home.to_string_lossy().into_owned())],
        )
        .unwrap();
        Env {
            core,
            sink,
            log: Default::default(),
            servers: Vec::new(),
            roots: Vec::new(),
            secrets,
            rt,
            local: tempfile::tempdir().unwrap(),
            _tmp: tmp,
        }
    }

    /// 启动带 SFTP 子系统的服务器，返回下标。
    fn start_sftp(&mut self, tweak: impl FnOnce(&mut SftpServerConfig)) -> usize {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(HOME.trim_start_matches('/'))).unwrap();
        let mut sftp = SftpServerConfig::new(root.path());
        sftp.home = HOME.into();
        tweak(&mut sftp);
        let server = self.rt.block_on(TestServer::start(ServerConfig {
            policy: Policy {
                password: Some("pw".into()),
                ..Policy::default()
            },
            sftp: Some(sftp),
            ..ServerConfig::default()
        }));
        self.servers.push(server);
        self.roots.push(root);
        self.servers.len() - 1
    }

    /// 远端虚拟路径对应的真实路径（第 `i` 台服务器）。
    fn remote(&self, i: usize, p: &str) -> PathBuf {
        self.roots[i].path().join(p.trim_start_matches('/'))
    }

    fn local(&self, p: &str) -> PathBuf {
        self.local.path().join(p)
    }

    fn save(&self, p: &Profile) {
        self.core.save_profile(p.clone()).unwrap();
        self.secrets
            .set(
                &SecretRef {
                    profile: p.id,
                    kind: SecretKind::Password,
                },
                &SecretString::new("pw"),
            )
            .unwrap();
    }

    /// 保存并以 SFTP Profile 打开，等到 Connected。
    fn open_sftp(&self, server: usize) -> SessionId {
        let p = sftp_profile("files", target(self.servers[server].port));
        self.save(&p);
        let id = self
            .core
            .open_session(OpenTarget::Profile(p.id), GRID)
            .unwrap();
        self.wait_connected(id);
        id
    }

    /// 保存并以 SSH Profile 打开，等到 Connected。
    fn open_ssh(&self, server: usize) -> SessionId {
        let p = ssh_profile("shell", target(self.servers[server].port));
        self.save(&p);
        let id = self
            .core
            .open_session(OpenTarget::Profile(p.id), GRID)
            .unwrap();
        self.wait_connected(id);
        id
    }

    fn events(&self) -> Vec<(Instant, CoreEvent)> {
        let mut log = self.log.lock().unwrap();
        log.extend(self.sink.0.lock().unwrap().drain(..));
        log.clone()
    }

    fn wait_until(&self, what: &str, mut cond: impl FnMut(&[(Instant, CoreEvent)]) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let ev = self.events();
            if cond(&ev) {
                return;
            }
            assert!(Instant::now() < deadline, "等不到：{what}\n{ev:#?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_connected(&self, id: SessionId) {
        self.wait_until("Connected", |ev| {
            ev.iter()
                .any(|(_, e)| state_of(e, id) == Some(SessionState::Connected))
        });
    }

    fn wait_closed(&self, id: SessionId) -> CloseReason {
        self.wait_until("Closed", |ev| {
            ev.iter().any(|(_, e)| closed_of(e, id).is_some())
        });
        self.events()
            .iter()
            .find_map(|(_, e)| closed_of(e, id).cloned())
            .unwrap()
    }

    /// 在无 IO / 时间驱动的运行时上跑 core 的异步方法（模拟 FFI 执行器）。
    fn block<T>(&self, fut: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// 取完整列表（逐批读完）。
    fn list(&self, id: SessionId, path: &str) -> Result<Vec<DirEntry>, CoreError> {
        let mut stream = self.core.sftp_list(id, path)?;
        let mut all = Vec::new();
        while let Some(batch) = self.block(stream.next_batch())? {
            all.extend(batch);
        }
        Ok(all)
    }

    /// 某次传输的全部事件（保序）。
    fn transfer_events(&self, t: TransferId) -> Vec<(Instant, CoreEvent)> {
        self.events()
            .into_iter()
            .filter(|(_, e)| match e {
                CoreEvent::TransferStarted { transfer, .. }
                | CoreEvent::TransferProgress { transfer, .. }
                | CoreEvent::TransferFinished { transfer, .. } => *transfer == t,
                _ => false,
            })
            .collect()
    }

    fn wait_finished(&self, t: TransferId) -> TransferOutcome {
        self.wait_until("TransferFinished", |ev| {
            ev.iter().any(|(_, e)| finished_of(e, t).is_some())
        });
        self.events()
            .iter()
            .find_map(|(_, e)| finished_of(e, t).cloned())
            .unwrap()
    }

    /// 等到该传输至少有一条带已传字节的 Progress。
    fn wait_progress(&self, t: TransferId) {
        self.wait_until("Progress", |ev| {
            ev.iter().any(|(_, e)| {
                matches!(e, CoreEvent::TransferProgress { transfer, done_bytes, .. }
                    if *transfer == t && *done_bytes > 0)
            })
        });
    }
}

fn state_of(e: &CoreEvent, id: SessionId) -> Option<SessionState> {
    match e {
        CoreEvent::SessionStateChanged { session, state } if *session == id => Some(state.clone()),
        _ => None,
    }
}

fn closed_of(e: &CoreEvent, id: SessionId) -> Option<&CloseReason> {
    match e {
        CoreEvent::SessionStateChanged {
            session,
            state: SessionState::Closed(r),
        } if *session == id => Some(r),
        _ => None,
    }
}

fn finished_of(e: &CoreEvent, t: TransferId) -> Option<&TransferOutcome> {
    match e {
        CoreEvent::TransferFinished { transfer, outcome } if *transfer == t => Some(outcome),
        _ => None,
    }
}

fn is_transfer_event(e: &CoreEvent) -> bool {
    matches!(
        e,
        CoreEvent::TransferStarted { .. }
            | CoreEvent::TransferProgress { .. }
            | CoreEvent::TransferFinished { .. }
    )
}

fn names(entries: &[DirEntry]) -> Vec<String> {
    let mut v: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
    v.sort();
    v
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

/// 慢服务器：每个请求延迟，让传输持续足够久以便观察进度与取消。
fn slow(cfg: &mut SftpServerConfig) {
    cfg.delay = Duration::from_millis(40);
}

// ---- 纯 SFTP 会话 ----

#[test]
fn sftp_profile_connects_without_shell_and_rejects_terminal_ops() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);

    let info = env.core.session_info(id).unwrap();
    assert_eq!(info.kind, SessionKind::Sftp);
    assert_eq!(info.state, SessionState::Connected);
    // 没有 shell：服务器没有收到 pty 请求，也没有开 shell。
    env.servers[s].stats.with(|o| {
        assert_eq!(o.shells, 0);
        assert!(o.ptys.is_empty());
    });

    let ns = Err(CoreError::NotSupported);
    assert_eq!(env.core.key(id, key_event()), ns);
    assert_eq!(env.core.text_input(id, "x"), ns);
    assert_eq!(env.core.paste(id, PasteContent::Text("x".into())), ns);
    assert_eq!(env.core.resize(id, GRID, 8, 16), ns);
    assert_eq!(env.core.focus(id, true), ns);
    assert_eq!(
        env.core.scroll(
            id,
            1.0,
            ViewportPoint {
                col: 0,
                row: 0,
                side: CellSide::Left
            },
            Modifiers::NONE
        ),
        ns
    );
    assert_eq!(env.core.select_all(id), ns);
    assert_eq!(env.core.clear_buffer(id), ns);
    assert_eq!(
        env.core
            .search(
                id,
                SearchQuery {
                    text: "x".into(),
                    case_sensitive: false,
                    regex: false
                }
            )
            .map(|_| ()),
        ns
    );
    assert_eq!(
        env.block(env.core.working_directory(id)),
        Err(CoreError::NotSupported)
    );
    assert!(env.core.take_frame(id, &mut None).is_none());
    // 没有终端帧，也不该有唤醒事件。
    assert!(
        !env.events()
            .iter()
            .any(|(_, e)| matches!(e, CoreEvent::TerminalDirty { .. }))
    );
}

#[test]
fn sftp_session_is_marked_closed_when_connection_drops() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    // 客户端经一个可以随时掐断的 TCP 中继连接服务器。
    let server_port = env.servers[s].port;
    let (relay_port, relay) = env.rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut a, _) = listener.accept().await.unwrap();
            let mut b = tokio::net::TcpStream::connect(("127.0.0.1", server_port))
                .await
                .unwrap();
            let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
        });
        (port, task)
    });
    let p = sftp_profile("files", target(relay_port));
    env.save(&p);
    let id = env
        .core
        .open_session(OpenTarget::Profile(p.id), GRID)
        .unwrap();
    env.wait_connected(id);
    assert_eq!(env.list(id, HOME).unwrap().len(), 0);

    relay.abort();
    let reason = env.wait_closed(id);
    match reason {
        CloseReason::Error(m) => assert_eq!(m.key(), "ssh-connection-lost"),
        other => panic!("期望连接丢失，实际 {other:?}"),
    }
    // 之后的 SFTP 操作返回错误而不是挂起。
    assert!(env.list(id, HOME).is_err());
}

#[test]
fn user_close_of_sftp_session_is_by_user() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    env.core.close_session(id);
    assert_eq!(env.wait_closed(id), CloseReason::ByUser);
    assert!(env.core.session_info(id).is_none());
}

#[test]
fn duplicate_sftp_session_shares_connection_and_stays_sftp() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let dup = env
        .core
        .open_session(OpenTarget::Duplicate(id), GRID)
        .unwrap();
    env.wait_connected(dup);
    assert_eq!(env.core.session_info(dup).unwrap().kind, SessionKind::Sftp);
    env.servers[s].stats.with(|o| assert_eq!(o.accepted, 1));
    std::fs::write(env.remote(s, "/home/tester/x.txt"), b"x").unwrap();
    assert_eq!(names(&env.list(dup, HOME).unwrap()), ["x.txt"]);
}

#[test]
fn shell_and_sftp_work_together_through_a_jump_host() {
    let mut env = Env::new();
    let j = env.start_sftp(|_| {});
    let t = env.start_sftp(|_| {});
    std::fs::write(env.remote(t, "/home/tester/on-target.txt"), b"hi").unwrap();

    let jump = ssh_profile("bastion", target(env.servers[j].port));
    env.save(&jump);
    let via = SshTarget {
        jump: Some(jump.id),
        ..target(env.servers[t].port)
    };
    let files = sftp_profile("files", via.clone());
    let shell = ssh_profile("shell", via);
    env.save(&files);
    env.save(&shell);

    let sftp_id = env
        .core
        .open_session(OpenTarget::Profile(files.id), GRID)
        .unwrap();
    let shell_id = env
        .core
        .open_session(OpenTarget::Profile(shell.id), GRID)
        .unwrap();
    env.wait_connected(sftp_id);
    env.wait_connected(shell_id);
    // 两个会话都经过跳板机，看到的是目标机的文件系统。
    assert_eq!(names(&env.list(sftp_id, HOME).unwrap()), ["on-target.txt"]);
    assert_eq!(names(&env.list(shell_id, HOME).unwrap()), ["on-target.txt"]);
    assert_eq!(
        env.core.session_info(sftp_id).unwrap().kind,
        SessionKind::Sftp
    );
    assert_eq!(
        env.core.session_info(shell_id).unwrap().kind,
        SessionKind::Ssh
    );
}

// ---- 文件操作 ----

#[test]
fn file_operations_on_ssh_session() {
    let mut env = Env::new();
    let s = env.start_sftp(|c| c.readdir_batch = 2);
    let id = env.open_ssh(s);

    assert_eq!(env.block(env.core.sftp_home(id)).unwrap(), HOME);

    // 流式列表：5 项、每批 2 项 → 多批；条目字段映射。
    for n in ["a", "b", "c", "d"] {
        std::fs::write(env.remote(s, &format!("/home/tester/{n}.txt")), n).unwrap();
    }
    std::fs::create_dir(env.remote(s, "/home/tester/sub")).unwrap();
    let mut stream = env.core.sftp_list(id, HOME).unwrap();
    let mut batches = Vec::new();
    while let Some(b) = env.block(stream.next_batch()).unwrap() {
        batches.push(b);
    }
    assert!(batches.len() >= 3, "{batches:?}");
    let all: Vec<DirEntry> = batches.into_iter().flatten().collect();
    assert_eq!(names(&all), ["a.txt", "b.txt", "c.txt", "d.txt", "sub"]);
    let a = all.iter().find(|e| e.name == "a.txt").unwrap();
    assert_eq!(a.path, "/home/tester/a.txt");
    assert!(!a.is_dir && !a.is_symlink);
    assert_eq!(a.size, 1);
    let stat = env.block(env.core.sftp_stat(id, &a.path)).unwrap();
    assert_eq!(stat.size, Some(a.size));
    assert_eq!(stat.mtime_ms, a.mtime_ms);
    assert!(env.block(env.core.sftp_stat(id, "/nope")).is_err());
    assert!(all.iter().find(|e| e.name == "sub").unwrap().is_dir);

    // mkdir / create_file / rename / remove。
    env.block(env.core.sftp_mkdir(id, "/home/tester/new"))
        .unwrap();
    assert!(env.remote(s, "/home/tester/new").is_dir());
    env.block(env.core.sftp_create_file(id, "/home/tester/new/f"))
        .unwrap();
    assert!(env.remote(s, "/home/tester/new/f").is_file());
    env.block(
        env.core
            .sftp_rename(id, "/home/tester/new/f", "/home/tester/new/g"),
    )
    .unwrap();
    assert!(env.remote(s, "/home/tester/new/g").is_file());
    env.block(env.core.sftp_remove(id, "/home/tester/new/g", false))
        .unwrap();
    assert!(!env.remote(s, "/home/tester/new/g").exists());

    // 错误映射：已存在不覆盖、不存在、重复创建目录。
    let exists = env
        .block(env.core.sftp_create_file(id, "/home/tester/a.txt"))
        .unwrap_err();
    assert_eq!(
        exists,
        CoreError::User(Message::SftpFileExists {
            path: "/home/tester/a.txt".into()
        })
    );
    assert_eq!(read(&env.remote(s, "/home/tester/a.txt")), b"a");
    let missing = env
        .block(env.core.sftp_remove(id, "/home/tester/none", false))
        .unwrap_err();
    match missing {
        CoreError::User(Message::SftpOpFailed { path, reason, .. }) => {
            assert_eq!(path, "/home/tester/none");
            assert_eq!(reason, Reason::NotFound);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        env.core.sftp_list(id, "/nope").is_ok(),
        "打开延迟到 next_batch"
    );
    assert!(env.list(id, "/nope").is_err());
}

#[test]
fn recursive_remove_deletes_tree_but_not_symlink_targets() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_ssh(s);
    let outside = env.remote(s, "/home/tester/keep");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("precious"), b"x").unwrap();
    let tree = env.remote(s, "/home/tester/tree");
    std::fs::create_dir_all(tree.join("a/b")).unwrap();
    std::fs::write(tree.join("a/b/f"), b"1").unwrap();
    std::fs::write(tree.join("top"), b"2").unwrap();
    // 指向目录的符号链接：只能删链接，不能进去删。
    std::os::unix::fs::symlink(&outside, tree.join("link")).unwrap();

    env.block(env.core.sftp_remove(id, "/home/tester/tree", true))
        .unwrap();
    assert!(!tree.exists());
    assert!(outside.join("precious").exists());
}

#[test]
fn sftp_ops_on_local_and_missing_sessions() {
    let mut env = Env::new();
    env.start_sftp(|_| {});
    let local = env
        .core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap();
    let ghost = SessionId::from_raw(9999);
    assert_eq!(
        env.block(env.core.sftp_home(local)),
        Err(CoreError::NotSupported)
    );
    assert_eq!(
        env.block(env.core.sftp_mkdir(ghost, "/x")),
        Err(CoreError::NoSuchSession(ghost))
    );
    assert_eq!(
        env.core.sftp_list(local, "/").map(|_| ()),
        Err(CoreError::NotSupported)
    );
    assert_eq!(
        env.core.download(local, "/a", "/tmp", ConflictPolicy::Fail),
        Err(CoreError::NotSupported)
    );
    assert_eq!(
        env.core.upload(ghost, "/a", "/b", ConflictPolicy::Fail),
        Err(CoreError::NoSuchSession(ghost))
    );
    env.core.close_session(local);
}

// ---- 传输 ----

#[test]
fn download_file_emits_ordered_throttled_progress_and_content_matches() {
    let mut env = Env::new();
    // 每个请求延迟 40 ms，窗口 64 × 32 KiB：12 MiB 需要几轮往返，足够产生多条被节流的进度。
    let s = env.start_sftp(slow);
    let id = env.open_sftp(s);
    let data = pattern(12 * 1024 * 1024);
    std::fs::write(env.remote(s, "/home/tester/big.bin"), &data).unwrap();

    let t = env
        .core
        .download(
            id,
            "/home/tester/big.bin",
            env.local.path().to_str().unwrap(),
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    assert_eq!(read(&env.local("big.bin")), data);

    let ev = env.transfer_events(t);
    match &ev[0].1 {
        CoreEvent::TransferStarted {
            session,
            protocol,
            direction,
            name,
            total_bytes,
            ..
        } => {
            assert_eq!(*session, id);
            assert_eq!(*protocol, XferProtocol::Sftp);
            assert_eq!(*direction, TransferDirection::Download);
            assert_eq!(name, "big.bin");
            assert_eq!(*total_bytes, Some(data.len() as u64));
        }
        other => panic!("第一个事件应是 Started：{other:?}"),
    }
    assert!(matches!(
        ev.last().unwrap().1,
        CoreEvent::TransferFinished { .. }
    ));
    let progress: Vec<(Instant, u64)> = ev
        .iter()
        .filter_map(|(at, e)| match e {
            CoreEvent::TransferProgress { done_bytes, .. } => Some((*at, *done_bytes)),
            _ => None,
        })
        .collect();
    assert!(progress.len() >= 3, "{progress:?}");
    // 字节数单调不减，最终等于总量。
    assert!(progress.windows(2).all(|w| w[0].1 <= w[1].1));
    assert_eq!(progress.last().unwrap().1, data.len() as u64);
    // 节流：中间的进度事件间隔不小于 50 ms（结束时强制报告的最后一条除外；留 5 ms 余量给调度抖动）。
    for w in progress[..progress.len() - 1].windows(2) {
        assert!(
            w[1].0.duration_since(w[0].0) >= Duration::from_millis(45),
            "进度间隔过短：{:?}",
            w[1].0.duration_since(w[0].0)
        );
    }
}

#[test]
fn upload_file_roundtrip_and_remote_listing_during_transfer_does_not_block() {
    let mut env = Env::new();
    let s = env.start_sftp(slow);
    let id = env.open_ssh(s);
    let data = pattern(8 * 1024 * 1024);
    std::fs::write(env.local("up.bin"), &data).unwrap();

    let t = env
        .core
        .upload(
            id,
            env.local("up.bin").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        )
        .unwrap();
    env.wait_progress(t);
    // 传输进行中列目录：不排队在传输后面（传输还要持续数百毫秒，列表只需一两个往返）。
    let started = Instant::now();
    let _ = env.list(id, HOME).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "列目录被传输阻塞：{:?}",
        started.elapsed()
    );
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    assert_eq!(read(&env.remote(s, "/home/tester/up.bin")), data);

    let ev = env.transfer_events(t);
    assert!(matches!(
        &ev[0].1,
        CoreEvent::TransferStarted {
            direction: TransferDirection::Upload,
            total_bytes: Some(n),
            name,
            ..
        } if *n == data.len() as u64 && name == "up.bin"
    ));
}

#[test]
fn directory_transfers_roundtrip_with_total_from_plan() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let src = env.remote(s, "/home/tester/proj");
    std::fs::create_dir_all(src.join("sub/deep")).unwrap();
    std::fs::write(src.join("a.txt"), pattern(1000)).unwrap();
    std::fs::write(src.join("sub/b.bin"), pattern(70_000)).unwrap();
    std::fs::write(src.join("sub/deep/c"), b"c").unwrap();

    // 下载目录：落在 local_dir/proj。
    let t = env
        .core
        .download(
            id,
            "/home/tester/proj",
            env.local.path().to_str().unwrap(),
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    let got = env.local("proj");
    assert_eq!(read(&got.join("a.txt")), read(&src.join("a.txt")));
    assert_eq!(read(&got.join("sub/b.bin")), read(&src.join("sub/b.bin")));
    assert_eq!(read(&got.join("sub/deep/c")), b"c");
    let ev = env.transfer_events(t);
    // 目录传输要先扫描：Started 不带总量，计划定稿后的 Progress 带总量。
    assert!(matches!(
        &ev[0].1,
        CoreEvent::TransferStarted { total_bytes: None, name, .. } if name == "proj"
    ));
    let total = 1000 + 70_000 + 1;
    assert!(ev.iter().any(|(_, e)| matches!(
        e,
        CoreEvent::TransferProgress { total_bytes: Some(n), .. } if *n == total
    )));

    // 上传目录：落在 remote_dir/up-proj（本地目录改名后上传）。
    std::fs::rename(&got, env.local("up-proj")).unwrap();
    std::fs::write(env.local("up-proj/new.txt"), b"new").unwrap();
    let t = env
        .core
        .upload(
            id,
            env.local("up-proj").to_str().unwrap(),
            "/home/tester/",
            ConflictPolicy::Fail,
        )
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    let up = env.remote(s, "/home/tester/up-proj");
    assert_eq!(read(&up.join("sub/b.bin")), read(&src.join("sub/b.bin")));
    assert_eq!(read(&up.join("new.txt")), b"new");
}

#[test]
fn overwrite_false_reports_already_exists_and_true_replaces() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();

    // 下载：本地已存在。
    std::fs::write(env.remote(s, "/home/tester/f.txt"), b"remote").unwrap();
    std::fs::write(env.local("f.txt"), b"local-old").unwrap();
    let t = env
        .core
        .download(id, "/home/tester/f.txt", &local_dir, ConflictPolicy::Fail)
        .unwrap();
    match env.wait_finished(t) {
        TransferOutcome::Failed {
            already_exists,
            message,
        } => {
            assert!(already_exists);
            assert_eq!(message.key(), "xfer-local-write-failed");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&env.local("f.txt")), b"local-old");
    let t = env
        .core
        .download(
            id,
            "/home/tester/f.txt",
            &local_dir,
            ConflictPolicy::Overwrite,
        )
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    assert_eq!(read(&env.local("f.txt")), b"remote");

    // 上传：远端已存在。
    std::fs::write(env.local("u.txt"), b"local-new").unwrap();
    std::fs::write(env.remote(s, "/home/tester/u.txt"), b"remote-old").unwrap();
    let t = env
        .core
        .upload(
            id,
            env.local("u.txt").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        )
        .unwrap();
    match env.wait_finished(t) {
        TransferOutcome::Failed {
            already_exists,
            message,
        } => {
            assert!(already_exists);
            assert_eq!(message.key(), "sftp-file-exists");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&env.remote(s, "/home/tester/u.txt")), b"remote-old");
    let t = env
        .core
        .upload(
            id,
            env.local("u.txt").to_str().unwrap(),
            HOME,
            ConflictPolicy::Overwrite,
        )
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    assert_eq!(read(&env.remote(s, "/home/tester/u.txt")), b"local-new");
}

#[test]
fn skip_policy_skips_existing_files_and_reports_count() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();

    // 下载：本地已存在 -> 跳过，内容不变，结果带跳过数 1。
    std::fs::write(env.remote(s, "/home/tester/f.txt"), b"remote").unwrap();
    std::fs::write(env.local("f.txt"), b"local-old").unwrap();
    let t = env
        .core
        .download(id, "/home/tester/f.txt", &local_dir, ConflictPolicy::Skip)
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 1 }
    );
    assert_eq!(read(&env.local("f.txt")), b"local-old");

    // 上传：远端已存在 -> 跳过。
    std::fs::write(env.local("u.txt"), b"local-new").unwrap();
    std::fs::write(env.remote(s, "/home/tester/u.txt"), b"remote-old").unwrap();
    let t = env
        .core
        .upload(
            id,
            env.local("u.txt").to_str().unwrap(),
            HOME,
            ConflictPolicy::Skip,
        )
        .unwrap();
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 1 }
    );
    assert_eq!(read(&env.remote(s, "/home/tester/u.txt")), b"remote-old");
}

#[test]
fn other_failures_are_not_flagged_as_already_exists() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let t = env
        .core
        .download(
            id,
            "/home/tester/missing",
            env.local.path().to_str().unwrap(),
            ConflictPolicy::Fail,
        )
        .unwrap();
    // 即使失败，生命周期仍完整：先 Started 再 Finished。
    match env.wait_finished(t) {
        TransferOutcome::Failed {
            already_exists: false,
            message,
        } => assert_eq!(message.key(), "sftp-op-failed"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        env.transfer_events(t)[0].1,
        CoreEvent::TransferStarted { .. }
    ));
}

#[test]
fn cancel_download_and_upload_leave_no_partial_files() {
    let mut env = Env::new();
    let s = env.start_sftp(slow);
    let id = env.open_sftp(s);
    let data = pattern(24 * 1024 * 1024);
    std::fs::write(env.remote(s, "/home/tester/big.bin"), &data).unwrap();
    std::fs::write(env.local("big-up.bin"), &data).unwrap();

    let t = env
        .core
        .download(
            id,
            "/home/tester/big.bin",
            env.local.path().to_str().unwrap(),
            ConflictPolicy::Fail,
        )
        .unwrap();
    env.wait_progress(t);
    env.core.cancel_transfer(t);
    assert_eq!(env.wait_finished(t), TransferOutcome::Cancelled);
    assert!(!env.local("big.bin").exists(), "下载的残缺文件应已删除");

    let t = env
        .core
        .upload(
            id,
            env.local("big-up.bin").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        )
        .unwrap();
    env.wait_progress(t);
    env.core.cancel_transfer(t);
    assert_eq!(env.wait_finished(t), TransferOutcome::Cancelled);
    assert!(
        !env.remote(s, "/home/tester/big-up.bin").exists(),
        "上传的残缺文件应已删除"
    );

    // 已结束 / 未知的 id：忽略。
    env.core.cancel_transfer(t);
    env.core.cancel_transfer(TransferId::from_raw(424_242));
}

#[test]
fn closing_session_cancels_its_transfers() {
    let mut env = Env::new();
    let s = env.start_sftp(slow);
    let id = env.open_sftp(s);
    let other = env.open_sftp(s);
    let data = pattern(24 * 1024 * 1024);
    std::fs::write(env.local("a.bin"), &data).unwrap();
    std::fs::write(env.local("b.bin"), &data).unwrap();

    let doomed = env
        .core
        .upload(
            id,
            env.local("a.bin").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        )
        .unwrap();
    let survivor = env
        .core
        .upload(
            other,
            env.local("b.bin").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        )
        .unwrap();
    env.wait_progress(doomed);
    env.wait_progress(survivor);

    env.core.close_session(id);
    assert_eq!(env.wait_finished(doomed), TransferOutcome::Cancelled);
    // 别的会话的传输不受影响：此刻仍在进行。
    assert!(
        env.events()
            .iter()
            .all(|(_, e)| finished_of(e, survivor).is_none())
    );
    env.core.cancel_transfer(survivor);
    assert_eq!(env.wait_finished(survivor), TransferOutcome::Cancelled);
}

#[test]
fn parameter_errors_are_returned_without_events() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();
    let missing_local = env.local("nope").to_str().unwrap().to_owned();

    // 上传的本地源不存在。
    match env
        .core
        .upload(id, &missing_local, HOME, ConflictPolicy::Fail)
    {
        Err(CoreError::User(Message::XferLocalReadFailed { reason, .. })) => {
            assert_eq!(reason, Reason::NotFound);
        }
        other => panic!("{other:?}"),
    }
    // 下载的目标目录不存在 / 不是目录。
    match env
        .core
        .download(id, "/home/tester/x", &missing_local, ConflictPolicy::Fail)
    {
        Err(CoreError::User(Message::XferLocalWriteFailed { reason, .. })) => {
            assert_eq!(reason, Reason::NotFound);
        }
        other => panic!("{other:?}"),
    }
    std::fs::write(env.local("file"), b"").unwrap();
    assert!(matches!(
        env.core.download(
            id,
            "/home/tester/x",
            env.local("file").to_str().unwrap(),
            ConflictPolicy::Fail
        ),
        Err(CoreError::User(Message::XferLocalWriteFailed { .. }))
    ));
    // 下载根目录 / `..`：没有可用作本地文件名的名字。
    assert!(matches!(
        env.core.download(id, "/", &local_dir, ConflictPolicy::Fail),
        Err(CoreError::User(Message::XferUnsafeName { .. }))
    ));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!env.events().iter().any(|(_, e)| is_transfer_event(e)));
}

// ---- MCP 的 SFTP 补充：entry / read / write / rmdir ----

use kanso_core::mcp::SftpWriteMode;

#[test]
fn mcp_sftp_entry_read_write_rmdir_roundtrip() {
    let mut env = Env::new();
    let s = env.start_sftp(|c| c.max_read = Some(777));
    let id = env.open_sftp(s);
    let data = pattern(70_000);
    std::fs::write(env.remote(s, "/home/tester/f.bin"), &data).unwrap();

    let e = env
        .block(env.core.sftp_entry(id, "/home/tester/f.bin", true))
        .unwrap();
    assert_eq!(
        (e.name.as_str(), e.is_dir, e.size),
        ("f.bin", false, 70_000)
    );
    assert_eq!(e.path, "/home/tester/f.bin");
    assert_eq!(
        env.block(env.core.sftp_read(id, "/home/tester/f.bin", 1_000, 60_000))
            .unwrap(),
        data[1_000..61_000]
    );
    // 超出文件末尾：空；读不存在的文件：错误。
    assert!(
        env.block(env.core.sftp_read(id, "/home/tester/f.bin", 1 << 30, 10))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        env.block(env.core.sftp_read(id, "/home/tester/none", 0, 10)),
        Err(CoreError::User(m)) if m.key() == "sftp-op-failed"
    ));

    // CreateNew：已存在报 SftpFileExists 且原文件不变。
    let exists = env.block(env.core.sftp_write(
        id,
        "/home/tester/f.bin",
        b"x".to_vec(),
        SftpWriteMode::CreateNew,
    ));
    assert_eq!(
        exists,
        Err(CoreError::User(Message::SftpFileExists {
            path: "/home/tester/f.bin".into()
        }))
    );
    assert_eq!(read(&env.remote(s, "/home/tester/f.bin")), data);
    env.block(env.core.sftp_write(
        id,
        "/home/tester/new.txt",
        b"hello".to_vec(),
        SftpWriteMode::CreateNew,
    ))
    .unwrap();
    assert_eq!(read(&env.remote(s, "/home/tester/new.txt")), b"hello");

    // Overwrite：替换内容。
    env.block(env.core.sftp_write(
        id,
        "/home/tester/new.txt",
        b"replaced".to_vec(),
        SftpWriteMode::Overwrite,
    ))
    .unwrap();
    assert_eq!(read(&env.remote(s, "/home/tester/new.txt")), b"replaced");

    // rmdir：空目录删除，非空报错。
    std::fs::create_dir(env.remote(s, "/home/tester/empty")).unwrap();
    std::fs::create_dir(env.remote(s, "/home/tester/full")).unwrap();
    std::fs::write(env.remote(s, "/home/tester/full/f"), b"x").unwrap();
    env.block(env.core.sftp_rmdir(id, "/home/tester/empty"))
        .unwrap();
    assert!(!env.remote(s, "/home/tester/empty").exists());
    assert!(matches!(
        env.block(env.core.sftp_rmdir(id, "/home/tester/full")),
        Err(CoreError::User(m)) if m.key() == "sftp-op-failed"
    ));
    assert!(env.remote(s, "/home/tester/full/f").exists());
}

#[test]
fn mcp_sftp_overwrite_without_posix_rename_is_not_supported() {
    let mut env = Env::new();
    let s = env.start_sftp(|c| c.posix_rename = false);
    let id = env.open_sftp(s);
    std::fs::write(env.remote(s, "/home/tester/f"), b"original").unwrap();
    assert_eq!(
        env.block(env.core.sftp_write(
            id,
            "/home/tester/f",
            b"new".to_vec(),
            SftpWriteMode::Overwrite
        )),
        Err(CoreError::NotSupported)
    );
    assert_eq!(read(&env.remote(s, "/home/tester/f")), b"original");
}

#[test]
fn mcp_sftp_overwrite_failure_reports_leftover_temp_path_in_detail() {
    let mut env = Env::new();
    // 写第 1 个块之后连接断开：临时文件删不掉。
    let s = env.start_sftp(|c| c.disconnect_after_writes = Some(1));
    let id = env.open_sftp(s);
    std::fs::write(env.remote(s, "/home/tester/f"), b"original").unwrap();
    let err = env
        .block(env.core.sftp_write(
            id,
            "/home/tester/f",
            pattern(200_000),
            SftpWriteMode::Overwrite,
        ))
        .unwrap_err();
    let CoreError::User(Message::SftpOpFailed {
        reason: Reason::Other { detail },
        ..
    }) = err
    else {
        panic!("{err:?}");
    };
    assert!(detail.contains("/home/tester/.f.kanso-tmp-"), "{detail}");
    assert_eq!(read(&env.remote(s, "/home/tester/f")), b"original");
}

#[test]
fn mcp_sftp_create_new_failure_reports_partially_written_file() {
    let mut env = Env::new();
    // 写第 1 个块之后连接断开：已创建的目标文件删不掉，只写了一半。
    let s = env.start_sftp(|c| c.disconnect_after_writes = Some(1));
    let id = env.open_sftp(s);
    let err = env
        .block(env.core.sftp_write(
            id,
            "/home/tester/new",
            pattern(200_000),
            SftpWriteMode::CreateNew,
        ))
        .unwrap_err();
    let CoreError::User(Message::SftpOpFailed {
        reason: Reason::Other { detail },
        ..
    }) = err
    else {
        panic!("{err:?}");
    };
    assert!(
        detail.contains("partially written file left at /home/tester/new"),
        "{detail}"
    );
    assert!(env.remote(s, "/home/tester/new").exists());
}

#[test]
fn mcp_sftp_ops_on_local_and_missing_sessions() {
    let mut env = Env::new();
    env.start_sftp(|_| {});
    let local = env
        .core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap();
    let ghost = SessionId::from_raw(9999);
    for s in [local, ghost] {
        let want = if s == local {
            CoreError::NotSupported
        } else {
            CoreError::NoSuchSession(s)
        };
        assert_eq!(
            env.block(env.core.sftp_entry(s, "/", true)).map(|_| ()),
            Err(want.clone())
        );
        assert_eq!(
            env.block(env.core.sftp_read(s, "/x", 0, 1)).map(|_| ()),
            Err(want.clone())
        );
        assert_eq!(
            env.block(
                env.core
                    .sftp_write(s, "/x", vec![], SftpWriteMode::CreateNew)
            ),
            Err(want.clone())
        );
        assert_eq!(env.block(env.core.sftp_rmdir(s, "/x")), Err(want.clone()));
        assert_eq!(
            env.block(
                env.core
                    .download_and_wait(s, "/a", "/tmp", ConflictPolicy::Fail)
            )
            .map(|_| ()),
            Err(want.clone())
        );
        assert_eq!(
            env.block(
                env.core
                    .upload_and_wait(s, "/a", "/b", ConflictPolicy::Fail)
            )
            .map(|_| ()),
            Err(want)
        );
    }
    env.core.close_session(local);
}

// ---- download_and_wait / upload_and_wait ----

impl Env {
    /// 等到出现 `TransferStarted` 且名字为 `name` 的传输，返回它的 id。
    fn started_transfer(&self, name: &str) -> TransferId {
        self.wait_until("TransferStarted", |ev| {
            ev.iter().any(|(_, e)| started_named(e, name).is_some())
        });
        self.events()
            .iter()
            .find_map(|(_, e)| started_named(e, name))
            .unwrap()
    }
}

fn started_named(e: &CoreEvent, want: &str) -> Option<TransferId> {
    match e {
        CoreEvent::TransferStarted { transfer, name, .. } if name == want => Some(*transfer),
        _ => None,
    }
}

#[test]
fn download_and_upload_and_wait_return_the_outcome_and_still_emit_events() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();
    let data = pattern(300_000);
    std::fs::write(env.remote(s, "/home/tester/d.bin"), &data).unwrap();

    let out = env
        .block(env.core.download_and_wait(
            id,
            "/home/tester/d.bin",
            &local_dir,
            ConflictPolicy::Fail,
        ))
        .unwrap();
    assert_eq!(out, TransferOutcome::Completed { skipped: 0 });
    assert_eq!(read(&env.local("d.bin")), data);
    // 传输照常发事件：返回时 Finished 事件已经发出。
    let t = env.started_transfer("d.bin");
    assert!(
        env.transfer_events(t).iter().any(
            |(_, e)| matches!(e, CoreEvent::TransferFinished { outcome, .. } if *outcome == out)
        )
    );

    std::fs::write(env.local("u.bin"), &data).unwrap();
    let out = env
        .block(env.core.upload_and_wait(
            id,
            env.local("u.bin").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        ))
        .unwrap();
    assert_eq!(out, TransferOutcome::Completed { skipped: 0 });
    assert_eq!(read(&env.remote(s, "/home/tester/u.bin")), data);
}

#[test]
fn and_wait_reports_already_exists_under_fail_policy() {
    let mut env = Env::new();
    let s = env.start_sftp(|_| {});
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();
    std::fs::write(env.remote(s, "/home/tester/f.txt"), b"remote").unwrap();
    std::fs::write(env.local("f.txt"), b"local-old").unwrap();
    let out = env
        .block(env.core.download_and_wait(
            id,
            "/home/tester/f.txt",
            &local_dir,
            ConflictPolicy::Fail,
        ))
        .unwrap();
    assert!(
        matches!(
            out,
            TransferOutcome::Failed {
                already_exists: true,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(read(&env.local("f.txt")), b"local-old");

    std::fs::write(env.local("u.txt"), b"local-new").unwrap();
    std::fs::write(env.remote(s, "/home/tester/u.txt"), b"remote-old").unwrap();
    let out = env
        .block(env.core.upload_and_wait(
            id,
            env.local("u.txt").to_str().unwrap(),
            HOME,
            ConflictPolicy::Fail,
        ))
        .unwrap();
    assert!(
        matches!(
            out,
            TransferOutcome::Failed {
                already_exists: true,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(read(&env.remote(s, "/home/tester/u.txt")), b"remote-old");
}

#[test]
fn and_wait_returns_cancelled_when_cancelled_while_waiting() {
    let mut env = Env::new();
    let s = env.start_sftp(slow);
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();
    std::fs::write(env.remote(s, "/home/tester/big.bin"), pattern(12 << 20)).unwrap();
    std::fs::write(env.local("up.bin"), pattern(12 << 20)).unwrap();
    let up_path = env.local("up.bin").to_str().unwrap().to_owned();

    let core = &env.core;
    let (down, up) = std::thread::scope(|sc| {
        let d = sc.spawn(|| {
            block_on(core.download_and_wait(
                id,
                "/home/tester/big.bin",
                &local_dir,
                ConflictPolicy::Fail,
            ))
        });
        let u =
            sc.spawn(|| block_on(core.upload_and_wait(id, &up_path, HOME, ConflictPolicy::Fail)));
        core.cancel_transfer(env.started_transfer("big.bin"));
        core.cancel_transfer(env.started_transfer("up.bin"));
        (d.join().unwrap(), u.join().unwrap())
    });
    assert_eq!(down, Ok(TransferOutcome::Cancelled));
    assert_eq!(up, Ok(TransferOutcome::Cancelled));
    assert!(!env.local("big.bin").exists());
}

fn block_on<T>(fut: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(fut)
}

#[test]
fn dropping_the_and_wait_future_does_not_cancel_the_transfer() {
    let mut env = Env::new();
    let s = env.start_sftp(slow);
    let id = env.open_sftp(s);
    let local_dir = env.local.path().to_str().unwrap().to_owned();
    let data = pattern(600_000);
    std::fs::write(env.remote(s, "/home/tester/keep.bin"), &data).unwrap();

    // 带时间驱动的运行时：超时到期后 future 被丢弃。传输每个请求延迟 40 ms，远长于 1 ms。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    // timeout() 必须在运行时上下文里创建，所以包进 async 块。
    let r = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_millis(1),
            env.core.download_and_wait(
                id,
                "/home/tester/keep.bin",
                &local_dir,
                ConflictPolicy::Fail,
            ),
        )
        .await
    });
    assert!(r.is_err(), "应当超时而不是已经完成");

    let t = env.started_transfer("keep.bin");
    assert_eq!(
        env.wait_finished(t),
        TransferOutcome::Completed { skipped: 0 }
    );
    assert_eq!(read(&env.local("keep.bin")), data);
}
