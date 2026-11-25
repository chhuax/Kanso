import AppKit
import KansoBridge
import KansoUI
@testable import Kanso
import XCTest

@MainActor
final class SessionLauncherTests: XCTestCase {
    private func makeStore() -> MemoryStore {
        MemoryStore(
            profiles: [
                SessionFixtures.ssh("1", "Zeta", host: "Alpha.example.com", user: "Root"),
                SessionFixtures.ssh("2", "beta", host: "db.internal", user: "deploy", group: "g2"),
                SessionFixtures.local("3", "Shell 10", shell: "/bin/zsh -l"),
                SessionFixtures.local("4", "Shell 2"),
                SessionFixtures.sftp("5", "files"),
                SessionFixtures.ssh("6", "alpha-two", group: "g1"),
            ],
            groups: [FfiGroup(id: "g2", name: "Prod"), FfiGroup(id: "g1", name: "Dev"), FfiGroup(id: "g3", name: "Empty")]
        )
    }

    func testSearchReusesLibrarySnapshotUntilDataChanges() throws {
        let store = makeStore()
        let model = SessionLauncherModel(store: store)
        XCTAssertEqual(store.profileReads, 1)
        XCTAssertEqual(store.groupReads, 1)
        for index in 0..<100 { model.query = "query \(index)" }
        XCTAssertEqual(store.profileReads, 1)
        XCTAssertEqual(store.groupReads, 1)
        _ = try model.createGroup(named: "New")
        XCTAssertEqual(store.profileReads, 2)
        XCTAssertEqual(store.groupReads, 2)
        model.query = ""
        XCTAssertTrue(model.groups.contains { $0.name == "New" })
    }

    func testSearchMatchesNameHostAndUserIgnoringCase() {
        let model = SessionLauncherModel(store: makeStore())
        model.query = "ZETA"
        XCTAssertEqual(model.visibleProfiles.map(\.id), ["1"])
        model.query = "alpha"  // 主机 Alpha.example.com 与名称 alpha-two
        XCTAssertEqual(Set(model.visibleProfiles.map(\.id)), ["1", "6"])
        model.query = "DEPLOY"  // 用户名
        XCTAssertEqual(model.visibleProfiles.map(\.id), ["2"])
        model.query = "  "
        XCTAssertEqual(model.visibleProfiles.count, 6)
    }

    func testSectionsUngroupedFirstThenGroupsSortedByName() {
        let model = SessionLauncherModel(store: makeStore())
        XCTAssertEqual(model.sections.map { $0.group?.name }, [nil, "Dev", "Empty", "Prod"])
        // 未分组按名称自然排序：Shell 2 在 Shell 10 之前；大小写不影响。
        XCTAssertEqual(model.sections[0].profiles.map(\.name), ["files", "Shell 2", "Shell 10", "Zeta"])
        XCTAssertEqual(model.sections[1].profiles.map(\.id), ["6"])
    }

    func testCollapsedGroupsAreSkippedByKeyboardAndRevealedDuringSearch() {
        let model = SessionLauncherModel(store: makeStore(), collapsedGroupIDs: ["g2"])
        model.select("6")
        model.toggleGroup("g1")
        XCTAssertEqual(model.selectedID, model.visibleProfiles.first?.id)
        XCTAssertFalse(model.visibleProfiles.contains { ["2", "6"].contains($0.id) })
        XCTAssertTrue(model.sections.contains { $0.group?.id == "g1" })

        model.query = "deploy"
        XCTAssertEqual(model.selectedID, "2")
        model.query = ""
        XCTAssertFalse(model.visibleProfiles.contains { $0.id == "2" })
        model.toggleGroup("g2")
        XCTAssertEqual(model.visibleProfiles.last?.id, "2")
    }

    func testTogglingGroupKeepsScrollPositionWhenSelectionIsOffscreen() throws {
        let group = FfiGroup(id: "dev", name: "Dev")
        let trailingGroup = FfiGroup(id: "prod", name: "Prod")
        let ungrouped = (0..<12).map { SessionFixtures.local("local-\($0)", "Local \($0)") }
        let trailing = (0..<12).map { SessionFixtures.ssh("prod-\($0)", "Prod \($0)", group: trailingGroup.id) }
        let member = SessionFixtures.ssh("dev-1", "Dev 1", group: group.id)
        let store = MemoryStore(profiles: ungrouped + [member] + trailing, groups: [group, trailingGroup])
        let model = SessionLauncherModel(store: store)
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { _ in }, edit: { _ in }, dismiss: {},
            openSettings: {}, withDialog: { _ in }
        )
        let launcher = SessionLauncherViewController(model: model, dialogs: AlertSessionDialogs(), actions: actions)
        launcher.view.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(launcher.view.subviews.compactMap { $0 as? NSScrollView }.first)
        let table = try XCTUnwrap(scroll.documentView as? NSTableView)
        let groupRow = ungrouped.count
        let y = table.rect(ofRow: groupRow).minY
        scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
        scroll.reflectScrolledClipView(scroll.contentView)
        XCTAssertGreaterThan(scroll.documentVisibleRect.minY, table.rect(ofRow: 0).maxY)

        let header = try XCTUnwrap(table.view(atColumn: 0, row: groupRow, makeIfNecessary: true) as? LauncherHeaderView)
        let button = try XCTUnwrap(header.subviews.compactMap { $0 as? KButton }.first { $0.symbol == "chevron.down" })
        button.performClick(nil)
        launcher.view.layoutSubtreeIfNeeded()

        XCTAssertEqual(table.numberOfRows, ungrouped.count + trailing.count + 2)
        XCTAssertEqual(scroll.documentVisibleRect.minY, y, accuracy: 1)
        XCTAssertTrue(launcher.control(
            launcher.searchField.textField, textView: NSTextView(), doCommandBy: #selector(NSResponder.moveDown(_:))
        ))
        XCTAssertEqual(model.selectedID, ungrouped[1].id)
        XCTAssertTrue(scroll.documentVisibleRect.intersects(table.rect(ofRow: 1)))
    }

    func testSingleClickConnects() throws {
        let model = SessionLauncherModel(store: makeStore())
        let target = model.visibleProfiles[1]
        var connected: [String] = []
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { connected.append($0) }, edit: { _ in }, dismiss: {},
            openSettings: {}, withDialog: { _ in }
        )
        let launcher = SessionLauncherViewController(model: model, dialogs: AlertSessionDialogs(), actions: actions)
        launcher.view.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(launcher.view.subviews.compactMap { $0 as? NSScrollView }.first)
        let table = try XCTUnwrap(scroll.documentView as? NSTableView)
        let row = try XCTUnwrap((0..<table.numberOfRows).compactMap { index -> LauncherProfileRowView? in
            guard let view = table.view(atColumn: 0, row: index, makeIfNecessary: true) as? LauncherProfileRowView,
                  view.accessibilityLabel() == target.name else { return nil }
            return view
        }.first)
        XCTAssertEqual(row.subviews.compactMap { ($0 as? KButton)?.symbol }, ["ellipsis"])

        row.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: .zero))
        XCTAssertEqual(connected, [target.id])
    }

    func testColorSwatchDoesNotEatTheClick() throws {
        var profile = SessionFixtures.ssh("1", "Zeta")
        profile.color = FfiRgb(r: 12, g: 34, b: 56)
        var connected: [String] = []
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { connected.append($0) }, edit: { _ in }, dismiss: {},
            openSettings: {}, withDialog: { _ in }
        )
        let launcher = SessionLauncherViewController(
            model: SessionLauncherModel(store: MemoryStore(profiles: [profile], groups: [])),
            dialogs: AlertSessionDialogs(),
            actions: actions
        )
        launcher.view.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(launcher.view.subviews.compactMap { $0 as? NSScrollView }.first)
        let table = try XCTUnwrap(scroll.documentView as? NSTableView)
        let row = try XCTUnwrap(table.view(atColumn: 0, row: 0, makeIfNecessary: true) as? LauncherProfileRowView)
        row.layoutSubtreeIfNeeded()
        let swatch = try XCTUnwrap(row.subviews.compactMap { $0 as? ColorSquareView }.first)
        XCTAssertFalse(swatch.isHidden)
        let point = swatch.convert(NSPoint(x: swatch.bounds.midX, y: swatch.bounds.midY), to: row)
        XCTAssertNil(swatch.hitTest(point))
        XCTAssertIdentical(row.hitTest(point), row)

        row.mouseDown(with: InputFixtures.mouse(.leftMouseDown, at: point))
        XCTAssertEqual(connected, ["1"])
    }

    func testSearchKeepsScrollPosition() throws {
        let profiles = (0..<30).map { SessionFixtures.local("local-\($0)", "Local \($0)") }
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { _ in }, edit: { _ in }, dismiss: {},
            openSettings: {}, withDialog: { _ in }
        )
        let launcher = SessionLauncherViewController(
            model: SessionLauncherModel(store: MemoryStore(profiles: profiles, groups: [])),
            dialogs: AlertSessionDialogs(),
            actions: actions
        )
        launcher.view.layoutSubtreeIfNeeded()
        let scroll = try XCTUnwrap(launcher.view.subviews.compactMap { $0 as? NSScrollView }.first)
        let table = try XCTUnwrap(scroll.documentView as? NSTableView)
        let y = table.rect(ofRow: 20).minY
        scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
        scroll.reflectScrolledClipView(scroll.contentView)

        launcher.searchField.stringValue = "local"
        launcher.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
        launcher.view.layoutSubtreeIfNeeded()

        XCTAssertEqual(scroll.documentVisibleRect.minY, y, accuracy: 1)
        XCTAssertTrue(launcher.control(
            launcher.searchField.textField, textView: NSTextView(), doCommandBy: #selector(NSResponder.moveDown(_:))
        ))
        XCTAssertTrue(scroll.documentVisibleRect.intersects(table.rect(ofRow: 1)))
    }

    func testEmptyGroupsHiddenWhileSearching() {
        let model = SessionLauncherModel(store: makeStore())
        model.query = "beta"
        XCTAssertEqual(model.sections.map { $0.group?.name }, ["Prod"])
    }

    func testMoveToGroupAndBackToUngrouped() throws {
        let store = makeStore()
        let model = SessionLauncherModel(store: store)
        try model.move(profileID: "1", toGroup: "g1")
        XCTAssertEqual(store.storedProfiles.first { $0.id == "1" }?.group, "g1")
        XCTAssertEqual(model.sections.first { $0.group?.id == "g1" }?.profiles.map(\.id), ["6", "1"])
        try model.move(profileID: "1", toGroup: nil)
        XCTAssertNil(store.storedProfiles.first { $0.id == "1" }?.group)
    }

    func testDeleteGroupConfirmationShowsImpactNumbers() throws {
        let store = makeStore()
        store.impact = FfiDeleteImpact(profiles: 7, snippets: 13)
        let model = SessionLauncherModel(store: store)
        let text = try model.deleteGroupConfirmation(FfiGroup(id: "g2", name: "Prod"))
        XCTAssertTrue(text.contains("7"), text)
        XCTAssertTrue(text.contains("13"), text)
    }

    func testDeleteGroupConfirmationUsesCoreImpact() async throws {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        defer { core.shutdown() }
        let model = SessionLauncherModel(store: core)
        let prod = try XCTUnwrap(core.groups().first)
        let impact = try core.groupDeleteImpact(id: prod.id)
        XCTAssertGreaterThan(impact.profiles, 0)
        let text = try model.deleteGroupConfirmation(prod)
        XCTAssertTrue(text.contains("\(impact.profiles)") && text.contains("\(impact.snippets)"), text)
        try await model.deleteGroup(prod)
        XCTAssertTrue(core.groups().isEmpty)
        XCTAssertEqual(core.profiles().count, 5 - Int(impact.profiles))
    }

    func testCreateAndRenameGroupTrimNames() throws {
        let store = makeStore()
        let model = SessionLauncherModel(store: store)
        XCTAssertNil(try model.createGroup(named: "   "))
        let created = try XCTUnwrap(try model.createGroup(named: "  New  "))
        XCTAssertEqual(created.name, "New")
        try model.renameGroup(created, to: " Renamed ")
        XCTAssertEqual(store.storedGroups.first { $0.id == created.id }?.name, "Renamed")
    }

    func testDeleteProfileRemovesIt() async throws {
        let model = SessionLauncherModel(store: makeStore())
        try await model.deleteProfile(id: "3")
        XCTAssertFalse(model.visibleProfiles.contains { $0.id == "3" })
    }

    func testSubtitles() {
        XCTAssertEqual(SessionLauncherModel.subtitle(for: SessionFixtures.ssh("a", "a", host: "h.io", user: "me")), "me@h.io:22")
        XCTAssertEqual(SessionLauncherModel.subtitle(for: SessionFixtures.local("b", "b", shell: "/bin/fish")), "/bin/fish")
        XCTAssertEqual(SessionLauncherModel.subtitle(for: SessionFixtures.local("c", "c")), String(localized: "Default Shell"))
    }

    func testKeyboardSelectionClampsAndResetsOnSearch() {
        let model = SessionLauncherModel(store: makeStore())
        let order = model.visibleProfiles.map(\.id)
        XCTAssertEqual(model.selectedID, order.first)
        model.moveSelection(by: -1)
        XCTAssertEqual(model.selectedID, order.first)
        model.moveSelection(by: 1)
        XCTAssertEqual(model.selectedID, order[1])
        for _ in 0..<20 { model.moveSelection(by: 1) }
        XCTAssertEqual(model.selectedID, order.last)
        model.query = "beta"
        XCTAssertEqual(model.selectedID, "2")
    }

    func testEmptyStoreHasNoProfiles() {
        let model = SessionLauncherModel(store: MemoryStore())
        XCTAssertFalse(model.hasAnyProfile)
        XCTAssertNil(model.selectedID)
        model.moveSelection(by: 1)
        XCTAssertNil(model.selectedID)
    }

    func testOpenProfileFailureSurfacesToErrorHandler() throws {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let controller = WorkspaceController(core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true))
        defer { controller.shutdown() }
        var errors = 0
        controller.errorHandler = { _ in errors += 1 }
        let local = try XCTUnwrap(core.profiles().first { $0.kindChoice == .local })
        let id = try controller.openProfile(local.id)
        XCTAssertEqual(controller.workspace.item(id)?.profileID, local.id)
        controller.perform { try controller.openProfile("00000000-0000-0000-0000-000000000000") }
        XCTAssertEqual(errors, 1)
    }

    func testSidebarSwitchesBetweenTabsAndSessions() {
        let dispatcher = CoreEventDispatcher()
        let core = KansoCore.newMock(listener: dispatcher)
        let controller = WorkspaceController(core: core, dispatcher: dispatcher, confirmer: StubCloseConfirmer(answer: true))
        defer { controller.shutdown() }
        let sidebar = TabSidebarViewController(controller: controller)
        sidebar.view.frame = NSRect(x: 0, y: 0, width: 260, height: 600)
        sidebar.view.layoutSubtreeIfNeeded()
        XCTAssertEqual(sidebar.activePage, .tabs)

        sidebar.showSessionsPage(focusSearch: false)
        XCTAssertEqual(sidebar.activePage, .sessions)
        XCTAssertEqual(sidebar.children.count, 1)
        let embedded = sidebar.children.first
        XCTAssertEqual(embedded?.view.isHidden, false)
        XCTAssertEqual(embedded?.view.frame.width, 260)

        sidebar.showTabsPage()
        XCTAssertEqual(sidebar.activePage, .tabs)
        XCTAssertEqual(embedded?.view.isHidden, true)
    }

    func testEmbeddedLauncherKeepsContainerSizeAndReloadsOnChange() throws {
        let store = makeStore()
        let model = SessionLauncherModel(store: store)
        let actions = SessionLauncherActions(
            newLocalShell: {}, newSession: {}, connect: { _ in }, edit: { _ in },
            dismiss: {}, openSettings: {}, withDialog: { _ in }
        )
        let launcher = SessionLauncherViewController(model: model, dialogs: AlertSessionDialogs(), actions: actions, embedded: true)
        launcher.view.frame = NSRect(x: 0, y: 0, width: 240, height: 520)
        launcher.view.layoutSubtreeIfNeeded()
        XCTAssertEqual(launcher.view.frame.size, NSSize(width: 240, height: 520))

        _ = try model.createGroup(named: "Ops")
        XCTAssertTrue(model.groups.contains { $0.name == "Ops" })
        XCTAssertEqual(launcher.view.frame.size, NSSize(width: 240, height: 520))
    }
}
