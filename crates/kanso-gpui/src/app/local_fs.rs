//! 本机文件系统操作（docs/spec/filer.md §3–§5、§8）：列目录（分批）、新建、重命名、删除，
//! 以及把本地条目复制进另一个本地目录（拖入本地 Filer 的行为：复制而非移动、目录合并、
//! 跳过源已在目标目录的项目、拒绝把目录复制进自己的后代）。
//!
//! 全部是同步函数，调用方放在后台线程里跑。错误统一为 [`LocalError`]，带分类以便界面给出可理解的原因。
//! 本文件不依赖窗口系统，三个平台都能编译与测试。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use kanso_types::fs::DirEntry;

use crate::app::filer_text::{t, tf};

/// 错误分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalErrorKind {
    /// 不存在。
    NotFound,
    /// 权限不足。
    PermissionDenied,
    /// 路径不是目录。
    NotADirectory,
    /// 目标已存在。
    AlreadyExists,
    /// 目录非空（只能删除空目录）。
    NotEmpty,
    /// 用户取消。
    Cancelled,
    /// 其他（磁盘不可用等）。
    Other,
}

/// 本地操作错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalError {
    /// 分类。
    pub kind: LocalErrorKind,
    /// 系统给的原文（用于补充说明）。
    pub detail: String,
}

impl LocalError {
    fn new(kind: LocalErrorKind, detail: impl Into<String>) -> LocalError {
        LocalError {
            kind,
            detail: detail.into(),
        }
    }

    /// 给用户看的原因（不含路径；路径由调用方拼在前面）。
    pub fn text(&self) -> String {
        match self.kind {
            LocalErrorKind::NotFound => t("ui-filer-err-not-found"),
            LocalErrorKind::PermissionDenied => t("ui-filer-err-denied"),
            LocalErrorKind::NotADirectory => t("ui-filer-err-not-dir"),
            LocalErrorKind::AlreadyExists => t("ui-filer-err-exists"),
            LocalErrorKind::NotEmpty => t("ui-filer-err-not-empty"),
            LocalErrorKind::Cancelled => t("ui-filer-err-cancelled"),
            LocalErrorKind::Other => self.detail.clone(),
        }
    }
}

impl From<io::Error> for LocalError {
    fn from(e: io::Error) -> LocalError {
        let kind = match e.kind() {
            io::ErrorKind::NotFound => LocalErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => LocalErrorKind::PermissionDenied,
            io::ErrorKind::NotADirectory => LocalErrorKind::NotADirectory,
            io::ErrorKind::AlreadyExists => LocalErrorKind::AlreadyExists,
            io::ErrorKind::DirectoryNotEmpty => LocalErrorKind::NotEmpty,
            _ => LocalErrorKind::Other,
        };
        LocalError::new(kind, e.to_string())
    }
}

type Result<T> = std::result::Result<T, LocalError>;

fn mtime_ms(meta: &fs::Metadata) -> Option<i64> {
    let t = meta.modified().ok()?;
    Some(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    })
}

#[cfg(unix)]
fn permissions(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn permissions(_meta: &fs::Metadata) -> Option<u32> {
    None
}

/// 把一个目录项转成条目。属性读不到时名称照样保留，大小 0 / 修改时间 `None` 表示“未知”
/// （filer.md §4.4：不能因为个别属性取不到就丢掉条目）。
fn entry_of(path: &Path, name: String, file_type: Option<fs::FileType>) -> DirEntry {
    let is_symlink = file_type.is_some_and(|t| t.is_symlink());
    // 指向目录的链接按目录处理（可进入）；悬空链接 `metadata` 失败，保持非目录。
    let followed = if is_symlink {
        fs::metadata(path).ok()
    } else {
        fs::symlink_metadata(path).ok()
    };
    let is_dir = match (&followed, file_type) {
        (Some(m), _) => m.is_dir(),
        (None, Some(t)) => t.is_dir(),
        (None, None) => false,
    };
    DirEntry {
        name,
        path: path.to_string_lossy().into_owned(),
        is_dir,
        is_symlink,
        size: followed
            .as_ref()
            .filter(|m| !m.is_dir())
            .map_or(0, |m| m.len()),
        mtime_ms: followed.as_ref().and_then(mtime_ms),
        permissions: followed.as_ref().and_then(permissions),
        owner: None,
        group: None,
    }
}

/// 分批列目录：每满 `batch` 项调用一次 `sink`；`sink` 返回 `false` 表示调用方已不需要（过期），立刻停止。
/// 目录打不开返回错误；个别条目读不到属性时仍保留名称。
pub fn list_dir_streaming(
    dir: &Path,
    batch: usize,
    mut sink: impl FnMut(Vec<DirEntry>) -> bool,
) -> Result<()> {
    let meta = fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(LocalError::new(
            LocalErrorKind::NotADirectory,
            t("ui-filer-err-not-dir"),
        ));
    }
    let rd = fs::read_dir(dir)?;
    let mut buf = Vec::with_capacity(batch.min(4096));
    for item in rd {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        buf.push(entry_of(&item.path(), name, item.file_type().ok()));
        if buf.len() >= batch && !sink(std::mem::take(&mut buf)) {
            return Ok(());
        }
    }
    if !buf.is_empty() {
        sink(buf);
    }
    Ok(())
}

/// 一次列出全部（测试与小目录用）。
pub fn list_dir(dir: &Path) -> Result<Vec<DirEntry>> {
    let mut all = Vec::new();
    list_dir_streaming(dir, usize::MAX, |b| {
        all.extend(b);
        true
    })?;
    Ok(all)
}

/// Windows 的盘符列表（“此电脑”）：逐个探测 `C:\`…`Z:\`，读不到的（无介质、断开的网络盘）不列出。
/// 其他平台返回空。
pub fn drives() -> Vec<DirEntry> {
    if !cfg!(windows) {
        return Vec::new();
    }
    ('A'..='Z')
        .filter_map(|c| {
            let root = format!("{c}:\\");
            // A / B 是历史软驱盘符，探测会很慢，也几乎没有实际用途。
            if c < 'C' {
                return None;
            }
            fs::metadata(&root).ok()?;
            Some(DirEntry {
                name: tf("ui-filer-drive", &[("letter", &c.to_string())]),
                path: root,
                is_dir: true,
                is_symlink: false,
                size: 0,
                mtime_ms: None,
                permissions: None,
                owner: None,
                group: None,
            })
        })
        .collect()
}

/// 新建目录。
pub fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir(path).map_err(Into::into)
}

/// 新建空文件；已存在则失败，不清空已有内容（filer.md §5.3）。
pub fn create_file(path: &Path) -> Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
        .map_err(Into::into)
}

/// 重命名。目标已存在时拒绝（Windows 的 `rename` 会静默替换文件）；只改大小写的重命名放行。
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        let same_ignoring_case = from.to_string_lossy().to_lowercase()
            == to.to_string_lossy().to_lowercase()
            && from != to;
        if !same_ignoring_case {
            return Err(LocalError::new(
                LocalErrorKind::AlreadyExists,
                t("ui-filer-err-exists"),
            ));
        }
    }
    fs::rename(from, to).map_err(Into::into)
}

/// 删除：文件、空目录或链接本身（不跟随，不删目标）；非空目录保留并报 `NotEmpty`。
pub fn remove(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        // Windows 的目录链接要用 remove_dir。
        return fs::remove_file(path)
            .or_else(|_| fs::remove_dir(path))
            .map_err(Into::into);
    }
    if ft.is_dir() {
        return fs::remove_dir(path).map_err(Into::into);
    }
    fs::remove_file(path).map_err(Into::into)
}

/// 复制的整体进度回调：(已复制字节, 总字节)。
pub type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// 一次复制的结果统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyReport {
    /// 复制了几个文件。
    pub copied: u32,
    /// 因“已经在此目录”而跳过的源项数。
    pub already_there: u32,
    /// 因同名冲突策略为跳过而跳过的文件数。
    pub skipped_existing: u32,
}

/// 同名文件的策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyPolicy {
    /// 目标已存在即失败，返回 `AlreadyExists`（先问用户）。
    Fail,
    /// 覆盖。
    Overwrite,
    /// 跳过已存在的文件。
    Skip,
}

fn path_eq(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// `child` 是不是 `ancestor` 本身或其后代（按路径分量比较，Windows 忽略大小写）。
pub fn is_same_or_descendant(child: &Path, ancestor: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| {
                let s = c.as_os_str().to_string_lossy().into_owned();
                if cfg!(windows) { s.to_lowercase() } else { s }
            })
            .collect()
    };
    let (c, a) = (norm(child), norm(ancestor));
    c.len() >= a.len() && c[..a.len()] == a[..]
}

/// 计算一组源项的总字节数（目录递归，不跟随链接）。
pub fn total_size(sources: &[PathBuf]) -> u64 {
    fn walk(p: &Path) -> u64 {
        let Ok(m) = fs::symlink_metadata(p) else {
            return 0;
        };
        if m.is_dir() {
            fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| walk(&e.path())).sum())
                .unwrap_or(0)
        } else if m.is_file() {
            m.len()
        } else {
            0
        }
    }
    sources.iter().map(|p| walk(p)).sum()
}

/// 单个源项能不能复制到 `dest_dir`；不能时返回给用户的原因。
///
/// - 源已经位于目标目录：跳过并提示“已经在此目录”（返回 `Ok(false)` 由调用方计数）；
/// - 目录不能复制到自己或后代目录里（否则复制过程中会不断发现新生成的副本）。
pub fn check_copy(src: &Path, dest_dir: &Path) -> Result<bool> {
    if src.parent().is_some_and(|p| path_eq(p, dest_dir)) {
        return Ok(false);
    }
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() && is_same_or_descendant(dest_dir, src) {
        return Err(LocalError::new(
            LocalErrorKind::Other,
            t("ui-filer-err-copy-into-self"),
        ));
    }
    Ok(true)
}

/// 把 `sources` 逐项复制进 `dest_dir`（保留源项，目录合并，不跟随符号链接，普通文件保留权限位）。
/// 大文件按固定块流式复制，内存与文件大小无关；每块检查 `cancel`，取消时删除写了一半的文件。
pub fn copy_into(
    sources: &[PathBuf],
    dest_dir: &Path,
    policy: CopyPolicy,
    cancel: &AtomicBool,
    progress: Progress<'_>,
) -> Result<CopyReport> {
    let total = total_size(sources);
    let mut state = CopyState {
        policy,
        cancel,
        done: 0,
        total,
        report: CopyReport::default(),
        progress,
    };
    (state.progress)(0, total);
    for src in sources {
        if state.cancel.load(Ordering::Relaxed) {
            return Err(LocalError::new(
                LocalErrorKind::Cancelled,
                t("ui-filer-err-cancelled"),
            ));
        }
        if !check_copy(src, dest_dir)? {
            state.report.already_there += 1;
            continue;
        }
        let name = src
            .file_name()
            .ok_or_else(|| LocalError::new(LocalErrorKind::Other, t("ui-filer-err-no-name")))?;
        state.copy_item(src, &dest_dir.join(name))?;
    }
    (state.progress)(state.done, total);
    Ok(state.report)
}

struct CopyState<'a> {
    policy: CopyPolicy,
    cancel: &'a AtomicBool,
    done: u64,
    total: u64,
    report: CopyReport,
    progress: Progress<'a>,
}

const CHUNK: usize = 256 * 1024;

impl CopyState<'_> {
    fn copy_item(&mut self, src: &Path, dst: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(src)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            // 本地复制不通过链接写到目录外，也不展开链接：跳过。
            return Ok(());
        }
        if ft.is_dir() {
            match fs::symlink_metadata(dst) {
                Ok(m) if m.is_dir() => {}
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(LocalError::new(
                        LocalErrorKind::Other,
                        t("ui-filer-err-target-link"),
                    ));
                }
                Ok(_) => {
                    return Err(LocalError::new(
                        LocalErrorKind::AlreadyExists,
                        t("ui-filer-err-exists"),
                    ));
                }
                Err(_) => fs::create_dir(dst)?,
            }
            let mut children: Vec<_> = fs::read_dir(src)?.flatten().collect();
            children.sort_by_key(|e| e.file_name());
            for c in children {
                self.copy_item(&c.path(), &dst.join(c.file_name()))?;
            }
            return Ok(());
        }
        if !ft.is_file() {
            return Ok(());
        }
        match fs::symlink_metadata(dst) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Err(LocalError::new(
                        LocalErrorKind::Other,
                        t("ui-filer-err-target-link"),
                    ));
                }
                if m.is_dir() {
                    return Err(LocalError::new(
                        LocalErrorKind::AlreadyExists,
                        t("ui-filer-err-target-dir"),
                    ));
                }
                match self.policy {
                    CopyPolicy::Fail => {
                        return Err(LocalError::new(
                            LocalErrorKind::AlreadyExists,
                            t("ui-filer-err-exists"),
                        ));
                    }
                    CopyPolicy::Skip => {
                        self.report.skipped_existing += 1;
                        self.done += meta.len();
                        (self.progress)(self.done, self.total);
                        return Ok(());
                    }
                    CopyPolicy::Overwrite => {}
                }
            }
            Err(_) => {}
        }
        self.copy_file(src, dst, &meta)
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, meta: &fs::Metadata) -> Result<()> {
        let mut input = fs::File::open(src)?;
        let mut output = fs::File::create(dst)?;
        let mut buf = vec![0u8; CHUNK];
        let result = (|| -> Result<()> {
            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(LocalError::new(
                        LocalErrorKind::Cancelled,
                        t("ui-filer-err-cancelled"),
                    ));
                }
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                output.write_all(&buf[..n])?;
                self.done += n as u64;
                (self.progress)(self.done, self.total);
            }
            output.flush()?;
            Ok(())
        })();
        drop(output);
        if let Err(e) = result {
            // 半成品不当作可用文件。
            let _ = fs::remove_file(dst);
            return Err(e);
        }
        // 保留权限位；失败不影响复制结果。
        let _ = fs::set_permissions(dst, meta.permissions());
        self.report.copied += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "kanso-localfs-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn noop() -> impl FnMut(u64, u64) {
        |_, _| {}
    }

    #[test]
    fn listing_keeps_names_and_reports_errors() {
        let d = tmp("list");
        fs::write(d.join("a.txt"), b"hello").unwrap();
        fs::create_dir(d.join("sub")).unwrap();
        let mut got = list_dir(&d).unwrap();
        got.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "a.txt");
        assert_eq!(got[0].size, 5);
        assert!(got[1].is_dir);
        assert!(got[0].mtime_ms.is_some());

        let missing = list_dir(&d.join("nope")).unwrap_err();
        assert_eq!(missing.kind, LocalErrorKind::NotFound);
        let not_dir = list_dir(&d.join("a.txt")).unwrap_err();
        assert_eq!(not_dir.kind, LocalErrorKind::NotADirectory);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn streaming_batches_and_stops_when_stale() {
        let d = tmp("stream");
        for i in 0..10 {
            fs::write(d.join(format!("f{i}")), b"").unwrap();
        }
        let mut sizes = Vec::new();
        list_dir_streaming(&d, 4, |b| {
            sizes.push(b.len());
            true
        })
        .unwrap();
        assert_eq!(sizes, vec![4, 4, 2]);
        let mut calls = 0;
        list_dir_streaming(&d, 4, |_| {
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn create_never_truncates_and_rename_refuses_clobber() {
        let d = tmp("ops");
        fs::write(d.join("keep"), b"data").unwrap();
        let e = create_file(&d.join("keep")).unwrap_err();
        assert_eq!(e.kind, LocalErrorKind::AlreadyExists);
        assert_eq!(fs::read(d.join("keep")).unwrap(), b"data");
        create_file(&d.join("new")).unwrap();
        let e = rename(&d.join("new"), &d.join("keep")).unwrap_err();
        assert_eq!(e.kind, LocalErrorKind::AlreadyExists);
        rename(&d.join("new"), &d.join("renamed")).unwrap();
        assert!(d.join("renamed").exists());
        mkdir(&d.join("dir")).unwrap();
        assert_eq!(
            mkdir(&d.join("dir")).unwrap_err().kind,
            LocalErrorKind::AlreadyExists
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn remove_refuses_non_empty_directories() {
        let d = tmp("rm");
        fs::create_dir(d.join("full")).unwrap();
        fs::write(d.join("full").join("x"), b"1").unwrap();
        let e = remove(&d.join("full")).unwrap_err();
        assert_eq!(e.kind, LocalErrorKind::NotEmpty);
        assert!(d.join("full").join("x").exists());
        remove(&d.join("full").join("x")).unwrap();
        remove(&d.join("full")).unwrap();
        assert!(!d.join("full").exists());
        assert_eq!(
            remove(&d.join("full")).unwrap_err().kind,
            LocalErrorKind::NotFound
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_link_leaves_the_target() {
        let d = tmp("link");
        fs::create_dir(d.join("target")).unwrap();
        fs::write(d.join("target").join("x"), b"1").unwrap();
        std::os::unix::fs::symlink(d.join("target"), d.join("ln")).unwrap();
        let listed = list_dir(&d).unwrap();
        let ln = listed.iter().find(|e| e.name == "ln").unwrap();
        assert!(ln.is_symlink && ln.is_dir);
        remove(&d.join("ln")).unwrap();
        assert!(d.join("target").join("x").exists());
        // 悬空链接：名称保留，不当作目录。
        std::os::unix::fs::symlink(d.join("gone"), d.join("dangling")).unwrap();
        let listed = list_dir(&d).unwrap();
        let dg = listed.iter().find(|e| e.name == "dangling").unwrap();
        assert!(dg.is_symlink && !dg.is_dir);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn copy_merges_directories_and_keeps_sources() {
        let src = tmp("src");
        let dst = tmp("dst");
        fs::create_dir(src.join("proj")).unwrap();
        fs::write(src.join("proj").join("a"), b"new-a").unwrap();
        fs::write(src.join("proj").join("b"), b"b").unwrap();
        fs::create_dir(dst.join("proj")).unwrap();
        fs::write(dst.join("proj").join("a"), b"old-a").unwrap();
        fs::write(dst.join("proj").join("extra"), b"x").unwrap();

        let cancel = AtomicBool::new(false);
        // 默认策略遇到同名文件失败，且不改动已有内容。
        let e = copy_into(
            &[src.join("proj")],
            &dst,
            CopyPolicy::Fail,
            &cancel,
            &mut noop(),
        )
        .unwrap_err();
        assert_eq!(e.kind, LocalErrorKind::AlreadyExists);
        assert_eq!(fs::read(dst.join("proj").join("a")).unwrap(), b"old-a");

        let r = copy_into(
            &[src.join("proj")],
            &dst,
            CopyPolicy::Skip,
            &cancel,
            &mut noop(),
        )
        .unwrap();
        assert_eq!(r.skipped_existing, 1);
        assert_eq!(fs::read(dst.join("proj").join("a")).unwrap(), b"old-a");
        assert_eq!(fs::read(dst.join("proj").join("b")).unwrap(), b"b");

        let r = copy_into(
            &[src.join("proj")],
            &dst,
            CopyPolicy::Overwrite,
            &cancel,
            &mut noop(),
        )
        .unwrap();
        assert_eq!(r.copied, 2);
        assert_eq!(fs::read(dst.join("proj").join("a")).unwrap(), b"new-a");
        // 接收侧多余内容保留（合并，不是替换），源项保留。
        assert!(dst.join("proj").join("extra").exists());
        assert!(src.join("proj").join("a").exists());
        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dst).unwrap();
    }

    #[test]
    fn copy_skips_items_already_in_the_target_and_rejects_descendants() {
        let d = tmp("self");
        fs::create_dir(d.join("dir")).unwrap();
        fs::write(d.join("f"), b"content").unwrap();
        let cancel = AtomicBool::new(false);
        // 文件已经在目标目录：跳过，源内容不被截断。
        let r = copy_into(
            &[d.join("f")],
            &d,
            CopyPolicy::Overwrite,
            &cancel,
            &mut noop(),
        )
        .unwrap();
        assert_eq!(r.already_there, 1);
        assert_eq!(fs::read(d.join("f")).unwrap(), b"content");
        // 目录复制进自己的后代：拒绝。
        let e = copy_into(
            &[d.join("dir")],
            &d.join("dir"),
            CopyPolicy::Overwrite,
            &cancel,
            &mut noop(),
        )
        .unwrap_err();
        assert!(e.text().contains("子目录"));
        fs::create_dir(d.join("dir").join("inner")).unwrap();
        assert!(
            copy_into(
                &[d.join("dir")],
                &d.join("dir").join("inner"),
                CopyPolicy::Overwrite,
                &cancel,
                &mut noop()
            )
            .is_err()
        );
        assert!(is_same_or_descendant(&d.join("a").join("b"), &d.join("a")));
        assert!(!is_same_or_descendant(&d.join("ab"), &d.join("a")));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn cancelled_copy_leaves_no_partial_file() {
        let src = tmp("csrc");
        let dst = tmp("cdst");
        fs::write(src.join("big"), vec![7u8; CHUNK * 3]).unwrap();
        let cancel = AtomicBool::new(false);
        let mut seen = 0u32;
        let mut cb = |done: u64, _t: u64| {
            if done > 0 {
                seen += 1;
                cancel.store(true, Ordering::Relaxed);
            }
        };
        let e = copy_into(
            &[src.join("big")],
            &dst,
            CopyPolicy::Overwrite,
            &cancel,
            &mut cb,
        )
        .unwrap_err();
        assert_eq!(e.kind, LocalErrorKind::Cancelled);
        assert!(!dst.join("big").exists());
        assert!(seen >= 1);
        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dst).unwrap();
    }
}
