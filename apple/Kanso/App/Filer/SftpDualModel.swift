import AppKit
import KansoUI

enum SftpSide: String, Codable {
    case remote, local

    var transferKind: FilerTransferKind { self == .remote ? .download : .upload }
}

struct SftpDualDrag: Codable {
    static let type = NSPasteboard.PasteboardType("com.chhuax.kanso.sftp-dual")
    var session: SessionID
    var side: SftpSide
    var path: String
}

@MainActor
final class SftpDualModel {
    let remote: FilerModel
    let local: FilerModel
    private(set) var tab: FilerTabSnapshot
    var onChange: (() -> Void)?

    init(tab: FilerTabSnapshot, env: FilerEnvironment, prompter: FilerPrompter) {
        self.tab = tab
        remote = FilerModel(env: env, prompter: prompter, isDualPane: true)
        local = FilerModel(env: env, prompter: prompter, isDualPane: true)
        remote.onChange = { [weak self] in self?.onChange?() }
        local.onChange = { [weak self] in self?.onChange?() }
        follow(tab)
    }

    var isVisible: Bool {
        get { remote.isVisible }
        set { remote.isVisible = newValue; local.isVisible = newValue }
    }

    var connectionText: String? {
        switch tab.connection {
        case .connected: nil
        case .connecting: String(localized: "Connecting…")
        case .closed(let reason): reason ?? String(localized: "Session ended")
        }
    }

    func follow(_ snapshot: FilerTabSnapshot) {
        let reconnected = !tab.isConnected && snapshot.isConnected
        tab = snapshot
        var localTab = snapshot
        localTab.kind = .local
        if reconnected {
            remote.reconnect(snapshot)
            local.reconnect(localTab)
        } else {
            remote.follow(snapshot)
            local.follow(localTab)
        }
        onChange?()
    }

    func pane(_ side: SftpSide) -> FilerModel { side == .remote ? remote : local }

    func canTransfer(from side: SftpSide) -> Bool {
        tab.isConnected && remote.canWrite && local.canWrite && !remote.hasActiveBatch && !pane(side).selectedEntries.isEmpty
    }

    @discardableResult
    func transfer(from side: SftpSide, entries: [FilerEntry]? = nil) -> Bool {
        let source = pane(side)
        let receiver = pane(side == .remote ? .local : .remote)
        let picked = entries ?? source.selectedEntries
        guard tab.isConnected, remote.canWrite, local.canWrite, !remote.hasActiveBatch,
              !picked.isEmpty, let destination = receiver.directory else { return false }
        remote.transfer(picked, kind: side.transferKind, to: destination,
                        destinationDescription: "\(receiver.source?.title ?? "") · \(destination)") { [weak receiver] in
            guard let receiver, receiver.directory == destination, receiver.tab?.isConnected == true else { return }
            receiver.refresh(keepingNotice: true)
        }
        return true
    }

    func dragItem(_ entry: FilerEntry, from side: SftpSide) -> NSPasteboardItem {
        let item = NSPasteboardItem()
        let drag = SftpDualDrag(session: tab.session, side: side, path: entry.path)
        item.setData(try! JSONEncoder().encode(drag), forType: SftpDualDrag.type)
        item.setString("dual", forType: FilerPasteboard.marker)
        return item
    }

    func dropVerdict(_ drags: [SftpDualDrag], onto side: SftpSide) -> FilerDropVerdict {
        guard !drags.isEmpty, drags.allSatisfy({ $0.session == tab.session }) else {
            return .reject(reason: String(localized: "Drag items from the other pane"))
        }
        if drags.allSatisfy({ $0.side == side }) { return .ignore }
        guard drags.allSatisfy({ $0.side != side }), tab.isConnected, remote.canWrite, local.canWrite,
              !remote.hasActiveBatch else {
            return .reject(reason: connectionText ?? String(localized: "Wait until the folder finishes loading."))
        }
        return .accept(hint: side == .remote ? String(localized: "Upload") : String(localized: "Download"))
    }

    func performDrop(_ drags: [SftpDualDrag], onto side: SftpSide) -> Bool {
        guard case .accept = dropVerdict(drags, onto: side) else { return false }
        let sourceSide: SftpSide = side == .remote ? .local : .remote
        let paths = Set(drags.map(\.path))
        let picked = pane(sourceSide).entries.filter { paths.contains($0.path) }
        return picked.count == paths.count && transfer(from: sourceSide, entries: picked)
    }

    static func drags(on pasteboard: NSPasteboard) -> [SftpDualDrag] {
        (pasteboard.pasteboardItems ?? []).compactMap {
            $0.data(forType: SftpDualDrag.type).flatMap { try? JSONDecoder().decode(SftpDualDrag.self, from: $0) }
        }
    }
}
