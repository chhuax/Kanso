import AppKit
import KansoBridge

/// 标签行等处按 AI 工具类型取图标。
enum AgentIcon {
    /// 有单色素材时返回 Assets 图片名；否则 `nil`，调用方用 SF Symbol `sparkles`。
    static func assetName(for kind: FfiAgentKind) -> String? {
        switch kind {
        case .claude: "claude"
        case .codex: "codex"
        case .gemini: "gemini"
        case .cursorAgent: "cursor-agent"
        case .copilot: "copilot"
        case .amp: "amp"
        case .openCode: "opencode"
        case .goose: "goose"
        case .qwen: "qwen"
        case .openHands: "openhands"
        case .grok: "grok"
        case .aider, .crush, .droid: nil
        }
    }

    /// 有素材用模板图，否则 SF Symbol `sparkles`。
    static func image(for kind: FfiAgentKind) -> NSImage {
        image(assetName: assetName(for: kind))
    }

    /// 标签图标的颜色：取工具品牌色（ui-shell.md §2.2）；品牌色本身是黑 / 白或没有鲜明主色的工具用主文字色。
    /// 不按运行 / 完成状态变色：状态由标签行右侧的状态标记表达，图标再变色会让人误以为是工具的颜色。
    static func tint(assetName: String?) -> NSColor {
        switch assetName {
        case "claude": .token(DesignTokens.Color.agentClaude)
        case "gemini": .token(DesignTokens.Color.agentGemini)
        case "qwen": .token(DesignTokens.Color.agentQwen)
        default: .token(DesignTokens.Color.textPrimary)
        }
    }

    static func image(assetName: String?) -> NSImage {
        if let assetName, let image = NSImage(named: assetName) { return image }
        return .symbol("sparkles")
    }
}
