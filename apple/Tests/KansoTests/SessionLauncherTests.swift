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
}
