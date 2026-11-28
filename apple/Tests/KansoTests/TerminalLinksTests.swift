import AppKit
import CKansoFFI
import KansoBridge
@testable import KansoUI
@testable import TerminalKit
import XCTest

@MainActor
final class TerminalLinksTests: XCTestCase {
    private func makeFrame(
        cols: Int = 80,
        rows: Int = 12,
        flags: FrameFlags = [],
        lines: [(row: Int, text: String, wrapped: Bool)]
    ) throws -> FrameSnapshot {
        let handle = FrameHandle.mockCommandBlocks(cols: UInt16(cols), rows: UInt16(rows))
        let header = try XCTUnwrap(UnsafeMutablePointer<KansoFrameHeader>(bitPattern: UInt(handle.headerAddress())))
        header.pointee.flags |= flags.rawValue
        for item in lines {
            let meta = try XCTUnwrap(UnsafeMutablePointer(mutating: kanso_frame_line_meta(header, UInt16(item.row))))
            if item.wrapped {
                meta.pointee.flags |= LineFlags.wrapped.rawValue
            }
            let cells = try XCTUnwrap(UnsafeMutablePointer(mutating: kanso_frame_row(header, UInt16(item.row))))
            for c in 0..<cols {
                cells[c].ch = 0
                cells[c].flags = 0
                cells[c].extra = 0
            }
            var col = 0
            for scalar in item.text.unicodeScalars where col < cols {
                let wide = isWide(scalar)
                if wide && col + 1 >= cols { break }
                cells[col].ch = scalar.value
                if wide {
                    cells[col].flags |= UInt16(KANSO_CELL_WIDE)
                    cells[col + 1].ch = 0
                    cells[col + 1].flags |= UInt16(KANSO_CELL_WIDE_SPACER)
                    col += 2
                } else {
                    col += 1
                }
            }
        }
        return try XCTUnwrap(FrameSnapshot(coreFrame: handle))
    }

    private func isWide(_ scalar: Unicode.Scalar) -> Bool {
        let v = scalar.value
        return (0x4E00...0x9FFF).contains(v) || (0x3400...0x4DBF).contains(v)
    }

    private func point(row: Int, col: Int, in view: TerminalView) throws -> NSPoint {
        let geometry = try XCTUnwrap(view.hitGeometry())
        return NSPoint(
            x: geometry.originX + (Double(col) + 0.5) * geometry.cellWidth,
            y: view.bounds.height - geometry.originY - (Double(row) + 0.5) * geometry.cellHeight
        )
    }

    // MARK: - 纯逻辑 URL 识别与白名单

    func testPlainURLDetectionAndPunctuationTrimming() throws {
        let frame = try makeFrame(lines: [
            (0, "Press Enter to open https://github.com/login/device in your browser...", false),
            (1, "(see http://a.com/w/Foo_(bar)) and \"https://x.io/p?a=1&b=2\", HTTP://UPPER.COM/x.", false),
            (2, "中文https://example.com/zh 还有 xhttp://bad.com http://", false),
            (3, "http://first.com https://second.com", false),
        ])

        let gh = try XCTUnwrap(TerminalLinks.link(in: frame, row: 0, col: 25) { _ in nil })
        XCTAssertEqual(gh.url, "https://github.com/login/device")
        XCTAssertEqual(gh.segments, [TerminalLinkSegment(row: 0, startCol: 20, endCol: 51)])
        XCTAssertTrue(gh.contains(row: 0, col: 20))
        XCTAssertTrue(gh.contains(row: 0, col: 50))
        XCTAssertFalse(gh.contains(row: 0, col: 51))
        XCTAssertNil(TerminalLinks.link(in: frame, row: 0, col: 19) { _ in nil })
        XCTAssertNil(TerminalLinks.link(in: frame, row: 0, col: 52) { _ in nil })

        let wiki = try XCTUnwrap(TerminalLinks.link(in: frame, row: 1, col: 10) { _ in nil })
        XCTAssertEqual(wiki.url, "http://a.com/w/Foo_(bar)")

        let quoted = try XCTUnwrap(TerminalLinks.link(in: frame, row: 1, col: 40) { _ in nil })
        XCTAssertEqual(quoted.url, "https://x.io/p?a=1&b=2")

        let upper = try XCTUnwrap(TerminalLinks.link(in: frame, row: 1, col: 65) { _ in nil })
        XCTAssertEqual(upper.url, "HTTP://UPPER.COM/x")

        // 中文之后没有 ASCII 字母数字边界问题，紧接 URL 应识别；第 4 列是“中”之后的第 2 个宽字符起点。
        let zh = try XCTUnwrap(TerminalLinks.link(in: frame, row: 2, col: 6) { _ in nil })
        XCTAssertEqual(zh.url, "https://example.com/zh")
        XCTAssertEqual(zh.segments, [TerminalLinkSegment(row: 2, startCol: 4, endCol: 26)])
        // 宽字符占位格本身不算链接。
        XCTAssertNil(TerminalLinks.link(in: frame, row: 2, col: 1) { _ in nil })
        // 左边界不合法的协议与空地址不识别。
        XCTAssertNil(TerminalLinks.link(in: frame, row: 2, col: 36) { _ in nil })
        XCTAssertNil(TerminalLinks.link(in: frame, row: 2, col: 50) { _ in nil })

        let first = try XCTUnwrap(TerminalLinks.link(in: frame, row: 3, col: 3) { _ in nil })
        let second = try XCTUnwrap(TerminalLinks.link(in: frame, row: 3, col: 20) { _ in nil })
        XCTAssertEqual(first.url, "http://first.com")
        XCTAssertEqual(second.url, "https://second.com")
        XCTAssertNil(TerminalLinks.link(in: frame, row: 3, col: 16) { _ in nil })
    }

    func testSoftWrappedAndHardNewlineURLs() throws {
        let frame = try makeFrame(cols: 20, rows: 6, lines: [
            (0, "open https://github.", true),
            (1, "com/login/device ok", false),
            (2, "https://notwrapped.", false),
            (3, "com/path", false),
        ])

        let wrapped = try XCTUnwrap(TerminalLinks.link(in: frame, row: 1, col: 3) { _ in nil })
        XCTAssertEqual(wrapped.url, "https://github.com/login/device")
        XCTAssertEqual(wrapped.segments, [
            TerminalLinkSegment(row: 0, startCol: 5, endCol: 20),
            TerminalLinkSegment(row: 1, startCol: 0, endCol: 16),
        ])
        XCTAssertEqual(TerminalLinks.link(in: frame, row: 0, col: 10) { _ in nil }?.url, wrapped.url)

        let hardA = try XCTUnwrap(TerminalLinks.link(in: frame, row: 2, col: 2) { _ in nil })
        XCTAssertEqual(hardA.url, "https://notwrapped")
        XCTAssertNil(TerminalLinks.link(in: frame, row: 3, col: 2) { _ in nil })
    }

    func testOpenableSchemeWhitelist() {
        XCTAssertNotNil(TerminalLinks.openableURL(from: "https://github.com/login/device"))
        XCTAssertNotNil(TerminalLinks.openableURL(from: "HTTP://example.com"))
        XCTAssertNotNil(TerminalLinks.openableURL(from: "ftp://host/file"))
        XCTAssertNotNil(TerminalLinks.openableURL(from: "ftps://host/file"))
        XCTAssertNotNil(TerminalLinks.openableURL(from: "mailto:user@example.com"))

        XCTAssertNil(TerminalLinks.openableURL(from: "file:///etc/passwd"))
        XCTAssertNil(TerminalLinks.openableURL(from: "javascript:alert(1)"))
        XCTAssertNil(TerminalLinks.openableURL(from: "ssh://host"))
        XCTAssertNil(TerminalLinks.openableURL(from: "http:"))
        XCTAssertNil(TerminalLinks.openableURL(from: ""))
    }

    // MARK: - OSC 8（经真实 KansoCore 本地会话）

    func testOSC8HyperlinkViaRealCore() async throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("kanso-links-\(UUID().uuidString)", isDirectory: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }

        let dispatcher = CoreEventDispatcher()
        let config = FfiCoreConfig(
            dataDir: dir.appendingPathComponent("data").path,
            appVersion: "0.0.0-test",
            bundleId: "test.kanso",
            preferredLanguage: "en",
            region: nil,
            defaultShell: "/bin/sh -i",
            shellIntegration: false
        )
        let core = try KansoCore(config: config, listener: dispatcher, secrets: InMemorySecretStore())
        defer { core.shutdown() }

        let session = try core.openSession(target: .defaultLocalShell, size: FfiGridSize(cols: 80, rows: 12))
        defer { core.closeSession(sessionId: session) }

        let osc8Command = "printf '\\033]8;;https://github.com/login/device\\007Click Me\\033]8;;\\007\\n'\r"
        let sink = CoreInputSink(core: core, session: session)
        let source = CoreFrameSource(core: core, session: session)
        sink.textInput(osc8Command)

        var previous: FrameSnapshot?
        var matchedFrame: FrameSnapshot?
        var matchedRow = -1
        var matchedCol = -1
        let deadline = Date().addingTimeInterval(15)
        while matchedFrame == nil, Date() < deadline {
            if let frame = source.takeFrame(reusing: previous) {
                previous = frame
                for r in 0..<frame.rows {
                    for c in 0..<frame.cols {
                        let cell = frame.cell(row: r, col: c)
                        if let id = frame.hyperlinkId(for: cell),
                           core.hyperlinkUrl(sessionId: session, id: id) == "https://github.com/login/device" {
                            matchedFrame = frame
                            matchedRow = r
                            matchedCol = c
                            break
                        }
                    }
                    if matchedFrame != nil { break }
                }
            }
            if matchedFrame == nil {
                try await Task.sleep(nanoseconds: 20_000_000)
            }
        }

        let frame = try XCTUnwrap(matchedFrame, "未在超时内收到 OSC 8 超链接帧")
        let hit = try XCTUnwrap(TerminalLinks.link(in: frame, row: matchedRow, col: matchedCol) {
            core.hyperlinkUrl(sessionId: session, id: $0)
        })
        XCTAssertEqual(hit.url, "https://github.com/login/device")
        XCTAssertEqual(hit.segments.first?.row, matchedRow)
        XCTAssertEqual((hit.segments.first?.endCol ?? 0) - (hit.segments.first?.startCol ?? 0), "Click Me".count)
    }

    // MARK: - TerminalView 悬停与点击交互

    func testCommandClickOpensURLAndPlainClickDoesNot() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let sink = RecordingSink()
        view.inputSink = sink
        var opened: [URL] = []
        view.openURL = { url in
            opened.append(url)
            return true
        }
        view.currentFrame = try makeFrame(lines: [
            (0, "Press Enter to open https://github.com/login/device in your browser...", false),
        ])

        let urlPoint = try point(row: 0, col: 25, in: view)

        // 普通左键点击：不打开链接，正常发给 inputSink。
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: urlPoint))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: urlPoint))
        XCTAssertTrue(opened.isEmpty)
        XCTAssertEqual(sink.mice.count, 2)

        // ⌘+左键点击：打开链接，不向 inputSink 发鼠标事件。
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: urlPoint, flags: .command))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: urlPoint, flags: .command))
        XCTAssertEqual(opened, [URL(string: "https://github.com/login/device")!])
        XCTAssertEqual(sink.mice.count, 2)
    }

    func testCommandClickMarginOrDragAwayDoesNotOpen() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let sink = RecordingSink()
        view.inputSink = sink
        var opened: [URL] = []
        view.openURL = { opened.append($0); return true }
        view.currentFrame = try makeFrame(lines: [
            (0, "https://github.com/login/device", false),
        ])

        // 落在左侧留白/栏区（x = 2，即使第 0 列就是 URL）也不能误打开。
        let marginPoint = NSPoint(x: 2, y: try point(row: 0, col: 0, in: view).y)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: marginPoint, flags: .command))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: marginPoint, flags: .command))
        XCTAssertTrue(opened.isEmpty)

        // ⌘+按下 URL 后拖离再松开：取消打开，也不发孤立鼠标事件。
        let start = try point(row: 0, col: 5, in: view)
        let outside = try point(row: 2, col: 5, in: view)
        let countBefore = sink.mice.count
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: start, flags: .command))
        view.mouseDragged(with: InputFixtures.mouse(.leftMouseDragged, at: outside, flags: .command))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: outside, flags: .command))
        XCTAssertTrue(opened.isEmpty)
        XCTAssertEqual(sink.mice.count, countBefore)
    }

    func testCommandClickWorksEvenDuringMouseReporting() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        let sink = RecordingSink()
        view.inputSink = sink
        var opened: [URL] = []
        view.openURL = { opened.append($0); return true }
        view.currentFrame = try makeFrame(flags: [.mouseReporting], lines: [
            (0, "open https://example.com/a", false),
        ])

        let urlPoint = try point(row: 0, col: 10, in: view)
        view.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: urlPoint, flags: .command))
        view.mouseUp(with: InputFixtures.mouse(.leftMouseUp, at: urlPoint, flags: .command))
        XCTAssertEqual(opened, [URL(string: "https://example.com/a")!])
        XCTAssertTrue(sink.mice.isEmpty)
    }

    func testHoverStateTracksMouseAndCommandModifier() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        view.currentFrame = try makeFrame(lines: [
            (0, "open https://example.com/hover", false),
        ])

        let urlPoint = try point(row: 0, col: 10, in: view)
        view.mouseMoved(with: InputFixtures.mouse(.mouseMoved, at: urlPoint))
        XCTAssertNil(view.hoveredLink)
        XCTAssertNil(view.renderer?.hoveredLink)

        // 悬停在链接上按下 ⌘：立刻高亮并通知渲染器。
        view.flagsChanged(with: InputFixtures.flags(.command))
        XCTAssertEqual(view.hoveredLink?.url, "https://example.com/hover")
        XCTAssertEqual(view.renderer?.hoveredLink, view.hoveredLink)

        // 松开 ⌘：立即清除。
        view.flagsChanged(with: InputFixtures.flags([]))
        XCTAssertNil(view.hoveredLink)
        XCTAssertNil(view.renderer?.hoveredLink)

        // 按住 ⌘ 移动与离开视图。
        view.mouseMoved(with: InputFixtures.mouse(.mouseMoved, at: urlPoint, flags: .command))
        XCTAssertEqual(view.hoveredLink?.url, "https://example.com/hover")
        let exitEvent = try XCTUnwrap(NSEvent.enterExitEvent(
            with: .mouseExited, location: urlPoint, modifierFlags: [], timestamp: 0,
            windowNumber: 0, context: nil, eventNumber: 0, trackingNumber: 0, userData: nil
        ))
        view.mouseExited(with: exitEvent)
        XCTAssertNil(view.hoveredLink)
    }

    func testUnsupportedSchemeIsRejectedWithoutOpening() throws {
        let (view, window) = try InputFixtures.makeView()
        defer { withExtendedLifetime(window) {} }
        var opened: [URL] = []
        var rejected: [String] = []
        view.openURL = { opened.append($0); return true }
        view.onOpenLinkRejected = { rejected.append($0) }

        view.openLink("file:///etc/passwd")
        XCTAssertTrue(opened.isEmpty)
        XCTAssertEqual(rejected.count, 1)
    }
}
