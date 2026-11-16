import AppKit
@testable import Kanso
import XCTest

@MainActor
final class SidebarFilterFieldTests: XCTestCase {
    func testClearButtonVisibilityFollowsText() throws {
        let field = SidebarFilterField(frame: NSRect(x: 0, y: 0, width: 200, height: 24))
        let clear = try XCTUnwrap(field.subviews.compactMap { $0 as? NSButton }.first)
        XCTAssertTrue(clear.isHidden)
        field.stringValue = "src"
        XCTAssertFalse(clear.isHidden)

        var cleared = false
        field.onClear = { cleared = true }
        clear.performClick(nil)
        XCTAssertEqual(field.stringValue, "")
        XCTAssertTrue(clear.isHidden)
        XCTAssertTrue(cleared)
    }

    /// 不是系统胶囊样式：圆角取 design token 的小圆角，而不是高度的一半。
    func testUsesSmallCornerRadius() {
        let field = SidebarFilterField(frame: NSRect(x: 0, y: 0, width: 200, height: 24))
        XCTAssertEqual(field.layer?.cornerRadius, DesignTokens.Radius.md)
        XCTAssertLessThan(DesignTokens.Radius.md, 24 / 2)
    }
}
