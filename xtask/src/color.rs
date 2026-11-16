//! 颜色解析与 WCAG 2.x 对比度计算，供 token 校验与生成共用。

/// 8 位 sRGB 颜色，带 alpha。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    /// 是否完全不透明。
    pub fn is_opaque(self) -> bool {
        self.a == 255
    }

    /// 解析 `#RRGGBB` 或 `#RRGGBBAA`（十六进制大小写均可）；其他写法返回 `None`。
    pub fn parse(s: &str) -> Option<Rgba> {
        let hex = s.strip_prefix('#')?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgba {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: if hex.len() == 8 { byte(6)? } else { 255 },
        })
    }

    /// 把本色叠在不透明背景 `bg` 上，返回合成后的不透明色。
    ///
    /// 在 sRGB 编码值上线性插值，与系统合成器对 8 位 sRGB 的默认混合方式一致；
    /// 对比度以用户实际看到的合成结果为准，而不是半透明色本身。
    pub fn over(self, bg: Rgba) -> Rgba {
        let a = f64::from(self.a) / 255.0;
        let mix = |f: u8, b: u8| (f64::from(f) * a + f64::from(b) * (1.0 - a)).round() as u8;
        Rgba {
            r: mix(self.r, bg.r),
            g: mix(self.g, bg.g),
            b: mix(self.b, bg.b),
            a: 255,
        }
    }
}

/// WCAG 2.x 相对亮度（忽略 alpha，调用方需先合成）。
pub fn relative_luminance(c: Rgba) -> f64 {
    // sRGB 线性化；阈值取 IEC 61966-2-1 的 0.04045（WCAG 早期文本里的 0.03928 差异可忽略）。
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// 两个不透明色之间的 WCAG 对比度，范围 1.0 到 21.0，与参数顺序无关。
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Rgba {
        Rgba::parse(s).unwrap()
    }

    #[test]
    fn black_white_is_21() {
        assert!((contrast_ratio(c("#000000"), c("#FFFFFF")) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(c("#FFFFFF"), c("#000000")) - 21.0).abs() < 1e-9);
    }

    #[test]
    fn same_color_is_1() {
        assert!((contrast_ratio(c("#336699"), c("#336699")) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn known_mid_value() {
        // #777777 对白色约 4.478:1（常见的“差一点到 AA”的灰）。
        assert!((contrast_ratio(c("#777777"), c("#FFFFFF")) - 4.478).abs() < 0.005);
    }

    #[test]
    fn parse_rules() {
        assert_eq!(Rgba::parse("#0a0B0c").unwrap().g, 0x0b);
        assert_eq!(Rgba::parse("#11223344").unwrap().a, 0x44);
        assert!(Rgba::parse("123456").is_none());
        assert!(Rgba::parse("#12345").is_none());
        assert!(Rgba::parse("#12345G").is_none());
        assert!(Rgba::parse("#+12345").is_none());
    }

    #[test]
    fn composite_half_alpha() {
        let out = c("#00000080").over(c("#FFFFFF"));
        assert_eq!((out.r, out.g, out.b, out.a), (127, 127, 127, 255));
    }
}
