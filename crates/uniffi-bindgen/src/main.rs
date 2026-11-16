//! UniFFI 绑定生成器入口，由 `cargo xtask bindgen` / `check-bindings` 调用。
//!
//! 单独成 crate 是为了让 `uniffi` 的 `cli` feature（clap、askama 等一批生成器依赖）
//! 只出现在这个二进制里，不进入 `kanso-ffi` 的依赖树与最终静态库。

fn main() {
    uniffi::uniffi_bindgen_main()
}
