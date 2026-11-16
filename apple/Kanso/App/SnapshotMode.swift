import AppKit
import KansoBridge
import TerminalKit

/// 截图模式的命令行参数：`--snapshot <png 路径> [--appearance light|dark]`。
struct SnapshotOptions {
    let path: String
    let appearance: TerminalAppearance

    /// 没有 `--snapshot` 返回 nil（走正常启动）。
    init?(arguments: [String]) {
        guard let i = arguments.firstIndex(of: "--snapshot"), i + 1 < arguments.count else { return nil }
        path = arguments[i + 1]
        if let j = arguments.firstIndex(of: "--appearance"), j + 1 < arguments.count {
            appearance = arguments[j + 1] == "dark" ? .dark : .light
        } else {
            appearance = .light
        }
    }
}

/// 离屏渲染一帧到 PNG，用于不碰真实屏幕的自动截图验证。
@MainActor
enum SnapshotRenderer {
    /// 返回进程退出码：成功 0，失败 1。
    static func run(_ options: SnapshotOptions) -> Int32 {
        let size = CGSize(width: 900, height: 600)
        // 固定 2x：输出尺寸不依赖运行机器的屏幕，截图可以直接对比。
        let scale: CGFloat = 2
        guard let renderer = TerminalRenderer() else {
            fputs("截图失败：没有可用的 Metal 设备\n", stderr)
            return 1
        }
        let grid = renderer.gridSize(for: size, scale: scale)
        // seq 固定为 0，保证截图内容确定。
        guard let frame = FrameSnapshot.mock(cols: grid.cols, rows: grid.rows, seq: 0),
              let image = renderer.renderImage(frame: frame, size: size, scale: scale, appearance: options.appearance)
        else {
            fputs("截图失败：无法创建帧或渲染\n", stderr)
            return 1
        }
        PerformanceProbe.record(renderer: renderer)
        let rep = NSBitmapImageRep(cgImage: image)
        guard let png = rep.representation(using: .png, properties: [:]) else {
            fputs("截图失败：PNG 编码失败\n", stderr)
            return 1
        }
        do {
            try png.write(to: URL(fileURLWithPath: options.path))
        } catch {
            fputs("截图失败：写入 \(options.path) 出错：\(error)\n", stderr)
            return 1
        }
        return 0
    }
}
