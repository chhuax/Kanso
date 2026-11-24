//! 会话编辑对话框（ui-shell.md §5）：新建与编辑已保存的会话。
//!
//! 字段与显隐规则在库里的 `app::sessions::SessionEditor`（三平台可测）：按类型显示本地 / SSH 字段、
//! 认证方式决定显示密码还是私钥、跳板机与 ProxyCommand 互斥。校验以核心为准，这里只把错误原样显示。
//! 密码与私钥口令只在内存里，保存成功后清空，写入走 `kanso-secret`（经 `Core::set_credential`）。
//!
//! 键盘：Tab / Shift+Tab 只在对话框内循环（字段 → 按钮 → 回到第一个）；回车 = 保存；Esc = 取消。
//! 点遮罩不关闭（与 mac sheet 一致）。色板只支持鼠标点选。

use kanso_gpui::app::i18n::label;
use kanso_types::message::Message;
use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{
    App, BoxShadow, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, PathPromptOptions, ScrollHandle, Subscription, Window, div, point,
    prelude::*, px, rgb, rgba,
};
use kanso_core::Core;
use kanso_gpui::app::launcher::{
    auth_label, editor_error_text, kind_label, render_message, toggle_color,
};
use kanso_gpui::app::sessions::{
    EditorField, EditorOptions, EncodingOption, SaveOutcome, SessionEditor,
};
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::{Appearance, color, number};
use kanso_types::ProfileId;
use kanso_types::mcp::McpAccess;
use kanso_types::profile::{AuthMethod, Group, LocalProfile, Profile, ProfileKind, SessionKind};

use super::log;
use super::settings::{MCP_ACCESS_LEVELS, mcp_access_title};
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
    /// 会话列表发生变化，授权或凭据部分保存失败时也会发出。
    LibraryChanged,
    /// 打开临时本地 Shell。
    OpenLocal(LocalProfile),
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
    KeepaliveInterval,
    KeepaliveMaxMisses,
    IdleInterval,
    IdleString,
}

/// 一个下拉字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Kind,
    Shell,
    Group,
    McpAccess,
    Auth,
    Jump,
    Encoding,
    KeepaliveEnabled,
    IdleEnabled,
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
    Group,
    McpAccess,
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
    Keepalive,
    Idle,
}

fn row_of(stop: Stop) -> Row {
    match stop {
        Stop::Pick(Pick::Kind) => Row::Kind,
        Stop::Text(Text::Name) => Row::Name,
        Stop::Pick(Pick::Group) => Row::Group,
        Stop::Pick(Pick::McpAccess) => Row::McpAccess,
        Stop::Text(Text::Shell) | Stop::Pick(Pick::Shell) => Row::Shell,
        Stop::Text(Text::StartDir) => Row::StartDir,
        Stop::Text(Text::Host | Text::Port) => Row::HostPort,
        Stop::Text(Text::User) | Stop::Pick(Pick::Auth) => Row::UserAuth,
        Stop::Text(Text::Password) => Row::Password,
        Stop::Text(Text::KeyPath) => Row::KeyPath,
        Stop::Text(Text::KeyPassphrase) => Row::KeyPassphrase,
        Stop::Pick(Pick::Jump) => Row::Jump,
        Stop::Text(Text::Proxy) => Row::Proxy,
        Stop::Pick(Pick::Encoding) | Stop::Text(Text::Locale) => Row::EncodingLocale,
        Stop::Pick(Pick::KeepaliveEnabled)
        | Stop::Text(Text::KeepaliveInterval | Text::KeepaliveMaxMisses) => Row::Keepalive,
        Stop::Pick(Pick::IdleEnabled) | Stop::Text(Text::IdleInterval | Text::IdleString) => {
            Row::Idle
        }
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
    custom_local: bool,
    core: Arc<Core>,
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
    keepalive_interval: Entity<TextInput>,
    keepalive_max_misses: Entity<TextInput>,
    idle_interval: Entity<TextInput>,
    idle_string: Entity<TextInput>,
    kind: FocusHandle,
    shell_pick: Entity<Dropdown>,
    shell_commands: Vec<String>,
    group: Entity<Dropdown>,
    mcp_access: Entity<Dropdown>,
    show_mcp_sensitive_notice: bool,
    auth: Entity<Dropdown>,
    jump: Entity<Dropdown>,
    encoding: Entity<Dropdown>,
    keepalive_enabled: Entity<Dropdown>,
    idle_enabled: Entity<Dropdown>,

    buttons: [FocusHandle; 3],
    root: FocusHandle,
    scroll: ScrollHandle,
    /// 上一帧画出的行（按 Tab 聚焦字段时据此滚动到可见）。
    rows: Vec<(Row, Rc<Cell<gpui::Bounds<gpui::Pixels>>>)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for SessionEditorView {}

impl Focusable for SessionEditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root.clone()
    }
}

fn appearance(window: &Window) -> Appearance {
    super::display::window_appearance(window)
}

const ALL_FIELDS: [EditorField; 24] = [
    EditorField::Name,
    EditorField::Kind,
    EditorField::Color,
    EditorField::Group,
    EditorField::McpAccess,
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
    EditorField::KeepaliveEnabled,
    EditorField::KeepaliveInterval,
    EditorField::KeepaliveMaxMisses,
    EditorField::IdleEnabled,
    EditorField::IdleInterval,
    EditorField::IdleString,
];

impl SessionEditorView {
    /// 新建（`editing` 为空）或编辑已保存的会话。必须在窗口上下文里创建。
    pub(crate) fn new(
        core: Arc<Core>,
        editing: Option<Profile>,
        custom_local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let options = EditorOptions {
            supports_mcp: kanso_mcp_server::supported(),
            ..EditorOptions::default()
        };
        let mut model = match editing {
            Some(p) => SessionEditor::editing(core.clone(), options, p),
            None => SessionEditor::new(core.clone(), options),
        };
        let is_editing = model.is_editing();
        if custom_local {
            let defaults = core.local_shell_defaults();
            model.kind = SessionKind::Local;
            model.shell_command = defaults.shell_command;
            model.start_dir = defaults.start_dir.unwrap_or_default();
            model.encoding = defaults.encoding.as_str().to_owned();
            model.locale = defaults
                .locale
                .map(|locale| locale.as_str().to_owned())
                .unwrap_or_default();
        } else if !is_editing {
            model.kind = SessionKind::Ssh;
        }
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
                    TextInputEvent::Submit(_) => this.activate(
                        if this.custom_local {
                            Btn::Connect
                        } else {
                            Btn::Save
                        },
                        window,
                        cx,
                    ),
                    TextInputEvent::Escape => this.cancel(cx),
                    _ => {}
                },
            ));
            input
        };
        let keep = if is_editing {
            render_message(&Message::UiKeepSaved {})
        } else {
            String::new()
        };
        let name = text(Text::Name, &model.name, "", false, window, cx);
        let shell = text(
            Text::Shell,
            &model.shell_command,
            if cfg!(windows) {
                "cmd.exe"
            } else {
                "/bin/bash"
            },
            false,
            window,
            cx,
        );
        let start_dir = text(Text::StartDir, &model.start_dir, "~", false, window, cx);
        let host = text(Text::Host, &model.host, "example.com", false, window, cx);
        let port = text(Text::Port, &model.port_text, "22", false, window, cx);
        let user = text(Text::User, &model.user, "user", false, window, cx);
        let password = text(Text::Password, "", &keep, true, window, cx);
        let key_path = text(
            Text::KeyPath,
            &model.key_path,
            "~/.ssh/id_ed25519",
            false,
            window,
            cx,
        );
        let key_passphrase = text(Text::KeyPassphrase, "", &keep, true, window, cx);
        let proxy = text(
            Text::Proxy,
            &model.proxy_command,
            "ssh -W %h:%p gateway",
            false,
            window,
            cx,
        );
        let locale = text(
            Text::Locale,
            &model.locale,
            &render_message(&Message::UiAutomatic {}),
            false,
            window,
            cx,
        );
        let keepalive_interval = text(
            Text::KeepaliveInterval,
            &model.keepalive_interval_text,
            "30",
            false,
            window,
            cx,
        );
        let keepalive_max_misses = text(
            Text::KeepaliveMaxMisses,
            &model.keepalive_max_misses_text,
            "3",
            false,
            window,
            cx,
        );
        let idle_interval = text(
            Text::IdleInterval,
            &model.idle_interval_text,
            "3600",
            false,
            window,
            cx,
        );
        let idle_string = text(
            Text::IdleString,
            &model.idle_string,
            "\\r",
            false,
            window,
            cx,
        );

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
        let kind = cx.focus_handle().tab_stop(true);
        #[cfg(windows)]
        let (shell_commands, mut shell_items) = {
            // 系统自带 Shell 始终可选；PowerShell 7 只在安装路径里存在时显示。
            let mut shell_commands = vec![String::new(), "cmd.exe".into(), "powershell.exe".into()];
            let mut shell_items = vec![
                Item::option(render_message(&Message::UiShellDefault {})),
                Item::option("CMD"),
                Item::option("Windows PowerShell"),
            ];
            let mut paths: Vec<_> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default();
            if let Some(dir) = std::env::var_os("ProgramFiles") {
                paths.push(std::path::PathBuf::from(dir).join("PowerShell/7"));
            }
            if let Some(path) = paths
                .into_iter()
                .map(|p| p.join("pwsh.exe"))
                .find(|p| p.is_file())
            {
                shell_items.push(Item::option("PowerShell 7"));
                shell_commands.push(format!("\"{}\"", path.display()));
            }
            (shell_commands, shell_items)
        };
        #[cfg(target_os = "linux")]
        let (shell_commands, mut shell_items) = {
            let mut commands = vec![String::new()];
            let mut items = vec![Item::option(render_message(&Message::UiShellDefault {}))];
            for (path, label) in [
                ("/bin/bash", "Bash"),
                ("/bin/zsh", "Zsh"),
                ("/usr/bin/fish", "Fish"),
                ("/bin/sh", "sh"),
            ] {
                if std::path::Path::new(path).is_file() {
                    commands.push(path.into());
                    items.push(Item::option(label));
                }
            }
            (commands, items)
        };
        let shell_selected = shell_commands
            .iter()
            .position(|s| {
                if cfg!(windows) {
                    s.eq_ignore_ascii_case(model.shell_command.trim())
                } else {
                    s == model.shell_command.trim()
                }
            })
            .unwrap_or(shell_commands.len());
        shell_items.push(Item::option(render_message(&Message::UiShellCustom {})));
        let shell_pick = dropdown(Pick::Shell, shell_items, shell_selected, window, cx);
        let group = dropdown(
            Pick::Group,
            std::iter::once(Item::option(label(Message::UiNoGroup {})))
                .chain(groups.iter().map(|g| Item::option(g.name.clone())))
                .collect(),
            model
                .group
                .and_then(|id| groups.iter().position(|g| g.id == id))
                .map_or(0, |i| i + 1),
            window,
            cx,
        );
        let mcp_access = dropdown(
            Pick::McpAccess,
            MCP_ACCESS_LEVELS
                .iter()
                .map(|&level| Item::option(mcp_access_title(level)))
                .collect(),
            MCP_ACCESS_LEVELS
                .iter()
                .position(|&level| level == model.mcp_access())
                .expect("MCP 访问级别必须有对应的选项"),
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
            std::iter::once(Item::option(label(Message::UiNone {})))
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
        let on_off = || {
            vec![
                Item::option(label(Message::UiOn {})),
                Item::option(label(Message::UiOff {})),
            ]
        };
        let keepalive_enabled = dropdown(
            Pick::KeepaliveEnabled,
            on_off(),
            usize::from(!model.keepalive_enabled),
            window,
            cx,
        );
        let idle_enabled = dropdown(
            Pick::IdleEnabled,
            on_off(),
            usize::from(!model.idle_enabled),
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
            custom_local,
            core,
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
            keepalive_interval,
            keepalive_max_misses,
            idle_interval,
            idle_string,
            kind,
            shell_pick,
            shell_commands,
            group,
            mcp_access,
            show_mcp_sensitive_notice: false,
            auth,
            jump,
            encoding,
            keepalive_enabled,
            idle_enabled,
            buttons: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            root: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            rows: Vec::new(),
            _subs: subs,
        };
        view.refresh_visible();
        view.sync_exclusive(cx);
        if custom_local {
            view.shell.read(cx).focus(window);
        } else {
            view.name.read(cx).focus(window);
        }
        // 首帧渲染之后再聚焦一次，保证焦点节点已进入调度树。
        cx.defer_in(window, |this, window, cx| {
            if this.custom_local {
                this.shell.read(cx).focus(window);
            } else {
                this.name.read(cx).focus(window);
            }
        });
        view
    }

    fn refresh_visible(&mut self) {
        if let Some(model) = &self.model {
            self.visible = Self::visible_fields(model, self.custom_local);
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
            Text::Shell => {
                let selected = self
                    .shell_commands
                    .iter()
                    .position(|s| s.eq_ignore_ascii_case(v.trim()))
                    .unwrap_or(self.shell_commands.len());
                m.shell_command = v;
                self.shell_pick
                    .update(cx, |d, cx| d.set_selected(Some(selected), cx));
            }
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
            Text::KeepaliveInterval | Text::KeepaliveMaxMisses | Text::IdleInterval => {
                let clean = kanso_gpui::app::sessions::sanitized_port_text(&v);
                if clean != v {
                    // 保活间隔只接受数字：过滤后写回输入框（再触发一次 Change，到时 clean == v）。
                    match field {
                        Text::KeepaliveInterval => self
                            .keepalive_interval
                            .update(cx, |t, cx| t.set_text(&clean, cx)),
                        Text::KeepaliveMaxMisses => self
                            .keepalive_max_misses
                            .update(cx, |t, cx| t.set_text(&clean, cx)),
                        Text::IdleInterval => self
                            .idle_interval
                            .update(cx, |t, cx| t.set_text(&clean, cx)),
                        _ => unreachable!(),
                    }
                    return;
                }
                match field {
                    Text::KeepaliveInterval => m.keepalive_interval_text = v,
                    Text::KeepaliveMaxMisses => m.keepalive_max_misses_text = v,
                    Text::IdleInterval => m.idle_interval_text = v,
                    _ => unreachable!(),
                }
            }
            Text::IdleString => m.idle_string = v,
        }
        if field == Text::Proxy {
            self.sync_exclusive(cx);
        }
    }

    fn on_pick(&mut self, pick: Pick, index: usize, cx: &mut Context<Self>) {
        let Some(m) = self.model.as_mut() else { return };
        match pick {
            Pick::Kind => m.kind = KINDS[index.clamp(usize::from(!self.editing), KINDS.len() - 1)],
            Pick::Shell => {
                if let Some(command) = self.shell_commands.get(index) {
                    m.shell_command = command.clone();
                    self.shell.update(cx, |t, cx| t.set_text(command, cx));
                }
            }
            Pick::Group => {
                m.group = index
                    .checked_sub(1)
                    .and_then(|i| self.groups.get(i))
                    .map(|g| g.id)
            }
            Pick::McpAccess => {
                let next = MCP_ACCESS_LEVELS[index];
                if m.mcp_access() == McpAccess::Off && next != McpAccess::Off {
                    self.show_mcp_sensitive_notice = true;
                }
                m.edit_mcp_access(next);
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
            Pick::KeepaliveEnabled => m.keepalive_enabled = index == 0,
            Pick::IdleEnabled => m.idle_enabled = index == 0,
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
            prompt: Some(label(Message::UiChoosePrivateKeyFile {}).into()),
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
        if self.custom_local {
            let model = self.model.as_ref().expect("本地 Shell 表单存在");
            match model.assemble() {
                Ok(profile) => {
                    let ProfileKind::Local(local) = profile.kind else {
                        unreachable!("本地 Shell 配置")
                    };
                    if connect {
                        cx.emit(EditorEvent::OpenLocal(local));
                    } else {
                        self.error = self
                            .core
                            .set_local_shell_defaults(&local)
                            .err()
                            .map(|error| kanso_gpui::app::launcher::core_error_text(&error))
                            .or_else(|| Some(render_message(&Message::UiShellDefaultsSaved {})));
                    }
                }
                Err(error) => self.error = Some(editor_error_text(&error, render_message)),
            }
            cx.notify();
            return;
        }
        let Some(mut model) = self.model.take() else {
            return;
        };
        self.refresh_visible_from(&model);
        self.saving = true;
        self.mcp_access.update(cx, |d, cx| d.set_disabled(true, cx));
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = model.save().await;
            this.update_in(cx, |this, _window, cx| {
                let id = model.profile_id();
                this.error = model.error().map(|e| editor_error_text(e, render_message));
                this.model = Some(model);
                this.saving = false;
                this.mcp_access
                    .update(cx, |d, cx| d.set_disabled(false, cx));
                if log_enabled() {
                    log(&format!("session-editor save -> {outcome:?} id={id}"));
                }
                if outcome != SaveOutcome::Failed {
                    cx.emit(EditorEvent::LibraryChanged);
                }
                match outcome {
                    SaveOutcome::Saved => cx.emit(EditorEvent::Saved { id, connect }),
                    SaveOutcome::SavedButMcpAccessFailed => this.wipe_secrets(cx),
                    SaveOutcome::Failed | SaveOutcome::SavedButCredentialFailed => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_visible_from(&mut self, model: &SessionEditor<Arc<Core>>) {
        self.visible = Self::visible_fields(model, self.custom_local);
    }

    fn visible_fields(model: &SessionEditor<Arc<Core>>, custom_local: bool) -> Vec<EditorField> {
        ALL_FIELDS
            .iter()
            .copied()
            .filter(|f| {
                model.is_visible(*f)
                    && (!custom_local
                        || matches!(
                            f,
                            EditorField::ShellCommand
                                | EditorField::StartDir
                                | EditorField::Encoding
                                | EditorField::Locale
                        ))
            })
            .collect()
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
            Text::KeepaliveInterval => &self.keepalive_interval,
            Text::KeepaliveMaxMisses => &self.keepalive_max_misses,
            Text::IdleInterval => &self.idle_interval,
            Text::IdleString => &self.idle_string,
        }
    }

    fn pick_entity(&self, p: Pick) -> &Entity<Dropdown> {
        match p {
            Pick::Kind => unreachable!("类型使用分段卡片，不是下拉框"),
            Pick::Shell => &self.shell_pick,
            Pick::Group => &self.group,
            Pick::McpAccess => &self.mcp_access,
            Pick::Auth => &self.auth,
            Pick::Jump => &self.jump,
            Pick::Encoding => &self.encoding,
            Pick::KeepaliveEnabled => &self.keepalive_enabled,
            Pick::IdleEnabled => &self.idle_enabled,
        }
    }

    /// 当前可聚焦的字段，按画面顺序。
    fn stops(&self) -> Vec<Stop> {
        use EditorField as F;
        let mut v = vec![
            (F::Kind, Stop::Pick(Pick::Kind)),
            (F::Name, Stop::Text(Text::Name)),
            (F::Group, Stop::Pick(Pick::Group)),
            (F::McpAccess, Stop::Pick(Pick::McpAccess)),
            (F::ShellCommand, Stop::Pick(Pick::Shell)),
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
            (F::KeepaliveEnabled, Stop::Pick(Pick::KeepaliveEnabled)),
            (F::KeepaliveInterval, Stop::Text(Text::KeepaliveInterval)),
            (F::KeepaliveMaxMisses, Stop::Text(Text::KeepaliveMaxMisses)),
            (F::IdleEnabled, Stop::Pick(Pick::IdleEnabled)),
            (F::IdleInterval, Stop::Text(Text::IdleInterval)),
            (F::IdleString, Stop::Text(Text::IdleString)),
        ];
        v.retain(|(f, _)| self.vis(*f));
        v.into_iter().map(|(_, s)| s).collect()
    }

    fn stop_handle(&self, s: Stop, cx: &App) -> FocusHandle {
        match s {
            Stop::Text(t) => self.text_entity(t).read(cx).focus_handle(cx),
            Stop::Pick(Pick::Kind) => self.kind.clone(),
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
            Pick::Shell,
            Pick::Group,
            Pick::McpAccess,
            Pick::Auth,
            Pick::Jump,
            Pick::Encoding,
            Pick::KeepaliveEnabled,
            Pick::IdleEnabled,
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
            && let Some((_, bounds)) = self.rows.iter().find(|(r, _)| *r == row_of(stop))
        {
            let row = bounds.get();
            let viewport = self.scroll.bounds();
            let dy = if row.top() < viewport.top() {
                viewport.top() - row.top()
            } else if row.bottom() > viewport.bottom() {
                viewport.bottom() - row.bottom()
            } else {
                px(0.0)
            };
            let mut offset = self.scroll.offset();
            offset.y += dy;
            self.scroll.set_offset(offset);
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
        if self.kind.is_focused(window)
            && matches!(
                ev.keystroke.key.as_str(),
                "left" | "right" | "up" | "down" | "space"
            )
        {
            let current = self
                .model
                .as_ref()
                .map_or(0, |m| KINDS.iter().position(|k| *k == m.kind).unwrap_or(0));
            let delta = if matches!(ev.keystroke.key.as_str(), "left" | "up") {
                2
            } else {
                1
            };
            let next = if self.editing {
                (current + delta) % KINDS.len()
            } else if current == 1 {
                2
            } else {
                1
            };
            self.on_pick(Pick::Kind, next, cx);
            cx.stop_propagation();
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

    fn label(&self, th: Theme, text: &str) -> gpui::Div {
        div()
            .text_size(px(number("font", "ui.caption")))
            .text_color(th.c("text.secondary"))
            .child(text.to_owned())
    }

    fn field(&self, th: Theme, label: &str, control: impl IntoElement) -> gpui::Div {
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
        let mut grid = div()
            .w(px(224.0))
            .h(px(56.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.0))
            .p(px(3.0));
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
        label: String,
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
            .min_w(px(64.0))
            .h(px(30.0))
            .px(px(12.0))
            .gap(px(6.0))
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
            .when(btn != Btn::Cancel, |d| {
                d.child(super::widgets::icons::icon(
                    if btn == Btn::Save { "save" } else { "plug" },
                    14.0,
                    fg,
                ))
            })
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

        use super::widgets::{card::card, icons::icon};
        let tr = |m: Message| render_message(&m);
        let selected = self.model.as_ref().map(|m| m.kind).unwrap_or(KINDS[0]);
        let mut kinds = div()
            .id("session-kinds")
            .track_focus(&self.kind)
            .flex()
            .gap(px(8.0));
        for (i, kind) in KINDS
            .iter()
            .copied()
            .enumerate()
            .skip(usize::from(!self.editing))
        {
            let on = selected == kind;
            let symbol = ["square-terminal", "server", "folder"][i];
            kinds = kinds.child(
                div()
                    .id(("session-kind", i))
                    .w(px(112.0))
                    .h(px(52.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(3.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(th.c(if on {
                        "border.segmentSelected"
                    } else {
                        "border.subtle"
                    }))
                    .bg(th.c(if on { "bg.segmentSelected" } else { "bg.field" }))
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(th.c(if on { "text.primary" } else { "text.secondary" }))
                    .when(on && self.kind.is_focused(window), |d| {
                        d.border_color(th.c("accent"))
                    })
                    .cursor_pointer()
                    .hover(|s| s.border_color(th.c("accent")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if !this.saving {
                                window.focus(&this.kind);
                                this.on_pick(Pick::Kind, i, cx);
                            }
                        }),
                    )
                    .child(icon(
                        symbol,
                        18.0,
                        th.c(if on { "accent" } else { "text.secondary" }),
                    ))
                    .child(kind_label(kind)),
            );
        }
        let pair = |a: gpui::Div, b: gpui::Div| div().flex().gap(px(12.0)).child(a).child(b);
        let mut rows = vec![
            (
                Row::Kind,
                self.field(th, &tr(Message::UiType {}), kinds)
                    .into_any_element(),
            ),
            (
                Row::Name,
                pair(
                    self.field(th, &tr(Message::UiName {}), self.name.clone()),
                    self.field(th, &tr(Message::UiColor {}), self.render_palette(th, cx)),
                )
                .into_any_element(),
            ),
            (
                Row::Group,
                self.field(th, &tr(Message::UiGroup {}), self.group.clone())
                    .into_any_element(),
            ),
        ];
        if self.vis(F::McpAccess) {
            rows.push((
                Row::McpAccess,
                self.field(
                    th,
                    &tr(Message::WinMcpAccess {}),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(self.mcp_access.clone())
                        .when(self.show_mcp_sensitive_notice, |d| {
                            d.child(
                                div()
                                    .text_size(px(number("font", "ui.caption")))
                                    .text_color(th.c("text.tertiary"))
                                    .child(tr(Message::WinMcpSensitiveNotice {})),
                            )
                        }),
                )
                .into_any_element(),
            ));
        }
        if self.custom_local {
            rows.clear();
        }
        let session_len = rows.len();
        if self.vis(F::ShellCommand) {
            rows.push((
                Row::Shell,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(self.field(th, &tr(Message::UiLocalShell {}), self.shell_pick.clone()))
                    .child(self.field(th, &tr(Message::UiShellCommand {}), self.shell.clone()))
                    .into_any_element(),
            ));
            rows.push((
                Row::StartDir,
                self.field(
                    th,
                    &tr(Message::UiStartDirectory {}),
                    self.start_dir.clone(),
                )
                .into_any_element(),
            ));
        }
        if self.vis(F::Host) {
            rows.push((
                Row::HostPort,
                pair(
                    self.field(th, &tr(Message::UiHost {}), self.host.clone()),
                    self.field(th, &tr(Message::UiPort {}), self.port.clone()),
                )
                .into_any_element(),
            ));
            rows.push((
                Row::UserAuth,
                pair(
                    self.field(th, &tr(Message::UiUser {}), self.user.clone()),
                    self.field(th, &tr(Message::UiAuthentication {}), self.auth.clone()),
                )
                .into_any_element(),
            ));
        }
        if self.vis(F::Password) {
            rows.push((
                Row::Password,
                self.field(th, &tr(Message::UiPassword {}), self.password.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::KeyPath) {
            let browse = div()
                .id("browse-key")
                .flex_none()
                .h(px(30.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .rounded(px(6.0))
                .border_1()
                .border_color(th.c("border.strong"))
                .bg(th.c("bg.buttonSecondary"))
                .cursor_pointer()
                .hover(|s| s.opacity(0.88))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.browse_key(window, cx)),
                )
                .child(tr(Message::UiChoose {}));
            rows.push((
                Row::KeyPath,
                self.field(
                    th,
                    &tr(Message::UiPrivateKey {}),
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(self.key_path.clone()))
                        .child(browse),
                )
                .into_any_element(),
            ));
            rows.push((
                Row::KeyPassphrase,
                self.field(
                    th,
                    &tr(Message::UiKeyPassphrase {}),
                    self.key_passphrase.clone(),
                )
                .into_any_element(),
            ));
        }
        if self.vis(F::Jump) {
            rows.push((
                Row::Jump,
                self.field(th, &tr(Message::UiJumpHost {}), self.jump.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::ProxyCommand) {
            rows.push((
                Row::Proxy,
                self.field(th, &tr(Message::UiProxyCommand {}), self.proxy.clone())
                    .into_any_element(),
            ));
        }
        if self.vis(F::Encoding) {
            rows.push((
                Row::EncodingLocale,
                pair(
                    self.field(th, &tr(Message::UiEncoding {}), self.encoding.clone()),
                    self.field(th, &tr(Message::UiLocale {}), self.locale.clone()),
                )
                .into_any_element(),
            ));
        }
        if self.vis(F::KeepaliveEnabled) {
            rows.push((
                Row::Keepalive,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(self.field(
                        th,
                        &tr(Message::UiKeepaliveEnabled {}),
                        self.keepalive_enabled.clone(),
                    ))
                    .child(pair(
                        self.field(
                            th,
                            &tr(Message::UiKeepaliveInterval {}),
                            self.keepalive_interval.clone(),
                        ),
                        self.field(
                            th,
                            &tr(Message::UiKeepaliveMaxMisses {}),
                            self.keepalive_max_misses.clone(),
                        ),
                    ))
                    .into_any_element(),
            ));
        }
        if self.vis(F::IdleEnabled) {
            rows.push((
                Row::Idle,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(self.field(
                        th,
                        &tr(Message::UiIdleKeepalive {}),
                        self.idle_enabled.clone(),
                    ))
                    .child(pair(
                        self.field(
                            th,
                            &tr(Message::UiIdleInterval {}),
                            self.idle_interval.clone(),
                        ),
                        self.field(th, &tr(Message::UiIdleString {}), self.idle_string.clone()),
                    ))
                    .into_any_element(),
            ));
        }
        self.rows.clear();
        let mut elements = Vec::new();
        for (row, content) in rows {
            let bounds = Rc::new(Cell::new(gpui::Bounds::default()));
            self.rows.push((row, bounds.clone()));
            elements.push(
                div()
                    .relative()
                    .flex_none()
                    .w_full()
                    .child(content)
                    .child(
                        gpui::canvas(move |rect, _, _| bounds.set(rect), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .into_any_element(),
            );
        }
        let connection_rows = elements.split_off(session_len);
        let viewport = window.viewport_size();
        let max_h = (f32::from(viewport.height) - 48.0).clamp(240.0, 680.0);
        let body = div()
            .id("editor-body")
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .gap(px(16.0))
            .p(px(20.0))
            .when(!self.custom_local, |body| {
                body.child(card(th, tr(Message::UiSession {}), "tag", elements))
            })
            .child(card(
                th,
                tr(if self.vis(F::Host) {
                    Message::UiSshConnection {}
                } else {
                    Message::UiLocalSession {}
                }),
                if self.vis(F::Host) {
                    "server"
                } else {
                    "square-terminal"
                },
                connection_rows,
            ));
        let title = tr(if self.custom_local {
            Message::UiCustomizeShell {}
        } else if self.editing {
            Message::UiEditSession {}
        } else {
            Message::UiNewSession {}
        });
        let card = div()
            .id("session-editor-card")
            .w(px(560.0))
            .max_w_full()
            .max_h(px(max_h))
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .overflow_hidden()
            .border_1()
            .border_color(th.c("border.subtle"))
            .bg(th.c("bg.dialog"))
            .text_color(th.c("text.primary"))
            .text_size(px(number("font", "ui.body")))
            .shadow(vec![BoxShadow {
                color: rgba(0x00000066).into(),
                offset: point(px(0.0), px(8.0)),
                blur_radius: px(24.0),
                spread_radius: px(0.0),
            }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .py(px(14.0))
                    .border_b_1()
                    .border_color(th.c("border.subtle"))
                    .child(
                        div()
                            .size(px(32.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.0))
                            .bg(th.c("bg.segmentSelected"))
                            .border_1()
                            .border_color(th.c("border.segmentSelected"))
                            .child(icon("server", 16.0, th.c("accent"))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .id("editor-close")
                            .size(px(26.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(th.c("bg.rowHover")))
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
                            .child(icon("x", 14.0, th.c("text.secondary"))),
                    ),
            )
            .child(body)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(20.0))
                    .py(px(14.0))
                    .border_t_1()
                    .border_color(th.c("border.subtle"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .text_color(th.c("status.error"))
                            .children(self.error.clone()),
                    )
                    .child(self.render_button(
                        Btn::Cancel,
                        tr(Message::UiCancel {}),
                        false,
                        window,
                        th,
                        cx,
                    ))
                    .child(self.render_button(
                        Btn::Save,
                        tr(if self.custom_local {
                            Message::UiSetShellDefault {}
                        } else {
                            Message::UiSave {}
                        }),
                        false,
                        window,
                        th,
                        cx,
                    ))
                    .child(self.render_button(
                        Btn::Connect,
                        tr(if self.custom_local {
                            Message::UiOpenShell {}
                        } else {
                            Message::UiConnect {}
                        }),
                        true,
                        window,
                        th,
                        cx,
                    )),
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
