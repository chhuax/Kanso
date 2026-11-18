//! 光标闪烁相位。
//!
//! 约定：有输入 / 移动光标后先常亮一个半周期，然后按固定周期交替。视图用定时器调用 [`Blink::tick`]；
//! 每次“重置”会递增纪元，旧定时器看到纪元变了就退出，避免重置后第一个半周期被缩短。

use std::time::Duration;

/// 半个闪烁周期（Windows 默认光标闪烁时间约 530 ms）。
pub const HALF_PERIOD: Duration = Duration::from_millis(530);

/// 光标闪烁状态。
#[derive(Clone, Debug)]
pub struct Blink {
    visible: bool,
    epoch: u64,
}

impl Default for Blink {
    fn default() -> Self {
        Blink {
            visible: true,
            epoch: 0,
        }
    }
}

impl Blink {
    /// 光标当前是否可见。
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// 重置为常亮并开启新纪元，返回新纪元。
    pub fn reset(&mut self) -> u64 {
        self.visible = true;
        self.epoch += 1;
        self.epoch
    }

    /// 停止闪烁（失焦）：不可见，旧定时器作废。
    pub fn stop(&mut self) {
        self.visible = false;
        self.epoch += 1;
    }

    /// 定时器到点。纪元匹配才翻转并返回 `true`；不匹配说明定时器已过期，返回 `false`（应退出循环）。
    pub fn tick(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.visible = !self.visible;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_with_matching_epoch() {
        let mut b = Blink::default();
        let e = b.reset();
        assert!(b.visible());
        assert!(b.tick(e));
        assert!(!b.visible());
        assert!(b.tick(e));
        assert!(b.visible());
    }

    #[test]
    fn stale_timer_is_rejected() {
        let mut b = Blink::default();
        let old = b.reset();
        b.tick(old);
        let new = b.reset();
        assert!(b.visible());
        assert!(!b.tick(old));
        assert!(b.visible());
        assert!(b.tick(new));
    }

    #[test]
    fn stop_hides_and_invalidates() {
        let mut b = Blink::default();
        let e = b.reset();
        b.stop();
        assert!(!b.visible());
        assert!(!b.tick(e));
    }
}
