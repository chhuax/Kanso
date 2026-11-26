import Foundation
import KansoBridge
import KansoUI
import XCTest
@testable import Kanso

@MainActor
final class MonitorPanelTests: XCTestCase {
    private func makeSample(rx: UInt64, tx: UInt64, cpu: UInt8? = 35) -> FfiHostSample {
        FfiHostSample(
            load1: 0.25,
            load5: 0.50,
            load15: 0.75,
            cpuPercent: cpu,
            memTotal: 8 * 1024 * 1024 * 1024,
            memAvailable: 2 * 1024 * 1024 * 1024,
            swapTotal: 2 * 1024 * 1024 * 1024,
            swapFree: 1 * 1024 * 1024 * 1024,
            disks: [FfiDiskUse(mount: "/", size: 100 * 1024 * 1024, used: 40 * 1024 * 1024, percent: 40)],
            nets: [FfiNetCounter(iface: "eth0", rx: rx, tx: tx)]
        )
    }

    func testFollowFiltersLocalAndDisconnectedSessions() {
        let model = MonitorPanelModel(fetcher: { _ in self.makeSample(rx: 0, tx: 0) })
        let local = TabItem(id: 1, kind: .local, name: "zsh", connection: .connected)
        model.follow(local)
        XCTAssertNil(model.targetSession)

        let connecting = TabItem(id: 2, kind: .ssh, name: "prod", connection: .connecting)
        model.follow(connecting)
        XCTAssertNil(model.targetSession)

        let ssh = TabItem(id: 2, kind: .ssh, name: "prod", connection: .connected)
        model.follow(ssh)
        XCTAssertEqual(model.targetSession, 2)

        let sftp = TabItem(id: 3, kind: .sftp, name: "files", connection: .connected)
        model.follow(sftp)
        XCTAssertEqual(model.targetSession, 3)

        let closed = TabItem(id: 3, kind: .sftp, name: "files", connection: .closed(reason: nil))
        model.follow(closed)
        XCTAssertNil(model.targetSession)
        XCTAssertNil(model.snapshot)
    }

    func testNetRateCalculatedAcrossSamplesAndGuardsWraparound() {
        let model = MonitorPanelModel(fetcher: { _ in self.makeSample(rx: 0, tx: 0) })
        model.apply(makeSample(rx: 1000, tx: 500), at: 10.0)
        // 第一拍没有速率，只有累计值。
        XCTAssertNil(model.snapshot?.nets.first?.rxPerSec)
        XCTAssertNil(model.snapshot?.nets.first?.txPerSec)
        XCTAssertEqual(model.snapshot?.memUsed, 6 * 1024 * 1024 * 1024)
        XCTAssertEqual(model.snapshot?.swapUsed, 1 * 1024 * 1024 * 1024)

        // 2 秒后增加 4096 / 2048 字节 -> 2048 B/s、1024 B/s。
        model.apply(makeSample(rx: 5096, tx: 2548), at: 12.0)
        XCTAssertEqual(model.snapshot?.nets.first?.rxPerSec, 2048)
        XCTAssertEqual(model.snapshot?.nets.first?.txPerSec, 1024)

        // 计数器回绕 / 重置时保护为 nil，不崩溃。
        model.apply(makeSample(rx: 100, tx: 50), at: 14.0)
        XCTAssertNil(model.snapshot?.nets.first?.rxPerSec)
        XCTAssertNil(model.snapshot?.nets.first?.txPerSec)
    }

    func testStaleSampleDiscardedAfterTabSwitch() async {
        let firstGate = AsyncGate()
        let sample1 = makeSample(rx: 100, tx: 100, cpu: 11)
        let sample2 = makeSample(rx: 200, tx: 200, cpu: 99)
        let model = MonitorPanelModel(fetcher: { session in
            if session == 1 {
                await firstGate.wait()
                return sample1
            }
            return sample2
        })
        model.follow(TabItem(id: 1, kind: .ssh, name: "a", connection: .connected))
        let slowTask = Task { @MainActor in await model.sampleOnce() }
        await Task.yield()
        // 切到会话 2 后，会话 1 的迟到结果不能画回来。
        model.follow(TabItem(id: 2, kind: .ssh, name: "b", connection: .connected))
        await model.sampleOnce()
        XCTAssertEqual(model.snapshot?.cpuPercent, 99)
        await firstGate.open()
        await slowTask.value
        XCTAssertEqual(model.snapshot?.cpuPercent, 99)
    }

    func testSampleFailureReportsErrorInPlace() async {
        let model = MonitorPanelModel(fetcher: { _ in
            throw FfiCoreError.User(message: .monitorUnsupported)
        })
        model.follow(TabItem(id: 1, kind: .ssh, name: "mac", connection: .connected))
        await model.sampleOnce()
        XCTAssertNil(model.snapshot)
        XCTAssertNotNil(model.error)
    }

    func testFormatBytesAndRate() {
        XCTAssertEqual(MonitorPanelModel.formatBytes(512), "512 B")
        XCTAssertEqual(MonitorPanelModel.formatBytes(1536), "1.5 KiB")
        XCTAssertEqual(MonitorPanelModel.formatRate(2 * 1024 * 1024), "2.0 MiB/s")
    }
}

private actor AsyncGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if opened { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        opened = true
        for waiter in waiters { waiter.resume() }
        waiters.removeAll()
    }
}
