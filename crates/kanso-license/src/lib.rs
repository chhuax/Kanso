//! Entitlement 模型与 License Key 离线校验（sans-IO）。

// 发行版 feature 必须恰好启用一个。放在最底层的 license 是因为所有渠道差异
// 最终都汇到授权上，这里拦一次即可覆盖 core 与 ffi（它们的 feature 都转发到这里）。
#[cfg(not(any(feature = "direct", feature = "mas", feature = "setapp")))]
compile_error!(
    "必须恰好启用一个发行版 feature（direct / mas / setapp），当前一个都没启用。\
     正确用法示例：`--no-default-features --features mas`"
);

#[cfg(any(
    all(feature = "direct", feature = "mas"),
    all(feature = "direct", feature = "setapp"),
    all(feature = "mas", feature = "setapp"),
))]
compile_error!(
    "发行版 feature（direct / mas / setapp）只能启用一个，当前启用了多个。\
     正确用法示例：`--no-default-features --features mas`"
);

// 下面的平台抽象不受发行版 feature 影响：CI 从第一天就编译 mas / setapp，
// 平台抽象不能被 cfg 绕过（architecture §9）。

use std::fmt;

/// 授权来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntitlementSource {
    /// License Key（仅 Direct 渠道）。
    LicenseKey,
    /// App Store 购买（MAS 渠道）。
    AppStore,
    /// Setapp 订阅。
    Setapp,
}

/// 一份授权。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entitlement {
    /// 来源；存在这份授权即表示已授权。
    pub source: EntitlementSource,
}

impl Entitlement {
    /// 渠道自带的授权；Direct 渠道仍需有效 License Key。
    pub fn for_channel() -> Option<Self> {
        #[cfg(feature = "direct")]
        {
            None
        }
        #[cfg(feature = "mas")]
        {
            Some(Self {
                source: EntitlementSource::AppStore,
            })
        }
        #[cfg(feature = "setapp")]
        {
            Some(Self {
                source: EntitlementSource::Setapp,
            })
        }
    }
}

/// 购买流程的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurchaseOutcome {
    /// 购买成功。
    Purchased(Entitlement),
    /// 用户取消。
    Cancelled,
    /// 待处理（例如家长审批），稍后通过 [`PurchaseProvider::current`] 查询。
    Pending,
}

/// 购买提供方错误。`detail` 为英文技术信息，只进日志。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurchaseError {
    /// 商店服务不可用（离线、未登录等）。
    Unavailable {
        /// 技术细节。
        detail: String,
    },
    /// 操作失败。
    Failed {
        /// 技术细节。
        detail: String,
    },
}

impl fmt::Display for PurchaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PurchaseError::Unavailable { detail } => write!(f, "purchase unavailable: {detail}"),
            PurchaseError::Failed { detail } => write!(f, "purchase failed: {detail}"),
        }
    }
}

impl std::error::Error for PurchaseError {}

/// 商店购买提供方。
///
/// - 实现者：MAS 版由 Swift 用 StoreKit 2 实现，Setapp 版由 Swift 用 Setapp SDK 实现。
///   **Direct 版不使用它**，License Key 的校验在本 crate 内部完成（后续 T11）。
/// - 调用者：core，只在后台线程（`spawn_blocking`）调用，不会在 UI 线程上调用。
/// - 可以阻塞：`purchase` 会弹系统购买界面，可能阻塞很久；`current` / `restore` 也可能访问网络。
/// - 出错处理：调用方保留上一次已知的授权状态，不因一次查询失败撤销授权，
///   并向用户给出可重试的提示。
pub trait PurchaseProvider: Send + Sync + 'static {
    /// 查询当前授权；没有任何授权返回 `Ok(None)`。
    fn current(&self) -> Result<Option<Entitlement>, PurchaseError>;

    /// 发起购买，会弹出系统购买界面。
    fn purchase(&self) -> Result<PurchaseOutcome, PurchaseError>;

    /// 恢复购买；没有可恢复的授权返回 `Ok(None)`。
    fn restore(&self) -> Result<Option<Entitlement>, PurchaseError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn channel_entitlement() {
        #[cfg(feature = "direct")]
        assert_eq!(Entitlement::for_channel(), None);
        #[cfg(feature = "mas")]
        assert_eq!(
            Entitlement::for_channel(),
            Some(Entitlement {
                source: EntitlementSource::AppStore
            })
        );
        #[cfg(feature = "setapp")]
        assert_eq!(
            Entitlement::for_channel(),
            Some(Entitlement {
                source: EntitlementSource::Setapp
            })
        );
    }

    #[test]
    fn trait_is_object_safe_and_thread_safe() {
        fn assert_send_sync<T: Send + Sync + ?Sized>(_: &T) {}
        // 只需通过编译：`Arc<dyn PurchaseProvider>` 可构造且 `Send + Sync`。
        fn check(p: &Arc<dyn PurchaseProvider>) {
            assert_send_sync(p);
        }
        let _ = check;
    }
}
