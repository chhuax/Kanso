//! 客户端所在标签的识别（spec/mcp.md §2.5）：控制终端 + 进程祖先链。
//!
//! 对端自报的 tty 与对端进程号都只用于**限制**该客户端自己（拒绝它写自己所在的标签），
//! 伪造它们只会让这个客户端多被拒几个标签，得不到任何额外权限，所以不需要把它们当作可信输入。

use std::cell::OnceCell;

use kanso_core::Core;
use kanso_core::mcp::McpSessionStatus;
use kanso_types::SessionId;

use super::AncestorsFn;

/// 祖先链最多走这么多层：防止异常的父子关系（pid 复用、读取竞态）导致死循环。
const MAX_DEPTH: usize = 64;

/// 一个连接上的客户端身份线索。握手时只记下"原料"（上报的终端、对端 pid），**不缓存祖先链**：
/// pid 会被复用，连接存活期间原先的祖先进程可能已退出、pid 被无关进程占用，缓存的链会让后续判定
/// 命中错误的标签（或漏掉真正的标签）。每次判定都通过 [`ClientIdentity::probe`] 从对端 pid 现算。
#[derive(Clone)]
pub(super) struct ClientIdentity {
    /// 握手上报的控制终端设备路径，没有或为空串时为 `None`。仅在没有 `tty_dev` 的旧转发程序下使用。
    tty: Option<String>,
    /// 握手上报的控制终端设备号（`st_rdev`）；比对以它为准（见 [`ClientIdentity::tty_matches`]）。
    tty_dev: Option<u64>,
    /// 对端 pid（连接建立时由本机通道取得，不是对端自报）。没有时只能靠控制终端识别。
    pid: Option<u32>,
    /// 读进程祖先链的函数（测试可替换）；同步系统调用，只能在阻塞线程池里调用。
    read_ancestors: AncestorsFn,
}

impl ClientIdentity {
    pub(super) fn new(
        tty: Option<String>,
        tty_dev: Option<u64>,
        pid: Option<u32>,
        read_ancestors: AncestorsFn,
    ) -> ClientIdentity {
        ClientIdentity {
            tty: tty.filter(|t| !t.is_empty()),
            tty_dev,
            pid,
            read_ancestors,
        }
    }

    /// 没有任何线索的身份（测试用）。
    #[cfg(test)]
    pub(super) fn unknown() -> ClientIdentity {
        ClientIdentity::new(None, None, None, std::sync::Arc::new(|_| Vec::new()))
    }

    /// 开始一轮判定。同一轮内最多读一次进程表（例如 `list_sessions` 对每个标签判定）；
    /// 下一轮会重新读，所以进程号复用不会让旧结果跨调用残留。
    /// 读进程表是同步系统调用，持有者必须在 `spawn_blocking` 里使用。
    pub(super) fn probe(&self) -> Probe<'_> {
        Probe {
            identity: self,
            chain: OnceCell::new(),
        }
    }

    /// 标签的本地终端是不是客户端的控制终端。
    ///
    /// 有设备号时只按设备号比较：对标签的 `local_tty` 路径 `stat`，要求是字符设备且 `st_rdev` 相等。
    /// 不能按路径字符串比较——客户端打开的是 `/dev/tty` 别名，路径写法也可能不同（符号链接等），
    /// 设备号才是同一个终端的唯一标识。`stat` 失败（终端已关闭）视为不命中。
    /// 没有设备号（旧版转发程序）时退回路径字符串比较。
    fn tty_matches(&self, local_tty: &str) -> bool {
        match self.tty_dev {
            Some(dev) => device_of(local_tty) == Some(dev),
            None => self.tty.as_deref() == Some(local_tty),
        }
    }
}

/// 一轮判定的上下文，见 [`ClientIdentity::probe`]。
pub(super) struct Probe<'a> {
    identity: &'a ClientIdentity,
    /// 惰性读取：控制终端已命中、或标签没有 shell pid 时不需要读进程表。
    chain: OnceCell<Vec<u32>>,
}

impl Probe<'_> {
    /// 这个标签是不是客户端所在的标签：控制终端相同，或客户端进程在这个标签 shell 的子孙里。
    ///
    /// 对端进程已退出时祖先链读不到（空），只剩控制终端一条线索；宁可漏判也不用过期信息误判。
    pub(super) fn is_own(&self, status: &McpSessionStatus) -> bool {
        if status.headless {
            return false;
        }
        if status
            .local_tty
            .as_deref()
            .is_some_and(|t| self.identity.tty_matches(t))
        {
            return true;
        }
        status.shell_pid.is_some_and(|shell| {
            self.chain
                .get_or_init(|| {
                    self.identity
                        .pid
                        .map_or_else(Vec::new, |pid| (self.identity.read_ancestors)(pid))
                })
                .contains(&shell)
        })
    }
}

/// 路径对应字符设备的设备号；不存在、不是字符设备或平台不支持时为 `None`。
/// 不是字符设备也要拒绝：普通文件的 `st_rdev` 是 0，会与上报 0 的客户端误匹配。
#[cfg(unix)]
fn device_of(path: &str) -> Option<u64> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::metadata(path).ok()?;
    meta.file_type().is_char_device().then(|| meta.rdev())
}

#[cfg(not(unix))]
fn device_of(_path: &str) -> Option<u64> {
    None
}

/// 找出客户端所在的标签（供设置页显示）；同步读 core，调用方放进阻塞线程池。
pub(super) fn find_own_tab(core: &Core, identity: &ClientIdentity) -> Option<SessionId> {
    let probe = identity.probe();
    core.sessions().into_iter().find_map(|info| {
        let status = core.mcp_session_status(info.id)?;
        probe.is_own(&status).then_some(info.id)
    })
}

/// 取对端进程及其祖先链（由近及远，含自身），最多 [`MAX_DEPTH`] 层，遇到 pid 0 或 1 停止（不含）。
///
/// 对端自身要算进去：AI 工具若用 `exec kanso-mcp` 之类的方式启动，对端 pid 就是 shell 的 pid。
/// 读取失败（进程已退出、没有权限）时链在那一层截断，只会少识别，不会误判；
/// 对端自己都读不到（已退出）时返回空链。每次判定都重新调用，见 [`ClientIdentity`]。
/// Windows：一次进程快照建出 pid -> ppid 表再沿表上溯；其他平台没有实现，返回空链，只靠控制终端识别。
pub(super) fn ancestors(pid: u32) -> Vec<u32> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        chain_with(pid, parent_of)
    }
    #[cfg(windows)]
    {
        // 快照失败按"识别不到"处理。
        let parents = kanso_mcp::windows::process_parents().unwrap_or_default();
        chain_with(pid, |p| parents.get(&p).copied())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = pid;
        Vec::new()
    }
}

/// 链的构造与平台无关，抽出来便于用假的父子关系测试。
///
/// 对端自身的父进程都查不到，说明它已经退出（pid 可能已被复用），此时返回空链而不是 `[pid]`：
/// 否则一个恰好占用同 pid 的无关进程会被当成客户端。
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
fn chain_with(pid: u32, parent_of: impl Fn(u32) -> Option<u32>) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut cur = pid;
    while cur > 1 && chain.len() < MAX_DEPTH {
        let parent = parent_of(cur);
        if chain.is_empty() && parent.is_none() {
            return Vec::new();
        }
        chain.push(cur);
        match parent {
            Some(parent) if parent != cur => cur = parent,
            _ => break,
        }
    }
    chain
}

/// macOS：`proc_pidinfo(PROC_PIDTBSDINFO)` 取父进程号。
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn parent_of(pid: u32) -> Option<u32> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: `proc_bsdinfo` 是纯数据的 C 结构体，全零是合法的初始值。
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: 缓冲区指针与长度来自同一个存活的局部变量；内核最多写入 `size` 字节。
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            size,
        )
    };
    // 返回值不是完整结构体的大小说明失败（进程不存在、无权限）。
    (written == size).then_some(info.pbi_ppid)
}

/// Linux：读 `/proc/<pid>/stat`。
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat_ppid(&stat)
}

/// 解析 `/proc/<pid>/stat` 的父进程号：格式为 `pid (comm) state ppid ...`。
/// 进程名 `comm` 可以含空格和括号，所以必须从**最后一个**右括号之后开始切分。
#[cfg(any(target_os = "linux", test))]
fn parse_stat_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn parses_stat_with_tricky_comm() {
        assert_eq!(parse_stat_ppid("42 (zsh) S 41 42 42 0 -1"), Some(41));
        assert_eq!(parse_stat_ppid("42 (a) b (c)) S 7 1 1"), Some(7));
        assert_eq!(parse_stat_ppid("garbage"), None);
    }

    #[test]
    fn chain_stops_at_init_depth_and_cycles() {
        // 5 -> 4 -> 3 -> 2 -> 1：不含 1。
        assert_eq!(
            chain_with(5, |p| Some(p - 1).filter(|_| p > 1)),
            [5, 4, 3, 2]
        );
        assert_eq!(chain_with(5, |p| if p == 5 { Some(1) } else { None }), [5]);
        // 父进程指向自己：只记一层。
        assert_eq!(chain_with(9, Some), [9]);
        // 无限向上：被深度上限截断。
        assert_eq!(chain_with(1000, |p| Some(p + 1)).len(), MAX_DEPTH);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn real_chain_contains_self_and_parent() {
        let me = std::process::id();
        let chain = ancestors(me);
        assert_eq!(chain.first(), Some(&me));
        // SAFETY: getppid 没有参数与前置条件。
        #[allow(unsafe_code)]
        let parent = unsafe { libc::getppid() };
        if parent > 1 {
            assert_eq!(chain.get(1).copied(), u32::try_from(parent).ok());
        }
    }

    fn status(tty: Option<&str>, shell_pid: Option<u32>, headless: bool) -> McpSessionStatus {
        use kanso_core::SessionInfo;
        use kanso_types::event::{Activity, SessionState};
        use kanso_types::profile::SessionKind;
        McpSessionStatus {
            info: SessionInfo {
                id: SessionId::from_raw(1),
                profile: None,
                kind: SessionKind::Local,
                name: String::new(),
                custom_title: None,
                state: SessionState::Connected,
            },
            activity: Activity::Idle,
            locked: false,
            terminal_transfer: false,
            sender_active: false,
            run_blocker: None,
            headless,
            osc133: true,
            local_tty: tty.map(str::to_owned),
            shell_pid,
        }
    }

    fn identity(
        tty: Option<&str>,
        tty_dev: Option<u64>,
        pid: Option<u32>,
        chain: Vec<u32>,
    ) -> ClientIdentity {
        ClientIdentity::new(
            tty.map(str::to_owned),
            tty_dev,
            pid,
            std::sync::Arc::new(move |_| chain.clone()),
        )
    }

    fn is_own(id: &ClientIdentity, status: &McpSessionStatus) -> bool {
        id.probe().is_own(status)
    }

    #[test]
    fn matches_by_tty_path_without_device_number() {
        // 旧版转发程序没有 tty_dev：退回路径字符串比较。
        let id = identity(Some("/dev/ttys003"), None, Some(300), vec![300, 200, 100]);
        assert!(is_own(&id, &status(Some("/dev/ttys003"), None, false)));
        assert!(is_own(&id, &status(Some("/dev/ttys009"), Some(200), false)));
        assert!(!is_own(
            &id,
            &status(Some("/dev/ttys009"), Some(201), false)
        ));
        assert!(!is_own(&id, &status(None, None, false)));
        assert!(!is_own(&id, &status(Some("/dev/ttys003"), None, true)));
        let none = ClientIdentity::unknown();
        assert!(!is_own(
            &none,
            &status(Some("/dev/ttys003"), Some(1), false)
        ));
    }

    #[test]
    fn chain_is_recomputed_on_every_probe_and_needs_a_peer_pid() {
        use std::sync::atomic::{AtomicU32, Ordering};
        // 第一次判定时对端的祖先里有 shell 200，之后"进程退出、pid 被复用"，链变了。
        let shell = Arc::new(AtomicU32::new(200));
        let s = shell.clone();
        let id = ClientIdentity::new(
            None,
            None,
            Some(300),
            Arc::new(move |_| vec![300, s.load(Ordering::SeqCst)]),
        );
        let st = status(None, Some(200), false);
        assert!(is_own(&id, &st));
        shell.store(999, Ordering::SeqCst);
        assert!(!is_own(&id, &st), "旧链不能跨判定残留");
        // 没有对端 pid：不读进程表。
        let id = identity(None, None, None, vec![200]);
        assert!(!is_own(&id, &st));
        // 对端已退出：读出空链，不命中。
        let id = identity(None, None, Some(300), Vec::new());
        assert!(!is_own(&id, &st));
    }

    #[test]
    fn exited_peer_gives_empty_chain() {
        assert!(chain_with(500, |_| None).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn device_number_matching_with_stat_failures() {
        use std::os::unix::fs::MetadataExt;
        // /dev/null 是字符设备，借它的设备号验证比较逻辑；不依赖 pty。
        let dev = std::fs::metadata("/dev/null").unwrap().rdev();
        let id = identity(None, Some(dev), None, vec![]);
        assert!(is_own(&id, &status(Some("/dev/null"), None, false)));
        // 另一个设备：设备号不同。
        assert!(!is_own(&id, &status(Some("/dev/zero"), None, false)));
        // stat 失败（路径不存在）视为不命中，哪怕路径字符串相同。
        let id = identity(Some("/dev/ttys-nope"), Some(dev), None, vec![]);
        assert!(!is_own(&id, &status(Some("/dev/ttys-nope"), None, false)));
        // 有设备号时不再按路径字符串比较：路径相同但设备号不同不命中。
        let id = identity(Some("/dev/null"), Some(dev.wrapping_add(1)), None, vec![]);
        assert!(!is_own(&id, &status(Some("/dev/null"), None, false)));
        // 普通文件的 st_rdev 为 0，不能因为客户端上报 0 而命中。
        let file = tempfile::NamedTempFile::new().unwrap();
        let id = identity(None, Some(0), None, vec![]);
        let path = file.path().to_str().unwrap();
        assert!(!is_own(&id, &status(Some(path), None, false)));
    }

    /// 用真实 pty 做端到端比对：客户端像转发程序那样对设备取 `st_rdev`，
    /// 标签侧给出 pty 从设备的路径，二者必须命中；另一个 pty 不命中。
    #[cfg(unix)]
    #[test]
    #[allow(unsafe_code)]
    fn real_pty_matches_by_device_number() {
        use std::os::unix::fs::MetadataExt;

        struct Pty {
            master: libc::c_int,
            slave: libc::c_int,
            name: String,
        }
        impl Drop for Pty {
            fn drop(&mut self) {
                // SAFETY: 两个描述符都由 openpty 创建、仅在此处关闭一次。
                unsafe {
                    libc::close(self.master);
                    libc::close(self.slave);
                }
            }
        }
        fn open_pty() -> Pty {
            let (mut master, mut slave) = (0, 0);
            let mut name = [0 as libc::c_char; 128];
            // SAFETY: 输出参数都是可写的局部变量，name 缓冲区足够容纳从设备路径；
            // termios 与 winsize 传 NULL 表示用默认值。
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    name.as_mut_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
            // SAFETY: openpty 成功后 name 是以 NUL 结尾的字符串。
            let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            Pty {
                master,
                slave,
                name,
            }
        }

        let mine = open_pty();
        let other = open_pty();
        // 客户端侧：取设备号（转发程序对 /dev/tty 做的同一件事）。
        let dev = std::fs::metadata(&mine.name).unwrap().rdev();
        let id = identity(None, Some(dev), None, vec![]);
        assert!(is_own(&id, &status(Some(&mine.name), None, false)));
        assert!(!is_own(&id, &status(Some(&other.name), None, false)));
    }
}
