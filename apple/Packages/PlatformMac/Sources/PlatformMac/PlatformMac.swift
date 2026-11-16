/// macOS 平台集成的命名空间占位（architecture §1、§9）。
///
/// 将来放 core 的 platform trait 在 macOS 上的实现：Keychain（见 `KeychainSecretStore`）、security-scoped bookmark、
/// StoreKit 2、file promise、Secure Enclave，以及仅 Direct 渠道的 Sparkle。P0.8 只建空壳。
public enum PlatformMac {}
