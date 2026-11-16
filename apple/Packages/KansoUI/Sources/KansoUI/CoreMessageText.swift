import Foundation
import KansoBridge

/// 把 core 产生的 `FfiMessage` 渲染成当前语言的文字。
///
/// 渲染本身由 Rust 的 `kanso-l10n`（fluent-bundle）完成，`locales/*.ftl` 是唯一来源；
/// Swift 只负责两件 Rust 做不好的事：用系统格式化器处理字节数与列表。
public enum CoreMessageText {
    /// 渲染消息。
    ///
    /// - Parameter languages: 按偏好排序的语言标识。默认 `Bundle.main.preferredLocalizations`，
    ///   它已经反映了系统设置里的“按 App 设置语言”，所以界面文字与 core 消息语言一致，
    ///   不需要应用内另做语言选择。测试可显式传 `["zh-Hans"]` / `["en"]`。
    public static func text(
        _ message: FfiMessage,
        languages: [String] = Bundle.main.preferredLocalizations
    ) -> String {
        // 格式化器必须使用与 Rust 渲染相同的语言，否则会出现“已复制 1.5 MB 到 …”
        // 这种半中半英的句子；Rust 按 languages 首项协商，这里同样取首项。
        let locale = Locale(identifier: languages.first ?? "en")
        let prepared = FfiMessage(
            key: message.key,
            args: message.args.map { FfiMessageArg(name: $0.name, value: preformat($0.value, locale: locale)) }
        )
        return renderMessage(message: prepared, languages: languages)
    }

    /// 用户可见的错误文字：`FfiCoreError.User` 渲染成本地化文字；
    /// 其他错误（`Internal` 等）是技术细节，保持英文原文，只用于排查。
    public static func text(for error: Error, languages: [String] = Bundle.main.preferredLocalizations) -> String {
        if let core = error as? FfiCoreError, case .User(let message) = core {
            return text(message, languages: languages)
        }
        return String(describing: error)
    }

    /// 字节数、列表转成 `Str`，其余原样。Rust 侧对这两种类型只有兜底格式，
    /// 平台格式化器才能给出符合系统习惯的结果（`ByteCountFormatter` 同款的 `.file` 风格）。
    public static func preformat(_ value: FfiArgValue, locale: Locale) -> FfiArgValue {
        switch value {
        case .byteSize(let bytes):
            // 用 FormatStyle 而不是 `ByteCountFormatter`：两者同为 `.file` 风格（十进制单位），
            // 但 `ByteCountFormatter` 不能指定 locale，只能跟随进程语言，无法保证与 `languages` 首项一致。
            let style = ByteCountFormatStyle(style: .file, allowedUnits: .all, spellsOutZero: false, includesActualByteCount: false, locale: locale)
            return .str(value: Int64(clamping: bytes).formatted(style))
        case .strList(let values):
            let formatter = ListFormatter()
            formatter.locale = locale
            return .str(value: formatter.string(from: values) ?? values.joined(separator: ", "))
        default:
            return value
        }
    }
}
