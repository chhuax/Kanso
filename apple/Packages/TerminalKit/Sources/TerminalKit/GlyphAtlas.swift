import AppKit
import CoreText
import Metal
import os

/// 图集里一个字形的位置与摆放信息。
struct GlyphEntry {
    /// 所在页（在对应种类的页数组里的下标）。
    var page: Int
    var isColor: Bool
    /// 没有墨迹（空白字形）。这类字形也要缓存，否则每帧都会重新栅格化。
    var isEmpty: Bool
    /// 图集中的归一化纹理坐标 (u0, v0, u1, v1)，v 向下。
    var uv: SIMD4<Float>
    /// 位图左上角相对"单元格左上角"的偏移（设备像素，y 向下）。可以为负或超出单元格，
    /// 即字形允许溢出到相邻单元格。
    var offsetX: Int32
    var offsetY: Int32
    var width: Int32
    var height: Int32

    static let empty = GlyphEntry(
        page: 0, isColor: false, isEmpty: true, uv: .zero, offsetX: 0, offsetY: 0, width: 0, height: 0
    )
}

/// 图集键，单标量路径：把 (标量, 粗体, 斜体, 宽, 字面) 打包进一个整数，
/// 每帧每个单元格都要查表，必须避免为 ASCII 文本构造和哈希 `String`。
struct ScalarGlyphKey: Hashable {
    let bits: UInt64

    init(scalar: UInt32, bold: Bool, italic: Bool, wide: Bool, face: GlyphFace) {
        bits = UInt64(scalar) | (bold ? 1 << 32 : 0) | (italic ? 1 << 33 : 0) | (wide ? 1 << 34 : 0) | (face.rawValue << 35)
    }
}

/// 图集键，字形簇路径（组合符号、ZWJ emoji 等多标量文本）。
struct ClusterGlyphKey: Hashable {
    let text: String
    let bold: Bool
    let italic: Bool
    let wide: Bool
    let face: GlyphFace
}

/// 货架式装箱的一页。
///
/// 每个"货架"是一条水平带，高度由放入的第一个字形决定；后续字形只要不比货架高就依次向右放。
/// 终端字形高度高度集中（基本等于单元格高），货架利用率很高，实现也比通用矩形装箱简单。
private struct ShelfPage {
    let texture: MTLTexture
    let size: Int
    private var shelves: [(y: Int, height: Int, x: Int)] = []
    private var nextY = 0

    init(texture: MTLTexture, size: Int) {
        self.texture = texture
        self.size = size
    }

    /// 尝试放入 `w × h`，成功返回左上角像素坐标。
    mutating func allocate(width w: Int, height h: Int) -> (x: Int, y: Int)? {
        // 货架高度不超过需要的 1.5 倍才复用，避免一个小字形占用一条很高的货架。
        for i in shelves.indices where shelves[i].height >= h && shelves[i].height * 2 <= h * 3 {
            if shelves[i].x + w <= size {
                let pos = (shelves[i].x, shelves[i].y)
                shelves[i].x += w
                return pos
            }
        }
        guard nextY + h <= size, w <= size else { return nil }
        shelves.append((nextY, h, w))
        let pos = (0, nextY)
        nextY += h
        return pos
    }

    var usedHeight: Int { nextY }
}

/// 字形图集：用 CoreText 栅格化字形并装进 Metal 纹理。
///
/// - 灰度图集（`r8Unorm`）存覆盖率，着色器里乘前景色，所以同一个字形可以用任意前景色而不必重新栅格化。
///   彩色图集（`bgra8Unorm`，预乘 alpha）存 emoji 之类自带颜色的字形（architecture §6）。
///   两者分开是因为它们的像素格式、采样方式和混合公式都不同。
/// - 栅格化尺寸是设备像素，字形位图原点对齐整数像素，着色器用最近邻采样 1:1 贴到屏幕上，
///   所以不会产生重采样模糊。
/// - 满了就新开一页，每页一个纹理。本阶段不做淘汰：终端实际用到的不同字形一般只有几百到几千个，
///   一页 2048² 能放下大部分场景；占用量会写入调试日志，超出预期时再设计淘汰策略。
/// - 只在主线程使用。
final class GlyphAtlas {
    private let device: MTLDevice
    let fonts: TerminalFonts
    private var grayPages: [ShelfPage] = []
    private var colorPages: [ShelfPage] = []
    private var scalarCache: [ScalarGlyphKey: GlyphEntry] = [:]
    private var clusterCache: [ClusterGlyphKey: GlyphEntry] = [:]
    /// ASCII 快速表：面(2) x 样式(4) x 128，未栅格化的槽为 nil。
    private var asciiTable = [GlyphEntry?](repeating: nil, count: 2 * 4 * 128)
    /// 栅格化用的临时位图，反复复用以避免每个字形都分配。
    private var scratch = [UInt8]()
    private let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "glyph-atlas")

    /// 页边长上限；从 256² 开始按需翻倍，灰度 / 彩色单页最大均为 4 MiB。
    private static let grayPageSize = 2048
    private static let colorPageSize = 1024

    /// 已创建纹理占用的字节数（调试日志用）。
    private(set) var totalBytes = 0
    var grayPageCount: Int { grayPages.count }
    var colorPageCount: Int { colorPages.count }
    var glyphCount: Int { scalarCache.count + clusterCache.count }

    init(device: MTLDevice, fonts: TerminalFonts) {
        self.device = device
        self.fonts = fonts
    }

    func grayTexture(page: Int) -> MTLTexture { grayPages[page].texture }
    func colorTexture(page: Int) -> MTLTexture { colorPages[page].texture }

    // MARK: 查询

    func entry(scalar: UInt32, bold: Bool, italic: Bool, wide: Bool, face: GlyphFace) -> GlyphEntry {
        // ASCII 单格字符占绝大多数单元格：直接按下标查数组，省掉每个单元格一次字典哈希。
        if scalar < 128, !wide {
            let slot = ((Int(face.rawValue) * 4 + (bold ? 1 : 0) + (italic ? 2 : 0)) << 7) | Int(scalar)
            if let hit = asciiTable[slot] { return hit }
            let made = entrySlow(scalar: scalar, bold: bold, italic: italic, wide: wide, face: face)
            asciiTable[slot] = made
            return made
        }
        return entrySlow(scalar: scalar, bold: bold, italic: italic, wide: wide, face: face)
    }

    private func entrySlow(scalar: UInt32, bold: Bool, italic: Bool, wide: Bool, face: GlyphFace) -> GlyphEntry {
        let key = ScalarGlyphKey(scalar: scalar, bold: bold, italic: italic, wide: wide, face: face)
        if let hit = scalarCache[key] { return hit }
        let made: GlyphEntry
        if scalar == Self.branchIconScalar {
            made = rasterizeBranchIcon(face: face)
        } else {
            let text = Unicode.Scalar(scalar).map { String(Character($0)) } ?? ""
            made = rasterize(text: text, bold: bold, italic: italic, wide: wide, face: face)
        }
        scalarCache[key] = made
        logIfNeeded()
        return made
    }

    func entry(cluster: String, bold: Bool, italic: Bool, wide: Bool, face: GlyphFace) -> GlyphEntry {
        let key = ClusterGlyphKey(text: cluster, bold: bold, italic: italic, wide: wide, face: face)
        if let hit = clusterCache[key] { return hit }
        let made = rasterize(text: cluster, bold: bold, italic: italic, wide: wide, face: face)
        clusterCache[key] = made
        logIfNeeded()
        return made
    }

    // MARK: 栅格化

    private func rasterize(text: String, bold: Bool, italic: Bool, wide: Bool, face: GlyphFace) -> GlyphEntry {
        guard !text.isEmpty else { return .empty }
        let (font, isColor) = fonts.resolve(text: text, face: face, bold: bold, italic: italic)
        let metrics = (face == .terminal ? fonts.terminal : fonts.gutter).metrics

        // 关闭连字：终端按单元格对齐，连字会让相邻字符的位置错乱；连字是后期可选项（architecture §6）。
        let attributed = NSAttributedString(string: text, attributes: [
            NSAttributedString.Key(kCTFontAttributeName as String): font,
            NSAttributedString.Key(kCTLigatureAttributeName as String): 0,
            // 默认前景色是黑色，在"白字黑底"的覆盖率位图里什么都画不出来；必须声明使用上下文的填充色。
            NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String): true,
        ])
        let line = CTLineCreateWithAttributedString(attributed)
        let advance = CGFloat(CTLineGetTypographicBounds(line, nil, nil, nil))

        // 宽字符在两格宽里水平居中；取整到整数像素，让位图原点落在像素边界上。
        // 单格字符左对齐：等宽字体的前进宽度就是单元格宽度，回退字体较窄时贴左也符合终端习惯。
        let cellSpan = metrics.cellWidth * (wide ? 2 : 1)
        let dx = wide ? Int(((CGFloat(cellSpan) - advance) / 2).rounded()) : 0

        // 用墨迹包围盒确定位图大小：字形允许超出单元格（图标、emoji、斜体悬挑），不能按单元格裁掉。
        let bounds = Self.imageBounds(of: line, fallbackAdvance: advance, font: font, isColor: isColor)
        guard bounds.width > 0, bounds.height > 0 else { return .empty }
        // 四周各留 1 像素：抗锯齿的边缘像素可能落在包围盒之外。
        let left = Int(floor(bounds.minX)) - 1
        let bottom = Int(floor(bounds.minY)) - 1
        let right = Int(ceil(bounds.maxX)) + 1
        let top = Int(ceil(bounds.maxY)) + 1
        let w = right - left, h = top - bottom

        let bytesPerPixel = isColor ? 4 : 1
        let bytesPerRow = w * bytesPerPixel
        if scratch.count < bytesPerRow * h { scratch = [UInt8](repeating: 0, count: bytesPerRow * h) }
        for i in 0..<(bytesPerRow * h) { scratch[i] = 0 }

        let drew: Bool = scratch.withUnsafeMutableBytes { raw in
            let ctx: CGContext?
            if isColor {
                // BGRA 预乘，与 `bgra8Unorm` 纹理的内存布局一致，可以直接上传。
                ctx = CGContext(
                    data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: bytesPerRow,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                    bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
                )
            } else {
                ctx = CGContext(
                    data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: bytesPerRow,
                    space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue
                )
            }
            guard let ctx else { return false }
            // 关闭字体平滑：它会按"黑字白底"的假设加粗笔画，我们存的是覆盖率，加粗会让浅色文字发胖，
            // 且结果依赖系统的"字体平滑"偏好，不利于跨机器一致的截图测试。
            ctx.setShouldSmoothFonts(false)
            ctx.setShouldAntialias(true)
            ctx.setAllowsFontSubpixelPositioning(false)
            ctx.setAllowsFontSubpixelQuantization(false)
            ctx.setFillColor(gray: 1, alpha: 1)
            ctx.textPosition = CGPoint(x: -left, y: -bottom)
            CTLineDraw(line, ctx)
            return true
        }
        guard drew else { return .empty }

        guard let (page, x, y) = allocate(width: w, height: h, isColor: isColor) else { return .empty }
        let texture = isColor ? colorPages[page].texture : grayPages[page].texture
        scratch.withUnsafeBytes { raw in
            texture.replace(
                region: MTLRegionMake2D(x, y, w, h), mipmapLevel: 0,
                withBytes: raw.baseAddress!, bytesPerRow: bytesPerRow
            )
        }
        let size = Float(texture.width)
        return GlyphEntry(
            page: page, isColor: isColor, isEmpty: false,
            uv: SIMD4(Float(x) / size, Float(y) / size, Float(x + w) / size, Float(y + h) / size),
            offsetX: Int32(dx + left),
            // 位图顶边在基线上方 top 像素，基线在单元格顶部下方 baseline 像素。
            offsetY: Int32(metrics.baseline - top),
            width: Int32(w), height: Int32(h)
        )
    }

    // MARK: 内置图标

    /// Powerline 的分支符号（私有区 U+E0A0）。内置 zsh 提示符在分支名前输出它；
    /// 系统字体都没有这个字，所以不依赖 Nerd Font，由渲染器自己画（Lucide git-branch，ISC）。
    /// 装了 Nerd Font 也用内置图形，保证不同字体下观感一致。
    static let branchIconScalar: UInt32 = 0xE0A0

    /// 在一个单元格内画分支图标：边长取单元格宽，按 24×24 的 Lucide 坐标缩放，竖直方向居中在小写字母中线附近。
    private func rasterizeBranchIcon(face: GlyphFace) -> GlyphEntry {
        let metrics = (face == .terminal ? fonts.terminal : fonts.gutter).metrics
        let side = max(8, metrics.cellWidth)
        let w = side, h = side
        if scratch.count < w * h { scratch = [UInt8](repeating: 0, count: w * h) }
        for i in 0..<(w * h) { scratch[i] = 0 }
        let drew: Bool = scratch.withUnsafeMutableBytes { raw in
            guard let ctx = CGContext(
                data: raw.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w,
                space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue
            ) else { return false }
            // 转成与 SVG 相同的 y 向下坐标，再把 24 单位映射到位图边长。
            let k = CGFloat(side) / 24
            ctx.translateBy(x: 0, y: CGFloat(h))
            ctx.scaleBy(x: k, y: -k)
            ctx.setShouldAntialias(true)
            ctx.setStrokeColor(gray: 1, alpha: 1)
            // 线宽至少 1.2 像素：Lucide 原稿 2/24，小字号时会细到发虚。
            ctx.setLineWidth(max(2, 1.2 / k))
            ctx.setLineCap(.round)
            ctx.setLineJoin(.round)
            // M15 6 a9 9 0 0 0 -9 9 V3：圆心 (15,15) 的左上四分之一圆弧接主干竖线。
            ctx.move(to: CGPoint(x: 15, y: 6))
            ctx.addArc(center: CGPoint(x: 15, y: 15), radius: 9, startAngle: -.pi / 2, endAngle: .pi, clockwise: true)
            ctx.addLine(to: CGPoint(x: 6, y: 3))
            ctx.strokePath()
            ctx.strokeEllipse(in: CGRect(x: 15, y: 3, width: 6, height: 6))
            ctx.strokeEllipse(in: CGRect(x: 3, y: 15, width: 6, height: 6))
            return true
        }
        guard drew, let (page, x, y) = allocate(width: w, height: h, isColor: false) else { return .empty }
        let texture = grayPages[page].texture
        scratch.withUnsafeBytes { raw in
            texture.replace(region: MTLRegionMake2D(x, y, w, h), mipmapLevel: 0, withBytes: raw.baseAddress!, bytesPerRow: w)
        }
        let size = Float(texture.width)
        // 图标中心对齐小写字母高度的中线：大约基线上方 0.35 个单元格高。
        let centerY = metrics.baseline - Int((Double(metrics.cellHeight) * 0.3).rounded())
        return GlyphEntry(
            page: page, isColor: false, isEmpty: false,
            uv: SIMD4(Float(x) / size, Float(y) / size, Float(x + w) / size, Float(y + h) / size),
            offsetX: 0, offsetY: Int32(centerY - h / 2),
            width: Int32(w), height: Int32(h)
        )
    }

    /// 字形墨迹的包围盒（基线坐标系，y 向上）。
    private static func imageBounds(of line: CTLine, fallbackAdvance: CGFloat, font: CTFont, isColor: Bool) -> CGRect {
        // CTLineGetImageBounds 需要一个 CGContext 来确定变换；包围盒与上下文内容无关，用 1x1 的即可。
        let probe = CGContext(
            data: nil, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue
        )
        var rect = probe.map { CTLineGetImageBounds(line, $0) } ?? .null
        if (rect.isNull || rect.isEmpty) && isColor {
            // 位图字体（sbix emoji）的墨迹包围盒有时取不到，退化为排版盒。
            rect = CGRect(
                x: 0, y: -CTFontGetDescent(font), width: fallbackAdvance,
                height: CTFontGetAscent(font) + CTFontGetDescent(font)
            )
        }
        return rect.isNull ? .zero : rect
    }

    // MARK: 装箱

    private func allocate(width w: Int, height h: Int, isColor: Bool) -> (page: Int, x: Int, y: Int)? {
        let maximumSize = isColor ? Self.colorPageSize : Self.grayPageSize
        guard w <= maximumSize, h <= maximumSize else { return nil }
        if isColor {
            if let pos = Self.allocate(in: &colorPages, width: w, height: h) { return pos }
        } else if let pos = Self.allocate(in: &grayPages, width: w, height: h) { return pos }
        let previousSize = isColor ? colorPages.last?.size : grayPages.last?.size
        var size = min((previousSize ?? 128) * 2, maximumSize)
        while size < max(w, h) { size *= 2 }
        if isColor {
            colorPages.append(makePage(size: size, format: .bgra8Unorm, bytesPerPixel: 4, name: "color"))
            return Self.allocate(in: &colorPages, width: w, height: h)
        }
        grayPages.append(makePage(size: size, format: .r8Unorm, bytesPerPixel: 1, name: "gray"))
        return Self.allocate(in: &grayPages, width: w, height: h)
    }

    /// 只在最新一页里找位置：旧页的货架即使有零星空隙也不回头填，换取查找 O(1)。
    private static func allocate(in pages: inout [ShelfPage], width: Int, height: Int) -> (page: Int, x: Int, y: Int)? {
        guard let last = pages.indices.last, let pos = pages[last].allocate(width: width, height: height) else { return nil }
        return (last, pos.x, pos.y)
    }

    private func makePage(size: Int, format: MTLPixelFormat, bytesPerPixel: Int, name: String) -> ShelfPage {
        let desc = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: format, width: size, height: size, mipmapped: false)
        desc.usage = .shaderRead
        // Apple Silicon 统一内存：shared 存储下 CPU 的 replace 对 GPU 直接可见，无需 managed 同步。
        desc.storageMode = .shared
        guard let texture = device.makeTexture(descriptor: desc) else {
            fatalError("无法创建 \(size)x\(size) 的图集纹理")
        }
        texture.label = "GlyphAtlas.\(name)"
        totalBytes += size * size * bytesPerPixel
        log.debug("新增 \(name) 图集页：共 gray \(self.grayPages.count + (name == "gray" ? 1 : 0)) 页 / color \(self.colorPages.count + (name == "color" ? 1 : 0)) 页，\(self.totalBytes / 1024) KiB")
        return ShelfPage(texture: texture, size: size)
    }

    private func logIfNeeded() {
        let n = glyphCount
        if n.isMultiple(of: 256) {
            log.debug("图集字形数 \(n)，纹理占用 \(self.totalBytes / 1024) KiB（gray \(self.grayPages.count) 页 / color \(self.colorPages.count) 页）")
        }
    }

    /// 输出一行占用统计，供调试与测试使用。
    var statsDescription: String {
        "glyphs=\(glyphCount) grayPages=\(grayPages.count) colorPages=\(colorPages.count) bytes=\(totalBytes)"
    }
}

/// 同一 Metal 设备、同字体、同字号、同倍率、同行高的终端共用一份图集：
/// 字形位图只取决于这几项，每个标签各存一份是纯浪费（实测一个标签显示过常用汉字约 5 MiB）。
///
/// 只保存弱引用：最后一个使用它的渲染器释放（或换了字体/倍率）后图集随之释放，不会常驻。
/// 共用是安全的：图集只在主线程使用；新开页、页面翻倍都不改变已有字形的页号和坐标，
/// 所以一个渲染器新增字形不会让另一个渲染器已编码的实例失效。
@MainActor
enum GlyphAtlasCache {
    private struct Key: Hashable {
        let device: ObjectIdentifier
        let fontName: String
        let pointSize: CGFloat
        let scale: CGFloat
        let lineHeight: Double
    }

    private final class WeakAtlas {
        weak var atlas: GlyphAtlas?
        init(_ atlas: GlyphAtlas) { self.atlas = atlas }
    }

    private static var entries: [Key: WeakAtlas] = [:]

    /// 取（或新建）匹配的图集。调用方持有返回值的强引用，决定它的生命周期。
    static func atlas(device: MTLDevice, font: NSFont, scale: CGFloat, lineHeight: Double) -> GlyphAtlas {
        let key = Key(device: ObjectIdentifier(device), fontName: font.fontName, pointSize: font.pointSize,
                      scale: scale, lineHeight: lineHeight)
        if let hit = entries[key]?.atlas { return hit }
        // 顺手清掉已释放的条目，字典大小只和"当前同时在用的字体组合"数量有关。
        entries = entries.filter { $0.value.atlas != nil }
        let atlas = GlyphAtlas(device: device, fonts: TerminalFonts(base: font, scale: scale, lineHeight: lineHeight))
        entries[key] = WeakAtlas(atlas)
        return atlas
    }
}
