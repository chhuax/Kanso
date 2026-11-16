// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "TerminalKit",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "TerminalKit", targets: ["TerminalKit"]),
    ],
    dependencies: [
        .package(path: "../KansoBridge"),
    ],
    targets: [
        .target(
            name: "TerminalKit",
            dependencies: [.product(name: "KansoBridge", package: "KansoBridge")]
        ),
    ]
)
