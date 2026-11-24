//! SFTP 客户端与传输引擎（ssh.md §7–§9、§12）。服务器是进程内的 SFTP 测试服务器，
//! 文件系统是临时目录，不连真实主机、不碰用户文件。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_types::transfer::ConflictPolicy;
use tempfile::TempDir;

use super::server::{Policy, ServerConfig, TestServer};
use super::support::{Scripted, connect_ok, eventually};
use crate::sftp::{
    BufferGauge, CancelToken, SftpClient, SftpError, SkipReason, StatusKind, TransferError,
    TransferOptions, TransferOutcome, TransferProgress, TransferReport,
};
use crate::test_sftp_server::SftpServerConfig;
use crate::{AuthMethod, Connection};

struct Env {
    server: TestServer,
    client: SftpClient,
    conn: Connection,
    /// 虚拟根对应的临时目录。
    root: TempDir,
    /// 本地侧的临时目录（下载目标 / 上传源）。
    local: TempDir,
}

impl Env {
    fn remote(&self, p: &str) -> PathBuf {
        self.root.path().join(p.trim_start_matches('/'))
    }
    fn local(&self, p: &str) -> PathBuf {
        self.local.path().join(p)
    }
}

async fn env_with(tweak: impl FnOnce(&mut SftpServerConfig)) -> Env {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home/tester")).unwrap();
    let mut cfg = SftpServerConfig::new(root.path());
    cfg.home = "/home/tester".into();
    tweak(&mut cfg);
    let server = TestServer::start(ServerConfig {
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        sftp: Some(cfg),
        ..ServerConfig::default()
    })
    .await;
    let conn = connect_ok(&server, AuthMethod::Password, &Scripted::new())
        .await
        .connection;
    let client = SftpClient::new(conn.clone());
    Env {
        server,
        client,
        conn,
        root,
        local: TempDir::new().unwrap(),
    }
}

async fn env() -> Env {
    env_with(|_| {}).await
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

/// 服务器处理到第 `n` 个 READ（`write` 为假）/ WRITE 之后取消传输。
///
/// 不用进度回调触发取消：回调被节流到 50ms，传输快的话根本不会在途中被调用。
fn cancel_after(e: &Env, n: usize, write: bool) -> CancelToken {
    let token = CancelToken::new();
    let t = token.clone();
    let stats = e.server.sftp_stats.clone();
    tokio::spawn(async move {
        loop {
            let c = if write { &stats.writes } else { &stats.reads };
            if c.load(Ordering::SeqCst) >= n {
                t.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    token
}

fn completed(r: &TransferReport) {
    assert_eq!(r.outcome, TransferOutcome::Completed, "{r:?}");
}

// ---------------------------------------------------------------------------
// 客户端基本操作

#[tokio::test]
async fn subsystem_opens_lazily_on_first_request_and_is_reused() {
    let e = env().await;
    // 连接并认证之后、第一次文件请求之前，不开子系统。
    assert_eq!(
        e.server.sftp_stats.subsystem_opens.load(Ordering::SeqCst),
        0
    );
    assert_eq!(e.client.home().await.unwrap(), "/home/tester");
    assert_eq!(
        e.server.sftp_stats.subsystem_opens.load(Ordering::SeqCst),
        1
    );
    e.client.mkdir("/home/tester/a").await.unwrap();
    e.client.stat("/home/tester/a").await.unwrap();
    assert_eq!(
        e.server.sftp_stats.subsystem_opens.load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn concurrent_first_requests_open_one_subsystem() {
    let e = env().await;
    let c = &e.client;
    let (a, b, d) = tokio::join!(c.home(), c.home(), c.stat("/"));
    a.unwrap();
    b.unwrap();
    d.unwrap();
    assert_eq!(
        e.server.sftp_stats.subsystem_opens.load(Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn subsystem_refused_maps_to_subsystem_failed_message() {
    // 没有启用 SFTP 的服务器会拒绝子系统请求。
    let server = TestServer::start(ServerConfig {
        policy: Policy {
            none_ok: true,
            ..Policy::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let conn = connect_ok(&server, AuthMethod::Password, &Scripted::new())
        .await
        .connection;
    let err = SftpClient::new(conn).home().await.unwrap_err();
    assert!(matches!(err, SftpError::SubsystemFailed { .. }), "{err:?}");
    assert_eq!(err.to_message().key(), "sftp-subsystem-failed");
}

#[tokio::test]
async fn list_streams_first_batch_before_directory_is_fully_read() {
    // 350 项、每批 100 项、每个 READDIR 延迟 150ms：第一批应在 ~150ms 到达，整个目录要 600ms 以上。
    let e = env_with(|c| {
        c.readdir_batch = 100;
        c.readdir_delay = Duration::from_millis(150);
    })
    .await;
    for i in 0..350 {
        std::fs::write(e.remote(&format!("home/tester/f{i:03}")), b"x").unwrap();
    }
    let start = Instant::now();
    let mut listing = e.client.list(".").await.unwrap();
    let first = listing.next_batch().await.unwrap().unwrap();
    let t_first = start.elapsed();
    // 一批 100 项里含 `.` 与 `..`，被过滤后少两项。
    assert!((90..=100).contains(&first.len()), "{}", first.len());
    assert!(
        t_first < Duration::from_millis(400),
        "第一批来得太晚：{t_first:?}"
    );
    let mut total = first.len();
    let mut batches = 1;
    while let Some(b) = listing.next_batch().await.unwrap() {
        total += b.len();
        batches += 1;
    }
    assert_eq!(total, 350);
    assert_eq!(batches, 4); // (350 + 2) / 100 向上取整
    assert!(start.elapsed() > t_first + Duration::from_millis(300));
    // 读完后句柄已关闭。
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

// 测试服务器的目录在本机文件系统上，Windows 建符号链接需要特权，只在 Unix 上运行。
#[cfg(unix)]
#[tokio::test]
async fn completion_directory_limit_does_not_stat_excess_links() {
    let e = env_with(|config| config.readdir_batch = 2500).await;
    for index in 0..2100 {
        std::os::unix::fs::symlink("missing", e.remote(&format!("home/tester/link{index:04}")))
            .unwrap();
    }
    let mut listing = e.client.list(".").await.unwrap();
    let before = e.server.sftp_stats.requests.load(Ordering::SeqCst);
    let entries = listing.next_batch_limited(2000).await.unwrap().unwrap();
    assert_eq!(entries.len(), 2000);
    assert!(entries.iter().all(|entry| entry.is_symlink));
    let requests = e.server.sftp_stats.requests.load(Ordering::SeqCst) - before;
    assert_eq!(requests, 2001);
    drop(listing);
    eventually(|| e.server.sftp_stats.open_handles() == 0).await;
}

#[tokio::test]
async fn list_skips_dot_entries_and_fills_fields() {
    let e = env_with(|c| c.owner_names = Some(("alice".into(), "staff".into()))).await;
    std::fs::write(e.remote("home/tester/a.txt"), b"hello").unwrap();
    std::fs::create_dir(e.remote("home/tester/d")).unwrap();
    let all = e
        .client
        .list("/home/tester")
        .await
        .unwrap()
        .collect_all()
        .await
        .unwrap();
    let names: Vec<_> = all.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["a.txt", "d"]);
    let f = &all[0];
    assert_eq!(f.path, "/home/tester/a.txt");
    assert!(!f.is_dir && !f.is_symlink);
    assert_eq!(f.size, 5);
    assert!(f.mtime_ms.unwrap() > 1_000_000_000_000);
    assert!(f.permissions.is_some());
    // longname 里有用户名时优先采用。
    assert_eq!(f.owner.as_deref(), Some("alice"));
    assert_eq!(f.group.as_deref(), Some("staff"));
    assert!(all[1].is_dir);
}

#[tokio::test]
async fn list_owner_falls_back_to_numeric_ids() {
    let e = env().await; // owner_names = None：longname 里是数字
    std::fs::write(e.remote("home/tester/a"), b"x").unwrap();
    let all = e
        .client
        .list("/home/tester")
        .await
        .unwrap()
        .collect_all()
        .await
        .unwrap();
    let owner = all[0].owner.as_deref().unwrap();
    assert!(owner.chars().all(|c| c.is_ascii_digit()), "{owner}");
}

#[tokio::test]
async fn list_uses_original_path_when_realpath_fails() {
    let e = env_with(|c| c.realpath_fails = true).await;
    std::fs::write(e.remote("home/tester/a"), b"x").unwrap();
    let all = e
        .client
        .list("/home/tester")
        .await
        .unwrap()
        .collect_all()
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].path, "/home/tester/a");
}

#[tokio::test]
async fn list_missing_dir_is_not_found_error() {
    let e = env().await;
    let err = e.client.list("/nope").await.err().unwrap();
    assert!(err.is_not_found(), "{err:?}");
    assert_eq!(err.to_message().key(), "sftp-op-failed");
}

#[cfg(unix)]
#[tokio::test]
async fn list_reports_symlink_flags_and_target_kind() {
    use std::os::unix::fs::symlink;
    let e = env().await;
    let h = e.remote("home/tester");
    std::fs::create_dir(h.join("realdir")).unwrap();
    std::fs::write(h.join("realfile"), b"abc").unwrap();
    symlink("realdir", h.join("link-to-dir")).unwrap();
    symlink("realfile", h.join("link-to-file")).unwrap();
    symlink("missing", h.join("dangling")).unwrap();
    let all = e
        .client
        .list("/home/tester")
        .await
        .unwrap()
        .collect_all()
        .await
        .unwrap();
    let get = |n: &str| all.iter().find(|x| x.name == n).unwrap();
    assert!(!get("realdir").is_symlink && get("realdir").is_dir);
    assert!(!get("realfile").is_symlink && !get("realfile").is_dir);
    assert!(get("link-to-dir").is_symlink && get("link-to-dir").is_dir);
    assert!(get("link-to-file").is_symlink && !get("link-to-file").is_dir);
    // 悬空链接：仍作为链接列出，不当目录。
    assert!(get("dangling").is_symlink && !get("dangling").is_dir);
}

#[tokio::test]
async fn mkdir_remove_rename() {
    let e = env().await;
    let c = &e.client;
    c.mkdir("/home/tester/d").await.unwrap();
    assert!(e.remote("home/tester/d").is_dir());
    // 已存在：失败。
    assert!(c.mkdir("/home/tester/d").await.is_err());

    c.create_file("/home/tester/d/f").await.unwrap();
    c.rename("/home/tester/d/f", "/home/tester/d/g")
        .await
        .unwrap();
    assert!(!e.remote("home/tester/d/f").exists() && e.remote("home/tester/d/g").exists());

    // 文件与目录分开删除；非空目录删不掉。
    assert!(c.remove_dir("/home/tester/d").await.is_err());
    c.remove_file("/home/tester/d/g").await.unwrap();
    c.remove_dir("/home/tester/d").await.unwrap();
    assert!(!e.remote("home/tester/d").exists());

    let err = c.remove_file("/home/tester/none").await.unwrap_err();
    assert!(err.is_not_found());
}

#[tokio::test]
async fn permission_denied_maps_to_reason() {
    let e = env_with(|c| c.deny_prefixes = vec!["/home/tester/locked".into()]).await;
    std::fs::create_dir(e.remote("home/tester/locked")).unwrap();
    let err = e.client.mkdir("/home/tester/locked/x").await.unwrap_err();
    assert!(matches!(
        err,
        SftpError::Status {
            kind: StatusKind::PermissionDenied,
            ..
        }
    ));
    assert_eq!(err.to_message().key(), "sftp-op-failed");
}

#[tokio::test]
async fn create_file_refuses_to_truncate_existing() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/keep"), b"precious").unwrap();
    let err = e.client.create_file("/home/tester/keep").await.unwrap_err();
    assert!(err.is_already_exists(), "{err:?}");
    assert_eq!(err.to_message().key(), "sftp-file-exists");
    assert_eq!(
        std::fs::read(e.remote("home/tester/keep")).unwrap(),
        b"precious"
    );
    // 不存在时创建空文件。
    e.client.create_file("/home/tester/new").await.unwrap();
    assert_eq!(std::fs::read(e.remote("home/tester/new")).unwrap(), b"");
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[tokio::test]
async fn dropping_a_listing_early_closes_its_handle() {
    let e = env_with(|c| c.readdir_batch = 2).await;
    for i in 0..10 {
        std::fs::write(e.remote(&format!("home/tester/f{i}")), b"x").unwrap();
    }
    let mut l = e.client.list("/home/tester").await.unwrap();
    l.next_batch().await.unwrap().unwrap();
    assert_eq!(e.server.sftp_stats.open_handles(), 1);
    drop(l);
    eventually(|| e.server.sftp_stats.open_handles() == 0).await;
}

#[tokio::test]
async fn connection_loss_fails_in_flight_requests() {
    // 在途的 READ 因服务器断开通道而以错误结束，而不是永远挂着。
    let e = env_with(|c| c.disconnect_after_reads = Some(0)).await;
    std::fs::write(e.remote("home/tester/f"), pattern(100_000)).unwrap();
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("f"), &TransferOptions::default())
        .await;
    assert!(
        matches!(
            r.outcome,
            TransferOutcome::Failed(TransferError::Sftp(SftpError::Disconnected))
        ),
        "{r:?}"
    );
    assert!(!e.local("f").exists());
    let _ = &e.conn;
}

// ---------------------------------------------------------------------------
// 单文件传输

const SIZES: &[usize] = &[0, 1, 100, 32767, 32768, 32769, 65536, 1_000_003];

#[tokio::test]
async fn download_is_byte_identical_for_awkward_sizes() {
    let e = env().await;
    for &n in SIZES {
        let data = pattern(n);
        std::fs::write(e.remote("home/tester/f"), &data).unwrap();
        let dest = e.local(&format!("out{n}"));
        let r = e
            .client
            .download_file("/home/tester/f", &dest, &TransferOptions::default())
            .await;
        completed(&r);
        assert_eq!(r.bytes, n as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), data, "size {n}");
    }
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[tokio::test]
async fn upload_is_byte_identical_for_awkward_sizes() {
    let e = env().await;
    for &n in SIZES {
        let data = pattern(n);
        let src = e.local("src");
        std::fs::write(&src, &data).unwrap();
        let r = e
            .client
            .upload_file(
                &src,
                &format!("/home/tester/u{n}"),
                &TransferOptions::default(),
            )
            .await;
        completed(&r);
        assert_eq!(r.bytes, n as u64);
        assert_eq!(
            std::fs::read(e.remote(&format!("home/tester/u{n}"))).unwrap(),
            data,
            "size {n}"
        );
    }
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[tokio::test]
async fn file_larger_than_window_with_small_window_is_identical() {
    let e = env().await;
    let data = pattern(500_000);
    std::fs::write(e.remote("home/tester/f"), &data).unwrap();
    let opts = TransferOptions {
        window: 3,
        chunk_size: 4096,
        ..TransferOptions::default()
    };
    completed(
        &e.client
            .download_file("/home/tester/f", &e.local("o"), &opts)
            .await,
    );
    assert_eq!(std::fs::read(e.local("o")).unwrap(), data);
    completed(
        &e.client
            .upload_file(&e.local("o"), "/home/tester/g", &opts)
            .await,
    );
    assert_eq!(std::fs::read(e.remote("home/tester/g")).unwrap(), data);
}

#[tokio::test]
async fn short_reads_are_completed_with_follow_up_requests() {
    // 服务器每次最多回 1000 字节：必须补读缺口，且写入顺序不能乱。
    let e = env_with(|c| c.max_read = Some(1000)).await;
    let data = pattern(200_003);
    std::fs::write(e.remote("home/tester/f"), &data).unwrap();
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &TransferOptions::default())
        .await;
    completed(&r);
    assert_eq!(std::fs::read(e.local("o")).unwrap(), data);
}

#[tokio::test]
async fn download_pipelines_many_requests_on_high_latency_link() {
    // debug 构建并发跑全量测试时，5ms 延迟容易被调度抖动吃掉导致在途数不到 32；
    // 与下方上传用例一致用 40ms，让窗口深度而不是 CPU/调度决定在途数。
    let e = env_with(|c| c.delay = Duration::from_millis(40)).await;
    std::fs::write(e.remote("home/tester/f"), pattern(4 * 1024 * 1024)).unwrap();
    e.server.sftp_stats.reset_max_inflight();
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &TransferOptions::default())
        .await;
    completed(&r);
    let max = e.server.sftp_stats.max_inflight();
    assert!(max >= 32, "服务器观察到的最大在途请求数只有 {max}");
}

#[tokio::test]
async fn upload_pipelines_many_requests_on_high_latency_link() {
    // 上传的在途数受"服务器每秒能收多少字节"限制：debug 构建下加密很慢，5ms 的延迟填不满 64 个窗口
    // （实测只有十几个），所以这里用 40ms，让窗口的深度而不是 CPU 决定在途数。
    let e = env_with(|c| c.delay = Duration::from_millis(40)).await;
    std::fs::write(e.local("src"), pattern(4 * 1024 * 1024)).unwrap();
    e.server.sftp_stats.reset_max_inflight();
    let r = e
        .client
        .upload_file(
            &e.local("src"),
            "/home/tester/f",
            &TransferOptions::default(),
        )
        .await;
    completed(&r);
    let max = e.server.sftp_stats.max_inflight();
    assert!(max >= 32, "服务器观察到的最大在途请求数只有 {max}");
}

#[tokio::test]
async fn pipelined_transfer_time_is_far_below_blocks_times_latency() {
    // 128 块 × 20ms：逐块往返至少 2.56 秒；流水线应远低于这个数。
    let latency = Duration::from_millis(20);
    let e = env_with(|c| c.delay = latency).await;
    let blocks = 128u32;
    std::fs::write(
        e.remote("home/tester/f"),
        pattern(32 * 1024 * blocks as usize),
    )
    .unwrap();
    let serial = latency * blocks;

    let t = Instant::now();
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &TransferOptions::default())
        .await;
    completed(&r);
    let down = t.elapsed();
    assert!(down < serial / 3, "下载 {down:?}，逐块往返约 {serial:?}");

    let t = Instant::now();
    completed(
        &e.client
            .upload_file(&e.local("o"), "/home/tester/g", &TransferOptions::default())
            .await,
    );
    let up = t.elapsed();
    assert!(up < serial / 3, "上传 {up:?}，逐块往返约 {serial:?}");
}

type ProgressLog = Arc<Mutex<Vec<(Instant, TransferProgress)>>>;

fn recording_progress() -> (ProgressLog, crate::sftp::ProgressFn) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    (
        log,
        Arc::new(move |p| l.lock().unwrap().push((Instant::now(), p))),
    )
}

#[tokio::test]
async fn progress_is_throttled_starts_at_zero_and_ends_full() {
    // 窗口 4、每请求 10ms：2 MiB 要几百毫秒，足够产生多次节流后的回调。
    let e = env_with(|c| c.delay = Duration::from_millis(10)).await;
    let size = 2 * 1024 * 1024u64;
    std::fs::write(e.remote("home/tester/f"), pattern(size as usize)).unwrap();
    let (log, cb) = recording_progress();
    let opts = TransferOptions {
        window: 4,
        progress: Some(cb),
        ..TransferOptions::default()
    };
    completed(
        &e.client
            .download_file("/home/tester/f", &e.local("o"), &opts)
            .await,
    );
    let log = log.lock().unwrap();
    assert!(log.len() >= 4, "回调太少：{}", log.len());
    assert_eq!(log[0].1.bytes, 0);
    assert_eq!(log[0].1.total, Some(size));
    let last = log.last().unwrap().1;
    assert_eq!((last.bytes, last.total), (size, Some(size)));
    // 除最后一次（强制报告）外，相邻回调间隔不小于 50ms（留 1ms 给计时误差）。
    for w in log[..log.len() - 1].windows(2) {
        let gap = w[1].0 - w[0].0;
        assert!(gap >= Duration::from_millis(49), "回调间隔 {gap:?}");
    }
    // 字节数单调不减。
    assert!(log.windows(2).all(|w| w[0].1.bytes <= w[1].1.bytes));
}

#[tokio::test]
async fn upload_progress_starts_at_zero_and_ends_full() {
    let e = env().await;
    std::fs::write(e.local("s"), pattern(300_000)).unwrap();
    let (log, cb) = recording_progress();
    let opts = TransferOptions {
        progress: Some(cb),
        ..TransferOptions::default()
    };
    completed(
        &e.client
            .upload_file(&e.local("s"), "/home/tester/f", &opts)
            .await,
    );
    let log = log.lock().unwrap();
    assert_eq!(log[0].1.bytes, 0);
    let last = log.last().unwrap().1;
    assert_eq!((last.bytes, last.total), (300_000, Some(300_000)));
}

#[tokio::test]
async fn empty_file_reports_zero_then_zero() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"").unwrap();
    let (log, cb) = recording_progress();
    let opts = TransferOptions {
        progress: Some(cb),
        ..TransferOptions::default()
    };
    completed(
        &e.client
            .download_file("/home/tester/f", &e.local("o"), &opts)
            .await,
    );
    assert!(e.local("o").exists());
    let log = log.lock().unwrap();
    // 大小未知（服务器报 0）时开始报 total = None，结束时 total = 实际字节数。
    assert!(log.iter().all(|(_, p)| p.bytes == 0));
    assert_eq!(log[0].1.total, None);
    assert_eq!(log.last().unwrap().1.total, Some(0));
}

#[tokio::test]
async fn cancelling_download_stops_at_chunk_boundary_and_removes_partial_file() {
    let e = env_with(|c| c.delay = Duration::from_millis(2)).await;
    let size = 4 * 1024 * 1024u64;
    std::fs::write(e.remote("home/tester/f"), pattern(size as usize)).unwrap();
    let cancel = cancel_after(&e, 20, false);
    let opts = TransferOptions {
        cancel,
        ..TransferOptions::default()
    };
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &opts)
        .await;
    assert_eq!(r.outcome, TransferOutcome::Cancelled);
    assert!(r.bytes < size);
    assert!(!e.local("o").exists(), "取消后不应留下残缺文件");
    eventually(|| e.server.sftp_stats.open_handles() == 0).await;
    // 取消不影响会话：之后还能正常工作。
    assert!(e.client.stat("/home/tester/f").await.is_ok());
}

#[tokio::test]
async fn cancelling_upload_closes_and_removes_remote_partial_file() {
    let e = env_with(|c| c.delay = Duration::from_millis(2)).await;
    let size = 4 * 1024 * 1024u64;
    std::fs::write(e.local("s"), pattern(size as usize)).unwrap();
    let cancel = cancel_after(&e, 20, true);
    let opts = TransferOptions {
        cancel,
        ..TransferOptions::default()
    };
    let r = e
        .client
        .upload_file(&e.local("s"), "/home/tester/f", &opts)
        .await;
    assert_eq!(r.outcome, TransferOutcome::Cancelled);
    assert!(
        !e.remote("home/tester/f").exists(),
        "取消后远端不应留下残缺文件"
    );
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[tokio::test]
async fn already_cancelled_transfer_does_nothing() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"data").unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let opts = TransferOptions {
        cancel,
        ..TransferOptions::default()
    };
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &opts)
        .await;
    assert_eq!(r.outcome, TransferOutcome::Cancelled);
    assert!(!e.local("o").exists());
}

#[tokio::test]
async fn download_cleans_up_when_connection_drops_mid_transfer() {
    let e = env_with(|c| c.disconnect_after_reads = Some(20)).await;
    std::fs::write(e.remote("home/tester/f"), pattern(4 * 1024 * 1024)).unwrap();
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &TransferOptions::default())
        .await;
    assert!(matches!(r.outcome, TransferOutcome::Failed(_)), "{r:?}");
    assert!(!e.local("o").exists(), "断线后本地不应留下残缺文件");
}

#[tokio::test]
async fn upload_returns_error_when_connection_drops_mid_transfer() {
    // 连接已断，远端残缺文件无法再删除（尽力而为）；这里只验证引擎不挂死并报失败。
    let e = env_with(|c| c.disconnect_after_writes = Some(10)).await;
    std::fs::write(e.local("s"), pattern(4 * 1024 * 1024)).unwrap();
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        e.client
            .upload_file(&e.local("s"), "/home/tester/f", &TransferOptions::default()),
    )
    .await
    .expect("断线后上传不应挂死");
    assert!(matches!(r.outcome, TransferOutcome::Failed(_)), "{r:?}");
}

#[tokio::test]
async fn failed_upload_over_server_error_removes_remote_file() {
    // 非取消的失败同样清理：第 N 个块之后服务器开始拒绝写入——用只读句柄之外的办法不好造，
    // 所以这里用"上传目标目录没有权限"验证打开阶段的失败不会留下任何东西，并返回权限错误。
    let e = env_with(|c| c.deny_prefixes = vec!["/home/tester/locked".into()]).await;
    std::fs::create_dir(e.remote("home/tester/locked")).unwrap();
    std::fs::write(e.local("s"), b"x").unwrap();
    let r = e
        .client
        .upload_file(
            &e.local("s"),
            "/home/tester/locked/f",
            &TransferOptions::default(),
        )
        .await;
    match &r.outcome {
        TransferOutcome::Failed(err @ TransferError::Sftp(SftpError::Status { kind, .. })) => {
            assert_eq!(*kind, StatusKind::PermissionDenied);
            assert_eq!(err.to_message().key(), "sftp-op-failed");
        }
        other => panic!("{other:?}"),
    }
    assert!(!e.remote("home/tester/locked/f").exists());
}

#[tokio::test]
async fn download_without_overwrite_keeps_existing_local_file() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"remote").unwrap();
    std::fs::write(e.local("o"), b"mine").unwrap();
    // 默认不覆盖。
    let opts = TransferOptions::default();
    assert_eq!(opts.conflict, ConflictPolicy::Fail);
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &opts)
        .await;
    match &r.outcome {
        TransferOutcome::Failed(err) => {
            assert!(err.is_already_exists(), "{err:?}");
            assert_eq!(err.to_message().key(), "xfer-local-write-failed");
        }
        o => panic!("{o:?}"),
    }
    // 因"已存在"而失败时不能把用户已有的文件当半成品删掉。
    assert_eq!(std::fs::read(e.local("o")).unwrap(), b"mine");
}

#[tokio::test]
async fn upload_without_overwrite_keeps_existing_remote_file() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"remote").unwrap();
    std::fs::write(e.local("s"), b"new").unwrap();
    let opts = TransferOptions::default();
    let r = e
        .client
        .upload_file(&e.local("s"), "/home/tester/f", &opts)
        .await;
    match r.outcome {
        TransferOutcome::Failed(ref err) => {
            assert!(err.is_already_exists(), "{err:?}");
            assert_eq!(err.to_message().key(), "sftp-file-exists");
        }
        ref o => panic!("{o:?}"),
    }
    assert_eq!(std::fs::read(e.remote("home/tester/f")).unwrap(), b"remote");

    // 带 Overwrite 重试即覆盖。
    let opts = TransferOptions {
        conflict: ConflictPolicy::Overwrite,
        ..TransferOptions::default()
    };
    completed(
        &e.client
            .upload_file(&e.local("s"), "/home/tester/f", &opts)
            .await,
    );
    assert_eq!(std::fs::read(e.remote("home/tester/f")).unwrap(), b"new");
}

#[tokio::test]
async fn download_with_overwrite_replaces_existing_local_file() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"remote").unwrap();
    std::fs::write(e.local("o"), b"mine, longer than remote").unwrap();
    let opts = TransferOptions {
        conflict: ConflictPolicy::Overwrite,
        ..TransferOptions::default()
    };
    completed(
        &e.client
            .download_file("/home/tester/f", &e.local("o"), &opts)
            .await,
    );
    assert_eq!(std::fs::read(e.local("o")).unwrap(), b"remote");
}

#[tokio::test]
async fn download_dir_fails_before_writing_when_local_file_exists_without_overwrite() {
    let e = env().await;
    tree(
        &e.remote("home/tester/proj"),
        &[("a", b"1"), ("b", b"2")],
        &[],
    );
    tree(&e.local("out"), &[("b", b"mine")], &[]);
    let r = e
        .client
        .download_dir(
            "/home/tester/proj",
            &e.local("out"),
            &TransferOptions::default(),
        )
        .await;
    match &r.outcome {
        TransferOutcome::Failed(err) => assert!(err.is_already_exists(), "{err:?}"),
        o => panic!("{o:?}"),
    }
    // 排在前面的 `a` 也没有被写。
    assert_eq!(walk(&e.local("out")), ["b"]);
    assert_eq!(std::fs::read(e.local("out/b")).unwrap(), b"mine");
}

#[tokio::test]
async fn files_reported_as_zero_size_are_read_until_eof() {
    // 服务器所有文件都声称 size = 0（类似 procfs）：不能当成空文件，要串行读到 EOF。
    let e = env_with(|c| c.zero_size_attrs = true).await;
    for n in [0usize, 5, 32768, 100_001] {
        let data = pattern(n);
        std::fs::write(e.remote("home/tester/f"), &data).unwrap();
        let dest = e.local(&format!("z{n}"));
        let (log, cb) = recording_progress();
        let opts = TransferOptions {
            progress: Some(cb),
            ..TransferOptions::default()
        };
        let r = e.client.download_file("/home/tester/f", &dest, &opts).await;
        completed(&r);
        assert_eq!(std::fs::read(&dest).unwrap(), data, "size {n}");
        assert_eq!(r.bytes, n as u64);
        let log = log.lock().unwrap();
        assert_eq!(log[0].1.total, None);
        let last = log.last().unwrap().1;
        assert_eq!((last.bytes, last.total), (n as u64, Some(n as u64)));
    }
    // 大小未知时只保持一个在途请求。
    assert!(e.server.sftp_stats.max_inflight() <= 2);
}

#[tokio::test]
async fn in_flight_buffer_is_bounded_by_window_times_chunk() {
    let e = env().await;
    let size = 8 * 1024 * 1024;
    std::fs::write(e.remote("home/tester/f"), pattern(size)).unwrap();
    let (window, chunk) = (16usize, 32 * 1024u32);
    let bound = window * chunk as usize;
    for upload in [false, true] {
        let gauge = BufferGauge::new();
        let opts = TransferOptions {
            window,
            chunk_size: chunk,
            gauge: Some(gauge.clone()),
            ..TransferOptions::default()
        };
        let r = if upload {
            e.client
                .upload_file(&e.remote("home/tester/f"), "/home/tester/g", &opts)
                .await
        } else {
            e.client
                .download_file("/home/tester/f", &e.local("o"), &opts)
                .await
        };
        completed(&r);
        assert!(gauge.peak() > 0);
        assert!(
            gauge.peak() <= bound + 4096,
            "峰值 {} 超过 {window} × {chunk} = {bound}（上传={upload}）",
            gauge.peak()
        );
        assert!(gauge.peak() < size / 4, "内存应与文件大小无关");
        assert_eq!(gauge.current(), 0);
    }
}

#[tokio::test]
async fn listing_and_other_requests_run_while_transfer_is_in_progress() {
    let e = env_with(|c| c.delay = Duration::from_millis(5)).await;
    std::fs::write(e.remote("home/tester/big"), pattern(8 * 1024 * 1024)).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let (client, d2, dest) = (e.client.clone(), done.clone(), e.local("o"));
    let t = tokio::spawn(async move {
        let r = client
            .download_file(
                "/home/tester/big",
                &dest,
                &TransferOptions {
                    window: 8,
                    ..TransferOptions::default()
                },
            )
            .await;
        d2.store(true, Ordering::SeqCst);
        r
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    // 传输还在跑时，列目录与 stat 照常返回。
    let all = e
        .client
        .list("/home/tester")
        .await
        .unwrap()
        .collect_all()
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    e.client.stat("/home/tester/big").await.unwrap();
    assert!(!done.load(Ordering::SeqCst), "传输不应早于这些请求结束");
    completed(&t.await.unwrap());
}

// ---------------------------------------------------------------------------
// 目录传输

fn tree(root: &Path, files: &[(&str, &[u8])], dirs: &[&str]) {
    std::fs::create_dir_all(root).unwrap();
    for d in dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for (f, c) in files {
        std::fs::write(root.join(f), c).unwrap();
    }
}

fn walk(root: &Path) -> Vec<String> {
    fn rec(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut es: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            // 统一用 `/` 拼接，期望值在各平台一致（Windows 的分隔符是 `\`）。
            let rel = e
                .path()
                .strip_prefix(base)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if e.file_type().unwrap().is_dir() {
                out.push(format!("{rel}/"));
                rec(base, &e.path(), out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    rec(root, root, &mut out);
    out
}

#[tokio::test]
async fn download_dir_copies_tree_and_reports_progress_over_whole_tree() {
    let e = env().await;
    let src = e.remote("home/tester/proj");
    tree(
        &src,
        &[
            ("a.txt", b"aaa"),
            ("sub/b.bin", &[1u8; 70_000]),
            ("sub/deep/c", b"c"),
        ],
        &["sub/deep", "empty"],
    );
    let (log, cb) = recording_progress();
    let opts = TransferOptions {
        progress: Some(cb),
        ..TransferOptions::default()
    };
    let dest = e.local("out");
    let r = e
        .client
        .download_dir("/home/tester/proj", &dest, &opts)
        .await;
    completed(&r);
    assert_eq!(r.files, 3);
    assert_eq!(r.bytes, 3 + 70_000 + 1);
    assert!(r.skipped.is_empty());
    assert_eq!(
        walk(&dest),
        [
            "a.txt",
            "empty/",
            "sub/",
            "sub/b.bin",
            "sub/deep/",
            "sub/deep/c"
        ]
    );
    assert_eq!(
        std::fs::read(dest.join("sub/b.bin")).unwrap(),
        vec![1u8; 70_000]
    );
    // 进度的总量覆盖整棵树，最后一次是满的。
    let last = log.lock().unwrap().last().unwrap().1;
    assert_eq!((last.bytes, last.total), (70_004, Some(70_004)));
    assert_eq!((last.files_done, last.files_total), (3, 3));
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn download_dir_follows_file_links_and_skips_dir_and_dangling_links() {
    use std::os::unix::fs::symlink;
    let e = env().await;
    let src = e.remote("home/tester/proj");
    tree(&src, &[("real.txt", b"data")], &["realdir"]);
    std::fs::write(src.join("realdir/in"), b"x").unwrap();
    symlink("real.txt", src.join("link-file")).unwrap();
    // 指回上级：若被跟随就会死循环。
    symlink("..", src.join("realdir/up")).unwrap();
    symlink("realdir", src.join("link-dir")).unwrap();
    symlink("nothing", src.join("dangling")).unwrap();
    let dest = e.local("out");
    let r = e
        .client
        .download_dir("/home/tester/proj", &dest, &TransferOptions::default())
        .await;
    completed(&r);
    assert_eq!(
        walk(&dest),
        ["link-file", "real.txt", "realdir/", "realdir/in"]
    );
    assert_eq!(std::fs::read(dest.join("link-file")).unwrap(), b"data");
    // 跳过的项都记入报告。
    let reason = |p: &str| {
        r.skipped
            .iter()
            .find(|s| s.path.ends_with(p))
            .map(|s| s.reason.clone())
    };
    assert_eq!(reason("link-dir"), Some(SkipReason::SymlinkToDir));
    assert_eq!(reason("realdir/up"), Some(SkipReason::SymlinkToDir));
    assert_eq!(reason("dangling"), Some(SkipReason::DanglingSymlink));
}

#[tokio::test]
async fn download_dir_rejects_unsafe_names_and_never_writes_outside_target() {
    let injected = [
        "..",
        "../evil",
        "/abs-evil",
        "a/b",
        "a\\b",
        "",
        "../../evil2",
        ".",
    ];
    let e = env_with(|c| {
        c.inject_entries = injected.iter().map(|n| ((*n).to_owned(), 1)).collect();
    })
    .await;
    std::fs::create_dir_all(e.remote("home/tester/proj")).unwrap();
    std::fs::write(e.remote("home/tester/proj/fine.txt"), b"ok").unwrap();
    // 目标目录放在一个独立的父目录里，便于检查父目录没有被写入。
    let parent = e.local("parent");
    std::fs::create_dir(&parent).unwrap();
    let dest = parent.join("dest");
    let r = e
        .client
        .download_dir("/home/tester/proj", &dest, &TransferOptions::default())
        .await;
    completed(&r);
    let unsafe_count = r
        .skipped
        .iter()
        .filter(|s| s.reason == SkipReason::UnsafeName)
        .count();
    // "." 与 ".." 按规格直接跳过（不算"不安全名字"）；其余 6 个都应被拒绝并记入报告。
    assert_eq!(unsafe_count, injected.len() - 2, "{:?}", r.skipped);
    assert_eq!(walk(&dest), ["fine.txt"]);
    assert_eq!(walk(&parent), ["dest/", "dest/fine.txt"]);
    assert!(!e.local.path().join("evil").exists());
    assert!(!e.local.path().join("evil2").exists());
    assert!(!Path::new("/abs-evil").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn download_dir_does_not_write_through_existing_local_symlinks() {
    use std::os::unix::fs::symlink;
    let e = env().await;
    let src = e.remote("home/tester/proj");
    tree(&src, &[("victim", b"remote"), ("d/x", b"remote")], &["d"]);
    let outside = e.local("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret"), b"local-secret").unwrap();
    let dest = e.local("dest");
    std::fs::create_dir(&dest).unwrap();
    // 目标里已有的符号链接：文件位置和目录位置各一个。
    symlink(outside.join("secret"), dest.join("victim")).unwrap();
    symlink(&outside, dest.join("d")).unwrap();
    let r = e
        .client
        .download_dir("/home/tester/proj", &dest, &TransferOptions::default())
        .await;
    completed(&r);
    assert_eq!(
        std::fs::read(outside.join("secret")).unwrap(),
        b"local-secret"
    );
    assert!(!outside.join("x").exists());
    assert!(
        r.skipped
            .iter()
            .all(|s| s.reason == SkipReason::LocalConflict)
    );
    assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
    assert_eq!(r.files, 0);
}

#[tokio::test]
async fn download_dir_total_size_overflow_is_an_error_before_any_write() {
    let e = env_with(|c| {
        c.inject_entries = vec![
            ("big1".into(), u64::MAX / 2 + 1),
            ("big2".into(), u64::MAX / 2 + 1),
        ];
    })
    .await;
    std::fs::create_dir_all(e.remote("home/tester/proj")).unwrap();
    let dest = e.local("dest");
    let r = e
        .client
        .download_dir("/home/tester/proj", &dest, &TransferOptions::default())
        .await;
    assert_eq!(
        r.outcome,
        TransferOutcome::Failed(TransferError::SizeOverflow)
    );
    assert!(!dest.exists(), "计划失败时不应创建任何本地内容");
    assert_eq!(
        TransferError::SizeOverflow.to_message().key(),
        "xfer-size-overflow"
    );
}

#[tokio::test]
async fn download_dir_on_a_file_reports_not_a_directory() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"x").unwrap();
    let r = e
        .client
        .download_dir("/home/tester/f", &e.local("d"), &TransferOptions::default())
        .await;
    assert!(matches!(
        r.outcome,
        TransferOutcome::Failed(TransferError::RemoteNotDir { .. })
    ));
}

#[tokio::test]
async fn cancelling_dir_download_removes_only_the_partial_file() {
    let e = env_with(|c| c.delay = Duration::from_millis(2)).await;
    let src = e.remote("home/tester/proj");
    tree(&src, &[("a_small", b"done")], &[]);
    std::fs::write(src.join("b_big"), pattern(4 * 1024 * 1024)).unwrap();
    let cancel = cancel_after(&e, 20, false);
    let opts = TransferOptions {
        cancel,
        ..TransferOptions::default()
    };
    let dest = e.local("out");
    let r = e
        .client
        .download_dir("/home/tester/proj", &dest, &opts)
        .await;
    assert_eq!(r.outcome, TransferOutcome::Cancelled);
    // 已完成的文件保留，被取消的残缺文件删除。
    assert_eq!(walk(&dest), ["a_small"]);
}

#[tokio::test]
async fn upload_dir_merges_existing_directories_and_overwrites_files() {
    let e = env().await;
    let src = e.local("src");
    tree(
        &src,
        &[
            ("a.txt", b"new-a"),
            ("sub/b", b"new-b"),
            ("sub/deep/c", b"c"),
        ],
        &["sub/deep", "empty"],
    );
    // 远端已有 sub/ 与其中一个同名文件、一个无关文件。
    tree(
        &e.remote("home/tester/dst"),
        &[("sub/b", b"old"), ("sub/other", b"keep")],
        &["sub"],
    );
    let (log, cb) = recording_progress();
    let opts = TransferOptions {
        progress: Some(cb),
        conflict: ConflictPolicy::Overwrite,
        ..TransferOptions::default()
    };
    let r = e.client.upload_dir(&src, "/home/tester/dst", &opts).await;
    completed(&r);
    assert_eq!(r.files, 3);
    let dst = e.remote("home/tester/dst");
    assert_eq!(
        walk(&dst),
        [
            "a.txt",
            "empty/",
            "sub/",
            "sub/b",
            "sub/deep/",
            "sub/deep/c",
            "sub/other"
        ]
    );
    assert_eq!(std::fs::read(dst.join("sub/b")).unwrap(), b"new-b");
    assert_eq!(std::fs::read(dst.join("sub/other")).unwrap(), b"keep");
    let last = log.lock().unwrap().last().unwrap().1;
    assert_eq!((last.bytes, last.total), (11, Some(11)));
}

#[tokio::test]
async fn upload_dir_creates_missing_remote_root() {
    let e = env().await;
    let src = e.local("src");
    tree(&src, &[("a", b"1")], &[]);
    let r = e
        .client
        .upload_dir(&src, "/home/tester/fresh", &TransferOptions::default())
        .await;
    completed(&r);
    assert_eq!(
        std::fs::read(e.remote("home/tester/fresh/a")).unwrap(),
        b"1"
    );
}

#[tokio::test]
async fn upload_dir_fails_when_remote_same_name_is_not_a_directory() {
    let e = env().await;
    let src = e.local("src");
    tree(&src, &[("sub/x", b"1"), ("z", b"z")], &["sub"]);
    // 远端 `sub` 是文件：必须报错且不覆盖。
    tree(
        &e.remote("home/tester/dst"),
        &[("sub", b"i am a file")],
        &[],
    );
    let r = e
        .client
        .upload_dir(&src, "/home/tester/dst", &TransferOptions::default())
        .await;
    match &r.outcome {
        TransferOutcome::Failed(TransferError::RemoteNotDir { path }) => {
            assert_eq!(path, "/home/tester/dst/sub");
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(
        std::fs::read(e.remote("home/tester/dst/sub")).unwrap(),
        b"i am a file"
    );
    // 目录阶段就失败：没有任何文件被上传。
    assert!(!e.remote("home/tester/dst/z").exists());

    // 根路径本身是文件。
    let r = e
        .client
        .upload_dir(&src, "/home/tester/dst/sub", &TransferOptions::default())
        .await;
    assert!(matches!(
        r.outcome,
        TransferOutcome::Failed(TransferError::RemoteNotDir { .. })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn upload_dir_uploads_regular_files_only() {
    use std::os::unix::fs::symlink;
    let e = env().await;
    let src = e.local("src");
    tree(&src, &[("real", b"r")], &["d"]);
    symlink("real", src.join("link")).unwrap();
    symlink("d", src.join("dirlink")).unwrap();
    let r = e
        .client
        .upload_dir(&src, "/home/tester/dst", &TransferOptions::default())
        .await;
    completed(&r);
    assert_eq!(walk(&e.remote("home/tester/dst")), ["d/", "real"]);
    assert_eq!(r.skipped.len(), 2);
    assert!(
        r.skipped
            .iter()
            .all(|s| s.reason == SkipReason::NotRegularFile)
    );
}

#[tokio::test]
async fn upload_dir_cancel_removes_partial_remote_file() {
    let e = env_with(|c| c.delay = Duration::from_millis(2)).await;
    let src = e.local("src");
    tree(&src, &[("a_small", b"done")], &[]);
    std::fs::write(src.join("b_big"), pattern(4 * 1024 * 1024)).unwrap();
    let cancel = cancel_after(&e, 20, true);
    let opts = TransferOptions {
        cancel,
        ..TransferOptions::default()
    };
    let r = e.client.upload_dir(&src, "/home/tester/dst", &opts).await;
    assert_eq!(r.outcome, TransferOutcome::Cancelled);
    assert_eq!(walk(&e.remote("home/tester/dst")), ["a_small"]);
}

// ---------------------------------------------------------------------------
// 吞吐基准：不进 CI 判定，只打印数字。
//   cargo test -p kanso-ssh --release sftp_throughput -- --ignored --nocapture

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "基准：本机回环 256 MiB 吞吐，只打印数字"]
async fn sftp_throughput_loopback_256mib() {
    use std::io::Write;
    let e = env().await;
    const TOTAL: u64 = 256 * 1024 * 1024;
    // 分块写入，避免测试自己吃掉 256 MiB 内存。
    let block = pattern(1024 * 1024);
    for path in [e.remote("home/tester/big"), e.local("big")] {
        let mut f = std::fs::File::create(path).unwrap();
        for _ in 0..TOTAL / block.len() as u64 {
            f.write_all(&block).unwrap();
        }
    }
    let opts = TransferOptions::default();
    let t = Instant::now();
    let r = e
        .client
        .download_file("/home/tester/big", &e.local("down"), &opts)
        .await;
    completed(&r);
    let down = TOTAL as f64 / 1e6 / t.elapsed().as_secs_f64();
    let t = Instant::now();
    let r = e
        .client
        .upload_file(&e.local("big"), "/home/tester/up", &opts)
        .await;
    completed(&r);
    let up = TOTAL as f64 / 1e6 / t.elapsed().as_secs_f64();
    println!("SFTP loopback 256 MiB: download {down:.1} MB/s, upload {up:.1} MB/s");
}

// ---------------------------------------------------------------------------
// 冲突策略 Skip（filer.md D4）

fn skipped_existing(r: &TransferReport) -> usize {
    r.skipped
        .iter()
        .filter(|s| s.reason == SkipReason::AlreadyExists)
        .count()
}

#[tokio::test]
async fn skip_policy_download_file_leaves_existing_and_reports_skipped() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"remote").unwrap();
    std::fs::write(e.local("o"), b"mine").unwrap();
    let opts = TransferOptions {
        conflict: ConflictPolicy::Skip,
        ..TransferOptions::default()
    };
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("o"), &opts)
        .await;
    completed(&r);
    assert_eq!(skipped_existing(&r), 1, "{r:?}");
    assert_eq!(r.files, 0);
    // 跳过不能把用户已有的文件当半成品删掉。
    assert_eq!(std::fs::read(e.local("o")).unwrap(), b"mine");
    // 目标不存在时照常传输。
    let r = e
        .client
        .download_file("/home/tester/f", &e.local("new"), &opts)
        .await;
    completed(&r);
    assert_eq!(skipped_existing(&r), 0);
    assert_eq!(std::fs::read(e.local("new")).unwrap(), b"remote");
}

#[tokio::test]
async fn skip_policy_upload_file_leaves_existing_remote() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f"), b"remote").unwrap();
    std::fs::write(e.local("s"), b"new").unwrap();
    let opts = TransferOptions {
        conflict: ConflictPolicy::Skip,
        ..TransferOptions::default()
    };
    let r = e
        .client
        .upload_file(&e.local("s"), "/home/tester/f", &opts)
        .await;
    completed(&r);
    assert_eq!(skipped_existing(&r), 1, "{r:?}");
    assert_eq!(std::fs::read(e.remote("home/tester/f")).unwrap(), b"remote");
}

#[tokio::test]
async fn skip_policy_download_dir_merges_and_skips_existing_files() {
    let e = env().await;
    tree(
        &e.remote("home/tester/proj"),
        &[("a", b"1"), ("b", b"2"), ("sub/c", b"3")],
        &["sub"],
    );
    tree(&e.local("out"), &[("b", b"mine")], &[]);
    let opts = TransferOptions {
        conflict: ConflictPolicy::Skip,
        ..TransferOptions::default()
    };
    let r = e
        .client
        .download_dir("/home/tester/proj", &e.local("out"), &opts)
        .await;
    completed(&r);
    assert_eq!(skipped_existing(&r), 1, "{r:?}");
    assert_eq!(r.files, 2);
    assert_eq!(walk(&e.local("out")), ["a", "b", "sub/", "sub/c"]);
    assert_eq!(std::fs::read(e.local("out/b")).unwrap(), b"mine");
}

#[tokio::test]
async fn skip_policy_upload_dir_merges_and_skips_existing_files() {
    let e = env().await;
    let src = e.local("src");
    tree(
        &src,
        &[
            ("a.txt", b"new-a"),
            ("sub/b", b"new-b"),
            ("sub/c", b"new-c"),
        ],
        &["sub"],
    );
    tree(&e.remote("home/tester/dst"), &[("sub/b", b"old")], &["sub"]);
    let opts = TransferOptions {
        conflict: ConflictPolicy::Skip,
        ..TransferOptions::default()
    };
    let r = e.client.upload_dir(&src, "/home/tester/dst", &opts).await;
    completed(&r);
    assert_eq!(skipped_existing(&r), 1, "{r:?}");
    assert_eq!(r.files, 2);
    let dst = e.remote("home/tester/dst");
    assert_eq!(std::fs::read(dst.join("sub/b")).unwrap(), b"old");
    assert_eq!(std::fs::read(dst.join("sub/c")).unwrap(), b"new-c");
    assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"new-a");
}

// ---------------------------------------------------------------------------
// MCP 的 SFTP 补充：entry / read_range / write_new / overwrite_file / rmdir

use crate::sftp::OverwriteError;

/// 目录里没有 `.kanso-tmp-` 临时文件残留。
fn no_tmp_left(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".kanso-tmp-")
    })
}

#[tokio::test]
async fn entry_matches_listing_fields() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/f.txt"), b"12345").unwrap();
    std::fs::create_dir(e.remote("home/tester/d")).unwrap();
    let listed = e
        .client
        .list("/home/tester")
        .await
        .unwrap()
        .collect_all()
        .await
        .unwrap();
    for name in ["f.txt", "d"] {
        let want = listed.iter().find(|x| x.name == name).unwrap();
        let got = e
            .client
            .entry(&format!("/home/tester/{name}"), true)
            .await
            .unwrap();
        assert_eq!(&got, want);
    }
    let err = e.client.entry("/home/tester/none", true).await.unwrap_err();
    assert!(err.is_not_found());
}

#[cfg(unix)]
#[tokio::test]
async fn entry_follow_symlinks_switches_between_stat_and_lstat() {
    let e = env().await;
    std::fs::create_dir(e.remote("home/tester/d")).unwrap();
    std::os::unix::fs::symlink("d", e.remote("home/tester/link")).unwrap();
    let lstat = e.client.entry("/home/tester/link", false).await.unwrap();
    assert!(lstat.is_symlink && lstat.is_dir);
    assert_eq!(lstat.name, "link");
    assert_eq!(lstat.path, "/home/tester/link");
    let stat = e.client.entry("/home/tester/link", true).await.unwrap();
    assert!(!stat.is_symlink && stat.is_dir);
}

#[tokio::test]
async fn read_range_reads_in_segments_to_eof_and_past_it() {
    // 服务器每次最多回 1000 字节：必须循环补读。
    let e = env_with(|c| c.max_read = Some(1000)).await;
    let data = pattern(100_000);
    std::fs::write(e.remote("home/tester/big"), &data).unwrap();
    let c = &e.client;
    // 分段：中间一段。
    let got = c
        .read_range("/home/tester/big", 12_345, 50_000)
        .await
        .unwrap();
    assert_eq!(got, data[12_345..62_345]);
    // 请求超过文件末尾：返回实际剩余。
    let got = c
        .read_range("/home/tester/big", 99_000, 5_000)
        .await
        .unwrap();
    assert_eq!(got, data[99_000..]);
    // 恰好在 EOF 与超出 EOF：空。
    assert!(
        c.read_range("/home/tester/big", 100_000, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        c.read_range("/home/tester/big", 1 << 40, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // 长度 0。
    assert!(
        c.read_range("/home/tester/big", 0, 0)
            .await
            .unwrap()
            .is_empty()
    );
    // 不存在。
    assert!(
        c.read_range("/home/tester/none", 0, 1)
            .await
            .unwrap_err()
            .is_not_found()
    );
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[tokio::test]
async fn write_new_fails_when_exists_and_keeps_original() {
    let e = env().await;
    std::fs::write(e.remote("home/tester/keep"), b"precious").unwrap();
    let err = e
        .client
        .write_new("/home/tester/keep", b"overwritten?")
        .await
        .unwrap_err();
    assert!(err.error.is_already_exists(), "{err:?}");
    assert_eq!(err.leftover, None);
    assert_eq!(
        std::fs::read(e.remote("home/tester/keep")).unwrap(),
        b"precious"
    );
    // 不存在时写入，大于一个块。
    let data = pattern(100_000);
    e.client.write_new("/home/tester/new", &data).await.unwrap();
    assert_eq!(std::fs::read(e.remote("home/tester/new")).unwrap(), data);
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[tokio::test]
async fn write_new_removes_the_file_it_created_when_write_fails() {
    let e = env_with(|c| c.fail_writes_after = Some(1)).await;
    let err = e
        .client
        .write_new("/home/tester/new", &pattern(200_000))
        .await
        .unwrap_err();
    assert_eq!(err.leftover, None);
    assert!(!e.remote("home/tester/new").exists());
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_replaces_content_keeps_permissions_and_leaves_no_temp() {
    use std::os::unix::fs::PermissionsExt;
    let e = env().await;
    let p = e.remote("home/tester/f");
    std::fs::write(&p, b"old").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
    let data = pattern(100_000);
    e.client
        .overwrite_file("/home/tester/f", &data)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), data);
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    assert!(no_tmp_left(&e.remote("home/tester")));
    assert_eq!(e.server.sftp_stats.open_handles(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_restricts_temp_permissions_before_first_write() {
    use std::os::unix::fs::PermissionsExt;
    let e = env().await;
    let p = e.remote("home/tester/secret");
    std::fs::write(&p, b"old").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    e.client
        .overwrite_file("/home/tester/secret", b"new")
        .await
        .unwrap();
    // 第一个 WRITE 到达时临时文件就已经是 0600，而不是服务器默认权限。
    let mode = e
        .server
        .sftp_stats
        .first_write_mode
        .lock()
        .unwrap()
        .unwrap();
    assert_eq!(mode & 0o7777, 0o600);
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o7777,
        0o600
    );
}

#[tokio::test]
async fn overwrite_creates_missing_target() {
    let e = env().await;
    e.client
        .overwrite_file("/home/tester/fresh", b"hello")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(e.remote("home/tester/fresh")).unwrap(),
        b"hello"
    );
    assert!(no_tmp_left(&e.remote("home/tester")));
}

#[tokio::test]
async fn overwrite_refuses_directory_target_without_writing() {
    let e = env().await;
    std::fs::create_dir(e.remote("home/tester/d")).unwrap();
    let err = e
        .client
        .overwrite_file("/home/tester/d", b"x")
        .await
        .unwrap_err();
    assert!(matches!(err, OverwriteError::Failed(_)), "{err:?}");
    assert!(no_tmp_left(&e.remote("home/tester")));
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_through_symlink_updates_target_and_keeps_link() {
    let e = env().await;
    std::fs::create_dir(e.remote("home/tester/real")).unwrap();
    std::fs::write(e.remote("home/tester/real/target"), b"old").unwrap();
    std::os::unix::fs::symlink("real/target", e.remote("home/tester/link")).unwrap();
    e.client
        .overwrite_file("/home/tester/link", b"new content")
        .await
        .unwrap();
    // 链接仍是链接，指向不变；真实文件被更新。
    let md = std::fs::symlink_metadata(e.remote("home/tester/link")).unwrap();
    assert!(md.file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(e.remote("home/tester/link")).unwrap(),
        Path::new("real/target")
    );
    assert_eq!(
        std::fs::read(e.remote("home/tester/real/target")).unwrap(),
        b"new content"
    );
    // 临时文件建在真实目标所在目录，而不是链接所在目录，两处都不应残留。
    assert!(no_tmp_left(&e.remote("home/tester")));
    assert!(no_tmp_left(&e.remote("home/tester/real")));
}

#[cfg(unix)]
#[tokio::test]
async fn overwrite_dangling_symlink_fails_without_writing() {
    let e = env().await;
    std::os::unix::fs::symlink("nowhere", e.remote("home/tester/dangling")).unwrap();
    let err = e
        .client
        .overwrite_file("/home/tester/dangling", b"x")
        .await
        .unwrap_err();
    let OverwriteError::Failed(crate::sftp::WriteFailure { error, leftover }) = err else {
        panic!("expected Failed");
    };
    assert_eq!(leftover, None);
    assert_eq!(error.to_message().key(), "sftp-op-failed");
    assert!(no_tmp_left(&e.remote("home/tester")));
    assert!(!e.remote("home/tester/nowhere").exists());
}

#[tokio::test]
async fn overwrite_without_posix_rename_is_unsupported_and_writes_nothing() {
    let e = env_with(|c| c.posix_rename = false).await;
    std::fs::write(e.remote("home/tester/f"), b"original").unwrap();
    assert!(
        !e.client
            .has_extension("posix-rename@openssh.com")
            .await
            .unwrap()
    );
    let err = e
        .client
        .overwrite_file("/home/tester/f", b"replacement")
        .await
        .unwrap_err();
    assert_eq!(err, OverwriteError::Unsupported);
    assert_eq!(
        std::fs::read(e.remote("home/tester/f")).unwrap(),
        b"original"
    );
    assert!(no_tmp_left(&e.remote("home/tester")));
    // 检查先于任何写入：服务器没有收到 WRITE，除了协商后的 LSTAT 之类请求外也没有 OPEN。
    assert_eq!(e.server.sftp_stats.writes.load(Ordering::SeqCst), 0);
    // 扩展存在时能查到。
    let e2 = env().await;
    assert!(
        e2.client
            .has_extension("posix-rename@openssh.com")
            .await
            .unwrap()
    );
    // 低层 posix_rename 在没有扩展时同样拒绝。
    let err = e
        .client
        .posix_rename("/home/tester/f", "/home/tester/g")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SftpError::Status {
            kind: StatusKind::Unsupported,
            ..
        }
    ));
}

#[tokio::test]
async fn overwrite_failure_removes_temp_and_keeps_original() {
    let e = env_with(|c| c.fail_writes_after = Some(1)).await;
    std::fs::write(e.remote("home/tester/f"), b"original").unwrap();
    let err = e
        .client
        .overwrite_file("/home/tester/f", &pattern(200_000))
        .await
        .unwrap_err();
    let OverwriteError::Failed(f) = err else {
        panic!("expected Failed");
    };
    assert_eq!(f.leftover, None);
    assert_eq!(
        std::fs::read(e.remote("home/tester/f")).unwrap(),
        b"original"
    );
    assert!(no_tmp_left(&e.remote("home/tester")));
}

#[tokio::test]
async fn overwrite_reports_leftover_temp_path_when_cleanup_fails() {
    // 写到一半连接断开：删除临时文件也会失败，残留路径要交给调用方。
    let e = env_with(|c| c.disconnect_after_writes = Some(1)).await;
    std::fs::write(e.remote("home/tester/f"), b"original").unwrap();
    let err = e
        .client
        .overwrite_file("/home/tester/f", &pattern(200_000))
        .await
        .unwrap_err();
    let OverwriteError::Failed(f) = err else {
        panic!("expected Failed");
    };
    let leftover = f.leftover.expect("leftover path");
    assert!(
        leftover.starts_with("/home/tester/.f.kanso-tmp-"),
        "{leftover}"
    );
    assert_eq!(
        std::fs::read(e.remote("home/tester/f")).unwrap(),
        b"original"
    );
    assert!(e.remote(leftover.trim_start_matches('/')).exists());
}

#[tokio::test]
async fn rmdir_removes_empty_directory_only() {
    let e = env().await;
    std::fs::create_dir(e.remote("home/tester/empty")).unwrap();
    std::fs::create_dir(e.remote("home/tester/full")).unwrap();
    std::fs::write(e.remote("home/tester/full/f"), b"x").unwrap();
    e.client.remove_dir("/home/tester/empty").await.unwrap();
    assert!(!e.remote("home/tester/empty").exists());
    assert!(e.client.remove_dir("/home/tester/full").await.is_err());
    assert!(e.remote("home/tester/full/f").exists());
}
