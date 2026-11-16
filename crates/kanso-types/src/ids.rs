//! 各类 id 的新类型。
//!
//! 分两类：
//! - 持久化 id：UUID v4，写入存储与导出文件。用 UUID 而不是自增整数，是为将来 iCloud 同步
//!   留余地（多设备各自生成 id 不会冲突，见 architecture §8）。
//! - 运行时 id：`u64`，只在进程内唯一，不持久化、不跨进程。分配由 core 负责，本 crate 不分配
//!   （这里没有全局计数器，避免类型 crate 持有可变状态）。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 生成持久化 id 新类型（UUID v4，序列化为带连字符的字符串）。
macro_rules! persistent_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// 随机生成一个新 id（UUID v4）。
            // 随机生成的 `new()` 不应被当作 `Default`：默认值每次不同会掩盖遗漏初始化的错误。
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// 由已有 UUID 构造，用于从存储或导入文件还原。
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// 取出底层 UUID。
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::from_str(s).map(Self)
            }
        }
    };
}

/// 生成运行时 id 新类型（`u64`，进程内唯一）。
macro_rules! runtime_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(u64);

        impl $name {
            /// 由 core 分配的原始值构造。调用方必须保证进程内不重复。
            pub const fn from_raw(raw: u64) -> Self {
                Self(raw)
            }

            /// 取出原始值，仅用于跨 FFI 边界传递与日志。
            pub const fn raw(&self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

persistent_id! {
    /// 会话配置（Profile）的持久化 id。跳板机引用（`SshTarget::jump`）也用它。
    ProfileId
}
persistent_id! {
    /// 分组的持久化 id。
    GroupId
}
persistent_id! {
    /// 快捷片段的持久化 id。
    SnippetId
}

runtime_id! {
    /// 一次会话（终端或 SFTP）的运行时 id。进程重启后不复用语义，不得持久化。
    SessionId
}
runtime_id! {
    /// 一次文件传输的运行时 id。
    TransferId
}
runtime_id! {
    /// 一次需要用户应答的请求的运行时 id，应答与 `PromptDismissed` 都靠它对应。
    PromptId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_id_serde_roundtrip_as_string() {
        let id = ProfileId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        let back: ProfileId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        assert_eq!(id.to_string().parse::<ProfileId>().unwrap(), id);
        assert_eq!(GroupId::from_uuid(id.as_uuid()).as_uuid(), id.as_uuid());
    }

    #[test]
    fn persistent_ids_are_unique() {
        assert_ne!(SnippetId::new(), SnippetId::new());
    }

    #[test]
    fn runtime_id_raw_and_display() {
        let id = SessionId::from_raw(42);
        assert_eq!(id.raw(), 42);
        assert_eq!(id.to_string(), "42");
        assert!(TransferId::from_raw(1) < TransferId::from_raw(2));
    }
}
