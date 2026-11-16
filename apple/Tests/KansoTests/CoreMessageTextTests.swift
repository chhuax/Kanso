import KansoBridge
import KansoUI
import XCTest

/// `CoreMessageText`：Swift 侧预格式化 + Rust 渲染器的端到端检查。
final class CoreMessageTextTests: XCTestCase {
    private func endpoint(_ via: [String] = []) -> FfiArgValue {
        .endpoint(host: "example.com", port: 22, via: via)
    }

    private func connectionLost(_ via: [String] = []) -> FfiMessage {
        FfiMessage(key: "ssh-connection-lost", args: [FfiMessageArg(name: "endpoint", value: endpoint(via))])
    }

    func testRendersEnglish() {
        XCTAssertEqual(
            CoreMessageText.text(connectionLost(), languages: ["en"]),
            "The connection to example.com:22 was lost."
        )
    }

    func testRendersSimplifiedChinese() {
        let text = CoreMessageText.text(connectionLost(), languages: ["zh-Hans"])
        XCTAssertTrue(text.contains("example.com:22"))
        XCTAssertTrue(text.unicodeScalars.contains { (0x4E00...0x9FFF).contains($0.value) }, text)
    }

    func testChineseVariantsNegotiate() {
        let zh = CoreMessageText.text(connectionLost(), languages: ["zh-Hans"])
        XCTAssertEqual(CoreMessageText.text(connectionLost(), languages: ["zh-Hans-CN"]), zh)
        XCTAssertEqual(CoreMessageText.text(connectionLost(), languages: ["zh-CN"]), zh)
        // 繁体不归到简体，回退英文。
        XCTAssertEqual(
            CoreMessageText.text(connectionLost(), languages: ["zh-Hant"]),
            CoreMessageText.text(connectionLost(), languages: ["en"])
        )
    }

    func testNoBidiIsolationMarks() {
        let text = CoreMessageText.text(connectionLost(["jump"]), languages: ["en"])
        XCTAssertFalse(text.unicodeScalars.contains { $0.value == 0x2068 || $0.value == 0x2069 }, text)
        XCTAssertTrue(text.contains("example.com:22 via jump"), text)
    }

    func testByteSizeIsFormattedBySystem() {
        let message = FfiMessage(key: "xfer-completed", args: [
            FfiMessageArg(name: "name", value: .str(value: "a.bin")),
            FfiMessageArg(name: "size", value: .byteSize(bytes: 1_500_000)),
        ])
        let en = CoreMessageText.text(message, languages: ["en"])
        XCTAssertTrue(en.contains("1.5 MB"), en)
        let zh = CoreMessageText.text(message, languages: ["zh-Hans"])
        XCTAssertTrue(zh.contains("1.5") && zh.contains("MB"), zh)
    }

    func testPreformatConvertsByteSizeAndListToStr() {
        let locale = Locale(identifier: "en")
        XCTAssertEqual(CoreMessageText.preformat(.byteSize(bytes: 1_500_000), locale: locale), .str(value: "1.5 MB"))
        XCTAssertEqual(CoreMessageText.preformat(.strList(values: ["a", "b"]), locale: locale), .str(value: "a and b"))
        // 其他类型原样。
        XCTAssertEqual(CoreMessageText.preformat(.int(value: 3), locale: locale), .int(value: 3))
    }

    func testListUsesLocaleOfFirstLanguage() {
        let message = FfiMessage(key: "auth-agent-unavailable", args: [
            FfiMessageArg(name: "details", value: .strList(values: ["x", "y"])),
        ])
        let en = CoreMessageText.text(message, languages: ["en"])
        XCTAssertTrue(en.contains("x and y"), en)
        let zh = CoreMessageText.text(message, languages: ["zh-Hans"])
        XCTAssertFalse(zh.contains("x and y"), zh)
    }

    func testUserErrorIsRenderedAndInternalKeepsDetail() {
        let user = FfiCoreError.User(message: connectionLost())
        XCTAssertEqual(
            CoreMessageText.text(for: user, languages: ["en"]),
            "The connection to example.com:22 was lost."
        )
        XCTAssertTrue(CoreMessageText.text(for: FfiCoreError.Internal(detail: "boom"), languages: ["en"]).contains("boom"))
    }
}
