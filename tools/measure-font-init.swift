import AppKit
import Dispatch

@main
struct MeasureFontInit {
    @MainActor
    static func main() {
        let started = DispatchTime.now().uptimeNanoseconds
        let fonts = TerminalFonts(base: .monospacedSystemFont(ofSize: 13, weight: .regular), scale: 2)
        _ = fonts.resolve(text: "kanso-perf-ready>", face: .terminal, bold: false, italic: false)
        let elapsed = Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000
        print(String(format: "%.3f", elapsed))
    }
}
