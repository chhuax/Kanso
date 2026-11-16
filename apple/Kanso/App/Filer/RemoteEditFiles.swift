import Foundation

struct FilerFileVersion: Codable, Equatable, Sendable {
    var size: UInt64?
    var modified: Date?

    static func local(_ url: URL) throws -> Self {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return Self(size: (attributes[.size] as? NSNumber)?.uint64Value, modified: attributes[.modificationDate] as? Date)
    }
}

@MainActor
final class RemoteEditWatcher {
    private var directorySource: DispatchSourceFileSystemObject?
    private var fileSource: DispatchSourceFileSystemObject?
    private let url: URL
    private let onChange: () -> Void

    init(url: URL, onChange: @escaping () -> Void) throws {
        self.url = url
        self.onChange = onChange
        directorySource = try source(for: url.deletingLastPathComponent(), events: [.write, .rename, .delete])
        rearmFile()
    }

    private func source(for url: URL, events: DispatchSource.FileSystemEvent) throws -> DispatchSourceFileSystemObject {
        let descriptor = open(url.path, O_EVTONLY)
        guard descriptor >= 0 else { throw FilerError.fromErrno(errno, path: url.path) }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: descriptor, eventMask: events, queue: .main)
        source.setEventHandler { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.rearmFile()
                self.onChange()
            }
        }
        source.setCancelHandler { close(descriptor) }
        source.resume()
        return source
    }

    private func rearmFile() {
        fileSource?.cancel()
        fileSource = try? source(for: url, events: [.write, .rename, .delete])
    }

    func stop() {
        directorySource?.cancel()
        fileSource?.cancel()
        directorySource = nil
        fileSource = nil
    }
}

enum RemoteEditFiles {
    struct SyncedCopy: Codable {
        var name: String
        var version: FilerFileVersion
    }

    static func markSynced(_ file: URL, version: FilerFileVersion) throws {
        let state = SyncedCopy(name: file.lastPathComponent, version: version)
        let url = file.deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("state.json")
        try JSONEncoder().encode(state).write(to: url, options: .atomic)
    }

    static func cleanPreviousRuns(in root: URL, excluding activeRun: URL? = nil) throws {
        let files = FileManager.default
        guard files.fileExists(atPath: root.path) else { return }
        for run in try files.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey]) {
            if run.lastPathComponent == activeRun?.lastPathComponent { continue }
            guard try run.resourceValues(forKeys: [.isDirectoryKey]).isDirectory == true else { continue }
            for session in try files.contentsOfDirectory(at: run, includingPropertiesForKeys: nil) {
                for record in try files.contentsOfDirectory(at: session, includingPropertiesForKeys: nil) {
                    let state = record.appendingPathComponent("state.json")
                    guard let data = try? Data(contentsOf: state),
                          let copy = try? JSONDecoder().decode(SyncedCopy.self, from: data),
                          case .success(let name) = FilerNameValidator.validate(copy.name, existing: []) else { continue }
                    let file = record.appendingPathComponent("content").appendingPathComponent(name)
                    if (try? FilerFileVersion.local(file)) == copy.version {
                        try files.removeItem(at: record)
                    }
                }
                if try files.contentsOfDirectory(atPath: session.path).isEmpty { try files.removeItem(at: session) }
            }
            if try files.contentsOfDirectory(atPath: run.path).isEmpty { try files.removeItem(at: run) }
        }
    }
}
