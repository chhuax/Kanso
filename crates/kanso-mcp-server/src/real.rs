//! 真实服务。**尚未实现（F9）**：`start` 返回 `NotSupported`，状态为 `Unavailable(NotSupported)`。
//!
//! F9 在这里实现：单实例锁与 socket 监听、令牌校验、rmcp 服务端、工具与授权、审计、
//! 客户端所在标签识别（architecture §16）。非 Direct 渠道（`direct` feature 关闭）永远保持"不支持"。

use std::sync::{Arc, Mutex};

use kanso_core::Core;
use kanso_types::secret::SecretString;

use crate::{
    McpClientInfo, McpServerConfig, McpServerDelegate, McpServerError, McpServerStatus,
    McpUnavailable, Result,
};

pub(crate) struct RealServer {
    // F9 用它执行工具调用、读写授权与审计；现在只为固定构造签名而持有。
    #[allow(dead_code)]
    core: Arc<Core>,
    delegate: Arc<dyn McpServerDelegate>,
    status: Mutex<McpServerStatus>,
}

impl RealServer {
    pub(crate) fn new(core: Arc<Core>, delegate: Arc<dyn McpServerDelegate>) -> RealServer {
        RealServer {
            core,
            delegate,
            status: Mutex::new(McpServerStatus::Stopped),
        }
    }

    pub(crate) fn start(&self, config: McpServerConfig) -> Result<()> {
        let _ = config;
        let status = McpServerStatus::Unavailable(McpUnavailable::NotSupported);
        let mut current = self.status.lock().unwrap_or_else(|e| e.into_inner());
        *current = status.clone();
        // 持锁回调：并发的 stop 不会把 Stopped 排到这次 Unavailable 之前。
        self.delegate.on_status_changed(status);
        Err(McpServerError::NotSupported)
    }

    /// 停止后状态回到 `Stopped`。启动失败（`Unavailable`）后用户关掉开关时也要回到 `Stopped`，
    /// 否则设置页在开关已关的情况下还一直显示"不可用"。
    pub(crate) fn stop(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if *status == McpServerStatus::Stopped {
            return;
        }
        *status = McpServerStatus::Stopped;
        // 持锁回调，与状态变化严格保序（回调契约禁止重入，见 crate 文档）。
        self.delegate.on_status_changed(McpServerStatus::Stopped);
    }

    pub(crate) fn set_token(&self, token: SecretString) {
        let _ = token;
    }

    pub(crate) fn status(&self) -> McpServerStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn clients(&self) -> Vec<McpClientInfo> {
        Vec::new()
    }

    pub(crate) fn disconnect(&self, client_id: u64) {
        let _ = client_id;
    }
}
