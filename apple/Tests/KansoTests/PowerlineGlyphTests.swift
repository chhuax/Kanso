import AppKit
import CKansoFFI
import KansoBridge
import Metal
@testable import TerminalKit
import XCTest

/// Powerline 分隔符（U+E0B0–E0BF）由渲染器按单元格几何绘制：撑满单元格、与相邻色块无缝，
/// 与字体、Nerd Font 是否安装、行距都无关。
@MainActor
final class PowerlineGlyphTests: XCTestCase {
    typealias Color = (r: UInt8, g: UInt8, b: UInt8)
    private let a: Color = (0xE0, 0x40, 0x80)
    private let b: Color = (0x30, 0x70, 0xD0)

    private func raw(_ c: Color) -> UInt32 {
        (KANSO_COLOR_TAG_RGB << KANSO_COLOR_TAG_SHIFT) | UInt32(c.r) << 16 | UInt32(c.g) << 8 | UInt32(c.b)
    }

    /// 第 0 行：[A 底空格] [分隔符 fg=A bg=B] [B 底空格]，其余为空白。
    private func frame(scalar: UInt32, fg: Color? = nil, bg: Color? = nil, bold: Bool = false, italic: Bool = false) throws -> FrameSnapshot {
        let handle = FrameHandle.mockCommandBlocks(cols: 20, rows: 4)
        let header = try XCTUnwrap(UnsafeMutablePointer<KansoFrameHeader>(bitPattern: UInt(handle.headerAddress())))
        header.pointee.flags &= ~FrameFlags.cursorVisible.rawValue
        let cells = try XCTUnwrap(UnsafeMutablePointer(mutating: kanso_frame_row(header, 0)))
        for col in 0..<20 {
            cells[col].ch = 32
            cells[col].fg = 0
            cells[col].bg = 0
            cells[col].flags = 0
            cells[col].extra = 0
        }
        cells[0].bg = raw(a)
        cells[1].ch = scalar
        cells[1].fg = raw(fg ?? a)
        cells[1].bg = raw(bg ?? b)
        if bold { cells[1].flags |= CellFlags.bold.rawValue }
        if italic { cells[1].flags |= CellFlags.italic.rawValue }
        cells[2].bg = raw(bg ?? b)
        return try XCTUnwrap(FrameSnapshot(coreFrame: handle))
    }

    private struct Shot {
        let pixels: [UInt8]
        let width: Int
        let x0: Int, y0: Int, cw: Int, ch: Int
        /// 分隔符格内的像素，(0, 0) 为格左上角。
        func at(_ x: Int, _ y: Int) -> Color {
            let o = ((y0 + y) * width + x0 + x) * 4
            return (pixels[o], pixels[o + 1], pixels[o + 2])
        }
        /// 相邻格（dx 为相对分隔符格的格数）。
        func neighbor(_ dx: Int, _ y: Int) -> Color {
            let o = ((y0 + y) * width + x0 + dx * cw + (dx < 0 ? cw - 1 : 0)) * 4
            return (pixels[o], pixels[o + 1], pixels[o + 2])
        }
    }

    private func render(scalar: UInt32, lineHeight: Double, scale: CGFloat = 2, bold: Bool = false, italic: Bool = false,
                        file: StaticString = #filePath, line: UInt = #line) throws -> Shot {
        let renderer = try XCTUnwrap(TerminalRenderer(), file: file, line: line)
        renderer.gutterMode = .off
        renderer.lineHeight = lineHeight
        let layout = renderer.layout(scale: scale)
        let image = try XCTUnwrap(renderer.renderImage(
            frame: frame(scalar: scalar, bold: bold, italic: italic), size: CGSize(width: 400, height: 200), scale: scale, appearance: .dark
        ), file: file, line: line)
        let m = layout.metrics
        return Shot(pixels: try SnapshotTests.pixels(image), width: image.width,
                    x0: layout.originX + m.cellWidth, y0: layout.originY, cw: m.cellWidth, ch: m.cellHeight)
    }

    private func assertColor(_ actual: Color, _ expected: Color, _ what: String, tolerance: Int = 2,
                             file: StaticString = #filePath, line: UInt = #line) {
        let ok = abs(Int(actual.r) - Int(expected.r)) <= tolerance && abs(Int(actual.g) - Int(expected.g)) <= tolerance
            && abs(Int(actual.b) - Int(expected.b)) <= tolerance
        XCTAssertTrue(ok, "\(what)：期望 \(expected)，实际 \(actual)", file: file, line: line)
    }

    /// 像素必须落在 A、B 的混合线上（不能出现第三种颜色）。
    private func assertOnlyBlend(_ shot: Shot, file: StaticString = #filePath, line: UInt = #line) {
        for y in 0..<shot.ch {
            for x in -1...shot.cw {
                let p = x < 0 ? shot.neighbor(-1, y) : (x == shot.cw ? shot.neighbor(1, y) : shot.at(x, y))
                // 以 R 通道求混合比例，再检查 G、B。
                let t = (Double(p.r) - Double(b.r)) / (Double(a.r) - Double(b.r))
                let g = Double(b.g) + t * (Double(a.g) - Double(b.g))
                let bl = Double(b.b) + t * (Double(a.b) - Double(b.b))
                XCTAssertLessThanOrEqual(abs(Double(p.g) - g), 3, "(\(x),\(y)) 出现第三种颜色 \(p)", file: file, line: line)
                XCTAssertLessThanOrEqual(abs(Double(p.b) - bl), 3, "(\(x),\(y)) 出现第三种颜色 \(p)", file: file, line: line)
            }
        }
    }

    func testRightTriangleFillsCellAndJoinsSeamlesslyAtAnyLineHeight() throws {
        for lineHeight in [1.0, 1.6] {
            for scale: CGFloat in [1, 2] {
                let s = try render(scalar: 0xE0B0, lineHeight: lineHeight, scale: scale)
                let tag = "行距 \(lineHeight) 倍率 \(scale)"
                for y in 0..<s.ch { assertColor(s.at(0, y), a, "\(tag) 左边整列 y=\(y)") }
                // 左邻格（A 底）与分隔符左列同色，右邻格（B 底）与右列之外同色：没有缝。
                for y in 0..<s.ch {
                    assertColor(s.neighbor(-1, y), a, "\(tag) 左邻格 y=\(y)")
                    assertColor(s.neighbor(1, y), b, "\(tag) 右邻格 y=\(y)")
                }
                assertColor(s.at(s.cw - 2, s.ch / 2), a, "\(tag) 右边中点附近")
                // 尖端像素只有部分覆盖（约一半），不做精确断言，只要求不是背景色。
                let tip = s.at(s.cw - 1, s.ch / 2)
                XCTAssertGreaterThanOrEqual(Int(tip.r), (Int(a.r) + Int(b.r)) / 2, "\(tag) 尖端")
                assertColor(s.at(s.cw - 1, 0), b, "\(tag) 右上角")
                assertColor(s.at(s.cw - 1, s.ch - 1), b, "\(tag) 右下角")
                assertOnlyBlend(s)
            }
        }
    }

    func testLeftTriangleMirrors() throws {
        let s = try render(scalar: 0xE0B2, lineHeight: 1.6)
        for y in 0..<s.ch { assertColor(s.at(s.cw - 1, y), a, "右边整列 y=\(y)") }
        assertColor(s.at(1, s.ch / 2), a, "左边中点附近")
        assertColor(s.at(0, 0), b, "左上角")
        assertColor(s.at(0, s.ch - 1), b, "左下角")
        assertOnlyBlend(s)
    }

    func testThinArrowsDrawOnlyDiagonals() throws {
        for scalar: UInt32 in [0xE0B1, 0xE0B3] {
            let s = try render(scalar: scalar, lineHeight: 1.2)
            // 中间一列的上半、下半各有一条斜线（像素更接近前景色）。
            let column = (0..<s.ch).map { Int(s.at(s.cw / 2, $0).r) }
            let mid = (Int(a.r) + Int(b.r)) / 2
            XCTAssertTrue(column[0..<(s.ch / 2)].contains { $0 > mid }, "上半斜线")
            XCTAssertTrue(column[(s.ch / 2)...].contains { $0 > mid }, "下半斜线")
            assertColor(s.at(scalar == 0xE0B1 ? 0 : s.cw - 1, s.ch / 2), b, "开口侧中点为空")
            assertOnlyBlend(s)
        }
    }

    func testHalfCirclesFillCellHeightAndHugCorners() throws {
        for lineHeight in [1.0, 1.6] {
            for scalar: UInt32 in [0xE0B4, 0xE0B6] {
                let s = try render(scalar: scalar, lineHeight: lineHeight)
                let flat = scalar == 0xE0B4 ? 0 : s.cw - 1  // 竖边所在列
                let round = scalar == 0xE0B4 ? s.cw - 1 : 0
                for y in 0..<s.ch { assertColor(s.at(flat, y), a, "\(scalar) 竖边 y=\(y)") }
                // 弧顶、弧底在竖边的上下端；弧在对侧收到边中点，所以对侧四角是背景。
                assertColor(s.at(flat, 0), a, "\(scalar) 上端")
                assertColor(s.at(flat, s.ch - 1), a, "\(scalar) 下端")
                assertColor(s.at(round, 0), b, "\(scalar) 对侧上角")
                assertColor(s.at(round, s.ch - 1), b, "\(scalar) 对侧下角")
                assertColor(s.at(scalar == 0xE0B4 ? s.cw - 2 : 1, s.ch / 2), a, "\(scalar) 凸出端")
                assertOnlyBlend(s)
            }
        }
    }

    func testHalfCircleOutlineKeepsOpenSideEmpty() throws {
        let s = try render(scalar: 0xE0B5, lineHeight: 1.2)
        assertColor(s.at(0, s.ch / 2), b, "开口侧中点为空")
        assertColor(s.at(s.cw - 1, s.ch / 2), a, "弧顶")
        assertColor(s.at(s.cw / 2, s.ch / 2), b, "内部为空")
        assertOnlyBlend(s)
    }

    func testCornerTrianglesPutRightAngleInTheirCorner() throws {
        // (码点, 直角所在角, 对角)
        let cases: [(UInt32, String)] = [(0xE0B8, "bl"), (0xE0BA, "br"), (0xE0BC, "tl"), (0xE0BE, "tr")]
        for (scalar, corner) in cases {
            let s = try render(scalar: scalar, lineHeight: 1.2)
            func pos(_ c: String) -> (Int, Int) {
                (c.hasSuffix("l") ? 0 : s.cw - 1, c.hasPrefix("t") ? 0 : s.ch - 1)
            }
            let opposite = String(corner.first == "t" ? "b" : "t") + String(corner.last == "l" ? "r" : "l")
            let right = pos(corner), far = pos(opposite)
            assertColor(s.at(right.0, right.1), a, "\(String(scalar, radix: 16)) 直角")
            assertColor(s.at(far.0, far.1), b, "\(String(scalar, radix: 16)) 对角")
            // 与直角相邻的两条边整条都是前景色。
            for i in 0..<s.ch where corner.last == "l" { assertColor(s.at(0, i), a, "左边 y=\(i)") }
            for i in 0..<s.ch where corner.last == "r" { assertColor(s.at(s.cw - 1, i), a, "右边 y=\(i)") }
            for i in 0..<s.cw where corner.first == "b" { assertColor(s.at(i, s.ch - 1), a, "下边 x=\(i)") }
            for i in 0..<s.cw where corner.first == "t" { assertColor(s.at(i, 0), a, "上边 x=\(i)") }
            assertOnlyBlend(s)
        }
    }

    func testDiagonalLinesCrossTheCell() throws {
        // "\\"：E0B9、E0BF；"/"：E0BB、E0BD。
        for (scalar, backslash) in [(UInt32(0xE0B9), true), (0xE0BB, false), (0xE0BD, false), (0xE0BF, true)] {
            let s = try render(scalar: scalar, lineHeight: 1.2)
            let cx = s.cw / 2
            let cy = s.ch / 2
            // 中心在线上，对角两端的另一侧角落为空。
            assertColor(s.at(cx, cy), a, "\(String(scalar, radix: 16)) 中心", tolerance: 60)
            let emptyCorner = backslash ? (s.cw - 1, 0) : (0, 0)
            assertColor(s.at(emptyCorner.0, emptyCorner.1), b, "\(String(scalar, radix: 16)) 空角")
            assertOnlyBlend(s)
        }
    }

    func testBoldAndItalicDoNotChangeShape() throws {
        let plain = try render(scalar: 0xE0B0, lineHeight: 1.2)
        let styled = try render(scalar: 0xE0B0, lineHeight: 1.2, bold: true, italic: true)
        XCTAssertEqual(plain.pixels, styled.pixels)
    }

    func testSameBitmapWithAndWithoutNerdFont() throws {
        let device = try XCTUnwrap(MTLCreateSystemDefaultDevice())
        let base = NSFont.monospacedSystemFont(ofSize: 14, weight: .regular)
        func bitmaps(nerd: String?, lineHeight: Double) -> [UInt32: (GlyphEntry, [UInt8])] {
            let fonts = TerminalFonts(base: base, scale: 2, lineHeight: lineHeight, nerdFamily: nerd)
            let atlas = GlyphAtlas(device: device, fonts: fonts)
            var out: [UInt32: (GlyphEntry, [UInt8])] = [:]
            for scalar in UInt32(0xE0B0)...0xE0BF {
                let e = atlas.entry(scalar: scalar, bold: scalar % 2 == 0, italic: scalar % 3 == 0, wide: false, face: .terminal)
                var data = [UInt8](repeating: 0, count: Int(e.width * e.height))
                let tex = atlas.grayTexture(page: e.page)
                let x = Int((e.uv.x * Float(tex.width)).rounded()), y = Int((e.uv.y * Float(tex.width)).rounded())
                data.withUnsafeMutableBytes {
                    tex.getBytes($0.baseAddress!, bytesPerRow: Int(e.width), from: MTLRegionMake2D(x, y, Int(e.width), Int(e.height)), mipmapLevel: 0)
                }
                out[scalar] = (e, data)
            }
            return out
        }
        for lineHeight in [1.0, 1.6] {
            let without = bitmaps(nerd: nil, lineHeight: lineHeight)
            let with = bitmaps(nerd: "Menlo", lineHeight: lineHeight)
            let cell = TerminalFonts(base: base, scale: 2, lineHeight: lineHeight, nerdFamily: nil).terminal.metrics
            for scalar in UInt32(0xE0B0)...0xE0BF {
                let (e1, d1) = try XCTUnwrap(without[scalar]), (e2, d2) = try XCTUnwrap(with[scalar])
                XCTAssertFalse(e1.isEmpty)
                XCTAssertEqual(Int(e1.width), cell.cellWidth)
                XCTAssertEqual(Int(e1.height), cell.cellHeight)
                XCTAssertEqual(e1.offsetX, 0)
                XCTAssertEqual(e1.offsetY, 0)
                XCTAssertEqual(e1.width, e2.width)
                XCTAssertEqual(e1.height, e2.height)
                XCTAssertEqual(d1, d2, "U+\(String(scalar, radix: 16)) 有无 Nerd Font 应一致")
            }
        }
    }
}
