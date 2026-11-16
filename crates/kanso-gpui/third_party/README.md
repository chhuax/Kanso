# 第三方改编代码

本目录说明 `kanso-gpui` 里**改编自 Apache-2.0 项目**的源码。许可证全文在 `../LICENSE-APACHE`。
整个 crate 仍以 GPL-3.0-or-later 发布（Apache-2.0 与 GPL-3.0 兼容）；下列文件的头部注释保留了来源声明。

| 本仓库文件 | 来源 | 版权 | 说明 |
|---|---|---|---|
| `src/ui/widgets/text_input.rs` | `gpui` 0.2.2，`examples/input.rs` | Copyright Zed Industries, Inc.，Apache-2.0 | 只借用了自绘元素与 `EntityInputHandler` 的整体骨架（`prepaint` 整形、`paint` 里 `handle_input`、UTF-16 接口）。编辑逻辑、选区、撤销、掩码、滚动、闪烁、事件、样式均为本仓库新写，已修改。 |
| `src/widgets/edit.rs`（UTF-16 ↔ UTF-8 换算、按字素簇移动的思路） | 同上 | 同上 | 换算与 `previous_boundary` / `next_boundary` 的做法参考同一示例，代码为重写。 |

`src/widgets/` 下其余文件（`undo.rs`、`word.rs`、`blink.rs`、`scroll.rs`、`style.rs`）为本仓库原创，没有复制第三方代码。
没有使用 `gpui-component` / `gpui-base` 的源码。

新增依赖：`unicode-segmentation`（MIT OR Apache-2.0；已在 `Cargo.lock` 中，由 gpui 引入）。
