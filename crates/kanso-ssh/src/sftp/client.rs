//! [`SftpClient`]：在 [`Connection`] 上按需打开 `sftp` 子系统，提供目录与文件操作（ssh.md §7、§8）。

use std::sync::Arc;

use kanso_types::fs::DirEntry;
use kanso_types::message::SftpOp;
use tokio::sync::Mutex as AsyncMutex;

use super::error::{SftpError, StatusKind};
use super::proto::{self, FileAttrs};
use super::session::{Reply, Response, Session};
use crate::{Connection, SshError};

/// SFTP 客户端。可廉价克隆，克隆体共享同一条子系统通道。
///
/// 子系统在**第一次请求时**才打开，之后复用；通道结束（连接断开、服务器关闭）后，
/// 下一次请求会在连接仍可用时重新打开，连接已断则报错，不偷偷重连整条 SSH（ssh.md §7）。
/// 通道上的所有请求都是并发安全的：目录列表、传输与其他文件操作可以同时进行，互不排队。
#[derive(Clone)]
pub struct SftpClient {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    conn: Connection,
    session: AsyncMutex<Option<Arc<Session>>>,
}

impl SftpClient {
    /// 基于一条已认证的连接创建客户端。此时不会打开任何通道。
    pub fn new(conn: Connection) -> Self {
        Self {
            inner: Arc::new(Inner {
                conn,
                session: AsyncMutex::new(None),
            }),
        }
    }

    /// 取（必要时打开）子系统会话。
    ///
    /// 持有异步锁完成整个"检查 → 打开 → 版本协商"过程：并发的第一次请求只会开一条通道。
    pub(crate) async fn session(&self) -> Result<Arc<Session>, SftpError> {
        let mut guard = self.inner.session.lock().await;
        if let Some(s) = guard.as_ref()
            && !s.is_dead()
        {
            return Ok(s.clone());
        }
        *guard = None;
        let conn = &self.inner.conn;
        let stream = crate::channel::open_subsystem(conn, "sftp")
            .await
            .map_err(|e| match e {
                SshError::ChannelFailed { endpoint, detail } => {
                    SftpError::SubsystemFailed { endpoint, detail }
                }
                other => SftpError::Ssh(other),
            })?;
        let s = Arc::new(Session::start(stream, Some(conn.clone())).await?);
        *guard = Some(s.clone());
        Ok(s)
    }

    /// 家目录：解析 `.` 的规范路径。
    pub async fn home(&self) -> Result<String, SftpError> {
        self.realpath(".").await
    }

    /// 规范化路径（REALPATH）。
    pub async fn realpath(&self, path: &str) -> Result<String, SftpError> {
        let s = self.session().await?;
        realpath(&s, path).await
    }

    /// 取属性，跟随符号链接。
    pub async fn stat(&self, path: &str) -> Result<FileAttrs, SftpError> {
        let s = self.session().await?;
        stat(&s, proto::FXP_STAT, path).await
    }

    /// 取属性，不跟随符号链接。
    pub async fn lstat(&self, path: &str) -> Result<FileAttrs, SftpError> {
        let s = self.session().await?;
        stat(&s, proto::FXP_LSTAT, path).await
    }

    /// 打开目录列表。先规范化路径（失败就用原路径），然后 OPENDIR。
    ///
    /// 返回的 [`DirListing`] 按 READDIR 批次流式给出结果，调用方可以边收边显示。
    pub async fn list(&self, path: &str) -> Result<DirListing, SftpError> {
        let s = self.session().await?;
        // 规范化失败（路径不存在、服务器不支持）不是错误：交给 OPENDIR 报出真正的原因。
        let norm = realpath(&s, path).await.unwrap_or_else(|_| path.to_owned());
        let handle = open_dir(&s, &norm).await?;
        Ok(DirListing {
            session: s,
            handle: Some(handle),
            base: norm,
        })
    }

    /// 新建目录（属性交给服务器默认值）。
    pub async fn mkdir(&self, path: &str) -> Result<(), SftpError> {
        let s = self.session().await?;
        mkdir(&s, path).await
    }

    /// 删除文件（不是目录）。
    pub async fn remove_file(&self, path: &str) -> Result<(), SftpError> {
        let s = self.session().await?;
        remove(&s, proto::FXP_REMOVE, path).await
    }

    /// 删除**空**目录。非空目录由服务器拒绝，递归删除是调用方的事。
    pub async fn remove_dir(&self, path: &str) -> Result<(), SftpError> {
        let s = self.session().await?;
        remove(&s, proto::FXP_RMDIR, path).await
    }

    /// 重命名 / 移动。目标已存在时的行为由服务器决定（OpenSSH 拒绝）。
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SftpError> {
        let s = self.session().await?;
        let r = s
            .send(proto::FXP_RENAME, from.len() + to.len() + 8, |b| {
                proto::put_str(b, from.as_bytes());
                proto::put_str(b, to.as_bytes());
            })
            .await?
            .wait()
            .await?;
        expect_ok(&r, SftpOp::Rename, from)
    }

    /// 新建空文件。
    ///
    /// 必须用"已存在则失败"（`CREAT | EXCL`）：名字打错时不能把已有文件清空（ssh.md §8）。
    /// OpenSSH 对 `EXCL` 冲突只回通用的 `FAILURE`，无法与其他失败区分，
    /// 所以失败后再 LSTAT 一次：路径确实存在就改报 `AlreadyExists`。
    pub async fn create_file(&self, path: &str) -> Result<(), SftpError> {
        let s = self.session().await?;
        let handle = open_exclusive(
            &s,
            path,
            proto::OPEN_WRITE | proto::OPEN_CREAT | proto::OPEN_EXCL,
            SftpOp::CreateFile,
        )
        .await?;
        close_handle(&s, &handle, path, SftpOp::CreateFile).await
    }
}

/// 以带 `EXCL` 的标志打开；`FAILURE` 时再 LSTAT 区分"已存在"与其他失败，见 [`SftpClient::create_file`]。
pub(crate) async fn open_exclusive(
    s: &Session,
    path: &str,
    flags: u32,
    op: SftpOp,
) -> Result<Vec<u8>, SftpError> {
    match open_file(s, path, flags, op).await {
        Err(SftpError::Status {
            kind: StatusKind::Failure,
            op,
            path: p,
            code,
            detail,
        }) => {
            let exists = stat(s, proto::FXP_LSTAT, path).await.is_ok();
            Err(SftpError::Status {
                op,
                path: p,
                kind: if exists {
                    StatusKind::AlreadyExists
                } else {
                    StatusKind::Failure
                },
                code,
                detail,
            })
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// 以下是 Session 上的基本请求，传输引擎也直接复用。

/// 把 STATUS 响应转成错误；OK 码返回 `Ok(())`。
pub(crate) fn status_result(r: &Response, op: SftpOp, path: &str) -> Result<(), SftpError> {
    let mut rd = r.reader();
    let code = rd.u32()?;
    if code == proto::FX_OK {
        return Ok(());
    }
    // message 与 lang 缺失时（极简服务器）退化为空串，不因此判协议错误。
    let detail = rd.string().unwrap_or_default();
    Err(SftpError::Status {
        op,
        path: path.to_owned(),
        kind: StatusKind::from_code(code),
        code,
        detail,
    })
}

/// 期望一个 STATUS(OK)。
pub(crate) fn expect_ok(r: &Response, op: SftpOp, path: &str) -> Result<(), SftpError> {
    if r.ty != proto::FXP_STATUS {
        return Err(unexpected(r.ty, proto::FXP_STATUS));
    }
    status_result(r, op, path)
}

/// 响应类型不符时的错误；若响应是 STATUS（错误码）则调用方应先走 [`status_result`]。
pub(crate) fn unexpected(got: u8, want: u8) -> SftpError {
    SftpError::Protocol {
        detail: format!("unexpected response type {got}, expected {want}"),
    }
}

/// 期望一个指定类型的响应；若收到 STATUS 则按错误处理。
pub(crate) fn expect_type(
    r: Response,
    want: u8,
    op: SftpOp,
    path: &str,
) -> Result<Response, SftpError> {
    if r.ty == want {
        return Ok(r);
    }
    if r.ty == proto::FXP_STATUS {
        status_result(&r, op, path)?;
        // OK 状态但调用方要的是数据：协议违规。
    }
    Err(unexpected(r.ty, want))
}

pub(crate) async fn open_file(
    s: &Session,
    path: &str,
    flags: u32,
    op: SftpOp,
) -> Result<Vec<u8>, SftpError> {
    let r = s
        .send(proto::FXP_OPEN, path.len() + 16, |b| {
            proto::put_str(b, path.as_bytes());
            proto::put_u32(b, flags);
            FileAttrs::default().encode(b);
        })
        .await?
        .wait()
        .await?;
    let r = expect_type(r, proto::FXP_HANDLE, op, path)?;
    Ok(r.reader().bytes()?.to_vec())
}

pub(crate) async fn open_dir(s: &Session, path: &str) -> Result<Vec<u8>, SftpError> {
    let r = s
        .send(proto::FXP_OPENDIR, path.len() + 4, |b| {
            proto::put_str(b, path.as_bytes());
        })
        .await?
        .wait()
        .await?;
    let r = expect_type(r, proto::FXP_HANDLE, SftpOp::List, path)?;
    Ok(r.reader().bytes()?.to_vec())
}

pub(crate) async fn close_handle(
    s: &Session,
    handle: &[u8],
    path: &str,
    op: SftpOp,
) -> Result<(), SftpError> {
    let r = s
        .send(proto::FXP_CLOSE, handle.len() + 4, |b| {
            proto::put_str(b, handle);
        })
        .await?
        .wait()
        .await?;
    expect_ok(&r, op, path)
}

pub(crate) async fn realpath(s: &Session, path: &str) -> Result<String, SftpError> {
    let r = s
        .send(proto::FXP_REALPATH, path.len() + 4, |b| {
            proto::put_str(b, path.as_bytes());
        })
        .await?
        .wait()
        .await?;
    let r = expect_type(r, proto::FXP_NAME, SftpOp::Stat, path)?;
    let mut rd = r.reader();
    if rd.u32()? < 1 {
        return Err(SftpError::Protocol {
            detail: "REALPATH returned no name".into(),
        });
    }
    rd.string()
}

pub(crate) fn parse_attrs_reply(r: Response, path: &str) -> Result<FileAttrs, SftpError> {
    let r = expect_type(r, proto::FXP_ATTRS, SftpOp::Stat, path)?;
    r.reader().attrs()
}

/// 发 STAT / LSTAT，不等响应。
pub(crate) async fn send_stat(s: &Session, ty: u8, path: &str) -> Result<Reply, SftpError> {
    s.send(ty, path.len() + 4, |b| proto::put_str(b, path.as_bytes()))
        .await
}

pub(crate) async fn stat(s: &Session, ty: u8, path: &str) -> Result<FileAttrs, SftpError> {
    parse_attrs_reply(send_stat(s, ty, path).await?.wait().await?, path)
}

pub(crate) async fn mkdir(s: &Session, path: &str) -> Result<(), SftpError> {
    let r = s
        .send(proto::FXP_MKDIR, path.len() + 8, |b| {
            proto::put_str(b, path.as_bytes());
            FileAttrs::default().encode(b);
        })
        .await?
        .wait()
        .await?;
    expect_ok(&r, SftpOp::Mkdir, path)
}

pub(crate) async fn remove(s: &Session, ty: u8, path: &str) -> Result<(), SftpError> {
    let r = send_stat(s, ty, path).await?.wait().await?;
    expect_ok(&r, SftpOp::Remove, path)
}

// ---------------------------------------------------------------------------
// 目录列表

/// 流式目录列表：每次 [`DirListing::next_batch`] 发一个 READDIR，拿到一批就返回。
///
/// 大目录的第一批在第一个 READDIR 回来时就能显示，不必等全部读完（ssh.md §8）。
/// 读完（`next_batch` 返回 `None`）或出错时句柄会自动关闭；提前放弃时丢弃本对象也会
/// 在后台关闭句柄，避免服务器侧句柄泄漏。
pub struct DirListing {
    session: Arc<Session>,
    handle: Option<Vec<u8>>,
    /// 规范化后的目录路径，用来拼子项完整路径。
    base: String,
}

/// 解析 NAME 响应，丢掉 `.` 与 `..`。返回 `(文件名, longname, attrs)`。
pub(crate) fn parse_names(resp: &Response) -> Result<Vec<(String, String, FileAttrs)>, SftpError> {
    parse_names_limited(resp, usize::MAX)
}

fn parse_names_limited(
    resp: &Response,
    limit: usize,
) -> Result<Vec<(String, String, FileAttrs)>, SftpError> {
    if resp.ty != proto::FXP_NAME {
        return Err(unexpected(resp.ty, proto::FXP_NAME));
    }
    let mut rd = resp.reader();
    let count = rd.u32()?;
    // count 来自对端：不按它预分配，边解析边增长，包长上限已经限制了总量。
    let mut raw = Vec::new();
    for _ in 0..count {
        if raw.len() == limit {
            break;
        }
        let name = rd.string()?;
        let longname = rd.string()?;
        let attrs = rd.attrs()?;
        if name == "." || name == ".." {
            continue;
        }
        raw.push((name, longname, attrs));
    }
    Ok(raw)
}

/// 读完一个目录的全部项（带 lstat 语义的 attrs），供传输引擎扫描用。
pub(crate) async fn read_dir_raw(
    s: &Session,
    path: &str,
) -> Result<Vec<(String, FileAttrs)>, SftpError> {
    let handle = open_dir(s, path).await?;
    let mut out = Vec::new();
    let res = loop {
        let r = match s
            .send(proto::FXP_READDIR, handle.len() + 4, |b| {
                proto::put_str(b, &handle);
            })
            .await
        {
            Ok(r) => r.wait().await,
            Err(e) => Err(e),
        };
        let r = match r {
            Ok(r) => r,
            Err(e) => break Err(e),
        };
        if r.ty == proto::FXP_STATUS {
            match r.reader().u32() {
                Ok(proto::FX_EOF) => break Ok(()),
                Ok(_) => break status_result(&r, SftpOp::List, path),
                Err(e) => break Err(e),
            }
        }
        match parse_names(&r) {
            Ok(v) => out.extend(v.into_iter().map(|(n, _, a)| (n, a))),
            Err(e) => break Err(e),
        }
    };
    let _ = close_handle(s, &handle, path, SftpOp::List).await;
    res.map(|()| out)
}

/// 拼子项完整路径；目录本身是根（以 `/` 结尾）时不重复分隔符。
pub(crate) fn join_remote(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// 从 `longname`（`ls -l` 风格）解析属主、属组名；解析不了返回 `None`。
///
/// v3 的 attrs 只有数字 uid/gid，longname 是唯一能拿到用户名的地方，但它的格式协议里没有规定，
/// 所以只在"形如 `drwxr-xr-x 2 owner group ...`"时采用，否则调用方回退到数字。
fn owner_group_from_longname(longname: &str) -> Option<(String, String)> {
    let mut it = longname.split_whitespace();
    let mode = it.next()?;
    let well_formed = mode.len() >= 10
        && matches!(
            mode.as_bytes()[0],
            b'-' | b'd' | b'l' | b'c' | b'b' | b'p' | b's'
        );
    if !well_formed {
        return None;
    }
    let _nlink = it.next()?;
    let owner = it.next()?;
    let group = it.next()?;
    // 至少还要有大小等后续字段，避免把"名字"误当成属组。
    it.next()?;
    Some((owner.to_owned(), group.to_owned()))
}

impl DirListing {
    /// 取下一批；读完返回 `Ok(None)`。批内已去掉 `.` 与 `..`，且不会返回空批。
    pub async fn next_batch(&mut self) -> Result<Option<Vec<DirEntry>>, SftpError> {
        self.next_batch_limited(usize::MAX).await
    }

    /// 最多解析指定数量的条目，供补全的大目录读取使用。
    pub async fn next_batch_limited(
        &mut self,
        limit: usize,
    ) -> Result<Option<Vec<DirEntry>>, SftpError> {
        if limit == 0 {
            return Ok(None);
        }
        loop {
            let Some(handle) = self.handle.clone() else {
                return Ok(None);
            };
            let reply = self
                .session
                .send(proto::FXP_READDIR, handle.len() + 4, |b| {
                    proto::put_str(b, &handle);
                })
                .await;
            let resp = match reply {
                Ok(r) => r.wait().await,
                Err(e) => Err(e),
            };
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    self.finish().await;
                    return Err(e);
                }
            };
            if resp.ty == proto::FXP_STATUS {
                let mut rd = resp.reader();
                let code = rd.u32()?;
                self.finish().await;
                if code == proto::FX_EOF {
                    return Ok(None);
                }
                return Err(status_result(&resp, SftpOp::List, &self.base)
                    .err()
                    .unwrap_or(SftpError::Protocol {
                        detail: "unexpected OK status for READDIR".into(),
                    }));
            }
            let parsed = self.parse_names(&resp, limit).await;
            match parsed {
                Ok(batch) if batch.is_empty() => continue,
                Ok(batch) => return Ok(Some(batch)),
                Err(e) => {
                    self.finish().await;
                    return Err(e);
                }
            }
        }
    }

    async fn parse_names(&self, resp: &Response, limit: usize) -> Result<Vec<DirEntry>, SftpError> {
        let raw = parse_names_limited(resp, limit)?;
        // 符号链接要知道"指向的是不是目录"：先把这一批的 STAT 全部发出去，再统一收，
        // 链接多的目录也只花一个往返时间而不是逐个往返。
        let mut stats: Vec<Option<Reply>> = Vec::with_capacity(raw.len());
        for (name, _, attrs) in &raw {
            if attrs.is_symlink() {
                let p = join_remote(&self.base, name);
                stats.push(send_stat(&self.session, proto::FXP_STAT, &p).await.ok());
            } else {
                stats.push(None);
            }
        }
        let mut out = Vec::with_capacity(raw.len());
        for ((name, longname, attrs), st) in raw.into_iter().zip(stats) {
            let is_symlink = attrs.is_symlink();
            let mut is_dir = attrs.is_dir();
            if let Some(reply) = st {
                // 悬空链接 / 无权限：STAT 失败就当"不是目录"，仍作为链接列出。
                if let Ok(r) = reply.wait().await
                    && let Ok(target) = parse_attrs_reply(r, &name)
                {
                    is_dir = target.is_dir();
                }
            }
            let (owner, group) = match owner_group_from_longname(&longname) {
                Some((o, g)) => (Some(o), Some(g)),
                None => (
                    attrs.uid.map(|u| u.to_string()),
                    attrs.gid.map(|g| g.to_string()),
                ),
            };
            out.push(DirEntry {
                path: join_remote(&self.base, &name),
                name,
                is_dir,
                is_symlink,
                size: attrs.size.unwrap_or(0),
                mtime_ms: attrs.mtime.map(|t| i64::from(t) * 1000),
                permissions: attrs.permissions.map(|p| p & 0o7777),
                owner,
                group,
            });
        }
        Ok(out)
    }

    async fn finish(&mut self) {
        if let Some(h) = self.handle.take() {
            // 关闭失败（连接已断）无处可报，也不影响调用方已经拿到的结果。
            let _ = close_handle(&self.session, &h, &self.base, SftpOp::List).await;
        }
    }

    /// 读完剩余全部批次。便于不需要流式显示的调用方（传输引擎的扫描）。
    pub async fn collect_all(mut self) -> Result<Vec<DirEntry>, SftpError> {
        let mut all = Vec::new();
        while let Some(b) = self.next_batch().await? {
            all.extend(b);
        }
        Ok(all)
    }
}

impl Drop for DirListing {
    fn drop(&mut self) {
        let Some(h) = self.handle.take() else { return };
        // Drop 里不能 await：交给运行时后台关闭。没有运行时（进程退出阶段）就算了，连接本身也要断了。
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let s = self.session.clone();
            rt.spawn(async move {
                let _ = close_handle(&s, &h, "", SftpOp::List).await;
            });
        }
    }
}
