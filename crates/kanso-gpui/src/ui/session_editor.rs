//! 会话编辑对话框（ui-shell.md §5）：新建与编辑已保存的会话。
//!
//! 字段与显隐规则在库里的 `app::sessions::SessionEditor`（三平台可测）：按类型显示本地 / SSH 字段、
//! 认证方式决定显示密码还是私钥、跳板机与 ProxyCommand 互斥。校验以核心为准，这里只把错误原样显示。
//! 密码与私钥口令只在内存里，保存成功后清空，写入走 `kanso-secret`（经 `Core::set_credential`）。
//!
//! 键盘：Tab / Shift+Tab 只在对话框内循环（字段 → 按钮 → 回到第一个）；回车 = 保存并连接；Esc = 取消。
//! 点遮罩不关闭（与 mac sheet 一致）。色板只支持鼠标点选。

use std::sync::Arc;

use gpui::{
    App, BoxShadow, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, PathPromptOptions, ScrollHandle, Subscription, Window,
    WindowAppearance, div, point, prelude::*, px, rgb, rgba,
};
use kanso_core::Core;
use kanso_gpui::app::launcher::{
    auth_label, editor_error_text, kind_label, render_message, toggle_color,
};
use kanso_gpui::app::sessions::{EditorField, EncodingOption, SaveOutcome, SessionEditor};
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::{Appearance, color, number};
use kanso_types::ProfileId;
use kanso_types::profile::{AuthMethod, Group, Profile, SessionKind};

use super::log;
use super::shell::Theme;
use super::widgets::dropdown::{Dropdown, DropdownEvent};
use super::widgets::text_input::{TextInput, TextInputEvent};

const KINDS: [SessionKind; 3] = [SessionKind::Local, SessionKind::Ssh, SessionKind::Sftp];
const AUTHS: [AuthMethod; 3] = [
    AuthMethod::Password,
    AuthMethod::PrivateKey,
    AuthMethod::Agent,
];

/// 编辑对话框向宿主发出的事件。
#[derive(Clone, Debug)]
pub(crate) enum EditorEvent {
    /// 取消（Esc、取消按钮）。
    Cancel,
    /// 已保存（`connect` 为真时宿主接着连接它）。
    Saved { id: ProfileId, connect: bool },
}

/// 一个文本字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Text {
    Name,
    Shell,
    StartDir,
    Host,
    Port,
    User,
    Password,
    KeyPath,
    KeyPassphrase,
    Proxy,
    Locale,
}

/// 一个下拉字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Kind,
    Group,
    Auth,
    Jump,
    Encoding,
}

/// Tab 环上的一站（字段，不含按钮）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Text(Text),
    Pick(Pick),
}

/// 表单的一行（滚动到可见用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Kind,
    Name,
    Color,
    Group,
    Shell,
    StartDir,
    HostPort,
    UserAuth,
    Password,
    KeyPath,
    KeyPassphrase,
    Jump,
    Proxy,
    EncodingLocale,
}

fn row_of(stop: Stop) -> Row {
    match stop {
        Stop::Pick(Pick::Kind) => Row::Kind,
        Stop::Text(Text::Name) => Row::Name,
        Stop::Pick(Pick::Group) => Row::Group,
        Stop::Text(Text::Shell) => Row::Shell,
        Stop::Text(Text::StartDir) => Row::StartDir,
        Stop::Text(Text::Host | Text::Port) => Row::HostPort,
        Stop::Text(Text::User) | Stop::Pick(Pick::Auth) => Row::UserAuth,
        Stop::Text(Text::Password) => Row::Password,
        Stop::Text(Text::KeyPath) => Row::KeyPath,
        Stop::Text(Text::KeyPassphrase) => Row::KeyPassphrase,
        Stop::Pick(Pick::Jump) => Row::Jump,
        Stop::Text(Text::Proxy) => Row::Proxy,
        Stop::Pick(Pick::Encoding) | Stop::Text(Text::Locale) => Row::EncodingLocale,
    }
}

/// 按钮（Tab 环的最后三站）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Btn {
    Cancel,
    Save,
    Connect,
}

/// 会话编辑对话框视图。
pub(crate) struct SessionEditorView {
    model: Option<SessionEditor<Arc<Core>>>,
    /// 保存进行中（模型暂时被取走）。
    saving: bool,
    /// 每帧从模型刷新的可见字段快照（保存期间模型不在手里，界面仍要能画）。
    visible: Vec<EditorField>,
    editing: bool,
    error: Option<String>,

    groups: Vec<Group>,
    jumps: Vec<Profile>,
    encodings: Vec<EncodingOption>,
    color: Option<kanso_types::geometry::Rgb>,
    color_options: Vec<kanso_types::geometry::Rgb>,

    name: Entity<TextInput>,
    shell: Entity<TextInput>,
    start_dir: Entity<TextInput>,
    host: Entity<TextInput>,
    port: Entity<TextInput>,
    user: Entity<TextInput>,
    password: Entity<TextInput>,
    key_path: Entity<TextInput>,
    key_passphrase: Entity<TextInput>,
    proxy: Entity<TextInput>,
    locale: Entity<TextInput>,
    kind: Entity<Dropdown>,
    group: Entity<Dropdown>,
    auth: Entity<Dropdown>,
    jump: Entity<Dropdown>,
    encoding: Entity<Dropdown>,

    buttons: [FocusHandle; 3],
    root: FocusHandle,
    scroll: ScrollHandle,
    /// 上一帧画出的行（按 Tab 聚焦字段时据此滚动到可见）。
    rows: Vec<Row>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for SessionEditorView {}

impl Focusable for SessionEditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root.clone()
    }
}

fn appearance(window: &Window) -> Appearance {
    match window.appearance() {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
        _ => Appearance::Dark,
    }
}

const ALL_FIELDS: [EditorField; 17] = [
    EditorField::Name,
    EditorField::Kind,
    EditorField::Color,
    EditorField::Group,
    EditorField::ShellCommand,
    EditorField::StartDir,
    EditorField::Host,
    EditorField::Port,
    EditorField::User,
    EditorField::Auth,
    EditorField::Password,
    EditorField::KeyPath,
    EditorField::KeyPassphrase,
    EditorField::Jump,
    EditorField::ProxyCommand,
    EditorField::Encoding,
    EditorField::Locale,
];

impl SessionEditorView {
    /// 新建（`editing` 为空）或编辑已保存的会话。必须在窗口上下文里创建。
    pub(crate) fn new(
        core: Arc<Core>,
        editing: Option<Profile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = match editing {
            Some(p) => SessionEditor::editing(core.clone(), Default::default(), p),
            None => SessionEditor::new(core.clone(), Default::default()),
        };
        let is_editing = model.is_editing();
        let groups = model.groups();
        let jumps = model.jump_candidates();
        let encodings = model.encoding_options();

        let mut subs: Vec<Subscription> = Vec::new();
        let mut text = |t: Text,
                        value: &str,
                        placeholder: &str,
                        masked: bool,
                        window: &mut Window,
                        cx: &mut Context<Self>|
         -> Entity<TextInput> {
            let (value, placeholder) = (value.to_owned(), placeholder.to_owned());
            let input = cx.new(|cx| {
                let mut i = TextInput::new(window, cx).placeholder(placeholder);
                if masked {
                    i = i.masked();
                }
                i.with_text(&value)
            });
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this, _i, ev: &TextInputEvent, window, cx| match ev {
                    TextInputEvent::Change(s) => this.on_text(t, s, cx),
                    TextInputEvent::Submit(_) => this.activate(Btn::Connect, window, cx),
                    TextInputEvent::Escape => this.cancel(cx),
                    _ => {}
                },
            ));
            input
        };
        let keep = if is_editing {
            "留空则不修改已保存的值"
        } else {
            ""
        };
        let name = text(Text::Name, &model.name, "", false, window, cx);
        let shell = text(
            Text::Shell,
            &model.shell_command,
            "留空使用默认 Shell",
            false,
            window,
            cx,
        );
        let start_dir = text(
            Text::StartDir,
            &model.start_dir,
            "留空使用用户主目录",
            false,
            window,
            cx,
        );
        let host = text(Text::Host, &model.host, "", false, window, cx);
        let port = text(Text::Port, &model.port_text, "22", false, window, cx);
        let user = text(Text::User, &model.user, "", false, window, cx);
        let password = text(Text::Password, "", keep, true, window, cx);
        let key_path = text(
            Text::KeyPath,
            &model.key_path,
            "例如 C:\\Users\\你\\.ssh\\id_ed25519",
            false,
            window,
            cx,
        );
        let key_passphrase = text(Text::KeyPassphrase, "", keep, true, window, cx);
        let proxy = text(
            Text::Proxy,
            &model.proxy_command,
            "例如 ssh -W %h:%p 跳板机",
            false,
            window,
            cx,
        );
        let locale = text(Text::Locale, &model.locale, "自动", false, window, cx);

        let mut dropdown = |p: Pick,
                            items: Vec<Item>,
                            selected: usize,
                            window: &mut Window,
                            cx: &mut Context<Self>|
         -> Entity<Dropdown> {
            let dd = cx.new(|cx| Dropdown::new(window, cx).items(items).selected(selected));
            subs.push(cx.subscribe_in(
                &dd,
                window,
                move |this, _d, ev: &DropdownEvent, _window, cx| {
                    if let DropdownEvent::Change(i) = ev {
                        this.on_pick(p, *i, cx);
                    }
                },
            ));
            dd
        };
        let kind = dropdown(
            Pick::Kind,
            KINDS.iter().map(|k| Item::option(kind_label(*k))).collect(),
            KINDS.iter().position(|k| *k == model.kind).unwrap_or(0),
            window,
            cx,
        );
        let group = dropdown(
            Pick::Group,
            std::iter::once(Item::option("不分组"))
                .chain(groups.iter().map(|g| Item::option(g.name.clone())))
                .collect(),
            model
                .group
                .and_then(|id| groups.iter().position(|g| g.id == id))
                .map_or(0, |i| i + 1),
            window,
            cx,
        );
        let auth = dropdown(
            Pick::Auth,
            AUTHS.iter().map(|a| Item::option(auth_label(*a))).collect(),
            AUTHS.iter().position(|a| *a == model.auth).unwrap_or(0),
            window,
            cx,
        );
        let jump = dropdown(
            Pick::Jump,
            std::iter::once(Item::option("无"))
                .chain(jumps.iter().map(|p| Item::option(p.name.clone())))
                .collect(),
            model
                .jump
                .and_then(|id| jumps.iter().position(|p| p.id == id))
                .map_or(0, |i| i + 1),
            window,
            cx,
        );
        let encoding = dropdown(
            Pick::Encoding,
            encodings
                .iter()
                .map(|e| Item::option(e.title.clone()))
                .collect(),
            encodings
                .iter()
                .position(|e| e.label == model.encoding)
                .unwrap_or(0),
            window,
            cx,
        );

        let color = model.color;
        let color_options = model.color_options();
        let mut view = SessionEditorView {
            model: Some(model),
            saving: false,
            visible: Vec::new(),
            editing: is_editing,
            error: None,
            groups,
            jumps,
            encodings,
            color,
            color_options,
            name,
            shell,
            start_dir,
            host,
            port,
            user,
            password,
            key_path,
            key_passphrase,
            proxy,
            locale,
            kind,
            group,
            auth,
            jump,
            encoding,
            buttons: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            root: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            rows: Vec::new(),
            _subs: subs,
        };
        view.refresh_visible();
        view.sync_exclusive(cx);
        view.name.read(cx).focus(window);
        // 首帧渲染之后再聚焦一次，保证焦点节点已进入调度树。
        cx.defer_in(window, |this, window, cx| this.name.read(cx).focus(window));
        view
    }

    fn refresh_visible(&mut self) {
        if let Some(m) = &self.model {
            self.visible = ALL_FIELDS
                .iter()
                .copied()
                .filter(|f| m.is_visible(*f))
                .collect();
        }
    }

    fn vis(&self, f: EditorField) -> bool {
        self.visible.contains(&f)
    }

    /// 跳板机与 ProxyCommand 互斥：选了一个就禁用另一个。
    fn sync_exclusive(&mut self, cx: &mut Context<Self>) {
        let Some(m) = &self.model else { return };
        let (jump_enabled, proxy_enabled) = (m.is_jump_enabled(), m.is_proxy_command_enabled());
        self.jump
            .update(cx, |d, cx| d.set_disabled(!jump_enabled, cx));
        self.proxy
            .update(cx, |t, cx| t.set_disabled(!proxy_enabled, cx));
    }

    // ---- 字段 → 模型 ----

    fn on_text(&mut self, field: Text, value: &str, cx: &mut Context<Self>) {
        let Some(m) = self.model.as_mut() else { return };
        let v = value.to_owned();
        match field {
            Text::Name => m.name = v,
            Text::Shell => m.shell_command = v,
            Text::StartDir => m.start_dir = v,
            Text::Host => m.host = v,
            Text::Port => {
                let clean = kanso_gpui::app::sessions::sanitized_port_text(&v);
                if clean != v {
                    // 端口只接受数字：把过滤后的内容写回输入框（再触发一次 Change，到时 clean == v）。
                    self.port.update(cx, |t, cx| t.set_text(&clean, cx));
                    return;
                }
                m.port_text = v;
            }
            Text::User => m.user = v,
            Text::Password => m.password = v,
            Text::KeyPath => m.key_path = v,
            Text::KeyPassphrase => m.key_passphrase = v,
            Text::Proxy => m.proxy_command = v,
            Text::Locale => m.locale = v,
        }
        if field == Text::Proxy {
            self.sync_exclusive(cx);
        }
    }

    fn on_pick(&mut self, pick: Pick, index: usize, cx: &mut Context<Self>) {
        let Some(m) = self.model.as_mut() else { return };
        match pick {
            Pick::Kind => m.kind = KINDS[index.min(KINDS.len() - 1)],
            Pick::Group => {
                m.group = index
                    .checked_sub(1)
                    .and_then(|i| self.groups.get(i))
                    .map(|g| g.id)
            }
            Pick::Auth => m.auth = AUTHS[index.min(AUTHS.len() - 1)],
            Pick::Jump => {
                m.jump = index
                    .checked_sub(1)
                    .and_then(|i| self.jumps.get(i))
                    .map(|p| p.id)
            }
            Pick::Encoding => {
                if let Some(e) = self.encodings.get(index) {
                    m.encoding = e.label.clone();
                }
            }
        }
        self.refresh_visible();
        if pick == Pick::Jump {
            self.sync_exclusive(cx);
        }
        cx.notify();
    }

    fn pick_color(&mut self, c: kanso_types::geometry::Rgb, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.color = toggle_color(self.color, c);
        if let Some(m) = self.model.as_mut() {
            m.color = self.color;
        }
        cx.notify();
    }

    fn browse_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择私钥文件".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.first()
            {
                let text = p.to_string_lossy().into_owned();
                this.update_in(cx, |this, _w, cx| {
                    this.key_path.update(cx, |t, cx| t.set_text(&text, cx));
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- 动作 ----

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if log_enabled() {
            log("session-editor cancel");
        }
        cx.emit(EditorEvent::Cancel);
    }

    fn activate(&mut self, btn: Btn, window: &mut Window, cx: &mut Context<Self>) {
        match btn {
            Btn::Cancel => self.cancel(cx),
            Btn::Save => self.save(false, window, cx),
            Btn::Connect => self.save(true, window, cx),
        }
    }

    fn save(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(mut model) = self.model.take() else {
            return;
        };
        self.refresh_visible_from(&model);
        self.saving = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = model.save().await;
            this.update_in(cx, |this, _window, cx| {
                let id = model.profile_id();
                this.error = model.error().map(|e| editor_error_text(e, render_message));
                this.model = Some(model);
                this.saving = false;
                if log_enabled() {
                    log(&format!("session-editor save -> {outcome:?} id={id}"));
                }
                match outcome {
                    SaveOutcome::Saved => cx.emit(EditorEvent::Saved { id, connect }),
                    SaveOutcome::Failed | SaveOutcome::SavedButCredentialFailed => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_visible_from(&mut self, model: &SessionEditor<Arc<Core>>) {
        self.visible = ALL_FIELDS
            .iter()
            .copied()
            .filter(|f| model.is_visible(*f))
            .collect();
    }

    // ---- 焦点 ----

    fn text_entity(&self, t: Text) -> &Entity<TextInput> {
        match t {
            Text::Name => &self.name,
            Text::Shell => &self.shell,
            Text::StartDir => &self.start_dir,
            Text::Host => &self.host,
            Text::Port => &self.port,
            Text::User => &self.user,
            Text::Password => &self.password,
            Text::KeyPath => &self.key_path,
            Text::KeyPassphrase => &self.key_passphrase,
            Text::Proxy => &self.proxy,
            Text::Locale => &self.locale,
        }
    }

    fn pick_entity(&self, p: Pick) -> &Entity<Dropdown> {
        match p {
            Pick::Kind => &self.kind,
            Pick::Group => &self.group,
            Pick::Auth => &self.auth,
            Pick::Jump => &self.jump,
            Pick::Encoding => &self.encoding,
        }
    }

    /// 当前可聚焦的字段，按画面顺序。
    fn stops(&self) -> Vec<Stop> {
        use EditorField as F;
        let mut v = vec![
            (F::Kind, Stop::Pick(Pick::Kind)),
            (F::Name, Stop::Text(Text::Name)),
            (F::Group, Stop::Pick(Pick::Group)),
            (F::ShellCommand, Stop::Text(Text::Shell)),
            (F::StartDir, Stop::Text(Text::StartDir)),
            (F::Host, Stop::Text(Text::Host)),
            (F::Port, Stop::Text(Text::Port)),
            (F::User, Stop::Text(Text::User)),
            (F::Auth, Stop::Pick(Pick::Auth)),
            (F::Password, Stop::Text(Text::Password)),
            (F::KeyPath, Stop::Text(Text::KeyPath)),
            (F::KeyPassphrase, Stop::Text(Text::KeyPassphrase)),
            (F::Jump, Stop::Pick(Pick::Jump)),
            (F::ProxyCommand, Stop::Text(Text::Proxy)),
            (F::Encoding, Stop::Pick(Pick::Encoding)),
            (F::Locale, Stop::Text(Text::Locale)),
        ];
        v.retain(|(f, _)| self.vis(*f));
        v.into_iter().map(|(_, s)| s).collect()
    }

    fn stop_handle(&self, s: Stop, cx: &App) -> FocusHandle {
        match s {
            Stop::Text(t) => self.text_entity(t).read(cx).focus_handle(cx),
            Stop::Pick(p) => self.pick_entity(p).read(cx).focus_handle(cx),
        }
    }

    /// 站点是否可用（被互斥禁用的跳板机 / ProxyCommand 跳过）。
    fn stop_enabled(&self, s: Stop) -> bool {
        match (s, &self.model) {
            (Stop::Pick(Pick::Jump), Some(m)) => m.is_jump_enabled(),
            (Stop::Text(Text::Proxy), Some(m)) => m.is_proxy_command_enabled(),
            _ => true,
        }
    }

    /// Tab 环：可用字段 + 三个按钮。`Err(btn)` 是按钮。
    fn ring(&self) -> Vec<Result<Stop, Btn>> {
        self.stops()
            .into_iter()
            .filter(|s| self.stop_enabled(*s))
            .map(Ok)
            .chain([Btn::Cancel, Btn::Save, Btn::Connect].into_iter().map(Err))
            .collect()
    }

    fn handle_of(&self, item: Result<Stop, Btn>, cx: &App) -> FocusHandle {
        match item {
            Ok(s) => self.stop_handle(s, cx),
            Err(b) => self.buttons[b as usize].clone(),
        }
    }

    fn focused_button(&self, window: &Window) -> Option<Btn> {
        [Btn::Cancel, Btn::Save, Btn::Connect]
            .into_iter()
            .find(|b| self.buttons[*b as usize].is_focused(window))
    }

    fn any_dropdown_open(&self, cx: &App) -> bool {
        [
            Pick::Kind,
            Pick::Group,
            Pick::Auth,
            Pick::Jump,
            Pick::Encoding,
        ]
        .into_iter()
        .any(|p| self.pick_entity(p).read(cx).is_open())
    }

    fn composing(&self, cx: &App) -> bool {
        [
            Text::Name,
            Text::Shell,
            Text::StartDir,
            Text::Host,
            Text::Port,
            Text::User,
            Text::Password,
            Text::KeyPath,
            Text::KeyPassphrase,
            Text::Proxy,
            Text::Locale,
        ]
        .into_iter()
        .any(|t| self.text_entity(t).read(cx).is_composing())
    }

    fn move_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ring = self.ring();
        let n = ring.len();
        let cur = ring
            .iter()
            .position(|it| self.handle_of(*it, cx).is_focused(window));
        let next = match (cur, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        let target = ring[next];
        if let Ok(stop) = target
            && let Some(i) = self.rows.iter().position(|r| *r == row_of(stop))
        {
            self.scroll.scroll_to_item(i);
        }
        window.focus(&self.handle_of(target, cx));
        cx.notify();
    }

    /// 捕获阶段：Tab 焦点陷阱、回车、空格（先于输入框）。
    fn on_capture_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if self.saving || m.control || m.alt || m.platform || self.composing(cx) {
            return;
        }
        match ev.keystroke.key.as_str() {
            // 下拉浮层展开时，Tab / 回车 / Esc 归下拉框自己。
            _ if self.any_dropdown_open(cx) => {}
            "tab" => {
                self.move_focus(!m.shift, window, cx);
                cx.stop_propagation();
            }
            "space" if !m.shift => {
                if let Some(b) = self.focused_button(window) {
                    self.activate(b, window, cx);
                    cx.stop_propagation();
                }
            }
            "enter" => {
                // 焦点在下拉框 / 按钮上：下拉框打开浮层，按钮触发自己；输入框的回车走 Submit 事件。
                if let Some(b) = self.focused_button(window) {
                    self.activate(b, window, cx);
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }

    /// 冒泡阶段：焦点不在输入框 / 展开的下拉框里时的 Esc。
    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        if ev.keystroke.key == "escape" && !m.control && !m.alt && !m.platform {
            self.cancel(cx);
            cx.stop_propagation();
        }
    }

    // ---- 绘制 ----

    fn label(&self, th: Theme, text: &'static str) -> gpui::Div {
        div()
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .child(text)
    }

    fn field(&self, th: Theme, label: &'static str, control: impl IntoElement) -> gpui::Div {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(self.label(th, label))
            .child(control)
    }

    fn render_palette(&self, th: Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut grid = div().flex().flex_row().flex_wrap().gap(px(6.0));
        for (i, c) in self.color_options.iter().copied().enumerate() {
            let selected = self.color == Some(c);
            let fill: Hsla =
                rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into();
            grid = grid.child(
                div()
                    .id(("swatch", i))
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded_full()
                    .bg(fill)
                    .border_2()
                    .border_color(if selected {
                        th.c("text.primary")
                    } else {
                        gpui::transparent_black()
                    })
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                            this.pick_color(c, cx);
                        }),
                    ),
            );
        }
        grid
    }

    fn render_button(
        &self,
        btn: Btn,
        label: &'static str,
        primary: bool,
        window: &Window,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let handle = self.buttons[btn as usize].clone();
        let focused = handle.is_focused(window);
        let (bg, fg, border): (Hsla, Hsla, Hsla) = if primary {
            (th.c("accent"), th.c("text.onAccent"), th.c("accent"))
        } else {
            (
                th.c("bg.buttonSecondary"),
                th.c("text.primary"),
                th.c("border.strong"),
            )
        };
        let ring: Hsla = rgba(color("accent", th.appearance).with_alpha(0x80).to_u32()).into();
        div()
            .id(("editor-button", btn as usize))
            .track_focus(&handle)
            .cursor_pointer()
            .min_w(px(84.0))
            .h(px(30.0))
            .px(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(px(13.0))
            .when(self.saving, |d| d.opacity(0.5))
            .hover(|s| s.opacity(0.88))
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: ring,
                    offset: point(px(0.0), px(0.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(2.0),
                }])
            })
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                if !ev.is_keyboard() {
                    this.activate(btn, window, cx);
                }
            }))
            .child(label)
    }
}

fn log_enabled() -> bool {
    crate::ui::LOG_INPUT.load(std::sync::atomic::Ordering::Relaxed)
}

impl Render for SessionEditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = Theme {
            appearance: appearance(window),
        };
        if let Some(m) = &self.model {
            self.visible = ALL_FIELDS
                .iter()
                .copied()
                .filter(|f| m.is_visible(*f))
                .collect();
        }
        use EditorField as F;

        let mut rows: Vec<(Row, gpui::AnyElement)> = Vec::new();
        rows.push((
            Row::Kind,
            self.field(th, "类型", self.kind.clone()).into_any_element(),
        ));
        rows.push((
            Row::Name,
            self.field(th, "名称", self.name.clone()).into_any_element(),
        ));
        rows.push((
            Row::Color,
            self.field(
                th,
                "颜色（再次点击已选颜色可取消）",
                self.render_palette(th, cx),
            )
            .into_any_element(),
        ));
        rows.push((
            Row::Group,
            self.field(th, "分组", self.group.clone())
                .into_any_element(),
        ));
        if self.vis(F::ShellCommand) {
            rows.push((
                Row::Shell,
                self.field(th, "Shell 命令行", self.shell.clone())
                    .into_any_element(),
            ));
            rows.push((
                Row::StartDir,
                self.field(th, "起始目录", self.start_dir.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::Host) {
            rows.push((
                Row::HostPort,
                div()
                    .flex()
                    .flex_row()
                    .gap(px(12.0))
                    .child(self.field(th, "主机", self.host.clone()))
                    .child(
                        self.field(th, "端口", self.port.clone())
                            .flex_none()
                            .w(px(110.0)),
                    )
                    .into_any_element(),
            ));
            rows.push((
                Row::UserAuth,
                div()
                    .flex()
                    .flex_row()
                    .gap(px(12.0))
                    .child(self.field(th, "用户名", self.user.clone()))
                    .child(self.field(th, "认证方式", self.auth.clone()))
                    .into_any_element(),
            ));
        }
        if self.vis(F::Password) {
            rows.push((
                Row::Password,
                self.field(th, "密码", self.password.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::KeyPath) {
            rows.push((
                Row::KeyPath,
                self.field(
                    th,
                    "私钥文件",
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(self.key_path.clone()))
                        .child(
                            div()
                                .id("browse-key")
                                .flex_none()
                                .h(px(number("spacing", "field.height")))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .rounded(px(number("radius", "md")))
                                .border_1()
                                .border_color(th.c("border.strong"))
                                .bg(th.c("bg.buttonSecondary"))
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.88))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                        this.browse_key(window, cx)
                                    }),
                                )
                                .child("选择…"),
                        ),
                )
                .into_any_element(),
            ));
            rows.push((
                Row::KeyPassphrase,
                self.field(th, "私钥口令", self.key_passphrase.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::Jump) {
            rows.push((
                Row::Jump,
                self.field(th, "跳板机", self.jump.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::ProxyCommand) {
            rows.push((
                Row::Proxy,
                self.field(th, "ProxyCommand（与跳板机二选一）", self.proxy.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::Encoding) {
            rows.push((
                Row::EncodingLocale,
                div()
                    .flex()
                    .flex_row()
                    .gap(px(12.0))
                    .child(self.field(th, "编码", self.encoding.clone()))
                    .child(self.field(th, "Locale", self.locale.clone()))
                    .into_any_element(),
            ));
        }
        self.rows = rows.iter().map(|(r, _)| *r).collect();

        let viewport = window.viewport_size();
        let max_h = (f32::from(viewport.height) - 48.0).max(240.0);
        let body = div()
            .id("editor-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(12.0))
            .pr(px(4.0))
            .children(rows.into_iter().map(|(_, e)| e));

        let title = if self.editing {
            "编辑会话"
        } else {
            "新建会话"
        };
        let card = div()
            .id("session-editor-card")
            .w(px(560.0))
            .max_w_full()
            .max_h(px(max_h))
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(th.c("border.strong"))
            .bg(th.c("bg.dialog"))
            .text_color(th.c("text.primary"))
            .text_size(px(number("font", "ui.body")))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000066).into(),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(24.0),
                spread_radius: px(0.0),
            }])
            // 卡片内的点击不能冒泡到遮罩。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(number("font", "ui.title")))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(body)
            .children(self.error.clone().map(|e| {
                div()
                    .text_size(px(12.0))
                    .text_color(th.c("status.error"))
                    .child(e)
            }))
            .child(
                div()
                    .mt(px(4.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.0))
                    .child(self.render_button(Btn::Cancel, "取消", false, window, th, cx))
                    .child(self.render_button(Btn::Save, "保存", false, window, th, cx))
                    .child(self.render_button(Btn::Connect, "保存并连接", true, window, th, cx)),
            );

        div()
            .id("session-editor")
            .key_context("SessionEditor")
            .track_focus(&self.root)
            .capture_key_down(cx.listener(Self::on_capture_key))
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(th.c("bg.scrim"))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(card)
    }
}

impl SessionEditorView {
    /// 明文用完即清：宿主丢弃实体前调用，把密码类输入框清空。
    pub(crate) fn wipe_secrets(&mut self, cx: &mut Context<Self>) {
        for input in [&self.password, &self.key_passphrase] {
            input.update(cx, |t, cx| t.set_text("", cx));
        }
    }
}
