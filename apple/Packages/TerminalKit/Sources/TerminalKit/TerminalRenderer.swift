import AppKit
import CoreText
import KansoBridge
import Metal
import QuartzCore
import os

/// 时间戳与行号栏的显示模式（terminal.md §2）。
public enum GutterMode: Sendable, CaseIterable {
    case off
    case lineNumbers
    case timestamps
    case both
}

/// 一次渲染的 CPU 侧耗时与规模，供性能测试与调试使用。
public struct RenderStats: Sendable {
    /// 背景 / 字形 / 装饰三层的实例总数。
    public var instanceCount = 0
    /// 生成实例数据（遍历单元格、查图集、写缓冲区）的耗时，秒。
    public var encodeSeconds = 0.0
    /// 编码命令并提交的耗时，秒；不含等待 GPU。
    public var submitSeconds = 0.0
    public var totalSeconds: Double { encodeSeconds + submitSeconds }
}

/// 网格尺寸（单元格数）与单元格像素尺寸，上层据此通知 core 调整 PTY 大小。
public struct TerminalGridSize: Equatable, Sendable {
    public var cols: Int
    public var rows: Int
    public var cellWidthPx: Int
    public var cellHeightPx: Int
}

/// Metal 渲染管线，与 `NSView` 解耦：既可以画到 `CAMetalLayer` 的 drawable，也可以离屏渲染成 `CGImage`。
///
/// 三层实例化绘制（architecture §6），同一个 render pass 内按顺序提交：
/// 1. **背景**：每个单元格一个色块（相邻同色在行内合并），连同时间戳栏的光标行高亮；
/// 2. **字形**：每个字形一个四边形，采样灰度 / 彩色图集；
/// 3. **装饰**：下划线五种样式、删除线、非块状光标、命令分隔线。
/// 背景层整体先于字形层绘制：字形允许溢出到相邻单元格，后画的背景不能把它盖掉，这就是分层的意义。
/// 选区与搜索高亮会改变单元格的最终背景色，所以在 CPU 解析颜色时直接并入背景层，而不是作为半透明叠层画在字形之上
/// （叠在字形上会把文字染色，也会让最低对比度保护算错背景）。
///
/// **颜色空间**：渲染目标用非 sRGB 的 `bgra8Unorm`，所有颜色以 sRGB 编码值直接写入并在编码空间混合，
/// 图层声明为 sRGB。原因：设计 token 与终端配色给的就是 sRGB 十六进制值，必须逐值还原；
/// CoreText 与 AppKit 的文字抗锯齿也是在编码空间混合的，用线性空间混合（`bgra8Unorm_srgb`）
/// 会让深色背景上的浅色文字显得偏粗、浅色背景上的深色文字偏细，与系统其他文字观感不一致。
///
/// **像素对齐**：所有几何都以整数设备像素计算（单元格宽高取整、栏宽取整），一像素的线（分隔线、下划线、
/// 竖线光标）的边缘天然落在像素边界上，不会被抗锯齿成两条淡线（terminal.md §4）。
///
/// 只在主线程使用。
@MainActor
public final class TerminalRenderer {
    // MARK: 配置

    /// 终端字体，`pointSize` 即字号。改变后图集清空并在下次渲染时按新字体重排。
    public var font: NSFont = .monospacedSystemFont(ofSize: 14, weight: .regular) {
        didSet { invalidateFonts() }
    }

    public var lineHeight: Double = 1.2 {
        didSet { invalidateFonts() }
    }

    /// 时间戳与行号栏模式。栏宽随模式变化，调用方需要重新计算网格尺寸（`gridSize`）。
    public var gutterMode: GutterMode = .both

    /// 时间戳显示用的时区。默认本地时区；测试里固定为 UTC，保证参考截图与机器无关。
    public var timeZone: TimeZone = .current

    /// 最低对比度保护开关（默认开启，architecture §11）。
    public var isMinimumContrastEnabled = true {
        didSet { contrast.resetCache() }
    }

    /// 最低对比度阈值（WCAG 对比度，默认 4.5）。
    public var minimumContrastRatio: Double = 4.5 {
        didSet {
            contrast.minimumRatio = minimumContrastRatio
            contrast.resetCache()
        }
    }

    /// 浅色外观使用的配色（默认 Kanso Light）。切换后下一次渲染生效；调用方负责请求重绘。
    public var lightPalette: TerminalPalette = .kansoLight
    /// 深色外观使用的配色（默认 Kanso Dark）。
    public var darkPalette: TerminalPalette = .kansoDark

    /// 光标当前是否处于"显示"相位。闪烁由视图的计时器翻转它；帧里光标不可见时无论如何不画。
    /// 只影响绘制，不改帧：闪烁不需要向 core 取新帧。
    public var isCursorPhaseVisible = true
    var selectedCommandBlock: UInt64?

    /// 顶部让出的高度（pt）。全尺寸内容视图下标题栏会盖住顶部，由视图按 `safeAreaInsets.top` 设置。
    public var topInset: CGFloat = 0

    /// 最近一次渲染的统计。
    public private(set) var lastStats = RenderStats()

    /// 着色器编译与管线创建耗时（秒）。
    public var shaderCompileSeconds: Double { resources.shaderCompileSeconds }

    /// 图集占用的调试描述。
    public var atlasStatsDescription: String { atlas?.statsDescription ?? "atlas 未创建" }

    // MARK: 内部状态

    let device: MTLDevice
    private let resources: MetalResources
    private var fonts: TerminalFonts?
    private var atlas: GlyphAtlas?
    private var contrast = ContrastGuard(minimumRatio: 4.5)

    private let signposter = OSSignposter(
        subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "terminal-render"
    )

    // 三份实例缓冲轮流使用：CPU 在写第 n+1 帧的数据时，GPU 可能还在读第 n 帧的，
    // 必须各用各的缓冲区；信号量保证同时在途的帧不超过缓冲份数。
    private static let framesInFlight = 3
    private var instanceBuffers = [MTLBuffer?](repeating: nil, count: framesInFlight)
    private var bufferIndex = 0
    private let inFlight = DispatchSemaphore(value: framesInFlight)

    // 每帧复用的 CPU 侧实例数组，`removeAll(keepingCapacity:)` 避免反复分配。
    private var bgInstances: [RectInstance] = []
    private var decoInstances: [RectInstance] = []
    private var grayInstances: [[GlyphInstance]] = []
    private var colorInstances: [[GlyphInstance]] = []

    // MARK: 初始化

    /// 无 Metal 设备或着色器编译失败时返回 nil。
    public init?(device: MTLDevice? = MTLCreateSystemDefaultDevice()) {
        guard let device, let resources = MetalResources.shared(for: device) else { return nil }
        self.device = device
        self.resources = resources
    }

    // MARK: 度量与布局

    private func invalidateFonts() {
        fonts = nil
        atlas = nil
    }

    private func prepare(scale: CGFloat) -> (fonts: TerminalFonts, atlas: GlyphAtlas) {
        if let fonts, let atlas, fonts.scale == scale { return (fonts, atlas) }
        // 倍率变化（窗口移到不同 DPI 的屏幕）时字形必须按新像素字号重新栅格化，换用对应倍率的图集。
        // 图集按字体/倍率/行高在所有终端间共用，见 `GlyphAtlasCache`。
        let newAtlas = GlyphAtlasCache.atlas(device: device, font: font, scale: scale, lineHeight: lineHeight)
        fonts = newAtlas.fonts
        atlas = newAtlas
        return (newAtlas.fonts, newAtlas)
    }

    /// 单元格的设备像素尺寸。
    public func metrics(scale: CGFloat) -> TerminalMetrics {
        prepare(scale: scale).fonts.terminal.metrics
    }

    /// 布局常量（设备像素）。
    struct Layout {
        let metrics: TerminalMetrics
        let gutterMetrics: TerminalMetrics
        /// 栏内字符格宽。
        let gutterCharWidth: Int
        /// 栏总宽（含左侧留白与右侧与正文的间隔）；栏关闭时为 0。
        let gutterWidth: Int
        /// 正文区上下的留白。
        let padding: Int
        /// 正文区左右的留白：比上下宽，文字不贴着窗口边或时间栏（2026-11-16 用户反馈"左边一点边距都没有"）。
        let paddingX: Int
        let originX: Int
        let originY: Int
    }

    /// 栏内字符数：时间 `HH:MM:SS.mmm` 12 个，行号字段 6 个，两者之间 1 个空格。
    private static let timeChars = 12
    private static let numberChars = 6

    private func gutterChars() -> Int {
        switch gutterMode {
        case .off: 0
        case .lineNumbers: Self.numberChars
        case .timestamps: Self.timeChars
        case .both: Self.timeChars + 1 + Self.numberChars
        }
    }

    func layout(scale: CGFloat) -> Layout {
        let (fonts, _) = prepare(scale: scale)
        let chars = gutterChars()
        let gcw = fonts.gutter.metrics.cellWidth
        // 左侧 1 个字符的留白，右侧 2 个字符的间隔，把栏与正文隔开。
        let gutterWidth = chars == 0 ? 0 : (chars + 3) * gcw
        let padding = Int((4 * scale).rounded())
        let paddingX = Int((12 * scale).rounded())
        return Layout(
            metrics: fonts.terminal.metrics, gutterMetrics: fonts.gutter.metrics,
            gutterCharWidth: gcw, gutterWidth: gutterWidth, padding: padding, paddingX: paddingX,
            originX: gutterWidth + paddingX,
            originY: Int((topInset * scale).rounded()) + padding
        )
    }

    /// 给定视图大小（pt）下能放下的网格。
    public func gridSize(for size: CGSize, scale: CGFloat) -> TerminalGridSize {
        let l = layout(scale: scale)
        let widthPx = Int((size.width * scale).rounded())
        let heightPx = Int((size.height * scale).rounded())
        return TerminalGridSize(
            cols: max(1, (widthPx - l.originX - l.paddingX) / l.metrics.cellWidth),
            rows: max(1, (heightPx - l.originY - l.padding) / l.metrics.cellHeight),
            cellWidthPx: l.metrics.cellWidth,
            cellHeightPx: l.metrics.cellHeight
        )
    }

    // MARK: 渲染入口

    /// 渲染到 `CAMetalLayer` 的 drawable 并呈现。`frame` 为 nil 时只清成背景色。
    public func render(
        frame: FrameSnapshot?, to drawable: CAMetalDrawable, scale: CGFloat, appearance: TerminalAppearance
    ) {
        draw(frame: frame, target: drawable.texture, scale: scale, appearance: appearance, present: drawable, wait: false)
    }

    /// 离屏渲染成图片（截图与截图测试用）。`size` 为 pt，输出像素为 `size * scale`。
    ///
    /// 同步等待 GPU 完成并读回，结果与屏幕上显示的像素一致（同一条管线，同一份图集）。
    /// 帧按它自己的列数、行数从左上角排布，不会按 `size` 重新布局。
    public func renderImage(
        frame: FrameSnapshot, size: CGSize, scale: CGFloat, appearance: TerminalAppearance
    ) -> CGImage? {
        let width = Int((size.width * scale).rounded()), height = Int((size.height * scale).rounded())
        guard width > 0, height > 0 else { return nil }
        let desc = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: MetalResources.colorPixelFormat, width: width, height: height, mipmapped: false
        )
        desc.usage = .renderTarget
        // 统一内存用 shared；独显用 managed，渲染后需 blit synchronize 才能让 CPU 读到。
        desc.storageMode = device.cpuAccessTextureStorageMode
        guard let texture = device.makeTexture(descriptor: desc) else { return nil }
        guard draw(
            frame: frame, target: texture, scale: scale, appearance: appearance, present: nil, wait: true,
            readback: desc.storageMode == .managed
        ) else {
            return nil
        }

        let bytesPerRow = width * 4
        var pixels = Data(count: bytesPerRow * height)
        pixels.withUnsafeMutableBytes { raw in
            texture.getBytes(raw.baseAddress!, bytesPerRow: bytesPerRow, from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0)
        }
        guard let provider = CGDataProvider(data: pixels as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: bytesPerRow,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            // 纹理是 BGRA，alpha 恒为 1：声明为"跳过 alpha 的首字节"小端，与内存布局一致。
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
        )
    }

    // MARK: 命令提交

    @discardableResult
    private func draw(
        frame: FrameSnapshot?, target: MTLTexture, scale: CGFloat, appearance: TerminalAppearance,
        present drawable: CAMetalDrawable?, wait: Bool, readback: Bool = false
    ) -> Bool {
        let (_, atlas) = prepare(scale: scale)
        let palette = appearance == .light ? lightPalette : darkPalette

        // 等到有空闲的缓冲份数再开始，等待时间不计入 CPU 编码耗时。
        inFlight.wait()
        var committed = false
        defer { if !committed { inFlight.signal() } }

        let t0 = DispatchTime.now().uptimeNanoseconds
        let encodeState = signposter.beginInterval("EncodeInstances")
        if let frame {
            encode(frame: frame, palette: palette, layout: layout(scale: scale), scale: scale, atlas: atlas, target: target)
        } else {
            clearInstances()
        }
        let layoutBuffers = uploadInstances()
        signposter.endInterval("EncodeInstances", encodeState)
        let t1 = DispatchTime.now().uptimeNanoseconds

        guard let buffer = layoutBuffers.buffer, let commandBuffer = resources.queue.makeCommandBuffer() else {
            return false
        }
        let submitState = signposter.beginInterval("SubmitCommandBuffer")
        let pass = MTLRenderPassDescriptor()
        let color = pass.colorAttachments[0]!
        color.texture = target
        color.loadAction = .clear
        color.storeAction = .store
        color.clearColor = MTLClearColor(
            red: Double(palette.background.r) / 255, green: Double(palette.background.g) / 255,
            blue: Double(palette.background.b) / 255, alpha: 1
        )
        guard let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: pass) else {
            signposter.endInterval("SubmitCommandBuffer", submitState)
            return false
        }
        var viewport = SIMD2<Float>(Float(target.width), Float(target.height))
        encoder.setVertexBytes(&viewport, length: MemoryLayout<SIMD2<Float>>.size, index: 1)

        func drawRects(_ range: (offset: Int, count: Int)) {
            guard range.count > 0 else { return }
            encoder.setRenderPipelineState(resources.rectPipeline)
            encoder.setVertexBuffer(buffer, offset: range.offset, index: 0)
            encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: range.count)
        }
        func drawGlyphs(_ ranges: [(offset: Int, count: Int)], color isColor: Bool) {
            for (page, range) in ranges.enumerated() where range.count > 0 {
                encoder.setRenderPipelineState(isColor ? resources.colorGlyphPipeline : resources.grayGlyphPipeline)
                encoder.setFragmentTexture(isColor ? atlas.colorTexture(page: page) : atlas.grayTexture(page: page), index: 0)
                encoder.setVertexBuffer(buffer, offset: range.offset, index: 0)
                encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: range.count)
            }
        }
        drawRects(layoutBuffers.background)
        drawGlyphs(layoutBuffers.gray, color: false)
        drawGlyphs(layoutBuffers.color, color: true)
        drawRects(layoutBuffers.decoration)
        encoder.endEncoding()

        if readback, let blit = commandBuffer.makeBlitCommandEncoder() {
            // managed 纹理：把 GPU 渲染结果同步回 CPU 副本，之后 getBytes 才读得到。
            blit.synchronize(resource: target)
            blit.endEncoding()
        }

        if let drawable { commandBuffer.present(drawable) }
        let semaphore = inFlight
        // Metal 在自己的队列上执行完成回调。
        commandBuffer.addCompletedHandler { @Sendable _ in semaphore.signal() }
        commandBuffer.commit()
        committed = true
        signposter.endInterval("SubmitCommandBuffer", submitState)
        let t2 = DispatchTime.now().uptimeNanoseconds

        lastStats = RenderStats(
            instanceCount: layoutBuffers.total,
            encodeSeconds: Double(t1 - t0) / 1e9,
            submitSeconds: Double(t2 - t1) / 1e9
        )
        if wait { commandBuffer.waitUntilCompleted() }
        return commandBuffer.error == nil || !wait
    }

    private struct BufferLayout {
        var buffer: MTLBuffer?
        var background: (offset: Int, count: Int) = (0, 0)
        var gray: [(offset: Int, count: Int)] = []
        var color: [(offset: Int, count: Int)] = []
        var decoration: (offset: Int, count: Int) = (0, 0)
        var total = 0
    }

    /// 把 CPU 侧的实例数组按"背景 | 灰度字形各页 | 彩色字形各页 | 装饰"顺序拷进本帧的缓冲区。
    /// 一次 memcpy 即可：实例结构体是纯值类型，布局与着色器一致。
    private func uploadInstances() -> BufferLayout {
        let stride = MemoryLayout<RectInstance>.stride
        precondition(stride == MemoryLayout<GlyphInstance>.stride && stride == 48, "实例结构体布局与着色器不一致")
        let glyphCount = grayInstances.reduce(0) { $0 + $1.count } + colorInstances.reduce(0) { $0 + $1.count }
        let total = bgInstances.count + decoInstances.count + glyphCount
        var out = BufferLayout()
        out.total = total

        bufferIndex = (bufferIndex + 1) % Self.framesInFlight
        let needed = max(total * stride, stride)
        var buffer = instanceBuffers[bufferIndex]
        if buffer == nil || buffer!.length < needed {
            // 按 1.5 倍增长，避免窗口缓慢拉大时每次都重新分配。
            buffer = device.makeBuffer(length: needed + needed / 2, options: .storageModeShared)
            buffer?.label = "TerminalRenderer.instances.\(bufferIndex)"
            instanceBuffers[bufferIndex] = buffer
        }
        guard let buffer else { return out }
        out.buffer = buffer

        var offset = 0
        func copy<T>(_ array: [T]) -> (offset: Int, count: Int) {
            let range = (offset: offset, count: array.count)
            guard !array.isEmpty else { return range }
            array.withUnsafeBytes { raw in
                (buffer.contents() + offset).copyMemory(from: raw.baseAddress!, byteCount: raw.count)
            }
            offset += raw(array)
            return range
        }
        func raw<T>(_ array: [T]) -> Int { array.count * MemoryLayout<T>.stride }
        out.background = copy(bgInstances)
        out.gray = grayInstances.map { copy($0) }
        out.color = colorInstances.map { copy($0) }
        out.decoration = copy(decoInstances)
        return out
    }

    private func clearInstances() {
        bgInstances.removeAll(keepingCapacity: true)
        decoInstances.removeAll(keepingCapacity: true)
        for i in grayInstances.indices { grayInstances[i].removeAll(keepingCapacity: true) }
        for i in colorInstances.indices { colorInstances[i].removeAll(keepingCapacity: true) }
    }

    // MARK: 实例生成

    private static func vec(_ c: RGB8, alpha: Float = 1) -> SIMD4<Float> {
        SIMD4(Float(c.r) / 255, Float(c.g) / 255, Float(c.b) / 255, alpha)
    }

    private static func vec(_ c: RGBA8) -> SIMD4<Float> {
        SIMD4(Float(c.r) / 255, Float(c.g) / 255, Float(c.b) / 255, Float(c.a) / 255)
    }

    private func rect(_ x: Int, _ y: Int, _ w: Int, _ h: Int, _ color: SIMD4<Float>, style: Float = 0,
                      params: SIMD4<Float> = .zero) -> RectInstance {
        RectInstance(
            rect: SIMD4(Float(x), Float(y), Float(w), Float(h)), color: color,
            params: SIMD4(style, params.y, params.z, params.w)
        )
    }

    private func addGlyph(_ entry: GlyphEntry, x: Int, y: Int, color: SIMD4<Float>) {
        guard !entry.isEmpty else { return }
        let instance = GlyphInstance(
            rect: SIMD4(Float(x), Float(y), Float(entry.width), Float(entry.height)),
            uv: entry.uv, color: color
        )
        if entry.isColor {
            while colorInstances.count <= entry.page { colorInstances.append([]) }
            colorInstances[entry.page].append(instance)
        } else {
            while grayInstances.count <= entry.page { grayInstances.append([]) }
            grayInstances[entry.page].append(instance)
        }
    }

    /// 私有区里的 Powerline / Nerd Font 图标。历史提示符不画，避免依赖特殊字体。
    private static func isSpecialSymbol(_ ch: UInt32) -> Bool {
        (0xE000...0xF8FF).contains(ch) || (0xF0000...0xFFFFD).contains(ch) || (0x100000...0x10FFFD).contains(ch)
    }

    /// 已经执行过的提示符行（命令行和输出不算）。当前待输入的块除外。
    private func historicalPrompt(frame: FrameSnapshot, row: Int) -> Bool {
        let blocks = frame.commandBlocks
        guard !blocks.isEmpty else { return false }
        let line = frame.lineMeta(row: row).absLine
        let live = blocks.first { $0.latest && !$0.running && $0.commandLine == nil }
        return blocks.contains { block in
            if let live, block.id == live.id { return false }
            if let command = block.commandLine {
                return block.startLine <= line && line < command
            }
            return block.startLine <= line && line <= block.endLine
        }
    }

    /// 最新且尚未执行命令的块才是待输入提示符。没有命令块时无法区分，全部按当前提示符画。
    private func chipEmphasized(frame: FrameSnapshot, row: Int) -> Bool {
        let blocks = frame.commandBlocks
        guard !blocks.isEmpty else { return true }
        guard let live = blocks.first(where: { $0.latest && !$0.running && $0.commandLine == nil }) else {
            return false
        }
        let line = frame.lineMeta(row: row).absLine
        return live.startLine <= line && line <= live.endLine
    }

    private func encode(
        frame: FrameSnapshot, palette: TerminalPalette, layout l: Layout, scale: CGFloat, atlas: GlyphAtlas,
        target: MTLTexture
    ) {
        clearInstances()
        let m = l.metrics
        let altScreen = frame.flags.contains(.altScreen)
        // 只生成目标范围内可见的行列：窗口缩小后、core 尚未响应 resize 之前，帧可能比视图大。
        let cols = min(frame.cols, max(0, (target.width - l.originX) / m.cellWidth + 1))
        let rows = min(frame.rows, max(0, (target.height - l.originY) / m.cellHeight + 1))
        let cursor = frame.flags.contains(.cursorVisible) && isCursorPhaseVisible ? frame.cursor : nil
        let background = palette.background.packed
        let stroke = max(1, Int(scale.rounded()))
        var cursorWide = false

        if !altScreen, let block = frame.commandBlocks.first(where: { $0.id == selectedCommandBlock }),
           block.canCopyOutput {
            for row in 0..<rows {
                let line = frame.lineMeta(row: row).absLine
                if block.startLine <= line && line <= block.endLine {
                    bgInstances.append(rect(l.gutterWidth, l.originY + row * m.cellHeight,
                        target.width - l.gutterWidth, m.cellHeight, Self.vec(palette.blockSelection)))
                }
            }
        }

        // 时间戳栏与正文之间的竖线：贯穿整个视图高度，而不是只画到有内容的行——
        // 栏下方大片空白时，没有这条线就看不出栏的边界。全屏程序期间栏留空，线照样保留，
        // 让版面不跳动。线宽与命令分隔线一致，取同一个低对比度颜色。
        if l.gutterWidth > 0 {
            decoInstances.append(rect(l.gutterWidth, 0, stroke, target.height, Self.vec(palette.commandSeparator)))
        }

        for block in frame.commandBlocks where !altScreen && rows > 0 && (block.running || (block.exitCode.map { $0 != 0 } ?? false)) {
            let first = frame.lineMeta(row: 0).absLine
            let start = Int(max(block.startLine, first) - first)
            let end = Int(min(block.endLine, first + UInt64(rows - 1)) - first)
            if end >= start {
                let color = block.running ? palette.cursor : palette.ansi[1]
                decoInstances.append(rect(l.originX - l.paddingX / 2, l.originY + start * m.cellHeight,
                    2, (end - start + 1) * m.cellHeight, Self.vec(color)))
            }
        }

        for row in 0..<rows {
            let rowY = l.originY + row * m.cellHeight
            let meta = frame.lineMeta(row: row)
            let rowEmphasized = chipEmphasized(frame: frame, row: row)
            let rowQuiet = historicalPrompt(frame: frame, row: row)

            if !altScreen {
                // 命令分隔线：提示符行上方 1 设备像素，横跨整个视图（包括栏）。同一行只画一条由 core 去重保证。
                if meta.flags.contains(.prompt), !meta.flags.contains(.firstPrompt) {
                    decoInstances.append(rect(0, rowY, target.width, 1, Self.vec(palette.commandSeparator)))
                }
                if l.gutterWidth > 0 {
                    encodeGutter(frame: frame, meta: meta, rowY: rowY, palette: palette, layout: l, atlas: atlas)
                }
            }

            var runColor: UInt32 = background
            var runStart = 0
            func flushRun(upTo end: Int) {
                guard runColor != background, end > runStart else { return }
                bgInstances.append(rect(
                    l.originX + runStart * m.cellWidth, rowY, (end - runStart) * m.cellWidth, m.cellHeight,
                    Self.vec(RGB8(hex: runColor))
                ))
            }

            var col = 0
            while col < cols {
                let cell = frame.cell(row: row, col: col)
                // 宽字符：前半格带 WIDE，或者后一格是占位格（有的生产者不给前半格打 WIDE，见 mock）。
                var wide = cell.flags.contains(.wide)
                if col + 1 < frame.cols, wide || cell.ch > 0x7F, !cell.flags.contains(.wideSpacer) {
                    if frame.cell(row: row, col: col + 1).flags.contains(.wideSpacer) { wide = true }
                }
                if col + 1 >= frame.cols { wide = false }
                let span = wide ? 2 : 1

                if let cursor, cursor.row == row, cursor.col == col { cursorWide = wide }

                // ---- 颜色解析 ----
                let bold = cell.flags.contains(.bold)
                var fg = palette.resolveForeground(cell.fg, fallback: palette.foreground, bold: bold)
                var bg = palette.resolve(cell.bg, fallback: palette.background)
                if cell.flags.contains(.inverse) { swap(&fg, &bg) }
                if cell.flags.contains(.selected) {
                    bg = palette.selectionBackground
                } else if cell.flags.contains(.matchCurrent) {
                    // 搜索高亮：用光标强调色按不同强度混进背景，当前匹配项更醒目。
                    bg = bg.mixed(with: palette.cursor, 0.7)
                } else if cell.flags.contains(.match) {
                    bg = bg.mixed(with: palette.cursor, 0.35)
                }
                let isBlockCursor = cursor.map { $0.row == row && $0.col == col && $0.shape == .block } ?? false
                if isBlockCursor {
                    // 光标与其上字符的配色由主题成对给出，保证可读，不再做对比度调整。
                    bg = palette.cursor
                    fg = palette.cursorText
                } else if cell.flags.contains(.chip), GlyphAtlas.isContextIcon(cell.ch) {
                    // 标签内的内置图标统一用 token 的次要灰，保持 token 原值，不参与最低对比度调整。
                    fg = RGB8(palette.chipIcon.r, palette.chipIcon.g, palette.chipIcon.b)
                } else if isMinimumContrastEnabled, !GlyphAtlas.isPowerlineGeometric(cell.ch) {
                    // Powerline 分隔符的前景色必须与相邻色块的背景色一致才能无缝拼接，不做对比度调整。
                    // 提示符标签里的字实际画在标签底色上（默认背景时），按标签底色算对比度。
                    let chipBg = RGB8(palette.chipBackground.r, palette.chipBackground.g, palette.chipBackground.b)
                    let shown = cell.flags.contains(.chip) && bg == palette.background ? chipBg : bg
                    fg = contrast.adjusted(foreground: fg, background: shown)
                }
                // dim 在对比度保护之后应用：SGR 2 的本意就是降低强调度，不应被保护逻辑抵消。
                var glyphFg = fg
                if cell.flags.contains(.dim) { glyphFg = fg.mixed(with: bg, 0.4) }
                let hasHighlight = cell.flags.contains(.selected)
                    || cell.flags.contains(.matchCurrent)
                    || cell.flags.contains(.match)
                if rowQuiet, !isBlockCursor, !hasHighlight {
                    glyphFg = RGB8(palette.gutterText.r, palette.gutterText.g, palette.gutterText.b)
                }

                // ---- 背景（行内同色合并）----
                // 默认背景色等于清屏色，不需要实例；其余颜色按行内连续的同色段合并成一个色块。
                // 宽字符占两格，后半格（占位格）整体跳过，背景由前半格的这个 run 覆盖。
                if bg.packed != runColor {
                    flushRun(upTo: col)
                    runColor = bg.packed
                    runStart = col
                }

                let x = l.originX + col * m.cellWidth
                let cellWidthPx = m.cellWidth * span

                // ---- 字形 ----
                if !cell.flags.contains(.hidden), !(rowQuiet && Self.isSpecialSymbol(cell.ch)) {
                    var entry: GlyphEntry?
                    // 历史提示符去掉目录粗体；当前待输入的提示符保留（字形弱化，不影响颜色提亮的 bold）。
                    let glyphBold = cell.flags.contains(.bold) && !rowQuiet
                        && !(cell.flags.contains(.chip) && !rowEmphasized)
                    let italic = cell.flags.contains(.italic)
                    if cell.extra != 0, let cluster = frame.grapheme(for: cell) {
                        entry = atlas.entry(cluster: cluster, bold: glyphBold, italic: italic, wide: wide, face: .terminal)
                    } else if cell.ch > 0x20, cell.ch != 0x7F {
                        entry = atlas.entry(scalar: cell.ch, bold: glyphBold, italic: italic, wide: wide, face: .terminal)
                    }
                    if let entry {
                        let color = entry.isColor
                            ? SIMD4<Float>(1, 1, 1, (cell.flags.contains(.dim) || rowQuiet) ? 0.6 : 1)
                            : Self.vec(glyphFg)
                        addGlyph(entry, x: x + Int(entry.offsetX), y: rowY + Int(entry.offsetY), color: color)
                    }
                }

                // ---- 装饰：下划线与删除线 ----
                if cell.underlineStyle != 0 {
                    let underline = frame.underlineColor(for: cell).map { palette.resolve($0, fallback: fg) } ?? glyphFg
                    encodeUnderline(
                        style: cell.underlineStyle, x: x, rowY: rowY, width: cellWidthPx, metrics: m,
                        color: Self.vec(underline)
                    )
                }
                if cell.flags.contains(.strike) {
                    decoInstances.append(rect(x, rowY + m.strikeTop, cellWidthPx, m.lineThickness, Self.vec(glyphFg)))
                }
                col += span
            }
            flushRun(upTo: cols)

            // 标签在单元格背景之上、字形之下；不外扩，描边宽度固定为一个设备像素。
            var chipCol = 0
            while chipCol < cols {
                guard frame.cell(row: row, col: chipCol).flags.contains(.chip) else {
                    chipCol += 1
                    continue
                }
                let start = chipCol
                repeat { chipCol += 1 } while chipCol < cols && frame.cell(row: row, col: chipCol).flags.contains(.chip)
                // 内边距由 shim 在标签首尾放的空格提供，这里按单元格原样画、不外扩，框左边与光标列对齐。
                // 上下只收 1 像素：标签尽量高，文字与边框之间留出上下边距，又不与相邻行的框贴死。
                let inset = max(1, stroke)
                let x = l.originX + start * m.cellWidth
                let right = l.originX + chipCol * m.cellWidth
                let y = rowY + inset
                let height = m.cellHeight - inset * 2
                // 约 3 逻辑像素。按半个字宽会收成胶囊，在黑底上显得突兀。
                let radius = Float(min(height / 2, max(2 * stroke, Int((3.0 * scale).rounded()))))
                if rowEmphasized {
                    bgInstances.append(rect(x, y, right - x, height, Self.vec(palette.chipBorder), style: 4,
                                            params: SIMD4(0, radius, 0, 0)))
                    bgInstances.append(rect(x + 1, y + 1, right - x - 2, height - 2, Self.vec(palette.chipBackground), style: 4,
                                            params: SIMD4(0, max(0, radius - 1), 0, 0)))
                }
            }
        }

        // ---- 非块状光标 ----
        if let cursor, cursor.shape != .block, cursor.row < rows, cursor.col < cols {
            let x = l.originX + cursor.col * m.cellWidth
            let y = l.originY + cursor.row * m.cellHeight
            let color = Self.vec(palette.cursor)
            let w = m.cellWidth * (cursorWide ? 2 : 1)
            switch cursor.shape {
            case .underline:
                decoInstances.append(rect(x, y + m.cellHeight - stroke, w, stroke, color))
            case .bar:
                // 竖线约一个字母高、宽约 1/4 格，两端圆角（style 4 圆角矩形，半径取半宽）。
                let barWidth = max(2 * stroke, (m.cellWidth + 2) / 4)
                decoInstances.append(rect(x, y + m.barTop, barWidth, m.barBottom - m.barTop, color, style: 4,
                                          params: SIMD4(0, Float(barWidth) / 2, 0, 0)))
            case .hollowBlock:
                // 四条互不重叠的边，半透明主题色下重叠处不会变深。
                decoInstances.append(rect(x, y, w, stroke, color))
                decoInstances.append(rect(x, y + m.cellHeight - stroke, w, stroke, color))
                decoInstances.append(rect(x, y + stroke, stroke, m.cellHeight - 2 * stroke, color))
                decoInstances.append(rect(x + w - stroke, y + stroke, stroke, m.cellHeight - 2 * stroke, color))
            case .block:
                break
            }
        }
    }
}

// MARK: - 栏与装饰

extension TerminalRenderer {
    /// 一行的时间戳与行号（terminal.md §2）。调用方保证非全屏程序期间才调用：全屏期间整列留空。
    fileprivate func encodeGutter(
        frame: FrameSnapshot, meta: FrameLineMeta, rowY: Int, palette: TerminalPalette, layout l: Layout,
        atlas: GlyphAtlas
    ) {
        if meta.flags.contains(.cursor) {
            bgInstances.append(rect(0, rowY, l.gutterWidth, l.metrics.cellHeight, Self.vec(palette.gutterCursorLine)))
        }
        // 空白行（还没写过内容）不显示时间和行号。
        guard !meta.flags.contains(.blank) else { return }

        var text = [UInt8]()
        text.reserveCapacity(Self.timeChars + 1 + Self.numberChars)
        let showTime = gutterMode == .timestamps || gutterMode == .both
        let showNumber = gutterMode == .lineNumbers || gutterMode == .both
        if showTime { appendTime(ms: meta.timestampMs, to: &text) }
        if showTime && showNumber { text.append(0x20) }
        if showNumber {
            // 用有符号数相减，避免 absLine 小于 base 时无符号下溢崩溃。
            let number = Int64(bitPattern: meta.absLine) - Int64(bitPattern: frame.lineNumberBase) + 1
            appendNumber(number, width: Self.numberChars, to: &text)
        }

        let color = Self.vec(palette.gutterText)
        // 栏字体比正文小，行内垂直居中；字形偏移是相对栏字面自己的单元格顶部算的。
        let top = rowY + (l.metrics.cellHeight - l.gutterMetrics.cellHeight) / 2
        for (i, byte) in text.enumerated() where byte != 0x20 {
            let entry = atlas.entry(scalar: UInt32(byte), bold: false, italic: false, wide: false, face: .gutter)
            addGlyph(
                entry, x: (1 + i) * l.gutterCharWidth + Int(entry.offsetX), y: top + Int(entry.offsetY), color: color
            )
        }
    }

    /// `HH:MM:SS.mmm`，不随语言变化（architecture §10）。手工拆分而不用 `DateFormatter`：
    /// 后者的区域设置可能改变数字形态，而这一栏要求固定格式；这里也是热路径，不能每行分配格式化对象。
    /// `ms == 0` 表示没有时间，留空。
    private func appendTime(ms: Int64, to out: inout [UInt8]) {
        guard ms != 0 else {
            out.append(contentsOf: repeatElement(0x20, count: Self.timeChars))
            return
        }
        let seconds = Self.floorDiv(ms, 1000)
        let millis = Int(ms - seconds * 1000)
        // 按该时刻的 UTC 偏移换算，夏令时切换期间的行也能显示对应的本地时间。
        let offset = Int64(timeZone.secondsFromGMT(for: Date(timeIntervalSince1970: TimeInterval(seconds))))
        let local = seconds + offset
        let secondOfDay = Int(local - Self.floorDiv(local, 86400) * 86400)
        let (h, m, s) = (secondOfDay / 3600, secondOfDay / 60 % 60, secondOfDay % 60)
        func two(_ v: Int) {
            out.append(UInt8(0x30 + v / 10))
            out.append(UInt8(0x30 + v % 10))
        }
        two(h); out.append(0x3A); two(m); out.append(0x3A); two(s); out.append(0x2E)
        out.append(UInt8(0x30 + millis / 100))
        out.append(UInt8(0x30 + millis / 10 % 10))
        out.append(UInt8(0x30 + millis % 10))
    }

    /// 右对齐的行号。超过字段宽度时只保留低位：栏宽是固定的，宁可截掉高位也不能挤进正文区。
    private func appendNumber(_ value: Int64, width: Int, to out: inout [UInt8]) {
        var digits = [UInt8]()
        var v = value < 0 ? 0 : value
        repeat {
            digits.append(UInt8(0x30 + v % 10))
            v /= 10
        } while v > 0 && digits.count < width
        out.append(contentsOf: repeatElement(0x20, count: width - digits.count))
        out.append(contentsOf: digits.reversed())
    }

    /// 向负无穷取整的除法，保证毫秒余数与一天内的秒数始终非负。
    private static func floorDiv(_ a: Int64, _ d: Int64) -> Int64 {
        let q = a / d
        return (a % d != 0 && (a < 0) != (d < 0)) ? q - 1 : q
    }

    /// 五种下划线样式（docs/frame-abi.md：1 单线、2 双线、3 波浪、4 点线、5 虚线）。
    /// 几何都是整数像素：单线 / 双线用实心矩形，波浪 / 点线 / 虚线用着色器按像素列判定。
    fileprivate func encodeUnderline(
        style: UInt8, x: Int, rowY: Int, width: Int, metrics m: TerminalMetrics, color: SIMD4<Float>
    ) {
        let t = m.lineThickness
        switch style {
        case 1:
            decoInstances.append(rect(x, rowY + m.underlineTop, width, t, color))
        case 2:
            // 两条线靠单元格底部排列，间隔一个线宽，保证整体不越出单元格被下一行背景盖住。
            let lower = m.cellHeight - t
            let upper = max(0, lower - 2 * t)
            decoInstances.append(rect(x, rowY + upper, width, t, color))
            decoInstances.append(rect(x, rowY + lower, width, t, color))
        case 3:
            let h = min(m.cellHeight, 3 * t)
            decoInstances.append(rect(
                x, rowY + m.cellHeight - h, width, h, color,
                style: 1, params: SIMD4(0, Float(t), 0, Float(m.cellWidth))
            ))
        case 4:
            decoInstances.append(rect(
                x, rowY + m.underlineTop, width, t, color,
                style: 2, params: SIMD4(0, Float(t), Float(t), Float(2 * t))
            ))
        case 5:
            let on = max(2, m.cellWidth * 6 / 10)
            decoInstances.append(rect(
                x, rowY + m.underlineTop, width, t, color,
                style: 3, params: SIMD4(0, Float(t), Float(on), Float(m.cellWidth))
            ))
        default:
            break
        }
    }
}
