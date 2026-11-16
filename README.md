# Kanso

[简体中文](README.zh-CN.md)

Kanso is a native macOS terminal for local shells and SSH, with a built-in SFTP file browser, AI CLI-aware tabs, and command blocks. It uses Swift/AppKit and Metal for the interface and Rust for the core.

## Features

- Local shell sessions and SSH connections, including key, password, ssh-agent and jump-host authentication.
- Tabs, split panes, saved sessions and OpenSSH configuration import.
- Local and SFTP file browsing, uploads and downloads, and a dual-pane transfer view.
- Tab icons and status indicators for recognized AI command-line tools.
- Command blocks with exit status, duration and copy actions when the shell supplies OSC 133 markers; local zsh integration is included.
- Terminal search, command history and completion, a Sender panel for saved commands, and a Git changes panel for local sessions.
- Light and dark appearances, English and Simplified Chinese, and configurable keyboard shortcuts.

## Screenshots

These are reference images from the UI snapshot tests, using sample data.

![Command blocks in dark appearance](apple/Tests/Snapshots/command-blocks-dark.png)

![Dual-pane SFTP file browser](apple/Tests/Snapshots/sftp-dual-light.png)

## Install

Requires **macOS 14 or later** and an **Apple Silicon** Mac. Intel Macs are not supported by the current build.

Download `Kanso-<version>-macOS-arm64.dmg` from [Releases](https://github.com/chhuax/Kanso/releases), open it, and drag Kanso into Applications.

The app is ad-hoc signed and **not notarized**. For the first launch, right-click Kanso and choose **Open**. If macOS still blocks a download you trust, remove its quarantine attribute:

```sh
xattr -dr com.apple.quarantine /Applications/Kanso.app
```

## Build from source

Install Xcode with Swift 6 support (Xcode 16 or later), select it as the active developer directory, and install [rustup](https://rustup.rs/) and XcodeGen. The repository pins Rust **1.94.1**, rustfmt, clippy and the `aarch64-apple-darwin` target in `rust-toolchain.toml`; rustup installs them when you run Cargo here.

```sh
brew install xcodegen
cargo xtask apple-build --channel direct
```

The Debug app is at `target/apple/DerivedData.noindex/Build/Products/Debug-Direct/Kanso.app`.

```sh
cargo xtask apple-build --channel direct --configuration release
cargo xtask dmg
```

`dmg` builds the Release Direct app and writes `target/dist/Kanso-1.0.0-macOS-arm64.dmg`, then prints its SHA-256. It uses the existing ad-hoc signing configuration and does not notarize the app.

To run all checks, including Apple builds and tests:

```sh
cargo install cargo-deny --locked
cargo xtask ci
```

Run `cargo xtask notices` after dependency changes to regenerate [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). More build details are in [docs/apple-build.md](docs/apple-build.md).

## Roadmap and legacy version

Linux and Windows versions are planned, using GPUI. These ports are not available yet. Current Windows and Linux users should use the **0.x** releases of [chhuax/Kanso-legacy](https://github.com/chhuax/Kanso-legacy).

This repository is an independent rewrite and shares no code with Kanso-legacy, the earlier Tauri app based on EdgeTerm.

## Privacy

Kanso does not collect telemetry. It has no analytics SDK or usage-reporting service. SSH and SFTP connect to the hosts you configure; AI command-line tools run as separate programs and have their own privacy policies.

## License

Copyright (c) chhuax. Source code is licensed under **GPL-3.0-or-later**; see [LICENSE](LICENSE). The Kanso name and application icon are excluded from the GPL grant. Third-party code and tool icons retain their own licenses and trademarks; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [asset provenance](docs/assets-provenance.md).
