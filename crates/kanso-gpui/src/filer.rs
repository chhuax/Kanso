//! 远程文件列表的纯逻辑：命令行目标解析、分批到达的条目模型（增量排序）、路径与显示格式。
//!
//! 与窗口系统无关，三个平台都跑单元测试；视图在 `src/ui/filer.rs`（只在 Windows 编译）。
//!
//! 排序规则（docs/spec/filer.md §4.1）：目录永远在前；组内按所选列排序，名称忽略大小写，
//! 忽略大小写相同时再按原名称的字符顺序，保证刷新后不跳动。数字作为普通文字比较（非自然排序）。
//! 点击列头切换排序列与方向是原型的扩展（规格 D2 决定首版不做，这里为验证 GPUI 才加）。

use std::cmp::Ordering;

use kanso_types::fs::DirEntry;

/// `--sftp` 参数解析出的连接目标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SftpSpec {
    /// 用户名。
    pub user: String,
    /// 主机。
    pub host: String,
    /// 端口，缺省 22。
    pub port: u16,
    /// 初始目录；`None` 表示远端家目录。
    pub path: Option<String>,
}

impl SftpSpec {
    /// 解析 `user@host[:port][:/path]`，主机可写成 `[::1]`。
    ///
    /// 路径必须以 `/` 开头（Windows OpenSSH 的形式是 `/C:/dir`）；`:` 之后不是纯数字端口时
    /// 视为路径的开头，所以 `user@host:/C:/x` 与 `user@host:2222:/C:/x` 都能正确解析。
    pub fn parse(s: &str) -> Option<SftpSpec> {
        let (user, rest) = s.split_once('@')?;
        if user.is_empty() {
            return None;
        }
        let (host, rest) = if let Some(r) = rest.strip_prefix('[') {
            let (h, r) = r.split_once(']')?;
            (h, r)
        } else {
            match rest.find(':') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            }
        };
        if host.is_empty() {
            return None;
        }
        let mut port = 22u16;
        let mut rest = rest;
        if let Some(r) = rest.strip_prefix(':') {
            // `:端口` 后面要么结束要么紧跟 `:/路径`。
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            let after = &r[digits..];
            if digits > 0 && (after.is_empty() || after.starts_with(':')) {
                port = r[..digits].parse().ok().filter(|p| *p != 0)?;
                rest = after;
            }
        }
        let path = if rest.is_empty() || rest == ":" {
            None
        } else {
            match rest.strip_prefix(':') {
                Some(p) if p.starts_with('/') => Some(p.to_owned()),
                _ => return None,
            }
        };
        Some(SftpSpec {
            user: user.to_owned(),
            host: host.to_owned(),
            port,
            path,
        })
    }
}

/// 排序列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// 名称。
    Name,
    /// 大小。
    Size,
    /// 修改时间。
    Time,
}

/// 排序状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    /// 排序列。
    pub key: SortKey,
    /// 是否降序（目录仍在前）。
    pub desc: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::Name,
            desc: false,
        }
    }
}

impl Sort {
    /// 点击列头：同一列切换方向，换列则升序。
    pub fn toggled(self, key: SortKey) -> Sort {
        if self.key == key {
            Sort {
                key,
                desc: !self.desc,
            }
        } else {
            Sort { key, desc: false }
        }
    }
}

/// 列表中的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// 名称。
    pub name: String,
    /// 完整远端路径。
    pub path: String,
    /// 目录（含指向目录的链接）。
    pub is_dir: bool,
    /// 符号链接。
    pub is_symlink: bool,
    /// 大小。
    pub size: u64,
    /// 修改时间（Unix 毫秒）。
    pub mtime_ms: Option<i64>,
    /// POSIX 权限位（未提供为 `None`）。
    pub permissions: Option<u32>,
    /// 所有者。
    pub owner: Option<String>,
    /// 所属组。
    pub group: Option<String>,
    /// 忽略大小写比较用的名称（到达时算一次，排序不再分配）。
    fold: String,
}

impl From<DirEntry> for Row {
    fn from(e: DirEntry) -> Row {
        Row {
            fold: e.name.to_lowercase(),
            name: e.name,
            path: e.path,
            is_dir: e.is_dir,
            is_symlink: e.is_symlink,
            size: e.size,
            mtime_ms: e.mtime_ms,
            permissions: e.permissions,
            owner: e.owner,
            group: e.group,
        }
    }
}

impl Row {
    /// 大小是否已知。条目类型没有“未知大小”，约定：普通文件既无修改时间、大小又是 0，
    /// 视为属性读取失败（filer.md §4.1：读不到的元数据不显示为真实的零字节文件）。
    pub fn size_known(&self) -> bool {
        self.is_dir || self.size > 0 || self.mtime_ms.is_some()
    }
}

fn name_order(a: &Row, b: &Row) -> Ordering {
    a.fold.cmp(&b.fold).then_with(|| a.name.cmp(&b.name))
}

fn compare(a: &Row, b: &Row, sort: Sort) -> Ordering {
    // 目录永远在前，不受方向影响。
    let group = b.is_dir.cmp(&a.is_dir);
    if group != Ordering::Equal {
        return group;
    }
    let key = match sort.key {
        SortKey::Name => name_order(a, b),
        // 目录没有有意义的大小：目录之间按名称。
        SortKey::Size if a.is_dir => return name_order(a, b),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Time => a.mtime_ms.cmp(&b.mtime_ms),
    };
    let key = if sort.desc { key.reverse() } else { key };
    // 并列时固定按名称升序，刷新不跳动。
    key.then_with(|| name_order(a, b))
}

/// 目录内容模型：条目按到达顺序存放，另维护一份已排序的下标，分批到达时做归并而不是整体重排。
#[derive(Clone, Debug, Default)]
pub struct DirModel {
    rows: Vec<Row>,
    /// 已排序的行下标（可见顺序 → 到达序号）。
    order: Vec<u32>,
    sort: Sort,
    dirs: usize,
    links: usize,
    /// 隐藏以点开头的名称（filer.md D2：默认显示，所以默认 `false`）。
    hide_hidden: bool,
    /// `hide_hidden` 为真时的可见子集（同样按排序顺序）。
    vis: Vec<u32>,
}

impl DirModel {
    /// 空模型。
    pub fn new(sort: Sort) -> DirModel {
        DirModel {
            sort,
            ..DirModel::default()
        }
    }

    /// 清空（进入新目录）。
    pub fn clear(&mut self) {
        self.rows.clear();
        self.order.clear();
        self.vis.clear();
        self.dirs = 0;
        self.links = 0;
    }

    /// 并入一批条目：只排新批次，再与已有顺序线性归并（O(n + k log k)）。
    pub fn push_batch(&mut self, batch: Vec<DirEntry>) {
        let base = self.rows.len() as u32;
        let sort = self.sort;
        for e in batch {
            let row = Row::from(e);
            self.dirs += usize::from(row.is_dir);
            self.links += usize::from(row.is_symlink);
            self.rows.push(row);
        }
        let mut fresh: Vec<u32> = (base..self.rows.len() as u32).collect();
        let rows = &self.rows;
        fresh.sort_by(|&a, &b| compare(&rows[a as usize], &rows[b as usize], sort));
        if self.order.is_empty() {
            self.order = fresh;
            self.rebuild_vis();
            return;
        }
        let mut merged = Vec::with_capacity(self.order.len() + fresh.len());
        let (mut i, mut j) = (0, 0);
        while i < self.order.len() && j < fresh.len() {
            let (a, b) = (self.order[i], fresh[j]);
            if compare(&rows[a as usize], &rows[b as usize], sort) != Ordering::Greater {
                merged.push(a);
                i += 1;
            } else {
                merged.push(b);
                j += 1;
            }
        }
        merged.extend_from_slice(&self.order[i..]);
        merged.extend_from_slice(&fresh[j..]);
        self.order = merged;
        self.rebuild_vis();
    }

    fn rebuild_vis(&mut self) {
        if !self.hide_hidden {
            return;
        }
        let rows = &self.rows;
        self.vis = self
            .order
            .iter()
            .copied()
            .filter(|&id| !crate::app::fspath::is_hidden_name(&rows[id as usize].name))
            .collect();
    }

    /// 是否隐藏以点开头的条目。
    pub fn set_hide_hidden(&mut self, hide: bool) {
        self.hide_hidden = hide;
        self.vis.clear();
        self.rebuild_vis();
    }

    /// 当前是否在隐藏以点开头的条目。
    pub fn hide_hidden(&self) -> bool {
        self.hide_hidden
    }

    /// 可见条目数（应用了隐藏过滤）。
    pub fn visible_len(&self) -> usize {
        if self.hide_hidden {
            self.vis.len()
        } else {
            self.order.len()
        }
    }

    /// 可见顺序第 `pos` 行的稳定编号和内容（应用了隐藏过滤）。
    pub fn visible_at(&self, pos: usize) -> Option<(u32, &Row)> {
        let id = *if self.hide_hidden {
            self.vis.get(pos)?
        } else {
            self.order.get(pos)?
        };
        Some((id, &self.rows[id as usize]))
    }

    /// 稳定编号当前的可见位置（应用了隐藏过滤）。
    pub fn visible_position_of(&self, id: u32) -> Option<usize> {
        if self.hide_hidden {
            self.vis.iter().position(|&x| x == id)
        } else {
            self.order.iter().position(|&x| x == id)
        }
    }

    /// 切换排序并整体重排。
    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        let rows = &self.rows;
        self.order
            .sort_by(|&a, &b| compare(&rows[a as usize], &rows[b as usize], sort));
        self.rebuild_vis();
    }

    /// 当前排序。
    pub fn sort(&self) -> Sort {
        self.sort
    }

    /// 条目总数。
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 目录数。
    pub fn dir_count(&self) -> usize {
        self.dirs
    }

    /// 符号链接数。
    pub fn link_count(&self) -> usize {
        self.links
    }

    /// 可见顺序第 `pos` 行的稳定编号（到达序号）和内容。
    pub fn at(&self, pos: usize) -> Option<(u32, &Row)> {
        let id = *self.order.get(pos)?;
        Some((id, &self.rows[id as usize]))
    }

    /// 可见顺序是否满足当前排序（自检用）。
    pub fn is_sorted(&self) -> bool {
        self.order.windows(2).all(|w| {
            compare(
                &self.rows[w[0] as usize],
                &self.rows[w[1] as usize],
                self.sort,
            ) != Ordering::Greater
        })
    }

    /// 稳定编号对应的行。
    pub fn row(&self, id: u32) -> Option<&Row> {
        self.rows.get(id as usize)
    }

    /// 稳定编号当前的可见位置（线性查找，只在键盘导航时用）。
    pub fn position_of(&self, id: u32) -> Option<usize> {
        self.order.iter().position(|&x| x == id)
    }
}

/// 上一级目录。远端根停在 `/`；Windows OpenSSH 的盘符 `/C:` 的上一级是 `/`，`/C:/x` 的上一级是 `/C:/`。
pub fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None | Some(0) => "/".to_owned(),
        Some(i) => {
            let parent = &trimmed[..i];
            if is_drive_component(&parent[parent.rfind('/').map_or(0, |p| p + 1)..]) {
                format!("{parent}/")
            } else {
                parent.to_owned()
            }
        }
    }
}

fn is_drive_component(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// 易读大小：1024 进制，保留一位小数（B 不带小数）。
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// Unix 毫秒 → `YYYY-MM-DD HH:MM`（按给定的时区偏移分钟数，东为正）。
pub fn format_mtime(ms: i64, tz_offset_min: i32) -> String {
    let secs = ms.div_euclid(1000) + i64::from(tz_offset_min) * 60;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// 自 1970-01-01 的天数 → 公历年月日（Howard Hinnant 的公开算法）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 帧间隔统计（毫秒）。
#[derive(Clone, Debug, PartialEq)]
pub struct FrameStats {
    /// 样本数。
    pub count: usize,
    /// 中位数。
    pub p50: f64,
    /// 95 分位。
    pub p95: f64,
    /// 99 分位。
    pub p99: f64,
    /// 最大值。
    pub max: f64,
    /// 超过 `slow_ms` 的帧数。
    pub slow: usize,
}

/// 统计一组帧间隔；空输入返回 `None`。
pub fn frame_stats(samples: &[f64], slow_ms: f64) -> Option<FrameStats> {
    if samples.is_empty() {
        return None;
    }
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Some(FrameStats {
        count: v.len(),
        p50: pct(0.50),
        p95: pct(0.95),
        p99: pct(0.99),
        max: v[v.len() - 1],
        slow: v.iter().filter(|x| **x > slow_ms).count(),
    })
}

/// 一次目录读取的结束方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListEnd {
    /// 正常列完。
    Done,
    /// 读取出错（附错误文字）。
    Failed(String),
}

/// 读取线程与界面之间的共享缓冲：读取线程不停地把到达的批次放进来，
/// 界面每次醒来一次取走这段时间累计的全部批次，再一次性并入模型、只重绘一次。
///
/// 读取速度因此不再受重绘帧率约束（软渲染下一帧 100 ms 以上，逐批重绘会把列表拖慢几十倍）。
/// 缓冲带着请求代数：界面已切到新请求时，旧缓冲里的内容在 [`BatchBuffer::take`] 时被丢弃。
pub struct BatchBuffer {
    generation: u64,
    pending: Vec<DirEntry>,
    batches: usize,
    end: Option<ListEnd>,
    /// 已经通知过界面、界面还没来取：此时再到的批次不必重复唤醒。
    wake_pending: bool,
    cancelled: bool,
}

/// [`BatchBuffer::take`] 取走的内容。
#[derive(Debug, PartialEq, Eq)]
pub struct Drained {
    /// 累计的全部条目（按到达顺序）。
    pub entries: Vec<DirEntry>,
    /// 这些条目来自几批。
    pub batches: usize,
    /// 读取是否已结束；`Some` 时这是最后一次取。
    pub end: Option<ListEnd>,
}

impl BatchBuffer {
    /// 新建属于某个请求代数的缓冲。
    pub fn new(generation: u64) -> BatchBuffer {
        BatchBuffer {
            generation,
            pending: Vec::new(),
            batches: 0,
            end: None,
            wake_pending: false,
            cancelled: false,
        }
    }

    /// 放入一批；返回 `true` 表示调用方需要唤醒界面（之前没有未处理的唤醒）。
    pub fn push(&mut self, batch: Vec<DirEntry>) -> bool {
        if self.cancelled {
            return false;
        }
        self.pending.extend(batch);
        self.batches += 1;
        self.arm_wake()
    }

    /// 标记读取结束；返回值含义同 [`BatchBuffer::push`]。
    pub fn finish(&mut self, end: ListEnd) -> bool {
        if self.cancelled {
            return false;
        }
        self.end = Some(end);
        self.arm_wake()
    }

    fn arm_wake(&mut self) -> bool {
        !std::mem::replace(&mut self.wake_pending, true)
    }

    /// 取走累计内容。`current_generation` 与缓冲的代数不一致（请求已过期）时丢弃内容、取消缓冲并返回 `None`。
    pub fn take(&mut self, current_generation: u64) -> Option<Drained> {
        self.wake_pending = false;
        if self.cancelled || current_generation != self.generation {
            self.cancel();
            return None;
        }
        Some(Drained {
            entries: std::mem::take(&mut self.pending),
            batches: std::mem::take(&mut self.batches),
            end: self.end.clone(),
        })
    }

    /// 取消：清空内容，之后的放入一律忽略。
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.pending = Vec::new();
        self.batches = 0;
    }

    /// 是否已取消（读取线程据此提前退出）。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dir: bool, size: u64, mtime: Option<i64>) -> DirEntry {
        DirEntry {
            name: name.into(),
            path: format!("/x/{name}"),
            is_dir: dir,
            is_symlink: false,
            size,
            mtime_ms: mtime,
            permissions: None,
            owner: None,
            group: None,
        }
    }

    fn names(m: &DirModel) -> Vec<String> {
        (0..m.len())
            .map(|i| m.at(i).unwrap().1.name.clone())
            .collect()
    }

    #[test]
    fn parse_spec_variants() {
        let s = SftpSpec::parse("bob@host:2222:/C:/data").unwrap();
        assert_eq!(
            (s.user.as_str(), s.host.as_str(), s.port, s.path.as_deref()),
            ("bob", "host", 2222, Some("/C:/data"))
        );
        let s = SftpSpec::parse("bob@host:/C:/data").unwrap();
        assert_eq!((s.port, s.path.as_deref()), (22, Some("/C:/data")));
        let s = SftpSpec::parse("bob@host:2200").unwrap();
        assert_eq!((s.port, s.path), (2200, None));
        let s = SftpSpec::parse("bob@host").unwrap();
        assert_eq!((s.host.as_str(), s.port, s.path), ("host", 22, None));
        let s = SftpSpec::parse("bob@[::1]:22:/tmp").unwrap();
        assert_eq!(
            (s.host.as_str(), s.port, s.path.as_deref()),
            ("::1", 22, Some("/tmp"))
        );
        let s = SftpSpec::parse("bob@host:").unwrap();
        assert_eq!(s.path, None);
    }

    #[test]
    fn parse_spec_rejects_garbage() {
        assert_eq!(SftpSpec::parse("host:/x"), None);
        assert_eq!(SftpSpec::parse("@host"), None);
        assert_eq!(SftpSpec::parse("bob@"), None);
        assert_eq!(SftpSpec::parse("bob@host:0:/x"), None);
        assert_eq!(SftpSpec::parse("bob@host:99999:/x"), None);
        assert_eq!(SftpSpec::parse("bob@host:relative"), None);
    }

    #[test]
    fn dirs_first_then_case_insensitive_name() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![
            entry("b.txt", false, 1, None),
            entry("Zdir", true, 0, None),
            entry("A.txt", false, 1, None),
            entry("adir", true, 0, None),
        ]);
        assert_eq!(names(&m), ["adir", "Zdir", "A.txt", "b.txt"]);
        assert_eq!((m.dir_count(), m.len()), (2, 4));
    }

    #[test]
    fn case_ties_are_stable_by_original_name() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![entry("a", false, 0, None), entry("A", false, 0, None)]);
        assert_eq!(names(&m), ["A", "a"]);
    }

    #[test]
    fn batches_merge_into_sorted_order() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![entry("c", false, 0, None), entry("e", false, 0, None)]);
        m.push_batch(vec![entry("d", false, 0, None), entry("a", true, 0, None)]);
        m.push_batch(vec![entry("b", false, 0, None), entry("f", false, 0, None)]);
        assert_eq!(names(&m), ["a", "b", "c", "d", "e", "f"]);
        // 与一次性排序结果一致。
        let mut once = DirModel::new(Sort::default());
        once.push_batch(vec![
            entry("c", false, 0, None),
            entry("e", false, 0, None),
            entry("d", false, 0, None),
            entry("a", true, 0, None),
            entry("b", false, 0, None),
            entry("f", false, 0, None),
        ]);
        assert_eq!(names(&m), names(&once));
    }

    #[test]
    fn sort_by_size_and_time_keeps_dirs_first() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![
            entry("big", false, 900, Some(10)),
            entry("small", false, 5, Some(30)),
            entry("mid", false, 50, None),
            entry("d2", true, 7, Some(1)),
            entry("d1", true, 99, Some(2)),
        ]);
        m.set_sort(Sort::default().toggled(SortKey::Size));
        assert_eq!(names(&m), ["d1", "d2", "small", "mid", "big"]);
        m.set_sort(m.sort().toggled(SortKey::Size)); // 降序
        assert_eq!(names(&m), ["d1", "d2", "big", "mid", "small"]);
        m.set_sort(Sort::default().toggled(SortKey::Time));
        // 目录按时间：d2(1) < d1(2)；文件：无时间最小。
        assert_eq!(names(&m), ["d2", "d1", "mid", "big", "small"]);
        m.set_sort(m.sort().toggled(SortKey::Time));
        assert_eq!(names(&m), ["d1", "d2", "small", "big", "mid"]);
    }

    #[test]
    fn push_after_resort_uses_new_order() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![entry("a", false, 1, None), entry("b", false, 3, None)]);
        m.set_sort(Sort {
            key: SortKey::Size,
            desc: true,
        });
        m.push_batch(vec![entry("c", false, 2, None)]);
        assert_eq!(names(&m), ["b", "c", "a"]);
    }

    #[test]
    fn stable_ids_and_positions() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![entry("b", false, 0, None)]);
        let id = m.at(0).unwrap().0;
        m.push_batch(vec![entry("a", false, 0, None)]);
        assert_eq!(
            m.position_of(id),
            Some(1),
            "新条目插到前面后位置后移，编号不变"
        );
        assert_eq!(m.row(id).unwrap().name, "b");
    }

    #[test]
    fn parent_paths() {
        assert_eq!(parent_path("/home/u/x"), "/home/u");
        assert_eq!(parent_path("/home/u/x/"), "/home/u");
        assert_eq!(parent_path("/home"), "/");
        assert_eq!(parent_path("/"), "/");
        assert_eq!(parent_path("/C:/kanso/big"), "/C:/kanso");
        assert_eq!(parent_path("/C:/kanso"), "/C:/");
        assert_eq!(parent_path("/C:/"), "/");
        assert_eq!(parent_path("/C:"), "/");
    }

    #[test]
    fn size_formatting() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(u64::MAX), "16384.0 PB");
    }

    #[test]
    fn mtime_formatting() {
        assert_eq!(format_mtime(0, 0), "1970-01-01 00:00");
        assert_eq!(format_mtime(1_700_000_000_000, 0), "2023-11-14 22:13");
        assert_eq!(format_mtime(1_700_000_000_000, 480), "2023-11-15 06:13");
        assert_eq!(format_mtime(-1000, 0), "1969-12-31 23:59");
        // 闰日。
        assert_eq!(format_mtime(1_709_164_800_000, 0), "2024-02-29 00:00");
    }

    #[test]
    fn frame_stat_percentiles() {
        assert_eq!(frame_stats(&[], 20.0), None);
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = frame_stats(&samples, 90.0).unwrap();
        assert_eq!((s.count, s.max, s.slow), (100, 100.0, 10));
        assert_eq!(s.p50, 51.0);
        assert_eq!(s.p95, 95.0);
    }

    #[test]
    fn buffer_wakes_once_until_taken() {
        let mut b = BatchBuffer::new(1);
        assert!(b.push(vec![entry("a", false, 1, None)]));
        assert!(!b.push(vec![entry("b", false, 1, None)]));
        assert!(!b.push(vec![entry("c", false, 1, None)]));
        let d = b.take(1).unwrap();
        assert_eq!(d.batches, 3);
        assert_eq!(d.entries.len(), 3);
        assert_eq!(d.end, None);
        // 取走后下一批要重新唤醒；空取也不出错
        assert!(b.push(vec![entry("d", false, 1, None)]));
        assert_eq!(b.take(1).unwrap().entries.len(), 1);
        assert_eq!(b.take(1).unwrap().entries.len(), 0);
    }

    #[test]
    fn buffer_merges_accumulated_batches_into_model_once() {
        let mut b = BatchBuffer::new(3);
        b.push(vec![entry("z", false, 1, None), entry("d1", true, 0, None)]);
        b.push(vec![entry("a", false, 1, None)]);
        b.finish(ListEnd::Done);
        let d = b.take(3).unwrap();
        assert_eq!(d.end, Some(ListEnd::Done));
        let mut m = DirModel::new(Sort::default());
        m.push_batch(d.entries);
        assert_eq!(names(&m), ["d1", "a", "z"]);
    }

    #[test]
    fn buffer_stale_generation_is_dropped_and_cancelled() {
        let mut b = BatchBuffer::new(1);
        b.push(vec![entry("a", false, 1, None)]);
        assert!(b.take(2).is_none());
        assert!(b.is_cancelled());
        // 取消后读取线程再放入的内容被忽略，也不再唤醒
        assert!(!b.push(vec![entry("b", false, 1, None)]));
        assert!(!b.finish(ListEnd::Done));
        assert!(b.take(1).is_none());
    }

    #[test]
    fn buffer_reports_error_after_partial_data() {
        let mut b = BatchBuffer::new(1);
        b.push(vec![entry("a", false, 1, None)]);
        assert!(!b.finish(ListEnd::Failed("断开".into())));
        let d = b.take(1).unwrap();
        assert_eq!(d.entries.len(), 1);
        assert_eq!(d.end, Some(ListEnd::Failed("断开".into())));
    }

    #[test]
    fn buffer_finish_alone_wakes() {
        let mut b = BatchBuffer::new(1);
        assert!(b.finish(ListEnd::Done));
        let d = b.take(1).unwrap();
        assert!(d.entries.is_empty());
        assert_eq!(d.end, Some(ListEnd::Done));
    }

    #[test]
    fn hidden_filter_is_off_by_default_and_keeps_order() {
        let mut m = DirModel::new(Sort::default());
        m.push_batch(vec![
            entry(".git", true, 0, Some(1)),
            entry("src", true, 0, Some(1)),
            entry(".env", false, 1, Some(1)),
            entry("a.txt", false, 1, Some(1)),
        ]);
        assert_eq!(m.visible_len(), 4);
        m.set_hide_hidden(true);
        assert_eq!(m.visible_len(), 2);
        let names: Vec<_> = (0..m.visible_len())
            .map(|i| m.visible_at(i).unwrap().1.name.clone())
            .collect();
        assert_eq!(names, vec!["src", "a.txt"]);
        // 之后到达的批次同样被过滤。
        m.push_batch(vec![entry(".cache", true, 0, Some(1)), entry("b", true, 0, Some(1))]);
        assert_eq!(m.visible_len(), 3);
        m.set_hide_hidden(false);
        assert_eq!(m.visible_len(), 6);
    }

    #[test]
    fn unknown_size_is_not_a_real_zero_byte_file() {
        let m = Row::from(entry("x", false, 0, None));
        assert!(!m.size_known());
        assert!(Row::from(entry("x", false, 0, Some(5))).size_known());
        assert!(Row::from(entry("d", true, 0, None)).size_known());
    }
}
