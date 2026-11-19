//! 测试用 SFTP v3 服务器：把请求映射到一个**临时目录**里的真实文件系统，可注入故障。
//!
//! 只服务于测试，不属于公开 API。虚拟根 `/` 对应 [`SftpServerConfig::root`]，
//! 路径一律先做词法规范化再拼到 root 下，`..` 不会跑出 root。
//! 每个请求在独立任务里处理（先 sleep `delay` 再执行），所以延迟不会让请求串行：
//! 这正是用来验证客户端流水线的前提。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;

use crate::sftp::proto::{self, FileAttrs, Reader, put_str, put_u32};

/// 服务器行为配置。
#[derive(Clone)]
pub struct SftpServerConfig {
    /// 虚拟根对应的真实目录。
    pub root: PathBuf,
    /// `REALPATH "."` 返回的家目录（虚拟路径）。
    pub home: String,
    /// 每个请求的响应延迟（并发处理，不串行）。
    pub delay: Duration,
    /// 每个 READ 最多返回的字节数（模拟短读）。
    pub max_read: Option<u32>,
    /// 每个 READDIR 返回的项数。
    pub readdir_batch: usize,
    /// 每个 READDIR 的额外延迟（模拟大目录逐批到达）。
    pub readdir_delay: Duration,
    /// 发出 N 个 DATA 响应后断开通道（不回答触发断开的那个请求）。
    pub disconnect_after_reads: Option<usize>,
    /// 收到 N 个 WRITE 之后断开通道（第 N 个照常处理，之后的请求不再处理）。
    pub disconnect_after_writes: Option<usize>,
    /// 访问这些虚拟路径前缀下的任何对象都返回 PERMISSION_DENIED。
    pub deny_prefixes: Vec<String>,
    /// longname 里带的属主 / 属组名；`None` 时 longname 用数字。
    pub owner_names: Option<(String, String)>,
    /// REALPATH 一律失败（模拟不支持规范化的服务器）。
    pub realpath_fails: bool,
    /// 每个目录列表里额外注入的"普通文件"项 `(名字, 大小)`，用来模拟恶意 / 异常的文件名与大小
    /// （真实文件系统造不出 `..`、含 `/` 的名字）。这些项不对应真实文件，客户端不应该去打开它们。
    pub inject_entries: Vec<(String, u64)>,
    /// 所有普通文件的 attrs 都声称 size = 0（模拟 procfs 之类大小不可信的文件）。
    pub zero_size_attrs: bool,
    /// VERSION 回复里是否声明 `posix-rename@openssh.com`（并处理对应的 EXTENDED 请求）。
    pub posix_rename: bool,
    /// 第 N 个 WRITE 起一律回 FAILURE（通道保持正常，用来验证写失败后的清理）。
    pub fail_writes_after: Option<usize>,
}

impl SftpServerConfig {
    /// 以 `root` 为根的默认配置。
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            home: "/".into(),
            delay: Duration::ZERO,
            max_read: None,
            readdir_batch: 100,
            readdir_delay: Duration::ZERO,
            disconnect_after_reads: None,
            disconnect_after_writes: None,
            deny_prefixes: Vec::new(),
            owner_names: None,
            realpath_fails: false,
            inject_entries: Vec::new(),
            zero_size_attrs: false,
            posix_rename: true,
            fail_writes_after: None,
        }
    }
}

/// 服务器侧观察到的统计。
#[derive(Default)]
pub struct SftpServerStats {
    /// 已收到、尚未响应的请求数。
    inflight: AtomicUsize,
    /// 观察到的最大在途请求数。
    max_inflight: AtomicUsize,
    /// 收到的子系统请求次数（验证"第一次请求时才打开"）。
    pub subsystem_opens: AtomicUsize,
    /// 当前打开的句柄数（验证句柄不泄漏）。
    open_handles: AtomicUsize,
    /// 已发出的 DATA 响应数。
    pub reads: AtomicUsize,
    /// 已收到的 WRITE 请求数。
    pub writes: AtomicUsize,
    /// 收到的请求总数。
    pub requests: AtomicUsize,
}

impl SftpServerStats {
    /// 最大在途请求数。
    pub fn max_inflight(&self) -> usize {
        self.max_inflight.load(Ordering::SeqCst)
    }
    /// 当前打开的句柄数。
    pub fn open_handles(&self) -> usize {
        self.open_handles.load(Ordering::SeqCst)
    }
    /// 重置最大在途计数（在一个测试步骤前调用）。
    pub fn reset_max_inflight(&self) {
        self.max_inflight.store(0, Ordering::SeqCst);
    }
}

enum Handle {
    File(Mutex<std::fs::File>),
    Dir {
        /// `(名字, attrs, longname)`。
        entries: Vec<(String, FileAttrs, String)>,
        pos: Mutex<usize>,
    },
}

struct Shared {
    cfg: SftpServerConfig,
    stats: Arc<SftpServerStats>,
    handles: Mutex<HashMap<Vec<u8>, Arc<Handle>>>,
    next_handle: AtomicUsize,
    writer: AsyncMutex<Box<dyn AsyncWrite + Send + Unpin>>,
    /// 置位后读循环退出（断线注入）。
    kill: tokio::sync::Notify,
}

fn status_of(e: &std::io::Error) -> u32 {
    match e.kind() {
        std::io::ErrorKind::NotFound => proto::FX_NO_SUCH_FILE,
        std::io::ErrorKind::PermissionDenied => proto::FX_PERMISSION_DENIED,
        // OpenSSH 对 O_EXCL 冲突、非空目录删除等都回通用 FAILURE。
        _ => proto::FX_FAILURE,
    }
}

fn status_packet(id: u32, code: u32, msg: &str) -> Vec<u8> {
    let mut p = proto::begin_packet(proto::FXP_STATUS, Some(id), 16 + msg.len());
    put_u32(&mut p, code);
    put_str(&mut p, msg.as_bytes());
    put_str(&mut p, b"en");
    proto::finish_packet(&mut p);
    p
}

/// 词法规范化虚拟路径（相对路径按 `home` 解析）。
fn normalize(home: &str, path: &str) -> Vec<String> {
    let full = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{home}/{path}")
    };
    let mut out: Vec<String> = Vec::new();
    for c in full.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            x => out.push(x.to_owned()),
        }
    }
    out
}

fn virt(parts: &[String]) -> String {
    format!("/{}", parts.join("/"))
}

#[cfg(unix)]
fn ids(md: &std::fs::Metadata) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (md.uid(), md.gid())
}

#[cfg(not(unix))]
fn ids(_: &std::fs::Metadata) -> (u32, u32) {
    (0, 0)
}

fn mode_of(md: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.mode()
    }
    #[cfg(not(unix))]
    {
        let t = md.file_type();
        let ty = if t.is_dir() {
            proto::S_IFDIR
        } else if t.is_symlink() {
            proto::S_IFLNK
        } else {
            proto::S_IFREG
        };
        ty | 0o644
    }
}

fn attrs_of(sh: &Shared, md: &std::fs::Metadata) -> FileAttrs {
    let (uid, gid) = ids(md);
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32);
    FileAttrs {
        size: Some(if sh.cfg.zero_size_attrs && md.is_file() {
            0
        } else {
            md.len()
        }),
        uid: Some(uid),
        gid: Some(gid),
        permissions: Some(mode_of(md)),
        atime: mtime,
        mtime,
    }
}

fn longname(sh: &Shared, name: &str, a: &FileAttrs) -> String {
    let m = a.permissions.unwrap_or(0);
    let t = match m & proto::S_IFMT {
        proto::S_IFDIR => 'd',
        proto::S_IFLNK => 'l',
        _ => '-',
    };
    let (u, g) = match &sh.cfg.owner_names {
        Some((u, g)) => (u.clone(), g.clone()),
        None => (
            a.uid.unwrap_or(0).to_string(),
            a.gid.unwrap_or(0).to_string(),
        ),
    };
    format!(
        "{t}rwxr-xr-x 1 {u} {g} {} Jan  1 00:00 {name}",
        a.size.unwrap_or(0)
    )
}

impl Shared {
    /// 虚拟路径 → 真实路径；命中 deny 前缀返回 PERMISSION_DENIED。
    fn resolve(&self, path: &str) -> Result<PathBuf, u32> {
        let parts = normalize(&self.cfg.home, path);
        let v = virt(&parts);
        if self
            .cfg
            .deny_prefixes
            .iter()
            .any(|d| v == *d || v.starts_with(&format!("{}/", d.trim_end_matches('/'))))
        {
            return Err(proto::FX_PERMISSION_DENIED);
        }
        let mut p = self.cfg.root.clone();
        for c in parts {
            p.push(c);
        }
        Ok(p)
    }

    fn new_handle(&self, h: Handle) -> Vec<u8> {
        let id = self.next_handle.fetch_add(1, Ordering::SeqCst);
        let key = id.to_be_bytes().to_vec();
        self.handles
            .lock()
            .unwrap()
            .insert(key.clone(), Arc::new(h));
        self.stats.open_handles.fetch_add(1, Ordering::SeqCst);
        key
    }

    fn handle(&self, key: &[u8]) -> Option<Arc<Handle>> {
        self.handles.lock().unwrap().get(key).cloned()
    }

    /// 主动关闭写半边：仍在处理中的请求任务持有 `Shared`，光靠返回不会释放流，通道就不会断。
    async fn close(&self) {
        let _ = self.writer.lock().await.shutdown().await;
    }

    async fn send(&self, pkt: Vec<u8>) {
        let mut w = self.writer.lock().await;
        let _ = w.write_all(&pkt).await;
        let _ = w.flush().await;
    }
}

/// 处理一个请求，返回响应包；`None` 表示不回答（断线注入）。
async fn handle_request(sh: &Arc<Shared>, ty: u8, id: u32, body: &[u8]) -> Option<Vec<u8>> {
    let st = |code: u32, msg: &str| Some(status_packet(id, code, msg));
    let mut rd = Reader::new(body);
    macro_rules! tryp {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(_) => return st(proto::FX_BAD_MESSAGE, "bad request"),
            }
        };
    }
    macro_rules! tryio {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return st(status_of(&e), &e.to_string()),
            }
        };
    }
    match ty {
        proto::FXP_REALPATH => {
            let path = tryp!(rd.string());
            if sh.cfg.realpath_fails {
                return st(proto::FX_FAILURE, "realpath disabled");
            }
            let real = match sh.resolve(&path) {
                Ok(p) => p,
                Err(c) => return st(c, "denied"),
            };
            // 与 realpath(3) 一致：不存在的路径失败。
            if !real.exists() {
                return st(proto::FX_NO_SUCH_FILE, "no such file");
            }
            // 与 realpath(3) 一致：解析符号链接。解析结果落在 root 之内时映射回虚拟路径，
            // 否则（指向 root 之外）退回词法规范化。
            let lexical = virt(&normalize(&sh.cfg.home, &path));
            let v = match (real.canonicalize(), sh.cfg.root.canonicalize()) {
                (Ok(c), Ok(r)) => match c.strip_prefix(&r) {
                    Ok(rel) => {
                        let parts: Vec<String> = rel
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect();
                        virt(&parts)
                    }
                    Err(_) => lexical,
                },
                _ => lexical,
            };
            let mut p = proto::begin_packet(proto::FXP_NAME, Some(id), 32);
            put_u32(&mut p, 1);
            put_str(&mut p, v.as_bytes());
            put_str(&mut p, v.as_bytes());
            FileAttrs::default().encode(&mut p);
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_STAT | proto::FXP_LSTAT => {
            let path = tryp!(rd.string());
            let real = match sh.resolve(&path) {
                Ok(p) => p,
                Err(c) => return st(c, "denied"),
            };
            let md = if ty == proto::FXP_STAT {
                std::fs::metadata(&real)
            } else {
                std::fs::symlink_metadata(&real)
            };
            let md = tryio!(md);
            let mut p = proto::begin_packet(proto::FXP_ATTRS, Some(id), 32);
            attrs_of(sh, &md).encode(&mut p);
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_FSTAT => {
            let h = tryp!(rd.bytes());
            let Some(hd) = sh.handle(h) else {
                return st(proto::FX_FAILURE, "bad handle");
            };
            let Handle::File(f) = &*hd else {
                return st(proto::FX_FAILURE, "not a file");
            };
            let md = tryio!(f.lock().unwrap().metadata());
            let mut p = proto::begin_packet(proto::FXP_ATTRS, Some(id), 32);
            attrs_of(sh, &md).encode(&mut p);
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_OPEN => {
            let path = tryp!(rd.string());
            let flags = tryp!(rd.u32());
            let _attrs = tryp!(rd.attrs());
            let real = match sh.resolve(&path) {
                Ok(p) => p,
                Err(c) => return st(c, "denied"),
            };
            let mut oo = std::fs::OpenOptions::new();
            oo.read(flags & proto::OPEN_READ != 0)
                .write(flags & proto::OPEN_WRITE != 0);
            if flags & proto::OPEN_EXCL != 0 {
                oo.create_new(true);
            } else {
                oo.create(flags & proto::OPEN_CREAT != 0)
                    .truncate(flags & proto::OPEN_TRUNC != 0);
            }
            let f = tryio!(oo.open(&real));
            let key = sh.new_handle(Handle::File(Mutex::new(f)));
            let mut p = proto::begin_packet(proto::FXP_HANDLE, Some(id), 16);
            put_str(&mut p, &key);
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_CLOSE => {
            let h = tryp!(rd.bytes());
            if sh.handles.lock().unwrap().remove(h).is_some() {
                sh.stats.open_handles.fetch_sub(1, Ordering::SeqCst);
                st(proto::FX_OK, "")
            } else {
                st(proto::FX_FAILURE, "bad handle")
            }
        }
        proto::FXP_READ => {
            let h = tryp!(rd.bytes());
            let off = tryp!(rd.u64());
            let mut len = tryp!(rd.u32());
            if let Some(m) = sh.cfg.max_read {
                len = len.min(m);
            }
            let Some(hd) = sh.handle(h) else {
                return st(proto::FX_FAILURE, "bad handle");
            };
            let Handle::File(f) = &*hd else {
                return st(proto::FX_FAILURE, "not a file");
            };
            let mut buf = vec![0u8; len as usize];
            let n = {
                let mut f = f.lock().unwrap();
                tryio!(f.seek(SeekFrom::Start(off)));
                // 读到缓冲满或 EOF：普通文件上一次 read 可能少于请求。
                let mut got = 0;
                while got < buf.len() {
                    let r = tryio!(f.read(&mut buf[got..]));
                    if r == 0 {
                        break;
                    }
                    got += r;
                }
                got
            };
            if n == 0 {
                return st(proto::FX_EOF, "eof");
            }
            let sent = sh.stats.reads.fetch_add(1, Ordering::SeqCst) + 1;
            if sh.cfg.disconnect_after_reads.is_some_and(|m| sent > m) {
                sh.kill.notify_one();
                return None;
            }
            let mut p = proto::begin_packet(proto::FXP_DATA, Some(id), n + 4);
            put_str(&mut p, &buf[..n]);
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_WRITE => {
            let h = tryp!(rd.bytes());
            let off = tryp!(rd.u64());
            let data = tryp!(rd.bytes());
            let Some(hd) = sh.handle(h) else {
                return st(proto::FX_FAILURE, "bad handle");
            };
            let Handle::File(f) = &*hd else {
                return st(proto::FX_FAILURE, "not a file");
            };
            let n = sh.stats.writes.fetch_add(1, Ordering::SeqCst) + 1;
            if sh.cfg.fail_writes_after.is_some_and(|m| n > m) {
                return st(proto::FX_FAILURE, "injected write failure");
            }
            if sh.cfg.disconnect_after_writes.is_some_and(|m| n > m) {
                sh.kill.notify_one();
                return None;
            }
            {
                let mut f = f.lock().unwrap();
                tryio!(f.seek(SeekFrom::Start(off)));
                tryio!(f.write_all(data));
            }
            st(proto::FX_OK, "")
        }
        proto::FXP_OPENDIR => {
            let path = tryp!(rd.string());
            let real = match sh.resolve(&path) {
                Ok(p) => p,
                Err(c) => return st(c, "denied"),
            };
            let rdir = tryio!(std::fs::read_dir(&real));
            let dir_md = tryio!(std::fs::metadata(&real));
            let mk = |n: &str, md: &std::fs::Metadata| {
                let a = attrs_of(sh, md);
                let l = longname(sh, n, &a);
                (n.to_owned(), a, l)
            };
            // 与 OpenSSH 一样先给出 `.` 与 `..`，客户端必须自己过滤。
            let mut entries = vec![mk(".", &dir_md), mk("..", &dir_md)];
            let mut rest = Vec::new();
            for e in rdir {
                let e = tryio!(e);
                let md = tryio!(std::fs::symlink_metadata(e.path()));
                rest.push(mk(&e.file_name().to_string_lossy(), &md));
            }
            rest.sort_by(|a, b| a.0.cmp(&b.0));
            entries.extend(rest);
            for (name, size) in &sh.cfg.inject_entries {
                let a = FileAttrs {
                    size: Some(*size),
                    uid: Some(0),
                    gid: Some(0),
                    permissions: Some(proto::S_IFREG | 0o644),
                    atime: Some(0),
                    mtime: Some(0),
                };
                let l = longname(sh, name, &a);
                entries.push((name.clone(), a, l));
            }
            let key = sh.new_handle(Handle::Dir {
                entries,
                pos: Mutex::new(0),
            });
            let mut p = proto::begin_packet(proto::FXP_HANDLE, Some(id), 16);
            put_str(&mut p, &key);
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_READDIR => {
            let h = tryp!(rd.bytes());
            let Some(hd) = sh.handle(h) else {
                return st(proto::FX_FAILURE, "bad handle");
            };
            let Handle::Dir { entries, pos } = &*hd else {
                return st(proto::FX_FAILURE, "not a dir");
            };
            if !sh.cfg.readdir_delay.is_zero() {
                tokio::time::sleep(sh.cfg.readdir_delay).await;
            }
            let (from, to) = {
                let mut pos = pos.lock().unwrap();
                let from = *pos;
                let to = (from + sh.cfg.readdir_batch.max(1)).min(entries.len());
                *pos = to;
                (from, to)
            };
            if from >= entries.len() {
                return st(proto::FX_EOF, "eof");
            }
            let mut p = proto::begin_packet(proto::FXP_NAME, Some(id), 64 * (to - from));
            put_u32(&mut p, (to - from) as u32);
            for (name, a, l) in &entries[from..to] {
                put_str(&mut p, name.as_bytes());
                put_str(&mut p, l.as_bytes());
                a.encode(&mut p);
            }
            proto::finish_packet(&mut p);
            Some(p)
        }
        proto::FXP_REMOVE | proto::FXP_RMDIR | proto::FXP_MKDIR => {
            let path = tryp!(rd.string());
            let real = match sh.resolve(&path) {
                Ok(p) => p,
                Err(c) => return st(c, "denied"),
            };
            let r = match ty {
                proto::FXP_REMOVE => std::fs::remove_file(&real),
                proto::FXP_RMDIR => std::fs::remove_dir(&real),
                _ => std::fs::create_dir(&real),
            };
            tryio!(r);
            st(proto::FX_OK, "")
        }
        proto::FXP_RENAME => {
            let from = tryp!(rd.string());
            let to = tryp!(rd.string());
            let (a, b) = match (sh.resolve(&from), sh.resolve(&to)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(c), _) | (_, Err(c)) => return st(c, "denied"),
            };
            // SFTP v3 的 RENAME 不覆盖已有目标。
            if b.exists() {
                return st(proto::FX_FAILURE, "target exists");
            }
            tryio!(std::fs::rename(a, b));
            st(proto::FX_OK, "")
        }
        proto::FXP_SETSTAT => {
            let path = tryp!(rd.string());
            let attrs = tryp!(rd.attrs());
            let real = match sh.resolve(&path) {
                Ok(p) => p,
                Err(c) => return st(c, "denied"),
            };
            #[cfg(unix)]
            if let Some(mode) = attrs.permissions {
                use std::os::unix::fs::PermissionsExt;
                tryio!(std::fs::set_permissions(
                    &real,
                    std::fs::Permissions::from_mode(mode & 0o7777)
                ));
            }
            #[cfg(not(unix))]
            let _ = (attrs, real);
            st(proto::FX_OK, "")
        }
        proto::FXP_EXTENDED => {
            let name = tryp!(rd.string());
            if name != proto::EXT_POSIX_RENAME || !sh.cfg.posix_rename {
                return st(proto::FX_OP_UNSUPPORTED, "unsupported extension");
            }
            let from = tryp!(rd.string());
            let to = tryp!(rd.string());
            let (a, b) = match (sh.resolve(&from), sh.resolve(&to)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(c), _) | (_, Err(c)) => return st(c, "denied"),
            };
            // rename(2) 语义：覆盖已有目标。
            tryio!(std::fs::rename(a, b));
            st(proto::FX_OK, "")
        }
        _ => st(proto::FX_OP_UNSUPPORTED, "unsupported"),
    }
}

/// 在一条字节流上提供 SFTP 服务，直到对端关闭或注入的断线触发。
pub async fn serve<S>(stream: S, cfg: SftpServerConfig, stats: Arc<SftpServerStats>)
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (mut rd, wr) = tokio::io::split(stream);
    let sh = Arc::new(Shared {
        cfg,
        stats,
        handles: Mutex::new(HashMap::new()),
        next_handle: AtomicUsize::new(1),
        writer: AsyncMutex::new(Box::new(wr)),
        kill: tokio::sync::Notify::new(),
    });
    loop {
        let mut len = [0u8; 4];
        let r = tokio::select! {
            r = rd.read_exact(&mut len) => r.is_ok(),
            () = sh.kill.notified() => false,
        };
        if !r {
            sh.close().await;
            return;
        }
        let len = u32::from_be_bytes(len) as usize;
        let mut buf = vec![0u8; len];
        if rd.read_exact(&mut buf).await.is_err() {
            sh.close().await;
            return;
        }
        let ty = buf[0];
        if ty == proto::FXP_INIT {
            let mut p = proto::begin_packet(proto::FXP_VERSION, None, 4);
            put_u32(&mut p, 3);
            if sh.cfg.posix_rename {
                put_str(&mut p, proto::EXT_POSIX_RENAME.as_bytes());
                put_str(&mut p, b"1");
            }
            proto::finish_packet(&mut p);
            sh.send(p).await;
            continue;
        }
        let id = Reader::new(&buf[1..]).u32().unwrap_or(0);
        let sh2 = sh.clone();
        sh.stats.requests.fetch_add(1, Ordering::SeqCst);
        let now = sh.stats.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        sh.stats.max_inflight.fetch_max(now, Ordering::SeqCst);
        tokio::spawn(async move {
            if !sh2.cfg.delay.is_zero() {
                tokio::time::sleep(sh2.cfg.delay).await;
            }
            let resp = handle_request(&sh2, ty, id, &buf[5..]).await;
            sh2.stats.inflight.fetch_sub(1, Ordering::SeqCst);
            if let Some(p) = resp {
                sh2.send(p).await;
            }
        });
    }
}
