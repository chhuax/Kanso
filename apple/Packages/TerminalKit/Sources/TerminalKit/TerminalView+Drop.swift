import AppKit

/// 文件拖入终端（filer.md §7.5）：只向**松手所在的这个终端**插入路径（交给 core 转义），获得焦点；
/// 不执行、不上传。目录只插入目录路径。落点是 SSH 终端时插入的仍是本地路径。
///
/// 每个终端视图各自是独立的落点：隐藏标签的视图不在视图层级里、文件面板与对话框是别的视图 / 窗口，
/// 所以不会同时接收到同一次松手。
extension TerminalView {
    private func droppedFilePaths(_ info: NSDraggingInfo) -> [String] {
        let urls = info.draggingPasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]
        ) as? [URL] ?? []
        return urls.map(\.path)
    }

    public override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        editActions != nil && !droppedFilePaths(sender).isEmpty ? .copy : []
    }

    public override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        editActions != nil && !droppedFilePaths(sender).isEmpty ? .copy : []
    }

    public override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let paths = droppedFilePaths(sender)
        guard let actions = editActions, !paths.isEmpty else { return false }
        actions.paste(.files(paths))
        window?.makeFirstResponder(self)
        return true
    }
}
