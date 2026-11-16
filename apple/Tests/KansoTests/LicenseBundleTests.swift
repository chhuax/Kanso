import AppKit
import XCTest
@testable import Kanso

@MainActor
final class LicenseBundleTests: XCTestCase {
    func testAppContainsLicenseAndThirdPartyNotices() throws {
        let licenseURL = try XCTUnwrap(Bundle.main.url(forResource: "LICENSE", withExtension: nil))
        let noticesURL = try XCTUnwrap(Bundle.main.url(forResource: "THIRD_PARTY_NOTICES", withExtension: "md"))
        XCTAssertTrue(try String(contentsOf: licenseURL, encoding: .utf8).contains("GNU GENERAL PUBLIC LICENSE"))
        let notices = try String(contentsOf: noticesURL, encoding: .utf8)
        XCTAssertTrue(notices.contains("License and notice texts"))
        XCTAssertTrue(notices.contains("Permission is hereby granted"))
    }

    func testAppMenuContainsOpenSourceLicensesAndAbout() throws {
        let settings = SettingsStore.attached(to: MemorySettingsBackend())
        let menu = AppDelegate.makeMainMenu(
            appearance: AppearanceController(settings: settings, apply: { _ in }),
            gutter: GutterModeController(settings: settings)
        )
        let appMenu = try XCTUnwrap(menu.items.first?.submenu)
        let licenses = try XCTUnwrap(appMenu.items.first { $0.action == #selector(AppDelegate.kansoShowOpenSourceLicenses(_:)) })
        XCTAssertEqual(licenses.title, String(localized: "Open Source Licenses…"))
        XCTAssertTrue(licenses.target === NSApp.delegate)
        XCTAssertTrue(appMenu.items.contains { $0.action == #selector(AppDelegate.kansoShowAbout(_:)) })
    }

    func testAboutCreditsContainLicenseSourceLinkAndLicenseInstructions() {
        let credits = AppDelegate.aboutCredits
        XCTAssertTrue(credits.string.contains(String(localized: "Licensed under GPL-3.0-or-later")))
        XCTAssertTrue(credits.string.contains(String(localized: "Open Source Licenses: Choose Open Source Licenses… in the Kanso menu to read third-party licenses and notices.")))
        let source = "https://github.com/chhuax/Kanso"
        let range = (credits.string as NSString).range(of: source)
        XCTAssertNotEqual(range.location, NSNotFound)
        if range.location != NSNotFound {
            XCTAssertEqual(credits.attribute(.link, at: range.location, effectiveRange: nil) as? URL, URL(string: source))
        }
    }
}
