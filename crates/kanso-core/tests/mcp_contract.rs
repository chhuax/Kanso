//! MCP 契约测试（architecture §16.3）。
//!
//! - 真实 core：授权、总开关、审计日志落在真实 SQLite（临时目录）里，变更会通知订阅者；
//!   F9 才实现的方法不 panic。
//! - mock core：连接复用、借用释放、MFA 主机拒绝新建连接、无界面会话不出现在 `sessions()` 里，
//!   供 `kanso-mcp-server` 在没有真实网络时测试。

use std::sync::Arc;
use std::time::Duration;

use kanso_core::mcp::{
    ConnectMode, McpConnectOutcome, McpInput, SftpWriteMode, TerminalBusy, TerminalRunOutcome,
    TerminalSendOutcome, TextScope, UserNeed, WaitLimits,
};
use kanso_core::sender::SenderRequest;
use kanso_core::{
    CollectingEventSink, Core, CoreConfig, CoreError, OpenTarget, Platform, XmodemBlock,
};
use kanso_types::event::SessionState;
use kanso_types::mcp::{McpAccess, McpAuditEntry, McpAuditFilter};
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, LineEnding, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::{CoreEvent, GridSize, GroupId, Message, ProfileId, PromptResponse, SessionId};

const GRID: GridSize = GridSize { cols: 80, rows: 24 };

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn real_core(tmp: &tempfile::TempDir) -> Core {
    let sink = Arc::new(CollectingEventSink::new());
    Core::new_with_base_env(
        CoreConfig::new(tmp.path().join("data"), "0.0.0-test", "test.kanso"),
        Platform::for_tests(sink),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
    )
    .unwrap()
}

fn mock_core() -> Core {
    Core::new_mock(Platform::for_tests(Arc::new(CollectingEventSink::new())))
}

fn ssh_profile(name: &str, group: Option<GroupId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group,
        kind: ProfileKind::Ssh(SshProfile {
            target: SshTarget {
                host: "example.com".into(),
                port: 22,
                user: "root".into(),
                auth: AuthMethod::Password,
                private_key_path: None,
                jump: None,
                proxy_command: None,
                keepalive: kanso_types::profile::KeepaliveConfig::default(),
            },
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

fn audit(at_ms: i64, error: Option<&str>) -> McpAuditEntry {
    McpAuditEntry {
        at_ms,
        client: "claude-code".into(),
        mcp_session: "1".into(),
        tool: "kanso_exec".into(),
        profile: None,
        session: Some(SessionId::from_raw(3)),
        args: "uname -a".into(),
        error_code: error.map(str::to_owned),
        duration_ms: 5,
        bytes: 10,
    }
}

fn mock_profile(core: &Core, name: &str) -> ProfileId {
    core.profiles()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("mock seed profile {name} missing"))
        .id
}

// ---- 真实 core ----

#[test]
fn real_grants_and_switch_persist_across_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let p = ssh_profile("web", None);
    {
        let core = real_core(&tmp);
        assert!(!core.mcp_enabled());
        core.save_profile(p.clone()).unwrap();
        assert_eq!(core.mcp_access(p.id), McpAccess::Off);
        core.set_mcp_enabled(true).unwrap();
        core.set_mcp_access(p.id, McpAccess::Full).unwrap();
        core.shutdown();
    }
    let core = real_core(&tmp);
    assert!(core.mcp_enabled());
    assert_eq!(core.mcp_access(p.id), McpAccess::Full);
    assert_eq!(core.mcp_accesses().unwrap(), vec![(p.id, McpAccess::Full)]);
    core.shutdown();
}

#[test]
fn real_set_access_on_unknown_profile_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let core = real_core(&tmp);
    let ghost = ProfileId::new();
    assert_eq!(
        core.set_mcp_access(ghost, McpAccess::ReadOnly),
        Err(CoreError::NoSuchProfile(ghost))
    );
    core.shutdown();
}

#[test]
fn real_policy_watch_fires_on_every_relevant_change() {
    let tmp = tempfile::tempdir().unwrap();
    let core = real_core(&tmp);
    let rt = rt();
    let mut watch = core.mcp_policy_watch();
    let start = *watch.borrow_and_update();

    let p = ssh_profile("web", None);
    core.save_profile(p.clone()).unwrap();
    // 普通的配置保存与 MCP 授权无关，不通知。
    assert!(!watch.has_changed().unwrap());

    core.set_mcp_enabled(true).unwrap();
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();

    core.set_mcp_access(p.id, McpAccess::Full).unwrap();
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();

    // 失败的设置不通知。
    let _ = core.set_mcp_access(ProfileId::new(), McpAccess::Full);
    assert!(!watch.has_changed().unwrap());

    rt.block_on(core.delete_profile(p.id)).unwrap();
    assert!(watch.has_changed().unwrap());
    assert_eq!(core.mcp_access(p.id), McpAccess::Off);
    assert!(*watch.borrow_and_update() > start);

    let g = Group {
        id: GroupId::new(),
        name: "prod".into(),
    };
    core.save_group(g.clone()).unwrap();
    let q = ssh_profile("db", Some(g.id));
    core.save_profile(q.clone()).unwrap();
    core.set_mcp_access(q.id, McpAccess::ReadOnly).unwrap();
    watch.borrow_and_update();
    rt.block_on(core.delete_group(g.id)).unwrap();
    assert!(watch.has_changed().unwrap());
    assert_eq!(core.mcp_access(q.id), McpAccess::Off);
    core.shutdown();
}

#[test]
fn real_audit_append_query_prune_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let core = real_core(&tmp);
    core.mcp_audit_append(vec![audit(1, None), audit(2, Some("ACCESS_DENIED"))])
        .unwrap();
    let all = core.mcp_audit(McpAuditFilter::default()).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].entry, audit(2, Some("ACCESS_DENIED")));
    let errors = core
        .mcp_audit(McpAuditFilter {
            errors_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(errors.len(), 1);
    // 两条都早于 30 天前，全部清掉。
    let now = 40 * 24 * 60 * 60 * 1000;
    assert_eq!(core.mcp_audit_prune(now).unwrap(), 2);
    core.mcp_audit_append(vec![audit(now, None)]).unwrap();
    core.mcp_audit_clear().unwrap();
    assert!(
        core.mcp_audit(McpAuditFilter::default())
            .unwrap()
            .is_empty()
    );
    core.shutdown();
}

#[test]
fn real_mcp_methods_on_missing_targets_do_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let core = real_core(&tmp);
    let rt = rt();
    let s = SessionId::from_raw(999);
    let p = ProfileId::new();
    let limits = WaitLimits::default();
    assert_eq!(core.mcp_session_status(s), None);
    // 终端的读取、命令与输入已实现（行为见 real_core.rs）：不存在的会话是 NoSuchSession。
    assert_eq!(
        core.terminal_text(s, TextScope::Screen, 1024),
        Err(CoreError::NoSuchSession(s))
    );
    assert_eq!(
        rt.block_on(core.mcp_terminal_run(s, "ls", limits)),
        Err(CoreError::NoSuchSession(s))
    );
    assert_eq!(
        rt.block_on(core.mcp_terminal_send(s, vec![McpInput::Text("q".into())], None, 1024)),
        Err(CoreError::NoSuchSession(s))
    );
    assert_eq!(
        rt.block_on(core.mcp_connect(p, ConnectMode::ReuseOnly)),
        Err(CoreError::NoSuchProfile(p))
    );
    assert_eq!(
        rt.block_on(core.mcp_open_session(p, ConnectMode::NonInteractive, GRID)),
        Err(CoreError::NoSuchProfile(p))
    );
    assert_eq!(
        rt.block_on(core.mcp_exec(s, "ls", limits)),
        Err(CoreError::NoSuchSession(s))
    );
    // SFTP 补充操作已实现（行为见 real_sftp.rs）：不存在的会话是 NoSuchSession。
    assert_eq!(
        rt.block_on(core.sftp_entry(s, "/", false)),
        Err(CoreError::NoSuchSession(s))
    );
    assert_eq!(
        rt.block_on(core.sftp_read(s, "/x", 0, 10)),
        Err(CoreError::NoSuchSession(s))
    );
    assert_eq!(
        rt.block_on(core.sftp_write(s, "/x", vec![], SftpWriteMode::CreateNew)),
        Err(CoreError::NoSuchSession(s))
    );
    assert_eq!(
        rt.block_on(core.sftp_rmdir(s, "/x")),
        Err(CoreError::NoSuchSession(s))
    );
    core.shutdown();
}

// ---- mock core ----

#[test]
fn mock_access_rules_match_real_store() {
    let core = mock_core();
    let web = mock_profile(&core, "web-01");
    assert_eq!(core.mcp_access(web), McpAccess::Off);
    core.set_mcp_access(web, McpAccess::ReadOnly).unwrap();
    assert_eq!(
        core.mcp_accesses().unwrap(),
        vec![(web, McpAccess::ReadOnly)]
    );
    core.set_mcp_access(web, McpAccess::Off).unwrap();
    assert!(core.mcp_accesses().unwrap().is_empty());
    let ghost = ProfileId::new();
    assert_eq!(
        core.set_mcp_access(ghost, McpAccess::Full),
        Err(CoreError::NoSuchProfile(ghost))
    );
}

#[test]
fn mock_connect_reuse_only_and_mfa_need_user() {
    let core = mock_core();
    let rt = rt();
    let web = mock_profile(&core, "web-01");
    let mfa = mock_profile(&core, "mfa-bastion");

    assert_eq!(
        rt.block_on(core.mcp_connect(web, ConnectMode::ReuseOnly)),
        Ok(McpConnectOutcome::NotConnected)
    );
    assert_eq!(
        rt.block_on(core.mcp_connect(mfa, ConnectMode::NonInteractive)),
        Ok(McpConnectOutcome::NeedsUser(UserNeed::KeyboardInteractive))
    );
    assert_eq!(
        rt.block_on(core.mcp_open_session(mfa, ConnectMode::NonInteractive, GRID)),
        Ok(McpConnectOutcome::NeedsUser(UserNeed::KeyboardInteractive))
    );

    // 自建的无界面会话：不出现在 sessions() 里，但能查到状态、能执行。
    let Ok(McpConnectOutcome::Connected { session, borrowed }) =
        rt.block_on(core.mcp_connect(web, ConnectMode::NonInteractive))
    else {
        panic!("expected connected");
    };
    assert!(!borrowed);
    assert!(core.sessions().iter().all(|s| s.id != session));
    let status = core.mcp_session_status(session).unwrap();
    assert!(status.headless);
    let out = rt
        .block_on(core.mcp_exec(session, "uname -a", WaitLimits::default()))
        .unwrap();
    assert_eq!(out.stdout.text, "mock exec: uname -a\n");
    assert_eq!(out.exit_status, Some(0));
    // 无界面会话不是终端。
    assert_eq!(
        core.terminal_text(session, TextScope::Screen, 1024),
        Err(CoreError::NotSupported)
    );
}

#[test]
fn mock_borrowed_session_released_with_last_user_tab() {
    let core = mock_core();
    let rt = rt();
    let web = mock_profile(&core, "web-01");
    let tab = core.open_session(OpenTarget::Profile(web), GRID).unwrap();
    assert_eq!(
        core.session_info(tab).unwrap().state,
        SessionState::Connected
    );

    let Ok(McpConnectOutcome::Connected { session, borrowed }) =
        rt.block_on(core.mcp_connect(web, ConnectMode::ReuseOnly))
    else {
        panic!("expected borrowed connection");
    };
    assert!(borrowed);
    assert!(core.mcp_session_status(session).is_some());

    core.close_session(tab);
    // 用户关掉最后一个标签，借用的无界面会话随之关闭（spec/mcp.md §2.4）。
    assert!(core.mcp_session_status(session).is_none());
    assert_eq!(
        rt.block_on(core.mcp_connect(web, ConnectMode::ReuseOnly)),
        Ok(McpConnectOutcome::NotConnected)
    );
}

#[test]
fn mock_open_session_reuses_live_connection() {
    let core = mock_core();
    let rt = rt();
    let web = mock_profile(&core, "web-01");
    let first = core.open_session(OpenTarget::Profile(web), GRID).unwrap();
    let Ok(McpConnectOutcome::Connected { session, borrowed }) =
        rt.block_on(core.mcp_open_session(web, ConnectMode::ReuseOnly, GRID))
    else {
        panic!("expected a duplicated tab");
    };
    assert!(borrowed);
    assert_ne!(session, first);
    // MCP 打开的标签是普通标签，会出现在 sessions() 里。
    assert!(core.sessions().iter().any(|s| s.id == session));
}

#[test]
fn mock_terminal_run_send_and_read() {
    let core = mock_core();
    let rt = rt();
    let local = core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap();
    let limits = WaitLimits {
        timeout: Duration::from_secs(1),
        max_bytes: 1024,
    };

    match rt
        .block_on(core.mcp_terminal_run(local, "ls", limits))
        .unwrap()
    {
        TerminalRunOutcome::Completed { output, exit_code } => {
            assert_eq!(output.text, "mock output: ls\n");
            assert_eq!(exit_code, Some(0));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        rt.block_on(core.mcp_terminal_run(local, "mock-hang 100", limits)),
        Ok(TerminalRunOutcome::Unconfirmed { osc133: true, .. })
    ));
    assert_eq!(
        rt.block_on(core.mcp_terminal_run(local, "ls\nrm -rf /", limits)),
        Err(CoreError::NotSupported)
    );

    let sent = rt
        .block_on(core.mcp_terminal_send(
            local,
            vec![McpInput::Text("q".into())],
            Some(Duration::from_millis(10)),
            1024,
        ))
        .unwrap();
    assert!(matches!(
        sent,
        TerminalSendOutcome::Sent { screen: Some(_) }
    ));

    let text = core
        .terminal_text(local, TextScope::Recent { lines: 200 }, 1024)
        .unwrap();
    assert!(text.content.text.starts_with("mock terminal:"));

    core.close_session(local);
    assert_eq!(
        rt.block_on(core.mcp_terminal_run(local, "ls", limits)),
        Err(CoreError::NoSuchSession(local))
    );
}

#[test]
fn mock_closed_tab_rejects_run() {
    let core = mock_core();
    let rt = rt();
    let mfa = mock_profile(&core, "mfa-bastion");
    // mfa-bastion 打开后停在提问上，没有连上。
    let tab = core.open_session(OpenTarget::Profile(mfa), GRID).unwrap();
    assert_eq!(
        rt.block_on(core.mcp_terminal_run(tab, "ls", WaitLimits::default())),
        Ok(TerminalRunOutcome::Rejected(TerminalBusy::NotConnected))
    );
}

#[test]
fn mock_sftp_write_create_new_refuses_existing() {
    let core = mock_core();
    let rt = rt();
    let web = mock_profile(&core, "web-01");
    let Ok(McpConnectOutcome::Connected { session, .. }) =
        rt.block_on(core.mcp_connect(web, ConnectMode::NonInteractive))
    else {
        panic!("expected connected");
    };
    assert_eq!(
        rt.block_on(core.sftp_write(session, "/tmp/exists", vec![1], SftpWriteMode::CreateNew)),
        Err(CoreError::User(Message::SftpFileExists {
            path: "/tmp/exists".into()
        }))
    );
    rt.block_on(core.sftp_write(session, "/tmp/exists", vec![1], SftpWriteMode::Overwrite))
        .unwrap();
    let bytes = rt.block_on(core.sftp_read(session, "/x", 5, 4)).unwrap();
    assert_eq!(bytes, b"file");
}

#[test]
fn mock_transfer_and_wait_completes_and_can_be_cancelled() {
    use kanso_types::event::TransferOutcome;
    use kanso_types::transfer::ConflictPolicy;
    let (core, sink) = mock_core_with_sink();
    let rt = rt();
    let web = mock_profile(&core, "web-01");
    let Ok(McpConnectOutcome::Connected { session, .. }) =
        rt.block_on(core.mcp_connect(web, ConnectMode::NonInteractive))
    else {
        panic!("expected connected");
    };
    // 正常完成：下载与上传都等到 Completed，事件照常发出。
    assert_eq!(
        rt.block_on(core.download_and_wait(session, "/a/f.txt", "/tmp", ConflictPolicy::Fail)),
        Ok(TransferOutcome::Completed { skipped: 0 })
    );
    assert_eq!(
        rt.block_on(core.upload_and_wait(session, "/tmp/f.txt", "/a", ConflictPolicy::Fail)),
        Ok(TransferOutcome::Completed { skipped: 0 })
    );
    let finished = sink
        .take()
        .iter()
        .filter(|e| matches!(e, CoreEvent::TransferFinished { .. }))
        .count();
    assert_eq!(finished, 2);

    // 等待期间取消：从 TransferStarted 拿 id。
    let ghost = SessionId::from_raw(9999);
    assert_eq!(
        rt.block_on(core.download_and_wait(ghost, "/a", "/tmp", ConflictPolicy::Fail)),
        Err(CoreError::NoSuchSession(ghost))
    );
    let out = std::thread::scope(|sc| {
        let h = sc.spawn(|| {
            rt.block_on(core.upload_and_wait(session, "/tmp/g.txt", "/a", ConflictPolicy::Fail))
        });
        let id = loop {
            let found = sink.take().into_iter().find_map(|e| match e {
                CoreEvent::TransferStarted { transfer, name, .. } if name == "g.txt" => {
                    Some(transfer)
                }
                _ => None,
            });
            if let Some(id) = found {
                break id;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        core.cancel_transfer(id);
        h.join().unwrap()
    });
    assert_eq!(out, Ok(TransferOutcome::Cancelled));
}

// ---- 评审修复的回归测试 ----

fn mock_core_with_sink() -> (Core, Arc<CollectingEventSink>) {
    let sink = Arc::new(CollectingEventSink::new());
    (Core::new_mock(Platform::for_tests(sink.clone())), sink)
}

#[test]
fn mock_open_session_on_live_mfa_tab_never_prompts() {
    let (core, sink) = mock_core_with_sink();
    let rt = rt();
    let mfa = mock_profile(&core, "mfa-bastion");
    let tab = core.open_session(OpenTarget::Profile(mfa), GRID).unwrap();
    let prompt = sink
        .take()
        .into_iter()
        .find_map(|e| match e {
            CoreEvent::Prompt { prompt, .. } => Some(prompt),
            _ => None,
        })
        .expect("用户自己打开 MFA 主机时要提问");
    core.respond_prompt(prompt, PromptResponse::Answers(vec!["123456".into()]));
    assert_eq!(
        core.session_info(tab).unwrap().state,
        SessionState::Connected
    );
    sink.take();

    // 用户已经完成验证：MCP 打开新标签复用这条连接，不再提问，新标签直接是已连接（spec/mcp.md 验收 12）。
    let Ok(McpConnectOutcome::Connected { session, borrowed }) =
        rt.block_on(core.mcp_open_session(mfa, ConnectMode::NonInteractive, GRID))
    else {
        panic!("expected reuse of the verified connection");
    };
    assert!(borrowed);
    assert!(
        !sink
            .take()
            .iter()
            .any(|e| matches!(e, CoreEvent::Prompt { .. })),
        "non-interactive open must not prompt"
    );
    assert_eq!(
        core.session_info(session).unwrap().state,
        SessionState::Connected
    );
}

#[test]
fn mock_terminal_transfer_rejects_run_and_send() {
    let core = mock_core();
    let rt = rt();
    let web = mock_profile(&core, "web-01");
    let tab = core.open_session(OpenTarget::Profile(web), GRID).unwrap();
    core.xmodem_send(tab, "/tmp/file.bin", XmodemBlock::Bytes1K)
        .unwrap();
    assert!(core.mcp_session_status(tab).unwrap().terminal_transfer);
    assert_eq!(
        rt.block_on(core.mcp_terminal_run(tab, "ls", WaitLimits::default())),
        Ok(TerminalRunOutcome::Rejected(TerminalBusy::Transferring))
    );
    assert_eq!(
        rt.block_on(core.mcp_terminal_send(tab, vec![McpInput::Text("q".into())], None, 1024)),
        Ok(TerminalSendOutcome::Rejected(TerminalBusy::Transferring))
    );
    core.cancel_terminal_transfer(tab);
}

#[test]
fn mock_audit_ids_are_never_reused() {
    let core = mock_core();
    core.mcp_audit_append(vec![audit(1, None), audit(2, None)])
        .unwrap();
    let last = core.mcp_audit(McpAuditFilter::default()).unwrap()[0].id;
    core.mcp_audit_clear().unwrap();
    core.mcp_audit_append(vec![audit(3, None)]).unwrap();
    // 与 SQLite AUTOINCREMENT 一致：清空后 id 继续增长，旧的分页游标不会把新记录当成旧记录。
    assert!(core.mcp_audit(McpAuditFilter::default()).unwrap()[0].id > last);
}

#[test]
fn sender_and_mcp_run_exclude_each_other_through_core() {
    let core = mock_core();
    let rt = rt();
    let tab = core
        .open_session(OpenTarget::DefaultLocalShell, GRID)
        .unwrap();
    core.sender_start(
        SenderRequest {
            text: "echo one\necho two".into(),
            ending: LineEnding::Lf,
            target: Some(tab),
            repeating: false,
            interval_seconds: 1,
            count: 1,
        },
        vec![tab],
    )
    .unwrap();
    assert!(core.mcp_session_status(tab).unwrap().sender_active);
    assert_eq!(
        rt.block_on(core.mcp_terminal_run(tab, "ls", WaitLimits::default())),
        Ok(TerminalRunOutcome::Rejected(TerminalBusy::SenderActive))
    );
    core.sender_stop(false);
    // Sender 停止后可以执行；执行结束后占用即释放，可以再次执行。
    // 执行中途被取消时的释放靠 `McpClaim` 的 Drop 保证；mock 一次轮询就完成，测不到"中途"，
    // 留给 F9 的真实实现用会挂起的命令来测。
    assert!(matches!(
        rt.block_on(core.mcp_terminal_run(tab, "ls", WaitLimits::default())),
        Ok(TerminalRunOutcome::Completed { .. })
    ));
    assert!(matches!(
        rt.block_on(core.mcp_terminal_run(tab, "ls", WaitLimits::default())),
        Ok(TerminalRunOutcome::Completed { .. })
    ));
}
