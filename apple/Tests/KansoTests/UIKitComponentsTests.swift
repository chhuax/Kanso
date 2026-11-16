import AppKit
import KansoBridge
@testable import Kanso
import XCTest

/// 自绘组件库（`App/UIKit`）的测试。全部离屏，不弹窗、不依赖用户环境。
@MainActor
final class UIKitComponentsTests: XCTestCase {
    /// 按指定外观解析 token 颜色（与组件内部 `resolvedCGColor` 的口径一致）。
    private func resolved(_ token: String, _ name: NSAppearance.Name) -> CGColor {
        var result = NSColor.black.cgColor
        NSAppearance(named: name)!.performAsCurrentDrawingAppearance { result = NSColor.token(token).cgColor }
        return result
    }

    private func host(_ view: NSView, appearance: NSAppearance.Name = .darkAqua) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 400, height: 300), styleMask: [.borderless],
                              backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: appearance)
        window.contentView?.addSubview(view)
        return window
    }

    // MARK: 输入框

    func testFocusChangesBorderToAccent() {
        let field = KTextField(frame: NSRect(x: 0, y: 0, width: 200, height: 30))
        let window = host(field)
        defer { window.close() }
        field.updateLayer()
        let idle = field.layer?.borderColor
        XCTAssertEqual(idle, resolved(DesignTokens.Color.borderField, .darkAqua))

        var reported: [Bool] = []
        field.onFocusChange = { reported.append($0) }
        field.setFocused(true)
        field.updateLayer()
        XCTAssertEqual(field.layer?.borderColor, resolved(DesignTokens.Color.accent, .darkAqua))
        XCTAssertNotEqual(field.layer?.borderColor, idle)

        field.setFocused(false)
        field.updateLayer()
        XCTAssertEqual(field.layer?.borderColor, idle)
        XCTAssertEqual(reported, [true, false])
    }

    func testPasswordFieldIsSecureAndPlainFieldIsNot() {
        XCTAssertTrue(KTextField(secure: true).textField is NSSecureTextField)
        XCTAssertFalse(KTextField().textField is NSSecureTextField)
        // 不回显由 NSSecureTextField 保证；这里确认值仍可读写（提交时要取出来）。
        let password = KTextField(secure: true)
        password.stringValue = "s3cret"
        XCTAssertEqual(password.stringValue, "s3cret")
    }

    func testFieldUsesFieldHeightAndSmallRadius() {
        let field = KTextField()
        XCTAssertEqual(field.intrinsicContentSize.height, DesignTokens.Spacing.fieldHeight)
        field.updateLayer()
        XCTAssertEqual(field.layer?.cornerRadius, DesignTokens.Radius.md)
        XCTAssertEqual(field.layer?.borderWidth, 1)
    }

    func testLabeledFieldPutsTitleAboveControl() {
        let control = KTextField()
        let labeled = LabeledField(title: "Host", control: control)
        XCTAssertEqual(labeled.orientation, .vertical)
        XCTAssertEqual(labeled.arrangedSubviews.first, labeled.titleLabel)
        XCTAssertEqual(labeled.arrangedSubviews.last, control)
    }

    // MARK: 下拉框

    func testPopUpSelectionDoesNotCallbackUntilUserSelects() {
        let popup = KPopUpField()
        popup.setItems(["A", "B", "C"])
        var changes: [Int] = []
        popup.onChange = { changes.append($0) }
        popup.selectItem(at: 2)
        XCTAssertEqual(popup.indexOfSelectedItem, 2)
        XCTAssertTrue(changes.isEmpty, "程序设置选中项不应回调")
        popup.userSelect(at: 1)
        XCTAssertEqual(popup.selectedTitle, "B")
        XCTAssertEqual(changes, [1])
        popup.userSelect(at: 9)
        XCTAssertEqual(changes, [1], "越界忽略")
    }

    func testPopUpMenuMirrorsItemsAndChecksSelection() {
        let popup = KPopUpField()
        popup.setItems(["A", "B"])
        popup.selectItem(at: 1)
        let menu = popup.makeMenu()
        XCTAssertEqual(menu.items.map(\.title), ["A", "B"])
        XCTAssertEqual(menu.items.map(\.state), [.off, .on])
        var picked: Int?
        popup.onChange = { picked = $0 }
        let item = menu.items[0]
        _ = item.target?.perform(item.action, with: item)
        XCTAssertEqual(picked, 0)
    }

    // MARK: 按钮

    func testButtonActionsAndDisabledState() {
        let button = KButton(title: "Go", style: .primary)
        var count = 0
        button.onPress = { count += 1 }
        button.performClick(nil)
        XCTAssertEqual(count, 1)
        button.isEnabled = false
        button.performClick(nil)
        XCTAssertEqual(count, 1, "禁用时不触发")
    }

    func testToggleButtonFlipsStateAndSendsAction() {
        final class Receiver: NSObject { var hits = 0; @objc func hit(_ sender: Any?) { hits += 1 } }
        let receiver = Receiver()
        let button = KButton(title: "Aa", style: .icon)
        button.isToggle = true
        button.target = receiver
        button.action = #selector(Receiver.hit(_:))
        button.performClick(nil)
        XCTAssertEqual(button.state, .on)
        XCTAssertEqual(receiver.hits, 1)
        button.performClick(nil)
        XCTAssertEqual(button.state, .off)
    }

    // MARK: 分段卡片与色板

    func testSegmentedCardsSelectionCallback() {
        let cards = KSegmentedCards(segments: [.init(title: "A", symbol: "terminal"), .init(title: "B", symbol: "folder")])
        var picked: [Int] = []
        cards.onSelect = { picked.append($0) }
        cards.selectedIndex = 1
        XCTAssertTrue(picked.isEmpty, "程序设置不回调")
        cards.select(0)
        XCTAssertEqual(cards.selectedIndex, 0)
        XCTAssertEqual(picked, [0])
        cards.select(5)
        XCTAssertEqual(picked, [0], "越界忽略")
    }

    func testPaletteHasSixteenColorsAndSelectsAndClears() {
        XCTAssertEqual(SessionColorPresets.all.count, 16)
        XCTAssertEqual(Set(SessionColorPresets.all.map { [$0.r, $0.g, $0.b] }).count, 16, "颜色不应重复")
        let palette = ColorPaletteView(frame: NSRect(x: 0, y: 0, width: 240, height: 56))
        palette.setColors(SessionColorPresets.all)
        var picked: [FfiRgb?] = []
        palette.onSelect = { picked.append($0) }

        palette.choose(index: 3)
        XCTAssertEqual(palette.selected, SessionColorPresets.all[3])
        palette.choose(index: 3)
        XCTAssertNil(palette.selected, "再次点击取消选择")
        XCTAssertEqual(picked, [SessionColorPresets.all[3], nil])
    }

    func testPaletteLaysOutTwoRowsOfEight() {
        let palette = ColorPaletteView()
        palette.setColors(SessionColorPresets.all)
        XCTAssertEqual(palette.intrinsicContentSize.height, ColorPaletteView.pitch * 2)
        let pitch = ColorPaletteView.pitch
        XCTAssertEqual(palette.index(at: NSPoint(x: pitch * 0.5, y: pitch * 0.5)), 0)
        XCTAssertEqual(palette.index(at: NSPoint(x: pitch * 7.5, y: pitch * 0.5)), 7)
        XCTAssertEqual(palette.index(at: NSPoint(x: pitch * 0.5, y: pitch * 1.5)), 8)
        XCTAssertNil(palette.index(at: NSPoint(x: pitch * 8.5, y: pitch * 0.5)))
        XCTAssertNil(palette.index(at: NSPoint(x: pitch * 0.5, y: pitch * 2.5)))
    }

    // MARK: 浮动面板

    private func makePanel() -> FloatingPanel {
        let panel = FloatingPanel(content: NSView(), size: NSSize(width: 200, height: 100))
        return panel
    }

    func testPanelClosesOnEscape() {
        let panel = makePanel()
        var dismissed = 0
        panel.onDismiss = { dismissed += 1 }
        panel.cancelOperation(nil)
        XCTAssertEqual(dismissed, 1)
        panel.cancelOperation(nil)
        XCTAssertEqual(dismissed, 1, "dismiss 幂等，回调只触发一次")
    }

    func testPanelClosesOnOutsideClickButNotInsideClick() {
        let panel = makePanel()
        var dismissed = 0
        panel.onDismiss = { dismissed += 1 }
        panel.handleMouseDown(in: panel)
        XCTAssertEqual(dismissed, 0)
        panel.handleMouseDown(in: nil)
        XCTAssertEqual(dismissed, 1)
    }

    func testPanelClosesOnResignKeyUnlessMenuIsOpen() {
        let panel = makePanel()
        var dismissed = 0
        panel.onDismiss = { dismissed += 1 }
        panel.withoutAutoDismiss { panel.handleResignKey() }
        XCTAssertEqual(dismissed, 0, "菜单展开期间失焦不应关闭")
        panel.handleResignKey()
        XCTAssertEqual(dismissed, 1)
    }

    func testPanelResizeKeepsTopLeft() {
        let panel = makePanel()
        panel.setFrameOrigin(NSPoint(x: 50, y: 500))
        let top = panel.frame.maxY, left = panel.frame.minX
        panel.resize(to: NSSize(width: 200, height: 160))
        XCTAssertEqual(panel.frame.maxY, top)
        XCTAssertEqual(panel.frame.minX, left)
        XCTAssertEqual(panel.frame.height, 160)
    }

    // MARK: 外观切换

    func testColorsReresolveWhenAppearanceChanges() {
        let field = KTextField(frame: NSRect(x: 0, y: 0, width: 200, height: 30))
        let window = host(field, appearance: .darkAqua)
        defer { window.close() }
        field.updateLayer()
        XCTAssertEqual(field.layer?.backgroundColor, resolved(DesignTokens.Color.bgField, .darkAqua))
        let dark = field.layer?.backgroundColor

        window.appearance = NSAppearance(named: .aqua)
        field.updateLayer()
        XCTAssertEqual(field.layer?.backgroundColor, resolved(DesignTokens.Color.bgField, .aqua))
        XCTAssertNotEqual(field.layer?.backgroundColor, dark, "浅色与深色的 bg.field 必须不同")
    }

    func testCardAndDialogShellReresolveWhenAppearanceChanges() {
        let card = KCard(title: "Session", symbol: "tag")
        card.frame = NSRect(x: 0, y: 0, width: 300, height: 100)
        let window = host(card, appearance: .aqua)
        defer { window.close() }
        card.updateLayer()
        XCTAssertEqual(card.layer?.backgroundColor, resolved(DesignTokens.Color.bgCard, .aqua))
        XCTAssertEqual(card.layer?.borderColor, resolved(DesignTokens.Color.borderCard, .aqua))
        XCTAssertEqual(card.layer?.cornerRadius, DesignTokens.Radius.lg)
        window.appearance = NSAppearance(named: .darkAqua)
        card.updateLayer()
        XCTAssertEqual(card.layer?.backgroundColor, resolved(DesignTokens.Color.bgCard, .darkAqua))
    }

    func testSegmentCardsSelectedUsesAccentTokens() {
        let cards = KSegmentedCards(segments: [.init(title: "A", symbol: "terminal"), .init(title: "B", symbol: "folder")])
        cards.frame = NSRect(x: 0, y: 0, width: 300, height: 60)
        let window = host(cards, appearance: .darkAqua)
        defer { window.close() }
        cards.selectedIndex = 1
        cards.layoutSubtreeIfNeeded()
        let segments = cards.arrangedSubviews.compactMap { $0 as? StyledBoxView }
        XCTAssertEqual(segments.count, 2)
        segments.forEach { $0.updateLayer() }
        XCTAssertEqual(segments[1].layer?.backgroundColor, resolved(DesignTokens.Color.bgSegmentSelected, .darkAqua))
        XCTAssertEqual(segments[1].layer?.borderColor, resolved(DesignTokens.Color.borderSegmentSelected, .darkAqua))
        XCTAssertNotEqual(segments[0].layer?.backgroundColor, segments[1].layer?.backgroundColor)
    }
}
