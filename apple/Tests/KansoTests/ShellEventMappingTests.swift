import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

/// `CoreEventMapping`：core 事件到工作区更新的纯函数层。
@MainActor
final class ShellEventMappingTests: XCTestCase {
    private func tab(_ id: SessionID) -> TabItem {
        TabItem(id: id, kind: .local, name: "t\(id)", connection: .connected)
    }

    func testStateChangeMapsToConnection() {
        XCTAssertEqual(CoreEventMapping.update(for: .sessionStateChanged(session: 1, state: .connecting)), .connection(1, .connecting))
        XCTAssertEqual(CoreEventMapping.update(for: .sessionStateChanged(session: 1, state: .connected)), .connection(1, .connected))
    }

    func testUserCloseHasNoReasonText() {
        XCTAssertEqual(
            CoreEventMapping.update(for: .sessionStateChanged(session: 1, state: .closed(reason: .byUser))),
            .connection(1, .closed(reason: nil))
        )
    }

    func testExitedShowsExitCodeWhenKnown() {
        let withCode = CoreEventMapping.closeText(for: .exited(code: 3))
        XCTAssertNotNil(withCode)
        XCTAssertTrue(withCode?.contains("3") == true, "带退出码：\(withCode ?? "nil")")
        let noCode = CoreEventMapping.closeText(for: .exited(code: nil))
        XCTAssertNotNil(noCode)
        XCTAssertFalse(noCode?.contains("(") == true || noCode?.contains("（") == true)
    }

    func testErrorRendersConcreteMessage() {
        // 测试宿主的界面语言不确定，所以只断言“渲染出了具体内容”：
        // 含有参数代入的 `h:22`，且不是 key 本身。
        let message = FfiMessage(key: "ssh-connection-lost", args: [
            FfiMessageArg(name: "endpoint", value: .endpoint(host: "h", port: 22, via: [])),
        ])
        let text = CoreEventMapping.closeText(for: .error(message: message))
        XCTAssertNotNil(text)
        XCTAssertTrue(text?.contains("h:22") == true, text ?? "nil")
        XCTAssertNotEqual(text, "ssh-connection-lost")
    }

    func testLocationAndActivity() {
        XCTAssertEqual(
            CoreEventMapping.update(for: .locationChanged(session: 2, cwd: "/a", cwdHost: nil, gitBranch: "main")),
            .location(2, cwd: "/a", cwdHost: nil, gitBranch: "main")
        )
        XCTAssertEqual(CoreEventMapping.update(for: .activityChanged(session: 2, activity: .commandRunning)), .activity(2, .commandRunning))
        XCTAssertEqual(
            CoreEventMapping.update(for: .activityChanged(session: 2, activity: .agentWorking(agent: .claude))),
            .activity(2, .agentWorking(name: "Claude Code", iconAsset: "claude"))
        )
        XCTAssertEqual(
            CoreEventMapping.update(for: .activityChanged(session: 2, activity: .agentIdle(agent: .codex))),
            .activity(2, .agentIdle(name: "Codex", iconAsset: "codex"))
        )
    }

    func testAgentIconAssetNames() {
        XCTAssertEqual(AgentIcon.assetName(for: .claude), "claude")
        XCTAssertEqual(AgentIcon.assetName(for: .codex), "codex")
        XCTAssertEqual(AgentIcon.assetName(for: .gemini), "gemini")
        XCTAssertEqual(AgentIcon.assetName(for: .cursorAgent), "cursor-agent")
        XCTAssertEqual(AgentIcon.assetName(for: .copilot), "copilot")
        XCTAssertEqual(AgentIcon.assetName(for: .amp), "amp")
        XCTAssertEqual(AgentIcon.assetName(for: .openCode), "opencode")
        XCTAssertEqual(AgentIcon.assetName(for: .goose), "goose")
        XCTAssertEqual(AgentIcon.assetName(for: .qwen), "qwen")
        XCTAssertEqual(AgentIcon.assetName(for: .openHands), "openhands")
        XCTAssertEqual(AgentIcon.assetName(for: .grok), "grok")
        XCTAssertNil(AgentIcon.assetName(for: .aider))
        XCTAssertNil(AgentIcon.assetName(for: .crush))
        XCTAssertNil(AgentIcon.assetName(for: .droid))
    }

    func testAgentNames() {
        XCTAssertEqual(CoreEventMapping.agentName(.claude), "Claude Code")
        XCTAssertEqual(CoreEventMapping.agentName(.codex), "Codex")
        XCTAssertEqual(CoreEventMapping.agentName(.gemini), "Gemini CLI")
        XCTAssertEqual(CoreEventMapping.agentName(.aider), "Aider")
        XCTAssertEqual(CoreEventMapping.agentName(.goose), "AI")
    }

    func testUnrelatedEventsAreIgnored() {
        XCTAssertEqual(CoreEventMapping.update(for: .terminalDirty(session: 1)), .ignored)
    }

    func testUnreadSetWhenBackgroundTabFinishes() {
        let ws = Workspace()
        ws.addTab(tab(1))
        ws.addTab(tab(2)) // 2 成为活动标签
        var tracker = TransferTracker()
        CoreEventMapping.apply(.activity(1, .commandRunning), to: ws, transfers: &tracker)
        CoreEventMapping.apply(.activity(1, .idle), to: ws, transfers: &tracker)
        XCTAssertEqual(ws.item(1)?.unreadDone, true)
        // 活动标签完成不置未读。
        CoreEventMapping.apply(.activity(2, .commandRunning), to: ws, transfers: &tracker)
        CoreEventMapping.apply(.activity(2, .idle), to: ws, transfers: &tracker)
        XCTAssertEqual(ws.item(2)?.unreadDone, false)
    }

    func testTransferFinishResolvesSessionThroughTracker() {
        let ws = Workspace()
        ws.addTab(tab(1))
        var tracker = TransferTracker()
        CoreEventMapping.apply(.transferStarted(transfer: 10, session: 1), to: ws, transfers: &tracker)
        CoreEventMapping.apply(.transferStarted(transfer: 11, session: 1), to: ws, transfers: &tracker)
        XCTAssertEqual(ws.item(1)?.activeTransfers, 2)
        CoreEventMapping.apply(.transferFinished(transfer: 10), to: ws, transfers: &tracker)
        XCTAssertEqual(ws.item(1)?.activeTransfers, 1)
        // 重复的结束事件不会把计数减成负数，也不会减到别的传输头上。
        CoreEventMapping.apply(.transferFinished(transfer: 10), to: ws, transfers: &tracker)
        XCTAssertEqual(ws.item(1)?.activeTransfers, 1)
        CoreEventMapping.apply(.transferFinished(transfer: 11), to: ws, transfers: &tracker)
        XCTAssertEqual(ws.item(1)?.activeTransfers, 0)
        XCTAssertEqual(tracker.count, 0)
    }

    func testTransferForUnknownSessionIsNotTracked() {
        let ws = Workspace()
        var tracker = TransferTracker()
        CoreEventMapping.apply(.transferStarted(transfer: 1, session: 99), to: ws, transfers: &tracker)
        XCTAssertEqual(tracker.count, 0)
    }

    func testTransferEventMapping() {
        let e = FfiCoreEvent.transferStarted(
            transfer: 5, session: 7, protocol: .sftp, direction: .upload, name: "f", totalBytes: nil
        )
        XCTAssertEqual(CoreEventMapping.update(for: e), .transferStarted(transfer: 5, session: 7))
        XCTAssertEqual(CoreEventMapping.update(for: .transferFinished(transfer: 5, outcome: .completed(skipped: 0))), .transferFinished(transfer: 5))
    }

    func testTooltipMentionsStateAndBranch() {
        var item = TabItem(id: 1, kind: .local, name: "x", cwd: "/tmp/a", gitBranch: "dev", connection: .connected, activity: .commandRunning)
        let tip = TabDescriptions.tooltip(for: item)
        XCTAssertTrue(tip.contains("/tmp/a"))
        XCTAssertTrue(tip.contains("dev"))
        item.activity = .agentWorking(name: "Codex")
        XCTAssertTrue(TabDescriptions.tooltip(for: item).contains("Codex"))
    }
}
