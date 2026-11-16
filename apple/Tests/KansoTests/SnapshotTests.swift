import AppKit
import CoreGraphics
import ImageIO
import KansoBridge
import TerminalKit
import UniformTypeIdentifiers
import XCTest

/// 终端渲染的截图测试：用 mock 帧在浅色、深色两种外观下离屏渲染，与 `apple/Tests/Snapshots/` 下的参考 PNG 逐像素比较
/// （architecture §11：每个界面的截图测试都要在两种外观下各跑一次）。
///
/// 设置环境变量 `KANSO_RECORD_SNAPSHOTS=1` 重新录制参考图（`cargo xtask apple-test` 会把它转发给测试进程）。
///
/// 参考图依赖渲染它的 macOS 版本与系统字体（SF Mono 的字形轮廓随系统更新可能微调），
/// 系统大版本升级后如果出现整体性的小差异，重新录制即可；容差只用来吸收 GPU 抗锯齿的个位数差异。
@MainActor
final class SnapshotTests: XCTestCase {
    /// 与单元格网格对齐的画布：640x400 pt，2x。
    private static let size = CGSize(width: 640, height: 400)
    private static let scale: CGFloat = 2

    static var snapshotDirectory: URL {
        // #filePath 是编译时的源文件绝对路径：Tests/KansoTests/SnapshotTests.swift。
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Snapshots", isDirectory: true)
    }

    func testTerminalLight() throws { try verify(.light, seq: 0, name: "terminal-light") }
    func testTerminalDark() throws { try verify(.dark, seq: 0, name: "terminal-dark") }
    /// seq 5 让第二个提示符落在视口内，覆盖命令分隔线。
    func testTerminalScrolledLight() throws { try verify(.light, seq: 5, name: "terminal-scrolled-light") }
    func testTerminalScrolledDark() throws { try verify(.dark, seq: 5, name: "terminal-scrolled-dark") }

    // MARK: -

    private func verify(_ appearance: TerminalAppearance, seq: UInt64, name: String) throws {
        let renderer = try XCTUnwrap(TerminalRenderer(), "没有可用的 Metal 设备")
        // 时间戳栏按本机时区显示，固定 UTC 才能让参考图在任何机器上一致。
        renderer.timeZone = try XCTUnwrap(TimeZone(secondsFromGMT: 0))
        let grid = renderer.gridSize(for: Self.size, scale: Self.scale)
        let frame = try XCTUnwrap(FrameSnapshot.mock(cols: grid.cols, rows: grid.rows, seq: seq))
        let image = try XCTUnwrap(
            renderer.renderImage(frame: frame, size: Self.size, scale: Self.scale, appearance: appearance)
        )

        let url = Self.snapshotDirectory.appendingPathComponent("\(name).png")
        if ProcessInfo.processInfo.environment["KANSO_RECORD_SNAPSHOTS"] == "1" {
            try FileManager.default.createDirectory(at: Self.snapshotDirectory, withIntermediateDirectories: true)
            try Self.writePNG(image, to: url)
            print("已录制参考截图：\(url.path)")
            return
        }

        guard FileManager.default.fileExists(atPath: url.path) else {
            XCTFail("缺少参考截图 \(url.path)；用 KANSO_RECORD_SNAPSHOTS=1 录制")
            return
        }
        let reference = try XCTUnwrap(Self.loadPNG(url), "无法读取参考截图 \(url.path)")
        let result = try Self.compare(image, reference)
        if !result.passed {
            // 失败时把实际输出留在临时目录，方便肉眼对比。
            let actual = FileManager.default.temporaryDirectory.appendingPathComponent("\(name)-actual.png")
            try? Self.writePNG(image, to: actual)
            XCTFail("\(name) 与参考截图不一致：\(result.summary)；实际输出见 \(actual.path)")
        }
    }

    // MARK: 比较

    struct Comparison {
        var passed: Bool
        var summary: String
    }

    /// 每通道差 ≤ 2 视为相同；不同像素占比必须 < 0.1%。
    static func compare(_ a: CGImage, _ b: CGImage) throws -> Comparison {
        guard a.width == b.width, a.height == b.height else {
            return Comparison(passed: false, summary: "尺寸不同：\(a.width)x\(a.height) 对 \(b.width)x\(b.height)")
        }
        let pa = try pixels(a), pb = try pixels(b)
        var different = 0
        var maxDelta = 0
        for i in stride(from: 0, to: pa.count, by: 4) {
            var bad = false
            for c in 0..<4 {
                let d = abs(Int(pa[i + c]) - Int(pb[i + c]))
                maxDelta = max(maxDelta, d)
                if d > 2 { bad = true }
            }
            if bad { different += 1 }
        }
        let total = pa.count / 4
        let ratio = Double(different) / Double(total)
        return Comparison(
            passed: ratio < 0.001,
            summary: "\(different)/\(total) 个像素超出容差（\(String(format: "%.4f%%", ratio * 100))），最大通道差 \(maxDelta)"
        )
    }

    /// 统一解码成 sRGB、RGBA8 的像素，避免 PNG 编码方式（带不带 alpha、色彩配置）影响比较。
    static func pixels(_ image: CGImage) throws -> [UInt8] {
        var data = [UInt8](repeating: 0, count: image.width * image.height * 4)
        let ok = data.withUnsafeMutableBytes { raw -> Bool in
            guard let ctx = CGContext(
                data: raw.baseAddress, width: image.width, height: image.height, bitsPerComponent: 8,
                bytesPerRow: image.width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { return false }
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            return true
        }
        guard ok else { throw CocoaError(.fileReadUnknown) }
        return data
    }

    static func writePNG(_ image: CGImage, to url: URL) throws {
        guard let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
            throw CocoaError(.fileWriteUnknown)
        }
        CGImageDestinationAddImage(dest, image, nil)
        guard CGImageDestinationFinalize(dest) else { throw CocoaError(.fileWriteUnknown) }
    }

    static func loadPNG(_ url: URL) -> CGImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}
