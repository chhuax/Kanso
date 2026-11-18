//! 最低对比度保护（architecture.md 主题一节、terminal.md §15）。
//!
//! 渲染时如果前景色与最终背景色的 WCAG 2.x 对比度低于阈值，就调整前景色的亮度直到达标，色相与饱和度不变。
//! 典型场景：程序按深色背景设计，输出 ANSI 白 / 亮白，在浅色主题下几乎看不见。
//! 语义着色色与调色板已由 `cargo xtask check-tokens` 保证对比度，所以这里主要作用于程序输出的索引色与 RGB 色。

use std::collections::HashMap;

use crate::theme::Rgb8;

/// 对比度阈值范围（与设置一致）。
pub const RATIO_RANGE: (f64, f64) = (1.0, 21.0);

fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 相对亮度。
pub fn luminance(c: Rgb8) -> f64 {
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}

/// 两个颜色的 WCAG 对比度（1 到 21）。
pub fn ratio(a: Rgb8, b: Rgb8) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn to_hsl(c: Rgb8) -> (f64, f64, f64) {
    let (r, g, b) = (
        f64::from(c.r) / 255.0,
        f64::from(c.g) / 255.0,
        f64::from(c.b) / 255.0,
    );
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let l = (mx + mn) / 2.0;
    let d = mx - mn;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - mx - mn)
    } else {
        d / (mx + mn)
    };
    let h = if mx == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> Rgb8 {
    let channel = |t: f64, p: f64, q: f64| -> u8 {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    if s == 0.0 {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return Rgb8::new(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    Rgb8::new(
        channel(h + 1.0 / 3.0, p, q),
        channel(h, p, q),
        channel(h - 1.0 / 3.0, p, q),
    )
}

/// 返回满足 `target` 对比度的前景色；已达标原样返回。
///
/// 优先沿“远离背景亮度”的方向调整（改动最小、观感自然）；该方向到顶仍不达标（背景是中灰时可能发生）
/// 才尝试反方向；两个方向都不行就取对比度更高的黑或白，尽力而为。
pub fn adjust(fg: Rgb8, bg: Rgb8, target: f64) -> Rgb8 {
    if ratio(fg, bg) >= target {
        return fg;
    }
    let (h, s, l) = to_hsl(fg);
    let lighter_first = luminance(fg) >= luminance(bg);
    for lighter in [lighter_first, !lighter_first] {
        let extreme = from_hsl(h, s, if lighter { 1.0 } else { 0.0 });
        if ratio(extreme, bg) < target {
            continue;
        }
        // 二分查找满足阈值、最接近原亮度的 L，在 8 位量化后的颜色上判断，保证返回值真的达标。
        let (mut lo, mut hi) = if lighter { (l, 1.0) } else { (0.0, l) };
        for _ in 0..14 {
            let mid = (lo + hi) / 2.0;
            let ok = ratio(from_hsl(h, s, mid), bg) >= target;
            if ok == lighter {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let result = from_hsl(h, s, if lighter { hi } else { lo });
        if ratio(result, bg) >= target {
            return result;
        }
        return extreme;
    }
    let (black, white) = (Rgb8::new(0, 0, 0), Rgb8::new(255, 255, 255));
    if ratio(black, bg) >= ratio(white, bg) {
        black
    } else {
        white
    }
}

/// 带缓存的保护器：同一屏上的前景 / 背景组合很少，命中率接近 100%。
#[derive(Clone, Debug)]
pub struct ContrastGuard {
    ratio: f64,
    cache: HashMap<(u32, u32), Rgb8>,
}

impl ContrastGuard {
    /// 创建；`ratio` 小于等于 1 等价于关闭。
    pub fn new(ratio: f64) -> Self {
        ContrastGuard {
            ratio: ratio.clamp(RATIO_RANGE.0, RATIO_RANGE.1),
            cache: HashMap::new(),
        }
    }

    /// 目标对比度。
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// 是否起作用。
    pub fn enabled(&self) -> bool {
        self.ratio > 1.0
    }

    /// 调整前景色。
    pub fn adjusted(&mut self, fg: Rgb8, bg: Rgb8) -> Rgb8 {
        if !self.enabled() {
            return fg;
        }
        let key = (fg.to_u32(), bg.to_u32());
        if let Some(&c) = self.cache.get(&key) {
            return c;
        }
        let c = adjust(fg, bg, self.ratio);
        // 组合数异常多（程序输出大量不同的 RGB 色）时整体丢弃，避免无限增长。
        if self.cache.len() >= 4096 {
            self.cache.clear();
        }
        self.cache.insert(key, c);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb8 = Rgb8::new(255, 255, 255);
    const BLACK: Rgb8 = Rgb8::new(0, 0, 0);

    #[test]
    fn wcag_extremes() {
        assert!((ratio(WHITE, BLACK) - 21.0).abs() < 1e-9);
        assert!((ratio(WHITE, WHITE) - 1.0).abs() < 1e-9);
        assert_eq!(ratio(WHITE, BLACK), ratio(BLACK, WHITE));
    }

    #[test]
    fn compliant_colors_are_untouched() {
        let fg = Rgb8::new(0xED, 0xEE, 0xF0);
        let bg = Rgb8::new(0x05, 0x05, 0x05);
        assert_eq!(adjust(fg, bg, 4.5), fg);
    }

    #[test]
    fn white_on_light_background_gets_darker() {
        // 浅色主题下程序输出亮白：几乎看不见。
        let bg = Rgb8::new(0xFB, 0xFB, 0xF8);
        let out = adjust(WHITE, bg, 4.5);
        assert!(ratio(out, bg) >= 4.5, "{out:?}");
        assert!(luminance(out) < luminance(WHITE));
    }

    #[test]
    fn dark_blue_on_dark_background_gets_lighter_and_keeps_hue() {
        let bg = Rgb8::new(0x05, 0x05, 0x05);
        let fg = Rgb8::new(0x10, 0x10, 0x60);
        let out = adjust(fg, bg, 4.5);
        assert!(ratio(out, bg) >= 4.5);
        assert!(luminance(out) > luminance(fg));
        // 仍然偏蓝。
        assert!(out.b > out.r && out.b > out.g);
    }

    #[test]
    fn result_always_meets_the_target_when_reachable() {
        let bgs = [
            Rgb8::new(0x05, 0x05, 0x05),
            Rgb8::new(0xFB, 0xFB, 0xF8),
            Rgb8::new(0x2E, 0x47, 0x56),
        ];
        for bg in bgs {
            for i in 0..=15u8 {
                for target in [3.0, 4.5, 7.0] {
                    let fg = Rgb8::new(i * 17, 255 - i * 17, i * 9);
                    let out = adjust(fg, bg, target);
                    assert!(
                        ratio(out, bg) >= target,
                        "fg {fg:?} bg {bg:?} target {target} -> {out:?} ({})",
                        ratio(out, bg)
                    );
                }
            }
        }
    }

    #[test]
    fn mid_gray_background_falls_back_to_black_or_white() {
        let bg = Rgb8::new(0x77, 0x77, 0x77);
        // 21:1 不可能达到：取更好的黑或白，不死循环。
        let out = adjust(Rgb8::new(0x80, 0x80, 0x80), bg, 20.0);
        assert!(out == BLACK || out == WHITE);
    }

    #[test]
    fn guard_caches_and_can_be_disabled() {
        let bg = Rgb8::new(0xFB, 0xFB, 0xF8);
        let mut g = ContrastGuard::new(4.5);
        assert!(g.enabled());
        let a = g.adjusted(WHITE, bg);
        assert_eq!(a, g.adjusted(WHITE, bg));
        assert!(ratio(a, bg) >= 4.5);
        let mut off = ContrastGuard::new(1.0);
        assert!(!off.enabled());
        assert_eq!(off.adjusted(WHITE, bg), WHITE);
        // 越界的阈值被夹到范围内。
        assert_eq!(ContrastGuard::new(99.0).ratio(), 21.0);
    }
}
