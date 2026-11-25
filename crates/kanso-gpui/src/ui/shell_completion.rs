//! `ShellView` 与命令补全的接线（terminal.md §14、completion.md），对照 mac 版 `CompletionController`：
//!
//! - 候选内容由核心 `Core::complete` 产生；这里负责：读取终端当前输入（`completion_state`）、
//!   为路径 / Kubernetes 名称异步取外部数据（本地 300 毫秒、远端 1.5 秒内未返回就放弃该来源）、
//!   只采用最新一次请求的结果、被动状态下的按键（↓ 进入列表、Esc 关闭、Tab 接受）、弹窗绘制与位置；
//! - 接受候选 = 每个码点一次退格 + 输入替换文字，让 shell 自己编辑这一行；
//! - 输入法组字中、终端没有键盘焦点、有浮层时不弹出；
//! - 开关 `命令建议` 存在设置表的 `completion` 键里（与 mac 版同一份数据）；
//! - “清除命令历史…”先确认再调核心，失败在状态栏报告。

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, KeyDownEvent, Task, Window, div, prelude::*, px};
use kanso_core::{CompletionEntry, CompletionSources, CompletionState as EditState};
use kanso_gpui::app::completion::{
    Action, CWD_CACHE_MS, CompletionModel, DIRECTORY_CACHE_MS, KUBE_CACHE_MS,
    MAX_DIRECTORY_ENTRIES, POPUP_PADDING, ROW_HEIGHT, SETTING_KEY, anchor_for, category_label,
    decode_enabled, encode_enabled, is_local_host, key_from_name, kubeconfig_paths, needs_external,
    place_popup, replacement_plan, timeout_ms,
};
use kanso_gpui::app::confirm::{CANCEL_ID, CONFIRM_ID};
use kanso_gpui::app::launcher::core_error_text;
use kanso_gpui::app::layout::Rect;
use kanso_gpui::app::sender::t;
use kanso_gpui::app::tab::TabKind;
use kanso_gpui::widgets::dialog::{ButtonSpec, DialogSpec};
use kanso_types::input::{Key, KeyEvent, KeyEventKind, Modifiers};
use kanso_types::{Message, SessionId};

use super::shell::{ShellView, Theme};
use super::widgets::dialog::{Dialog, DialogEvent};
use super::{display, log};

/// 补全相关的外壳状态。
pub(super) struct CompletionCtl {
    /// “命令建议”开关。
    pub enabled: bool,
    model: CompletionModel,
    /// 当前候选对应的终端输入快照。
    last: Option<EditState>,
    /// 候选所属的会话。
    session: Option<SessionId>,
    /// 刚按过键，等 shell 把回显送回来再请求（避免用旧输入请求）。
    awaiting_echo: bool,
    task: Option<Task<()>>,
    deadline: Option<Task<()>>,
    cwd_cache: Option<(String, Instant)>,
    dir_cache: Option<(String, Vec<CompletionEntry>, Instant)>,
    kube_cache: Option<(Vec<String>, Instant)>,
    /// 清除历史的确认框正在显示。
    pub pending_clear: bool,
}

impl CompletionCtl {
    pub(super) fn new(enabled: bool) -> Self {
        CompletionCtl {
            enabled,
            model: CompletionModel::default(),
            last: None,
            session: None,
            awaiting_echo: false,
            task: None,
            deadline: None,
            cwd_cache: None,
            dir_cache: None,
            kube_cache: None,
            pending_clear: false,
        }
    }

    /// 读取设置里保存的开关。
    pub(super) fn load(core: &kanso_core::Core) -> Self {
        Self::new(decode_enabled(core.setting(SETTING_KEY).as_deref()))
    }

    fn invalidate(&mut self) {
        self.task = None;
        self.deadline = None;
        self.model.invalidate();
        self.last = None;
        self.awaiting_echo = false;
    }

    fn clear_caches(&mut self) {
        self.cwd_cache = None;
        self.dir_cache = None;
        self.kube_cache = None;
    }

    /// 刚发出按键：作废候选，等回显。
    fn input_changed(&mut self) {
        self.task = None;
        self.deadline = None;
        self.model.invalidate();
        self.awaiting_echo = true;
    }
}

/// 一次请求在后台收集到的外部数据。
struct Gathered {
    sources: CompletionSources,
    cwd: Option<String>,
    directory: Option<(String, Vec<CompletionEntry>)>,
    configs: Option<Vec<String>>,
}

/// 读取本地目录（后台线程）：最多 2000 项，符号链接按目标判断是否目录。
fn read_local_dir(path: &str) -> Vec<CompletionEntry> {
    let Ok(rd) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    rd.flatten()
        .take(MAX_DIRECTORY_ENTRIES)
        .map(|e| {
            let directory = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
            CompletionEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                directory,
            }
        })
        .collect()
}

/// 本机名称（用来判断目录上报的主机是不是本机）。
fn local_host_names() -> Vec<String> {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .collect()
}

impl ShellView {
    // ---- 开关与历史 ----

    /// 开关“命令建议”并保存。
    pub(super) fn toggle_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = !self.completion.enabled;
        self.completion.enabled = enabled;
        self.completion.invalidate();
        if let Err(e) = self.core.set_setting(SETTING_KEY, encode_enabled(enabled)) {
            log(&format!("保存命令建议开关失败：{e}"));
        }
        if enabled {
            self.completion_refresh(window, cx);
        }
        cx.notify();
    }

    /// “清除命令历史…”：先确认。
    pub(super) fn request_clear_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = None;
        self.menu_sub = None;
        let spec = DialogSpec::new(&t(Message::UiHistoryClearTitle {}))
            .text(&t(Message::UiHistoryClearBody {}))
            .button(
                ButtonSpec::secondary(CANCEL_ID, &t(Message::UiHistoryClearCancel {}))
                    .cancel()
                    .default_button(),
            )
            .button(ButtonSpec::danger(
                CONFIRM_ID,
                &t(Message::UiHistoryClearConfirm {}),
            ));
        let dialog = cx.new(|cx| Dialog::new(spec, window, cx));
        self.dialog_sub =
            Some(
                cx.subscribe_in(&dialog, window, |this, _d, ev: &DialogEvent, window, cx| {
                    let DialogEvent::Result(r) = ev;
                    this.on_dialog_result(r, window, cx);
                }),
            );
        self.dialog = Some(dialog);
        self.completion.pending_clear = true;
        cx.notify();
    }

    /// 确认后真正清除（清空存储里的全部条目），失败在状态栏报告。
    pub(super) fn clear_history(&mut self, cx: &mut Context<Self>) {
        match self.core.clear_history() {
            Ok(()) => self.completion.invalidate(),
            Err(e) => {
                let text = t(Message::UiHistoryClearFailed {
                    detail: core_error_text(&e),
                });
                self.show_notice(text, cx);
            }
        }
    }

    // ---- 请求 ----

    /// 活动终端是否在等输入（有键盘焦点、没有组字、没有浮层）。
    fn completion_ready(&self, sid: SessionId, window: &Window, cx: &gpui::App) -> bool {
        if self.modal_open(cx) {
            return false;
        }
        self.terminals.get(&sid).is_some_and(|t| {
            let t = t.read(cx);
            t.focus.is_focused(window) && !t.ime.is_composing()
        })
    }

    fn initial_sources(&self, state: &EditState, remote: bool) -> CompletionSources {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .ok();
        let local = state
            .directory_host
            .as_deref()
            .is_none_or(|h| is_local_host(h, &local_host_names()));
        CompletionSources {
            cwd: state.directory.clone(),
            home: if remote { None } else { home },
            allow_paths: remote || local,
            entries: Vec::new(),
            configs: Vec::new(),
        }
    }

    /// 终端输入变了（帧更新、开关打开、焦点回来）后重新请求候选。
    pub(super) fn completion_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.completion.enabled {
            return;
        }
        let Some(sid) = self.ctl.workspace().active_session() else {
            self.completion.invalidate();
            return;
        };
        if !self.completion_ready(sid, window, cx) {
            self.completion.invalidate();
            cx.notify();
            return;
        }
        let Some(state) = self.core.completion_state(sid) else {
            self.completion.invalidate();
            cx.notify();
            return;
        };
        if self.completion.session == Some(sid) && self.completion.last.as_ref() == Some(&state) {
            return;
        }
        if self.completion.awaiting_echo
            && self.completion.session == Some(sid)
            && self.completion.last.as_ref().map(|l| &l.input) == Some(&state.input)
        {
            return;
        }
        self.completion.awaiting_echo = false;
        self.completion.task = None;
        self.completion.deadline = None;
        let generation = self.completion.model.begin();
        self.completion.last = Some(state.clone());
        self.completion.session = Some(sid);
        let remote = self
            .ctl
            .workspace()
            .item(sid)
            .is_some_and(|i| i.kind == TabKind::Ssh);
        let initial = self.initial_sources(&state, remote);
        let fast = self.core.complete(sid, &state.input, initial.clone());
        let external = needs_external(&fast);
        self.completion
            .model
            .receive(fast.candidates.clone(), generation, !external);
        cx.notify();
        if !external {
            return;
        }

        // 外部来源：先显示已有候选，再异步补充；超时放弃该来源，只采用最新一次请求的结果。
        let core = self.core.clone();
        let input = state.input.clone();
        let cwd_cached = self
            .completion
            .cwd_cache
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_millis(CWD_CACHE_MS))
            .map(|(c, _)| c.clone());
        let dir_cache = self.completion.dir_cache.clone();
        let kube_cache = self.completion.kube_cache.clone();
        let (needs_cwd, needs_home, needs_kube) =
            (fast.needs_cwd, fast.needs_home, fast.needs_kube_names);
        let first_directory = fast.directory.clone();
        self.completion.task = Some(cx.spawn_in(window, async move |this, cx| {
            let bg = cx.background_executor().clone();
            let mut sources = initial;
            let mut gathered = Gathered {
                sources: CompletionSources::default(),
                cwd: None,
                directory: None,
                configs: None,
            };
            if needs_cwd {
                if let Some(c) = cwd_cached {
                    sources.cwd = Some(c);
                } else {
                    let c = core.clone();
                    let got = bg
                        .spawn(async move { c.working_directory(sid).await.ok() })
                        .await;
                    if let Some(cwd) = got {
                        gathered.cwd = Some(cwd.clone());
                        sources.cwd = Some(cwd);
                    }
                }
            }
            if needs_home {
                let c = core.clone();
                sources.home = bg.spawn(async move { c.sftp_home(sid).await.ok() }).await;
            }
            let directory = if needs_cwd || needs_home {
                core.completion_plan(sid, &input, sources.clone()).directory
            } else {
                first_directory
            };
            if let Some(dir) = directory {
                match dir_cache.filter(|(d, _, at)| {
                    *d == dir && at.elapsed() < Duration::from_millis(DIRECTORY_CACHE_MS)
                }) {
                    Some((_, entries, _)) => sources.entries = entries,
                    None => {
                        let entries = if remote {
                            let c = core.clone();
                            let path = dir.clone();
                            bg.spawn(async move {
                                let mut out: Vec<CompletionEntry> = Vec::new();
                                let Ok(mut stream) = c.sftp_list(sid, &path) else {
                                    return out;
                                };
                                while out.len() < MAX_DIRECTORY_ENTRIES {
                                    let room = MAX_DIRECTORY_ENTRIES - out.len();
                                    match stream.next_batch_limited(room).await {
                                        Ok(Some(batch)) => {
                                            out.extend(batch.into_iter().map(|e| CompletionEntry {
                                                name: e.name,
                                                directory: e.is_dir,
                                            }))
                                        }
                                        _ => break,
                                    }
                                }
                                out
                            })
                            .await
                        } else {
                            let path = dir.clone();
                            bg.spawn(async move { read_local_dir(&path) }).await
                        };
                        sources.entries = entries.clone();
                        gathered.directory = Some((dir, entries));
                    }
                }
            }
            if needs_kube && !remote {
                match kube_cache
                    .filter(|(_, at)| at.elapsed() < Duration::from_millis(KUBE_CACHE_MS))
                {
                    Some((configs, _)) => sources.configs = configs,
                    None => {
                        let files = kubeconfig_paths(
                            std::env::var_os("KUBECONFIG").as_deref(),
                            sources.home.as_deref(),
                        );
                        let configs = bg
                            .spawn(async move {
                                files
                                    .iter()
                                    .filter_map(|p| std::fs::read_to_string(p).ok())
                                    .collect::<Vec<_>>()
                            })
                            .await;
                        sources.configs = configs.clone();
                        gathered.configs = Some(configs);
                    }
                }
            }
            gathered.sources = sources;
            let _ = this.update_in(cx, move |this, _window, cx| {
                if this.completion.model.generation() != generation {
                    return;
                }
                let sources = gathered.sources;
                if let Some(c) = gathered.cwd {
                    this.completion.cwd_cache = Some((c, Instant::now()));
                }
                if let Some((d, e)) = gathered.directory {
                    this.completion.dir_cache = Some((d, e, Instant::now()));
                }
                if let Some(c) = gathered.configs {
                    this.completion.kube_cache = Some((c, Instant::now()));
                }
                let result = this.core.complete(sid, &input, sources);
                this.completion.deadline = None;
                this.completion
                    .model
                    .receive(result.candidates, generation, true);
                cx.notify();
            });
        }));
        let timeout = Duration::from_millis(timeout_ms(remote));
        self.completion.deadline = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(timeout).await;
            let _ = this.update(cx, move |this, cx| {
                if this.completion.model.generation() != generation {
                    return;
                }
                this.completion.task = None;
                this.completion
                    .model
                    .receive(fast.candidates, generation, true);
                cx.notify();
            });
        }));
    }

    // ---- 按键 ----

    /// 捕获阶段先问补全：返回 `true` 表示按键已被弹窗处理（不再往下传）。
    pub(super) fn completion_intercept(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.completion.enabled {
            return false;
        }
        let Some(sid) = self.ctl.workspace().active_session() else {
            return false;
        };
        if self.modal_open(cx) {
            return false;
        }
        let Some(term) = self.terminals.get(&sid).cloned() else {
            return false;
        };
        let (focused, composing) = {
            let t = term.read(cx);
            (t.focus.is_focused(window), t.ime.is_composing())
        };
        // 终端没有键盘焦点（例如在 Sender 命令框里打字）：补全不管。
        if !focused {
            return false;
        }
        let m = &ev.keystroke.modifiers;
        let key = key_from_name(&ev.keystroke.key, m.control, m.alt, m.shift, m.platform);
        match self.completion.model.key(key, composing) {
            Action::Forward => {
                self.completion.input_changed();
                cx.notify();
                false
            }
            Action::Consume => {
                if key == kanso_gpui::app::completion::Key::Escape {
                    self.completion.task = None;
                    self.completion.deadline = None;
                }
                cx.notify();
                true
            }
            Action::Accept(candidate) => {
                // 输入在候选产生之后变了：候选作废，不动用户的输入。
                let fresh = self.completion.session == Some(sid)
                    && self.core.completion_state(sid) == self.completion.last;
                if !fresh {
                    self.completion.invalidate();
                    cx.notify();
                    return true;
                }
                self.completion.input_changed();
                let (delete, text) = replacement_plan(&candidate);
                let text = text.to_owned();
                term.update(cx, |t, _| {
                    for _ in 0..delete {
                        t.send_key(KeyEvent {
                            key: Key::Backspace,
                            mods: Modifiers::NONE,
                            text: None,
                            kind: KeyEventKind::Press,
                        });
                    }
                    t.send_text(&text);
                });
                cx.notify();
                true
            }
        }
    }

    /// 会话关闭、重连、清除历史等：缓存与候选一并作废。
    pub(super) fn completion_reset(&mut self) {
        self.completion.invalidate();
        self.completion.clear_caches();
    }

    // ---- 绘制 ----

    /// 候选弹窗：水平对齐输入起点、放在光标行下方，下方放不下翻到上方。
    pub(super) fn render_completion(
        &self,
        area: Rect,
        th: Theme,
        cx: &gpui::App,
    ) -> Option<AnyElement> {
        let c = &self.completion;
        let sid = self.ctl.workspace().active_session()?;
        if !c.enabled || !c.model.visible() || c.session != Some(sid) || self.modal_open(cx) {
            return None;
        }
        let state = c.last.as_ref()?;
        let term = self.terminals.get(&sid)?.read(cx);
        let origin = term.origin?;
        let grid = term.grid.as_ref()?;
        let (_, cursor_row) = grid.cursor_cell;
        let (x, top, bottom) = anchor_for(
            (f32::from(origin.x), f32::from(origin.y)),
            term.metrics.cell_w,
            term.metrics.cell_h,
            state.start_column,
            cursor_row,
        );
        // 终端窗格的矩形：活动会话所在分屏。
        let ws = self.ctl.workspace();
        // 顶部标签条模式下扣掉标签条：弹窗不能盖到标签上。
        let pane = self
            .content_frames(area)
            .into_iter()
            .find(|(p, _)| ws.active_tab_in(*p) == Some(sid))
            .map(|(_, r)| r)?;
        let bounds = (
            pane.x as f32,
            pane.y as f32,
            pane.width as f32,
            pane.height as f32,
        );
        let rect = place_popup(x, top, bottom, c.model.candidates().len(), bounds);
        let selected = c.model.selected();
        let font = display::terminal_font().family;
        let rows = c.model.candidates().iter().enumerate().map(|(i, cand)| {
            div()
                .h(px(ROW_HEIGHT))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .rounded(px(6.0))
                .when(selected == Some(i), |d| d.bg(th.ca("accent", 0x40)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(font.clone())
                        .text_size(px(13.0))
                        .text_color(th.c("text.primary"))
                        .child(cand.replacement.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(th.c("text.secondary"))
                        .child(category_label(cand.category)),
                )
        });
        Some(
            div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.width))
                .h(px(rect.height))
                .p(px(POPUP_PADDING / 2.0))
                .rounded(px(12.0))
                .bg(th.c("bg.popover"))
                .border_1()
                .border_color(th.c("border.strong"))
                .children(rows)
                .into_any_element(),
        )
    }
}
