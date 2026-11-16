import AppKit

/// 主窗口。唯一的定制：让"双击标题栏"在透明标题栏下照常生效。
///
/// 标题栏里放了搜索入口、侧栏开关、Git 按钮等附件视图，落在它们之间空白处的双击不一定能到系统的
/// 标题栏处理，所以这里在分发事件前自己判断一次（控件上的双击照常交给控件）。
@MainActor
final class KansoWindow: NSWindow {
    override func sendEvent(_ event: NSEvent) {
        if event.type == .leftMouseDown, event.clickCount == 2, handleTitlebarDoubleClick(event) {
            return
        }
        super.sendEvent(event)
    }

    /// 双击落在标题栏区域且不在可交互控件上时，按系统设置执行动作。返回是否已处理。
    private func handleTitlebarDoubleClick(_ event: NSEvent) -> Bool {
        guard !styleMask.contains(.fullScreen) else { return false }
        let point = event.locationInWindow
        guard TitlebarDoubleClick.isInTitlebar(point: point, contentLayoutMaxY: contentLayoutRect.maxY) else {
            return false
        }
        let hit = contentView?.superview?.hitTest(point) ?? contentView?.hitTest(point)
        guard !TitlebarDoubleClick.isInteractive(hit) else { return false }
        switch TitlebarDoubleClick.action(preference: UserDefaults.standard.string(forKey: TitlebarDoubleClick.preferenceKey)) {
        case .zoom: zoom(nil)
        case .minimize: miniaturize(nil)
        case .none: break
        }
        return true
    }
}

/// 标题栏双击的判定规则（纯逻辑，便于测试）。
enum TitlebarDoubleClick {
    enum Action: Equatable { case zoom, minimize, none }

    /// 系统设置"桌面与程序坞 → 连按窗口标题栏以…"写入全局偏好的键。
    static let preferenceKey = "AppleActionOnDoubleClick"

    /// 按用户的系统设置决定动作。"Minimize" 最小化、"None" 不做事；其余（"Maximize"、新系统的 "Fill"、
    /// 未设置）都按缩放处理——AppKit 没有公开"填充"的接口，缩放是最接近的行为，也是用户要的
    /// "在最大化与原大小之间切换"。
    static func action(preference: String?) -> Action {
        switch preference {
        case "Minimize": .minimize
        case "None": .none
        default: .zoom
        }
    }

    /// 窗口坐标（原点在左下）中的点是否落在标题栏那一条：`contentLayoutRect` 是不被标题栏遮挡的区域，
    /// 它的上沿以上就是标题栏。
    static func isInTitlebar(point: NSPoint, contentLayoutMaxY: CGFloat) -> Bool {
        point.y >= contentLayoutMaxY
    }

    /// 点中的是可交互控件（按钮、输入框、文本视图）时让控件自己处理，不能把一次快速双击按钮变成缩放窗口。
    @MainActor
    static func isInteractive(_ view: NSView?) -> Bool {
        var current = view
        while let v = current {
            if let control = v as? NSControl, control.isEnabled { return true }
            if v is NSText { return true }
            current = v.superview
        }
        return false
    }
}
