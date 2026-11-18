//! 分屏布局树（值类型，与像素无关）。对照 `SplitLayout.swift`，规则同 ui-shell.md §3.1。
//!
//! 不变式（每次变更后都重新建立）：
//! - 同一 split 下子节点比例之和为 1；
//! - split 至少有 2 个子节点，单子节点的 split 被消掉；
//! - 子 split 与父 split 同方向时并入父节点；
//! - 根可以是单个 pane。

use std::collections::HashMap;

/// 分屏（pane）的稳定标识。由 [`Workspace`](super::workspace::Workspace) 单调分配，与像素、位置无关。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// 拆分方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitAxis {
    /// 左右排列（行）。
    Horizontal,
    /// 上下排列（列）。
    Vertical,
}

/// 布局树节点：叶子是 pane，内部节点按比例把空间分给子节点。
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutNode {
    /// 叶子。
    Pane(PaneId),
    /// 内部节点：方向与子节点。
    Split(SplitAxis, Vec<LayoutChild>),
}

/// 内部节点的一个子节点，`ratio` 是占父节点的份额。
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutChild {
    /// 子节点。
    pub node: LayoutNode,
    /// 占父节点的份额。
    pub ratio: f64,
}

/// 矩形，原点在左上，y 向下；单位由调用方决定（逻辑像素）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// 左边。
    pub x: f64,
    /// 上边。
    pub y: f64,
    /// 宽。
    pub width: f64,
    /// 高。
    pub height: f64,
}

impl Rect {
    /// 构造。
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// 右边。
    pub fn max_x(&self) -> f64 {
        self.x + self.width
    }

    /// 下边。
    pub fn max_y(&self) -> f64 {
        self.y + self.height
    }
}

/// 一条可拖动的分隔条（几何结果，供视图层命中测试与换算）。
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDivider {
    /// 从根到所属 split 的子节点下标序列（根本身是 split 时为空）。
    pub split_path: Vec<usize>,
    /// 分隔条位于子节点 `index` 与 `index + 1` 之间。
    pub index: usize,
    /// 所属 split 的方向。
    pub axis: SplitAxis,
    /// 分隔条矩形。
    pub rect: Rect,
    /// 所属 split 沿 axis 方向扣除全部分隔条后的可分配长度。
    /// 视图层用它把像素位移换算成 [`SplitLayout::move_divider`] 的 delta（delta = 像素 / parent_length）。
    pub parent_length: f64,
}

/// 分屏布局树。
#[derive(Clone, Debug, PartialEq)]
pub struct SplitLayout {
    root: LayoutNode,
}

impl SplitLayout {
    /// 只有一个 pane 的布局。
    pub fn single(pane: PaneId) -> Self {
        SplitLayout {
            root: LayoutNode::Pane(pane),
        }
    }

    /// 根节点（只读，视图递归绘制用）。
    pub fn root(&self) -> &LayoutNode {
        &self.root
    }

    /// 阅读顺序：深度优先、按子节点数组顺序（行从左到右，列从上到下）。
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    /// 是否包含该 pane。
    pub fn contains(&self, pane: PaneId) -> bool {
        self.panes().contains(&pane)
    }

    // ---- 结构变更 ----

    /// 在 `pane` 后面（右边 / 下边）拆出 `new_pane`，占 `pane` 原份额的一半。
    /// `pane` 不存在或 `new_pane` 已存在时返回 false 且不改动。
    pub fn split(&mut self, pane: PaneId, axis: SplitAxis, new_pane: PaneId) -> bool {
        self.split_at(pane, axis, new_pane, false)
    }

    /// 同 [`split`](Self::split)，`before` 为 true 时新 pane 放在 `pane` 的左边 / 上边
    /// （拖标签到分屏边缘新建分屏用，ui-shell.md §2.4）。
    pub fn split_at(
        &mut self,
        pane: PaneId,
        axis: SplitAxis,
        new_pane: PaneId,
        before: bool,
    ) -> bool {
        if self.contains(new_pane) {
            return false;
        }
        if let LayoutNode::Pane(p) = self.root {
            if p != pane {
                return false;
            }
            self.root = pair(pane, new_pane, axis, before);
            return true;
        }
        match inserting(&self.root, pane, axis, new_pane, before) {
            Some(replaced) => {
                self.root = normalized(replaced);
                true
            }
            None => false,
        }
    }

    /// 删除叶子，它的份额按兄弟现有比例成比例分给兄弟。
    /// 找不到，或它是最后一个 pane 时返回 false 且不改动。
    pub fn remove(&mut self, pane: PaneId) -> bool {
        match removing(&self.root, pane) {
            Some(replaced) => {
                self.root = normalized(replaced);
                true
            }
            None => false,
        }
    }

    /// 拖动分隔条。
    ///
    /// - `delta` 是占该 split 总尺寸的份额，正数表示往右 / 下。
    /// - 只改 i、i+1 两个子节点，二者都不小于 `min_fraction`；超出时夹到边界。
    /// - 若某一侧本来就小于 `min_fraction`（例如窗口被缩得很小），不会被强行拉大，
    ///   只允许朝让它变大的方向移动，避免一次拖动造成跳变。
    /// - 路径或下标无效时什么都不做。
    pub fn move_divider(
        &mut self,
        split_path: &[usize],
        divider_index: usize,
        delta: f64,
        min_fraction: f64,
    ) {
        mutate_split(&mut self.root, split_path, &mut |children| {
            if divider_index + 1 >= children.len() {
                return;
            }
            let a = children[divider_index].ratio;
            let b = children[divider_index + 1].ratio;
            let total = a + b;
            let lower = min_fraction.min(a);
            let upper = (total - min_fraction).max(a);
            let new_a = (a + delta).max(lower).min(upper);
            children[divider_index].ratio = new_a;
            children[divider_index + 1].ratio = total - new_a;
        });
    }

    // ---- 几何 ----

    /// 各 pane 的矩形。行从左到右，列从上到下。
    ///
    /// 分隔条厚度先从可分配长度里扣除；各子矩形用累积比例计算并取整到整数，
    /// 最后一个子节点吃掉剩余。不能改成各自 round(ratio * 长度)：舍入误差会累积成缝隙或重叠。
    pub fn frames(&self, rect: Rect, divider_thickness: f64) -> HashMap<PaneId, Rect> {
        let mut result = HashMap::new();
        lay_out(
            &self.root,
            rect,
            divider_thickness,
            &mut Vec::new(),
            &mut |pane, r| {
                result.insert(pane, r);
            },
            &mut |_| {},
        );
        result
    }

    /// 所有分隔条的几何信息，与 [`frames`](Self::frames) 使用同一套切分结果。
    pub fn dividers(&self, rect: Rect, divider_thickness: f64) -> Vec<LayoutDivider> {
        let mut result = Vec::new();
        lay_out(
            &self.root,
            rect,
            divider_thickness,
            &mut Vec::new(),
            &mut |_, _| {},
            &mut |d| result.push(d),
        );
        result
    }
}

fn collect(node: &LayoutNode, out: &mut Vec<PaneId>) {
    match node {
        LayoutNode::Pane(p) => out.push(*p),
        LayoutNode::Split(_, children) => children.iter().for_each(|c| collect(&c.node, out)),
    }
}

fn pair(pane: PaneId, new: PaneId, axis: SplitAxis, before: bool) -> LayoutNode {
    let (first, second) = if before { (new, pane) } else { (pane, new) };
    LayoutNode::Split(
        axis,
        vec![
            LayoutChild {
                node: LayoutNode::Pane(first),
                ratio: 0.5,
            },
            LayoutChild {
                node: LayoutNode::Pane(second),
                ratio: 0.5,
            },
        ],
    )
}

fn inserting(
    node: &LayoutNode,
    target: PaneId,
    axis: SplitAxis,
    new: PaneId,
    before: bool,
) -> Option<LayoutNode> {
    let LayoutNode::Split(a, children) = node else {
        return None;
    };
    let mut children = children.clone();
    for i in 0..children.len() {
        if children[i].node == LayoutNode::Pane(target) {
            if *a == axis {
                // 同方向：作为同级插入，二者平分原份额，不产生嵌套。
                let half = children[i].ratio / 2.0;
                children[i].ratio = half;
                let at = if before { i } else { i + 1 };
                children.insert(
                    at,
                    LayoutChild {
                        node: LayoutNode::Pane(new),
                        ratio: half,
                    },
                );
            } else {
                children[i].node = pair(target, new, axis, before);
            }
            return Some(LayoutNode::Split(*a, children));
        }
        if let Some(replaced) = inserting(&children[i].node, target, axis, new, before) {
            children[i].node = replaced;
            return Some(LayoutNode::Split(*a, children));
        }
    }
    None
}

fn removing(node: &LayoutNode, target: PaneId) -> Option<LayoutNode> {
    let LayoutNode::Split(a, children) = node else {
        return None;
    };
    let mut children = children.clone();
    if let Some(i) = children
        .iter()
        .position(|c| c.node == LayoutNode::Pane(target))
    {
        // 不在此处分配份额：留给 normalized() 按剩余兄弟的现有比例成比例归一。
        children.remove(i);
        return Some(LayoutNode::Split(*a, children));
    }
    for i in 0..children.len() {
        if let Some(replaced) = removing(&children[i].node, target) {
            children[i].node = replaced;
            return Some(LayoutNode::Split(*a, children));
        }
    }
    None
}

fn mutate_split(
    node: &mut LayoutNode,
    path: &[usize],
    body: &mut dyn FnMut(&mut Vec<LayoutChild>),
) -> bool {
    let LayoutNode::Split(_, children) = node else {
        return false;
    };
    match path.split_first() {
        Some((head, rest)) => match children.get_mut(*head) {
            Some(child) => mutate_split(&mut child.node, rest, body),
            None => false,
        },
        None => {
            body(children);
            true
        }
    }
}

/// 重建规范形式（自底向上）：并入同方向子 split、消掉单子节点 split、重新归一比例。
/// 每次变更后都归一，避免浮点误差在多次操作后累积漂移。
fn normalized(node: LayoutNode) -> LayoutNode {
    let LayoutNode::Split(axis, children) = &node else {
        return node;
    };
    let axis = *axis;
    let mut out: Vec<LayoutChild> = Vec::new();
    for child in children.iter().cloned() {
        let n = normalized(child.node);
        match n {
            LayoutNode::Split(a, grand) if a == axis => {
                for g in grand {
                    out.push(LayoutChild {
                        node: g.node,
                        ratio: g.ratio * child.ratio,
                    });
                }
            }
            other => out.push(LayoutChild {
                node: other,
                ratio: child.ratio,
            }),
        }
    }
    if out.len() == 1 {
        return out.remove(0).node;
    }
    if out.is_empty() {
        return node;
    }
    let sum: f64 = out.iter().map(|c| c.ratio.max(0.0)).sum();
    let n = out.len() as f64;
    for c in &mut out {
        c.ratio = if sum > 0.0 {
            c.ratio.max(0.0) / sum
        } else {
            1.0 / n
        };
    }
    LayoutNode::Split(axis, out)
}

/// 沿 axis 方向切出各子节点的 (start, end) 区间（绝对坐标），并返回可分配长度。
fn spans(
    children: &[LayoutChild],
    origin: f64,
    length: f64,
    thickness: f64,
) -> (Vec<(f64, f64)>, f64) {
    let n = children.len();
    let available = (length - thickness * (n as f64 - 1.0)).max(0.0);
    let mut result = Vec::with_capacity(n);
    let mut cumulative = 0.0;
    let mut previous_end = origin;
    for (k, child) in children.iter().enumerate() {
        cumulative += child.ratio;
        let start = if k == 0 {
            origin
        } else {
            previous_end + thickness
        };
        let mut end = if k == n - 1 {
            origin + length
        } else {
            origin + (cumulative * available).round() + k as f64 * thickness
        };
        end = end.max(start);
        result.push((start, end));
        previous_end = end;
    }
    (result, available)
}

fn lay_out(
    node: &LayoutNode,
    rect: Rect,
    thickness: f64,
    path: &mut Vec<usize>,
    pane: &mut dyn FnMut(PaneId, Rect),
    divider: &mut dyn FnMut(LayoutDivider),
) {
    match node {
        LayoutNode::Pane(p) => pane(*p, rect),
        LayoutNode::Split(axis, children) => {
            let horizontal = *axis == SplitAxis::Horizontal;
            let (cuts, available) = spans(
                children,
                if horizontal { rect.x } else { rect.y },
                if horizontal { rect.width } else { rect.height },
                thickness,
            );
            for (k, child) in children.iter().enumerate() {
                let (s, e) = cuts[k];
                let child_rect = if horizontal {
                    Rect::new(s, rect.y, e - s, rect.height)
                } else {
                    Rect::new(rect.x, s, rect.width, e - s)
                };
                path.push(k);
                lay_out(&child.node, child_rect, thickness, path, pane, divider);
                path.pop();
                if k < children.len() - 1 {
                    let gap = cuts[k + 1].0 - e;
                    let r = if horizontal {
                        Rect::new(e, rect.y, gap, rect.height)
                    } else {
                        Rect::new(rect.x, e, rect.width, gap)
                    };
                    divider(LayoutDivider {
                        split_path: path.clone(),
                        index: k,
                        axis: *axis,
                        rect: r,
                        parent_length: available,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PaneId = PaneId(1);
    const B: PaneId = PaneId(2);
    const C: PaneId = PaneId(3);
    const D: PaneId = PaneId(4);

    fn ratios(l: &SplitLayout) -> Vec<f64> {
        match l.root() {
            LayoutNode::Split(_, c) => c.iter().map(|c| c.ratio).collect(),
            LayoutNode::Pane(_) => vec![],
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn single_pane_has_no_split() {
        let l = SplitLayout::single(A);
        assert_eq!(l.panes(), vec![A]);
        assert!(l.contains(A) && !l.contains(B));
    }

    #[test]
    fn split_halves_and_orders_after() {
        let mut l = SplitLayout::single(A);
        assert!(l.split(A, SplitAxis::Horizontal, B));
        assert_eq!(l.panes(), vec![A, B]);
        assert_eq!(ratios(&l), vec![0.5, 0.5]);
        assert!(!l.split(A, SplitAxis::Horizontal, B), "重复 pane 被拒绝");
        assert!(!l.split(PaneId(99), SplitAxis::Vertical, C), "目标不存在");
        assert_eq!(l.panes(), vec![A, B]);
    }

    #[test]
    fn same_axis_split_flattens_into_sibling() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.split(B, SplitAxis::Horizontal, C);
        let LayoutNode::Split(axis, children) = l.root() else {
            panic!("应为 split");
        };
        assert_eq!(*axis, SplitAxis::Horizontal);
        assert_eq!(children.len(), 3, "同方向并入父节点，不嵌套");
        assert!(
            near(children[0].ratio, 0.5)
                && near(children[1].ratio, 0.25)
                && near(children[2].ratio, 0.25)
        );
    }

    #[test]
    fn cross_axis_split_nests() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.split(B, SplitAxis::Vertical, C);
        assert_eq!(l.panes(), vec![A, B, C]);
        let LayoutNode::Split(_, children) = l.root() else {
            panic!()
        };
        assert!(matches!(
            children[1].node,
            LayoutNode::Split(SplitAxis::Vertical, _)
        ));
    }

    #[test]
    fn split_before_places_new_pane_first() {
        let mut l = SplitLayout::single(A);
        assert!(l.split_at(A, SplitAxis::Horizontal, B, true));
        assert_eq!(l.panes(), vec![B, A]);
        l.split_at(A, SplitAxis::Horizontal, C, true);
        assert_eq!(l.panes(), vec![B, C, A]);
    }

    #[test]
    fn remove_redistributes_proportionally_and_collapses() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.split(B, SplitAxis::Horizontal, C); // 0.5 0.25 0.25
        assert!(l.remove(B));
        assert_eq!(l.panes(), vec![A, C]);
        let r = ratios(&l);
        assert!(near(r[0], 2.0 / 3.0) && near(r[1], 1.0 / 3.0), "{r:?}");
        assert!(l.remove(C));
        assert_eq!(*l.root(), LayoutNode::Pane(A), "单子节点的 split 被消掉");
        assert!(!l.remove(A), "最后一个 pane 不能删");
    }

    #[test]
    fn remove_nested_merges_into_parent_axis() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.split(B, SplitAxis::Vertical, C);
        l.split(C, SplitAxis::Horizontal, D);
        // 删掉 C 之后，竖向子 split 只剩 D 被消掉，D 与 A 同为横向兄弟。
        assert!(l.remove(C));
        assert!(l.panes().contains(&D));
        let LayoutNode::Split(axis, c) = l.root() else {
            panic!()
        };
        assert_eq!(*axis, SplitAxis::Horizontal);
        assert!((c.iter().map(|c| c.ratio).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn move_divider_clamps_to_min_fraction() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.move_divider(&[], 0, 0.2, 0.1);
        let r = ratios(&l);
        assert!(near(r[0], 0.7) && near(r[1], 0.3));
        l.move_divider(&[], 0, 5.0, 0.1);
        let r = ratios(&l);
        assert!(near(r[0], 0.9) && near(r[1], 0.1));
        l.move_divider(&[], 0, -5.0, 0.1);
        let r = ratios(&l);
        assert!(near(r[0], 0.1) && near(r[1], 0.9));
    }

    #[test]
    fn move_divider_never_grows_an_already_small_side() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.move_divider(&[], 0, 0.45, 0.01); // A = 0.95, B = 0.05
        l.move_divider(&[], 0, 0.04, 0.2); // B 已经小于 min：A 不会再变大
        let r = ratios(&l);
        assert!(near(r[0], 0.95), "{r:?}");
        l.move_divider(&[], 0, -0.1, 0.2); // 朝让 B 变大的方向可以移动
        assert!(ratios(&l)[1] > 0.05);
    }

    #[test]
    fn move_divider_invalid_path_is_noop() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        let before = l.clone();
        l.move_divider(&[3], 0, 0.1, 0.1);
        l.move_divider(&[], 5, 0.1, 0.1);
        assert_eq!(l, before);
    }

    #[test]
    fn frames_tile_without_gaps_or_overlap() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        l.split(B, SplitAxis::Horizontal, C);
        let frames = l.frames(Rect::new(0.0, 0.0, 1001.0, 600.0), 1.0);
        let (a, b, c) = (frames[&A], frames[&B], frames[&C]);
        assert!(near(a.x, 0.0));
        assert!(near(a.max_x() + 1.0, b.x));
        assert!(near(b.max_x() + 1.0, c.x));
        assert!(near(c.max_x(), 1001.0));
        assert!(near(a.height, 600.0));
        let divs = l.dividers(Rect::new(0.0, 0.0, 1001.0, 600.0), 1.0);
        assert_eq!(divs.len(), 2);
        assert!(near(divs[0].rect.x, a.max_x()) && near(divs[0].rect.width, 1.0));
        assert!(near(divs[0].parent_length, 999.0));
        assert_eq!(divs[1].index, 1);
    }

    #[test]
    fn frames_vertical_and_nested() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Vertical, B);
        l.split(B, SplitAxis::Horizontal, C);
        let frames = l.frames(Rect::new(10.0, 20.0, 400.0, 301.0), 1.0);
        assert!(near(frames[&A].y, 20.0));
        assert!(near(frames[&A].max_y() + 1.0, frames[&B].y));
        assert!(near(frames[&B].width + frames[&C].width + 1.0, 400.0));
        assert!(near(frames[&C].max_y(), 321.0));
        let divs = l.dividers(Rect::new(10.0, 20.0, 400.0, 301.0), 1.0);
        assert_eq!(divs.len(), 2);
        assert!(
            divs.iter()
                .any(|d| d.split_path == vec![1] && d.axis == SplitAxis::Horizontal)
        );
    }

    #[test]
    fn frames_with_tiny_rect_does_not_go_negative() {
        let mut l = SplitLayout::single(A);
        l.split(A, SplitAxis::Horizontal, B);
        let frames = l.frames(Rect::new(0.0, 0.0, 0.5, 10.0), 1.0);
        assert!(frames.values().all(|r| r.width >= 0.0));
    }
}
