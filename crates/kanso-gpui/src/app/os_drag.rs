//! 条目拖出到其他应用（资源管理器、桌面……）的纯逻辑（docs/spec/filer.md §7.2、§7.4）。
//!
//! Windows 上由 `ui/os_drag.rs` 调 OLE `DoDragDrop` 发起系统拖放；这里放不依赖系统的部分：
//! 何时从应用内拖动切换为系统拖放、`CF_HDROP` 数据怎么排布、`DoDragDrop` 询问"继续还是结束"时的判断、
//! 以及"自己拖出的文件又落回本应用"的标记。
//!
//! 切换策略：应用内拖动进行中轮询指针；鼠标离开窗口客户区、左键仍按着、并且条目已就绪（本地条目恒就绪，
//! 远端条目要等暂存副本下载完）时，结束 GPUI 的内部拖动并启动系统拖放。就绪前不启动——数据对象需要现成的本机路径。

use std::sync::atomic::{AtomicBool, Ordering};

use super::drag_out::DragPrep;

/// 一次指针采样（屏幕上的实际鼠标状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// 指针在窗口客户区之外。
    pub outside: bool,
    /// 鼠标左键仍按着。
    pub left_down: bool,
}

/// 指针（客户区坐标）是否在 `width` × `height` 的客户区之外。
pub fn is_outside(x: i32, y: i32, width: i32, height: i32) -> bool {
    x < 0 || y < 0 || x >= width || y >= height
}

/// 轮询的结论。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// 继续应用内拖动（还在窗口里、条目还没就绪，或已经松手）。
    Wait,
    /// 改为系统拖放，交付这些本机路径。`DragPrep` 已记为"已交给系统"，之后不再提示。
    Start(Vec<String>),
}

/// 根据指针与准备状态决定是否切换为系统拖放。
///
/// 没有采样（取不到窗口句柄）视为不切换。准备状态不满足时（还在下载、失败、已取消）一律等待：
/// 远端副本在指针持续留在窗口外期间准备好，下一次采样就会启动；提前松手由原有的结束处理取消下载。
pub fn next_step(pointer: Option<Pointer>, prep: &DragPrep) -> Step {
    let Some(p) = pointer else {
        return Step::Wait;
    };
    if !p.outside || !p.left_down {
        return Step::Wait;
    }
    match prep.begin_os_drag() {
        Some(paths) => Step::Start(paths),
        None => Step::Wait,
    }
}

/// `IDropSource::QueryContinueDrag` 的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinueDrag {
    /// 继续拖。
    Continue,
    /// 左键松开：落下。
    Drop,
    /// 按了 Esc：取消。
    Cancel,
}

/// 按 Esc 取消，左键松开落下，否则继续。Esc 优先于松手。
pub fn continue_drag(escape_pressed: bool, left_down: bool) -> ContinueDrag {
    if escape_pressed {
        ContinueDrag::Cancel
    } else if !left_down {
        ContinueDrag::Drop
    } else {
        ContinueDrag::Continue
    }
}

/// 系统拖放的结局。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OsOutcome {
    /// 接收方接受了（复制）。
    Dropped,
    /// 用户取消（Esc）或落在不接受的地方。
    Cancelled,
    /// 系统拖放启动失败，原因（HRESULT 文字）。
    Failed(String),
}

/// `DROPFILES` 头的字节数（`pFiles` + `POINT` + `fNC` + `fWide`）。
const DROPFILES_LEN: usize = 20;

/// 组装 `CF_HDROP` 的全局内存内容：`DROPFILES` 头 + 以 NUL 分隔、双 NUL 结尾的 UTF-16 路径列表。
pub fn build_hdrop(paths: &[String]) -> Vec<u8> {
    let body: usize = paths.iter().map(|p| p.len() * 2 + 2).sum();
    let mut out = Vec::with_capacity(DROPFILES_LEN + body + 2);
    out.extend_from_slice(&(DROPFILES_LEN as u32).to_le_bytes()); // pFiles：路径列表的偏移
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide：UTF-16
    for p in paths {
        for unit in p.encode_utf16().filter(|u| *u != 0) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes()); // 列表结尾的第二个 NUL
    out
}

static SELF_DRAGGING: AtomicBool = AtomicBool::new(false);

/// 系统拖放进行中（含它在本窗口产生的落下事件）。Filer 的"外部文件落下即上传"据此忽略自己拖出的文件，
/// 不会把拖出又拖回的文件回传一遍（§7.5）。拖放结束即清除，不影响之后的外部拖入。
pub fn self_dragging() -> bool {
    SELF_DRAGGING.load(Ordering::SeqCst)
}

/// 系统拖放期间持有；释放（含 panic 展开）时清除标记。
pub struct SelfDragGuard(());

impl SelfDragGuard {
    /// 置标记。
    pub fn new() -> SelfDragGuard {
        SELF_DRAGGING.store(true, Ordering::SeqCst);
        SelfDragGuard(())
    }
}

impl Default for SelfDragGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SelfDragGuard {
    fn drop(&mut self) {
        SELF_DRAGGING.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::super::drag_out::{DropOutcome, Effect};
    use super::*;

    fn out_held() -> Option<Pointer> {
        Some(Pointer {
            outside: true,
            left_down: true,
        })
    }

    #[test]
    fn outside_edges() {
        assert!(!is_outside(0, 0, 100, 50));
        assert!(!is_outside(99, 49, 100, 50));
        assert!(is_outside(100, 10, 100, 50));
        assert!(is_outside(10, 50, 100, 50));
        assert!(is_outside(-1, 10, 100, 50));
        assert!(is_outside(10, -1, 100, 50));
    }

    #[test]
    fn local_starts_when_outside_and_held() {
        let p = DragPrep::local(vec!["C:\\a".into(), "C:\\b".into()]);
        assert_eq!(
            next_step(out_held(), &p),
            Step::Start(vec!["C:\\a".into(), "C:\\b".into()])
        );
    }

    #[test]
    fn stays_in_app_while_inside_or_released_or_unknown() {
        let p = DragPrep::local(vec!["x".into()]);
        let inside = Pointer {
            outside: false,
            left_down: true,
        };
        let released = Pointer {
            outside: true,
            left_down: false,
        };
        assert_eq!(next_step(Some(inside), &p), Step::Wait);
        assert_eq!(next_step(Some(released), &p), Step::Wait);
        assert_eq!(next_step(None, &p), Step::Wait);
        // 没启动过：之后仍可交给应用内落点。
        assert_eq!(p.on_drop(), DropOutcome::Insert(vec!["x".into()]));
    }

    #[test]
    fn remote_waits_for_staging_then_starts() {
        let p = DragPrep::remote();
        assert_eq!(next_step(out_held(), &p), Step::Wait);
        assert!(p.is_preparing(), "等待期间不取消下载");
        p.complete(Ok(vec!["T\\f".into()]));
        assert_eq!(next_step(out_held(), &p), Step::Start(vec!["T\\f".into()]));
    }

    #[test]
    fn remote_failed_or_cancelled_never_starts() {
        let failed = DragPrep::remote();
        failed.complete(Err("boom".into()));
        assert_eq!(next_step(out_held(), &failed), Step::Wait);
        let cancelled = DragPrep::remote();
        cancelled.on_ended();
        assert_eq!(next_step(out_held(), &cancelled), Step::Wait);
    }

    #[test]
    fn starts_only_once_and_is_silent_afterwards() {
        let p = DragPrep::local(vec!["x".into()]);
        assert!(matches!(next_step(out_held(), &p), Step::Start(_)));
        assert_eq!(next_step(out_held(), &p), Step::Wait);
        assert!(p.is_over());
        assert_eq!(p.on_ended(), Effect::Nothing);
        // 已交给系统的远端副本也不再提示"请重新拖动"。
        let r = DragPrep::remote();
        r.complete(Ok(vec!["t".into()]));
        assert!(matches!(next_step(out_held(), &r), Step::Start(_)));
        assert_eq!(r.on_ended(), Effect::Nothing);
    }

    #[test]
    fn continue_rules() {
        assert_eq!(continue_drag(false, true), ContinueDrag::Continue);
        assert_eq!(continue_drag(false, false), ContinueDrag::Drop);
        assert_eq!(continue_drag(true, true), ContinueDrag::Cancel);
        assert_eq!(continue_drag(true, false), ContinueDrag::Cancel);
    }

    #[test]
    fn hdrop_layout() {
        let bytes = build_hdrop(&["C:\\a".into(), "é".into()]);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 20);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
        let units: Vec<u16> = bytes[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let mut want: Vec<u16> = "C:\\a".encode_utf16().collect();
        want.push(0);
        want.extend("é".encode_utf16());
        want.push(0);
        want.push(0);
        assert_eq!(units, want);
    }

    #[test]
    fn hdrop_empty_is_header_plus_terminator() {
        assert_eq!(build_hdrop(&[]).len(), 22);
    }

    #[test]
    fn self_drag_flag_follows_guard() {
        assert!(!self_dragging());
        {
            let _g = SelfDragGuard::new();
            assert!(self_dragging());
        }
        assert!(!self_dragging());
    }
}
