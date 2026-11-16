import AppKit
import KansoBridge
import KansoUI
import TerminalKit

@MainActor
enum PerformanceProbe {
    static let root = ProcessInfo.processInfo.environment["KANSO_PERF_ROOT"].map { URL(fileURLWithPath: $0) }
    static let defaults: UserDefaults? = root.map { _ in ProbeDefaults() }
    private static var phases: [[String: Any]] = []

    static func mark(_ phase: String) {
        guard root != nil else { return }
        phases.append(["phase": phase, "uptime_ns": DispatchTime.now().uptimeNanoseconds])
    }

    static func savePhases() {
        guard let root else { return }
        let data = try? JSONSerialization.data(withJSONObject: phases, options: [.prettyPrinted, .sortedKeys])
        try? data?.write(to: root.appendingPathComponent("phases.json"))
    }

    static func record(renderer: TerminalRenderer) {
        guard let root else { return }
        let values: [String: Any] = ["shader_compile_ms": renderer.shaderCompileSeconds * 1000,
                                     "atlas": renderer.atlasStatsDescription]
        let data = try? JSONSerialization.data(withJSONObject: values, options: [.prettyPrinted, .sortedKeys])
        try? data?.write(to: root.appendingPathComponent("renderer.json"))
    }

    static func makeCore(root: URL, listener: CoreEventDispatcher) throws -> KansoCore {
        try KansoCore(config: FfiCoreConfig(
            dataDir: root.appendingPathComponent("data").path,
            appVersion: "performance", bundleId: "test.kanso.performance", preferredLanguage: "en", region: nil,
            defaultShell: "/bin/sh -i", shellIntegration: false
        ), listener: listener, secrets: ProbeSecrets())
    }

    static func observe(terminal: TerminalView) {
        guard let root else { return }
        terminal.onFramePresented = { [weak terminal] frame in
            guard let terminal else { return }
            var text = ""
            for row in 0..<frame.rows {
                for col in 0..<frame.cols {
                    let scalar = Unicode.Scalar(frame.cell(row: row, col: col).ch) ?? " "
                    text.unicodeScalars.append(scalar)
                }
            }
            guard terminal.window?.firstResponder === terminal else { return }
            guard text.contains("kanso-perf-ready>") else { return }
            terminal.onFramePresented = nil
            if let renderer = terminal.renderer { record(renderer: renderer) }
            mark("ready")
            savePhases()
            try? Data("ready\n".utf8).write(to: root.appendingPathComponent("ready"))
        }
        terminal.setNeedsFrame()
    }

}

private final class ProbeDefaults: UserDefaults {
    private var values: [String: Any] = [:]
    override func object(forKey key: String) -> Any? { values[key] }
    override func bool(forKey key: String) -> Bool { values[key] as? Bool ?? false }
    override func string(forKey key: String) -> String? { values[key] as? String }
    override func set(_ value: Any?, forKey key: String) { values[key] = value }
    override func set(_ value: Bool, forKey key: String) { values[key] = value }
    override func removeObject(forKey key: String) { values[key] = nil }
}

private final class ProbeSecrets: FfiSecretStore, Sendable {
    func get(account: String) throws -> String? { nil }
    func set(account: String, secret: String) throws {}
    func delete(account: String) throws {}
}
