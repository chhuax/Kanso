//! 转发程序 `kanso-mcp` 的集成测试：未运行、开关关闭、与真实服务端到端。
//!
//! 通过 `KANSO_MCP_DIR`（仅 Debug 构建生效）指向临时目录，不碰真实的 HOME。
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use kanso_core::{CollectingEventSink, Core, Platform};
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpServer, McpServerConfig, McpServerDelegate, McpServerStatus,
};
use kanso_types::SessionId;
use kanso_types::mcp::McpAccess;
use kanso_types::secret::SecretString;
use serde_json::{Value, json};

const TOKEN: &str = "relay-token";

struct Relay {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Relay {
    fn spawn(dir: &std::path::Path, token: &str) -> Relay {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kanso-mcp"))
            .env("KANSO_MCP_DIR", dir)
            .env("KANSO_MCP_TOKEN", token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Relay {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.stdin
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "relay closed stdout"
        );
        serde_json::from_str(&line).unwrap()
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.read()
    }

    fn initialize(&mut self) -> Value {
        self.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "relay-test", "version": "0"}
            }),
        )
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        // 关闭 stdin 让转发程序自然退出；仍未退出就强制结束，避免测试残留进程。
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn reports_not_running_without_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut relay = Relay::spawn(dir.path(), TOKEN);
    let resp = relay.initialize();
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["error"]["code"], -32000);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Kanso is not running")
    );
}

#[test]
fn reports_disabled_when_lock_is_held_but_no_socket() {
    let dir = tempfile::tempdir().unwrap();
    // 模拟 App 在运行但开关关闭：持有单实例锁，没有监听 socket。
    let lock = std::fs::File::create(dir.path().join("kanso.lock")).unwrap();
    // SAFETY: `lock` 是刚创建、仍存活的有效文件描述符；flock 只作用于它，测试结束时随文件关闭释放。
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0);

    let mut relay = Relay::spawn(dir.path(), TOKEN);
    let resp = relay.initialize();
    assert_eq!(resp["error"]["code"], -32000);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("MCP is disabled")
    );
    drop(lock);
}

struct NoopDelegate;

impl McpServerDelegate for NoopDelegate {
    fn on_status_changed(&self, _: McpServerStatus) {}
    fn on_clients_changed(&self, _: Vec<McpClientInfo>) {}
    fn on_session_opened(&self, _: SessionId, _: McpClientInfo) {}
    fn on_activity(&self, _: SessionId, _: Option<McpActivity>) {}
    fn on_session_released(&self, _: SessionId) {}
}

fn started_server(dir: &std::path::Path) -> (Arc<Core>, McpServer) {
    let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
        CollectingEventSink::new(),
    ))));
    let server = McpServer::new_in_dir(core.clone(), Arc::new(NoopDelegate), dir.to_path_buf());
    server
        .start(McpServerConfig {
            bundle_id: "mock.kanso".into(),
            token: SecretString::new(TOKEN),
        })
        .unwrap();
    (core, server)
}

#[test]
fn end_to_end_with_real_server() {
    let dir = tempfile::tempdir().unwrap();
    let (core, server) = started_server(dir.path());
    core.set_mcp_enabled(true).unwrap();
    let web = core
        .profiles()
        .into_iter()
        .find(|p| p.name == "web-01")
        .unwrap()
        .id;
    core.set_mcp_access(web, McpAccess::Full).unwrap();

    let mut relay = Relay::spawn(dir.path(), TOKEN);
    let init = relay.initialize();
    assert_eq!(init["result"]["serverInfo"]["name"], "kanso");
    relay.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let resp = relay.request(
        2,
        "tools/call",
        json!({"name": "kanso_list_sessions", "arguments": {}}),
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["sessions"][0]["name"], "web-01");
    assert_eq!(v["sessions"][0]["access"], "full");
    assert_eq!(server.clients()[0].name, "relay-test");
    drop(relay);
    server.stop();
}

#[test]
fn wrong_token_is_rejected_through_relay() {
    let dir = tempfile::tempdir().unwrap();
    let (_core, server) = started_server(dir.path());
    let mut relay = Relay::spawn(dir.path(), "not-the-token");
    let resp = relay.initialize();
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid or missing Kanso MCP token")
    );
    server.stop();
}
