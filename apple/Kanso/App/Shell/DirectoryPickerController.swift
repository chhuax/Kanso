import AppKit
import TerminalKit

@MainActor
final class DirectoryPickerController {
    private weak var terminal: TerminalView?
    private let sink: TerminalInputSink
    private let workingDirectory: () async throws -> String
    private var panel: FloatingPanel?
    private var task: Task<Void, Never>?

    init(terminal: TerminalView, sink: TerminalInputSink, workingDirectory: @escaping () async throws -> String) {
        self.terminal = terminal
        self.sink = sink
        self.workingDirectory = workingDirectory
    }

    func present(anchor: NSRect) {
        dismiss()
        task = Task { [weak self] in
            guard let self, let directory = try? await workingDirectory(), !Task.isCancelled,
                  let terminal, terminal.isDirectoryChangeAvailable, let window = terminal.window else { return }
            let content = DirectoryPickerViewController(directory: directory)
            let panel = FloatingPanel(content: content.view, size: DirectoryPickerViewController.size, flipsAbove: true)
            panel.appearance = terminal.effectiveAppearance
            panel.onDismiss = { [weak self, content] in
                self?.task?.cancel()
                self?.panel = nil
                withExtendedLifetime(content) {}
            }
            content.onCancel = { [weak self] in self?.dismiss() }
            content.onAccept = { [weak self] path in
                guard let self else { return }
                if terminal.isDirectoryChangeAvailable { DirectoryPickerModel.changeDirectory(to: path, through: sink) }
                dismiss()
            }
            self.panel = panel
            panel.present(below: window.convertToScreen(terminal.convert(anchor, to: nil)), parent: window)
            panel.makeFirstResponder(content.searchField.textField)
            if let entries = try? await DirectoryPickerModel.load(directory: directory), !Task.isCancelled {
                content.model.candidates = entries
                content.reload()
            }
        }
    }

    func refresh() {
        if terminal?.isDirectoryChangeAvailable != true { dismiss() }
    }

    func dismiss() {
        task?.cancel()
        task = nil
        panel?.dismiss()
        panel = nil
    }
}
