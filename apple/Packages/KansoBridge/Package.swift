// swift-tools-version: 6.0
import PackageDescription

// 不在这里引用 Rust 二进制产物：binaryTarget 的路径是静态的，无法按发行渠道切换。
// libkanso_ffi.a 由 App target 的 Run Script 构建并链接，见 docs/apple-build.md。
let package = Package(
    name: "KansoBridge",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "KansoBridge", targets: ["KansoBridge"]),
    ],
    targets: [
        // 手写 C ABI（帧快照），头文件是 crates/kanso-ffi/include 的符号链接。
        .target(name: "CKansoFFI"),
        // UniFFI 生成的 C 头文件与 modulemap，由 `cargo xtask bindgen` 生成并入库。
        .target(name: "KansoFFIC"),
        .target(name: "KansoBridge", dependencies: ["CKansoFFI", "KansoFFIC"]),
    ]
)
