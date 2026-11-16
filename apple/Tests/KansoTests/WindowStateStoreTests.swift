@testable import Kanso
import XCTest

final class WindowStateStoreTests: XCTestCase {
    private func makeSuite() -> (UserDefaults, String) {
        let name = "kanso.test.\(UUID().uuidString)"
        return (UserDefaults(suiteName: name)!, name)
    }

    func testZoomedStateRoundTripsInInjectedSuite() {
        let (defaults, name) = makeSuite()
        defer { defaults.removePersistentDomain(forName: name) }
        let store = WindowStateStore(defaults: defaults)
        XCTAssertFalse(store.wasZoomed)
        store.save(isZoomed: true)
        XCTAssertTrue(store.wasZoomed)
        XCTAssertTrue(WindowStateStore(defaults: defaults).wasZoomed)
        store.save(isZoomed: false)
        XCTAssertFalse(store.wasZoomed)
    }

    func testKeyHasKansoPrefix() {
        XCTAssertTrue(WindowStateStore.zoomedKey.hasPrefix("kanso."))
    }
}
