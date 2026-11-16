import AppKit
@testable import Kanso
import XCTest

@MainActor
final class TitlebarDoubleClickTests: XCTestCase {
    func testActionFollowsSystemPreference() {
        XCTAssertEqual(TitlebarDoubleClick.action(preference: nil), .zoom)
        XCTAssertEqual(TitlebarDoubleClick.action(preference: "Maximize"), .zoom)
        XCTAssertEqual(TitlebarDoubleClick.action(preference: "Fill"), .zoom)
        XCTAssertEqual(TitlebarDoubleClick.action(preference: "Minimize"), .minimize)
        XCTAssertEqual(TitlebarDoubleClick.action(preference: "None"), .none)
    }

    func testOnlyTheStripAboveContentLayoutRectCounts() {
        XCTAssertTrue(TitlebarDoubleClick.isInTitlebar(point: NSPoint(x: 10, y: 660), contentLayoutMaxY: 652))
        XCTAssertTrue(TitlebarDoubleClick.isInTitlebar(point: NSPoint(x: 10, y: 652), contentLayoutMaxY: 652))
        XCTAssertFalse(TitlebarDoubleClick.isInTitlebar(point: NSPoint(x: 10, y: 600), contentLayoutMaxY: 652))
    }

    func testControlsAndTheirSubviewsAreInteractive() {
        let button = NSButton(title: "x", target: nil, action: nil)
        let inner = NSView()
        button.addSubview(inner)
        XCTAssertTrue(TitlebarDoubleClick.isInteractive(button))
        XCTAssertTrue(TitlebarDoubleClick.isInteractive(inner))
        button.isEnabled = false
        XCTAssertFalse(TitlebarDoubleClick.isInteractive(button))
        XCTAssertTrue(TitlebarDoubleClick.isInteractive(NSTextView()))
        XCTAssertFalse(TitlebarDoubleClick.isInteractive(NSView()))
        XCTAssertFalse(TitlebarDoubleClick.isInteractive(nil))
    }
}
