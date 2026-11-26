import AppKit

/// 终端字体的解析与枚举（ui-shell.md §10：不内置字体，字体族留空就用系统默认；可手填任意名称）。
enum TerminalFontResolver {
    /// 按设置里的家族名与字号取字体。
    ///
    /// - 空串：默认使用 Menlo（macOS 内置等宽字体；不存在时回退系统等宽字体）。
    /// - 家族名（"Menlo"）或字体全名 / PostScript 名（"JetBrainsMono-Regular"）都可以：用户手填的名称不一定是家族名。
    /// - 找不到：回落默认等宽字体，`isFallback` 为 true，设置页据此提示——不能静默换字体让用户以为设置生效了。
    static func font(family: String, size: CGFloat) -> (font: NSFont, isFallback: Bool) {
        let system = NSFont(name: "Menlo", size: size) ?? NSFont.monospacedSystemFont(ofSize: size, weight: .regular)
        let name = family.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return (system, false) }
        // 先按字体名（全名 / PostScript 名）找：NSFont(name:) 对不存在的名字返回 nil，不会悄悄换成别的字体。
        if let font = NSFont(name: name, size: size) { return (font, false) }
        // 再按家族名找。NSFontDescriptor 对未知家族会返回"最接近"的字体（通常是系统字体），
        // 所以必须核对结果的家族名，否则会把任意输入都当成"找到了"。
        let descriptor = NSFontDescriptor(fontAttributes: [.family: name])
        if let font = NSFont(descriptor: descriptor, size: size),
           font.familyName?.caseInsensitiveCompare(name) == .orderedSame {
            return (font, false)
        }
        return (system, true)
    }

    /// 枚举结果缓存。放在主线程隔离的容器里：只有 `installedMonospacedFamilies`（主线程）读写它。
    @MainActor
    private enum Cache {
        static var families: [String]?
    }

    /// 本机已安装的等宽字体家族（按名称排序）。
    ///
    /// 枚举全部家族并逐个判断有一定开销（几百个家族），只在设置页第一次需要时做一次并缓存；
    /// 运行期间新装的字体要重启后才会出现在列表里，但仍可手填。必须在主线程调用（`NSFontManager`）。
    @MainActor
    static func installedMonospacedFamilies() -> [String] {
        if let cached = Cache.families { return cached }
        let families = NSFontManager.shared.availableFontFamilies.filter { family in
            let descriptor = NSFontDescriptor(fontAttributes: [.family: family])
            return NSFont(descriptor: descriptor, size: 12)?.isFixedPitch == true
        }
        let sorted = families.sorted { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending }
        Cache.families = sorted
        return sorted
    }
}
