import Foundation

/// 最低对比度保护（architecture §11）：前景色与最终背景色的 WCAG 2.x 对比度低于阈值时，
/// 调整前景色的亮度直到达标，色相保持不变。
///
/// 典型场景：程序按深色背景设计，输出 ANSI 白 / 亮白，在浅色主题下几乎看不见。
/// 语义着色色与调色板已由 `check-tokens` 保证对比度，所以这里主要作用于程序输出的索引色与 RGB 色。
public struct ContrastGuard {
    /// 目标对比度；小于等于 1 等价于关闭。
    public var minimumRatio: Double

    /// 8 位 sRGB 分量到线性光的查表，避免每个单元格重复调用 `pow`。
    private static let linearTable: [Double] = (0..<256).map { i in
        let c = Double(i) / 255
        return c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
    }

    /// WCAG 相对亮度。
    public static func luminance(_ c: RGB8) -> Double {
        0.2126 * linearTable[Int(c.r)] + 0.7152 * linearTable[Int(c.g)] + 0.0722 * linearTable[Int(c.b)]
    }

    /// 两个颜色的 WCAG 2.x 对比度（1 到 21）。
    public static func ratio(_ a: RGB8, _ b: RGB8) -> Double {
        let (la, lb) = (luminance(a), luminance(b))
        return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
    }

    /// 直接映射缓存：同一屏上的前景 / 背景组合很少，命中率接近 100%，命中时只需一次比较。
    /// 容量取 2 的幂以便用掩码取下标；冲突时直接覆盖，因为重新计算只是二分查找，成本有限。
    private var cacheKeys = [UInt64](repeating: .max, count: 1024)
    private var cacheValues = [UInt32](repeating: 0, count: 1024)

    public init(minimumRatio: Double) {
        self.minimumRatio = minimumRatio
    }

    /// 返回满足对比度的前景色；已经达标或保护关闭时原样返回。
    public mutating func adjusted(foreground fg: RGB8, background bg: RGB8) -> RGB8 {
        guard minimumRatio > 1 else { return fg }
        let key = UInt64(fg.packed) << 24 | UInt64(bg.packed)
        // 乘一个奇数常量再取高位，避免相邻颜色挤在同一个槽里。
        let slot = Int((key &* 0x9E37_79B9_7F4A_7C15) >> 54)
        if cacheKeys[slot] == key {
            let v = cacheValues[slot]
            return RGB8(hex: v)
        }
        let result = Self.adjust(fg, against: bg, target: minimumRatio)
        cacheKeys[slot] = key
        cacheValues[slot] = result.packed
        return result
    }

    /// 阈值或主题改变后必须清缓存，否则会返回按旧阈值算出的颜色。
    public mutating func resetCache() {
        for i in cacheKeys.indices { cacheKeys[i] = .max }
    }

    private static func adjust(_ fg: RGB8, against bg: RGB8, target: Double) -> RGB8 {
        if ratio(fg, bg) >= target { return fg }
        let (h, s, l) = hsl(fg)
        // 优先沿"远离背景亮度"的方向调整，改动最小、观感最自然；该方向到顶仍不达标（背景是中灰时可能发生）
        // 才尝试反方向。
        let lighterFirst = luminance(fg) >= luminance(bg)
        for lighter in [lighterFirst, !lighterFirst] {
            let extreme = fromHSL(h, s, lighter ? 1 : 0)
            guard ratio(extreme, bg) >= target else { continue }
            // 二分查找满足阈值的最接近原亮度的 L。在 8 位量化后的颜色上判断，保证返回值真的达标。
            var (lo, hi) = lighter ? (l, 1.0) : (0.0, l)
            for _ in 0..<14 {
                let mid = (lo + hi) / 2
                let ok = ratio(fromHSL(h, s, mid), bg) >= target
                // lighter 时 L 越大越亮，对比度（对更暗的背景）单调上升；darker 时相反。
                if ok == lighter { hi = mid } else { lo = mid }
            }
            return fromHSL(h, s, lighter ? hi : lo)
        }
        // 两个方向都无法达标：取对比度更高的黑或白，尽力而为。
        let (black, white) = (RGB8(0, 0, 0), RGB8(255, 255, 255))
        return ratio(black, bg) >= ratio(white, bg) ? black : white
    }

    // MARK: HSL 转换

    private static func hsl(_ c: RGB8) -> (h: Double, s: Double, l: Double) {
        let r = Double(c.r) / 255, g = Double(c.g) / 255, b = Double(c.b) / 255
        let mx = max(r, g, b), mn = min(r, g, b)
        let l = (mx + mn) / 2
        let d = mx - mn
        guard d > 0 else { return (0, 0, l) }
        let s = l > 0.5 ? d / (2 - mx - mn) : d / (mx + mn)
        var h: Double
        if mx == r {
            h = (g - b) / d + (g < b ? 6 : 0)
        } else if mx == g {
            h = (b - r) / d + 2
        } else {
            h = (r - g) / d + 4
        }
        h /= 6
        return (h, s, l)
    }

    private static func fromHSL(_ h: Double, _ s: Double, _ l: Double) -> RGB8 {
        func channel(_ t0: Double, _ p: Double, _ q: Double) -> UInt8 {
            var t = t0
            if t < 0 { t += 1 }
            if t > 1 { t -= 1 }
            let v: Double
            if t < 1.0 / 6 {
                v = p + (q - p) * 6 * t
            } else if t < 0.5 {
                v = q
            } else if t < 2.0 / 3 {
                v = p + (q - p) * (2.0 / 3 - t) * 6
            } else {
                v = p
            }
            return UInt8(max(0, min(255, (v * 255).rounded())))
        }
        guard s > 0 else {
            let v = UInt8(max(0, min(255, (l * 255).rounded())))
            return RGB8(v, v, v)
        }
        let q = l < 0.5 ? l * (1 + s) : l + s - l * s
        let p = 2 * l - q
        return RGB8(channel(h + 1.0 / 3, p, q), channel(h, p, q), channel(h - 1.0 / 3, p, q))
    }
}
