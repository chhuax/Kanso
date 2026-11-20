//! 单文件的读写与元数据操作（MCP 的 SFTP 补充，spec/mcp.md §3.5）。
//!
//! 与 `transfer` 的区别：这里面向"内存里的一段数据"，不带进度、取消与冲突策略；
//! 与传输引擎共用同一条会话上的请求流水线。

use std::collections::VecDeque;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use kanso_types::fs::DirEntry;
use kanso_types::message::SftpOp;

use super::client::{
    SftpClient, build_entry, close_handle, expect_ok, join_remote, open_exclusive, open_file,
    realpath, remove, stat, status_result,
};
use super::error::{SftpError, StatusKind};
use super::proto::{self, FileAttrs};
use super::session::{Reply, Session};
use super::transfer::DEFAULT_CHUNK;

/// 读写时同时在途的请求数上限。32 KiB × 8 = 256 KiB，远小于单包上限，也不会压垮服务器。
const WINDOW: usize = 8;

/// 创建临时文件时撞名后的最大尝试次数（含第一次）。
const TMP_ATTEMPTS: usize = 3;

/// 临时文件名里原文件名部分的最大字节数，给前缀 `.` 和后缀留出余量，避免超过常见的 255 字节文件名上限。
const TMP_NAME_MAX: usize = 200;

/// 写文件失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteFailure {
    /// 失败原因。
    pub error: SftpError,
    /// 我们创建、但没能删掉的文件（完整远端路径）。调用方应把它告诉用户，由用户决定是否清理；
    /// 清理成功（或根本没创建）时为 `None`。
    pub leftover: Option<String>,
}

impl From<SftpError> for WriteFailure {
    fn from(error: SftpError) -> Self {
        WriteFailure {
            error,
            leftover: None,
        }
    }
}

/// [`SftpClient::overwrite_file`] 的失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverwriteError {
    /// 服务器没有声明 `posix-rename@openssh.com`。发生在任何写入之前，远端没有任何改动；
    /// 刻意不退回截断写入：那会在中途失败时留下半截文件。
    Unsupported,
    /// 其他失败。
    Failed(WriteFailure),
}

impl From<SftpError> for OverwriteError {
    fn from(e: SftpError) -> Self {
        OverwriteError::Failed(e.into())
    }
}

impl SftpClient {
    /// 服务器是否在 SFTP VERSION 回复里声明了扩展 `name`（如 `posix-rename@openssh.com`）。
    ///
    /// 必要时先打开子系统；打不开返回错误。
    pub async fn has_extension(&self, name: &str) -> Result<bool, SftpError> {
        Ok(self.session().await?.has_extension(name))
    }

    /// 查询单个路径，返回与目录列表同构的 [`DirEntry`]。
    ///
    /// `follow_symlinks` 为真用 STAT，否则用 LSTAT（此时 `is_symlink` 才可能为真）。
    /// `name` 取路径最后一段；`path` 是"父目录规范化 + 名字"，与目录列表拼出的路径规则一致
    /// （不能直接对整条路径 REALPATH：对符号链接它会解析到目标，丢掉链接自己的位置）。
    /// 规范化失败时用传入的原路径，与列目录一致。属主 / 属组没有 longname 可用，退回数字 id。
    pub async fn entry(&self, path: &str, follow_symlinks: bool) -> Result<DirEntry, SftpError> {
        let s = self.session().await?;
        let ty = if follow_symlinks {
            proto::FXP_STAT
        } else {
            proto::FXP_LSTAT
        };
        let attrs = stat(&s, ty, path).await?;
        // 链接要看目标是不是目录；悬空链接 / 无权限时 STAT 失败，当作"不是目录"，与目录列表一致。
        let is_dir = if attrs.is_symlink() {
            stat(&s, proto::FXP_STAT, path)
                .await
                .map(|t| t.is_dir())
                .unwrap_or(false)
        } else {
            attrs.is_dir()
        };
        let trimmed = path.trim_end_matches('/');
        let (full, name) = match trimmed.rsplit_once('/') {
            // 根目录（`/`）没有名字：整条路径规范化，名字就用它自己。
            _ if trimmed.is_empty() => {
                let norm = realpath(&s, path).await.unwrap_or_else(|_| path.to_owned());
                (norm.clone(), norm)
            }
            Some((parent, name)) => {
                let parent = if parent.is_empty() { "/" } else { parent };
                let norm = realpath(&s, parent)
                    .await
                    .unwrap_or_else(|_| parent.to_owned());
                (join_remote(&norm, name), name.to_owned())
            }
            // 相对路径、没有父目录部分：规范化 `.` 得到当前目录。
            None => {
                let norm = realpath(&s, ".").await.unwrap_or_else(|_| ".".to_owned());
                (join_remote(&norm, trimmed), trimmed.to_owned())
            }
        };
        Ok(build_entry(full, name, "", &attrs, is_dir))
    }

    /// 读文件的 `[offset, offset + len)`。
    ///
    /// 服务器单次 READ 可能返回得比请求少，这里循环补读直到凑够 `len` 字节或遇到 EOF；
    /// 到 EOF 时返回实际读到的字节（偏移超出文件长度则为空）。单个请求按 [`DEFAULT_CHUNK`] 分块，
    /// 最多 [`WINDOW`] 个在途。`len` 的上限由调用方负责（本函数不设上限，返回值会整块放进内存）。
    /// 句柄在出错路径上同样会关闭。
    pub async fn read_range(
        &self,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, SftpError> {
        let s = self.session().await?;
        let handle = open_file(&s, path, proto::OPEN_READ, SftpOp::Open).await?;
        let res = read_loop(&s, &handle, path, offset, len).await;
        // 读句柄关闭失败不影响已读到的数据。
        let _ = close_handle(&s, &handle, path, SftpOp::Open).await;
        res
    }

    /// 仅新建并写入全部数据：已存在则失败（`StatusKind::AlreadyExists`），原文件不受影响。
    ///
    /// 写入中途失败时会尽量删除刚创建的文件（它是我们建的）；删不掉时路径放在
    /// [`WriteFailure::leftover`] 里，由调用方决定怎么记录。打开阶段失败（含已存在）没有创建任何东西。
    pub async fn write_new(&self, path: &str, data: &[u8]) -> Result<(), WriteFailure> {
        let s = self.session().await?;
        let handle = open_exclusive(
            &s,
            path,
            proto::OPEN_WRITE | proto::OPEN_CREAT | proto::OPEN_EXCL,
            SftpOp::Open,
        )
        .await?;
        let res = write_and_close(&s, &handle, path, data).await;
        match res {
            Ok(()) => Ok(()),
            Err(error) => Err(WriteFailure {
                error,
                leftover: discard(&s, path).await,
            }),
        }
    }

    /// 设置权限位（SETSTAT，只带 permissions；属主、属组、时间都不动）。
    ///
    /// `mode` 只取低 12 位（含 setuid / setgid / sticky），文件类型位由服务器按实际文件决定。
    pub async fn setstat_permissions(&self, path: &str, mode: u32) -> Result<(), SftpError> {
        let s = self.session().await?;
        setstat_permissions(&s, path, mode).await
    }

    /// 用 `posix-rename@openssh.com` 改名，目标已存在时**原子替换**（`rename(2)` 语义）。
    ///
    /// 服务器没声明该扩展时返回 `StatusKind::Unsupported`，不会退回到 v3 的 RENAME（它不覆盖已有目标）。
    pub async fn posix_rename(&self, from: &str, to: &str) -> Result<(), SftpError> {
        let s = self.session().await?;
        posix_rename(&s, from, to).await
    }

    /// 覆盖写 `path`：写同目录临时文件，再用 `posix-rename@openssh.com` 原子替换目标。
    ///
    /// 保证：任何时刻读者看到的要么是旧内容要么是完整新内容；失败时目标文件保持原样。
    ///
    /// 流程与约定：
    /// 1. **先**检查扩展，没有就返回 [`OverwriteError::Unsupported`]，不做任何写入。
    /// 2. `path` 是符号链接时解析到真实路径，替换真实文件，链接本身不变；悬空链接解析失败，报错。
    ///    目标不存在时按新文件处理；目标是目录时直接报错（rename 会覆盖不了目录）。
    /// 3. 临时文件建在**真实目标所在目录**（rename 不能跨文件系统），名为
    ///    `.<原名>.kanso-tmp-<8 位十六进制>`，用 `EXCL` 创建，撞名换后缀重试，最多 3 次。
    /// 4. 目标原本存在时，在写任何数据之前把临时文件的权限位设成原权限（避免写入期间以默认权限暴露内容）；属主、属组不处理（普通用户改不了）。
    /// 5. 创建临时文件之后的任何失败都会尝试删除它；删不掉时路径记在 [`WriteFailure::leftover`]。
    pub async fn overwrite_file(&self, path: &str, data: &[u8]) -> Result<(), OverwriteError> {
        let s = self.session().await?;
        if !s.has_extension(proto::EXT_POSIX_RENAME) {
            return Err(OverwriteError::Unsupported);
        }

        // 解析真实目标与原权限。LSTAT 的"不存在"是正常分支（新文件），其他错误原样返回。
        let (target, old_mode) = match stat(&s, proto::FXP_LSTAT, path).await {
            Ok(a) if a.is_symlink() => {
                // 悬空链接在这里失败，报 realpath 的错误。
                let real = realpath(&s, path).await?;
                let a = stat(&s, proto::FXP_LSTAT, &real).await?;
                (real, a)
            }
            Ok(a) => (path.to_owned(), a),
            Err(e) if e.is_not_found() => (path.to_owned(), FileAttrs::default()),
            Err(e) => return Err(e.into()),
        };
        if old_mode.is_dir() {
            return Err(SftpError::Status {
                op: SftpOp::Open,
                path: target,
                kind: StatusKind::Failure,
                code: proto::FX_FAILURE,
                detail: "target is a directory".into(),
            }
            .into());
        }
        let old_perm = old_mode.permissions.map(|p| p & 0o7777);

        let (tmp, handle) = create_temp(&s, &target).await?;
        let res = write_swap(&s, &handle, &tmp, &target, data, old_perm).await;
        match res {
            Ok(()) => Ok(()),
            Err(error) => Err(OverwriteError::Failed(WriteFailure {
                error,
                leftover: discard(&s, &tmp).await,
            })),
        }
    }
}

/// 改权限、写完数据、改名；`handle` 在本函数内一定被关闭（任何一步失败都会关闭）。
///
/// 权限必须在写任何数据**之前**设置：临时文件创建时是服务器默认权限（常为 0644），
/// 等写完再设会让原本受限（如 0600）的内容在写入期间以更宽的权限暴露。写完后不再重复设置。
async fn write_swap(
    s: &Session,
    handle: &[u8],
    tmp: &str,
    target: &str,
    data: &[u8],
    old_perm: Option<u32>,
) -> Result<(), SftpError> {
    if let Some(mode) = old_perm
        && let Err(e) = setstat_permissions(s, tmp, mode).await
    {
        // 还没写数据，关闭句柄的结果无关紧要，以 setstat 的错误为准。
        let _ = close_handle(s, handle, tmp, SftpOp::Open).await;
        return Err(e);
    }
    write_and_close(s, handle, tmp, data).await?;
    posix_rename(s, tmp, target).await
}

/// 写入全部数据并关闭句柄。写失败时也会关闭句柄（忽略关闭自己的错误），返回写入的错误。
async fn write_and_close(
    s: &Session,
    handle: &[u8],
    path: &str,
    data: &[u8],
) -> Result<(), SftpError> {
    let res = write_all(s, handle, path, data).await;
    let closed = close_handle(s, handle, path, SftpOp::Open).await;
    res.and(closed)
}

/// 流水线写：同时最多 [`WINDOW`] 个在途 WRITE，按序收响应。
///
/// 出错后不再发新请求，并等完已发出的回复再返回：否则后面的清理（删文件）会和仍在途的写请求竞争。
async fn write_all(s: &Session, handle: &[u8], path: &str, data: &[u8]) -> Result<(), SftpError> {
    let mut q: VecDeque<Reply> = VecDeque::new();
    let mut first_err: Option<SftpError> = None;
    let mut offset = 0usize;
    let chunk = DEFAULT_CHUNK as usize;
    loop {
        while first_err.is_none() && q.len() < WINDOW && offset < data.len() {
            let end = (offset + chunk).min(data.len());
            let part = &data[offset..end];
            let off = offset as u64;
            match s
                .send(proto::FXP_WRITE, handle.len() + part.len() + 16, |b| {
                    proto::put_str(b, handle);
                    proto::put_u64(b, off);
                    proto::put_str(b, part);
                })
                .await
            {
                Ok(r) => q.push_back(r),
                Err(e) => first_err = Some(e),
            }
            offset = end;
        }
        let Some(reply) = q.pop_front() else { break };
        let r = match reply.wait().await {
            Ok(r) => r,
            Err(e) => {
                first_err.get_or_insert(e);
                continue;
            }
        };
        if let Err(e) = expect_ok(&r, SftpOp::Open, path) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// 读循环，见 [`SftpClient::read_range`]。
async fn read_loop(
    s: &Session,
    handle: &[u8],
    path: &str,
    offset: u64,
    len: u32,
) -> Result<Vec<u8>, SftpError> {
    let end = offset.saturating_add(u64::from(len));
    // 不按 len 预分配：len 来自调用方，实际大小以服务器返回为准。
    let mut out = Vec::new();
    let mut next = offset;
    let mut q: VecDeque<(u32, Reply)> = VecDeque::new();
    loop {
        while q.len() < WINDOW && next < end {
            let l = (end - next).min(u64::from(DEFAULT_CHUNK)) as u32;
            let off = next;
            let reply = s
                .send(proto::FXP_READ, handle.len() + 16, |b| {
                    proto::put_str(b, handle);
                    proto::put_u64(b, off);
                    proto::put_u32(b, l);
                })
                .await?;
            q.push_back((l, reply));
            next += u64::from(l);
        }
        let Some((asked, reply)) = q.pop_front() else {
            return Ok(out);
        };
        let r = reply.wait().await?;
        if r.ty == proto::FXP_STATUS {
            if r.reader().u32()? == proto::FX_EOF {
                // EOF 之后的所有请求也都是 EOF，丢弃即可（回复到达时登记项已不在，会被忽略）。
                return Ok(out);
            }
            status_result(&r, SftpOp::Open, path)?;
            return Err(SftpError::Protocol {
                detail: "unexpected OK status for READ".into(),
            });
        }
        if r.ty != proto::FXP_DATA {
            return Err(SftpError::Protocol {
                detail: format!("unexpected response type {}, expected DATA", r.ty),
            });
        }
        let data = r.reader().bytes()?;
        if data.len() > asked as usize {
            return Err(SftpError::Protocol {
                detail: "READ returned more data than requested".into(),
            });
        }
        out.extend_from_slice(data);
        if data.is_empty() {
            // 协议要求 DATA 非空；空回复会让补读死循环，按 EOF 处理。
            return Ok(out);
        }
        if (data.len() as u32) < asked {
            // 短读：后面在途请求的偏移都跳过了这段缺口，作废，从缺口起点重新请求。
            q.clear();
            next = offset + out.len() as u64;
        }
    }
}

/// 在 `target` 所在目录创建临时文件，返回 `(临时路径, 句柄)`。
async fn create_temp(s: &Session, target: &str) -> Result<(String, Vec<u8>), SftpError> {
    let (dir, name) = match target.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((dir, name)) => (dir, name),
        // 相对路径：临时文件同样放在相对当前目录的位置。
        None => ("", target),
    };
    let mut cut = name.len().min(TMP_NAME_MAX);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    let name = &name[..cut];
    let mut last = None;
    for _ in 0..TMP_ATTEMPTS {
        let file = format!(".{name}.kanso-tmp-{:08x}", random_u32());
        let tmp = if dir.is_empty() {
            file
        } else {
            join_remote(dir, &file)
        };
        match open_exclusive(
            s,
            &tmp,
            proto::OPEN_WRITE | proto::OPEN_CREAT | proto::OPEN_EXCL,
            SftpOp::Open,
        )
        .await
        {
            Ok(h) => return Ok((tmp, h)),
            // 只有撞名才换后缀重试；其他失败（权限、目录不存在）重试无意义。
            Err(e) if e.is_already_exists() => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or(SftpError::Protocol {
        detail: "no temp file attempt made".into(),
    }))
}

/// 随机 32 位数。`RandomState` 的哈希键在进程内由操作系统随机源播种，每个实例键不同，
/// 所以不需要引入 `rand` / `getrandom`；这里只用来避免与并发写者撞名，撞名还有 `EXCL` 兜底。
fn random_u32() -> u32 {
    RandomState::new().build_hasher().finish() as u32
}

/// 尽力删除我们创建的文件。返回仍然残留的路径：删除成功、或文件已经不存在时为 `None`。
async fn discard(s: &Session, path: &str) -> Option<String> {
    match remove(s, proto::FXP_REMOVE, path).await {
        Ok(()) => None,
        Err(e) if e.is_not_found() => None,
        Err(_) => Some(path.to_owned()),
    }
}

async fn setstat_permissions(s: &Session, path: &str, mode: u32) -> Result<(), SftpError> {
    let attrs = FileAttrs {
        permissions: Some(mode & 0o7777),
        ..FileAttrs::default()
    };
    let r = s
        .send(proto::FXP_SETSTAT, path.len() + 16, |b| {
            proto::put_str(b, path.as_bytes());
            attrs.encode(b);
        })
        .await?
        .wait()
        .await?;
    expect_ok(&r, SftpOp::Stat, path)
}

async fn posix_rename(s: &Session, from: &str, to: &str) -> Result<(), SftpError> {
    if !s.has_extension(proto::EXT_POSIX_RENAME) {
        return Err(SftpError::Status {
            op: SftpOp::Rename,
            path: from.to_owned(),
            kind: StatusKind::Unsupported,
            code: proto::FX_OP_UNSUPPORTED,
            detail: "server lacks posix-rename@openssh.com".into(),
        });
    }
    let r = s
        .send(proto::FXP_EXTENDED, from.len() + to.len() + 48, |b| {
            proto::put_str(b, proto::EXT_POSIX_RENAME.as_bytes());
            proto::put_str(b, from.as_bytes());
            proto::put_str(b, to.as_bytes());
        })
        .await?
        .wait()
        .await?;
    expect_ok(&r, SftpOp::Rename, from)
}
