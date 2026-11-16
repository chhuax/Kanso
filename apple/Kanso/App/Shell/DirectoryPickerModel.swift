import Foundation
import TerminalKit

struct DirectoryPickerModel {
    struct Entry: Equatable, Sendable {
        let name: String
        let path: String
    }

    let directory: String
    var candidates: [CommandPaletteFuzzy.Candidate<Entry>] = []
    var query = ""

    var entries: [Entry] {
        let needle = CommandPaletteFuzzy.normalize(query)
        let matches = candidates.filter {
            (query.hasPrefix(".") || !$0.item.name.hasPrefix("."))
                && CommandPaletteFuzzy.score(needle: needle, hay: $0.text) != nil
        }.map(\.item)
        return [Entry(name: "..", path: URL(fileURLWithPath: directory).deletingLastPathComponent().path)] + matches
    }

    static func load(directory: String) async throws -> [CommandPaletteFuzzy.Candidate<Entry>] {
        try await Task.detached(priority: .userInitiated) {
            let source = LocalFilerDataSource()
            var entries: [Entry] = []
            for try await batch in source.list(path: directory) {
                for entry in batch {
                    if entry.isDirectory {
                        entries.append(Entry(name: entry.name, path: entry.path))
                    } else if entry.isSymlink, case .directory = try? await source.resolveLink(entry) {
                        entries.append(Entry(name: entry.name, path: entry.path))
                    }
                }
            }
            return CommandPaletteFuzzy.prepare(entries, text: { $0.name })
        }.value
    }

    @MainActor
    static func changeDirectory(to path: String, through sink: TerminalInputSink) {
        let encoded = path.utf8.map { String(format: "%02x", $0) }.joined()
        sink.textInput("\u{1b}[9001~" + encoded + "\n")
    }
}
