import AppKit

// 顶层代码在主 actor 上运行（Swift 6 严格并发），可以直接构造 @MainActor 的对象。
PerformanceProbe.mark("main")
let application = NSApplication.shared
PerformanceProbe.mark("application")

if let options = SnapshotOptions(arguments: CommandLine.arguments) {
    // 截图模式不显示 UI、不进 Dock，渲染完直接退出进程。
    application.setActivationPolicy(.prohibited)
    exit(SnapshotRenderer.run(options))
}

// delegate 属性是 weak 引用，必须由全局变量持有，否则会立刻释放。
let appDelegate = AppDelegate()
application.delegate = appDelegate
// 以本 App 为测试宿主时不进 Dock：测试会反复拉起 App，Dock 图标不停出现消失会让用户以为应用在重启。
// 用 .accessory 而不是 .prohibited：测试仍需要创建窗口。
if ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil {
    application.setActivationPolicy(.accessory)
}
application.run()
