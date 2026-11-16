import AppKit

// 拖放（filer.md §7）。本地条目直接交给系统（文件 URL）；远端条目用 file promise：
// 只有接收方要求交付时才下载。

enum FilerPasteboard {
    /// 本应用 Filer 拖出的标记。落回 Filer 时据此识别自拖回流并忽略（§7.5）。
    /// 标记随每次拖动的剪贴板内容存在，没有任何跨拖动的全局状态，所以不会拦住之后的外部上传。
    static let marker = NSPasteboard.PasteboardType("com.chhuax.kanso.filer-drag")

    static func isFromFiler(_ pasteboard: NSPasteboard) -> Bool {
        pasteboard.types?.contains(marker) == true
    }

    static func fileURLs(_ pasteboard: NSPasteboard) -> [URL] {
        (pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]) ?? []
    }

    /// 本地条目的拖出写入项：文件 URL + 标记。
    static func localItem(path: String) -> NSPasteboardItem {
        let item = NSPasteboardItem()
        item.setString(URL(fileURLWithPath: path).absoluteString, forType: .fileURL)
        item.setString("local", forType: marker)
        return item
    }
}

/// 一次远端条目的 file promise 交付。任务锁定原会话与条目；只在 `deliver` 被调用（接收方要求交付）时才下载。
/// 交付结果只报告一次：成功回 nil，失败 / 取消回错误，接收方不会一直等待。
@MainActor
final class FilePromiseDelivery {
    let entry: FilerEntry
    private let engine: FilerTransferEngine
    private let onStatus: (FilerTransferStatus) -> Void
    private var handle: FilerTransferHandle?
    private var completion: ((Error?) -> Void)?
    /// 测试用：已启动的下载次数。
    private(set) var downloadsStarted = 0
    private(set) var isFinished = false

    init(entry: FilerEntry, engine: FilerTransferEngine, onStatus: @escaping (FilerTransferStatus) -> Void = { _ in }) {
        self.entry = entry
        self.engine = engine
        self.onStatus = onStatus
    }

    /// 接收方给出落点 `url`（完整路径，含接收方确定的文件名）。
    func deliver(to url: URL, completion: @escaping (Error?) -> Void) {
        guard self.completion == nil, handle == nil else {
            completion(CocoaError(.fileWriteUnknown))
            return
        }
        self.completion = completion
        var item = FilerTransferItem(
            kind: .download, name: entry.name, source: entry.path,
            destination: url.deletingLastPathComponent().path, isDirectory: entry.isDirectory
        )
        if url.lastPathComponent != entry.name { item.renamedTo = url.lastPathComponent }
        // 落点由接收方选定（已处理它自己的同名冲突），直接写到该位置。
        item.initialPolicy = .overwrite
        do {
            let h = try engine.start(item, policy: .overwrite)
            handle = h
            downloadsStarted += 1
            Task { [weak self] in
                var status = FilerTransferStatus(kind: .download, name: item.name, index: 1, count: 1)
                for await event in h.events {
                    switch event {
                    case .started(let total): status.totalBytes = total
                    case .progress(let done, let total):
                        status.doneBytes = done
                        if let total { status.totalBytes = total }
                    case .finished(let result):
                        self?.finish(result, status: &status)
                    }
                    self?.onStatus(status)
                }
            }
        } catch {
            finishWith(error)
        }
    }

    func cancel() { handle?.cancel() }

    private func finish(_ result: FilerTransferResult, status: inout FilerTransferStatus) {
        switch result {
        case .completed:
            status.phase = .completed(skipped: 0)
            finishWith(nil)
        case .cancelled:
            status.phase = .cancelled
            finishWith(CocoaError(.userCancelled))
        case .failed(let message, _):
            status.phase = .failed(message)
            finishWith(NSError(domain: "Kanso.Filer", code: 1, userInfo: [NSLocalizedDescriptionKey: message]))
        }
    }

    private func finishWith(_ error: Error?) {
        guard let completion else { return }
        self.completion = nil
        isFinished = true
        completion(error)
    }
}

/// 远端条目的 promise 提供者。额外带上自拖标记，其余类型由系统生成。
final class FilerPromiseProvider: NSFilePromiseProvider {
    /// 强持有委托：`delegate` 是弱引用，不持有会在拖动开始前就被释放。
    var retainedDelegate: AnyObject?

    override func writableTypes(for pasteboard: NSPasteboard) -> [NSPasteboard.PasteboardType] {
        super.writableTypes(for: pasteboard) + [FilerPasteboard.marker]
    }

    override func pasteboardPropertyList(forType type: NSPasteboard.PasteboardType) -> Any? {
        type == FilerPasteboard.marker ? "remote" : super.pasteboardPropertyList(forType: type)
    }
}

final class FilerPromiseDelegate: NSObject, NSFilePromiseProviderDelegate, @unchecked Sendable {
    private let delivery: FilePromiseDelivery
    private let name: String
    /// 系统在后台队列调用写出方法；这里只转发到主线程。
    let queue = OperationQueue()

    @MainActor
    init(delivery: FilePromiseDelivery) {
        self.delivery = delivery
        self.name = delivery.entry.name
        super.init()
    }

    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String { name }

    func filePromiseProvider(
        _ filePromiseProvider: NSFilePromiseProvider, writePromiseTo url: URL, completionHandler: @escaping (Error?) -> Void
    ) {
        // completionHandler 不是 Sendable：包一层，保证只在交付结束时调用一次（FilePromiseDelivery 保证）。
        nonisolated(unsafe) let done = completionHandler
        DispatchQueue.main.async { [delivery] in
            MainActor.assumeIsolated { delivery.deliver(to: url) { done($0) } }
        }
    }

    func operationQueue(for filePromiseProvider: NSFilePromiseProvider) -> OperationQueue { queue }
}
