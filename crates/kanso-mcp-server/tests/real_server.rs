//! 真实 MCP 服务的集成测试：单实例锁、socket、握手、令牌、工具与回调顺序。
//!
//! 全部用临时目录与 mock core，不碰真实的 HOME 或钥匙串。
#![cfg(all(feature = "direct", unix))]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_core::{CollectingEventSink, Core, OpenTarget, Platform};
use kanso_mcp::hello;
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpServer, McpServerConfig, McpServerDelegate, McpServerError,
    McpServerStatus, McpUnavailable,
};
use kanso_types::mcp::{McpAccess, McpAuditFilter};
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
struct Recorder(Mutex<Vec<Call>>);

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
    fn on_activity(&self, _: SessionId, _: Option<McpActivity>) {}
    fn on_session_released(&self, _: SessionId) {}
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
        let writer = UnixStream::connect(socket).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        let mut c = Client { writer, reader };
        c.writer
            .write_all(hello::encode(token, None).as_bytes())
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
fn lists_only_the_kanso_list_sessions_tool_and_records_client_name() {
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
    assert_eq!(names, ["kanso_list_sessions"]);
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
