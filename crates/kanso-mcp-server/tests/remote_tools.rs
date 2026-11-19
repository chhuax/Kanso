//! 后台执行、标签打开 / 关闭与 SFTP 工具的集成测试：经本机 socket、进程内客户端、mock core。
//!
//! mock core 的限制（不属于本 crate，未改动）：
//! - `mcp_exec` 不会超时，所以 `TIMEOUT` 只在 `real/remote.rs` 的单元测试里验证；
//! - `sftp_entry` 对任何路径都成功（目录与否只看路径是否以 `/` 结尾），所以逐级创建目录与重命名
//!   只能验证"已存在"的分支（`CONFLICT`）；
//! - mock 没有调用记录，`kanso_sftp_remove` 选 `sftp_rmdir` 还是 `sftp_remove(.., false)` 的决策
//!   由 `remote::remove_action` 的单元测试覆盖，这里只验证两条路径都能成功返回。
#![cfg(all(feature = "direct", unix))]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_core::{CollectingEventSink, Core, OpenTarget, Platform};
use kanso_mcp::hello;
use kanso_mcp_server::{
    McpActivity, McpClientInfo, McpServer, McpServerConfig, McpServerDelegate, McpServerStatus,
};
use kanso_types::mcp::{McpAccess, McpAuditFilter};
use kanso_types::secret::SecretString;
use kanso_types::{GridSize, SessionId};
use serde_json::{Value, json};

const TOKEN: &str = "secret-token";
const GRID: GridSize = GridSize { cols: 80, rows: 24 };

#[derive(Default)]
struct Recorder {
    opened: Mutex<Vec<SessionId>>,
    released: Mutex<Vec<SessionId>>,
}

impl McpServerDelegate for Recorder {
    fn on_status_changed(&self, _: McpServerStatus) {}
    fn on_clients_changed(&self, _: Vec<McpClientInfo>) {}
    fn on_session_opened(&self, s: SessionId, _: McpClientInfo) {
        self.opened.lock().unwrap().push(s);
    }
    fn on_activity(&self, _: SessionId, _: Option<McpActivity>) {}
    fn on_session_released(&self, s: SessionId) {
        self.released.lock().unwrap().push(s);
    }
    fn on_audit_changed(&self) {}
}

struct Fixture {
    core: Arc<Core>,
    rec: Arc<Recorder>,
    server: McpServer,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn started() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let core = Arc::new(Core::new_mock(Platform::for_tests(Arc::new(
            CollectingEventSink::new(),
        ))));
        core.set_mcp_enabled(true).unwrap();
        let rec = Arc::new(Recorder::default());
        let server = McpServer::new_in_dir(core.clone(), rec.clone(), dir.path().join("mcp"));
        server
            .start(McpServerConfig {
                bundle_id: "mock.kanso".into(),
                token: SecretString::new(TOKEN),
            })
            .unwrap();
        Fixture {
            core,
            rec,
            server,
            _dir: dir,
        }
    }

    fn client(&self) -> Client {
        let McpServerStatus::Running { socket_path } = self.server.status() else {
            panic!("not running")
        };
        let writer = UnixStream::connect(socket_path).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let reader = BufReader::new(writer.try_clone().unwrap());
        let mut c = Client {
            writer,
            reader,
            next_id: 1,
        };
        c.writer
            .write_all(hello::encode(TOKEN, None, None).as_bytes())
            .unwrap();
        let resp = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "tester", "version": "0"}}),
        );
        assert!(resp["result"].is_object(), "{resp}");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn profile(&self, name: &str) -> String {
        self.core
            .profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
            .id
            .to_string()
    }

    /// 设置级别并返回 profile id 字符串。
    fn grant(&self, name: &str, level: McpAccess) -> String {
        let id = self
            .core
            .profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
            .id;
        self.core.set_mcp_access(id, level).unwrap();
        id.to_string()
    }

    /// 模拟用户打开了该会话的标签。
    fn user_tab(&self, name: &str) -> SessionId {
        let id = self
            .core
            .profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
            .id;
        self.core
            .open_session(OpenTarget::Profile(id), GRID)
            .unwrap()
    }

    fn audit_len(&self) -> usize {
        self.core
            .mcp_audit(McpAuditFilter::default())
            .unwrap()
            .len()
    }
}

struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Client {
    fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.reader.read_line(&mut line).unwrap() > 0, "closed");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// 调用工具，返回 `(isError, 解析后的 JSON)`。
    fn call(&mut self, tool: &str, args: Value) -> (bool, Value) {
        let resp = self.request("tools/call", json!({"name": tool, "arguments": args}));
        let result = &resp["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"].as_bool().unwrap_or(false),
            serde_json::from_str(text).unwrap(),
        )
    }

    fn ok(&mut self, tool: &str, args: Value) -> Value {
        let (err, v) = self.call(tool, args);
        assert!(!err, "{tool}: {v}");
        v
    }

    fn code(&mut self, tool: &str, args: Value) -> String {
        let (err, v) = self.call(tool, args);
        assert!(err, "{tool} should fail: {v}");
        v["error"]["code"].as_str().unwrap().to_owned()
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn read_only_sftp_never_connects_and_needs_user_is_reported() {
    let f = Fixture::started();
    let web = f.grant("web-01", McpAccess::ReadOnly);
    let mut c = f.client();

    // 只读级别、没有连接：NOT_CONNECTED，说明里告诉 AI 怎么办。
    let (err, v) = c.call("kanso_sftp_list", json!({"session": web, "path": "/"}));
    assert!(err);
    assert_eq!(v["error"]["code"], "NOT_CONNECTED");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("open this session in Kanso first")
    );

    // 用户打开标签后，同样的调用借用该连接成功。
    f.user_tab("web-01");
    let v = c.ok("kanso_sftp_list", json!({"session": web, "path": "/data"}));
    assert!(v["entries"].as_array().unwrap().len() >= 3);
    assert_eq!(v["next_cursor"], Value::Null);

    // 完全级别、需要 MFA 的会话：NEEDS_USER。
    let mfa = f.grant("mfa-bastion", McpAccess::Full);
    let (err, v) = c.call("kanso_sftp_list", json!({"session": mfa, "path": "/"}));
    assert!(err);
    assert_eq!(v["error"]["code"], "NEEDS_USER");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("keyboard-interactive")
    );
    assert_eq!(
        c.code("kanso_exec", json!({"session": mfa, "command": "id"})),
        "NEEDS_USER"
    );
    assert_eq!(
        c.code("kanso_open_tab", json!({"session": mfa})),
        "NEEDS_USER"
    );
}

#[test]
fn exec_results_and_rejections() {
    let f = Fixture::started();
    let web = f.grant("web-01", McpAccess::Full);
    let local = f.grant("Local", McpAccess::Full);
    let files = f.grant("files", McpAccess::Full);
    let ro = f.grant("mfa-bastion", McpAccess::ReadOnly);
    let off = f.profile("legacy-switch");
    let mut c = f.client();

    let v = c.ok(
        "kanso_exec",
        json!({"session": web, "command": "uname -a\nid"}),
    );
    assert!(
        v["stdout"]["text"]
            .as_str()
            .unwrap()
            .contains("mock exec: uname -a")
    );
    assert_eq!(v["stdout"]["truncated"], false);
    assert!(v["stdout"]["original_bytes"].as_u64().unwrap() > 0);
    assert_eq!(v["stderr"]["original_bytes"], 0);
    assert_eq!(v["exit_status"], 0);
    // 没有打开标签：界面上不出现新标签（无界面会话不在 sessions() 里）。
    assert!(f.core.sessions().is_empty());

    assert_eq!(
        c.code("kanso_exec", json!({"session": local, "command": "ls"})),
        "UNSUPPORTED"
    );
    assert_eq!(
        c.code("kanso_exec", json!({"session": files, "command": "ls"})),
        "UNSUPPORTED"
    );
    assert_eq!(
        c.code("kanso_exec", json!({"session": ro, "command": "ls"})),
        "ACCESS_DENIED"
    );
    assert_eq!(
        c.code("kanso_exec", json!({"session": off, "command": "ls"})),
        "NOT_FOUND"
    );
    assert_eq!(
        c.code(
            "kanso_exec",
            json!({"session": "not-a-uuid", "command": "ls"})
        ),
        "NOT_FOUND"
    );
    assert_eq!(
        c.code("kanso_exec", json!({"session": web, "command": "  "})),
        "INVALID_ARGUMENT"
    );

    // 命令文本进审计，输出不进；被拒绝的调用同样有记录。
    wait_until("audit", || f.audit_len() == 7);
    let records = f.core.mcp_audit(McpAuditFilter::default()).unwrap();
    let first = &records.last().unwrap().entry;
    assert_eq!(first.tool, "kanso_exec");
    assert!(first.args.contains("uname -a"), "{}", first.args);
    assert!(!first.args.contains("mock exec"));
    assert_eq!(
        records[0].entry.error_code.as_deref(),
        Some("INVALID_ARGUMENT")
    );
    // 无法解析的会话 id 没有 profile 可记，其余（含被拒绝的）都记了目标。
    assert_eq!(
        records.iter().filter(|r| r.entry.profile.is_some()).count(),
        6
    );
}

#[test]
fn close_tab_is_limited_to_the_clients_own_tabs() {
    let f = Fixture::started();
    let web = f.grant("web-01", McpAccess::Full);
    let user_tab = f.user_tab("web-01");
    let mut a = f.client();
    let mut b = f.client();

    let mine = a.ok("kanso_open_tab", json!({"session": web}))["tab"]
        .as_u64()
        .unwrap();
    assert_eq!(f.rec.opened.lock().unwrap().len(), 1);
    let theirs = b.ok("kanso_open_tab", json!({"session": web}))["tab"]
        .as_u64()
        .unwrap();

    let opened_by_mcp = |c: &mut Client, tab: u64| -> bool {
        let v = c.ok("kanso_list_sessions", json!({}));
        v["sessions"][0]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == tab)
            .unwrap()["opened_by_mcp"]
            .as_bool()
            .unwrap()
    };
    assert!(opened_by_mcp(&mut a, mine));
    assert!(!opened_by_mcp(&mut a, user_tab.raw()));

    // 用户的标签、另一个客户端打开的标签都拒绝关闭。
    assert_eq!(
        a.code("kanso_close_tab", json!({"tab": user_tab.raw()})),
        "ACCESS_DENIED"
    );
    assert_eq!(
        a.code("kanso_close_tab", json!({"tab": theirs})),
        "ACCESS_DENIED"
    );
    assert!(f.core.session_info(user_tab).is_some());

    // 自己打开的可以关闭，关闭后不能再关。
    a.ok("kanso_close_tab", json!({"tab": mine}));
    assert!(f.core.session_info(SessionId::from_raw(mine)).is_none());
    assert_eq!(a.code("kanso_close_tab", json!({"tab": mine})), "NOT_FOUND");

    // 客户端 B 断开：它打开的标签保留，回调 on_session_released，变成普通标签，任何人都不能再关。
    drop(b);
    wait_until("release callback", || {
        f.rec
            .released
            .lock()
            .unwrap()
            .contains(&SessionId::from_raw(theirs))
    });
    assert!(f.core.session_info(SessionId::from_raw(theirs)).is_some());
    assert!(!opened_by_mcp(&mut a, theirs));
    assert_eq!(
        a.code("kanso_close_tab", json!({"tab": theirs})),
        "ACCESS_DENIED"
    );

    // 只读会话不能打开标签。
    let ro = f.grant("web-01", McpAccess::ReadOnly);
    assert_eq!(
        a.code("kanso_open_tab", json!({"session": ro})),
        "ACCESS_DENIED"
    );
}

#[test]
fn sftp_read_write_roundtrip_and_limits() {
    let f = Fixture::started();
    let web = f.grant("web-01", McpAccess::Full);
    let mut c = f.client();

    // mock 的文件内容固定为 "mock file content\n"（18 字节）。
    let v = c.ok("kanso_sftp_read", json!({"session": web, "path": "/f"}));
    assert_eq!(v["content"], "mock file content\n");
    assert_eq!(v["bytes"], 18);
    assert_eq!(v["eof"], true);
    let v = c.ok(
        "kanso_sftp_read",
        json!({"session": web, "path": "/f", "length": 5}),
    );
    assert_eq!(v["content"], "mock ");
    assert_eq!(v["eof"], false);
    let v = c.ok(
        "kanso_sftp_read",
        json!({"session": web, "path": "/f", "encoding": "base64", "offset": 5, "length": 4}),
    );
    assert_eq!(v["content"], "ZmlsZQ==");
    assert_eq!(v["encoding"], "base64");
    assert_eq!(
        c.code(
            "kanso_sftp_read",
            json!({"session": web, "path": "/f", "length": 1048577})
        ),
        "TOO_LARGE"
    );
    assert_eq!(
        c.code("kanso_sftp_read", json!({"session": web, "path": ""})),
        "INVALID_ARGUMENT"
    );

    // 写：create_new 冲突、base64 与文本都成功、超过 8 MiB。
    assert_eq!(
        c.code(
            "kanso_sftp_write",
            json!({"session": web, "path": "/already-exists", "content": "x", "mode": "create_new"})
        ),
        "CONFLICT"
    );
    let v = c.ok("kanso_sftp_write", json!({"session": web, "path": "/b", "content": "AAEC/w==", "encoding": "base64", "mode": "overwrite"}));
    assert_eq!(v["written"], 4);
    assert_eq!(
        c.code("kanso_sftp_write", json!({"session": web, "path": "/b", "content": "!!", "encoding": "base64", "mode": "create_new"})),
        "INVALID_ARGUMENT"
    );
    let big = "a".repeat(8 * 1024 * 1024 + 1);
    assert_eq!(
        c.code(
            "kanso_sftp_write",
            json!({"session": web, "path": "/big", "content": big, "mode": "create_new"})
        ),
        "TOO_LARGE"
    );

    // 审计只记路径、模式与大小，不记内容。
    c.ok("kanso_sftp_write", json!({"session": web, "path": "/secret.txt", "content": "TOP-SECRET-CONTENT", "mode": "create_new"}));
    wait_until("audit", || f.audit_len() == 10);
    let records = f.core.mcp_audit(McpAuditFilter::default()).unwrap();
    let last = &records[0].entry;
    assert_eq!(last.tool, "kanso_sftp_write");
    assert!(
        last.args.contains("/secret.txt") && last.args.contains("create_new"),
        "{}",
        last.args
    );
    assert!(last.args.contains("size=18"), "{}", last.args);
    assert!(records.iter().all(|r| !r.entry.args.contains("TOP-SECRET")));
    assert!(
        records.iter().all(|r| r.entry.args.len() < 600),
        "超大内容不应进审计"
    );
}

#[test]
fn sftp_list_stat_mkdir_remove_rename() {
    let f = Fixture::started();
    let web = f.grant("web-01", McpAccess::Full);
    let local = f.grant("Local", McpAccess::Full);
    let files = f.grant("files", McpAccess::ReadOnly);
    let mut c = f.client();

    // 续页：mock 目录只有几项；cursor 跳过已返回的条数，非法 cursor 报参数错误。
    let all = c.ok("kanso_sftp_list", json!({"session": web, "path": "/data"}));
    let n = all["entries"].as_array().unwrap().len();
    let rest = c.ok(
        "kanso_sftp_list",
        json!({"session": web, "path": "/data", "cursor": "c2"}),
    );
    assert_eq!(rest["entries"].as_array().unwrap().len(), n - 2);
    assert_eq!(all["entries"][0]["permissions"], "0755");
    assert_eq!(
        c.code(
            "kanso_sftp_list",
            json!({"session": web, "path": "/data", "cursor": "zzz"})
        ),
        "INVALID_ARGUMENT"
    );

    let v = c.ok(
        "kanso_sftp_stat",
        json!({"session": web, "path": "/data/", "follow_symlinks": false}),
    );
    assert_eq!(v["is_dir"], true);

    // 本地会话一律 UNSUPPORTED；纯 SFTP 会话只读级别在没有连接时 NOT_CONNECTED。
    assert_eq!(
        c.code("kanso_sftp_list", json!({"session": local, "path": "/"})),
        "UNSUPPORTED"
    );
    assert_eq!(
        c.code("kanso_sftp_stat", json!({"session": files, "path": "/"})),
        "NOT_CONNECTED"
    );
    assert_eq!(
        c.code("kanso_sftp_remove", json!({"session": files, "path": "/x"})),
        "ACCESS_DENIED"
    );

    c.ok("kanso_sftp_mkdir", json!({"session": web, "path": "/new"}));
    // 逐级创建：mock 的 sftp_entry 对不以 `/` 结尾的路径返回"文件"，于是命中"已存在但不是目录"。
    assert_eq!(
        c.code(
            "kanso_sftp_mkdir",
            json!({"session": web, "path": "/a/b", "parents": true})
        ),
        "CONFLICT"
    );
    // 目录（路径以 `/` 结尾，mock 视为目录）与文件都能删除。
    c.ok(
        "kanso_sftp_remove",
        json!({"session": web, "path": "/empty-dir/"}),
    );
    c.ok(
        "kanso_sftp_remove",
        json!({"session": web, "path": "/file.txt"}),
    );
    // mock 里任何目标都"已存在"。
    assert_eq!(
        c.code(
            "kanso_sftp_rename",
            json!({"session": web, "from": "/a", "to": "/b"})
        ),
        "CONFLICT"
    );
}

#[test]
fn every_call_including_rejected_ones_is_audited() {
    let f = Fixture::started();
    let web = f.grant("web-01", McpAccess::ReadOnly);
    let mut c = f.client();
    let before = f.audit_len();
    let calls: [(&str, Value); 5] = [
        ("kanso_exec", json!({"session": web, "command": "ls"})),
        (
            "kanso_sftp_write",
            json!({"session": web, "path": "/x", "content": "c", "mode": "overwrite"}),
        ),
        ("kanso_sftp_remove", json!({"session": web, "path": "/x"})),
        ("kanso_open_tab", json!({"session": web})),
        ("kanso_close_tab", json!({"tab": 424242})),
    ];
    for (tool, args) in &calls {
        let (err, _) = c.call(tool, args.clone());
        assert!(err, "{tool}");
    }
    wait_until("audit", || f.audit_len() == before + calls.len());
    let records = f.core.mcp_audit(McpAuditFilter::default()).unwrap();
    let denied = records
        .iter()
        .filter(|r| r.entry.error_code.as_deref() == Some("ACCESS_DENIED"))
        .count();
    assert_eq!(denied, 4);
}
