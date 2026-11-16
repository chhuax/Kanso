import AppKit
import KansoBridge
import TerminalKit
import XCTest

/// 渲染性能测试：200x60 网格、全屏彩色文字，连续渲染 300 帧，统计每帧 CPU 侧耗时。
///
/// 目标远小于 8.3 ms（120 Hz 的帧预算，design.md §6）。统计口径：`RenderStats.totalSeconds`，
/// 即生成实例数据 + 编码并提交命令缓冲，不含等待 GPU 与读回。
/// Debug 配置下 Swift 没有优化，数字只作参考；断言只在优化构建里生效。
@MainActor
final class PerformanceTests: XCTestCase {
    func testDenseFrameEncodeTime() throws {
        let renderer = try XCTUnwrap(TerminalRenderer())
        renderer.timeZone = try XCTUnwrap(TimeZone(secondsFromGMT: 0))
        let (cols, rows) = (200, 60)
        // 画布取刚好放下 200x60 网格的大小。
        let scale: CGFloat = 2
        let m = renderer.metrics(scale: scale)
        let size = CGSize(
            width: CGFloat(cols * m.cellWidth + 600) / scale,
            height: CGFloat(rows * m.cellHeight + 80) / scale
        )

        var encode = [Double](), submit = [Double](), total = [Double](), instances = 0
        for i in 0..<300 {
            // 每帧内容都不同（seq 变化让每个字符与颜色整体位移），避免只测到"同一帧重复命中缓存"。
            let frame = try XCTUnwrap(FrameSnapshot.mockDense(cols: cols, rows: rows, seq: UInt64(i)))
            _ = try XCTUnwrap(renderer.renderImage(frame: frame, size: size, scale: scale, appearance: i % 2 == 0 ? .dark : .light))
            let s = renderer.lastStats
            encode.append(s.encodeSeconds * 1000)
            submit.append(s.submitSeconds * 1000)
            total.append(s.totalSeconds * 1000)
            instances = s.instanceCount
        }

        func summary(_ name: String, _ v: [Double]) -> String {
            let sorted = v.sorted()
            let p99 = sorted[Int(Double(sorted.count - 1) * 0.99)]
            return String(
                format: "%@ 平均 %.3f ms  P99 %.3f ms  最大 %.3f ms", name, v.reduce(0, +) / Double(v.count), p99, sorted.last!
            )
        }
        let report = """
        PERF 200x60 满屏彩色文字，300 帧，每帧实例数 \(instances)，着色器编译 \(String(format: "%.1f", renderer.shaderCompileSeconds * 1000)) ms
        PERF \(summary("编码实例", encode))
        PERF \(summary("提交命令", submit))
        PERF \(summary("合计    ", total))
        PERF 图集：\(renderer.atlasStatsDescription)
        """
        print(report)

        #if !DEBUG
        let sorted = total.sorted()
        XCTAssertLessThan(total.reduce(0, +) / Double(total.count), 8.3, "平均帧编码耗时应远小于 8.3 ms")
        XCTAssertLessThan(sorted[Int(Double(sorted.count - 1) * 0.99)], 8.3, "P99 应小于 8.3 ms")
        #endif
    }
}
