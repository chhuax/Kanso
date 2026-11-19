//! 本地 Git 改动面板：机器格式解析、净变化、按需差异与写操作。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use kanso_types::Message;

use crate::{CoreError, api::Result};

const MAX_BYTES: usize = 512 * 1024;
const MAX_LINES: usize = 3000;

/// 一条仓库相对路径的改动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitFile {
    /// 仓库相对路径，不作显示转义。
    pub path: String,
    /// 重命名或复制的来源。
    pub original: Option<String>,
    /// 界面状态标记。
    pub status: String,
    /// 已知新增行数；二进制等无法计算时为空。
    pub added: Option<u64>,
    /// 已知删除行数。
    pub deleted: Option<u64>,
    submodule: bool,
}

/// 一次完整摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSummary {
    /// 工作区根目录，支持 worktree 与子模块。
    pub root: String,
    /// 完整分支名或游离提交的七位编号。
    pub branch: String,
    /// 有序的改动列表。
    pub files: Vec<GitFile>,
    /// 已知新增行数之和。
    pub added: u64,
    /// 已知删除行数之和。
    pub deleted: u64,
    unborn: bool,
}

/// 一行正文差异。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitLine {
    /// 正文前缀：空格、加号或减号。
    pub kind: String,
    /// 对应版本中的行号。
    pub number: u64,
    /// 不含前缀的正文。
    pub text: String,
}

/// 展开项的正文与说明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiff {
    /// 最多 3000 行。
    pub lines: Vec<GitLine>,
    /// 二进制、元数据、无净变化或条目消失的说明。
    pub notice: Option<Message>,
    /// 字节或行数达到上限。
    pub truncated: bool,
}

#[derive(Default)]
struct State {
    request: u64,
    summary: Option<GitSummary>,
}

/// 请求代次由 Rust 持有；旧请求不能覆盖摘要或返回旧正文。
#[derive(Default)]
pub struct GitPanel {
    state: Mutex<State>,
}

impl GitPanel {
    /// 作废以前的摘要和差异请求，不进行磁盘操作。
    pub fn begin(&self) -> u64 {
        let mut state = self.state.lock().expect("Git 状态锁未被毒化");
        state.request += 1;
        state.summary = None;
        state.request
    }

    /// UI 在接收异步结果时也通过此方法核对代次。
    pub fn is_current(&self, request: u64) -> bool {
        self.state.lock().expect("Git 状态锁未被毒化").request == request
    }

    /// 读取目录对应仓库；不在仓库或请求已过时返回空。
    pub fn read(&self, request: u64, cwd: &str) -> Result<Option<GitSummary>> {
        if !self.is_current(request) {
            return Ok(None);
        }
        let summary = summary(cwd)?;
        let mut state = self.state.lock().expect("Git 状态锁未被毒化");
        if state.request != request {
            return Ok(None);
        }
        state.summary = summary.clone();
        Ok(summary)
    }

    fn snapshot(&self, request: u64) -> Result<GitSummary> {
        let state = self.state.lock().expect("Git 状态锁未被毒化");
        if state.request == request
            && let Some(summary) = &state.summary
        {
            return Ok(summary.clone());
        }
        Err(CoreError::User(Message::GitSummaryExpired {}))
    }

    /// 只读取用户展开的文件，并重新检查该条目是否仍有改动。
    pub fn diff(&self, request: u64, path: &str) -> Result<Option<GitDiff>> {
        let snapshot = self.snapshot(request)?;
        // 只对这个条目（重命名时连同原路径）重查状态：整仓 status 在大量未跟踪文件时要几百毫秒，
        // 每展开一项都跑一遍会明显卡顿。原路径必须一起给，否则暂存区重命名会被拆成新增。
        let mut pathspec = vec![path];
        if let Some(original) = snapshot
            .files
            .iter()
            .find(|file| file.path == path)
            .and_then(|file| file.original.as_deref())
        {
            pathspec.push(original);
        }
        let current = status_of(&snapshot.root, &pathspec)?;
        let Some(file) = current.0.into_iter().find(|file| file.path == path) else {
            return Ok(Some(notice(Message::GitEntryUnavailable {})));
        };
        let diff = if snapshot.unborn || file.status == "U" {
            new_file_diff(&snapshot.root, &file)?
        } else {
            let mut args = diff_args();
            args.extend(["--unified=3", "HEAD", "--"]);
            if file.status == "R"
                && let Some(original) = &file.original
            {
                args.push(original);
            }
            args.push(&file.path);
            let (bytes, truncated) = bounded(&snapshot.root, &args)?;
            parse_diff(&bytes, truncated)
        };
        Ok(self.is_current(request).then_some(diff))
    }

    /// 暂存全部改动后提交；失败不撤销已经完成的暂存。
    pub fn commit(&self, request: u64, message: &str) -> Result<()> {
        let message = message.trim();
        if message.is_empty() || message.len() > 16 * 1024 {
            return Err(CoreError::User(Message::GitCommitMessageInvalid {}));
        }
        let snapshot = self.snapshot(request)?;
        run(&snapshot.root, &["add", "--all", "--"])?;
        run(&snapshot.root, &["commit", "-m", message])?;
        Ok(())
    }

    /// 丢弃一个条目或全部当前条目；忽略的未跟踪文件不在清理范围。
    pub fn discard(&self, request: u64, path: Option<&str>) -> Result<()> {
        let snapshot = self.snapshot(request)?;
        let (files, _, unborn) = status(&snapshot.root)?;
        let files: Vec<_> = files
            .into_iter()
            .filter(|file| path.is_none_or(|path| file.path == path))
            .collect();
        if path.is_some() && files.is_empty() {
            return Err(CoreError::User(Message::GitEntryUnavailable {}));
        }
        for file in files {
            let mut paths = vec![file.path.as_str()];
            if file.status == "R"
                && let Some(original) = &file.original
            {
                paths.push(original);
            }
            for path in paths {
                let in_head = !unborn
                    && !run(&snapshot.root, &["ls-tree", "-z", "HEAD", "--", path])?.is_empty();
                if in_head {
                    run(
                        &snapshot.root,
                        &[
                            "restore",
                            "--source=HEAD",
                            "--staged",
                            "--worktree",
                            "--",
                            path,
                        ],
                    )?;
                } else {
                    run(
                        &snapshot.root,
                        &["rm", "--cached", "-f", "-r", "--ignore-unmatch", "--", path],
                    )?;
                    run(&snapshot.root, &["clean", "-f", "-d", "--", path])?;
                }
            }
        }
        Ok(())
    }
}

fn failure(detail: impl ToString) -> CoreError {
    CoreError::User(Message::GitFailed {
        detail: detail.to_string(),
    })
}

fn command(cwd: &str, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .args(["--no-pager", "--literal-pathspecs", "-c", "color.ui=false"])
        .args(args);
    // GUI 应用启动控制台子进程会闪一个黑窗；git 只跑几十毫秒，窗口一闪而过。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

fn run(cwd: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = command(cwd, args).output().map_err(failure)?;
    if !output.status.success() {
        return Err(failure(String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(output.stdout)
}

fn text(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(failure)
}

fn summary(cwd: &str) -> Result<Option<GitSummary>> {
    let output = command(cwd, &["rev-parse", "--show-toplevel"])
        .output()
        .map_err(failure)?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        if error.starts_with("fatal: not a git repository") {
            return Ok(None);
        }
        return Err(failure(error.trim()));
    }
    let root = text(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout))?;
    let (mut files, branch, unborn) = status(&root)?;
    let mut stats = BTreeMap::new();
    if !unborn {
        let mut args = diff_args();
        args.extend(["--numstat", "-z", "HEAD", "--"]);
        stats = parse_numstat(&run(&root, &args)?)?;
    }
    for file in &mut files {
        let counts = if unborn || file.status == "U" {
            read_new(&root, file)?
                .filter(|(_, truncated)| !truncated)
                .map(|(bytes, _)| (line_count(&bytes), 0))
        } else if file.submodule || file.status == "!" {
            None
        } else {
            stats.get(&file.path).copied().unwrap_or(Some((0, 0)))
        };
        file.added = counts.map(|counts| counts.0);
        file.deleted = counts.map(|counts| counts.1);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(GitSummary {
        added: files.iter().filter_map(|file| file.added).sum(),
        deleted: files.iter().filter_map(|file| file.deleted).sum(),
        root,
        branch,
        files,
        unborn,
    }))
}

fn diff_args() -> Vec<&'static str> {
    vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
        "--ignore-submodules=none",
    ]
}

fn status(root: &str) -> Result<(Vec<GitFile>, String, bool)> {
    status_of(root, &[])
}

/// `pathspec` 为空时读整个仓库；否则只读这些路径（调用方已开启 `--literal-pathspecs`，不按通配符解释）。
fn status_of(root: &str, pathspec: &[&str]) -> Result<(Vec<GitFile>, String, bool)> {
    let mut args = vec![
        "status",
        "--porcelain=v2",
        "-z",
        "--branch",
        "--untracked-files=all",
    ];
    if !pathspec.is_empty() {
        args.push("--");
        args.extend_from_slice(pathspec);
    }
    parse_status(&run(root, &args)?)
}

fn parse_status(bytes: &[u8]) -> Result<(Vec<GitFile>, String, bool)> {
    let mut records = bytes.split(|byte| *byte == 0);
    let mut files = Vec::new();
    let mut branch = String::new();
    let mut oid = String::new();
    let mut unborn = false;
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(value) = record.strip_prefix(b"# branch.head ") {
            branch = text(value)?;
            continue;
        }
        if let Some(value) = record.strip_prefix(b"# branch.oid ") {
            oid = text(value)?;
            unborn = value == b"(initial)";
            continue;
        }
        let field_count = match record[0] {
            b'1' => 9,
            b'2' => 10,
            b'u' => 11,
            b'?' => 2,
            _ => continue,
        };
        let fields: Vec<_> = record.splitn(field_count, |byte| *byte == b' ').collect();
        if fields.len() != field_count {
            return Err(failure("invalid Git status fields"));
        }
        let path = text(fields.last().ok_or_else(|| failure("invalid Git status"))?)?;
        let original = if record[0] == b'2' {
            Some(text(
                records
                    .next()
                    .ok_or_else(|| failure("missing rename source"))?,
            )?)
        } else {
            None
        };
        let status = match record[0] {
            b'?' => "U".to_string(),
            b'u' => "!".to_string(),
            _ => {
                let xy = fields.get(1).ok_or_else(|| failure("invalid Git status"))?;
                [b'U', b'R', b'C', b'D', b'A', b'T', b'M']
                    .into_iter()
                    .find(|mark| xy.contains(mark))
                    .map(|mark| {
                        if mark == b'U' {
                            '!'.to_string()
                        } else {
                            (mark as char).to_string()
                        }
                    })
                    .ok_or_else(|| failure("unknown Git status"))?
            }
        };
        files.push(GitFile {
            path,
            original,
            status,
            added: None,
            deleted: None,
            submodule: fields.get(2).is_some_and(|field| field.starts_with(b"S")),
        });
    }
    if branch == "(detached)" {
        branch = oid.chars().take(7).collect();
    }
    Ok((files, branch, unborn))
}

type Counts = BTreeMap<String, Option<(u64, u64)>>;

fn parse_numstat(bytes: &[u8]) -> Result<Counts> {
    let mut result = BTreeMap::new();
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next().expect("非空记录有首字段");
        let deleted = fields.next().ok_or_else(|| failure("invalid numstat"))?;
        let mut path = fields
            .next()
            .ok_or_else(|| failure("numstat missing path"))?;
        if path.is_empty() {
            records
                .next()
                .ok_or_else(|| failure("numstat missing original path"))?;
            path = records
                .next()
                .ok_or_else(|| failure("numstat missing new path"))?;
        }
        let counts = if added == b"-" || deleted == b"-" {
            None
        } else {
            Some((
                text(added)?.parse::<u64>().map_err(failure)?,
                text(deleted)?.parse::<u64>().map_err(failure)?,
            ))
        };
        result.insert(text(path)?, counts);
    }
    Ok(result)
}

fn read_new(root: &str, file: &GitFile) -> Result<Option<(Vec<u8>, bool)>> {
    let path = Path::new(root).join(&file.path);
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failure(error)),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(failure)?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(None);
    }
    let truncated = bytes.len() > MAX_BYTES;
    bytes.truncate(MAX_BYTES);
    Ok(Some((bytes, truncated)))
}

fn line_count(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|byte| **byte == b'\n').count() as u64
        + u64::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}

fn notice(message: Message) -> GitDiff {
    GitDiff {
        lines: Vec::new(),
        notice: Some(message),
        truncated: false,
    }
}

fn new_file_diff(root: &str, file: &GitFile) -> Result<GitDiff> {
    let Some((bytes, truncated)) = read_new(root, file)? else {
        let mut diff = notice(Message::GitDiffUnavailable {});
        diff.truncated = Path::new(root)
            .join(&file.path)
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > MAX_BYTES as u64);
        return Ok(diff);
    };
    if bytes.is_empty() {
        return Ok(notice(Message::GitDiffEmptyFile {}));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.split_terminator('\n').enumerate();
    let displayed = lines
        .by_ref()
        .take(MAX_LINES)
        .map(|(index, line)| GitLine {
            kind: "+".into(),
            number: index as u64 + 1,
            text: line.into(),
        })
        .collect();
    Ok(GitDiff {
        lines: displayed,
        notice: None,
        truncated: truncated || lines.next().is_some(),
    })
}

fn bounded(root: &str, args: &[&str]) -> Result<(Vec<u8>, bool)> {
    let mut child = command(root, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(failure)?;
    let mut stderr = child.stderr.take().expect("已配置 stderr 管道");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("已配置 stdout 管道")
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes);
    let truncated = bytes.len() > MAX_BYTES;
    if truncated || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(failure)?;
    let error = reader
        .join()
        .expect("Git 错误读取线程未崩溃")
        .map_err(failure)?;
    read.map_err(failure)?;
    if !truncated && !status.success() {
        return Err(failure(String::from_utf8_lossy(&error).trim()));
    }
    bytes.truncate(MAX_BYTES);
    Ok((bytes, truncated))
}

fn parse_diff(bytes: &[u8], truncated: bool) -> GitDiff {
    let text = String::from_utf8_lossy(bytes);
    let mut diff = GitDiff {
        lines: Vec::new(),
        notice: None,
        truncated,
    };
    let mut old = 0;
    let mut new = 0;
    let mut in_hunk = false;
    for line in text.split_terminator('\n') {
        if line.starts_with("diff --git ") {
            in_hunk = false;
        }
        if line.starts_with("@@ ") {
            let fields: Vec<_> = line.split_whitespace().collect();
            old = fields
                .get(1)
                .and_then(|value| {
                    value
                        .trim_start_matches('-')
                        .split(',')
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap_or(0);
            new = fields
                .get(2)
                .and_then(|value| {
                    value
                        .trim_start_matches('+')
                        .split(',')
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap_or(0);
            in_hunk = true;
            continue;
        }
        if !in_hunk || !line.starts_with([' ', '+', '-']) {
            continue;
        }
        if diff.lines.len() == MAX_LINES {
            diff.truncated = true;
            break;
        }
        let kind = &line[..1];
        let number = if kind == "-" { old } else { new };
        diff.lines.push(GitLine {
            kind: kind.into(),
            number,
            text: line[1..].into(),
        });
        if kind != "+" {
            old += 1;
        }
        if kind != "-" {
            new += 1;
        }
    }
    if diff.lines.is_empty() {
        diff.notice = Some(
            if text.contains("Binary files") || text.contains("GIT binary patch") {
                Message::GitDiffBinary {}
            } else if bytes.is_empty() {
                Message::GitDiffNoContentChange {}
            } else {
                Message::GitDiffMetadataOnly {}
            },
        );
    }
    diff
}
