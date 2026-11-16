//! 单文件流水线传输（ssh.md §9.1）。
//!
//! # 流水线
//!
//! 逐块"发一个请求、等一个回复"在高延迟链路上吞吐 ≈ 块大小 / RTT，不可用。这里同时保持
//! `window` 个在途请求（默认 64 × 32 KiB）：先把窗口填满，再按**请求顺序**收响应、顺序写本地。
//! 响应在网络上可以乱序到达（每个请求有自己的 oneshot），但消费顺序固定，所以本地写入始终是
//! 顺序追加，不需要 `pwrite`，也不需要把乱序的块缓存起来。
//!
//! # 内存上限
//!
//! 任何时刻最多 `window` 个请求在途，每个至多 `chunk_size` 字节数据，
//! 所以引擎持有的数据缓冲 ≤ `window × chunk_size`，与文件大小无关。
//!
//! # 取消与清理
//!
//! 取消标记在每个块之间检查。取消、失败后：下载删除本地残缺文件；上传先关闭远端句柄再删除远端文件，
//! 因为残缺文件会被误当成完整文件。连接已断时远端清理必然失败，只能尽力而为。

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kanso_types::message::{ByteCount, Message, Reason, SftpOp};
use kanso_types::transfer::{ConflictPolicy, XferProtocol};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::client::{
    SftpClient, close_handle, expect_ok, open_exclusive, open_file, remove, status_result,
};
use super::error::SftpError;
use super::proto;
use super::session::{Reply, Session};

/// 默认在途请求数。
pub const DEFAULT_WINDOW: usize = 64;
/// 默认块大小：32 KiB 是 SFTP 服务器普遍接受的单次 READ/WRITE 上限。
pub const DEFAULT_CHUNK: u32 = 32 * 1024;
/// 块大小上限。超过的请求很多服务器会截短（短读），也会逼近 [`proto::MAX_PACKET`]。
pub const MAX_CHUNK: u32 = 256 * 1024;
/// 进度回调的最小间隔（ssh.md §9.1）。
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// 取消标记，可克隆；任何一份 `cancel()` 后所有克隆都能看到。
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// 新建（未取消）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消。传输会在下一个块边界停止并清理。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// 是否已请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 在途数据缓冲量的计数器，用来验证"内存与文件大小无关"。
///
/// 按在途请求计量：请求发出时占用 `len` 字节，响应被消费（写入本地 / 收到确认）后释放。
/// 只在测试与诊断里使用，不传时没有任何开销。
#[derive(Debug, Default)]
pub struct BufferGauge {
    cur: AtomicUsize,
    peak: AtomicUsize,
}

impl BufferGauge {
    /// 新建计数器。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 当前占用字节数。
    pub fn current(&self) -> usize {
        self.cur.load(Ordering::SeqCst)
    }

    /// 历史峰值字节数。
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

/// 一次占用；释放（drop）时归还。
struct Held {
    gauge: Option<Arc<BufferGauge>>,
    n: usize,
}

impl Held {
    fn new(gauge: &Option<Arc<BufferGauge>>, n: usize) -> Self {
        if let Some(g) = gauge {
            let now = g.cur.fetch_add(n, Ordering::SeqCst) + n;
            g.peak.fetch_max(now, Ordering::SeqCst);
        }
        Self {
            gauge: gauge.clone(),
            n,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(g) = &self.gauge {
            g.cur.fetch_sub(self.n, Ordering::SeqCst);
        }
    }
}

/// 传输进度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    /// 已完成字节数（下载：已写入本地；上传：已被服务器确认）。
    pub bytes: u64,
    /// 总字节数；单文件下载时取自服务器报告的大小，没有则为 `None`。
    pub total: Option<u64>,
    /// 已完成的文件数。
    pub files_done: u64,
    /// 总文件数（单文件为 1）。
    pub files_total: u64,
}

/// 进度回调。在传输任务里同步调用，实现必须快速返回（通常只是转发到事件通道）。
pub type ProgressFn = Arc<dyn Fn(TransferProgress) + Send + Sync>;

/// 传输参数。
#[derive(Clone)]
pub struct TransferOptions {
    /// 在途请求数，至少 1。
    pub window: usize,
    /// 每个请求的数据块大小，限制在 `1..=MAX_CHUNK`。
    pub chunk_size: u32,
    /// 目标已存在时的策略，默认 [`ConflictPolicy::Fail`]。
    ///
    /// 默认失败：引擎是库，调用方忘了先问用户时应当失败而不是悄悄覆盖。`Fail` 时下载用
    /// `create_new`、上传用 `EXCL`，目标已存在报可区分的错误（见 [`TransferError::is_already_exists`]），
    /// 调用方询问用户后带 `Overwrite` 或 `Skip` 重试。`Skip` 同样靠 `create_new` / `EXCL` 的原子判定
    /// （不先 stat 再写，避免检查与创建之间的竞争），已存在的文件记入 [`TransferReport::skipped`]
    /// （[`SkipReason::AlreadyExists`]）而不是失败。目录下载在写任何文件之前就检查本地已存在的文件
    /// （`Fail` 时整体失败，`Skip` 时从计划中剔除）；目录上传的远端冲突只能在传到该文件时才发现，
    /// 此前的文件已经传完。
    pub conflict: ConflictPolicy,
    /// 进度回调。
    pub progress: Option<ProgressFn>,
    /// 取消标记。
    pub cancel: CancelToken,
    /// 在途缓冲计数器（测试 / 诊断）。
    pub gauge: Option<Arc<BufferGauge>>,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            window: DEFAULT_WINDOW,
            chunk_size: DEFAULT_CHUNK,
            conflict: ConflictPolicy::Fail,
            progress: None,
            cancel: CancelToken::new(),
            gauge: None,
        }
    }
}

impl TransferOptions {
    fn window(&self) -> usize {
        self.window.max(1)
    }

    fn chunk(&self) -> u32 {
        self.chunk_size.clamp(1, MAX_CHUNK)
    }
}

/// 传输失败的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferError {
    /// 远端 SFTP 操作失败（含连接中断）。
    Sftp(SftpError),
    /// 读本地文件 / 目录失败。
    LocalRead {
        /// 本地路径。
        path: String,
        /// 原因。
        reason: Reason,
    },
    /// 写本地文件 / 建本地目录失败。
    LocalWrite {
        /// 本地路径。
        path: String,
        /// 原因。
        reason: Reason,
    },
    /// 总大小超出 `u64`。
    SizeOverflow,
    /// 远端路径应是目录而不是。
    RemoteNotDir {
        /// 远端路径。
        path: String,
    },
    /// 远端文件与它报告的大小不符（传输途中被截短）等协议层面的异常。
    Protocol {
        /// 英文技术细节。
        detail: String,
    },
}

impl TransferError {
    /// 失败原因是否"目标已存在"（上传：远端；下载：本地），用于决定是否询问用户后带 `overwrite = true` 重试。
    pub fn is_already_exists(&self) -> bool {
        match self {
            TransferError::Sftp(e) => e.is_already_exists(),
            TransferError::LocalWrite { reason, .. } => *reason == Reason::AlreadyExists,
            _ => false,
        }
    }

    /// 映射成面向用户的消息，只用 `kanso-types` 里已有的 key。
    ///
    /// 目标已存在：上传映射为 `sftp-file-exists`，下载映射为 `xfer-local-write-failed`（原因 already-exists）。
    pub fn to_message(&self) -> Message {
        match self {
            TransferError::Sftp(e) => e.to_message(),
            TransferError::LocalRead { path, reason } => Message::XferLocalReadFailed {
                path: path.clone(),
                reason: reason.clone(),
            },
            TransferError::LocalWrite { path, reason } => Message::XferLocalWriteFailed {
                path: path.clone(),
                reason: reason.clone(),
            },
            TransferError::SizeOverflow => Message::XferSizeOverflow {},
            TransferError::RemoteNotDir { path } => {
                Message::XferRemoteNotDir { path: path.clone() }
            }
            TransferError::Protocol { detail } => Message::XferProtocolError {
                protocol: XferProtocol::Sftp,
                detail: detail.clone(),
            },
        }
    }

    /// 成功完成时给用户的消息（`xfer-completed`）。
    pub fn completed_message(name: &str, bytes: u64) -> Message {
        Message::XferCompleted {
            name: name.to_owned(),
            size: ByteCount(bytes),
        }
    }
}

impl From<SftpError> for TransferError {
    fn from(e: SftpError) -> Self {
        TransferError::Sftp(e)
    }
}

/// 传输结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferOutcome {
    /// 全部完成。
    Completed,
    /// 用户取消。半成品已清理。
    Cancelled,
    /// 失败。半成品已尽力清理。
    Failed(TransferError),
}

/// 目录传输中被跳过的项的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// 远端文件名不安全（含分隔符、`..`、绝对路径、空名等），写出去可能跑出目标目录。
    UnsafeName,
    /// 子目录读取失败（例如无权限）。
    Unreadable,
    /// 符号链接指向目录（可能指回上级形成死循环）。
    SymlinkToDir,
    /// 悬空符号链接（或指向对象无法 STAT）。
    DanglingSymlink,
    /// 不是普通文件 / 目录（socket、设备、管道等）。
    NotRegularFile,
    /// 目录嵌套超过 [`super::tree::MAX_DEPTH`]。
    TooDeep,
    /// 本地文件名不是合法 UTF-8，无法作为远端路径。
    NonUtf8Name,
    /// 本地目标路径上已有符号链接，或类型与计划不符（例如要写文件的位置是目录）。
    LocalConflict,
    /// 目标文件已存在，按 [`ConflictPolicy::Skip`] 跳过（不是错误）。
    AlreadyExists,
    /// 同一目录下重复出现同名项（正常服务器不会这样，防的是恶意列表）。
    DuplicateName,
}

/// 被跳过的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedItem {
    /// 项的路径（下载是远端路径，上传是本地路径）。
    pub path: String,
    /// 原因。
    pub reason: SkipReason,
}

/// 传输报告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferReport {
    /// 结果。
    pub outcome: TransferOutcome,
    /// 已完成的字节数。
    pub bytes: u64,
    /// 已完成的文件数。
    pub files: u64,
    /// 被跳过的项。
    pub skipped: Vec<SkippedItem>,
}

/// 单个文件传输的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileResult {
    /// 已传输。
    Done,
    /// 目标已存在且策略为 `Skip`，什么也没写。
    Skipped,
}

/// 传输内部的提前结束。
pub(crate) enum Stop {
    Cancelled,
    Failed(TransferError),
}

impl From<SftpError> for Stop {
    fn from(e: SftpError) -> Self {
        Stop::Failed(TransferError::Sftp(e))
    }
}

impl From<TransferError> for Stop {
    fn from(e: TransferError) -> Self {
        Stop::Failed(e)
    }
}

pub(crate) fn io_reason(e: &io::Error) -> Reason {
    Reason::from(e)
}

pub(crate) fn local_read(path: &Path, e: &io::Error) -> Stop {
    Stop::Failed(TransferError::LocalRead {
        path: path.display().to_string(),
        reason: io_reason(e),
    })
}

pub(crate) fn local_write(path: &Path, e: &io::Error) -> Stop {
    Stop::Failed(TransferError::LocalWrite {
        path: path.display().to_string(),
        reason: io_reason(e),
    })
}

/// 进度节流器。
///
/// 回调最多每 [`PROGRESS_INTERVAL`] 一次；开始（`start`）与结束（`finish`）强制各报一次。
/// 目录传输中多个文件共用同一个节流器，保证整棵树的回调间隔也满足上限。
pub(crate) struct Tracker {
    cb: Option<ProgressFn>,
    last: Option<Instant>,
    pub p: TransferProgress,
}

impl Tracker {
    pub fn new(cb: Option<ProgressFn>, total: Option<u64>, files_total: u64) -> Self {
        Self {
            cb,
            last: None,
            p: TransferProgress {
                bytes: 0,
                total,
                files_done: 0,
                files_total,
            },
        }
    }

    fn emit(&mut self) {
        self.last = Some(Instant::now());
        if let Some(cb) = &self.cb {
            cb(self.p);
        }
    }

    /// 强制报一次（开始 / 结束）。
    pub fn force(&mut self) {
        self.emit();
    }

    /// 前进 `n` 字节，间隔够长才回调。
    pub fn advance(&mut self, n: u64) {
        self.p.bytes += n;
        if self.last.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL) {
            self.emit();
        }
    }
}

/// 在途的一个请求。`_held` 随槽位释放而归还计数。
struct Slot {
    offset: u64,
    len: u32,
    reply: Reply,
    _held: Held,
}

async fn send_read(s: &Session, handle: &[u8], offset: u64, len: u32) -> Result<Reply, SftpError> {
    s.send(proto::FXP_READ, handle.len() + 16, |b| {
        proto::put_str(b, handle);
        proto::put_u64(b, offset);
        proto::put_u32(b, len);
    })
    .await
}

/// 下载一个文件到 `local`。
///
/// `size_hint`：目录下载已从扫描里知道大小时传入，省一次 FSTAT；`single` 为真时把总量写进进度并报第一次 0。
pub(crate) async fn download_one(
    s: &Session,
    remote: &str,
    local: &Path,
    size_hint: Option<u64>,
    opts: &TransferOptions,
    tr: &mut Tracker,
    single: bool,
) -> Result<FileResult, Stop> {
    let handle = open_file(s, remote, proto::OPEN_READ, SftpOp::Open).await?;
    let mut created = false;
    let res = download_body(
        s,
        &handle,
        remote,
        local,
        size_hint,
        opts,
        tr,
        single,
        &mut created,
    )
    .await;
    // 读句柄关闭失败不影响已写好的数据，忽略；出错路径上连接可能已断，同样无处可报。
    let _ = close_handle(s, &handle, remote, SftpOp::Open).await;
    if res.is_err() && created {
        // 只删自己创建 / 截断过的文件：`create_new` 因已存在而失败时 `created` 为假，不会误删用户文件。
        let _ = tokio::fs::remove_file(local).await;
    }
    res
}

#[allow(clippy::too_many_arguments)]
async fn download_body(
    s: &Session,
    handle: &[u8],
    remote: &str,
    local: &Path,
    size_hint: Option<u64>,
    opts: &TransferOptions,
    tr: &mut Tracker,
    single: bool,
    created: &mut bool,
) -> Result<FileResult, Stop> {
    let size = match size_hint {
        Some(sz) => Some(sz),
        None => {
            let r = s
                .send(proto::FXP_FSTAT, handle.len() + 4, |b| {
                    proto::put_str(b, handle);
                })
                .await?
                .wait()
                .await?;
            // FSTAT 失败不致命：按"大小未知"读到 EOF。
            super::client::parse_attrs_reply(r, remote)
                .ok()
                .and_then(|a| a.size)
        }
    };
    // 服务器报告 0（或没报）时不能当成空文件：procfs 之类的特殊文件声称 0 字节却有内容。
    // 这时退化为串行读到 EOF，真实空文件只多花一个往返。
    let size = size.filter(|&sz| sz > 0);
    if single {
        tr.p.total = size;
    }

    let mut oo = tokio::fs::OpenOptions::new();
    oo.write(true);
    if opts.conflict == ConflictPolicy::Overwrite {
        oo.create(true).truncate(true);
    } else {
        oo.create_new(true);
    }
    let mut file = match oo.open(local).await {
        Ok(f) => f,
        // Skip：`create_new` 报已存在就是要跳过的文件（符号链接也算已存在，O_EXCL 不跟随链接）。
        // 此时 `created` 仍为假，调用方不会删除这个用户文件。
        Err(e)
            if opts.conflict == ConflictPolicy::Skip
                && e.kind() == io::ErrorKind::AlreadyExists =>
        {
            return Ok(FileResult::Skipped);
        }
        Err(e) => return Err(local_write(local, &e)),
    };
    *created = true;
    if single {
        tr.force();
    }

    // 大小未知时只保持一个在途请求：不知道文件多长，预发多个读只会换来一串 EOF。
    let window = if size.is_some() { opts.window() } else { 1 };
    let chunk = u64::from(opts.chunk());
    let mut q: VecDeque<Slot> = VecDeque::new();
    let mut next_off = 0u64;
    let mut received = 0u64;

    loop {
        if opts.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        while q.len() < window {
            let len = match size {
                Some(sz) if next_off >= sz => break,
                Some(sz) => (sz - next_off).min(chunk) as u32,
                None => chunk as u32,
            };
            let reply = send_read(s, handle, next_off, len).await?;
            q.push_back(Slot {
                offset: next_off,
                len,
                reply,
                _held: Held::new(&opts.gauge, len as usize),
            });
            next_off += u64::from(len);
        }
        let Some(slot) = q.pop_front() else { break };
        let resp = slot.reply.wait().await?;
        if resp.ty == proto::FXP_STATUS {
            let code = resp.reader().u32()?;
            if code == proto::FX_EOF {
                // 后面还在途的请求也都会是 EOF，它们的响应由读任务丢弃。
                q.clear();
                break;
            }
            status_result(&resp, SftpOp::Open, remote)?;
            return Err(Stop::Failed(TransferError::Protocol {
                detail: "OK status in reply to READ".into(),
            }));
        }
        if resp.ty != proto::FXP_DATA {
            return Err(SftpError::Protocol {
                detail: format!("unexpected response type {} to READ", resp.ty),
            }
            .into());
        }
        let mut rd = resp.reader();
        let data = rd.bytes()?;
        let n = data.len() as u32;
        if n > slot.len {
            return Err(TransferError::Protocol {
                detail: "server returned more data than requested".into(),
            }
            .into());
        }
        if n == 0 {
            return Err(TransferError::Protocol {
                detail: "server returned an empty DATA reply".into(),
            }
            .into());
        }
        file.write_all(data)
            .await
            .map_err(|e| local_write(local, &e))?;
        received += u64::from(n);
        tr.advance(u64::from(n));
        if n < slot.len {
            // 短读：服务器给的比请求的少。不能把缺口留着——后面在途的块已经按各自偏移排好队，
            // 把缺的那段作为新请求插回队首，保证写入顺序与偏移一致。
            let off = slot.offset + u64::from(n);
            let len = slot.len - n;
            let reply = send_read(s, handle, off, len).await?;
            q.push_front(Slot {
                offset: off,
                len,
                reply,
                _held: Held::new(&opts.gauge, len as usize),
            });
        }
        // `slot` 在此处释放，计数归还。
    }

    file.flush().await.map_err(|e| local_write(local, &e))?;
    drop(file);
    if size.is_none() && single {
        // 大小未知时总量只有读完才知道：结束时 total = 实际字节数。
        tr.p.total = Some(received);
    }
    if let Some(sz) = size
        && received != sz
    {
        return Err(TransferError::Protocol {
            detail: format!("remote file ended at {received} bytes, expected {sz}"),
        }
        .into());
    }
    Ok(FileResult::Done)
}

/// 上传一个本地文件到 `remote`。语义见模块文档；`single` 同 [`download_one`]。
pub(crate) async fn upload_one(
    s: &Session,
    local: &Path,
    remote: &str,
    opts: &TransferOptions,
    tr: &mut Tracker,
    single: bool,
) -> Result<FileResult, Stop> {
    let mut file = tokio::fs::File::open(local)
        .await
        .map_err(|e| local_read(local, &e))?;
    let size = file
        .metadata()
        .await
        .map_err(|e| local_read(local, &e))?
        .len();
    if single {
        tr.p.total = Some(size);
    }

    let flags = proto::OPEN_WRITE
        | proto::OPEN_CREAT
        | if opts.conflict == ConflictPolicy::Overwrite {
            proto::OPEN_TRUNC
        } else {
            proto::OPEN_EXCL
        };
    // 打开失败时远端什么也没改，不需要清理；成功后无论如何收尾都要清理残缺文件。
    let handle = match open_exclusive(s, remote, flags, SftpOp::Open).await {
        Ok(h) => h,
        // Skip：远端已存在就跳过；没有创建过任何东西，不需要清理。
        Err(e) if opts.conflict == ConflictPolicy::Skip && e.is_already_exists() => {
            return Ok(FileResult::Skipped);
        }
        Err(e) => return Err(e.into()),
    };
    if single {
        tr.force();
    }
    let res = upload_body(s, &handle, &mut file, local, remote, opts, tr).await;
    let closed = close_handle(s, &handle, remote, SftpOp::Open).await;
    let res = match (res, closed) {
        (Ok(()), Err(e)) => Err(e.into()),
        (r, _) => r,
    };
    if res.is_err() {
        // 先关句柄再删：多数服务器在句柄未关时也允许删，但关闭后删最稳妥。
        let _ = remove(s, proto::FXP_REMOVE, remote).await;
    }
    res.map(|()| FileResult::Done)
}

async fn upload_body(
    s: &Session,
    handle: &[u8],
    file: &mut tokio::fs::File,
    local: &Path,
    remote: &str,
    opts: &TransferOptions,
    tr: &mut Tracker,
) -> Result<(), Stop> {
    let window = opts.window();
    let chunk = opts.chunk() as usize;
    // 单个复用缓冲：`send` 返回时整包已写进通道，缓冲可以立刻复用，
    // 所以引擎自己的数据缓冲只有这一块；在途窗口占用的是通道侧的发送缓冲，受 SSH 窗口限制。
    let mut buf = vec![0u8; chunk];
    let mut q: VecDeque<(Reply, u32, Held)> = VecDeque::new();
    let mut offset = 0u64;
    let mut eof = false;

    loop {
        if opts.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        if !eof && q.len() < window {
            let mut filled = 0;
            while filled < chunk {
                let n = file
                    .read(&mut buf[filled..])
                    .await
                    .map_err(|e| local_read(local, &e))?;
                if n == 0 {
                    eof = true;
                    break;
                }
                filled += n;
            }
            if filled > 0 {
                let data = &buf[..filled];
                let off = offset;
                let reply = s
                    .send(proto::FXP_WRITE, handle.len() + filled + 16, |b| {
                        proto::put_str(b, handle);
                        proto::put_u64(b, off);
                        proto::put_str(b, data);
                    })
                    .await?;
                q.push_back((reply, filled as u32, Held::new(&opts.gauge, filled)));
                offset += filled as u64;
            }
            continue;
        }
        let Some((reply, len, _held)) = q.pop_front() else {
            break;
        };
        let r = reply.wait().await?;
        expect_ok(&r, SftpOp::Open, remote)?;
        tr.advance(u64::from(len));
    }
    Ok(())
}

impl SftpClient {
    /// 下载单个文件到本地路径 `local`（父目录必须已存在）。
    ///
    /// 永远返回报告：成功 / 取消 / 失败都在 [`TransferReport::outcome`]。取消或失败时本地残缺文件已删除。
    pub async fn download_file(
        &self,
        remote: &str,
        local: &Path,
        opts: &TransferOptions,
    ) -> TransferReport {
        let mut tr = Tracker::new(opts.progress.clone(), None, 1);
        let res = async {
            let s = self.session().await?;
            download_one(&s, remote, local, None, opts, &mut tr, true).await
        }
        .await;
        finish_single(res, tr, local)
    }

    /// 上传单个本地文件到远端路径 `remote`。
    ///
    /// 取消或失败时：关闭远端句柄后删除远端文件（连接已断则无法删除）。
    pub async fn upload_file(
        &self,
        local: &Path,
        remote: &str,
        opts: &TransferOptions,
    ) -> TransferReport {
        let mut tr = Tracker::new(opts.progress.clone(), None, 1);
        let res = async {
            let s = self.session().await?;
            upload_one(&s, local, remote, opts, &mut tr, true).await
        }
        .await;
        finish_single(res, tr, Path::new(remote))
    }
}

/// `target` 是已存在而被跳过的目标路径（下载是本地路径，上传是远端路径），只在 `Skipped` 时写进报告。
fn finish_single(res: Result<FileResult, Stop>, mut tr: Tracker, target: &Path) -> TransferReport {
    let mut skipped = Vec::new();
    let outcome = match res {
        Ok(FileResult::Done) => {
            tr.p.files_done = 1;
            TransferOutcome::Completed
        }
        // 跳过按完成处理，但不计入已完成文件数，UI 通过 `skipped` 得知。
        Ok(FileResult::Skipped) => {
            skipped.push(SkippedItem {
                path: target.display().to_string(),
                reason: SkipReason::AlreadyExists,
            });
            TransferOutcome::Completed
        }
        Err(Stop::Cancelled) => TransferOutcome::Cancelled,
        Err(Stop::Failed(e)) => TransferOutcome::Failed(e),
    };
    // 结束时无论节流状态都报一次最终值。
    tr.force();
    TransferReport {
        outcome,
        bytes: tr.p.bytes,
        files: tr.p.files_done,
        skipped,
    }
}
