//! 客户端所在标签的识别（spec/mcp.md §2.5）：控制终端 + 进程祖先链。
//!
//! 对端自报的 tty 与对端进程号都只用于**限制**该客户端自己（拒绝它写自己所在的标签），
//! 伪造它们只会让这个客户端多被拒几个标签，得不到任何额外权限，所以不需要把它们当作可信输入。

use kanso_core::Core;
use kanso_core::mcp::McpSessionStatus;
use kanso_types::SessionId;

/// 祖先链最多走这么多层：防止异常的父子关系（pid 复用、读取竞态）导致死循环。
const MAX_DEPTH: usize = 64;

/// 一个连接上的客户端身份线索，连接建立时算一次并缓存；与标签的比对在每次调用时做，
/// 因为标签会随时增减，而祖先链在进程存活期间不会变。
#[derive(Debug, Default, Clone)]
pub(super) struct ClientIdentity {
    /// 握手上报的控制终端，没有或为空串时为 `None`。
    pub(super) tty: Option<String>,
    /// 对端进程自身及其祖先的 pid（由近及远），不含 0 与 1。
    pub(super) ancestors: Vec<u32>,
}

impl ClientIdentity {
    /// 这个标签是不是客户端所在的标签：控制终端相同，或客户端进程在这个标签 shell 的子孙里。
    pub(super) fn is_own(&self, status: &McpSessionStatus) -> bool {
        if status.headless {
            return false;
        }
        let tty_hit = matches!(
            (&self.tty, &status.local_tty),
            (Some(mine), Some(theirs)) if mine == theirs
        );
        let chain_hit = status
            .shell_pid
            .is_some_and(|shell| self.ancestors.contains(&shell));
        tty_hit || chain_hit
    }
}

/// 找出客户端所在的标签（供设置页显示）；同步读 core，调用方放进阻塞线程池。
pub(super) fn find_own_tab(core: &Core, identity: &ClientIdentity) -> Option<SessionId> {
    core.sessions().into_iter().find_map(|info| {
        let status = core.mcp_session_status(info.id)?;
        identity.is_own(&status).then_some(info.id)
    })
}

/// 取对端进程及其祖先链（由近及远，含自身），最多 [`MAX_DEPTH`] 层，遇到 pid 0 或 1 停止（不含）。
///
/// 对端自身要算进去：AI 工具若用 `exec kanso-mcp` 之类的方式启动，对端 pid 就是 shell 的 pid。
/// 读取失败（进程已退出、没有权限）时链在那一层截断，只会少识别，不会误判。
/// 其他平台（Windows 等）没有实现，返回空链，只靠控制终端识别。
pub(super) fn ancestors(pid: u32) -> Vec<u32> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        chain_with(pid, parent_of)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Vec::new()
    }
}

/// 链的构造与平台无关，抽出来便于用假的父子关系测试。
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn chain_with(pid: u32, parent_of: impl Fn(u32) -> Option<u32>) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut cur = pid;
    while cur > 1 && chain.len() < MAX_DEPTH {
        chain.push(cur);
        match parent_of(cur) {
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
                state: SessionState::Connected,
            },
            activity: Activity::Idle,
            locked: false,
            terminal_transfer: false,
            sender_active: false,
            headless,
            osc133: true,
            local_tty: tty.map(str::to_owned),
            shell_pid,
        }
    }

    #[test]
    fn matches_by_tty_or_chain() {
        let id = ClientIdentity {
            tty: Some("/dev/ttys003".into()),
            ancestors: vec![300, 200, 100],
        };
        assert!(id.is_own(&status(Some("/dev/ttys003"), None, false)));
        assert!(id.is_own(&status(Some("/dev/ttys009"), Some(200), false)));
        assert!(!id.is_own(&status(Some("/dev/ttys009"), Some(201), false)));
        assert!(!id.is_own(&status(None, None, false)));
        assert!(!id.is_own(&status(Some("/dev/ttys003"), None, true)));
        let none = ClientIdentity::default();
        assert!(!none.is_own(&status(Some("/dev/ttys003"), Some(1), false)));
    }
}
