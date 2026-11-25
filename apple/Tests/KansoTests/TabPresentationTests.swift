import KansoUI
import XCTest

final class TabPresentationTests: XCTestCase {
    private func local(cwd: String?, branch: String? = nil, name: String = "zsh") -> TabItem {
        TabItem(id: 1, kind: .local, name: name, cwd: cwd, gitBranch: branch)
    }

    func testPrimaryTitleHomeReplacement() {
        let cases: [(String, String)] = [
            ("/Users/a", "~"),
            ("/Users/a/", "~"),
            ("/Users/ab", "/Users/ab"),
            ("/Users/a/x", "~/x"),
            ("/Users/a/x/", "~/x"),
            ("/tmp", "/tmp"),
            ("/", "/"),
        ]
        for (cwd, expected) in cases {
            XCTAssertEqual(TabPresentation.primaryTitle(local(cwd: cwd), homeDirectory: "/Users/a"), expected, cwd)
        }
        XCTAssertEqual(TabPresentation.primaryTitle(local(cwd: "/Users/a/x"), homeDirectory: "/Users/a/"), "~/x")
        XCTAssertEqual(TabPresentation.primaryTitle(local(cwd: "/x"), homeDirectory: "/"), "/x")
    }

    func testPrimaryTitleFallsBackToName() {
        XCTAssertEqual(TabPresentation.primaryTitle(local(cwd: nil), homeDirectory: "/Users/a"), "zsh")
        let ssh = TabItem(id: 2, kind: .ssh, name: "prod", cwd: "/Users/a/x")
        XCTAssertEqual(TabPresentation.primaryTitle(ssh, homeDirectory: "/Users/a"), "prod")
        let sftp = TabItem(id: 3, kind: .sftp, name: "files", cwd: "/srv")
        XCTAssertEqual(TabPresentation.primaryTitle(sftp, homeDirectory: "/Users/a"), "files")
    }

    func testSubtitle() {
        XCTAssertEqual(TabPresentation.subtitle(local(cwd: "/x", branch: "main")), "main")
        XCTAssertNil(TabPresentation.subtitle(local(cwd: "/x", branch: nil)))
        XCTAssertNil(TabPresentation.subtitle(local(cwd: "/x", branch: "")))
        let ssh = TabItem(id: 2, kind: .ssh, name: "p", gitBranch: "main")
        XCTAssertNil(TabPresentation.subtitle(ssh))
    }

    func testCustomTitlePrimaryAndSubtitle() {
        var localTab = TabItem(id: 1, kind: .local, name: "zsh", customTitle: "构建", cwd: "/Users/a/work")
        XCTAssertEqual(TabPresentation.primaryTitle(localTab, homeDirectory: "/Users/a"), "构建")
        XCTAssertEqual(TabPresentation.subtitle(localTab, homeDirectory: "/Users/a"), "~/work")
        XCTAssertEqual(TabPresentation.displayPath("/Users/a/work", homeDirectory: "/Users/a"), "~/work")
        localTab.gitBranch = "main"
        XCTAssertEqual(TabPresentation.subtitle(localTab, homeDirectory: "/Users/a"), "~/work · main")

        var ssh = TabItem(id: 2, kind: .ssh, name: "prod", customTitle: "日志")
        XCTAssertEqual(TabPresentation.primaryTitle(ssh, homeDirectory: "/Users/a"), "日志")
        XCTAssertEqual(TabPresentation.subtitle(ssh, homeDirectory: "/Users/a"), "prod")
        ssh.customTitle = "prod"
        XCTAssertNil(TabPresentation.subtitle(ssh, homeDirectory: "/Users/a"))
    }

    func testShortcutLabel() {
        XCTAssertEqual(TabPresentation.shortcutLabel(sidebarIndex: 0), "⌘1")
        XCTAssertEqual(TabPresentation.shortcutLabel(sidebarIndex: 8), "⌘9")
        XCTAssertEqual(TabPresentation.shortcutLabel(sidebarIndex: 9), "10")
        XCTAssertEqual(TabPresentation.shortcutLabel(sidebarIndex: 20), "21")
    }

    func testFNVKnownVectors() {
        XCTAssertEqual(TabPresentation.fnv1a64(""), 0xcbf2_9ce4_8422_2325)
        XCTAssertEqual(TabPresentation.fnv1a64("a"), 0xaf63_dc4c_8601_ec8c)
        XCTAssertEqual(TabPresentation.fnv1a64("foobar"), 0x8594_4171_f739_67e8)
    }

    func testColorIndexStable() {
        let byName = TabItem(id: 1, kind: .local, name: "a")
        XCTAssertEqual(TabPresentation.colorIndex(for: byName, paletteCount: 8), 4)
        XCTAssertEqual(TabPresentation.colorIndex(for: byName, paletteCount: 8), 4)
        // profileID 优先于 name。
        let byProfile = TabItem(id: 2, kind: .ssh, name: "ignored", profileID: "prod-1")
        XCTAssertEqual(TabPresentation.colorIndex(for: byProfile, paletteCount: 8), 6)
        XCTAssertEqual(TabPresentation.colorIndex(for: byProfile, paletteCount: 12), 10)
        XCTAssertEqual(TabPresentation.colorIndex(for: byProfile, paletteCount: 0), 0)
    }

    func testPathFilter() {
        let win = local(cwd: "C:\\Users\\Foo\\Proj")
        XCTAssertTrue(TabPresentation.matchesPathFilter(win, query: "users/foo"))
        XCTAssertTrue(TabPresentation.matchesPathFilter(win, query: "USERS\\FOO"))
        let unix = local(cwd: "/Users/Foo/proj")
        XCTAssertTrue(TabPresentation.matchesPathFilter(unix, query: "foo\\PROJ"))
        XCTAssertFalse(TabPresentation.matchesPathFilter(unix, query: "bar"))
        XCTAssertTrue(TabPresentation.matchesPathFilter(unix, query: "  proj  "))
    }

    func testPathFilterEmptyAndNonLocal() {
        let ssh = TabItem(id: 2, kind: .ssh, name: "prod", cwd: "/srv/prod")
        XCTAssertFalse(TabPresentation.matchesPathFilter(ssh, query: "prod"))
        XCTAssertFalse(TabPresentation.matchesPathFilter(ssh, query: "srv"))
        XCTAssertTrue(TabPresentation.matchesPathFilter(ssh, query: ""))
        XCTAssertTrue(TabPresentation.matchesPathFilter(ssh, query: "   "))
        XCTAssertFalse(TabPresentation.matchesPathFilter(local(cwd: nil), query: "x"))
        XCTAssertTrue(TabPresentation.matchesPathFilter(local(cwd: nil), query: ""))
    }
}
