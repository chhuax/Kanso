//! 目录传输（ssh.md §9.2、§9.3）：先扫描生成完整计划，再建目录，最后逐个文件传输。
//!
//! 先扫描的原因：进度条要覆盖整棵树，且路径校验必须在写任何文件之前全部完成。
//! 已经传完的文件与已建好的目录在取消 / 失败后**保留**（只清理正在传的那个残缺文件）：
//! 它们是完整的，删掉反而会让用户重试时多做无用功。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use kanso_types::message::Reason;
use kanso_types::transfer::ConflictPolicy;

use super::client::{SftpClient, join_remote, mkdir, read_dir_raw, stat};
use super::proto;
use super::session::Session;
use super::transfer::{
    FileResult, SkipReason, SkippedItem, Stop, Tracker, TransferError, TransferOptions,
    TransferOutcome, TransferReport, download_one, upload_one,
};

/// 目录嵌套深度上限。真实目录树远不到这个深度；限制它是为了挡住恶意服务器
/// 构造的无限深（或循环）目录，否则扫描永不结束。
pub const MAX_DEPTH: usize = 64;

/// 远端文件名是否可以安全地作为**单个**本地路径分量。
///
/// 拒绝：空名、`.`、`..`、含 `/` `\` NUL 的名字，以及按本机平台规则不是"单个普通分量"的名字
/// （例如 Windows 上的 `C:`）。这些名字一旦拼进本地路径就可能写到目标目录之外。
/// `\` 在 Unix 上是合法文件名字符，但它在 Windows 上是分隔符；服务器给的名字要能安全落在任何平台，
/// 所以一律拒绝。
pub fn is_safe_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return false;
    }
    let mut c = Path::new(name).components();
    matches!((c.next(), c.next()), (Some(Component::Normal(n)), None) if n == name)
}

struct PlanFile {
    remote: String,
    rel: PathBuf,
    size: u64,
}

struct DownloadPlan {
    /// 要建的目录（相对目标根），父目录在前。
    dirs: Vec<PathBuf>,
    files: Vec<PlanFile>,
    skipped: Vec<SkippedItem>,
    total: u64,
}

/// 扫描远端目录树（只取元数据）。
async fn scan_remote(s: &Session, root: &str) -> Result<DownloadPlan, TransferError> {
    let attrs = stat(s, proto::FXP_STAT, root).await?;
    if !attrs.is_dir() {
        return Err(TransferError::RemoteNotDir {
            path: root.to_owned(),
        });
    }
    let mut plan = DownloadPlan {
        dirs: Vec::new(),
        files: Vec::new(),
        skipped: Vec::new(),
        total: 0,
    };
    // 显式栈 + 预序：父目录先于子目录进入 `dirs`。
    let mut stack = vec![(root.to_owned(), PathBuf::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let mut entries = match read_dir_raw(s, &dir).await {
            Ok(e) => e,
            // 根目录读不了就是整体失败；子目录读不了只跳过它。
            Err(e) if depth == 0 => return Err(e.into()),
            Err(super::SftpError::Status { .. }) => {
                plan.skipped.push(SkippedItem {
                    path: dir,
                    reason: SkipReason::Unreadable,
                });
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        // 排序让计划（和进度顺序）可复现。
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut seen = HashSet::new();
        let mut subdirs = Vec::new();
        for (name, a) in entries {
            let remote = join_remote(&dir, &name);
            if !is_safe_name(&name) {
                plan.skipped.push(SkippedItem {
                    path: remote,
                    reason: SkipReason::UnsafeName,
                });
                continue;
            }
            if !seen.insert(name.clone()) {
                plan.skipped.push(SkippedItem {
                    path: remote,
                    reason: SkipReason::DuplicateName,
                });
                continue;
            }
            let child_rel = rel.join(&name);
            // 符号链接：只跟随指向普通文件的；指向目录的跳过（可能指回上级形成死循环），悬空的跳过。
            let (kind_attrs, from_symlink) = if a.is_symlink() {
                match stat(s, proto::FXP_STAT, &remote).await {
                    Ok(t) if t.is_dir() => {
                        plan.skipped.push(SkippedItem {
                            path: remote,
                            reason: SkipReason::SymlinkToDir,
                        });
                        continue;
                    }
                    Ok(t) => (t, true),
                    Err(_) => {
                        plan.skipped.push(SkippedItem {
                            path: remote,
                            reason: SkipReason::DanglingSymlink,
                        });
                        continue;
                    }
                }
            } else {
                (a, false)
            };
            if kind_attrs.is_dir() && !from_symlink {
                if depth + 1 >= MAX_DEPTH {
                    plan.skipped.push(SkippedItem {
                        path: remote,
                        reason: SkipReason::TooDeep,
                    });
                    continue;
                }
                plan.dirs.push(child_rel.clone());
                subdirs.push((remote, child_rel, depth + 1));
            } else if kind_attrs.is_regular() {
                let size = kind_attrs.size.unwrap_or(0);
                plan.total = plan
                    .total
                    .checked_add(size)
                    .ok_or(TransferError::SizeOverflow)?;
                plan.files.push(PlanFile {
                    remote,
                    rel: child_rel,
                    size,
                });
            } else {
                plan.skipped.push(SkippedItem {
                    path: remote,
                    reason: SkipReason::NotRegularFile,
                });
            }
        }
        // 栈是后进先出，倒序压入使子目录按名字升序处理；
        // `dirs` 已按"父先于子"记录，与处理顺序无关。
        for d in subdirs.into_iter().rev() {
            stack.push(d);
        }
    }
    Ok(plan)
}

/// 在本地建全部目录并校验每个目标文件路径。返回可传输的文件列表（冲突项已移入 `skipped`）。
///
/// 校验用 `symlink_metadata`（不跟随链接）：目标路径的任何一级若是已有的符号链接，
/// 写入会穿过链接跑到目标目录之外，必须拒绝。这是"检查时"的保护，不防并发修改本地目录的对手。
async fn prepare_local(
    root: &Path,
    plan: &mut DownloadPlan,
    conflict: ConflictPolicy,
) -> Result<Vec<PlanFile>, TransferError> {
    // 根目录是用户选定的，允许它本身是指向目录的链接。
    match tokio::fs::metadata(root).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(TransferError::LocalWrite {
                path: root.display().to_string(),
                reason: Reason::AlreadyExists,
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(root)
                .await
                .map_err(|e| TransferError::LocalWrite {
                    path: root.display().to_string(),
                    reason: Reason::from(&e),
                })?;
        }
        Err(e) => {
            return Err(TransferError::LocalWrite {
                path: root.display().to_string(),
                reason: Reason::from(&e),
            });
        }
    }

    let mut blocked: Vec<PathBuf> = Vec::new();
    for rel in &plan.dirs {
        if blocked.iter().any(|b| rel.starts_with(b)) {
            continue;
        }
        let p = root.join(rel);
        match tokio::fs::symlink_metadata(&p).await {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                // 已存在但不是真正的目录（文件或符号链接）：不覆盖，其下的内容一并跳过。
                plan.skipped.push(SkippedItem {
                    path: p.display().to_string(),
                    reason: SkipReason::LocalConflict,
                });
                blocked.push(rel.clone());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&p)
                    .await
                    .map_err(|e| TransferError::LocalWrite {
                        path: p.display().to_string(),
                        reason: Reason::from(&e),
                    })?;
            }
            Err(e) => {
                return Err(TransferError::LocalWrite {
                    path: p.display().to_string(),
                    reason: Reason::from(&e),
                });
            }
        }
    }

    let mut ok = Vec::new();
    let files = std::mem::take(&mut plan.files);
    for f in files {
        if blocked.iter().any(|b| f.rel.starts_with(b)) {
            continue;
        }
        let p = root.join(&f.rel);
        match tokio::fs::symlink_metadata(&p).await {
            // 目标位置是链接或目录：不写。
            Ok(m) if m.is_symlink() || m.is_dir() => plan.skipped.push(SkippedItem {
                path: p.display().to_string(),
                reason: SkipReason::LocalConflict,
            }),
            // Skip：从计划里剔除，总量与文件数只统计真正要传的文件。
            Ok(_) if conflict == ConflictPolicy::Skip => plan.skipped.push(SkippedItem {
                path: p.display().to_string(),
                reason: SkipReason::AlreadyExists,
            }),
            // Fail：在写任何文件之前就失败，避免传了一半才发现冲突。
            Ok(_) if conflict == ConflictPolicy::Fail => {
                return Err(TransferError::LocalWrite {
                    path: p.display().to_string(),
                    reason: Reason::AlreadyExists,
                });
            }
            Ok(_) => ok.push(f),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ok.push(f),
            Err(e) => {
                return Err(TransferError::LocalWrite {
                    path: p.display().to_string(),
                    reason: Reason::from(&e),
                });
            }
        }
    }
    Ok(ok)
}

fn finish_tree(
    res: Result<(), Stop>,
    mut tr: Tracker,
    skipped: Vec<SkippedItem>,
) -> TransferReport {
    let outcome = match res {
        Ok(()) => TransferOutcome::Completed,
        Err(Stop::Cancelled) => TransferOutcome::Cancelled,
        Err(Stop::Failed(e)) => TransferOutcome::Failed(e),
    };
    tr.force();
    TransferReport {
        outcome,
        bytes: tr.p.bytes,
        files: tr.p.files_done,
        skipped,
    }
}

impl SftpClient {
    /// 下载远端目录 `remote` 的**内容**到本地目录 `local`（不存在则创建，已存在则合并）。
    ///
    /// 流程：扫描 → 建目录 → 校验 → 逐文件传输，规则见模块文档与 ssh.md §9.2：
    /// 跳过 `.` `..`；符号链接只跟随指向普通文件的；文件名不安全的项跳过并记入报告，
    /// 绝不会写出 `local` 之外。总大小溢出报 [`TransferError::SizeOverflow`]。
    pub async fn download_dir(
        &self,
        remote: &str,
        local: &Path,
        opts: &TransferOptions,
    ) -> TransferReport {
        let mut tr = Tracker::new(opts.progress.clone(), None, 0);
        let mut skipped = Vec::new();
        let res = async {
            let s = self.session().await?;
            tr.force();
            let mut plan = scan_remote(&s, remote).await?;
            if opts.cancel.is_cancelled() {
                return Err(Stop::Cancelled);
            }
            let files = prepare_local(local, &mut plan, opts.conflict).await?;
            skipped = std::mem::take(&mut plan.skipped);
            // 计划定稿后总量才确定（冲突项已剔除）。
            tr.p.total = Some(files.iter().map(|f| f.size).sum());
            tr.p.files_total = files.len() as u64;
            tr.force();
            for f in &files {
                if opts.cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let dest = local.join(&f.rel);
                // 计划阶段已剔除已存在的文件；这里的 `Skipped` 只会来自检查之后才出现的同名文件。
                match download_one(&s, &f.remote, &dest, Some(f.size), opts, &mut tr, false).await?
                {
                    FileResult::Done => tr.p.files_done += 1,
                    FileResult::Skipped => {
                        // 跳过的文件不会产生字节，从总量里扣掉，进度才能走到 100%。
                        tr.p.total = tr.p.total.map(|t| t.saturating_sub(f.size));
                        skipped.push(SkippedItem {
                            path: dest.display().to_string(),
                            reason: SkipReason::AlreadyExists,
                        });
                    }
                }
            }
            Ok(())
        }
        .await;
        finish_tree(res, tr, skipped)
    }

    /// 上传本地目录 `local` 的**内容**到远端目录 `remote`（不存在则创建，已存在则合并）。
    ///
    /// 只上传普通文件，符号链接与特殊文件跳过并记入报告。远端同名路径若不是目录则报
    /// [`TransferError::RemoteNotDir`]，不覆盖。
    pub async fn upload_dir(
        &self,
        local: &Path,
        remote: &str,
        opts: &TransferOptions,
    ) -> TransferReport {
        let mut tr = Tracker::new(opts.progress.clone(), None, 0);
        let mut skipped = Vec::new();
        let res = async {
            let s = self.session().await?;
            tr.force();
            let root = local.to_owned();
            let plan = tokio::task::spawn_blocking(move || scan_local(&root))
                .await
                .map_err(|e| TransferError::LocalRead {
                    path: local.display().to_string(),
                    reason: Reason::Other {
                        detail: e.to_string(),
                    },
                })??;
            skipped = plan.skipped;
            tr.p.total = Some(plan.total);
            tr.p.files_total = plan.files.len() as u64;
            tr.force();

            ensure_remote_dir(&s, remote).await?;
            for rel in &plan.dirs {
                if opts.cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                ensure_remote_dir(&s, &remote_join(remote, rel)).await?;
            }
            for f in &plan.files {
                if opts.cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                let target = remote_join(remote, &f.rel);
                match upload_one(&s, &f.local, &target, opts, &mut tr, false).await? {
                    FileResult::Done => tr.p.files_done += 1,
                    FileResult::Skipped => {
                        tr.p.total = tr.p.total.map(|t| t.saturating_sub(f.size));
                        skipped.push(SkippedItem {
                            path: target,
                            reason: SkipReason::AlreadyExists,
                        });
                    }
                }
            }
            Ok(())
        }
        .await;
        finish_tree(res, tr, skipped)
    }
}

/// 确保远端目录存在：已存在的目录合并；同名路径不是目录报错；不存在则创建。
async fn ensure_remote_dir(s: &Session, path: &str) -> Result<(), Stop> {
    match stat(s, proto::FXP_STAT, path).await {
        Ok(a) if a.is_dir() => Ok(()),
        Ok(_) => Err(TransferError::RemoteNotDir {
            path: path.to_owned(),
        }
        .into()),
        Err(e) if e.is_not_found() => Ok(mkdir(s, path).await?),
        Err(e) => Err(e.into()),
    }
}

/// 相对路径（UTF-8 分量，已在扫描时保证）拼到远端根下。
fn remote_join(root: &str, rel: &[String]) -> String {
    rel.iter()
        .fold(root.to_owned(), |acc, c| join_remote(&acc, c))
}

struct UploadFile {
    local: PathBuf,
    rel: Vec<String>,
    size: u64,
}

struct UploadPlan {
    dirs: Vec<Vec<String>>,
    files: Vec<UploadFile>,
    skipped: Vec<SkippedItem>,
    total: u64,
}

/// 扫描本地目录（阻塞 IO，在 `spawn_blocking` 里跑）。
fn scan_local(root: &Path) -> Result<UploadPlan, TransferError> {
    let read_err = |p: &Path, e: &std::io::Error| TransferError::LocalRead {
        path: p.display().to_string(),
        reason: Reason::from(e),
    };
    let md = std::fs::metadata(root).map_err(|e| read_err(root, &e))?;
    if !md.is_dir() {
        return Err(TransferError::LocalRead {
            path: root.display().to_string(),
            reason: Reason::Other {
                detail: "not a directory".into(),
            },
        });
    }
    let mut plan = UploadPlan {
        dirs: Vec::new(),
        files: Vec::new(),
        skipped: Vec::new(),
        total: 0,
    };
    let mut stack = vec![(root.to_owned(), Vec::<String>::new())];
    while let Some((dir, rel)) = stack.pop() {
        let mut entries = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(|e| read_err(&dir, &e))? {
            let e = e.map_err(|e| read_err(&dir, &e))?;
            entries.push(e);
        }
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in entries {
            let path = e.path();
            let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                plan.skipped.push(SkippedItem {
                    path: path.display().to_string(),
                    reason: SkipReason::NonUtf8Name,
                });
                continue;
            };
            // `file_type` 不跟随符号链接：链接既不是 is_file 也不是 is_dir，落入"跳过"。
            let ft = e.file_type().map_err(|e| read_err(&path, &e))?;
            let mut child = rel.clone();
            child.push(name);
            if ft.is_dir() {
                if rel.len() + 1 >= MAX_DEPTH {
                    plan.skipped.push(SkippedItem {
                        path: path.display().to_string(),
                        reason: SkipReason::TooDeep,
                    });
                    continue;
                }
                plan.dirs.push(child.clone());
                subdirs.push((path, child));
            } else if ft.is_file() {
                let size = e.metadata().map_err(|e| read_err(&path, &e))?.len();
                plan.total = plan
                    .total
                    .checked_add(size)
                    .ok_or(TransferError::SizeOverflow)?;
                plan.files.push(UploadFile {
                    local: path,
                    rel: child,
                    size,
                });
            } else {
                plan.skipped.push(SkippedItem {
                    path: path.display().to_string(),
                    reason: SkipReason::NotRegularFile,
                });
            }
        }
        for d in subdirs.into_iter().rev() {
            stack.push(d);
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        for ok in ["a", "file.txt", ".hidden", "a b", "..a", "a..", "中文"] {
            assert!(is_safe_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "/",
            "/etc/passwd",
            "a/b",
            "../x",
            "a/../b",
            "a\\b",
            "..\\x",
            "a\0b",
        ] {
            assert!(!is_safe_name(bad), "{bad:?}");
        }
    }
}
