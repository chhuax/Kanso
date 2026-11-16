//! UniFFI 控制面与帧数据面。
//!
//! 全仓唯一允许出现 `extern "C"` 与 UniFFI 注解的 crate（architecture §2.2）。
//!
//! 模块导览：
//! - [`ffi_types`]：跨 FFI 边界的镜像类型与到 kanso-types / kanso-core 类型的转换。
//! - [`core_object`]：`KansoCore` 对象、事件回调 trait、目录流对象。
//! - [`secret_store`]：凭据存储的宿主回调接口（macOS 钥匙串）与到 core `SecretStore` 的适配器。
//! - [`frame_handle`]：`FrameHandle`，帧内容零拷贝的取帧句柄。
//! - [`frame`]：mock 帧的手写 C ABI（C 冒烟测试与 `--snapshot` 截图模式使用）。
//!
//! Swift 绑定由 `cargo xtask bindgen` 以 library mode 生成，见 docs/apple-build.md。

pub mod core_object;
pub mod ffi_types;
pub mod frame;
pub mod frame_handle;
pub mod git;
pub mod secret_store;

uniffi::setup_scaffolding!();
