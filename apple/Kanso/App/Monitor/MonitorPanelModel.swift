import Foundation
import KansoBridge
import KansoUI

/// 服务器监控面板的纯逻辑状态机与轮询控制（docs/spec/host-monitor.md）。
///
/// - 仅在面板可见且活动标签是已连接的 SSH / 纯 SFTP 会话时轮询；
/// - 切标签、隐藏面板或会话断开立即递增代次、取消进行中的采样任务，丢弃过时结果；
/// - 相邻两拍按网卡接口名计算实时速率，第一拍只展示累计字节；计数器倒退时不溢出。
@MainActor
final class MonitorPanelModel {
    struct NetDisplay: Equatable, Sendable {
        let iface: String
        let rx: UInt64
        let tx: UInt64
        let rxPerSec: UInt64?
        let txPerSec: UInt64?
    }

    struct Snapshot: Equatable, Sendable {
        let load1: Float
        let load5: Float
        let load15: Float
        let cpuPercent: UInt8?
        let memTotal: UInt64
        let memUsed: UInt64
        let swapTotal: UInt64
        let swapUsed: UInt64
        let disks: [FfiDiskUse]
        let nets: [NetDisplay]
    }

    /// 每次采样结束后的默认等待间隔（host-monitor.md §2：约 2 秒）。
    static let defaultIntervalNanoseconds: UInt64 = 2_000_000_000

    private let fetcher: @Sendable (SessionID) async throws -> FfiHostSample
    private let now: () -> TimeInterval
    private let sleep: (UInt64) async throws -> Void
    private let pollIntervalNanoseconds: UInt64

    private var generation: UInt64 = 0
    private var pollTask: Task<Void, Never>?
    private var previousNets: [String: (rx: UInt64, tx: UInt64)]?
    private var previousAt: TimeInterval?

    private(set) var targetSession: SessionID?
    private(set) var snapshot: Snapshot?
    private(set) var error: String?
    private(set) var isSampling = false

    var onChange: (() -> Void)?

    var isVisible = false {
        didSet {
            guard isVisible != oldValue else { return }
            if isVisible {
                if targetSession != nil { startPolling() }
            } else {
                stopPolling()
            }
        }
    }

    init(
        core: KansoCore,
        pollIntervalNanoseconds: UInt64 = MonitorPanelModel.defaultIntervalNanoseconds
    ) {
        self.fetcher = { session in try await core.hostSample(sessionId: session) }
        self.now = { ProcessInfo.processInfo.systemUptime }
        self.sleep = { try await Task.sleep(nanoseconds: $0) }
        self.pollIntervalNanoseconds = pollIntervalNanoseconds
    }

    init(
        fetcher: @escaping @Sendable (SessionID) async throws -> FfiHostSample,
        now: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime },
        sleep: @escaping (UInt64) async throws -> Void = { try await Task.sleep(nanoseconds: $0) },
        pollIntervalNanoseconds: UInt64 = MonitorPanelModel.defaultIntervalNanoseconds
    ) {
        self.fetcher = fetcher
        self.now = now
        self.sleep = sleep
        self.pollIntervalNanoseconds = pollIntervalNanoseconds
    }

    deinit {
        pollTask?.cancel()
    }

    /// 跟随当前活动标签（host-monitor.md §1）：只有已连接的 SSH / 纯 SFTP 会话可采样。
    func follow(_ tab: TabItem?) {
        let eligible: SessionID? = if let tab, tab.kind != .local, case .connected = tab.connection {
            tab.id
        } else {
            nil
        }
        guard eligible != targetSession else {
            if isVisible, targetSession != nil, pollTask == nil { startPolling() }
            return
        }
        stopPolling()
        targetSession = eligible
        snapshot = nil
        error = nil
        previousNets = nil
        previousAt = nil
        isSampling = false
        onChange?()
        if isVisible, eligible != nil {
            startPolling()
        }
    }

    /// 把一拍原始结果合进状态，并计算与上一拍的网卡速率差。
    func apply(_ raw: FfiHostSample, at timestamp: TimeInterval) {
        let dt: Double? = if let prevAt = previousAt, timestamp > prevAt + 0.05 {
            timestamp - prevAt
        } else {
            nil
        }
        let nets: [NetDisplay] = raw.nets.map { net in
            let rates: (UInt64?, UInt64?) = if let dt, let prev = previousNets?[net.iface] {
                (
                    net.rx >= prev.rx ? UInt64(Double(net.rx - prev.rx) / dt) : nil,
                    net.tx >= prev.tx ? UInt64(Double(net.tx - prev.tx) / dt) : nil
                )
            } else {
                (nil, nil)
            }
            return NetDisplay(iface: net.iface, rx: net.rx, tx: net.tx, rxPerSec: rates.0, txPerSec: rates.1)
        }
        previousNets = Dictionary(raw.nets.map { ($0.iface, ($0.rx, $0.tx)) }, uniquingKeysWith: { _, latest in latest })
        previousAt = timestamp
        let memUsed = raw.memTotal >= raw.memAvailable ? raw.memTotal - raw.memAvailable : 0
        let swapUsed = raw.swapTotal >= raw.swapFree ? raw.swapTotal - raw.swapFree : 0
        snapshot = Snapshot(
            load1: raw.load1,
            load5: raw.load5,
            load15: raw.load15,
            cpuPercent: raw.cpuPercent,
            memTotal: raw.memTotal,
            memUsed: memUsed,
            swapTotal: raw.swapTotal,
            swapUsed: swapUsed,
            disks: raw.disks,
            nets: nets
        )
        error = nil
        isSampling = false
        onChange?()
    }

    /// 供测试或宿主主动采一拍（不会影响已失效的代次）。
    func sampleOnce() async {
        guard let session = targetSession else { return }
        let gen = generation
        isSampling = snapshot == nil
        onChange?()
        do {
            let raw = try await fetcher(session)
            guard generation == gen, targetSession == session else { return }
            apply(raw, at: now())
        } catch {
            guard generation == gen, targetSession == session else { return }
            isSampling = false
            self.error = CoreMessageText.text(for: error)
            onChange?()
        }
    }

    private func startPolling() {
        stopPolling()
        guard isVisible, let session = targetSession else { return }
        let gen = generation
        isSampling = snapshot == nil
        onChange?()
        pollTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self, self.generation == gen, self.isVisible, self.targetSession == session else { return }
                do {
                    let raw = try await self.fetcher(session)
                    guard !Task.isCancelled, self.generation == gen, self.isVisible, self.targetSession == session else { return }
                    self.apply(raw, at: self.now())
                } catch {
                    guard !Task.isCancelled, self.generation == gen, self.isVisible, self.targetSession == session else { return }
                    self.isSampling = false
                    self.error = CoreMessageText.text(for: error)
                    self.onChange?()
                }
                do {
                    try await self.sleep(self.pollIntervalNanoseconds)
                } catch {
                    return
                }
            }
        }
    }

    private func stopPolling() {
        generation &+= 1
        pollTask?.cancel()
        pollTask = nil
        isSampling = false
    }

    /// 字节大小格式化（二进制单位：B / KiB / MiB / GiB / TiB）。
    static func formatBytes(_ bytes: UInt64) -> String {
        let units = ["B", "KiB", "MiB", "GiB", "TiB"]
        if bytes < 1024 { return "\(bytes) B" }
        var value = Double(bytes)
        var idx = 0
        while value >= 1024, idx + 1 < units.count {
            value /= 1024
            idx += 1
        }
        return String(format: "%.1f %@", value, units[idx])
    }

    /// 网卡速率格式化。
    static func formatRate(_ bytesPerSec: UInt64) -> String {
        "\(formatBytes(bytesPerSec))/s"
    }
}
