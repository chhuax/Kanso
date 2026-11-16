import Foundation

/// 可选的终端配色（ui-shell.md §10：浅色与深色外观各自绑定一套）。
///
/// 配色数据来自 `TerminalPalette` 的生成常量；这里只是给每套配色一个稳定的 id 和显示名。
/// `id` 会写进用户设置，**发布后不得改名**（改名会让已保存的选择失效，回落到默认配色）。
/// 显示名是配色的专有名称，不做本地化。
public struct TerminalTheme: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let palette: TerminalPalette

    public static func == (lhs: TerminalTheme, rhs: TerminalTheme) -> Bool { lhs.id == rhs.id }

    public static let kansoLight = TerminalTheme(id: "kanso-light", name: "Kanso Light", palette: .kansoLight)
    public static let kansoDark = TerminalTheme(id: "kanso-dark", name: "Kanso Dark", palette: .kansoDark)

    /// 全部内置配色。浅色与深色两个下拉框都列出全部，不按明暗过滤。
    public static let builtIn: [TerminalTheme] = [.kansoLight, .kansoDark]

    /// 按 id 查找；未知 id（旧版本或新版本写入的）返回 nil，由调用方回落到该外观的默认配色。
    public static func theme(id: String) -> TerminalTheme? {
        builtIn.first { $0.id == id }
    }
}
