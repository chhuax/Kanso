// 由 cargo xtask gen-tokens 生成，勿手改。
// 来源：design/terminal-themes/*.json 与 design/tokens.json 的 terminal.* token。
// 入库是为了让单独的 `swift build` 不依赖 xtask；cargo xtask check-tokens 会校验它与来源一致。

extension TerminalPalette {
    /// 数值来自 design/terminal-themes/kanso-light.json。
    public static let kansoLight = TerminalPalette(
        foreground: RGB8(hex: 0x1F2226),
        background: RGB8(hex: 0xFBFBF8),
        cursor: RGB8(hex: 0x2A6A8A),
        cursorText: RGB8(hex: 0xFBFBF8),
        selectionBackground: RGB8(hex: 0xC9DDE8),
        ansi: [
            RGB8(hex: 0x2B2E33), RGB8(hex: 0xC0392B), RGB8(hex: 0x2E7D32), RGB8(hex: 0x946500), RGB8(hex: 0x2F5FBF), RGB8(hex: 0x9B3FA6), RGB8(hex: 0x00798C), RGB8(hex: 0xD3D3CD),
            RGB8(hex: 0x6B7078), RGB8(hex: 0xE04F3F), RGB8(hex: 0x3E9B43), RGB8(hex: 0xB8860B), RGB8(hex: 0x4B7BE0), RGB8(hex: 0xB85BC4), RGB8(hex: 0x1B9AAE), RGB8(hex: 0xF0F0EA),
        ],
        semantic: [
            RGB8(hex: 0x0B6A8A), RGB8(hex: 0x8A5A00), RGB8(hex: 0x7A3FB0), RGB8(hex: 0xA0346F), RGB8(hex: 0x5C6B00), RGB8(hex: 0x1D63B0), RGB8(hex: 0x82439C), RGB8(hex: 0x465A70),
            RGB8(hex: 0x346B5A), RGB8(hex: 0x1A7A3A), RGB8(hex: 0xB3261E), RGB8(hex: 0x8C6000), RGB8(hex: 0x8C6000),
        ],
        gutterText: RGBA8(r: 106, g: 111, b: 118, a: 255),
        gutterCursorLine: RGBA8(r: 42, g: 106, b: 138, a: 31),
        commandSeparator: RGBA8(r: 27, g: 29, b: 32, a: 38),
        blockSelection: RGBA8(r: 223, g: 235, b: 239, a: 255),
        chipBackground: RGBA8(r: 245, g: 245, b: 240, a: 255),
        chipBorder: RGBA8(r: 201, g: 205, b: 197, a: 255)
    )

    /// 数值来自 design/terminal-themes/kanso-dark.json。
    public static let kansoDark = TerminalPalette(
        foreground: RGB8(hex: 0xEDEEF0),
        background: RGB8(hex: 0x050505),
        cursor: RGB8(hex: 0x62B0D2),
        cursorText: RGB8(hex: 0x050505),
        selectionBackground: RGB8(hex: 0x2E4756),
        ansi: [
            RGB8(hex: 0x3A3D42), RGB8(hex: 0xE5645A), RGB8(hex: 0x6CC070), RGB8(hex: 0xD9B04A), RGB8(hex: 0x6C9BEF), RGB8(hex: 0xC586D0), RGB8(hex: 0x55C0D0), RGB8(hex: 0xC9CCD0),
            RGB8(hex: 0x6B7078), RGB8(hex: 0xFF8A7E), RGB8(hex: 0x8EDB92), RGB8(hex: 0xF0CB6E), RGB8(hex: 0x8FB5FF), RGB8(hex: 0xDDA6E6), RGB8(hex: 0x7DD8E6), RGB8(hex: 0xF4F5F6),
        ],
        semantic: [
            RGB8(hex: 0x6CC4E0), RGB8(hex: 0xE0B070), RGB8(hex: 0xC39BF0), RGB8(hex: 0xE58FC0), RGB8(hex: 0xB8CC6A), RGB8(hex: 0x7DB4F0), RGB8(hex: 0xDDA6E6), RGB8(hex: 0xA7BACD),
            RGB8(hex: 0x8BC4B5), RGB8(hex: 0x6FCF8A), RGB8(hex: 0xFF8A80), RGB8(hex: 0xF0CB6E), RGB8(hex: 0xF0CB6E),
        ],
        gutterText: RGBA8(r: 142, g: 148, b: 155, a: 255),
        gutterCursorLine: RGBA8(r: 98, g: 176, b: 210, a: 38),
        commandSeparator: RGBA8(r: 236, g: 237, b: 239, a: 38),
        blockSelection: RGBA8(r: 32, g: 52, b: 62, a: 255),
        chipBackground: RGBA8(r: 37, g: 40, b: 44, a: 255),
        chipBorder: RGBA8(r: 75, g: 81, b: 88, a: 255)
    )
}
