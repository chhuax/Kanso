// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "PlatformMac",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "PlatformMac", targets: ["PlatformMac"]),
    ],
    dependencies: [
        .package(path: "../KansoBridge"),
    ],
    targets: [
        .target(
            name: "PlatformMac",
            dependencies: [.product(name: "KansoBridge", package: "KansoBridge")]
        ),
    ]
)
