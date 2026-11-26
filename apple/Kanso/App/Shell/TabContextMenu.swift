import AppKit
import KansoUI

/// 标签右键菜单（ui-shell.md §2.4），竖向侧栏与顶部标签条共用。
///
/// 菜单项的 target 是本对象，所以持有者必须让它活得比弹出的菜单久（作为属性持有即可）。
@MainActor
final class TabContextMenu: NSObject, NSMenuItemValidation {
    private let controller: WorkspaceController
    private var workspace: Workspace { controller.workspace }
    /// 防止连续双击叠出多个重命名对话框。
    private var isRenaming = false

    init(controller: WorkspaceController) {
        self.controller = controller
    }

    /// 菜单项载体：关闭动作的作用域与参照会话。
    private final class CloseAction: NSObject {
        let scope: CloseScope?
        let session: SessionID
        init(scope: CloseScope?, session: SessionID) {
            self.scope = scope
            self.session = session
        }
    }

    /// - Parameter horizontal: 标签横向排列（顶部标签条）时，`.above` / `.below` 叫"左侧 / 右侧"；
    ///   作用域本身不变，都是同一分屏组内排在前面 / 后面的标签。
    func makeMenu(for session: SessionID, horizontal: Bool) -> NSMenu {
        let menu = NSMenu()
        let renameItem = NSMenuItem(title: String(localized: "Rename…"), action: #selector(renameFromMenu(_:)), keyEquivalent: "")
        renameItem.target = self
        renameItem.representedObject = NSNumber(value: session)
        menu.addItem(renameItem)
        let item = workspace.item(session)
        // 断开的标签可用原已保存会话重新连接（新会话，见 reconnectSession）。
        if case .closed = item?.connection, item?.profileID != nil {
            let reconnect = NSMenuItem(title: String(localized: "Reconnect"), action: #selector(reconnectFromMenu(_:)), keyEquivalent: "")
            reconnect.target = self
            reconnect.representedObject = NSNumber(value: session)
            menu.addItem(reconnect)
        }
        // 只有由已保存会话打开的标签才能回到它的配置。
        if let profileID = item?.profileID {
            let edit = NSMenuItem(title: String(localized: "Edit Saved Session…"), action: #selector(editSavedSession(_:)), keyEquivalent: "")
            edit.target = self
            edit.representedObject = profileID
            menu.addItem(edit)
        }
        menu.addItem(.separator())
        let entries: [(String, CloseScope?)] = [
            (String(localized: "Close"), nil),
            (String(localized: "Close Others"), .others),
            (horizontal ? String(localized: "Close Tabs to the Left") : String(localized: "Close Tabs Above"), .above),
            (horizontal ? String(localized: "Close Tabs to the Right") : String(localized: "Close Tabs Below"), .below),
            (String(localized: "Close All"), .all),
        ]
        for (title, scope) in entries {
            let item = NSMenuItem(title: title, action: #selector(closeFromMenu(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = CloseAction(scope: scope, session: session)
            // 目标为空时该项禁用，见 validateMenuItem。
            menu.addItem(item)
            if scope == nil { menu.addItem(.separator()) }
        }
        return menu
    }

    /// 弹出轻量输入框重命名标签（留空恢复默认标题）。
    func rename(_ session: SessionID) {
        guard !isRenaming, let item = workspace.item(session) else { return }
        isRenaming = true
        let initial = item.customTitle ?? ""
        Task { @MainActor [weak self] in
            defer { self?.isRenaming = false }
            guard let self else { return }
            guard let name = await controller.sessions.dialogs.promptText(
                title: String(localized: "Rename Tab"),
                message: String(localized: "Leave blank to restore the default title."),
                initial: initial,
                confirmTitle: String(localized: "Rename")
            ) else { return }
            controller.renameSession(session, to: name)
            controller.requestTerminalFocus?()
        }
    }

    /// 关闭一组标签（走控制器的关闭确认）。
    func close(_ ids: [SessionID]) {
        Task { @MainActor [controller] in await controller.requestClose(ids) }
    }

    private func targets(for action: CloseAction) -> [SessionID] {
        guard let scope = action.scope else { return [action.session] }
        return workspace.closeTargets(scope, relativeTo: action.session)
    }

    @objc private func renameFromMenu(_ sender: NSMenuItem) {
        guard let session = (sender.representedObject as? NSNumber)?.uint64Value else { return }
        rename(session)
    }

    @objc private func reconnectFromMenu(_ sender: NSMenuItem) {
        guard let session = (sender.representedObject as? NSNumber)?.uint64Value else { return }
        controller.perform { _ = try controller.reconnectSession(session) }
        controller.requestTerminalFocus?()
    }

    @objc private func editSavedSession(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        controller.sessions.editProfile(id: id)
    }

    @objc private func closeFromMenu(_ sender: NSMenuItem) {
        guard let action = sender.representedObject as? CloseAction else { return }
        close(targets(for: action))
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        guard let action = menuItem.representedObject as? CloseAction else { return true }
        return !targets(for: action).isEmpty
    }
}
