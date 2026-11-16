/// 当前构建的发行渠道，由 Xcode 配置里的 Swift 编译条件决定（MAS / SETAPP，都没有则为 Direct）。
enum KansoChannel {
    static var current: String {
        #if MAS
        "MAS"
        #elseif SETAPP
        "Setapp"
        #else
        "Direct"
        #endif
    }
}
