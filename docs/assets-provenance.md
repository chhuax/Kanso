# 素材来源登记

素材须由作者拥有版权，或以与 GPL-3.0 兼容的许可提供。每引入一项素材都在这里登记来源，
供发布前的相似度自查与许可审查使用（design.md §2）。

| 素材 | 位置 | 来源 | 作者 / 许可 | 登记日期 |
|---|---|---|---|---|
| 应用图标（全部尺寸，16–1024 px） | `apple/Kanso/Resources/Assets.xcassets/AppIcon.appiconset/` | 作者提供的图标；原始登记为从已安装的 Kanso 0.2.1 的 `icon.icns` 用 `iconutil -c iconset` 拆出 | chhuax 拥有版权；与 EdgeTerm 上游代码无关。名称及应用图标不在 GPL 授权范围内 | 2026-11-13 |
| AI 工具图标 | `apple/Kanso/Resources/Assets.xcassets/AgentIcons`、`design/agent-icons` | LobeHub 图标库 `@lobehub/icons-static-svg` 1.102.0，https://github.com/lobehub/lobe-icons | MIT；Copyright (c) 2023 LobeHub；各品牌标识属于其所有者，仅用于标示正在运行的工具；许可全文见 `../THIRD_PARTY_NOTICES.md` | 2026-11-16 |
| Git 分支图标 | `apple/Kanso/Resources/Assets.xcassets/GitBranch.imageset` | Lucide 图标库 `lucide-static` 1.73.0 的 `git-branch`，https://github.com/lucide-icons/lucide | ISC；Copyright (c) Lucide Icons and Contributors；许可全文见 `../THIRD_PARTY_NOTICES.md` | 2026-11-16 |

规则：
- 只登记素材（图标、图片、字体、声音等），不涉及代码；代码一律不得从旧 GPL 仓库引入。
- 来源不明或出自上游作者的素材不得使用。
