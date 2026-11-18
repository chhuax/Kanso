import AppKit

@MainActor
final class DirectoryPickerViewController: NSViewController, NSTextFieldDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let size = NSSize(width: 380, height: 320)
    let searchField = SidebarFilterField()
    let tableView = DirectoryPickerTableView()
    var model: DirectoryPickerModel
    private(set) var entries: [DirectoryPickerModel.Entry] = []
    var onAccept: ((String) -> Void)?
    var onCancel: (() -> Void)?

    init(directory: String) {
        model = DirectoryPickerModel(directory: directory)
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("DirectoryPickerViewController 只支持代码创建") }

    override func loadView() {
        let root = StyledBoxView(frame: NSRect(origin: .zero, size: Self.size))
        root.fillToken = DesignTokens.Color.bgPopover
        root.cornerRadius = DesignTokens.Radius.xl
        view = root
        searchField.frame = NSRect(x: 12, y: 12, width: Self.size.width - 24, height: 34)
        searchField.autoresizingMask = [.width]
        searchField.placeholder = String(localized: "Search directories…")
        searchField.textField.delegate = self
        searchField.onClear = { [weak self] in self?.model.query = ""; self?.reload() }
        root.addSubview(searchField)

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("directory"))
        column.width = Self.size.width - 12
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.style = .plain
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.backgroundColor = .clear
        tableView.rowHeight = 32
        tableView.intercellSpacing = .zero
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.autoresizingMask = [.width]
        tableView.doubleAction = #selector(acceptSelection)
        tableView.setAccessibilityLabel(String(localized: "Directory"))
        let scroll = NSScrollView(frame: NSRect(x: 6, y: 54, width: Self.size.width - 12, height: Self.size.height - 62))
        scroll.autoresizingMask = [.width, .height]
        scroll.documentView = tableView
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        root.addSubview(scroll)
        reload()
    }

    func reload() {
        entries = model.entries
        tableView.reloadData()
        tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        tableView.scrollRowToVisible(0)
    }

    func numberOfRows(in tableView: NSTableView) -> Int { entries.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let identifier = NSUserInterfaceItemIdentifier("directory")
        let cell = (tableView.makeView(withIdentifier: identifier, owner: nil) as? NSTableCellView) ?? NSTableCellView()
        if cell.identifier == nil {
            cell.identifier = identifier
            let icon = NSImageView()
            icon.image = .symbol("folder")
            icon.contentTintColor = .token(DesignTokens.Color.textSecondary)
            let label = NSTextField(labelWithString: "")
            label.font = .systemFont(ofSize: DesignTokens.Font.uiBody)
            label.textColor = .token(DesignTokens.Color.textPrimary)
            label.lineBreakMode = .byTruncatingTail
            for child in [icon, label] { child.translatesAutoresizingMaskIntoConstraints = false; cell.addSubview(child) }
            NSLayoutConstraint.activate([
                icon.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 10),
                icon.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
                icon.widthAnchor.constraint(equalToConstant: 16), icon.heightAnchor.constraint(equalToConstant: 16),
                label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 10),
                label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -10),
                label.centerYAnchor.constraint(equalTo: cell.centerYAnchor)
            ])
            cell.imageView = icon
            cell.textField = label
        }
        cell.textField?.stringValue = row == 0 ? String(localized: ".. (Parent Directory)") : entries[row].name
        return cell
    }

    @objc func acceptSelection() {
        guard entries.indices.contains(tableView.selectedRow) else { return }
        onAccept?(entries[tableView.selectedRow].path)
    }

    func controlTextDidChange(_ obj: Notification) {
        model.query = searchField.stringValue
        reload()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
        switch commandSelector {
        case #selector(NSResponder.moveUp(_:)), #selector(NSResponder.moveDown(_:)):
            let delta = commandSelector == #selector(NSResponder.moveUp(_:)) ? -1 : 1
            let row = min(max(0, tableView.selectedRow + delta), entries.count - 1)
            tableView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            tableView.scrollRowToVisible(row)
        case #selector(NSResponder.insertNewline(_:)):
            acceptSelection()
        case #selector(NSResponder.cancelOperation(_:)):
            onCancel?()
        default: return false
        }
        return true
    }
}

@MainActor
final class DirectoryPickerTableView: NSTableView {
    override var acceptsFirstResponder: Bool { false }
}
