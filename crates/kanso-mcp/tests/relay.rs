//! 转发程序 `kanso-mcp` 的集成测试：未运行、开关关闭、与真实服务端到端。
//!
//! 通过 `KANSO_MCP_DIR`（仅 Debug 构建生效）指向临时目录，不碰真实的 HOME。
#![cfg(all(any(unix, windows), debug_assertions))]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanso_core::mcp::TextScope;
use kanso_core::{CollectingEventSink, Core, CoreConfig, OpenTarget, Platform};
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpServer, McpServerConfig, McpServerDelegate, McpServerStatus,
};
use kanso_types::mcp::{McpAccess, McpAuditFilter};
use kanso_types::profile::{LocalProfile, Profile, ProfileKind};
use kanso_types::secret::SecretString;
use kanso_types::{CellSide, GridSize, Modifiers, ProfileId, SessionId, ViewportPoint};
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

    fn ready(&mut self) {
        assert_eq!(self.initialize()["result"]["serverInfo"]["name"], "kanso");
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn list_sessions(&mut self, id: u64, arguments: Value) -> Vec<Value> {
        let resp = self.request(
            id,
            "tools/call",
            json!({"name": "kanso_list_sessions", "arguments": arguments}),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        v["sessions"].as_array().unwrap().clone()
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
fn reports_not_running_without_app() {
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
    assert_eq!(resp["error"]["data"]["code"], "NOT_RUNNING");
}

#[test]
fn reports_disabled_when_app_is_running_without_mcp() {
    let dir = tempfile::tempdir().unwrap();
    // 实际创建服务对象并保留单实例锁，但不开启监听。
    let (_core, server) = make_server(dir.path());
    let mut relay = Relay::spawn(dir.path(), TOKEN);
    let resp = relay.initialize();
    assert_eq!(resp["error"]["code"], -32000);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("MCP is disabled")
    );
    assert_eq!(resp["error"]["data"]["code"], "MCP_DISABLED");
    drop(server);
}

struct NoopDelegate;

impl McpServerDelegate for NoopDelegate {
    fn on_status_changed(&self, _: McpServerStatus) {}
    fn on_clients_changed(&self, _: Vec<McpClientInfo>) {}
    fn on_session_opened(&self, _: SessionId, _: McpClientInfo) {}
    fn on_activity(&self, _: SessionId, _: Option<McpActivity>) {}
    fn on_session_released(&self, _: SessionId) {}
    fn on_audit_changed(&self) {}
}

fn make_server(dir: &std::path::Path) -> (Arc<Core>, McpServer) {
    let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
        CollectingEventSink::new(),
    ))));
    let server = McpServer::new_in_dir(core.clone(), Arc::new(NoopDelegate), dir.to_path_buf());
    (core, server)
}

fn started_server(dir: &std::path::Path) -> (Arc<Core>, McpServer) {
    let (core, server) = make_server(dir);
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
    let profiles = core.profiles();
    let web = profiles.iter().find(|p| p.name == "web-01").unwrap().id;
    let mfa = profiles
        .iter()
        .find(|p| p.name == "mfa-bastion")
        .unwrap()
        .id;
    core.set_mcp_access(web, McpAccess::ReadOnly).unwrap();
    core.set_mcp_access(mfa, McpAccess::Full).unwrap();

    let mut relay = Relay::spawn(dir.path(), TOKEN);
    relay.ready();
    let sessions = relay.list_sessions(2, json!({}));
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .any(|s| s["name"] == "mfa-bastion" && s["access"] == "full")
    );
    let matching = relay.list_sessions(3, json!({"name": "WEB"}));
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0]["name"], "web-01");
    assert_eq!(matching[0]["access"], "read-only");
    assert!(
        relay
            .list_sessions(4, json!({"group": "不存在"}))
            .is_empty()
    );
    assert_eq!(server.clients()[0].name, "relay-test");
    assert_eq!(server.clients()[0].calls, 3);

    server.stop(); // 停止时写完审计队列。
    let records = core.mcp_audit(McpAuditFilter::default()).unwrap();
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|record| {
        record.entry.client == "relay-test"
            && record.entry.tool == "kanso_list_sessions"
            && record.entry.error_code.is_none()
    }));
    assert!(
        records
            .iter()
            .any(|record| record.entry.args.contains("name=WEB"))
    );
}

#[test]
fn relay_reads_an_authorized_local_pty_and_stops_after_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let test_home = normalized_path(dir.path());
    let marker = "KANSO_RELAY_PTY";
    let shows_marker = |text: &str| text.lines().any(|line| line.trim() == marker);
    let mut environment = vec![("HOME".into(), test_home.clone())];
    if cfg!(windows) {
        environment.extend(
            ["SystemRoot", "windir", "ComSpec"]
                .into_iter()
                .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value))),
        );
        environment.push(("USERPROFILE".into(), test_home.clone()));
    }
    let bundle = "test.kanso.relay";
    let core = Arc::new(
        Core::new_with_base_env(
            CoreConfig::new(dir.path().join("data"), env!("CARGO_PKG_VERSION"), bundle),
            Platform::for_tests(Arc::new(CollectingEventSink::new())),
            environment,
        )
        .unwrap(),
    );
    let profile = ProfileId::new();
    core.save_profile(Profile {
        id: profile,
        name: "Relay test".into(),
        color: None,
        group: None,
        kind: ProfileKind::Local(LocalProfile {
            shell_command: if cfg!(windows) {
                format!(
                    r#""{}\System32\cmd.exe" /d /q"#,
                    std::env::var("SystemRoot").unwrap()
                )
            } else {
                "/bin/sh -i".into()
            },
            start_dir: Some(test_home.clone()),
            ..LocalProfile::default()
        }),
    })
    .unwrap();
    core.set_mcp_enabled(true).unwrap();
    core.set_mcp_access(profile, McpAccess::ReadOnly).unwrap();
    let tab = core
        .open_session(
            OpenTarget::Profile(profile),
            GridSize { cols: 80, rows: 24 },
        )
        .unwrap();
    let command = if cfg!(windows) {
        "echo KANSO_RELAY_PTY\r"
    } else {
        // 先输出换行，避免命令早于 `/bin/sh -i` 首个 `$ ` 提示符到达时标记与提示符拼在同一行。
        "printf '\\n%s%s\\n' KANSO_ RELAY_PTY\r"
    };
    core.text_input(tab, command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = core.terminal_text(tab, TextScope::Screen, 4096).unwrap();
        if shows_marker(&snapshot.content.text) {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", snapshot.content.text);
        std::thread::sleep(Duration::from_millis(20));
    }

    let endpoint = dir.path().join("mcp");
    let server = McpServer::new_in_dir(core.clone(), Arc::new(NoopDelegate), endpoint.clone());
    server
        .start(McpServerConfig {
            bundle_id: bundle.into(),
            token: SecretString::new(TOKEN),
        })
        .unwrap();
    let mut relay = Relay::spawn(&endpoint, TOKEN);
    relay.ready();
    let sessions = relay.list_sessions(2, json!({}));
    assert_eq!(sessions.len(), 1);
    let tabs = sessions[0]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    let status = &tabs[0];
    assert_eq!(status["id"], tab.raw());
    assert_eq!(status["state"], "connected");
    assert_eq!(status["sender_active"], false);
    assert!(status["osc133"].is_boolean());
    assert_eq!(status["terminal_run_blocker"], "read_only");
    assert_eq!(status["terminal_send_blocker"], "read_only");

    let mut call = |request, tool, args: Value| {
        let response = relay.request(
            request,
            "tools/call",
            json!({
                "name": tool, "arguments": args
            }),
        );
        let result = &response["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{response}"));
        (
            result["isError"] == true,
            serde_json::from_str::<Value>(text).unwrap(),
        )
    };
    let (error, cwd) = call(3, "kanso_get_cwd", json!({"tab": tab.raw()}));
    assert!(!error, "{cwd}");
    assert_eq!(
        normalized_path(std::path::Path::new(cwd["cwd"].as_str().unwrap())),
        test_home
    );
    let (error, snapshot) = call(4, "kanso_terminal_read", json!({"tab": tab.raw()}));
    assert!(!error, "{snapshot}");
    assert!(shows_marker(snapshot["text"].as_str().unwrap()));
    let command = if cfg!(windows) {
        "for /L %i in (1,1,55) do @echo VIEWPORT_%i\r"
    } else {
        "i=1; while [ \"$i\" -le 55 ]; do echo VIEWPORT_$i; i=$((i+1)); done\r"
    };
    core.text_input(tab, command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = core.terminal_text(tab, TextScope::Screen, 4096).unwrap();
        if snapshot
            .content
            .text
            .lines()
            .any(|line| line.trim() == "VIEWPORT_55")
        {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", snapshot.content.text);
        std::thread::sleep(Duration::from_millis(20));
    }
    core.scroll(
        tab,
        50.0,
        ViewportPoint {
            col: 0,
            row: 0,
            side: CellSide::Left,
        },
        Modifiers::NONE,
    )
    .unwrap();
    let (error, view) = call(
        5,
        "kanso_terminal_read",
        json!({"tab": tab.raw(), "scope": "viewport"}),
    );
    assert!(!error, "{view}");
    assert!(
        view["text"]
            .as_str()
            .unwrap()
            .lines()
            .any(|line| line.trim() == "VIEWPORT_1")
    );
    assert_eq!(view["cursor"], Value::Null);
    let (error, live) = call(6, "kanso_terminal_read", json!({"tab": tab.raw()}));
    assert!(!error, "{live}");
    assert!(
        live["text"]
            .as_str()
            .unwrap()
            .lines()
            .any(|line| line.trim() == "VIEWPORT_55")
    );
    assert!(live["cursor"]["row"].is_number());

    let (error, denied) = call(
        7,
        "kanso_terminal_run",
        json!({"tab": tab.raw(), "command": "echo KANSO_REJECTED"}),
    );
    assert!(error, "{denied}");
    assert_eq!(denied["error"]["code"], "ACCESS_DENIED");

    core.set_mcp_access(profile, McpAccess::Full).unwrap();
    let (error, sent) = call(
        8,
        "kanso_terminal_send",
        json!({"tab": tab.raw(), "inputs": [{"text": "echo KANSO_AUDIT"}, {"key": "Enter"}]}),
    );
    assert!(!error, "{sent}");

    core.set_mcp_access(profile, McpAccess::Off).unwrap();
    let (error, denied) = call(9, "kanso_terminal_read", json!({"tab": tab.raw()}));
    assert!(error, "{denied}");
    assert_eq!(denied["error"]["code"], "NOT_FOUND");
    assert!(relay.list_sessions(10, json!({})).is_empty());
    server.stop();
    let audit = core.mcp_audit(McpAuditFilter::default()).unwrap();
    assert!(audit.iter().any(|record| {
        record
            .entry
            .args
            .starts_with("command=echo KANSO_REJECTED\n")
            && record.entry.args.contains("\ndispatch=rejected\n")
    }));
    assert!(audit.iter().any(|record| {
        record
            .entry
            .args
            .starts_with("inputs=[text:\"echo KANSO_AUDIT\", key:\"Enter\"]\n")
            && record.entry.args.contains("\ndispatch=submitted\n")
    }));
    core.shutdown();
}

fn normalized_path(path: &std::path::Path) -> String {
    let path = path.canonicalize().unwrap();
    let path = path.to_str().unwrap();
    path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
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

#[test]
fn missing_token_is_rejected_through_relay() {
    let dir = tempfile::tempdir().unwrap();
    let (_core, server) = started_server(dir.path());
    // 空串与不设置 KANSO_MCP_TOKEN 等价：转发程序都发送空令牌。
    let mut relay = Relay::spawn(dir.path(), "");
    let resp = relay.initialize();
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid or missing Kanso MCP token")
    );
    server.stop();
}

#[test]
fn rotation_revokes_old_connections_and_accepts_new_token() {
    let dir = tempfile::tempdir().unwrap();
    let (_core, server) = started_server(dir.path());
    let mut old = Relay::spawn(dir.path(), TOKEN);
    old.ready();

    server.set_token(SecretString::new("rotated-token"));
    assert!(server.clients().is_empty());
    assert_eq!(old.stdout.read_line(&mut String::new()).unwrap(), 0);

    let mut stale = Relay::spawn(dir.path(), TOKEN);
    let response = stale.initialize();
    assert_eq!(response["error"]["data"]["code"], "INVALID_TOKEN");

    let mut current = Relay::spawn(dir.path(), "rotated-token");
    current.ready();
    assert_eq!(server.clients().len(), 1);
    server.stop();
}

#[test]
fn final_response_survives_stdin_eof() {
    let dir = tempfile::tempdir().unwrap();
    let (_core, server) = started_server(dir.path());
    let mut relay = Relay::spawn(dir.path(), "wrong-token");
    relay.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
    relay.stdin.take();
    let resp = relay.read();
    assert_eq!(resp["error"]["data"]["code"], "INVALID_TOKEN");
    server.stop();
}

#[cfg(windows)]
#[test]
fn windows_presence_is_exclusive_and_release_allows_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let (_core, first) = make_server(dir.path());
    let (core, second) = make_server(dir.path());
    let config = McpServerConfig {
        bundle_id: core.bundle_id().to_owned(),
        token: SecretString::new(TOKEN),
    };
    assert_eq!(
        second.start(config.clone()),
        Err(kanso_mcp_server::McpServerError::AnotherInstance)
    );
    drop(first);
    second.start(config).unwrap();
    second.stop();
}

#[cfg(windows)]
#[test]
fn windows_server_restarts_while_old_helper_is_still_open() {
    let dir = tempfile::tempdir().unwrap();
    let (core, server) = started_server(dir.path());
    let mut old = Relay::spawn(dir.path(), TOKEN);
    old.ready();

    server
        .start(McpServerConfig {
            bundle_id: core.bundle_id().to_owned(),
            token: SecretString::new(TOKEN),
        })
        .unwrap();
    let mut new = Relay::spawn(dir.path(), TOKEN);
    assert_eq!(new.initialize()["result"]["serverInfo"]["name"], "kanso");
    server.stop();
}
