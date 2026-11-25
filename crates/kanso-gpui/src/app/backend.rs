//! 模型对核心的最小依赖。
//!
//! 模型不直接持有 `Core`，而是依赖这几个小 trait：生产环境用 `Arc<Core>`（下面的实现），
//! 测试用记录调用的替身（也可以用 `Core::new_mock`）。这样"留空密码不调用 `set_credential`"
//! 之类的断言不必碰真实凭据存储。

use std::sync::Arc;

use kanso_core::{Core, CoreError, DeleteImpact, OpenTarget, SessionInfo};
use kanso_secret::SecretKind;
use kanso_types::mcp::McpAccess;
use kanso_types::profile::{Group, Profile};
use kanso_types::secret::SecretString;
use kanso_types::{GridSize, GroupId, ProfileId, PromptId, PromptResponse, SessionId};

/// 核心调用的结果。
pub type CoreResult<T> = Result<T, CoreError>;

/// 工作区控制器用到的会话能力。
pub trait SessionBackend {
    /// 打开会话。
    fn open_session(&self, target: OpenTarget, size: GridSize) -> CoreResult<SessionId>;
    /// 用户主动关闭会话；会话不存在时忽略。
    fn close_session(&self, session: SessionId);
    /// 单个会话的摘要。
    fn session_info(&self, session: SessionId) -> Option<SessionInfo>;
    /// 设置或清除标签的自定义名；会话不存在时忽略。
    fn set_session_title(&self, session: SessionId, title: Option<String>);
    /// 告知核心标签是否可见（不可见的标签只解析不渲染）。
    fn set_visible(&self, session: SessionId, visible: bool);
    /// 按 id 取已保存的会话（取颜色等展示信息）。
    fn profile(&self, id: ProfileId) -> Option<Profile>;
}

/// 回答核心提问的出口。
///
/// 必须在 UI 线程里、事件处理之外调用，不要在核心的事件回调栈里同步调用。
pub trait PromptResponder {
    /// 回答一个提问。未知或已过期的提问核心会静默忽略。
    fn respond_prompt(&self, prompt: PromptId, response: PromptResponse);
}

/// 会话库（Profile / 分组 / 凭据 / MCP 授权）的读写。
///
/// 删除与凭据写入是 `async`：钥匙串访问可能弹出系统授权框，不能卡住 UI 线程。
/// 调用方用自己的执行器驱动这些 future（GPUI 里用 `cx.spawn`）。
#[allow(async_fn_in_trait)]
pub trait ProfileStore {
    /// 全部会话配置。
    fn profiles(&self) -> Vec<Profile>;
    /// 全部分组。
    fn groups(&self) -> Vec<Group>;
    /// 新增或更新（按 id）。
    fn save_profile(&self, profile: Profile) -> CoreResult<()>;
    /// 已保存会话的 MCP 访问级别。
    fn mcp_access(&self, profile: ProfileId) -> McpAccess;
    /// 设置已保存会话的 MCP 访问级别。
    fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> CoreResult<()>;
    /// 删除会话配置（连同凭据）。
    async fn delete_profile(&self, id: ProfileId) -> CoreResult<()>;
    /// 新增或更新（按 id）分组。
    fn save_group(&self, group: Group) -> CoreResult<()>;
    /// 删除分组会连带删除多少内容。
    fn group_delete_impact(&self, id: GroupId) -> CoreResult<DeleteImpact>;
    /// 删除分组（连同组内会话）。
    async fn delete_group(&self, id: GroupId) -> CoreResult<()>;
    /// 保存凭据。
    async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> CoreResult<()>;
}

/// 控制器需要的全部能力。
pub trait AppBackend: SessionBackend + PromptResponder {}
impl<T: SessionBackend + PromptResponder> AppBackend for T {}

impl SessionBackend for Core {
    fn open_session(&self, target: OpenTarget, size: GridSize) -> CoreResult<SessionId> {
        Core::open_session(self, target, size)
    }

    fn close_session(&self, session: SessionId) {
        Core::close_session(self, session);
    }

    fn session_info(&self, session: SessionId) -> Option<SessionInfo> {
        Core::session_info(self, session)
    }

    fn set_session_title(&self, session: SessionId, title: Option<String>) {
        Core::set_session_title(self, session, title);
    }

    fn set_visible(&self, session: SessionId, visible: bool) {
        Core::set_visible(self, session, visible);
    }

    fn profile(&self, id: ProfileId) -> Option<Profile> {
        self.profiles().into_iter().find(|p| p.id == id)
    }
}

impl PromptResponder for Core {
    fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
        Core::respond_prompt(self, prompt, response);
    }
}

impl ProfileStore for Core {
    fn profiles(&self) -> Vec<Profile> {
        Core::profiles(self)
    }

    fn groups(&self) -> Vec<Group> {
        Core::groups(self)
    }

    fn save_profile(&self, profile: Profile) -> CoreResult<()> {
        Core::save_profile(self, profile)
    }

    fn mcp_access(&self, profile: ProfileId) -> McpAccess {
        Core::mcp_access(self, profile)
    }

    fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> CoreResult<()> {
        Core::set_mcp_access(self, profile, level)
    }

    async fn delete_profile(&self, id: ProfileId) -> CoreResult<()> {
        Core::delete_profile(self, id).await
    }

    fn save_group(&self, group: Group) -> CoreResult<()> {
        Core::save_group(self, group)
    }

    fn group_delete_impact(&self, id: GroupId) -> CoreResult<DeleteImpact> {
        Core::group_delete_impact(self, id)
    }

    async fn delete_group(&self, id: GroupId) -> CoreResult<()> {
        Core::delete_group(self, id).await
    }

    async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> CoreResult<()> {
        Core::set_credential(self, profile, kind, secret).await
    }
}

impl<T: SessionBackend + ?Sized> SessionBackend for Arc<T> {
    fn open_session(&self, target: OpenTarget, size: GridSize) -> CoreResult<SessionId> {
        (**self).open_session(target, size)
    }

    fn close_session(&self, session: SessionId) {
        (**self).close_session(session);
    }

    fn session_info(&self, session: SessionId) -> Option<SessionInfo> {
        (**self).session_info(session)
    }

    fn set_session_title(&self, session: SessionId, title: Option<String>) {
        (**self).set_session_title(session, title);
    }

    fn set_visible(&self, session: SessionId, visible: bool) {
        (**self).set_visible(session, visible);
    }

    fn profile(&self, id: ProfileId) -> Option<Profile> {
        (**self).profile(id)
    }
}

impl<T: PromptResponder + ?Sized> PromptResponder for Arc<T> {
    fn respond_prompt(&self, prompt: PromptId, response: PromptResponse) {
        (**self).respond_prompt(prompt, response);
    }
}

impl<T: ProfileStore + ?Sized> ProfileStore for Arc<T> {
    fn profiles(&self) -> Vec<Profile> {
        (**self).profiles()
    }

    fn groups(&self) -> Vec<Group> {
        (**self).groups()
    }

    fn save_profile(&self, profile: Profile) -> CoreResult<()> {
        (**self).save_profile(profile)
    }

    fn mcp_access(&self, profile: ProfileId) -> McpAccess {
        (**self).mcp_access(profile)
    }

    fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> CoreResult<()> {
        (**self).set_mcp_access(profile, level)
    }

    async fn delete_profile(&self, id: ProfileId) -> CoreResult<()> {
        (**self).delete_profile(id).await
    }

    fn save_group(&self, group: Group) -> CoreResult<()> {
        (**self).save_group(group)
    }

    fn group_delete_impact(&self, id: GroupId) -> CoreResult<DeleteImpact> {
        (**self).group_delete_impact(id)
    }

    async fn delete_group(&self, id: GroupId) -> CoreResult<()> {
        (**self).delete_group(id).await
    }

    async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> CoreResult<()> {
        (**self).set_credential(profile, kind, secret).await
    }
}

/// 测试工具：把简单的 future 同步跑完（模型里的 async 方法在测试替身上都是立即完成的）。
#[cfg(test)]
pub(crate) fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
