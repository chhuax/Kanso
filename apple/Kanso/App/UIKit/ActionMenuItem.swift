import AppKit

/// 带回调的菜单项：避免为每个动作写 selector 与 representedObject 样板。
@MainActor
final class ActionMenuItem: NSMenuItem {
    private let handler: () -> Void

    init(_ title: String, checked: Bool = false, handler: @escaping () -> Void) {
        self.handler = handler
        super.init(title: title, action: #selector(fire), keyEquivalent: "")
        target = self
        state = checked ? .on : .off
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("ActionMenuItem 只支持代码创建") }

    @objc private func fire() { handler() }
}
