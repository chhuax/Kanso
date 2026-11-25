//! `KansoCore`：Swift 持有的 core 对象，把 `kanso_core::Core` 的公开方法全部导出。
//!
//! 方法契约（线程、阻塞、出错）与 `kanso_core::Core` 完全一致，详见 `crates/kanso-core/src/api.rs`；
//! 这里只补充 FFI 层引入的差异：id 与类型的映射见 [`crate::ffi_types`]，
//! `async fn` 在 Swift 侧是 `async throws`，取帧见 [`KansoCore::take_frame`]。
//!
//! 构造器有两个：[`KansoCore::new`]（真实 core，I1 起可用：本地会话）和 [`KansoCore::new_mock`]。
//! 真实 core 的平台能力中，凭据已通过 [`crate::secret_store::FfiSecretStore`] 接入宿主（T8b）；
//! 主机密钥、文件访问、通知暂时仍是内存或空实现，对应回调接口随 I2 / T10 / I5 设计后再接入。

use std::sync::{Arc, Mutex};

use kanso_core::{Core, CoreConfig, EventSink, Platform};
use kanso_types::secret::SecretString;
use kanso_types::{CoreEvent, GroupId, ProfileId, SessionId, SnippetId, TransferId};

use crate::ffi_types::*;
use crate::frame_handle::FrameHandle;
use crate::secret_store::{FfiSecretStore, HostSecretStore};

/// 事件接收端：Swift 实现它，core 把事件推给它。
///
/// - **线程**：core 在**任意工作线程**上调用，不保证是主线程；
/// - **阻塞**：实现必须**立即返回**，只做“把事件投递到主线程”这一件事；
/// - **重入**：实现**不得**在回调里同步调用 `KansoCore` 的任何方法（core 可能正持有内部锁，会死锁）；
/// - **顺序**：同一会话内的事件保序；
/// - 没有返回值，core 不关心事件是否被消费。
#[uniffi::export(with_foreign)]
pub trait FfiEventListener: Send + Sync {
    /// 收到一个事件。
    fn on_event(&self, event: FfiCoreEvent);
}

/// 把 Swift 的 [`FfiEventListener`] 适配成 core 的 [`EventSink`]：只做类型转换后转发。
struct ListenerSink {
    listener: Arc<dyn FfiEventListener>,
}

impl EventSink for ListenerSink {
    fn on_event(&self, event: CoreEvent) {
        self.listener.on_event(FfiCoreEvent::from(&event));
    }
}

fn session(id: u64) -> SessionId {
    SessionId::from_raw(id)
}

/// Swift 持有的 core 对象。线程安全，所有方法可从任意线程调用。
#[derive(uniffi::Object)]
pub struct KansoCore {
    /// 放在 `Arc` 里：MCP 服务（`KansoMcpServer`）要与宿主共用同一个 core。
    pub(crate) inner: Arc<Core>,
}

#[uniffi::export]
impl KansoCore {
    /// 创建 mock core：纯内存、确定性数据，不碰磁盘与网络；事件通过 `listener` 推送。
    ///
    /// 平台能力（凭据、主机密钥、文件访问、通知）全部使用内存或空实现（同 `Platform::for_tests`）。
    #[uniffi::constructor]
    pub fn new_mock(listener: Arc<dyn FfiEventListener>) -> Arc<Self> {
        let platform = Platform::for_tests(Arc::new(ListenerSink { listener }));
        Arc::new(KansoCore {
            inner: Arc::new(Core::new_mock(platform)),
        })
    }

    /// 创建真实 core：建数据目录、启动运行时与会话管理器；事件通过 `listener` 推送。
    ///
    /// 阻塞：创建目录与线程池，毫秒级，但 T8 之后会打开数据库，届时不要在首帧前的关键路径上调用。
    /// 出错：数据目录无法创建等返回 `Internal`。
    /// 平台能力：**凭据已接入宿主的 `secrets`（macOS 为钥匙串）**；**主机密钥使用 core 内置的
    /// `~/.ssh/known_hosts`**（`host_keys = None`，Direct 版规格，ssh.md §3，与系统 ssh 共用）；
    /// 文件访问、通知仍是空实现（同 `Platform::for_tests`）。
    /// `secrets` 会在 core 的 `spawn_blocking` 线程上被调用，见 [`FfiSecretStore`]。
    #[uniffi::constructor]
    pub fn new(
        config: FfiCoreConfig,
        listener: Arc<dyn FfiEventListener>,
        secrets: Arc<dyn FfiSecretStore>,
    ) -> Result<Arc<Self>, FfiCoreError> {
        let mut platform = Platform::for_tests(Arc::new(ListenerSink { listener }));
        platform.secrets = Arc::new(HostSecretStore::new(secrets));
        // `for_tests` 给的是内存主机密钥存储；真实 App 必须读写用户的 known_hosts，否则每次启动都是"未知主机"。
        platform.host_keys = None;
        let config: CoreConfig = config.into();
        Ok(Arc::new(KansoCore {
            inner: Arc::new(Core::new(config, platform)?),
        }))
    }

    /// 关闭 core：停止后台任务、刷盘。应用退出前调用一次，之后不应再使用。
    pub fn shutdown(&self) {
        self.inner.shutdown()
    }

    // ---- 数据 ----

    /// 全部 Profile。
    pub fn profiles(&self) -> Vec<FfiProfile> {
        self.inner.profiles().iter().map(Into::into).collect()
    }

    /// 新增或更新（按 `id`）一个 Profile；校验失败返回 `User(..)`。
    pub fn save_profile(&self, profile: FfiProfile) -> Result<(), FfiCoreError> {
        Ok(self.inner.save_profile(profile.try_into()?)?)
    }

    /// 删除 Profile，连带删除它的凭据。
    pub async fn delete_profile(&self, id: String) -> Result<(), FfiCoreError> {
        let id: ProfileId = parse_id(&id, "profile")?;
        Ok(self.inner.delete_profile(id).await?)
    }

    /// 全部分组。
    pub fn groups(&self) -> Vec<FfiGroup> {
        self.inner.groups().iter().map(Into::into).collect()
    }

    /// 新增或更新（按 `id`）一个分组。
    pub fn save_group(&self, group: FfiGroup) -> Result<(), FfiCoreError> {
        Ok(self.inner.save_group(group.try_into()?)?)
    }

    /// 删除分组会连带删除多少内容。
    pub fn group_delete_impact(&self, id: String) -> Result<FfiDeleteImpact, FfiCoreError> {
        let id: GroupId = parse_id(&id, "group")?;
        Ok(self.inner.group_delete_impact(id)?.into())
    }

    /// 删除分组及其内容；调用方应先用 `group_delete_impact` 让用户确认。
    pub async fn delete_group(&self, id: String) -> Result<(), FfiCoreError> {
        let id: GroupId = parse_id(&id, "group")?;
        Ok(self.inner.delete_group(id).await?)
    }

    /// 全部 Sender 命令。
    pub fn snippets(&self) -> Vec<FfiSnippet> {
        self.inner.snippets().iter().map(Into::into).collect()
    }

    /// 当前标签可见的 Sender 命令。
    pub fn visible_snippets(&self, session: Option<u64>) -> Result<Vec<FfiSnippet>, FfiCoreError> {
        Ok(self
            .inner
            .visible_snippets(session.map(SessionId::from_raw))?
            .iter()
            .map(Into::into)
            .collect())
    }

    /// 启动手动批次或替换重复任务。
    pub fn sender_start(
        &self,
        request: FfiSenderRequest,
        sessions: Vec<u64>,
    ) -> Result<(), FfiCoreError> {
        self.inner.sender_start(
            kanso_core::sender::SenderRequest {
                text: request.text,
                ending: request.ending.into(),
                target: request.target.map(SessionId::from_raw),
                repeating: request.repeating,
                interval_seconds: request.interval_seconds,
                count: request.count,
            },
            sessions.into_iter().map(SessionId::from_raw).collect(),
        )?;
        Ok(())
    }

    /// 停止后续行，不撤回已写入内容。
    pub fn sender_stop(&self, repeating: bool) {
        self.inner.sender_stop(repeating);
    }

    /// 根据当前窗口标签推进发送。
    pub fn sender_tick(&self, sessions: Vec<u64>) -> FfiSenderStatus {
        self.inner
            .sender_tick(sessions.into_iter().map(SessionId::from_raw).collect())
            .into()
    }

    /// 新增或更新（按 `id`）一条 Sender 命令。
    pub fn save_snippet(&self, snippet: FfiSnippet) -> Result<(), FfiCoreError> {
        Ok(self.inner.save_snippet(snippet.try_into()?)?)
    }

    /// 删除一条 Sender 命令；不存在时视为成功。
    pub fn delete_snippet(&self, id: String) -> Result<(), FfiCoreError> {
        let id: SnippetId = parse_id(&id, "snippet")?;
        Ok(self.inner.delete_snippet(id)?)
    }

    /// 保存某个 Profile 的凭据。明文只在本次调用里存在，core 不写日志与数据库。
    pub async fn set_credential(
        &self,
        profile: String,
        kind: FfiSecretKind,
        secret: String,
    ) -> Result<(), FfiCoreError> {
        let profile: ProfileId = parse_id(&profile, "profile")?;
        Ok(self
            .inner
            .set_credential(profile, kind.into(), SecretString::new(secret))
            .await?)
    }

    /// 清除命令历史。
    pub fn clear_history(&self) -> Result<(), FfiCoreError> {
        Ok(self.inner.clear_history()?)
    }

    /// 获取仍可编辑的命令输入。
    pub fn completion_state(&self, session_id: u64) -> Option<FfiCompletionState> {
        self.inner
            .completion_state(session(session_id))
            .map(Into::into)
    }

    /// 对最新输入同步生成候选，不等外部 IO。
    pub fn complete(
        &self,
        session_id: u64,
        input: String,
        sources: FfiCompletionSources,
    ) -> FfiCompletionSuggestions {
        self.inner
            .complete(session(session_id), &input, sources.into())
            .into()
    }

    // ---- 设置 ----

    /// 只探测外部来源，不生成候选。
    pub fn completion_plan(
        &self,
        session_id: u64,
        input: String,
        sources: FfiCompletionSources,
    ) -> FfiCompletionSuggestions {
        self.inner
            .completion_plan(session(session_id), &input, sources.into())
            .into()
    }

    /// 读设置值（JSON 文本）；没有保存过返回 `None`。
    pub fn setting(&self, key: String) -> Option<String> {
        self.inner.setting(&key)
    }

    /// 写设置值；`json` 必须是合法 JSON。
    pub fn set_setting(&self, key: String, json: String) -> Result<(), FfiCoreError> {
        Ok(self.inner.set_setting(&key, json)?)
    }

    /// 读取本地 Shell 默认配置。
    pub fn local_shell_defaults(&self) -> FfiLocalProfile {
        (&self.inner.local_shell_defaults()).into()
    }

    /// 保存本地 Shell 默认配置。
    pub fn set_local_shell_defaults(&self, profile: FfiLocalProfile) -> Result<(), FfiCoreError> {
        Ok(self.inner.set_local_shell_defaults(&profile.try_into()?)?)
    }

    /// 清空全部设置（不动会话、分组、Sender 命令与历史）。
    pub fn reset_settings(&self) -> Result<(), FfiCoreError> {
        Ok(self.inner.reset_settings()?)
    }

    // ---- 会话 ----

    /// 打开会话，立即返回会话 id；连接进度通过事件推送。
    pub fn open_session(
        &self,
        target: FfiOpenTarget,
        size: FfiGridSize,
    ) -> Result<u64, FfiCoreError> {
        Ok(self
            .inner
            .open_session(target.try_into()?, size.into())?
            .raw())
    }

    /// 用户主动关闭会话；会话不存在时忽略。
    pub fn close_session(&self, session_id: u64) {
        self.inner.close_session(session(session_id))
    }

    /// 当前全部会话。
    pub fn sessions(&self) -> Vec<FfiSessionInfo> {
        self.inner.sessions().iter().map(Into::into).collect()
    }

    /// 单个会话的摘要；不存在为 `None`。
    pub fn session_info(&self, session_id: u64) -> Option<FfiSessionInfo> {
        self.inner
            .session_info(session(session_id))
            .as_ref()
            .map(Into::into)
    }

    /// 设置或清除标签自定义名；会话不存在时忽略。
    pub fn set_session_title(&self, session_id: u64, title: Option<String>) {
        self.inner.set_session_title(session(session_id), title)
    }

    /// 告知 core 标签是否可见。
    pub fn set_visible(&self, session_id: u64, visible: bool) {
        self.inner.set_visible(session(session_id), visible)
    }

    /// 调整终端尺寸（网格与像素）；终端因此变脏。
    pub fn resize(
        &self,
        session_id: u64,
        size: FfiGridSize,
        cell_width_px: u16,
        cell_height_px: u16,
    ) -> Result<(), FfiCoreError> {
        Ok(self.inner.resize(
            session(session_id),
            size.into(),
            cell_width_px,
            cell_height_px,
        )?)
    }

    // ---- 输入 ----

    /// 一次按键；core 负责编码成终端字节序列。IME 组字期间的按键不要发。
    pub fn key(&self, session_id: u64, event: FfiKeyEvent) -> Result<(), FfiCoreError> {
        Ok(self.inner.key(session(session_id), event.into())?)
    }

    /// 直接输入文本（IME 上屏等），不经按键编码。
    pub fn text_input(&self, session_id: u64, text: String) -> Result<(), FfiCoreError> {
        Ok(self.inner.text_input(session(session_id), &text)?)
    }

    /// 粘贴。
    pub fn paste(&self, session_id: u64, content: FfiPasteContent) -> Result<(), FfiCoreError> {
        Ok(self.inner.paste(session(session_id), content.into())?)
    }

    /// 鼠标事件。
    pub fn mouse(&self, session_id: u64, event: FfiMouseEvent) -> Result<(), FfiCoreError> {
        Ok(self.inner.mouse(session(session_id), event.into())?)
    }

    /// 滚轮 / 触摸板滚动。`delta_lines` 为带小数的行数，**正数向上**（看更早的内容）。
    pub fn scroll(
        &self,
        session_id: u64,
        delta_lines: f32,
        at: FfiViewportPoint,
        mods: FfiModifiers,
    ) -> Result<(), FfiCoreError> {
        Ok(self
            .inner
            .scroll(session(session_id), delta_lines, at.into(), mods.into())?)
    }

    /// 终端获得 / 失去焦点。
    pub fn focus(&self, session_id: u64, focused: bool) -> Result<(), FfiCoreError> {
        Ok(self.inner.focus(session(session_id), focused)?)
    }

    // ---- 选区与缓冲区 ----

    /// 当前是否有选区。
    pub fn has_selection(&self, session_id: u64) -> bool {
        self.inner.has_selection(session(session_id))
    }

    /// 提取命令块文本；块已裁掉或会话不存在时为 `None`。
    pub fn command_block_text(
        &self,
        session_id: u64,
        id: u64,
        part: crate::frame_handle::FfiBlockText,
    ) -> Option<String> {
        self.inner
            .command_block_text(session(session_id), id, part as u8)
    }

    /// 将相邻块的提示符滚动到视口顶部附近。
    pub fn jump_command_block(
        &self,
        session_id: u64,
        forward: bool,
    ) -> Result<Option<u64>, FfiCoreError> {
        Ok(self
            .inner
            .jump_command_block(session(session_id), forward)?)
    }

    /// 选区文本；没有选区或会话不存在为 `None`。
    pub fn selection_text(&self, session_id: u64) -> Option<String> {
        self.inner.selection_text(session(session_id))
    }

    /// 选中视口内 `at` 处的单词。
    pub fn select_word_at(
        &self,
        session_id: u64,
        at: FfiViewportPoint,
    ) -> Result<(), FfiCoreError> {
        Ok(self.inner.select_word_at(session(session_id), at.into())?)
    }

    /// 全选（含回滚缓冲区）。
    pub fn select_all(&self, session_id: u64) -> Result<(), FfiCoreError> {
        Ok(self.inner.select_all(session(session_id))?)
    }

    /// 清除选区。
    pub fn clear_selection(&self, session_id: u64) -> Result<(), FfiCoreError> {
        Ok(self.inner.clear_selection(session(session_id))?)
    }

    /// 清空缓冲区（⌘K）。
    pub fn clear_buffer(&self, session_id: u64) -> Result<(), FfiCoreError> {
        Ok(self.inner.clear_buffer(session(session_id))?)
    }

    /// 由帧里的超链接 id（OSC 8）取 URL；id 无效或会话不存在为 `None`。
    pub fn hyperlink_url(&self, session_id: u64, id: u32) -> Option<String> {
        self.inner.hyperlink_url(session(session_id), id)
    }

    // ---- 搜索 ----

    /// 开始或更新搜索。
    pub fn search(
        &self,
        session_id: u64,
        query: FfiSearchQuery,
    ) -> Result<FfiSearchStatus, FfiCoreError> {
        Ok(self.inner.search(session(session_id), query.into())?.into())
    }

    /// 跳到下一个（`backwards` 为真则上一个）匹配项。
    pub fn search_next(
        &self,
        session_id: u64,
        backwards: bool,
    ) -> Result<FfiSearchStatus, FfiCoreError> {
        Ok(self
            .inner
            .search_next(session(session_id), backwards)?
            .into())
    }

    /// 关闭搜索并清除高亮。
    pub fn search_close(&self, session_id: u64) {
        self.inner.search_close(session(session_id))
    }

    /// 只读取已维护的搜索状态，不触发搜索或重绘。
    pub fn search_status(&self, session_id: u64) -> Result<FfiSearchStatus, FfiCoreError> {
        Ok(self.inner.search_status(session(session_id))?.into())
    }

    // ---- 显示 ----

    /// 设置时间戳列的显示模式；`session_id` 为 `None` 表示全局。
    pub fn set_timestamp_mode(&self, session_id: Option<u64>, mode: FfiTimestampMode) {
        self.inner
            .set_timestamp_mode(session_id.map(session), mode.into())
    }

    /// 下发终端显示选项（回滚上限、默认光标样式），对现有与之后新开的会话都生效。
    pub fn set_terminal_options(&self, options: FfiTerminalOptions) {
        self.inner.set_terminal_options(options.into())
    }

    // ---- 帧（数据面）----

    /// 取最新帧。
    ///
    /// 语义同 `Core::take_frame`：只有终端自上次取帧后变脏时才返回 `Some` 并清除脏标记；
    /// 没有变化、会话不存在或纯 SFTP 会话返回 `None`。
    ///
    /// `reuse` 传 Swift 当前持有的上一帧句柄：**只有本次返回了新帧时**，`reuse` 的缓冲区才被
    /// core 取走复用（它的 `header_address` 从此返回 0，Swift 不得再读旧地址）；
    /// 返回 `None` 时 `reuse` 原封不动、仍然有效——渲染视图在没有新帧时会继续重绘旧帧
    /// （窗口缩放等），所以这是内存安全的前提。内容零拷贝的生命周期约定见 [`crate::frame_handle`]。
    pub fn take_frame(
        &self,
        session_id: u64,
        reuse: Option<Arc<FrameHandle>>,
    ) -> Option<Arc<FrameHandle>> {
        let frame = match &reuse {
            // 持有句柄的锁调用 core：core 在确定产出新帧之后才从槽里取走旧缓冲区，
            // 没有新帧时槽原样不动，因此不需要在这一层再做"备用槽、延迟一帧复用"。
            // 加锁顺序是 句柄锁 → core 内部的终端锁；`header_address()` 只拿句柄锁，不会反向等待。
            Some(handle) => {
                handle.with_slot(|slot| self.inner.take_frame(session(session_id), slot))
            }
            None => self.inner.take_frame(session(session_id), &mut None),
        };
        frame.map(|f| Arc::new(FrameHandle::new(f)))
    }

    // ---- 提问 ----

    /// 回答一个 `Prompt` 事件；未知或已过期的提问静默忽略。
    pub fn respond_prompt(&self, prompt: u64, response: FfiPromptResponse) {
        self.inner
            .respond_prompt(kanso_types::PromptId::from_raw(prompt), response.into())
    }

    // ---- 工作目录 ----

    /// 会话当前工作目录（⌘J）。
    pub async fn working_directory(&self, session_id: u64) -> Result<String, FfiCoreError> {
        Ok(self.inner.working_directory(session(session_id)).await?)
    }

    // ---- SFTP ----

    /// 远端用户主目录。
    pub async fn sftp_home(&self, session_id: u64) -> Result<String, FfiCoreError> {
        Ok(self.inner.sftp_home(session(session_id)).await?)
    }

    /// 查询远端文件版本。
    pub async fn sftp_stat(
        &self,
        session_id: u64,
        path: String,
    ) -> Result<FfiFileStat, FfiCoreError> {
        Ok(self
            .inner
            .sftp_stat(session(session_id), &path)
            .await?
            .into())
    }

    /// 列出远端目录，流式返回；反复调用返回对象的 `next_batch` 直到 `nil`。
    pub fn sftp_list(
        &self,
        session_id: u64,
        path: String,
    ) -> Result<Arc<FfiDirStream>, FfiCoreError> {
        let stream = self.inner.sftp_list(session(session_id), &path)?;
        Ok(Arc::new(FfiDirStream {
            stream: Mutex::new(Some(stream)),
        }))
    }

    /// 创建远端目录。
    pub async fn sftp_mkdir(&self, session_id: u64, path: String) -> Result<(), FfiCoreError> {
        Ok(self.inner.sftp_mkdir(session(session_id), &path).await?)
    }

    /// 删除远端文件或目录；`is_dir` 为真时递归删除目录。
    pub async fn sftp_remove(
        &self,
        session_id: u64,
        path: String,
        is_dir: bool,
    ) -> Result<(), FfiCoreError> {
        Ok(self
            .inner
            .sftp_remove(session(session_id), &path, is_dir)
            .await?)
    }

    /// 重命名 / 移动远端条目。
    pub async fn sftp_rename(
        &self,
        session_id: u64,
        from: String,
        to: String,
    ) -> Result<(), FfiCoreError> {
        Ok(self
            .inner
            .sftp_rename(session(session_id), &from, &to)
            .await?)
    }

    /// 创建空的远端文件；已存在则失败。
    pub async fn sftp_create_file(
        &self,
        session_id: u64,
        path: String,
    ) -> Result<(), FfiCoreError> {
        Ok(self
            .inner
            .sftp_create_file(session(session_id), &path)
            .await?)
    }

    /// 下载远端文件或目录到本地目录；立即返回传输 id，进度走事件。
    /// `conflict` 为 `Fail` 且目标已存在时传输以 `Failed(already_exists = true)` 结束；
    /// `Skip` 时已存在的文件被跳过，结果 `Completed(skipped)` 带回跳过数。
    pub fn download(
        &self,
        session_id: u64,
        remote_path: String,
        local_dir: String,
        conflict: FfiConflictPolicy,
    ) -> Result<u64, FfiCoreError> {
        Ok(self
            .inner
            .download(
                session(session_id),
                &remote_path,
                &local_dir,
                conflict.into(),
            )?
            .raw())
    }

    /// 上传本地文件或目录到远端目录；约定同 `download`。
    pub fn upload(
        &self,
        session_id: u64,
        local_path: String,
        remote_dir: String,
        conflict: FfiConflictPolicy,
    ) -> Result<u64, FfiCoreError> {
        Ok(self
            .inner
            .upload(
                session(session_id),
                &local_path,
                &remote_dir,
                conflict.into(),
            )?
            .raw())
    }

    /// 取消传输；未知或已结束的传输忽略。
    pub fn cancel_transfer(&self, transfer: u64) {
        self.inner.cancel_transfer(TransferId::from_raw(transfer))
    }

    // ---- 终端内传输 ----

    /// 通过终端用 XMODEM 发送本地文件。
    pub fn xmodem_send(
        &self,
        session_id: u64,
        path: String,
        block: FfiXmodemBlock,
    ) -> Result<u64, FfiCoreError> {
        Ok(self
            .inner
            .xmodem_send(session(session_id), &path, block.into())?
            .raw())
    }

    /// 通过终端用 XMODEM 接收文件，保存到本地 `path`。
    pub fn xmodem_receive(&self, session_id: u64, path: String) -> Result<u64, FfiCoreError> {
        Ok(self.inner.xmodem_receive(session(session_id), &path)?.raw())
    }

    /// 启动 XMODEM 文件面板；未选块大小时接收。
    pub fn xmodem_start(
        &self,
        session_id: u64,
        block: Option<FfiXmodemBlock>,
    ) -> Result<u64, FfiCoreError> {
        Ok(self
            .inner
            .xmodem_start(session(session_id), block.map(Into::into))?
            .raw())
    }

    /// 取消该会话正在进行的终端内传输。
    pub fn cancel_terminal_transfer(&self, session_id: u64) {
        self.inner.cancel_terminal_transfer(session(session_id))
    }

    // ---- 导入导出 ----

    /// 导出全部会话、分组、Sender 命令到 `.kanso` 文件（不含凭据）。
    pub async fn export_data(&self, path: String) -> Result<(), FfiCoreError> {
        Ok(self.inner.export_data(&path).await?)
    }

    /// 从 `.kanso` 文件导入。
    pub async fn import_data(&self, path: String) -> Result<FfiImportReport, FfiCoreError> {
        Ok((&self.inner.import_data(&path).await?).into())
    }

    /// 扫描 OpenSSH 配置；`path` 为 `None` 用默认的 `~/.ssh/config`。
    pub async fn scan_ssh_config(
        &self,
        path: Option<String>,
    ) -> Result<FfiSshConfigScan, FfiCoreError> {
        let found = self.inner.scan_ssh_config(path.as_deref()).await?;
        Ok((&found).into())
    }

    /// 导入所选别名到分组 `group`（`None` 为顶层）。`path` 与扫描时一致，`nil` 为默认的 `~/.ssh/config`。
    pub async fn import_ssh_config(
        &self,
        path: Option<String>,
        aliases: Vec<String>,
        group: Option<String>,
    ) -> Result<FfiSshConfigImportReport, FfiCoreError> {
        let group: Option<GroupId> = group.as_deref().map(|g| parse_id(g, "group")).transpose()?;
        Ok((&self
            .inner
            .import_ssh_config(path.as_deref(), &aliases, group)
            .await?)
            .into())
    }

    // ---- 授权 ----

    /// 当前授权状态；未授权返回 `nil`。
    pub fn entitlement(&self) -> Option<FfiEntitlement> {
        self.inner.entitlement().map(Into::into)
    }

    /// 用 License Key 激活（仅 Direct 渠道）。
    pub async fn activate_license(&self, key: String) -> Result<FfiEntitlement, FfiCoreError> {
        Ok(self.inner.activate_license(&key).await?.into())
    }

    /// 发起商店购买（MAS）。
    pub async fn purchase(&self) -> Result<FfiPurchaseOutcome, FfiCoreError> {
        Ok(self.inner.purchase().await?.into())
    }

    /// 恢复购买；没有可恢复的购买返回 `nil`。
    pub async fn restore_purchases(&self) -> Result<Option<FfiEntitlement>, FfiCoreError> {
        Ok(self.inner.restore_purchases().await?.map(Into::into))
    }
}

/// 大目录的流式列表，由 [`KansoCore::sftp_list`] 创建。
///
/// 反复调用 `next_batch` 直到返回 `nil`；丢弃它即取消后续读取。
#[derive(uniffi::Object)]
pub struct FfiDirStream {
    // `DirStream::next_batch` 需要 `&mut self`，而 UniFFI 对象方法只有 `&self`；
    // 又不能把 std 的 MutexGuard 持有过 `await`（future 会不再 `Send`）。
    // 所以每次调用把流从 Option 里取出、await 完再放回；同一个流并发调用时后到的调用会拿到 `None`
    // 并报错——core 本来就规定“同一个流不能并发调用”，这里把违反约定变成明确的错误而不是阻塞。
    stream: Mutex<Option<kanso_core::DirStream>>,
}

#[uniffi::export]
impl FfiDirStream {
    /// 取下一批条目；`nil` 表示已列完。
    ///
    /// 出错后流不再可用，之后的调用都返回 `Internal` 错误。同一个流不得并发调用。
    pub async fn next_batch(&self) -> Result<Option<Vec<FfiDirEntry>>, FfiCoreError> {
        self.next_batch_limited(u32::MAX).await
    }

    /// 补全读取使用的有限批次；上限之外不解析、不查询链接属性。
    pub async fn next_batch_limited(
        &self,
        limit: u32,
    ) -> Result<Option<Vec<FfiDirEntry>>, FfiCoreError> {
        let taken = self.stream.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(mut stream) = taken else {
            return Err(FfiCoreError::Internal {
                detail: "directory stream is busy or has failed".into(),
            });
        };
        let batch = stream.next_batch_limited(limit as usize).await?;
        // 只有成功才放回：出错后流不可用（core 文档约定）。
        *self.stream.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream);
        Ok(batch.map(|b| b.iter().map(Into::into).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录事件的监听器。
    #[derive(Default)]
    struct Recorder(Mutex<Vec<FfiCoreEvent>>);

    impl FfiEventListener for Recorder {
        fn on_event(&self, event: FfiCoreEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    fn new_core() -> (Arc<KansoCore>, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        (KansoCore::new_mock(rec.clone()), rec)
    }

    fn open_local(core: &KansoCore) -> u64 {
        core.open_session(
            FfiOpenTarget::DefaultLocalShell,
            FfiGridSize { cols: 80, rows: 24 },
        )
        .unwrap()
    }

    #[test]
    fn adapter_forwards_events() {
        // 适配器单测：直接喂 CoreEvent，断言监听器收到转换后的事件。
        let rec = Arc::new(Recorder::default());
        let sink = ListenerSink {
            listener: rec.clone(),
        };
        sink.on_event(CoreEvent::TerminalDirty {
            session: SessionId::from_raw(4),
        });
        assert_eq!(
            rec.0.lock().unwrap().as_slice(),
            [FfiCoreEvent::TerminalDirty { session: 4 }]
        );
    }

    #[test]
    fn open_session_emits_lifecycle_events() {
        let (core, rec) = new_core();
        let id = open_local(&core);
        let events = rec.0.lock().unwrap().clone();
        assert!(matches!(
            events.first(),
            Some(FfiCoreEvent::SessionStateChanged {
                state: FfiSessionState::Connecting,
                ..
            })
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            FfiCoreEvent::SessionStateChanged { state: FfiSessionState::Connected, session } if *session == id
        )));
        assert!(events.contains(&FfiCoreEvent::TerminalDirty { session: id }));
    }

    #[test]
    fn take_frame_and_reuse_semantics() {
        let (core, _rec) = new_core();
        let id = open_local(&core);

        let f1 = core.take_frame(id, None).expect("刚打开的会话是脏的");
        let a1 = f1.header_address();
        assert_ne!(a1, 0);
        assert_eq!(a1 % 8, 0);
        // 没变脏：返回 None，传入的 reuse 原封不动、地址仍然有效。
        assert!(core.take_frame(id, Some(f1.clone())).is_none());
        assert_eq!(f1.header_address(), a1);
        assert!(f1.byte_len() > 0);

        // 按键使会话变脏；取到新帧后旧句柄被回收，地址变 0。
        core.text_input(id, "a".into()).unwrap();
        let f2 = core.take_frame(id, Some(f1.clone())).expect("按键后变脏");
        assert_eq!(f1.header_address(), 0);
        assert_eq!(f1.byte_len(), 0);
        assert_ne!(f2.header_address(), 0);

        // 重复传回已被取走的旧句柄不会崩溃。
        core.text_input(id, "b".into()).unwrap();
        let f3 = core.take_frame(id, Some(f1)).expect("再次变脏");
        assert_ne!(f3.header_address(), 0);
    }

    /// 测试用宿主凭据存储：什么都不存，避免触碰真实钥匙串。
    struct NullSecrets;

    impl FfiSecretStore for NullSecrets {
        fn get(
            &self,
            _account: String,
        ) -> Result<Option<String>, crate::secret_store::FfiSecretError> {
            Ok(None)
        }
        fn set(
            &self,
            _account: String,
            _secret: String,
        ) -> Result<(), crate::secret_store::FfiSecretError> {
            Ok(())
        }
        fn delete(&self, _account: String) -> Result<(), crate::secret_store::FfiSecretError> {
            Ok(())
        }
    }

    #[test]
    fn real_core_constructor_creates_data_dir_and_unimplemented_methods_do_not_panic() {
        let dir = std::env::temp_dir().join(format!("kanso-ffi-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = Arc::new(Recorder(Mutex::new(Vec::new())));
        let core = KansoCore::new(
            FfiCoreConfig {
                data_dir: dir.join("nested").to_string_lossy().into_owned(),
                app_version: "0".into(),
                bundle_id: "t".into(),
                preferred_language: "en".into(),
                region: None,
                default_shell: Some("/bin/sh -i".into()),
                shell_integration: false,
            },
            rec,
            Arc::new(NullSecrets),
        )
        .unwrap();
        assert!(dir.join("nested").is_dir());
        assert_eq!(
            core.entitlement(),
            kanso_core::Entitlement::for_channel().map(Into::into)
        );
        // 未实现（T8）的方法不 panic：集合为空，Profile 目标返回错误。
        assert!(core.profiles().is_empty());
        assert!(
            core.open_session(
                FfiOpenTarget::Profile {
                    id: kanso_types::ProfileId::new().to_string()
                },
                FfiGridSize { cols: 80, rows: 24 }
            )
            .is_err()
        );
        core.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_frame_unknown_session_is_none() {
        let (core, _rec) = new_core();
        assert!(core.take_frame(999, None).is_none());
    }

    #[test]
    fn errors_map_to_ffi_errors() {
        let (core, _rec) = new_core();
        assert_eq!(core.key(999, key_a()), Err(FfiCoreError::NoSuchSession));
        // 空主机的 SSH profile：User 错误且 key 稳定。
        let profile = FfiProfile {
            id: kanso_types::ProfileId::new().to_string(),
            name: "x".into(),
            color: None,
            group: None,
            kind: FfiProfileKind::Ssh {
                profile: FfiSshProfile {
                    target: FfiSshTarget {
                        host: String::new(),
                        port: 22,
                        user: "u".into(),
                        auth: FfiAuthMethod::Password,
                        private_key_path: None,
                        jump: None,
                        proxy_command: None,
                        keepalive: FfiKeepaliveConfig {
                            enabled: true,
                            interval_secs: 30,
                            max_misses: 3,
                            idle_enabled: false,
                            idle_interval_secs: 3600,
                            idle_string: "\\r".into(),
                        },
                    },
                    encoding: String::new(),
                    locale: None,
                },
            },
        };
        match core.save_profile(profile) {
            Err(FfiCoreError::User { message }) => assert_eq!(message.key, "profile-host-empty"),
            other => panic!("非预期：{other:?}"),
        }
        // 非法 id 字符串是 Internal。
        assert!(matches!(
            core.group_delete_impact("nope".into()),
            Err(FfiCoreError::Internal { .. })
        ));
    }

    fn key_a() -> FfiKeyEvent {
        FfiKeyEvent {
            key: FfiKey::Character { text: "a".into() },
            mods: FfiModifiers::default(),
            text: Some("a".into()),
            kind: FfiKeyEventKind::Press,
        }
    }

    /// 最小的 `block_on`：mock 的 future 不会真正挂起，轮询一次即完成。
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("mock 的 future 不应挂起"),
        }
    }

    #[test]
    fn async_methods_and_dir_stream() {
        let (core, _rec) = new_core();
        let id = open_local(&core);
        assert_eq!(
            block_on(core.working_directory(id)).unwrap(),
            "~/workspace/kanso"
        );
        assert!(matches!(
            block_on(core.working_directory(12345)),
            Err(FfiCoreError::NoSuchSession)
        ));
        // 本地会话没有 SFTP。
        assert!(matches!(
            core.sftp_list(id, "/".into()),
            Err(FfiCoreError::NotSupported)
        ));
    }

    #[test]
    fn dir_stream_yields_batches_then_none() {
        let (core, _rec) = new_core();
        let sftp = core
            .profiles()
            .into_iter()
            .find(|p| matches!(p.kind, FfiProfileKind::Sftp { .. }))
            .expect("mock 带有 SFTP 种子 profile");
        let id = core
            .open_session(
                FfiOpenTarget::Profile { id: sftp.id },
                FfiGridSize { cols: 80, rows: 24 },
            )
            .unwrap();
        let stream = core.sftp_list(id, "/srv".into()).unwrap();
        let first = block_on(stream.next_batch()).unwrap().expect("第一批");
        assert_eq!(first[0].path, "/srv/docs");
        assert!(block_on(stream.next_batch()).unwrap().is_some());
        assert!(block_on(stream.next_batch()).unwrap().is_none());
        assert!(block_on(core.sftp_home(id)).unwrap().starts_with("/home/"));
    }
}
