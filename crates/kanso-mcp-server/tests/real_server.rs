//! 真实 MCP 服务的集成测试：单实例锁、socket、握手、令牌、工具与回调顺序。
//!
//! 全部用临时目录与 mock core，不碰真实的 HOME 或钥匙串。
#![cfg(all(feature = "direct", unix))]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_core::{CollectingEventSink, Core, OpenTarget, Platform};
use kanso_mcp::hello;
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpServer, McpServerConfig, McpServerDelegate, McpServerError,
    McpServerStatus, McpUnavailable, TestHooks,
};
use kanso_types::mcp::{McpAccess, McpAuditEntry, McpAuditFilter};
use kanso_types::secret::SecretString;
use kanso_types::{GridSize, SessionId};
use serde_json::{Value, json};

const TOKEN: &str = "secret-token";

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Status(McpServerStatus),
    Clients(Vec<String>),
}

#[derive(Default)]
/// 第二个字段统计 `on_audit_changed` 的调用次数；第三个字段按顺序记录 `on_activity`。
struct Recorder(
    Mutex<Vec<Call>>,
    AtomicUsize,
    Mutex<Vec<(SessionId, Option<McpActivity>)>>,
);

impl Recorder {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl McpServerDelegate for Recorder {
    fn on_status_changed(&self, status: McpServerStatus) {
        self.0.lock().unwrap().push(Call::Status(status));
    }
    fn on_clients_changed(&self, clients: Vec<McpClientInfo>) {
        self.0
            .lock()
            .unwrap()
            .push(Call::Clients(clients.into_iter().map(|c| c.name).collect()));
    }
    fn on_session_opened(&self, _: SessionId, _: McpClientInfo) {}
    fn on_activity(&self, session: SessionId, activity: Option<McpActivity>) {
        self.2.lock().unwrap().push((session, activity));
    }
    fn on_session_released(&self, _: SessionId) {}
    fn on_audit_changed(&self) {
        self.1.fetch_add(1, Ordering::SeqCst);
    }
}

fn new_core() -> Arc<Core> {
    Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
        CollectingEventSink::new(),
    ))))
}

fn config(token: &str) -> McpServerConfig {
    McpServerConfig {
        bundle_id: "mock.kanso".into(),
        token: SecretString::new(token),
    }
}

struct Fixture {
    core: Arc<Core>,
    rec: Arc<Recorder>,
    server: McpServer,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let core = new_core();
        let rec = Arc::new(Recorder::default());
        let server = McpServer::new_in_dir(core.clone(), rec.clone(), dir.path().join("mcp"));
        Fixture {
            core,
            rec,
            server,
            dir,
        }
    }

    fn started() -> Fixture {
        let f = Fixture::new();
        f.server.start(config(TOKEN)).unwrap();
        f
    }

    /// 注入祖先链读取的服务：测试进程自己就是对端，所以按 pid 返回假链。
    fn started_with_ancestors(chain: Vec<u32>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let core = new_core();
        let rec = Arc::new(Recorder::default());
        let hooks = TestHooks {
            clock: None,
            ancestors: Some(Arc::new(move |pid| {
                let mut v = vec![pid];
                v.extend(chain.iter().copied());
                v
            })),
        };
        let server = McpServer::new_in_dir_with_hooks(
            core.clone(),
            rec.clone(),
            dir.path().join("mcp"),
            hooks,
        );
        server.start(config(TOKEN)).unwrap();
        Fixture {
            core,
            rec,
            server,
            dir,
        }
    }

    fn socket(&self) -> String {
        match self.server.status() {
            McpServerStatus::Running { socket_path } => socket_path,
            other => panic!("not running: {other:?}"),
        }
    }

    fn connect(&self, token: &str) -> Client {
        Client::open(&self.socket(), token)
    }

    /// 已握手并完成 initialize 的客户端。
    fn ready_client(&self, name: &str) -> Client {
        let mut c = self.connect(TOKEN);
        c.initialize(name);
        c
    }

    fn web_01(&self) -> kanso_types::ProfileId {
        self.core
            .profiles()
            .into_iter()
            .find(|p| p.name == "web-01")
            .unwrap()
            .id
    }
}

struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn open(socket: &str, token: &str) -> Client {
        Client::open_with_tty(socket, token, None, None)
    }

    fn open_with_tty(socket: &str, token: &str, tty: Option<&str>, tty_dev: Option<u64>) -> Client {
        let writer = UnixStream::connect(socket).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        let mut c = Client { writer, reader };
        c.writer
            .write_all(hello::encode(token, tty, tty_dev).as_bytes())
            .unwrap();
        c
    }

    fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).expect("read response");
        assert!(n > 0, "connection closed");
        serde_json::from_str(&line).unwrap()
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let v = self.read();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn initialize(&mut self, name: &str) -> Value {
        let resp = self.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": name, "version": "0.0.0"}
            }),
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        resp
    }

    /// 调用工具，返回 `(isError, 文本)`。
    fn call(&mut self, id: u64, tool: &str, args: Value) -> (bool, String) {
        let resp = self.request(id, "tools/call", json!({"name": tool, "arguments": args}));
        let result = &resp["result"];
        (
            result["isError"].as_bool().unwrap_or(false),
            result["content"][0]["text"].as_str().unwrap().to_owned(),
        )
    }

    /// 服务端是否已关闭连接（读到 EOF 或连接被重置）；读超时说明还开着。
    fn is_closed(&mut self) -> bool {
        // macOS 上对端已关闭的 socket 再设置超时会返回 EINVAL，这本身就说明连接已关闭。
        if self
            .reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(200)))
            .is_err()
        {
            return true;
        }
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
            ),
        }
    }
}

fn wait_until(what: &str, cond: impl FnMut() -> bool) {
    wait_until_for(what, Duration::from_secs(5), cond);
}

fn wait_until_for(what: &str, limit: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !cond() {
        assert!(Instant::now() < deadline, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn second_instance_in_same_dir_is_unavailable() {
    let f = Fixture::new();
    let rec2 = Arc::new(Recorder::default());
    let second = McpServer::new_in_dir(new_core(), rec2.clone(), f.dir.path().join("mcp"));
    let unavailable = McpServerStatus::Unavailable(McpUnavailable::AnotherInstance);
    assert_eq!(second.status(), unavailable);
    assert_eq!(
        second.start(config(TOKEN)),
        Err(McpServerError::AnotherInstance)
    );
    assert_eq!(rec2.take(), vec![Call::Status(unavailable.clone())]);
    // 没拿到锁时 stop 不改变状态。
    second.stop();
    assert_eq!(second.status(), unavailable);
    assert!(rec2.take().is_empty());
    // 第一个实例不受影响。
    f.server.start(config(TOKEN)).unwrap();
}

/// 用独立的文件描述符持有锁文件的 flock，模拟转发程序探测时的短暂占用。
#[allow(unsafe_code)]
fn hold_flock(path: &std::path::Path, op: libc::c_int) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    // SAFETY: `file` 是刚打开、仍存活的有效描述符。
    let rc = unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) };
    assert_eq!(rc, 0);
    file
}

#[test]
fn start_retries_after_lock_was_briefly_held_by_a_probe() {
    let dir = tempfile::tempdir().unwrap();
    let mcp_dir = dir.path().join("mcp");
    std::fs::create_dir_all(&mcp_dir).unwrap();
    let probe = hold_flock(&mcp_dir.join("kanso.lock"), libc::LOCK_SH);
    let rec = Arc::new(Recorder::default());
    let server = McpServer::new_in_dir(new_core(), rec.clone(), mcp_dir);
    assert_eq!(
        server.status(),
        McpServerStatus::Unavailable(McpUnavailable::AnotherInstance)
    );
    // 探测还没结束时再启动，仍然是另一个实例。
    assert_eq!(
        server.start(config(TOKEN)),
        Err(McpServerError::AnotherInstance)
    );
    // 探测结束后重试成功，状态从 AnotherInstance 正常过渡到 Running。
    drop(probe);
    server.start(config(TOKEN)).unwrap();
    assert!(matches!(server.status(), McpServerStatus::Running { .. }));
}

#[test]
fn start_creates_private_socket_and_stop_removes_it() {
    let f = Fixture::new();
    f.server.start(config(TOKEN)).unwrap();
    let socket = f.socket();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(std::path::Path::new(&socket)), 0o600);
    assert_eq!(mode(&f.dir.path().join("mcp")), 0o700);

    f.server.stop();
    assert!(!std::path::Path::new(&socket).exists());
    assert_eq!(f.server.status(), McpServerStatus::Stopped);
    // 锁仍被持有：转发程序据此知道 App 在运行。可以再次启动。
    f.server.start(config(TOKEN)).unwrap();
    assert!(std::path::Path::new(&f.socket()).exists());
}

#[test]
fn stale_socket_file_is_replaced() {
    let f = Fixture::new();
    // 模拟上次异常退出留下的 socket 文件。
    let mcp = f.dir.path().join("mcp");
    let _stale = std::os::unix::net::UnixListener::bind({
        std::fs::create_dir_all(&mcp).unwrap();
        mcp.join("kanso.sock")
    })
    .unwrap();
    f.server.start(config(TOKEN)).unwrap();
    f.ready_client("x");
}

#[test]
fn lists_the_tools_and_records_client_name() {
    let f = Fixture::started();
    let mut c = f.connect(TOKEN);
    let init = c.initialize("tester");
    assert_eq!(init["result"]["serverInfo"]["name"], "kanso");
    let tools = c.request(2, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let mut names = names;
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "kanso_close_tab",
            "kanso_exec",
            "kanso_get_cwd",
            "kanso_list_sessions",
            "kanso_open_tab",
            "kanso_sftp_list",
            "kanso_sftp_mkdir",
            "kanso_sftp_read",
            "kanso_sftp_remove",
            "kanso_sftp_rename",
            "kanso_sftp_stat",
            "kanso_sftp_write",
            "kanso_terminal_read",
            "kanso_terminal_run",
            "kanso_terminal_send"
        ]
    );
    let clients = f.server.clients();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].name, "tester");
}

#[test]
fn list_sessions_respects_access_and_switch() {
    let f = Fixture::started();
    f.core.set_mcp_enabled(true).unwrap();
    let web = f.web_01();
    f.core.set_mcp_access(web, McpAccess::ReadOnly).unwrap();
    // 给 web-01 开一个标签，结果里应出现在它的 tabs 里。
    let tab = f
        .core
        .open_session(OpenTarget::Profile(web), GridSize { cols: 80, rows: 24 })
        .unwrap();

    let mut c = f.ready_client("tester");
    let (is_error, text) = c.call(2, "kanso_list_sessions", json!({}));
    assert!(!is_error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{text}");
    let s = &sessions[0];
    assert_eq!(s["name"], "web-01");
    assert_eq!(s["kind"], "ssh");
    assert_eq!(s["host"], "web-01.example.com");
    assert_eq!(s["user"], "deploy");
    assert_eq!(s["group"], "生产");
    assert_eq!(s["access"], "read-only");
    assert_eq!(s["tabs"][0]["id"], tab.raw());
    assert!(s["tabs"][0]["state"].is_string());
    assert!(!text.contains("legacy-switch"));

    // 过滤：名称不区分大小写的子串；分组名精确匹配。
    let (_, t) = c.call(3, "kanso_list_sessions", json!({"name": "WEB"}));
    assert_eq!(
        serde_json::from_str::<Value>(&t).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (_, t) = c.call(4, "kanso_list_sessions", json!({"group": "其他"}));
    assert!(
        serde_json::from_str::<Value>(&t).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // 总开关关闭：返回 MCP_DISABLED，且是工具结果而不是协议错误。
    f.core.set_mcp_enabled(false).unwrap();
    let (is_error, text) = c.call(5, "kanso_list_sessions", json!({}));
    assert!(is_error);
    assert!(text.contains("MCP_DISABLED"), "{text}");
    // 四次调用都计数（含被拒绝的）。
    wait_until("call count", || f.server.clients()[0].calls == 4);

    // 每次调用都有审计记录。
    wait_until("audit", || {
        f.core.mcp_audit(McpAuditFilter::default()).unwrap().len() == 4
    });
    let records = f.core.mcp_audit(McpAuditFilter::default()).unwrap();
    let newest = &records[0].entry;
    assert_eq!(newest.tool, "kanso_list_sessions");
    assert_eq!(newest.client, "tester");
    assert_eq!(newest.error_code.as_deref(), Some("MCP_DISABLED"));
}

#[test]
fn list_sessions_and_open_tab_support_custom_tab_title() {
    let f = Fixture::started();
    f.core.set_mcp_enabled(true).unwrap();
    let web = f.web_01();
    f.core.set_mcp_access(web, McpAccess::Full).unwrap();
    let grid = GridSize { cols: 80, rows: 24 };
    let first = f.core.open_session(OpenTarget::Profile(web), grid).unwrap();
    let second = f.core.open_session(OpenTarget::Profile(web), grid).unwrap();
    f.core.set_session_title(first, Some("  日志  ".into()));

    let mut c = f.ready_client("tester");
    // kanso_open_tab 传入 title 参数，打开的标签直接带上自定义名。
    let (is_error, opened_text) = c.call(
        2,
        "kanso_open_tab",
        json!({"session": web.to_string(), "title": "  部署  "}),
    );
    assert!(!is_error, "{opened_text}");
    let opened_id = serde_json::from_str::<Value>(&opened_text).unwrap()["tab"]
        .as_u64()
        .unwrap();

    let (is_error, text) = c.call(3, "kanso_list_sessions", json!({}));
    assert!(!is_error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let tabs = v["sessions"][0]["tabs"].as_array().unwrap();
    let title_of = |id: u64| {
        tabs.iter()
            .find(|t| t["id"] == id)
            .and_then(|t| t["title"].as_str())
            .map(str::to_owned)
    };
    assert_eq!(title_of(first.raw()).as_deref(), Some("日志"));
    assert_eq!(title_of(second.raw()).as_deref(), Some("web-01"));
    assert_eq!(title_of(opened_id).as_deref(), Some("部署"));

    // 留空清除自定义名，恢复默认名称。
    f.core.set_session_title(first, Some("   ".into()));
    let (is_error, text) = c.call(4, "kanso_list_sessions", json!({}));
    assert!(!is_error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    let tabs = v["sessions"][0]["tabs"].as_array().unwrap();
    let title = tabs
        .iter()
        .find(|t| t["id"] == first.raw())
        .and_then(|t| t["title"].as_str());
    assert_eq!(title, Some("web-01"));
}

#[test]
fn invalid_token_gets_error_reply_audit_and_is_closed() {
    let f = Fixture::started();
    let mut c = f.connect("wrong");
    c.send(json!({"jsonrpc": "2.0", "id": 7, "method": "initialize", "params": {}}));
    let resp = c.read();
    assert_eq!(resp["id"], 7);
    assert_eq!(resp["error"]["code"], -32000);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid or missing Kanso MCP token")
    );
    assert_eq!(resp["error"]["data"]["code"], "INVALID_TOKEN");
    assert!(c.is_closed());
    assert!(f.server.clients().is_empty());

    wait_until("audit", || {
        !f.core
            .mcp_audit(McpAuditFilter::default())
            .unwrap()
            .is_empty()
    });
    let entry = &f.core.mcp_audit(McpAuditFilter::default()).unwrap()[0].entry;
    assert_eq!(entry.error_code.as_deref(), Some("INVALID_TOKEN"));
    assert_eq!(entry.client, "");
    assert_eq!(entry.tool, "");
}

#[test]
fn audit_changed_callback_follows_successful_writes() {
    let f = Fixture::started();
    let audit_calls = || f.rec.1.load(Ordering::SeqCst);

    // 被拒连接也会写审计，同样要回调。
    let mut bad = f.connect("wrong");
    bad.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    let _ = bad.read();
    wait_until("audit callback after rejected connection", || {
        audit_calls() >= 1
    });
    let after_rejected = audit_calls();

    // 成功的工具调用落库后计数继续增加；总开关关着时工具调用会被拒，所以先打开。
    f.core.set_mcp_enabled(true).unwrap();
    let mut c = f.ready_client("tester");
    let (is_error, text) = c.call(2, "kanso_list_sessions", json!({}));
    assert!(!is_error, "{text}");
    wait_until("audit callback after tool call", || {
        audit_calls() > after_rejected
    });
}

#[test]
fn prune_that_removes_records_triggers_the_audit_callback() {
    let f = Fixture::new();
    // 启动后审计任务的第一次 prune 立即执行；先放一条远超保留期的旧记录让它有东西可删。
    f.core
        .mcp_audit_append(vec![McpAuditEntry {
            at_ms: 0,
            client: "old".into(),
            mcp_session: "s".into(),
            tool: "kanso_list_sessions".into(),
            profile: None,
            session: None,
            args: String::new(),
            error_code: None,
            duration_ms: 1,
            bytes: 0,
        }])
        .unwrap();
    f.server.start(config(TOKEN)).unwrap();
    wait_until("audit callback after prune", || {
        f.rec.1.load(Ordering::SeqCst) >= 1
    });
    assert!(
        f.core
            .mcp_audit(McpAuditFilter::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn prune_without_removals_does_not_trigger_the_audit_callback() {
    let f = Fixture::started();
    // 第一次 prune 在启动时立即执行；库里没有过期记录，等它跑完后计数仍为 0。
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(f.rec.1.load(Ordering::SeqCst), 0);
}

#[test]
fn no_audit_callback_after_drop_returns() {
    let f = Fixture::started();
    f.core.set_mcp_enabled(true).unwrap();
    let mut c = f.ready_client("tester");
    for id in 2..8 {
        let _ = c.call(id, "kanso_list_sessions", json!({}));
    }
    let rec = f.rec.clone();
    drop(c);
    // drop 之前已经发出的回调不算；返回之后计数必须不再增长。
    drop(f.server);
    let at_drop = rec.1.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(rec.1.load(Ordering::SeqCst), at_drop);
}

#[test]
fn seventeenth_client_is_refused() {
    let f = Fixture::started();
    let held: Vec<Client> = (0..16).map(|_| f.connect(TOKEN)).collect();
    wait_until("16 clients", || f.server.clients().len() == 16);
    let mut extra = f.connect(TOKEN);
    extra.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    let resp = extra.read();
    assert_eq!(
        resp["error"]["message"],
        "Too many MCP clients connected to Kanso."
    );
    assert_eq!(resp["error"]["data"]["code"], "TOO_MANY_CLIENTS");
    assert_eq!(f.server.clients().len(), 16);
    drop(held);
}

#[test]
fn set_token_disconnects_old_connections() {
    let f = Fixture::started();
    let mut old = f.ready_client("old");
    f.server.set_token(SecretString::new("rotated"));
    assert!(f.server.clients().is_empty());
    wait_until("old connection closed", || old.is_closed());

    let mut stale = f.connect(TOKEN);
    stale.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    let stale_resp = stale.read();
    assert_eq!(stale_resp["error"]["code"], -32000);
    assert_eq!(stale_resp["error"]["data"]["code"], "INVALID_TOKEN");

    let mut fresh = f.connect("rotated");
    fresh.initialize("new");
    assert_eq!(f.server.clients()[0].name, "new");
}

#[test]
fn disconnect_closes_that_client() {
    let f = Fixture::started();
    let mut c = f.ready_client("tester");
    let id = f.server.clients()[0].id;
    f.server.disconnect(id);
    assert!(f.server.clients().is_empty());
    wait_until("connection closed", || c.is_closed());
}

#[test]
fn callbacks_follow_state_changes() {
    let f = Fixture::new();
    f.server.start(config(TOKEN)).unwrap();
    let socket = f.socket();
    {
        let mut c = f.ready_client("tester");
        c.send(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
        c.read();
    }
    wait_until("client gone", || f.server.clients().is_empty());
    f.server.stop();
    assert_eq!(
        f.rec.take(),
        vec![
            Call::Status(McpServerStatus::Running {
                socket_path: socket
            }),
            Call::Clients(vec!["".into()]),
            Call::Clients(vec!["tester".into()]),
            Call::Clients(vec![]),
            Call::Status(McpServerStatus::Stopped),
        ]
    );
}

#[test]
fn stop_disconnects_clients_and_restart_works() {
    let f = Fixture::started();
    let mut c = f.ready_client("tester");
    f.server.stop();
    assert!(f.server.clients().is_empty());
    assert!(c.is_closed());
    f.rec.take();
    // 已在运行时再 start：先停再启，状态序列为 Stopped、Running。
    f.server.start(config(TOKEN)).unwrap();
    f.server.start(config(TOKEN)).unwrap();
    let calls = f.rec.take();
    assert!(matches!(
        calls[0],
        Call::Status(McpServerStatus::Running { .. })
    ));
    assert_eq!(calls[1], Call::Status(McpServerStatus::Stopped));
    assert!(matches!(
        calls[2],
        Call::Status(McpServerStatus::Running { .. })
    ));
}

#[test]
fn start_failures_report_failed_and_stop_returns_to_stopped_once() {
    // 空令牌与 bundle id 不一致都不能启动。
    for bad in [
        McpServerConfig {
            bundle_id: "mock.kanso".into(),
            token: SecretString::new(""),
        },
        McpServerConfig {
            bundle_id: "other.app".into(),
            token: SecretString::new(TOKEN),
        },
    ] {
        let f = Fixture::new();
        let Err(McpServerError::Failed { detail }) = f.server.start(bad) else {
            panic!("start should fail");
        };
        let unavailable = McpServerStatus::Unavailable(McpUnavailable::Failed { detail });
        assert_eq!(f.server.status(), unavailable);
        assert_eq!(f.rec.take(), vec![Call::Status(unavailable)]);
        assert!(!f.dir.path().join("mcp/kanso.sock").exists());

        f.server.stop();
        assert_eq!(f.server.status(), McpServerStatus::Stopped);
        assert_eq!(f.rec.take(), vec![Call::Status(McpServerStatus::Stopped)]);
        f.server.stop();
        assert!(f.rec.take().is_empty());
    }
}

#[test]
fn start_retries_when_directory_could_not_be_created() {
    let dir = tempfile::tempdir().unwrap();
    // 目录位置先被一个普通文件占着，构造时取锁失败。
    let blocker = dir.path().join("mcp");
    std::fs::write(&blocker, b"x").unwrap();
    let rec = Arc::new(Recorder::default());
    let server = McpServer::new_in_dir(new_core(), rec.clone(), blocker.clone());
    assert!(matches!(
        server.start(config(TOKEN)),
        Err(McpServerError::Failed { .. })
    ));
    std::fs::remove_file(&blocker).unwrap();
    server.start(config(TOKEN)).unwrap();
    assert!(matches!(server.status(), McpServerStatus::Running { .. }));
}

#[test]
fn dropping_a_running_server_removes_socket_and_releases_lock() {
    let f = Fixture::started();
    let socket = f.socket();
    let dir = f.dir.path().join("mcp");
    drop(f.server);
    assert!(!std::path::Path::new(&socket).exists());
    let again = McpServer::new_in_dir(new_core(), Arc::new(Recorder::default()), dir);
    assert_eq!(again.status(), McpServerStatus::Stopped);
}

#[test]
fn version_mismatch_gets_error_reply() {
    let f = Fixture::started();
    let stream = UnixStream::connect(f.socket()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut c = Client {
        reader: BufReader::new(stream.try_clone().unwrap()),
        writer: stream,
    };
    c.writer
        .write_all(b"{\"v\":99,\"token\":\"x\",\"tty\":null}\n")
        .unwrap();
    c.send(json!({"jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {}}));
    let resp = c.read();
    assert_eq!(resp["id"], 3);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("versions do not match")
    );
    assert_eq!(resp["error"]["data"]["code"], "VERSION_MISMATCH");
    assert!(f.server.clients().is_empty());
}

#[test]
fn connection_without_initialize_is_released_after_timeout() {
    let f = Fixture::started();
    // 握手通过但一直不发 initialize：10 秒后被释放（这个用例要等十几秒）。
    let mut c = f.connect(TOKEN);
    wait_until("admitted", || f.server.clients().len() == 1);
    wait_until_for("init timeout", Duration::from_secs(15), || {
        f.server.clients().is_empty()
    });
    wait_until("connection closed", || c.is_closed());
}

// ---- F9 阶段 2：终端工具 ----

const GRID: GridSize = GridSize { cols: 80, rows: 24 };

impl Fixture {
    fn profile(&self, name: &str) -> kanso_types::ProfileId {
        self.core
            .profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
            .id
    }

    /// 打开开关、给指定配置授权并开一个标签，返回标签 id。
    fn tab(&self, profile: &str, level: McpAccess) -> SessionId {
        self.core.set_mcp_enabled(true).unwrap();
        let id = self.profile(profile);
        self.core.set_mcp_access(id, level).unwrap();
        self.core
            .open_session(OpenTarget::Profile(id), GRID)
            .unwrap()
    }
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn error_code(text: &str) -> String {
    parse(text)["error"]["code"].as_str().unwrap().to_owned()
}

#[test]
fn read_and_cwd_work_with_read_only_access() {
    let f = Fixture::started();
    let tab = f.tab("web-01", McpAccess::ReadOnly);
    let mut c = f.ready_client("tester");

    let (err, text) = c.call(2, "kanso_get_cwd", json!({"tab": tab.raw()}));
    assert!(!err, "{text}");
    assert!(parse(&text)["cwd"].is_string(), "{text}");

    let (err, text) = c.call(3, "kanso_terminal_read", json!({"tab": tab.raw()}));
    assert!(!err, "{text}");
    let v = parse(&text);
    assert!(v["text"].as_str().unwrap().contains("mock terminal"));
    assert_eq!(v["truncated"], false);
    assert!(v["original_bytes"].as_u64().unwrap() > 0);
    assert!(v["cursor"]["row"].is_number() && v["cursor"]["col"].is_number());
    assert_eq!(v["size"]["cols"], 80);
    assert_eq!(v["size"]["rows"], 24);
    assert_eq!(v["alternate_screen"], false);

    let (err, text) = c.call(
        6,
        "kanso_terminal_read",
        json!({"tab": tab.raw(), "scope": "viewport"}),
    );
    assert!(!err, "{text}");
    assert_eq!(parse(&text)["cursor"], v["cursor"]);

    let (err, text) = c.call(7, "kanso_list_sessions", json!({"name": "web-01"}));
    assert!(!err, "{text}");
    let listed = &parse(&text)["sessions"][0]["tabs"][0];
    assert_eq!(listed["id"], tab.raw());
    assert_eq!(listed["terminal_run_blocker"], "read_only");
    assert_eq!(listed["terminal_send_blocker"], "read_only");

    // 行数夹取：超大、为 0 都不报错。
    for (id, lines) in [(4, 100_000), (5, 0)] {
        let (err, text) = c.call(
            id,
            "kanso_terminal_read",
            json!({"tab": tab.raw(), "scope": "recent", "lines": lines}),
        );
        assert!(!err, "{text}");
    }
}

#[test]
fn write_tools_run_and_send_succeed_with_full_access() {
    let f = Fixture::started();
    let ssh = f.tab("web-01", McpAccess::Full);
    let local = f.tab("Local", McpAccess::Full);
    let mut c = f.ready_client("tester");

    // SSH 标签没有 OSC 133：退出码为 "unknown"。
    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": ssh.raw(), "command": "ls"}),
    );
    assert!(!err, "{text}");
    let v = parse(&text);
    assert_eq!(v["status"], "completed");
    assert_eq!(v["exit_code"], "unknown");
    assert!(v["output"].as_str().unwrap().contains("ls"));

    // 本地标签有 OSC 133：带退出码。
    let (_, text) = c.call(
        3,
        "kanso_terminal_run",
        json!({"tab": local.raw(), "command": "ls"}),
    );
    assert_eq!(parse(&text)["exit_code"], 0);

    // send：文字与按键混合；带 settle_ms 时返回屏幕，不带则没有 screen。
    let (err, text) = c.call(
        4,
        "kanso_terminal_send",
        json!({"tab": ssh.raw(), "inputs": [{"text": "vim"}, {"key": "Enter"}, {"key": "Ctrl+C"}], "settle_ms": 50}),
    );
    assert!(!err, "{text}");
    let v = parse(&text);
    assert_eq!(v["status"], "sent");
    assert!(v["screen"]["text"].is_string());
    let (_, text) = c.call(
        5,
        "kanso_terminal_send",
        json!({"tab": ssh.raw(), "inputs": [{"key": "Up"}]}),
    );
    assert!(parse(&text).get("screen").is_none(), "{text}");

    // 四次写调用，活动标识成对出现：Some 在前、None 在后。
    let log = f.rec.2.lock().unwrap();
    assert_eq!(log.len(), 8);
    for pair in log.chunks(2) {
        assert!(pair[0].1.is_some() && pair[1].1.is_none());
        assert_eq!(pair[0].0, pair[1].0);
    }
    let first = log[0].1.as_ref().unwrap();
    assert_eq!(first.client_name, "tester");
    assert_eq!(first.tool, "kanso_terminal_run");
    assert_eq!(first.client_id, f.server.clients()[0].id);
}

#[test]
fn run_timeout_keeps_output_and_explains_missing_osc133() {
    let f = Fixture::started();
    let ssh = f.tab("web-01", McpAccess::Full);
    let local = f.tab("Local", McpAccess::Full);
    let mut c = f.ready_client("tester");

    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": ssh.raw(), "command": "mock-hang 1", "timeout_secs": 5}),
    );
    assert!(err);
    assert_eq!(error_code(&text), "TIMEOUT");
    let v = parse(&text);
    assert_eq!(v["status"], "unconfirmed");
    assert!(v["output"].is_string());
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("kanso_exec")
    );

    // 有 OSC 133 的标签不附这段说明。
    let (_, text) = c.call(
        3,
        "kanso_terminal_run",
        json!({"tab": local.raw(), "command": "mock-hang 1"}),
    );
    assert_eq!(error_code(&text), "TIMEOUT");
    assert!(!text.contains("kanso_exec"), "{text}");
}

#[test]
fn argument_errors_do_not_reach_the_terminal() {
    let f = Fixture::started();
    let tab = f.tab("web-01", McpAccess::Full);
    let mut c = f.ready_client("tester");
    let t = tab.raw();

    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": t, "command": "a\nb"}),
    );
    assert!(err);
    assert_eq!(error_code(&text), "INVALID_ARGUMENT");
    // 任何 C0 控制字符与 DEL 都拒绝：制表符、回车、ESC、NUL、DEL。
    for (n, bad) in ["a\tb", "ls\r", "\u{1b}[A", "a\u{0}b", "a\u{7f}"]
        .into_iter()
        .enumerate()
    {
        let (err, text) = c.call(
            20 + n as u64,
            "kanso_terminal_run",
            json!({"tab": t, "command": bad}),
        );
        assert!(err);
        assert_eq!(error_code(&text), "INVALID_ARGUMENT", "{bad:?}: {text}");
        assert!(text.contains("kanso_terminal_send"), "{text}");
    }
    let (_, text) = c.call(
        3,
        "kanso_terminal_send",
        json!({"tab": t, "inputs": [{"key": "F13"}]}),
    );
    assert_eq!(error_code(&text), "INVALID_ARGUMENT");
    let (_, text) = c.call(
        4,
        "kanso_terminal_send",
        json!({"tab": t, "inputs": [{"text": "a", "key": "Tab"}]}),
    );
    assert_eq!(error_code(&text), "INVALID_ARGUMENT");
    let (_, text) = c.call(5, "kanso_terminal_send", json!({"tab": t, "inputs": []}));
    assert_eq!(error_code(&text), "INVALID_ARGUMENT");
    // 没有任何输入到达终端：没有活动回调。
    assert!(f.rec.2.lock().unwrap().is_empty());
}

#[test]
fn access_levels_and_missing_tabs_map_to_error_codes() {
    let f = Fixture::started();
    let read_only = f.tab("web-01", McpAccess::ReadOnly);
    let off = f.tab("legacy-switch", McpAccess::Off);
    let sftp = f.tab("files", McpAccess::Full);
    let mut c = f.ready_client("tester");

    // 只读：写被拒，读允许。
    for (id, tool, args) in [
        (
            2,
            "kanso_terminal_run",
            json!({"tab": read_only.raw(), "command": "ls"}),
        ),
        (
            3,
            "kanso_terminal_send",
            json!({"tab": read_only.raw(), "inputs": [{"key": "Enter"}]}),
        ),
    ] {
        let (err, text) = c.call(id, tool, args);
        assert!(err);
        assert_eq!(error_code(&text), "ACCESS_DENIED", "{text}");
    }
    let (err, _) = c.call(4, "kanso_terminal_read", json!({"tab": read_only.raw()}));
    assert!(!err);

    // 关闭级别与不存在的标签：全部 NOT_FOUND，且说明文字只差 id（无法区分）。
    let mut messages = Vec::new();
    let mut id = 10;
    for tab in [off.raw(), 987_654] {
        for tool in ["kanso_get_cwd", "kanso_terminal_read"] {
            id += 1;
            let (err, text) = c.call(id, tool, json!({"tab": tab}));
            assert!(err);
            assert_eq!(error_code(&text), "NOT_FOUND", "{text}");
            messages.push(
                parse(&text)["error"]["message"]
                    .as_str()
                    .unwrap()
                    .replace(&tab.to_string(), "N"),
            );
        }
        for (tool, args) in [
            ("kanso_terminal_run", json!({"tab": tab, "command": "ls"})),
            (
                "kanso_terminal_send",
                json!({"tab": tab, "inputs": [{"key": "Tab"}]}),
            ),
        ] {
            id += 1;
            let (_, text) = c.call(id, tool, args);
            assert_eq!(error_code(&text), "NOT_FOUND", "{text}");
        }
    }
    assert!(messages.windows(2).all(|w| w[0] == w[1]), "{messages:?}");

    // 纯 SFTP 会话没有终端。
    for (id, tool, args) in [
        (40, "kanso_terminal_read", json!({"tab": sftp.raw()})),
        (
            41,
            "kanso_terminal_run",
            json!({"tab": sftp.raw(), "command": "ls"}),
        ),
    ] {
        let (_, text) = c.call(id, tool, args);
        assert_eq!(error_code(&text), "UNSUPPORTED", "{text}");
    }

    // 拒绝的调用没有产生活动标识。
    assert!(f.rec.2.lock().unwrap().is_empty());
}

#[test]
fn legacy_client_without_tty_dev_is_found_by_tty_path() {
    let f = Fixture::started();
    let own = f.tab("Local", McpAccess::Full);
    let other = f.tab("web-01", McpAccess::Full);
    // 旧版转发程序只上报路径、没有设备号：服务端退回路径字符串比较。
    // 路径取自 core 实际报告的值，而不是测试里手工拼的设备名。
    let tty = f
        .core
        .mcp_session_status(own)
        .unwrap()
        .local_tty
        .expect("local tab has a tty");
    let mut c = Client::open_with_tty(&f.socket(), TOKEN, Some(&tty), None);
    c.initialize("claude-code");
    assert_client_tab_behavior(&f, &mut c, own, other);
}

#[test]
fn tty_device_number_takes_precedence_over_the_path() {
    let f = Fixture::started();
    let own = f.tab("Local", McpAccess::Full);
    let tty = f
        .core
        .mcp_session_status(own)
        .unwrap()
        .local_tty
        .expect("local tab has a tty");
    // 设备号存在时不按路径比较：路径虽然相同，但 mock 标签的路径不是真实设备（stat 失败），不能命中。
    // 真实 pty 的设备号命中见 `identity.rs` 的单元测试。
    let mut c = Client::open_with_tty(&f.socket(), TOKEN, Some(&tty), Some(1));
    c.initialize("claude-code");
    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": own.raw(), "command": "ls"}),
    );
    assert!(!err, "{text}");
    assert_eq!(f.server.clients()[0].own_session, None);
}

#[test]
fn client_tab_is_found_by_process_ancestry() {
    // 先开标签才知道 shell pid；假祖先链读取在连接时才调用，所以用共享槽延后填入。
    let dir = tempfile::tempdir().unwrap();
    let core = new_core();
    let rec = Arc::new(Recorder::default());
    let slot: Arc<Mutex<Vec<u32>>> = Arc::default();
    let chain = slot.clone();
    let hooks = TestHooks {
        clock: None,
        ancestors: Some(Arc::new(move |pid| {
            let mut v = vec![pid, 4_000_000];
            v.extend(chain.lock().unwrap().iter().copied());
            v
        })),
    };
    let server =
        McpServer::new_in_dir_with_hooks(core.clone(), rec.clone(), dir.path().join("mcp"), hooks);
    server.start(config(TOKEN)).unwrap();
    let f = Fixture {
        core,
        rec,
        server,
        dir,
    };
    let own = f.tab("Local", McpAccess::Full);
    let other = f.tab("web-01", McpAccess::Full);
    let shell_pid = f.core.mcp_session_status(own).unwrap().shell_pid.unwrap();
    slot.lock().unwrap().push(shell_pid);
    // 没有 tty（脱离控制终端启动）：只能靠祖先链。
    let mut c = f.ready_client("claude-code");
    assert_client_tab_behavior(&f, &mut c, own, other);
}

fn assert_client_tab_behavior(f: &Fixture, c: &mut Client, own: SessionId, other: SessionId) {
    // 设置页能看到识别结果。
    wait_until("own_session", || {
        f.server.clients()[0].own_session == Some(own)
    });

    // 写被拒，读允许。
    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": own.raw(), "command": "ls"}),
    );
    assert!(err);
    assert_eq!(error_code(&text), "TAB_SELF", "{text}");
    let (_, text) = c.call(
        3,
        "kanso_terminal_send",
        json!({"tab": own.raw(), "inputs": [{"key": "Enter"}]}),
    );
    assert_eq!(error_code(&text), "TAB_SELF", "{text}");
    let (err, _) = c.call(4, "kanso_terminal_read", json!({"tab": own.raw()}));
    assert!(!err);
    let (err, _) = c.call(5, "kanso_get_cwd", json!({"tab": own.raw()}));
    assert!(!err);
    // 其他标签照常。
    let (err, text) = c.call(
        6,
        "kanso_terminal_run",
        json!({"tab": other.raw(), "command": "ls"}),
    );
    assert!(!err, "{text}");

    // list_sessions 里只有自己所在的标签 is_client_tab 为真。
    let (_, text) = c.call(7, "kanso_list_sessions", json!({}));
    let v = parse(&text);
    let mut flagged = Vec::new();
    for s in v["sessions"].as_array().unwrap() {
        for t in s["tabs"].as_array().unwrap() {
            if t["is_client_tab"] == true {
                flagged.push(t["id"].as_u64().unwrap());
            }
        }
    }
    assert_eq!(flagged, [own.raw()], "{text}");
    // 被拒绝的写调用没有产生活动标识：只有对 other 的那一次（一对）。
    assert_eq!(f.rec.2.lock().unwrap().len(), 2);
}

#[test]
fn unrelated_client_has_no_client_tab() {
    let f = Fixture::started_with_ancestors(vec![11, 22]);
    let local = f.tab("Local", McpAccess::Full);
    // tty 不匹配、祖先链里也没有这个标签的 shell。
    let mut c = Client::open_with_tty(&f.socket(), TOKEN, Some("/dev/ttys999"), None);
    c.initialize("outside");
    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": local.raw(), "command": "ls"}),
    );
    assert!(!err, "{text}");
    assert_eq!(f.server.clients()[0].own_session, None);
    let (_, text) = c.call(3, "kanso_list_sessions", json!({}));
    assert!(!text.contains("\"is_client_tab\":true"), "{text}");
}

#[test]
fn audit_records_target_and_command_but_not_output() {
    let f = Fixture::started();
    let tab = f.tab("web-01", McpAccess::Full);
    let web = f.web_01();
    let off = f.tab("legacy-switch", McpAccess::Off);
    let mut c = f.ready_client("tester");
    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": tab.raw(), "command": "echo hello-audit"}),
    );
    assert!(!err);
    assert!(text.contains("mock output"));
    let (_, text) = c.call(3, "kanso_terminal_read", json!({"tab": off.raw()}));
    assert_eq!(error_code(&text), "NOT_FOUND");

    wait_until("audit", || {
        f.core.mcp_audit(McpAuditFilter::default()).unwrap().len() == 2
    });
    let records = f.core.mcp_audit(McpAuditFilter::default()).unwrap();
    let denied = &records[0].entry;
    assert_eq!(denied.tool, "kanso_terminal_read");
    assert_eq!(denied.error_code.as_deref(), Some("NOT_FOUND"));
    // 被拒绝的调用也记下目标：关闭级别的会话 id，界面据此显示"未授权的会话"。
    assert_eq!(denied.profile, Some(f.profile("legacy-switch")));
    assert_eq!(denied.session, Some(off));

    let run = &records[1].entry;
    assert_eq!(run.tool, "kanso_terminal_run");
    assert_eq!(run.profile, Some(web));
    assert_eq!(run.session, Some(tab));
    assert!(
        run.args.starts_with("command=echo hello-audit\n"),
        "{}",
        run.args
    );
    assert!(run.args.contains("\ndispatch=submitted\n"), "{}", run.args);
    // mock 的输出是 "mock output: <命令>"，审计里不应出现输出。
    assert!(!run.args.contains("mock output"), "{}", run.args);
    assert_eq!(run.error_code, None);
}

#[test]
fn switch_off_rejects_terminal_tools_with_mcp_disabled() {
    let f = Fixture::started();
    let tab = f.tab("web-01", McpAccess::Full);
    let mut c = f.ready_client("tester");
    f.core.set_mcp_enabled(false).unwrap();
    for (id, tool, args) in [
        (2, "kanso_get_cwd", json!({"tab": tab.raw()})),
        (3, "kanso_terminal_read", json!({"tab": tab.raw()})),
        (
            4,
            "kanso_terminal_run",
            json!({"tab": tab.raw(), "command": "ls"}),
        ),
        (
            5,
            "kanso_terminal_send",
            json!({"tab": tab.raw(), "inputs": [{"key": "Tab"}]}),
        ),
    ] {
        let (err, text) = c.call(id, tool, args);
        assert!(err);
        assert_eq!(error_code(&text), "MCP_DISABLED", "{text}");
    }
}

/// 取最新一条审计记录（审计按时间倒序）。
fn latest_audit(f: &Fixture, expected_len: usize) -> McpAuditEntry {
    wait_until("audit", || {
        f.core.mcp_audit(McpAuditFilter::default()).unwrap().len() >= expected_len
    });
    f.core.mcp_audit(McpAuditFilter::default()).unwrap()[0]
        .entry
        .clone()
}

#[test]
fn audit_of_switch_off_rejection_keeps_the_requested_tab_and_profile() {
    let f = Fixture::started();
    let tab = f.tab("web-01", McpAccess::Full);
    let web = f.web_01();
    let mut c = f.ready_client("tester");
    f.core.set_mcp_enabled(false).unwrap();
    let (err, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": tab.raw(), "command": "ls"}),
    );
    assert!(err);
    assert_eq!(error_code(&text), "MCP_DISABLED");
    let entry = latest_audit(&f, 1);
    assert_eq!(entry.error_code.as_deref(), Some("MCP_DISABLED"));
    assert_eq!(entry.session, Some(tab));
    assert_eq!(entry.profile, Some(web));
}

#[test]
fn audit_of_argument_errors_keeps_the_profile() {
    let f = Fixture::started();
    let tab = f.tab("web-01", McpAccess::Full);
    let web = f.web_01();
    let mut c = f.ready_client("tester");
    let (_, text) = c.call(
        2,
        "kanso_terminal_run",
        json!({"tab": tab.raw(), "command": "a\tb"}),
    );
    assert_eq!(error_code(&text), "INVALID_ARGUMENT");
    let entry = latest_audit(&f, 1);
    assert_eq!(entry.error_code.as_deref(), Some("INVALID_ARGUMENT"));
    assert_eq!(entry.session, Some(tab));
    assert_eq!(entry.profile, Some(web));
    assert!(entry.args.starts_with("command=a\\tb\n"));
    assert!(entry.args.contains("\ndispatch=rejected\n"));

    let inputs: Vec<Value> = (0..300).map(|_| json!({"key": "Tab"})).collect();
    let (_, text) = c.call(
        3,
        "kanso_terminal_send",
        json!({"tab": tab.raw(), "inputs": inputs}),
    );
    assert_eq!(error_code(&text), "TOO_LARGE");
    let entry = latest_audit(&f, 2);
    assert_eq!(entry.error_code.as_deref(), Some("TOO_LARGE"));
    assert_eq!(entry.session, Some(tab));
    assert_eq!(entry.profile, Some(web));
    // 摘要有长度上限。
    assert!(entry.args.chars().count() <= 512, "{}", entry.args.len());
}
