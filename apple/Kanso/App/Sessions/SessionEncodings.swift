/// 会话可选编码（encoding-transfer.md §1.1）。
///
/// 配置里保存 WHATWG 标签（小写，UTF-8 存为空串）；显示名是本地化文字。
enum SessionEncodings {
    struct Option: Equatable {
        /// 保存到配置的标签；UTF-8 为空串。
        let label: String
        let title: String
    }

    static var all: [Option] {
        [
            Option(label: "", title: String(localized: "UTF-8 (Default)")),
            Option(label: "gb18030", title: String(localized: "GB18030 — Simplified Chinese")),
            Option(label: "gbk", title: String(localized: "GBK — Simplified Chinese")),
            Option(label: "big5", title: String(localized: "Big5 — Traditional Chinese")),
            Option(label: "shift_jis", title: String(localized: "Shift_JIS — Japanese")),
            Option(label: "euc-jp", title: String(localized: "EUC-JP — Japanese")),
            Option(label: "euc-kr", title: String(localized: "EUC-KR — Korean")),
            Option(label: "windows-1252", title: String(localized: "Windows-1252 / ISO-8859-1 — Western European")),
            Option(label: "windows-1251", title: String(localized: "Windows-1251 — Cyrillic")),
            Option(label: "koi8-r", title: String(localized: "KOI8-R — Cyrillic")),
        ]
    }

    /// 菜单选项：在固定列表之外，若当前值是手工编辑 / 导入带来的其他标签，也要列出来。
    /// 否则弹出菜单选不中任何项，保存时会悄悄把它改成 UTF-8。
    static func options(including current: String) -> [Option] {
        var list = all
        if !list.contains(where: { $0.label == current }) {
            list.append(Option(label: current, title: current))
        }
        return list
    }
}
