import Foundation

/// 窗口状态持久化（ui-shell §1）。大小与位置交给 AppKit 的 frame autosave，
/// 这里只记"关闭时是否最大化"——frame autosave 记不住这个状态。
///
/// `UserDefaults` 由调用方注入：测试必须用临时 suite，不能写用户真实的偏好。
struct WindowStateStore {
    static let zoomedKey = "kanso.window.isZoomed"

    let defaults: UserDefaults

    init(defaults: UserDefaults) {
        self.defaults = defaults
    }

    var wasZoomed: Bool { defaults.bool(forKey: Self.zoomedKey) }

    func save(isZoomed: Bool) {
        defaults.set(isZoomed, forKey: Self.zoomedKey)
    }
}
