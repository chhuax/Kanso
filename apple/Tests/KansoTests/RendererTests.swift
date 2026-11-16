import AppKit
import KansoBridge
import Metal
@testable import TerminalKit
import XCTest

/// 渲染器的行为测试（不依赖参考图）。
@MainActor
final class RendererTests: XCTestCase {
    func testPromptChipBordersStayInsideTaggedRow() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        renderer.gutterMode = .off
        let size = CGSize(width: 900, height: 720)
        for scale: CGFloat in [1, 2] {
            let layout = renderer.layout(scale: scale)
            let grid = renderer.gridSize(for: size, scale: scale)
            let frame = try XCTUnwrap(FrameSnapshot(coreFrame: FrameHandle.mockSemanticColors(cols: UInt16(grid.cols), rows: UInt16(grid.rows))))
            XCTAssertTrue(frame.cell(row: 0, col: 1).flags.contains(.chip))
            XCTAssertFalse(frame.cell(row: 1, col: 0).flags.contains(.chip))
            for appearance: TerminalAppearance in [.light, .dark] {
                let palette = appearance.palette
                let border = palette.chipBorder
                let image = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: scale, appearance: appearance))
                let pixels = try SnapshotTests.pixels(image)
                let inset = max(2, layout.metrics.cellHeight / 10)
                for row in [0, 1] {
                    let y = layout.originY + row * layout.metrics.cellHeight + inset
                    let count = (layout.originX..<image.width).filter { x in
                        let offset = (y * image.width + x) * 4
                        return pixels[offset] == border.r && pixels[offset + 1] == border.g && pixels[offset + 2] == border.b
                    }.count
                    if row == 0 { XCTAssertGreaterThan(count, layout.metrics.cellWidth * 10) }
                    else { XCTAssertEqual(count, 0) }
                }
                let background = RGB8(palette.chipBackground.r, palette.chipBackground.g, palette.chipBackground.b)
                for foreground in [palette.foreground, palette.ansi[2]] {
                    XCTAssertGreaterThanOrEqual(ContrastGuard.ratio(foreground, background), 4.5)
                }
            }
        }
    }

    func testContextIconsFitOneCellAndUseMonochromeAtlas() throws {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        for scale: CGFloat in [1, 2] {
            let fonts = TerminalFonts(base: .monospacedSystemFont(ofSize: 13, weight: .regular), scale: scale)
            let atlas = GlyphAtlas(device: device, fonts: fonts)
            for scalar in [GlyphAtlas.branchIconScalar, GlyphAtlas.folderIconScalar, GlyphAtlas.nodeIconScalar] {
                let entry = atlas.entry(scalar: scalar, bold: false, italic: false, wide: false, face: .terminal)
                XCTAssertFalse(entry.isEmpty)
                XCTAssertFalse(entry.isColor)
                XCTAssertLessThanOrEqual(Int(entry.width), fonts.terminal.metrics.cellWidth)
            }
        }
    }

    func testAtlasGrowsByPageAndKeepsExistingGlyphCoordinates() throws {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        let fonts = TerminalFonts(base: .monospacedSystemFont(ofSize: 13, weight: .regular), scale: 2)
        let atlas = GlyphAtlas(device: device, fonts: fonts)
        XCTAssertEqual(atlas.totalBytes, 0)
        let first = atlas.entry(scalar: 65, bold: false, italic: false, wide: false, face: .terminal)
        XCTAssertFalse(first.isEmpty)
        XCTAssertEqual(atlas.grayTexture(page: 0).width, 256)
        XCTAssertEqual(atlas.totalBytes, 256 * 256)
        for bold in [false, true] {
            for italic in [false, true] {
                for scalar in UInt32(33)...126 {
                    _ = atlas.entry(scalar: scalar, bold: bold, italic: italic, wide: false, face: .terminal)
                }
            }
        }
        XCTAssertGreaterThan(atlas.grayPageCount, 1)
        XCTAssertEqual(atlas.grayTexture(page: 1).width, 512)
        let cached = atlas.entry(scalar: 65, bold: false, italic: false, wide: false, face: .terminal)
        XCTAssertEqual(cached.page, first.page)
        XCTAssertEqual(cached.uv, first.uv)
        XCTAssertEqual(first.uv.z - first.uv.x, Float(first.width) / 256, accuracy: 0.00001)
    }

    func testAtlasIsSharedBySameFontAndScaleOnly() throws {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        let font = NSFont.monospacedSystemFont(ofSize: 15, weight: .regular)
        var first: GlyphAtlas? = GlyphAtlasCache.atlas(device: device, font: font, scale: 2, lineHeight: 1.2)
        let same = GlyphAtlasCache.atlas(device: device, font: font, scale: 2, lineHeight: 1.2)
        XCTAssertTrue(first === same, "同字体同倍率共用一份")
        XCTAssertFalse(first === GlyphAtlasCache.atlas(device: device, font: font, scale: 1, lineHeight: 1.2))
        XCTAssertFalse(first === GlyphAtlasCache.atlas(device: device, font: font, scale: 2, lineHeight: 1.5))
        let bigger = NSFont.monospacedSystemFont(ofSize: 16, weight: .regular)
        XCTAssertFalse(first === GlyphAtlasCache.atlas(device: device, font: bigger, scale: 2, lineHeight: 1.2))
        // 只持弱引用：没人用了就释放，下次重新建。
        weak var released = first
        first = nil
        _ = same
        XCTAssertNotNil(released, "还有 same 持有")
    }

    func testAtlasCacheDoesNotKeepUnusedAtlasAlive() throws {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        let font = NSFont.monospacedSystemFont(ofSize: 17, weight: .regular)
        weak var released: GlyphAtlas?
        autoreleasepool {
            released = GlyphAtlasCache.atlas(device: device, font: font, scale: 2, lineHeight: 1.2)
        }
        XCTAssertNil(released)
    }

    func testAtlasFitsLargeGlyphOnItsFirstPage() throws {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        let fonts = TerminalFonts(base: .monospacedSystemFont(ofSize: 220, weight: .regular), scale: 2)
        let atlas = GlyphAtlas(device: device, fonts: fonts)
        let glyph = atlas.entry(scalar: 87, bold: false, italic: false, wide: false, face: .terminal)
        XCTAssertFalse(glyph.isEmpty)
        let texture = atlas.grayTexture(page: glyph.page)
        XCTAssertGreaterThan(texture.width, 256)
        XCTAssertGreaterThanOrEqual(texture.width, Int(glyph.width))
        XCTAssertGreaterThanOrEqual(texture.height, Int(glyph.height))
        XCTAssertEqual(glyph.uv.z - glyph.uv.x, Float(glyph.width) / Float(texture.width), accuracy: 0.00001)
    }

    func testEverySemanticThemeColorMeetsContrast() {
        for theme in TerminalTheme.builtIn {
            XCTAssertEqual(theme.palette.semantic.count, 13)
            for color in theme.palette.semantic {
                XCTAssertGreaterThanOrEqual(ContrastGuard.ratio(color, theme.palette.background), 4.5, theme.id)
            }
        }
    }
    func testMetricsAreIntegralAndScaleWithBackingFactor() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        let m1 = renderer.metrics(scale: 1)
        let m2 = renderer.metrics(scale: 2)
        XCTAssertGreaterThan(m1.cellWidth, 0)
        XCTAssertGreaterThan(m1.cellHeight, m1.cellWidth)
        // 2x 的单元格应该接近 1x 的两倍（各自取整，允许 ±1 像素）。
        XCTAssertLessThanOrEqual(abs(m2.cellWidth - 2 * m1.cellWidth), 2)
        XCTAssertLessThanOrEqual(abs(m2.cellHeight - 2 * m1.cellHeight), 2)
    }

    func testLineHeightCentersGlyphsAndUpdatesGrid() throws {
        for scale: CGFloat in [1, 2] {
            let renderer = try XCTUnwrap(TerminalRenderer())
            renderer.lineHeight = 1
            let natural = renderer.metrics(scale: scale)
            let size = CGSize(width: 800, height: 400)
            let before = renderer.gridSize(for: size, scale: scale)
            renderer.lineHeight = 1.5
            let spaced = renderer.metrics(scale: scale)
            let after = renderer.gridSize(for: size, scale: scale)
            XCTAssertEqual(spaced.cellWidth, natural.cellWidth)
            XCTAssertEqual(spaced.cellHeight, Int((Double(natural.cellHeight) * 1.5).rounded()))
            let padding = (spaced.cellHeight - natural.cellHeight) / 2
            XCTAssertEqual(spaced.baseline - natural.baseline, padding)
            XCTAssertEqual(spaced.underlineTop - natural.underlineTop, padding)
            XCTAssertEqual(spaced.strikeTop - natural.strikeTop, padding)
            XCTAssertEqual(after.cols, before.cols)
            XCTAssertLessThan(after.rows, before.rows)
            XCTAssertEqual(after.cellHeightPx, spaced.cellHeight)
        }
    }

    func testGutterModeChangesColumnCount() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        let size = CGSize(width: 800, height: 400)
        var cols: [GutterMode: Int] = [:]
        for mode in GutterMode.allCases {
            renderer.gutterMode = mode
            cols[mode] = renderer.gridSize(for: size, scale: 2).cols
        }
        XCTAssertGreaterThan(cols[.off]!, cols[.lineNumbers]!)
        XCTAssertGreaterThan(cols[.lineNumbers]!, cols[.timestamps]!)
        XCTAssertGreaterThan(cols[.timestamps]!, cols[.both]!)
    }

    func testFontChangeRebuildsMetrics() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        let before = renderer.metrics(scale: 2)
        renderer.font = .monospacedSystemFont(ofSize: 20, weight: .regular)
        let after = renderer.metrics(scale: 2)
        XCTAssertGreaterThan(after.cellWidth, before.cellWidth)
        XCTAssertGreaterThan(after.cellHeight, before.cellHeight)
    }

    func testRenderImageProducesRequestedPixelSize() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        let grid = renderer.gridSize(for: CGSize(width: 400, height: 200), scale: 2)
        let frame = try XCTUnwrap(FrameSnapshot.mock(cols: grid.cols, rows: grid.rows, seq: 0))
        let image = try XCTUnwrap(
            renderer.renderImage(frame: frame, size: CGSize(width: 400, height: 200), scale: 2, appearance: .dark)
        )
        XCTAssertEqual(image.width, 800)
        XCTAssertEqual(image.height, 400)
        // 渲染过的图集应该有字形（日志里的占用统计也来自这里）。
        XCTAssertTrue(renderer.atlasStatsDescription.contains("glyphs="), renderer.atlasStatsDescription)
        XCTAssertGreaterThan(renderer.lastStats.instanceCount, 0)
    }

    func testMockFrameSourceOnlyReturnsFrameWhenDirty() throws {
        let source = MockFrameSource(cols: 20, rows: 5)
        XCTAssertNotNil(source.takeFrame(reusing: nil), "初始状态为脏")
        XCTAssertNil(source.takeFrame(reusing: nil), "取走后应为干净")
        source.advance()
        let frame = try XCTUnwrap(source.takeFrame(reusing: nil))
        XCTAssertEqual(frame.seq, 1)
        XCTAssertNil(source.takeFrame(reusing: nil))
    }

    // MARK: 最低对比度保护

    func testContrastGuardFixesWhiteOnLightBackground() {
        var guardrail = ContrastGuard(minimumRatio: 4.5)
        let lightBackground = TerminalPalette.kansoLight.background
        // 浅色主题下的 ANSI 亮白（15 号色）几乎看不见。
        let white = TerminalPalette.kansoLight.ansi[15]
        XCTAssertLessThan(ContrastGuard.ratio(white, lightBackground), 4.5)
        let fixed = guardrail.adjusted(foreground: white, background: lightBackground)
        XCTAssertGreaterThanOrEqual(ContrastGuard.ratio(fixed, lightBackground), 4.5)
    }

    func testContrastGuardLeavesCompliantColorsAlone() {
        var guardrail = ContrastGuard(minimumRatio: 4.5)
        let p = TerminalPalette.kansoDark
        XCTAssertEqual(guardrail.adjusted(foreground: p.foreground, background: p.background), p.foreground)
    }

    func testContrastGuardPreservesHue() {
        var guardrail = ContrastGuard(minimumRatio: 4.5)
        // 浅黄色在白底上不可读；调整后仍应是黄色系（红、绿分量明显大于蓝）。
        let fixed = guardrail.adjusted(foreground: RGB8(255, 235, 120), background: RGB8(255, 255, 255))
        XCTAssertGreaterThanOrEqual(ContrastGuard.ratio(fixed, RGB8(255, 255, 255)), 4.5)
        XCTAssertGreaterThan(Int(fixed.r), Int(fixed.b))
        XCTAssertGreaterThan(Int(fixed.g), Int(fixed.b))
    }

    func testContrastGuardDisabledIsIdentity() {
        var guardrail = ContrastGuard(minimumRatio: 1)
        let c = RGB8(250, 250, 250)
        XCTAssertEqual(guardrail.adjusted(foreground: c, background: RGB8(255, 255, 255)), c)
    }

    func testContrastGuardWorksOnMidGrayBackground() {
        var guardrail = ContrastGuard(minimumRatio: 4.5)
        let bg = RGB8(119, 119, 119)
        let fixed = guardrail.adjusted(foreground: RGB8(125, 125, 125), background: bg)
        XCTAssertGreaterThanOrEqual(ContrastGuard.ratio(fixed, bg), 4.5 - 0.01)
    }
}
