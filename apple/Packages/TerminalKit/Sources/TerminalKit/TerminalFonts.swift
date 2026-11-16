import AppKit
import CoreText

/// 字形所属的"字面"：终端正文与时间戳栏用不同字号，所以图集键里要区分。
enum GlyphFace: UInt64, Sendable {
    case terminal = 0
    case gutter = 1
}

/// 单元格尺寸与基线，全部是设备像素的整数。
///
/// 单元格宽高取整是为了让第 n 列的左边界恰好落在整数像素上：如果用浮点宽度累加，列越靠右
/// 误差越大，同一个字符在不同列的栅格化结果会因为亚像素偏移而不一样，滚动时字形会"抖"。
public struct TerminalMetrics: Equatable, Sendable {
    public let cellWidth: Int
    public let cellHeight: Int
    /// 基线到单元格顶部的距离。
    let baseline: Int
    /// 下划线顶边相对单元格顶部的位置（单线），已保证整条线落在单元格内。
    let underlineTop: Int
    /// 下划线 / 删除线的粗细。
    let lineThickness: Int
    /// 删除线顶边相对单元格顶部的位置。
    let strikeTop: Int
}

/// 一个字面（正文或栏）的字体集合与度量。
struct FontFace {
    let regular: CTFont
    let bold: CTFont
    let italic: CTFont
    let boldItalic: CTFont
    let metrics: TerminalMetrics
    /// 字体的 `NSFontDescriptor` 级别信息，用来在缺字形时构造回退字体的同字号副本。
    let pixelSize: CGFloat

    func font(bold: Bool, italic: Bool) -> CTFont {
        switch (bold, italic) {
        case (false, false): regular
        case (true, false): self.bold
        case (false, true): self.italic
        case (true, true): boldItalic
        }
    }
}

/// 字体解析：配置字体的四个变体、字号换算成像素、缺字形时的回退链。
///
/// 回退顺序（architecture §6、ui-shell §10）：配置的终端字体 → 已安装的 Nerd Font →
/// 系统回退（`CTFontCreateForString`，CJK 等）→ Apple Color Emoji。
final class TerminalFonts {
    let terminal: FontFace
    let gutter: FontFace
    /// 构造时的缩放倍率；倍率变化需要重新构造（字号换算成像素，图集也要重建）。
    let scale: CGFloat

    private let findNerdFamily: () -> String?
    private lazy var nerdFamily: String? = findNerdFamily()
    /// 字形覆盖判断。默认查 CoreText；测试注入，不依赖本机装了哪些字体。
    private let covers: (CTFont, Unicode.Scalar) -> Bool
    /// 回退字体缓存，键为 PostScript 名 + 变体 + 像素字号。
    private var fallbackCache: [String: CTFont] = [:]

    /// - Parameters:
    ///   - base: 终端字体；其 `pointSize` 是字号（pt）。
    ///   - scale: 窗口的 `backingScaleFactor`。字体按 `pointSize * scale` 像素栅格化，
    ///     所以 Retina 屏上字形是真正的 2x 栅格，而不是放大的 1x 位图。
    ///   - nerdFamily: 要用的 Nerd Font 家族名；默认查本机已安装的（结果缓存，只枚举一次）。
    ///     测试注入固定值，让"装了 / 没装 Nerd Font"两种情形都不依赖本机字体。
    ///   - covers: 判断字体是否含某个字符的字形；默认查 CoreText。
    init(
        base: NSFont, scale: CGFloat, lineHeight: Double = 1.2,
        nerdFamily: @autoclosure @escaping () -> String? = TerminalFonts.installedNerdFamily,
        covers: @escaping (CTFont, Unicode.Scalar) -> Bool = TerminalFonts.coverage
    ) {
        self.scale = scale
        self.covers = covers
        let size = max(1, base.pointSize * scale)
        terminal = Self.makeFace(base: base, pixelSize: size, lineHeight: lineHeight)
        // 栏字号略小于正文，让它在视觉上退到背景里；用系统等宽字体，不受用户终端字体影响，
        // 保证 `HH:MM:SS.mmm` 的宽度固定。
        let gutterBase = NSFont.monospacedSystemFont(ofSize: max(9, (base.pointSize * 0.85).rounded()), weight: .regular)
        gutter = Self.makeFace(base: gutterBase, pixelSize: gutterBase.pointSize * scale, lineHeight: 1)
        self.findNerdFamily = nerdFamily
    }

    // MARK: 构造

    private static func makeFace(base: NSFont, pixelSize: CGFloat, lineHeight: Double) -> FontFace {
        let manager = NSFontManager.shared
        func variant(_ traits: NSFontTraitMask) -> NSFont {
            traits.isEmpty ? base : manager.convert(base, toHaveTrait: traits)
        }
        func ct(_ f: NSFont) -> CTFont {
            CTFontCreateCopyWithAttributes(f as CTFont, pixelSize, nil, nil)
        }
        let regular = ct(base)
        let bold = ct(variant(.boldFontMask))
        var italic = ct(variant(.italicFontMask))
        var boldItalic = ct(variant([.boldFontMask, .italicFontMask]))
        // 字体没有斜体变体时 NSFontManager 返回原字体；用 12° 倾斜矩阵合成斜体，
        // 否则 SGR 3 完全看不出来。
        if !CTFontGetSymbolicTraits(italic).contains(.traitItalic) {
            var skew = CGAffineTransform(a: 1, b: 0, c: 0.2126, d: 1, tx: 0, ty: 0)
            italic = CTFontCreateCopyWithAttributes(italic, pixelSize, &skew, nil)
        }
        if !CTFontGetSymbolicTraits(boldItalic).contains(.traitItalic) {
            var skew = CGAffineTransform(a: 1, b: 0, c: 0.2126, d: 1, tx: 0, ty: 0)
            boldItalic = CTFontCreateCopyWithAttributes(boldItalic, pixelSize, &skew, nil)
        }
        return FontFace(
            regular: regular, bold: bold, italic: italic, boldItalic: boldItalic,
            metrics: metrics(for: regular, lineHeight: lineHeight), pixelSize: pixelSize
        )
    }

    private static func metrics(for font: CTFont, lineHeight: Double) -> TerminalMetrics {
        // 取 "M" 的前进宽度作为单元格宽度；等宽字体所有字符前进宽度相同。
        var glyph = CGGlyph()
        var m: UniChar = 0x4D
        CTFontGetGlyphsForCharacters(font, &m, &glyph, 1)
        var advance = CGSize.zero
        CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, &advance, 1)
        let cellWidth = max(1, Int(advance.width.rounded()))

        let ascent = Int(CTFontGetAscent(font).rounded(.up))
        let descent = Int(CTFontGetDescent(font).rounded(.up))
        let leading = Int(CTFontGetLeading(font).rounded(.up))
        let naturalHeight = ascent + descent + leading
        let cellHeight = max(1, Int((Double(naturalHeight) * lineHeight).rounded()))
        // 行距的一半放在上面，一半放在下面，使字形在单元格里垂直居中。
        let baseline = ascent + (leading + cellHeight - naturalHeight) / 2

        let thickness = max(1, Int(CTFontGetUnderlineThickness(font).rounded()))
        // CTFontGetUnderlinePosition 是基线下方的负偏移；要整条线都落在单元格里，否则会被下一行的背景盖住。
        let underlineOffset = max(1, Int((-CTFontGetUnderlinePosition(font)).rounded()))
        let underlineTop = min(baseline + underlineOffset, cellHeight - thickness)
        let strikeTop = max(0, baseline - Int((CTFontGetXHeight(font) / 2).rounded()) - thickness / 2)
        return TerminalMetrics(
            cellWidth: cellWidth, cellHeight: cellHeight, baseline: baseline,
            underlineTop: max(0, underlineTop), lineThickness: thickness, strikeTop: strikeTop
        )
    }

    // MARK: Nerd Font

    /// 已安装的 Nerd Font 家族名（按名字包含 "Nerd Font" 查找，优先选带 "Mono" 的等宽变体，
    /// 它的图标字形正好一格宽）。没有则为 nil，回退链跳过这一环。
    /// 枚举全部字体家族有一定开销，所以只在第一次需要时做一次（字体延迟加载，design §6）。
    static let installedNerdFamily: String? = {
        let families = NSFontManager.shared.availableFontFamilies.filter { $0.contains("Nerd Font") }
        return families.sorted().first { $0.contains("Nerd Font Mono") } ?? families.sorted().first
    }()

    // MARK: 回退

    /// 为一段文本选择字体：返回字体与是否为彩色字体（emoji）。
    ///
    /// 只检查第一个标量是否被字体覆盖：字形簇后面的组合符号、ZWJ、变体选择符由 CoreText 排版时处理，
    /// 能不能显示取决于基础字符所在的字体。
    func resolve(text: String, face: GlyphFace, bold: Bool, italic: Bool) -> (font: CTFont, isColor: Bool) {
        let fontFace = face == .terminal ? terminal : gutter
        let primary = fontFace.font(bold: bold, italic: italic)
        guard let first = text.unicodeScalars.first else { return (primary, false) }

        // emoji 呈现的字符直接走 emoji 字体：等宽字体里没有这些码点，让它们先经过 Nerd Font / 系统回退
        // 可能被 Apple Symbols 等黑白字体抢走，丢掉彩色。
        let wantsEmoji = first.properties.isEmojiPresentation || text.unicodeScalars.contains { $0.value == 0xFE0F }
        if wantsEmoji, let emoji = emojiFont(pixelSize: fontFace.pixelSize) {
            return (emoji, true)
        }
        if covers(primary, first) { return (primary, false) }

        if let nerdFamily, let nerd = fallbackFont(family: nerdFamily, bold: bold, pixelSize: fontFace.pixelSize),
           covers(nerd, first) {
            return (nerd, false)
        }
        // 系统回退：CoreText 按字符与用户语言偏好挑选（CJK 字体、符号字体等）。
        let ns = text as NSString
        let substitute = CTFontCreateForString(primary, ns, CFRange(location: 0, length: ns.length))
        if covers(substitute, first) {
            return (substitute, CTFontGetSymbolicTraits(substitute).contains(.traitColorGlyphs))
        }
        if let emoji = emojiFont(pixelSize: fontFace.pixelSize), covers(emoji, first) {
            return (emoji, true)
        }
        // 谁也没有这个字符：交给主字体，CoreText 会画出 .notdef（豆腐块），至少能看出缺字。
        return (primary, false)
    }

    private func fallbackFont(family: String, bold: Bool, pixelSize: CGFloat) -> CTFont? {
        let key = "\(family)|\(bold)|\(pixelSize)"
        if let cached = fallbackCache[key] { return cached }
        let descriptor = CTFontDescriptorCreateWithAttributes([
            kCTFontFamilyNameAttribute: family,
            kCTFontTraitsAttribute: [kCTFontWeightTrait: bold ? 0.4 : 0.0],
        ] as CFDictionary)
        let font = CTFontCreateWithFontDescriptor(descriptor, pixelSize, nil)
        fallbackCache[key] = font
        return font
    }

    private func emojiFont(pixelSize: CGFloat) -> CTFont? {
        let key = "emoji|\(pixelSize)"
        if let cached = fallbackCache[key] { return cached }
        let font = CTFontCreateWithName("Apple Color Emoji" as CFString, pixelSize, nil)
        fallbackCache[key] = font
        return font
    }

    static func coverage(_ font: CTFont, _ scalar: Unicode.Scalar) -> Bool {
        var utf16 = Array(String(scalar).utf16)
        var glyphs = [CGGlyph](repeating: 0, count: utf16.count)
        // 非 BMP 标量是代理对：返回值在代理对上不可靠，只看第一个字形是否非 0。
        _ = CTFontGetGlyphsForCharacters(font, &utf16, &glyphs, utf16.count)
        return glyphs[0] != 0
    }
}
