//! 焦点链（Tab / Shift+Tab）的纯逻辑：给定一排站点的“是否可停靠”标记与当前站点，求下一站。
//!
//! 规则：按登记顺序前进 / 后退，首尾相接；跳过不可停靠（禁用）的站点；
//! 当前不在链上（`None`）时 Tab 落到第一个可停靠站点、Shift+Tab 落到最后一个；
//! 没有任何可停靠站点时返回 `None`（调用方不动焦点）。
//! 只有一个可停靠站点且焦点就在它上面时，返回它自己。

/// 在 `enabled`（每站是否可停靠）上从 `current` 前进（`forward`）或后退一站。
pub fn step(enabled: &[bool], current: Option<usize>, forward: bool) -> Option<usize> {
    let n = enabled.len();
    if n == 0 || !enabled.iter().any(|e| *e) {
        return None;
    }
    let current = current.filter(|c| *c < n);
    let start = match (current, forward) {
        (Some(c), _) => c,
        // 不在链上：从“第一站之前”或“最后一站之后”起步，下面的循环自然落到两端。
        (None, true) => n - 1,
        (None, false) => 0,
    };
    (1..=n)
        .map(|k| {
            if forward {
                (start + k) % n
            } else {
                (start + n - k % n) % n
            }
        })
        .find(|i| enabled[*i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_and_wrap() {
        let e = [true, true, true];
        assert_eq!(step(&e, Some(0), true), Some(1));
        assert_eq!(step(&e, Some(1), true), Some(2));
        assert_eq!(step(&e, Some(2), true), Some(0));
    }

    #[test]
    fn backward_and_wrap() {
        let e = [true, true, true];
        assert_eq!(step(&e, Some(2), false), Some(1));
        assert_eq!(step(&e, Some(1), false), Some(0));
        assert_eq!(step(&e, Some(0), false), Some(2));
    }

    #[test]
    fn skips_disabled() {
        let e = [true, false, true, false];
        assert_eq!(step(&e, Some(0), true), Some(2));
        assert_eq!(step(&e, Some(2), true), Some(0));
        assert_eq!(step(&e, Some(0), false), Some(2));
        assert_eq!(step(&e, Some(2), false), Some(0));
        // 当前站点自己被禁用（刚被禁用的焦点）：照样从它的位置往前 / 后找。
        assert_eq!(step(&e, Some(1), true), Some(2));
        assert_eq!(step(&e, Some(1), false), Some(0));
    }

    #[test]
    fn not_in_chain_lands_on_ends() {
        let e = [false, true, true, false];
        assert_eq!(step(&e, None, true), Some(1));
        assert_eq!(step(&e, None, false), Some(2));
        // 越界当作不在链上。
        assert_eq!(step(&e, Some(9), true), Some(1));
        assert_eq!(step(&e, Some(9), false), Some(2));
    }

    #[test]
    fn single_and_empty() {
        assert_eq!(step(&[], None, true), None);
        assert_eq!(step(&[false, false], Some(0), true), None);
        assert_eq!(step(&[false, true], Some(1), true), Some(1));
        assert_eq!(step(&[false, true], Some(1), false), Some(1));
        assert_eq!(step(&[true], None, true), Some(0));
    }
}
