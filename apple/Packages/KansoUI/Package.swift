// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "KansoUI",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "KansoUI", targets: ["KansoUI"]),
    ],
    dependencies: [
        .package(path: "../TerminalKit"),
        .package(path: "../KansoBridge"),
    ],
    targets: [
        .target(
            name: "KansoUI",
            dependencies: [
                .product(name: "TerminalKit", package: "TerminalKit"),
                .product(name: "KansoBridge", package: "KansoBridge"),
            ]
        ),
    ]
)
