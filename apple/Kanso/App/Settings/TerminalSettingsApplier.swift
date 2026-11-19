import AppKit
import TerminalKit

/// 把显示设置与外观设置应用到终端视图。
///
/// 幂等：相同设置重复应用不会触发多余的重排（字体只在真的变了时才设置，因为设置字体会清空字形图集并重算网格）。
/// 终端选项（回滚、默认光标样式）不在这里——它们下发给 core，由 `SettingsStore` 负责。
@MainActor
enum TerminalSettingsApplier {
    static func apply(_ settings: SettingsStore, to view: TerminalView) {
        let display = settings.display
        let font = TerminalFontResolver.font(family: display.fontFamily, size: display.fontSize).font
        if view.font != font { view.font = font }
        if view.lineHeight != display.lineHeight { view.lineHeight = display.lineHeight }
        if view.isMinimumContrastEnabled != display.minimumContrastEnabled {
            view.isMinimumContrastEnabled = display.minimumContrastEnabled
        }
        if view.minimumContrastRatio != display.minimumContrastRatio {
            view.minimumContrastRatio = display.minimumContrastRatio
        }
        if view.copyOnSelect != display.copyOnSelect {
            view.copyOnSelect = display.copyOnSelect
        }
        view.lightPalette = settings.appearance.resolvedLightTheme(in: settings.themes).palette
        view.darkPalette = settings.appearance.resolvedDarkTheme(in: settings.themes).palette
    }
}
