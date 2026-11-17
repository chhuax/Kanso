//! 窗口外壳的纯逻辑：分隔条拖动换算、关闭二次确认。视图（`ui/shell.rs`）只负责把鼠标位置喂进来。

use std::time::{Duration, Instant};

use kanso_types::SessionId;

use super::layout::{LayoutDivider, SplitAxis};

/// 分隔条厚度（逻辑像素）。
pub const DIVIDER_THICKNESS: f64 = 4.0;
/// 每个分屏的最小边长（逻辑像素）：再小终端没法用。
pub const MIN_PANE_PX: f64 = 120.0;

/// 把最小边长换成占父节点的份额（上限 0.5，避免父节点很小时两侧都“不够小”而卡死）。
pub fn min_fraction(parent_length: f64) -> f64 {
    if parent_length <= 0.0 {
        return 0.5;
    }
    (MIN_PANE_PX / parent_length).min(0.5)
}

/// 一次分隔条拖动。按**鼠标绝对位置**换算而不是累加位移：分隔条被夹在边界时，
/// 鼠标继续往外拖再拖回来，分隔条会在鼠标回到它上面时才动，不会产生漂移。
#[derive(Clone, Debug, PartialEq)]
pub struct DividerDrag {
    /// 所属 split 的路径。
    pub split_path: Vec<usize>,
    /// 分隔条下标。
    pub index: usize,
    /// 方向。
    pub axis: SplitAxis,
    /// 按下时鼠标相对分隔条起点的偏移（沿拖动方向）。
    grab_offset: f64,
}

impl DividerDrag {
    /// 在分隔条 `divider` 上按下鼠标（`mouse` 为沿拖动方向的窗口坐标）。
    pub fn start(divider: &LayoutDivider, mouse: f64) -> DividerDrag {
        DividerDrag {
            split_path: divider.split_path.clone(),
            index: divider.index,
            axis: divider.axis,
            grab_offset: mouse - axis_start(divider),
        }
    }

    /// 鼠标移到 `mouse` 时，分隔条应移动的份额（传给 [`Workspace::move_divider`](super::workspace::Workspace::move_divider)）。
    /// `divider` 必须是**当前**布局里的同一条分隔条。
    pub fn delta(&self, divider: &LayoutDivider, mouse: f64) -> f64 {
        if divider.parent_length <= 0.0 {
            return 0.0;
        }
        (mouse - self.grab_offset - axis_start(divider)) / divider.parent_length
    }

    /// 是否指同一条分隔条。
    pub fn is(&self, divider: &LayoutDivider) -> bool {
        self.split_path == divider.split_path && self.index == divider.index
    }
}

fn axis_start(d: &LayoutDivider) -> f64 {
    match d.axis {
        SplitAxis::Horizontal => d.rect.x,
        SplitAxis::Vertical => d.rect.y,
    }
}

/// 关闭的二次确认（过渡方案）。
///
/// 正式的确认对话框由控件组提供；在它接进来之前，需要确认的关闭（本地 shell 里有前台命令、
/// SSH 仍连接、有传输）要求在 [`CONFIRM_WINDOW`] 内对同一会话再触发一次关闭命令。
/// TODO(dialog)：对话框就绪后，用 `CloseConfirmation` 弹框代替这里。
#[derive(Clone, Debug, Default)]
pub struct CloseGuard {
    pending: Option<(SessionId, Instant)>,
}

/// 二次确认的有效时间。
pub const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// 关闭请求的处理结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseDecision {
    /// 可以关闭。
    Close,
    /// 需要再确认一次：视图提示用户再按一次。
    Confirm,
}

impl CloseGuard {
    /// 处理一次关闭请求。`needs_confirm` 来自 `Workspace::close_confirmation`。
    pub fn request(
        &mut self,
        session: SessionId,
        needs_confirm: bool,
        now: Instant,
    ) -> CloseDecision {
        if !needs_confirm {
            self.pending = None;
            return CloseDecision::Close;
        }
        match self.pending {
            Some((s, at))
                if s == session && now.saturating_duration_since(at) <= CONFIRM_WINDOW =>
            {
                self.pending = None;
                CloseDecision::Close
            }
            _ => {
                self.pending = Some((session, now));
                CloseDecision::Confirm
            }
        }
    }

    /// 正在等待确认的会话（未过期）。
    pub fn pending(&self, now: Instant) -> Option<SessionId> {
        self.pending
            .filter(|(_, at)| now.saturating_duration_since(*at) <= CONFIRM_WINDOW)
            .map(|(s, _)| s)
    }

    /// 取消等待（做了别的操作）。
    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::layout::{PaneId, Rect, SplitLayout};

    fn sid(n: u64) -> SessionId {
        SessionId::from_raw(n)
    }

    fn two_panes() -> SplitLayout {
        let mut l = SplitLayout::single(PaneId(1));
        assert!(l.split(PaneId(1), SplitAxis::Horizontal, PaneId(2)));
        l
    }

    #[test]
    fn min_fraction_scales_and_caps() {
        assert!((min_fraction(1200.0) - 0.1).abs() < 1e-9);
        assert_eq!(min_fraction(100.0), 0.5);
        assert_eq!(min_fraction(0.0), 0.5);
    }

    #[test]
    fn drag_follows_absolute_mouse_position_without_drift() {
        let mut layout = two_panes();
        let area = Rect::new(240.0, 0.0, 1000.0, 600.0);
        let d = layout.dividers(area, DIVIDER_THICKNESS).remove(0);
        // 在分隔条中间按下。
        let down_x = d.rect.x + 2.0;
        let drag = DividerDrag::start(&d, down_x);
        assert!(drag.delta(&d, down_x).abs() < 1e-9, "没动就不该有位移");

        // 向右拖 100 像素。
        let delta = drag.delta(&d, down_x + 100.0);
        assert!((delta - 100.0 / d.parent_length).abs() < 1e-9);
        layout.move_divider(&d.split_path, d.index, delta, min_fraction(d.parent_length));
        let d2 = layout.dividers(area, DIVIDER_THICKNESS).remove(0);
        assert!((d2.rect.x - (d.rect.x + 100.0)).abs() <= 1.0);
        assert!(drag.is(&d2));
        // 鼠标没动，位移归零：自校正。
        assert!(drag.delta(&d2, down_x + 100.0).abs() < 0.002);
    }

    #[test]
    fn drag_past_the_limit_clamps_then_recovers() {
        let mut layout = two_panes();
        let area = Rect::new(0.0, 0.0, 1004.0, 600.0);
        let d = layout.dividers(area, DIVIDER_THICKNESS).remove(0);
        let min = min_fraction(d.parent_length);
        let drag = DividerDrag::start(&d, d.rect.x + 2.0);
        // 一口气拖到窗口外很远。
        let delta = drag.delta(&d, 5000.0);
        layout.move_divider(&d.split_path, d.index, delta, min);
        let d2 = layout.dividers(area, DIVIDER_THICKNESS).remove(0);
        let right = area.max_x() - d2.rect.max_x();
        assert!(
            right >= MIN_PANE_PX - 1.0,
            "右侧分屏不得小于最小边长，实际 {right}"
        );
        // 鼠标拖回分隔条原来的位置：分隔条应该回来（按绝对位置，不会因为之前被夹住而漂移）。
        let back = drag.delta(&d2, d.rect.x + 2.0);
        layout.move_divider(&d2.split_path, d2.index, back, min);
        let d3 = layout.dividers(area, DIVIDER_THICKNESS).remove(0);
        assert!((d3.rect.x - d.rect.x).abs() <= 1.0);
    }

    #[test]
    fn vertical_split_uses_y() {
        let mut l = SplitLayout::single(PaneId(1));
        assert!(l.split(PaneId(1), SplitAxis::Vertical, PaneId(2)));
        let area = Rect::new(0.0, 30.0, 800.0, 604.0);
        let d = l.dividers(area, DIVIDER_THICKNESS).remove(0);
        let drag = DividerDrag::start(&d, d.rect.y + 1.0);
        let delta = drag.delta(&d, d.rect.y + 1.0 - 60.0);
        assert!(delta < 0.0);
        assert!((delta + 60.0 / d.parent_length).abs() < 1e-9);
    }

    #[test]
    fn close_without_confirmation_is_immediate() {
        let mut g = CloseGuard::default();
        assert_eq!(
            g.request(sid(1), false, Instant::now()),
            CloseDecision::Close
        );
        assert_eq!(g.pending(Instant::now()), None);
    }

    #[test]
    fn close_needing_confirmation_requires_a_second_request_in_time() {
        let mut g = CloseGuard::default();
        let t0 = Instant::now();
        assert_eq!(g.request(sid(1), true, t0), CloseDecision::Confirm);
        assert_eq!(g.pending(t0), Some(sid(1)));
        assert_eq!(
            g.request(sid(1), true, t0 + Duration::from_secs(1)),
            CloseDecision::Close
        );
        assert_eq!(g.pending(t0), None);
    }

    #[test]
    fn confirmation_expires_and_is_per_session() {
        let mut g = CloseGuard::default();
        let t0 = Instant::now();
        assert_eq!(g.request(sid(1), true, t0), CloseDecision::Confirm);
        // 过期后重新开始。
        assert_eq!(
            g.request(sid(1), true, t0 + CONFIRM_WINDOW + Duration::from_secs(1)),
            CloseDecision::Confirm
        );
        // 换了一个会话：不能借用上一个的确认。
        assert_eq!(
            g.request(sid(2), true, t0 + Duration::from_secs(5)),
            CloseDecision::Confirm
        );
        assert_eq!(g.pending(t0 + Duration::from_secs(5)), Some(sid(2)));
        g.clear();
        assert_eq!(g.pending(t0 + Duration::from_secs(5)), None);
    }
}
