//! UniFFI 镜像类型：控制面上跨 FFI 边界的全部数据类型。
//!
//! # 为什么手写镜像类型，而不用 UniFFI 的 remote 类型
//!
//! - `uniffi::remote` 要求在本 crate 里把外部类型的**完整定义**再写一遍，
//!   变体和字段仍然是重复劳动，换来的只是省掉 `From` 实现；
//! - 我们需要改变一部分类型的线上形态：持久化 id（`Uuid` 新类型）在 FFI 上是字符串，
//!   运行时 id 是 `u64`，`SecretString` 是字符串，82 个变体的 `Message` 要扁平成 key + 参数表。
//!   这些都超出了 remote 类型能表达的范围；
//! - kanso-types / kanso-core 因此不需要为 FFI 做任何改动（architecture §2.2：UniFFI 注解只能在本 crate）。
//!
//! # 命名与转换约定
//!
//! - 所有镜像类型以 `Ffi` 开头，Swift 侧看到的名字就是它。
//! - 方向 core → UI 的类型（事件、消息、错误、返回值）实现 `From`；
//!   方向 UI → core 的类型（输入、Profile、应答）实现 `TryFrom`，因为字符串 id 与 locale 可能非法，
//!   失败返回 [`FfiCoreError::Internal`]（说明 Swift 侧传了不该传的值，属于调用方缺陷）。
//! - 持久化 id（Profile / Group / Snippet）在 FFI 上是小写连字符 UUID 字符串；运行时 id
//!   （会话 / 传输 / 提问）是 `u64`。

mod api;
mod error;
mod event;
mod input;
mod message;
mod profile;

use std::fmt::Display;
use std::str::FromStr;

pub use api::*;
pub use error::FfiCoreError;
pub use event::*;
pub use input::*;
pub use message::*;
pub use profile::*;

/// 解析 Swift 传来的持久化 id 字符串。`what` 只用于错误信息。
pub(crate) fn parse_id<T>(s: &str, what: &str) -> Result<T, FfiCoreError>
where
    T: FromStr,
    T::Err: Display,
{
    s.parse::<T>().map_err(|e| FfiCoreError::Internal {
        detail: format!("invalid {what} id `{s}`: {e}"),
    })
}

/// 为 C 风格枚举（无字段）生成镜像类型及双向 `From`。
///
/// 变体必须与源枚举同名；新增变体时源枚举的 `match` 会因穷尽检查而编译失败，不会静默漏掉。
macro_rules! mirror_enum {
    (
        $(#[$meta:meta])*
        $ffi:ident <=> $src:ty {
            $($(#[$vmeta:meta])* $variant:ident),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
        pub enum $ffi {
            $($(#[$vmeta])* $variant),+
        }

        impl From<$src> for $ffi {
            fn from(v: $src) -> Self {
                match v {
                    $(<$src>::$variant => $ffi::$variant),+
                }
            }
        }

        impl From<$ffi> for $src {
            fn from(v: $ffi) -> Self {
                match v {
                    $($ffi::$variant => <$src>::$variant),+
                }
            }
        }
    };
}
pub(crate) use mirror_enum;
