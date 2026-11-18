//! 设置窗口与设置模型（`ui-shell.md` §10 显示设置）。
//!
//! - [`SettingsModel`]：持有 [`SettingsStore`]（纯逻辑，写核心的设置表）的实体，任何改动都会发出
//!   [`SettingsEvent::Changed`]；外壳订阅它来即时刷新终端与界面，设置窗口订阅它来刷新控件。
//! - [`SettingsView`]：独立的设置窗口。分组卡片：外观、终端字体、光标、回滚、对比度、界面字体。
//!   外观与分组的结构照 mac 版 `DisplaySettingsViewController`；改动立即生效并持久化。
//!
//! 字体：终端字体下拉只列等宽字体（用步进宽度探测，后台线程完成，列表稍后补齐），界面字体下拉列全部；
//! 两者旁边都有文本框，可手动输入任意字体名（回车或失焦生效），方便私有构建的 Nerd Font。

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, rgba, size,
};
use kanso_core::Core;
use kanso_gpui::app::i18n::{Lang, LanguageSetting, label, lang};
use kanso_gpui::app::keybindings::{self, KeybindingSettings};
use kanso_gpui::app::settings::NumericSetting;
use kanso_gpui::app::settings::{
    AppearanceSettings, BUILTIN_THEMES, CursorShapeSetting, DisplaySettings, Section,
    SettingsStore, ThemeMode, TimelineSettings, clean_font_names, is_monospace, pick_nerd_fallback,
};
use kanso_gpui::widgets::dropdown::Item;
use kanso_gpui::widgets::style::{Appearance, color, number};

use super::display;
use super::log;
use super::mcp::McpModel;
use super::widgets::dropdown::{Dropdown, DropdownEvent};
use super::widgets::icons::icon;
use super::widgets::text_input::{TextInput, TextInputEvent};
use kanso_gpui::app::launcher::render_message;
use kanso_types::message::Message;

#[path = "settings_keys.rs"]
mod keys;
#[path = "settings_mcp.rs"]
mod mcp_page;

pub(super) use mcp_page::{MCP_ACCESS_LEVELS, mcp_access_title};

const MCP_PAGE: usize = 3;

// ---- 模型 ----

/// 设置变化事件。
pub(crate) enum SettingsEvent {
    /// 某个分区变了。
    Changed(Section),
}

/// 设置模型实体。
pub(crate) struct SettingsModel {
    store: SettingsStore<Arc<Core>>,
    themes_dir: std::path::PathBuf,
    themes: Vec<kanso_core::terminal_themes::TerminalTheme>,
}

impl EventEmitter<SettingsEvent> for SettingsModel {}

impl SettingsModel {
    /// 从核心的设置表载入，并把终端选项下发给核心。
    pub(crate) fn new(core: Arc<Core>, themes_dir: std::path::PathBuf) -> Self {
        let store = SettingsStore::load(core);
        // 启动时就把用户的快捷键覆盖装进生效表。
        keybindings::install(store.keybindings());
        SettingsModel {
            store,
            themes: kanso_core::terminal_themes::load_terminal_themes(&themes_dir),
            themes_dir,
        }
    }

    pub(crate) fn keybindings(&self) -> &KeybindingSettings {
        self.store.keybindings()
    }

    /// 修改快捷键覆盖；变化时重新安装生效表（菜单提示与外壳查表立即更新）。
    pub(crate) fn update_keybindings(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut KeybindingSettings),
    ) {
        if let Some(s) = self.store.update_keybindings(change) {
            keybindings::install(self.store.keybindings());
            cx.emit(SettingsEvent::Changed(s));
            cx.notify();
        }
    }

    /// 恢复默认设置（调用方负责确认）：清空设置表并回到默认值，每个分区各通知一次。
    pub(crate) fn reset_to_defaults(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        self.store.reset_to_defaults()?;
        keybindings::install(self.store.keybindings());
        for section in Section::ALL {
            cx.emit(SettingsEvent::Changed(section));
        }
        cx.notify();
        Ok(())
    }

    pub(crate) fn display(&self) -> &DisplaySettings {
        self.store.display()
    }

    pub(crate) fn appearance(&self) -> &AppearanceSettings {
        self.store.appearance()
    }

    fn reload_themes(&mut self, cx: &mut Context<Self>) {
        let themes = kanso_core::terminal_themes::load_terminal_themes(&self.themes_dir);
        if self.themes != themes {
            self.themes = themes;
            cx.emit(SettingsEvent::Changed(Section::Appearance));
            cx.notify();
        }
    }

    fn theme_choices(&self) -> Vec<(String, String)> {
        BUILTIN_THEMES
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .chain(self.themes.iter().map(|t| (t.id.clone(), t.name.clone())))
            .collect()
    }

    pub(super) fn terminal_palette(&self, dark: bool) -> kanso_gpui::theme::Palette {
        self.appearance().terminal_palette(dark, &self.themes)
    }

    pub(crate) fn timeline(&self) -> &TimelineSettings {
        self.store.timeline()
    }

    /// 修改显示设置。
    pub(crate) fn update_display(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut DisplaySettings),
    ) {
        if let Some(s) = self.store.update_display(change) {
            cx.emit(SettingsEvent::Changed(s));
            cx.notify();
        }
    }

    /// 修改外观设置。
    pub(crate) fn update_appearance(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut AppearanceSettings),
    ) {
        if let Some(s) = self.store.update_appearance(change) {
            cx.emit(SettingsEvent::Changed(s));
            cx.notify();
        }
    }

    /// 修改时间轴设置。
    pub(crate) fn update_timeline(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut TimelineSettings),
    ) {
        if let Some(s) = self.store.update_timeline(change) {
            cx.emit(SettingsEvent::Changed(s));
            cx.notify();
        }
    }

    /// 把当前设置写进 GPUI 一侧的生效状态（字体 / 字号 / 行距 / 强制外观）。
    pub(crate) fn push_to_display(&self, cx: &App) {
        display::apply(self.display(), self.appearance().mode, nerd_fallback(cx));
        display::apply_language(self.appearance().language);
    }
}

/// 已安装的 Nerd Font（图标回退用）；只在第一次探测，字体列表在运行期不变。
fn nerd_fallback(cx: &App) -> Option<String> {
    static NERD: OnceLock<Option<String>> = OnceLock::new();
    NERD.get_or_init(|| {
        let names = clean_font_names(cx.text_system().all_font_names());
        let pick = pick_nerd_fallback(&names);
        log(&format!("Nerd Font 回退：{pick:?}"));
        pick
    })
    .clone()
}

// ---- 窗口 ----

/// 打开设置窗口；已经打开就把它带到前台。返回窗口句柄供调用方保存。
pub(crate) fn open_window(
    model: Entity<SettingsModel>,
    existing: Option<WindowHandle<SettingsView>>,
    mcp: Entity<McpModel>,
    select_mcp: bool,
    cx: &mut App,
) -> Option<WindowHandle<SettingsView>> {
    model.update(cx, |m, cx| m.reload_themes(cx));
    if let Some(handle) = existing
        && handle
            .update(cx, |settings, window, cx| {
                if select_mcp && kanso_mcp_server::supported() {
                    settings.select_page(MCP_PAGE, cx);
                }
                window.activate_window();
            })
            .is_ok()
    {
        return Some(handle);
    }
    let bounds = Bounds::centered(None, size(px(560.0), px(760.0)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(render_message(&Message::UiSettings {}).into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| SettingsView::new(model, mcp, select_mcp, window, cx)),
    );
    match opened {
        Ok(handle) => {
            log("设置窗口已打开");
            Some(handle)
        }
        Err(e) => {
            log(&format!("打开设置窗口失败：{e:#}"));
            None
        }
    }
}

/// 设置窗口根视图。
pub(crate) struct SettingsView {
    titlebar_dark: Option<bool>,
    labels_language: Lang,
    page: usize,
    model: Entity<SettingsModel>,
    mcp: Entity<mcp_page::McpPage>,
    focus: FocusHandle,
    term_font: Entity<Dropdown>,
    term_font_input: Entity<TextInput>,
    /// 终端字体下拉的条目对应的字体名（下标 0 是"系统默认"，值为空串）。
    term_names: Vec<String>,
    ui_font: Entity<Dropdown>,
    ui_font_input: Entity<TextInput>,
    ui_names: Vec<String>,
    numbers: [Entity<TextInput>; 5],
    /// 键盘页的录制 / 冲突状态。
    keys: keys::KeysState,
    mode: Entity<Dropdown>,
    theme_choices: Vec<(String, String)>,
    light_theme: Entity<Dropdown>,
    dark_theme: Entity<Dropdown>,
    /// 等宽字体还在后台探测。
    probing: bool,
    _subs: Vec<Subscription>,
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// 字体下拉的条目：第 0 项是"系统默认"，之后是字体名；当前值不在列表里（手动输入的）就追加在末尾。
fn font_items(names: &[String], current: &str) -> (Vec<String>, Vec<Item>, usize) {
    let mut list: Vec<String> = vec![String::new()];
    list.extend(names.iter().cloned());
    if !current.is_empty() && !list.iter().any(|n| n == current) {
        list.push(current.to_owned());
    }
    let selected = list.iter().position(|n| n == current).unwrap_or(0);
    let items = font_labels(&list);
    (list, items, selected)
}

fn font_labels(names: &[String]) -> Vec<Item> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            Item::option(if i == 0 {
                label(Message::UiShellDefault {}).to_owned()
            } else {
                n.clone()
            })
        })
        .collect()
}

fn theme_items(themes: &[(String, String)]) -> Vec<Item> {
    themes
        .iter()
        .map(|(_, name)| Item::option(name.clone()))
        .collect()
}

fn theme_index(themes: &[(String, String)], id: &str, fallback: usize) -> usize {
    themes
        .iter()
        .position(|(i, _)| *i == id)
        .unwrap_or(fallback)
}

impl SettingsView {
    pub(super) fn select_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if self.page == MCP_PAGE || page == MCP_PAGE {
            self.mcp
                .update(cx, |mcp, cx| mcp.set_shown(page == MCP_PAGE, cx));
        }
        self.page = page;
        self.keys.cancel();
        cx.notify();
    }

    pub(crate) fn new(
        model: Entity<SettingsModel>,
        mcp: Entity<McpModel>,
        select_mcp: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (display, appearance) = {
            let m = model.read(cx);
            (m.display().clone(), m.appearance().clone())
        };
        let all_names = clean_font_names(cx.text_system().all_font_names());

        let (ui_names, ui_items, ui_sel) = font_items(&all_names, &display.ui_font_family);
        let ui_font = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(ui_items)
                .button_label(Message::UiChoose {})
                .filterable(Message::UiSearchFonts {})
                .selected(ui_sel)
        });
        // 终端字体：先只放"系统默认"和当前值，等宽字体探测完再补齐。
        let (term_names, term_items, term_sel) = font_items(&[], &display.font_family);
        let term_font = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(term_items)
                .button_label(Message::UiChoose {})
                .filterable(Message::UiSearchMonospacedFonts {})
                .selected(term_sel)
        });
        let term_font_input = cx.new(|cx| {
            TextInput::new(window, cx)
                .with_text(&display.font_family)
                .placeholder(Message::UiSystemDefaultOrTypeFont {})
        });
        let ui_font_input = cx.new(|cx| {
            TextInput::new(window, cx)
                .with_text(&display.ui_font_family)
                .placeholder(Message::UiSystemDefaultOrTypeFont {})
        });
        let numbers = NumericSetting::ALL
            .map(|field| cx.new(|cx| TextInput::new(window, cx).with_text(&field.text(&display))));
        numbers[NumericSetting::Contrast as usize].update(cx, |input, cx| {
            input.set_disabled(!display.minimum_contrast_enabled, cx)
        });
        let mode = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(
                    ThemeMode::ALL
                        .iter()
                        .map(|m| Item::option(m.label()))
                        .collect(),
                )
                .selected(
                    ThemeMode::ALL
                        .iter()
                        .position(|m| *m == appearance.mode)
                        .unwrap_or(0),
                )
        });
        let theme_choices = model.read(cx).theme_choices();
        let light_theme = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(theme_items(&theme_choices))
                .selected(theme_index(&theme_choices, &appearance.light_theme, 0))
        });
        let dark_theme = cx.new(|cx| {
            Dropdown::new(window, cx)
                .items(theme_items(&theme_choices))
                .selected(theme_index(&theme_choices, &appearance.dark_theme, 1))
        });

        let mut subs: Vec<Subscription> = Vec::new();
        subs.push(cx.subscribe(&mode, |this, _, ev: &DropdownEvent, cx| {
            if let DropdownEvent::Change(i) = ev {
                this.model.update(cx, |m, cx| {
                    m.update_appearance(cx, |a| a.mode = ThemeMode::ALL[*i])
                });
            }
        }));
        // 模型变化（含快捷键缩放）→ 重绘并同步控件。
        subs.push(cx.observe_in(&model, window, |this, _, w, cx| this.sync_widgets(w, cx)));
        // 窗口外观变化（跟随系统时）→ 重绘。
        subs.push(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.model.update(cx, |m, cx| m.reload_themes(cx));
            }
        }));

        subs.push(cx.subscribe(&term_font, |this, _, ev: &DropdownEvent, cx| {
            if let DropdownEvent::Change(i) = ev {
                let name = this.term_names.get(*i).cloned().unwrap_or_default();
                this.set_term_family(name, cx);
            }
        }));
        subs.push(cx.subscribe(&ui_font, |this, _, ev: &DropdownEvent, cx| {
            if let DropdownEvent::Change(i) = ev {
                let name = this.ui_names.get(*i).cloned().unwrap_or_default();
                this.set_ui_family(name, cx);
            }
        }));
        subs.push(
            cx.subscribe(&term_font_input, |this, input, ev: &TextInputEvent, cx| {
                if matches!(ev, TextInputEvent::Submit(_) | TextInputEvent::Blur) {
                    let t = input.read(cx).text().trim().to_owned();
                    this.set_term_family(t, cx);
                }
            }),
        );
        subs.push(
            cx.subscribe(&ui_font_input, |this, input, ev: &TextInputEvent, cx| {
                if matches!(ev, TextInputEvent::Submit(_) | TextInputEvent::Blur) {
                    let t = input.read(cx).text().trim().to_owned();
                    this.set_ui_family(t, cx);
                }
            }),
        );
        for (field, input) in NumericSetting::ALL.into_iter().zip(&numbers) {
            subs.push(
                cx.subscribe(input, move |this, input, ev: &TextInputEvent, cx| {
                    if matches!(ev, TextInputEvent::Submit(_) | TextInputEvent::Blur) {
                        let text = input.read(cx).text().to_owned();
                        this.model
                            .update(cx, |m, cx| m.update_display(cx, |d| field.commit(&text, d)));
                        let value = field.text(this.model.read(cx).display());
                        if text != value {
                            input.update(cx, |t, cx| t.set_text(&value, cx));
                        }
                    }
                }),
            );
        }
        subs.push(
            cx.subscribe(&light_theme, |this, _, ev: &DropdownEvent, cx| {
                if let DropdownEvent::Change(i) = ev {
                    let id = this.theme_choices[*i].0.clone();
                    this.model
                        .update(cx, |m, cx| m.update_appearance(cx, |a| a.light_theme = id));
                }
            }),
        );
        subs.push(
            cx.subscribe(&dark_theme, |this, _, ev: &DropdownEvent, cx| {
                if let DropdownEvent::Change(i) = ev {
                    let id = this.theme_choices[*i].0.clone();
                    this.model
                        .update(cx, |m, cx| m.update_appearance(cx, |a| a.dark_theme = id));
                }
            }),
        );

        let mcp = cx.new(|cx| mcp_page::McpPage::new(mcp, window, cx));
        let mut this = SettingsView {
            titlebar_dark: None,
            labels_language: lang(),
            page: 0,
            model,
            mcp,
            focus: cx.focus_handle(),
            term_font,
            term_font_input,
            term_names,
            ui_font,
            ui_font_input,
            ui_names,
            numbers,
            keys: keys::KeysState::default(),
            mode,
            theme_choices,
            light_theme,
            dark_theme,
            probing: true,
            _subs: subs,
        };
        if select_mcp && kanso_mcp_server::supported() {
            this.select_page(MCP_PAGE, cx);
        }
        this.probe_monospace(all_names, cx);
        this
    }

    /// 后台探测哪些字体是等宽的，完成后补齐终端字体下拉。
    fn probe_monospace(&self, names: Vec<String>, cx: &mut Context<Self>) {
        let ts = cx.text_system().clone();
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let mono: Vec<String> = cx
                .background_executor()
                .spawn(async move {
                    names
                        .into_iter()
                        .filter(|name| {
                            let font = gpui::Font {
                                family: name.clone().into(),
                                features: gpui::FontFeatures::default(),
                                fallbacks: None,
                                weight: gpui::FontWeight::NORMAL,
                                style: gpui::FontStyle::Normal,
                            };
                            // resolve_font 在族名和回退都解析不了时会 panic：当作非等宽跳过。
                            let Ok(id) =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    ts.resolve_font(&font)
                                }))
                            else {
                                return false;
                            };
                            let adv: Vec<f32> = ['i', 'm', 'W', '.']
                                .into_iter()
                                .filter_map(|ch| {
                                    ts.advance(id, px(16.0), ch)
                                        .ok()
                                        .map(|s| f32::from(s.width))
                                })
                                .collect();
                            adv.len() == 4 && is_monospace(&adv)
                        })
                        .collect()
                })
                .await;
            log(&format!(
                "等宽字体探测完成：{} 个，耗时 {} ms",
                mono.len(),
                started.elapsed().as_millis()
            ));
            let _ = this.update(cx, |this, cx| {
                this.probing = false;
                let current = this.model.read(cx).display().font_family.clone();
                let (names, items, sel) = font_items(&mono, &current);
                this.term_names = names;
                this.term_font.update(cx, |d, cx| {
                    d.set_items(items, cx);
                    d.set_selected(Some(sel), cx);
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn set_term_family(&mut self, name: String, cx: &mut Context<Self>) {
        self.model
            .update(cx, |m, cx| m.update_display(cx, |d| d.font_family = name));
    }

    fn set_ui_family(&mut self, name: String, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.update_display(cx, |d| d.ui_font_family = name)
        });
    }

    /// 模型变化后，把文本框 / 下拉的显示值对齐到模型（快捷键缩放、恢复默认等来源）。
    fn sync_widgets(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (d, a) = {
            let m = self.model.read(cx);
            (m.display().clone(), m.appearance().clone())
        };
        let sync_text = |input: &Entity<TextInput>, want: &str, cx: &mut Context<Self>| {
            if !input.read(cx).focus_handle(cx).is_focused(window) && input.read(cx).text() != want
            {
                input.update(cx, |t, cx| t.set_text(want, cx));
            }
        };
        sync_text(&self.term_font_input, &d.font_family, cx);
        sync_text(&self.ui_font_input, &d.ui_font_family, cx);
        if let Some(i) = self.term_names.iter().position(|n| *n == d.font_family) {
            self.term_font
                .update(cx, |dd, cx| dd.set_selected(Some(i), cx));
        }
        if let Some(i) = self.ui_names.iter().position(|n| *n == d.ui_font_family) {
            self.ui_font
                .update(cx, |dd, cx| dd.set_selected(Some(i), cx));
        }
        for (field, input) in NumericSetting::ALL.into_iter().zip(&self.numbers) {
            sync_text(input, &field.text(&d), cx);
        }
        self.numbers[NumericSetting::Contrast as usize].update(cx, |input, cx| {
            input.set_disabled(!d.minimum_contrast_enabled, cx)
        });
        self.mode.update(cx, |dd, cx| {
            dd.set_selected(ThemeMode::ALL.iter().position(|m| *m == a.mode), cx)
        });
        let choices = self.model.read(cx).theme_choices();
        let changed = choices != self.theme_choices;
        self.theme_choices = choices;
        for (dropdown, id, fallback) in [
            (&self.light_theme, &a.light_theme, 0),
            (&self.dark_theme, &a.dark_theme, 1),
        ] {
            dropdown.update(cx, |dd, cx| {
                if changed {
                    dd.set_items(theme_items(&self.theme_choices), cx);
                }
                dd.set_selected(Some(theme_index(&self.theme_choices, id, fallback)), cx);
            });
        }
        cx.notify();
    }
}

// ---- 绘制辅助 ----

struct Th(Appearance);

impl Th {
    fn c(&self, name: &str) -> Hsla {
        rgba(color(name, self.0).to_u32()).into()
    }
}

fn card(th: &Th, title: &str, symbol: &'static str, rows: Vec<AnyElement>) -> AnyElement {
    super::widgets::card::card(
        super::shell::Theme { appearance: th.0 },
        title,
        symbol,
        rows,
    )
    .into_any_element()
}

fn row(th: &Th, label: &str, control: impl IntoElement) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(th.c("text.secondary"))
                .child(label.to_owned()),
        )
        .child(control)
        .into_any_element()
}

fn check_row(th: &Th, label: String, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(control)
        .child(div().text_color(th.c("text.primary")).child(label))
        .into_any_element()
}

fn caption(th: &Th, text: &str) -> AnyElement {
    div()
        .text_size(px(number("font", "ui.caption")))
        .text_color(th.c("text.tertiary"))
        .child(text.to_owned())
        .into_any_element()
}

/// 分段选择：选中项用 `bg.segmentSelected`。
fn segmented(
    th: &Th,
    cx: &mut Context<SettingsView>,
    id: &'static str,
    labels: &[&str],
    selected: usize,
    on_pick: impl Fn(&mut SettingsView, usize, &mut Context<SettingsView>) + Clone + 'static,
) -> AnyElement {
    let buttons: Vec<AnyElement> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let on_pick = on_pick.clone();
            let on = i == selected;
            div()
                .id((id, i))
                .px(px(12.0))
                .w(px(112.0))
                .h(px(52.0))
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(3.0))
                .items_center()
                .rounded(px(number("radius", "md")))
                .cursor_pointer()
                .border_1()
                .border_color(if on {
                    th.c("border.segmentSelected")
                } else {
                    th.c("border.subtle")
                })
                .bg(if on {
                    th.c("bg.segmentSelected")
                } else {
                    th.c("bg.field")
                })
                .text_color(if on {
                    th.c("text.primary")
                } else {
                    th.c("text.secondary")
                })
                .hover(|s| s.bg(th.c("bg.rowHover")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| on_pick(this, i, cx)),
                )
                .when(id == "shape", |d| {
                    d.child(icon(
                        ["square", "underline", "text-cursor"][i],
                        18.0,
                        th.c(if on { "accent" } else { "text.secondary" }),
                    ))
                })
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child((*label).to_owned())
                .into_any_element()
        })
        .collect();
    div()
        .flex()
        .gap(px(8.0))
        .children(buttons)
        .into_any_element()
}

impl SettingsView {
    fn numeric_control(
        &self,
        th: &Th,
        field: NumericSetting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let d = self.model.read(cx).display();
        let value = field.value(d);
        let (lo, hi, step) = field.range_step();
        let enabled = !matches!(field, NumericSetting::Contrast) || d.minimum_contrast_enabled;
        let button = |direction: i32, text: &'static str, cx: &mut Context<Self>| {
            let active = enabled
                && if direction < 0 {
                    value > lo
                } else {
                    value < hi
                };
            div()
                .id((
                    "number-step",
                    field as usize * 2 + usize::from(direction > 0),
                ))
                .w(px(28.0))
                .h(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(number("radius", "md")))
                .bg(th.c("bg.buttonSecondary"))
                .when(!active, |b| b.opacity(0.45))
                .when(active, |b| {
                    b.cursor_pointer()
                        .hover(|s| s.bg(th.c("bg.rowHover")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.model.update(cx, |m, cx| {
                                    m.update_display(cx, |d| {
                                        field.set(d, field.value(d) + f64::from(direction) * step)
                                    })
                                });
                            }),
                        )
                })
                .child(text)
        };
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(boxed(
                self.numbers[field as usize].clone(),
                if matches!(field, NumericSetting::Scrollback) {
                    120.0
                } else {
                    72.0
                },
            ))
            .child(button(-1, "−", cx))
            .child(button(1, "+", cx))
            .into_any_element()
    }
}

/// 复选行（右侧一个方框）。
fn checkbox<V: 'static>(
    th: &Th,
    cx: &mut Context<V>,
    id: &'static str,
    checked: bool,
    on_toggle: impl Fn(&mut V, bool, &mut Context<V>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .size(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(number("radius", "sm")))
        .cursor_pointer()
        .border_1()
        .border_color(if checked {
            th.c("accent")
        } else {
            th.c("border.field")
        })
        .bg(if checked {
            th.c("accent")
        } else {
            th.c("bg.field")
        })
        .text_color(th.c("text.onAccent"))
        .text_size(px(12.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _w, cx| on_toggle(this, !checked, cx)),
        )
        .when(checked, |d| {
            d.child(icon("check", 13.0, th.c("text.onAccent")))
        })
        .into_any_element()
}

fn boxed(control: impl IntoElement, width: f32) -> AnyElement {
    div().w(px(width)).child(control).into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = display::effective_dark(window);
        if self.titlebar_dark != Some(dark) {
            display::apply_titlebar(window);
            self.titlebar_dark = Some(dark);
        }
        let th = Th(display::window_appearance(window));
        let (d, a) = {
            let m = self.model.read(cx);
            (m.display().clone(), m.appearance().clone())
        };

        if self.labels_language != lang() {
            self.labels_language = lang();
            window.set_window_title(label(Message::UiSettings {}));
            self.mode.update(cx, |picker, cx| {
                picker.set_items(
                    ThemeMode::ALL
                        .iter()
                        .map(|m| Item::option(m.label()))
                        .collect(),
                    cx,
                );
            });
            for (picker, names) in [
                (&self.ui_font, &self.ui_names),
                (&self.term_font, &self.term_names),
            ] {
                let items = font_labels(names);
                picker.update(cx, |picker, cx| picker.set_items(items, cx));
            }
            self.sync_widgets(window, cx);
            self.mcp.update(cx, |_, cx| cx.notify());
        }

        // 外观
        let appearance_card = card(
            &th,
            &render_message(&Message::UiAppearance {}),
            "sun-moon",
            vec![
                row(
                    &th,
                    &render_message(&Message::UiTheme {}),
                    self.mode.clone(),
                ),
                row(
                    &th,
                    &render_message(&Message::UiLightTheme {}),
                    boxed(self.light_theme.clone(), 220.0),
                ),
                row(
                    &th,
                    &render_message(&Message::UiDarkTheme {}),
                    boxed(self.dark_theme.clone(), 220.0),
                ),
                div()
                    .id("open-theme-folder")
                    .cursor_pointer()
                    .text_color(th.c("accent"))
                    .child(label(Message::UiOpenThemesFolder {}))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.open_with_system(&this.model.read(cx).themes_dir);
                    }))
                    .into_any_element(),
            ],
        );

        // 语言（ui-shell.md §10：Windows / Linux 在设置里切换）
        let lang_idx = LanguageSetting::ALL
            .iter()
            .position(|l| *l == a.language)
            .unwrap_or(0);
        let language_card = card(
            &th,
            label(Message::UiLanguage {}),
            "settings",
            vec![
                row(
                    &th,
                    label(Message::UiInterfaceLanguage {}),
                    segmented(
                        &th,
                        cx,
                        "language",
                        &LanguageSetting::ALL.map(LanguageSetting::label),
                        lang_idx,
                        |this, i, cx| {
                            this.model.update(cx, |m, cx| {
                                m.update_appearance(cx, |a| a.language = LanguageSetting::ALL[i])
                            })
                        },
                    ),
                ),
                caption(&th, label(Message::UiTakesEffectImmediatelyFollowSystem {})),
            ],
        );

        // 字体字段与字号并排，尺寸对照 Mac 的 KCard。
        let mut font_rows = vec![
            div()
                .flex()
                .items_start()
                .gap(px(12.0))
                .child(row(
                    &th,
                    &render_message(&Message::UiTerminalFont {}),
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(div().flex_1().min_w_0().child(self.term_font_input.clone()))
                        .child(boxed(self.term_font.clone(), 100.0)),
                ))
                .child(div().flex_none().child(row(
                    &th,
                    &render_message(&Message::UiFontSize {}),
                    self.numeric_control(&th, NumericSetting::FontSize, cx),
                )))
                .into_any_element(),
            row(
                &th,
                &render_message(&Message::UiLineHeight {}),
                self.numeric_control(&th, NumericSetting::LineHeight, cx),
            ),
            caption(
                &th,
                &render_message(&if self.probing {
                    Message::UiFontProbing {}
                } else {
                    Message::UiFontHint {}
                }),
            ),
        ];
        if !d.font_family.trim().is_empty()
            && !self
                .ui_names
                .iter()
                .any(|name| name.eq_ignore_ascii_case(d.font_family.trim()))
        {
            font_rows.push(
                div()
                    .text_size(px(11.0))
                    .text_color(th.c("status.warning"))
                    .child(render_message(&Message::UiMissingTerminalFont {
                        name: d.font_family.clone(),
                    }))
                    .into_any_element(),
            );
        }
        let font_card = card(&th, label(Message::UiFont {}), "type", font_rows);

        // 光标
        let shape_idx = CursorShapeSetting::ALL
            .iter()
            .position(|s| *s == d.cursor_shape)
            .unwrap_or(0);
        let cursor_card = card(
            &th,
            &render_message(&Message::UiCursor {}),
            "text-cursor",
            vec![
                row(
                    &th,
                    &render_message(&Message::UiShape {}),
                    segmented(
                        &th,
                        cx,
                        "shape",
                        &CursorShapeSetting::ALL.map(CursorShapeSetting::label),
                        shape_idx,
                        |this, i, cx| {
                            this.model.update(cx, |m, cx| {
                                m.update_display(cx, |d| {
                                    d.cursor_shape = CursorShapeSetting::ALL[i]
                                })
                            })
                        },
                    ),
                ),
                check_row(
                    &th,
                    render_message(&Message::UiCursorBlink {}),
                    checkbox(&th, cx, "blink", d.cursor_blink, |this, on, cx| {
                        this.model
                            .update(cx, |m, cx| m.update_display(cx, |d| d.cursor_blink = on))
                    }),
                ),
            ],
        );

        // 回滚
        let scroll_card = card(
            &th,
            &render_message(&Message::UiScrollback {}),
            "list",
            vec![
                row(
                    &th,
                    &render_message(&Message::UiScrollbackLines {}),
                    self.numeric_control(&th, NumericSetting::Scrollback, cx),
                ),
                caption(&th, &render_message(&Message::UiScrollbackHint {})),
            ],
        );

        // 界面字体
        let ui_card = card(
            &th,
            &render_message(&Message::UiInterfaceFont {}),
            "type",
            vec![
                row(
                    &th,
                    &render_message(&Message::UiFont {}),
                    boxed(self.ui_font.clone(), 300.0),
                ),
                row(
                    &th,
                    &render_message(&Message::UiFontCustom {}),
                    boxed(self.ui_font_input.clone(), 300.0),
                ),
                row(
                    &th,
                    &render_message(&Message::UiFontSize {}),
                    self.numeric_control(&th, NumericSetting::UiFontSize, cx),
                ),
            ],
        );

        let contrast_card = card(
            &th,
            &render_message(&Message::UiContrast {}),
            "palette",
            vec![
                check_row(
                    &th,
                    render_message(&Message::UiContrastEnabled {}),
                    checkbox(
                        &th,
                        cx,
                        "contrast",
                        d.minimum_contrast_enabled,
                        |this, on, cx| {
                            this.model.update(cx, |m, cx| {
                                m.update_display(cx, |d| d.minimum_contrast_enabled = on)
                            })
                        },
                    ),
                ),
                check_row(
                    &th,
                    render_message(&Message::UiSemanticColor {}),
                    checkbox(&th, cx, "semantic", d.semantic_coloring, |this, on, cx| {
                        this.model.update(cx, |m, cx| {
                            m.update_display(cx, |d| d.semantic_coloring = on)
                        })
                    }),
                ),
                row(
                    &th,
                    &render_message(&Message::UiContrastRatio {}),
                    self.numeric_control(&th, NumericSetting::Contrast, cx),
                ),
                caption(&th, &render_message(&Message::UiContrastHint {})),
            ],
        );
        let mut toolbar = div()
            .flex_none()
            .h(px(68.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .bg(th.c("bg.sidebar"))
            .border_b_1()
            .border_color(th.c("border.subtle"));
        for (i, title, symbol) in [
            (0, Message::UiGeneral {}, "sun-moon"),
            (1, Message::UiDisplay {}, "type"),
            (2, Message::UiKeysPage {}, "keyboard"),
            (MCP_PAGE, Message::WinMcpPage {}, "plug"),
        ] {
            if i == MCP_PAGE && !kanso_mcp_server::supported() {
                continue;
            }
            toolbar = toolbar.child(
                div()
                    .id(("settings-tab", i))
                    .w(px(72.0))
                    .h(px(54.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(3.0))
                    .rounded(px(8.0))
                    .text_size(px(12.0))
                    .text_color(th.c("text.secondary"))
                    .when(self.page == i, |d| {
                        d.bg(th.c("bg.tabSelected"))
                            .text_color(th.c("text.primary"))
                    })
                    .cursor_pointer()
                    .hover(|s| s.bg(th.c("bg.rowHover")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_page(i, cx);
                        }),
                    )
                    .child(icon(symbol, 22.0, th.c("text.secondary")))
                    .child(render_message(&title)),
            );
        }
        let cards = match self.page {
            0 => vec![appearance_card, language_card],
            1 => vec![font_card, cursor_card, scroll_card, contrast_card, ui_card],
            MCP_PAGE => vec![self.mcp.clone().into_any_element()],
            _ => self.keys_cards(&th, cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(th.c("bg.window"))
            .font_family(SharedString::from(display::ui_font_family()))
            .text_size(px(display::ui_font_size()))
            .text_color(th.c("text.primary"))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| this.keys_on_key(ev, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    // 点在别处：放弃正在进行的录制。
                    if this.keys.cancel() {
                        cx.notify();
                    }
                }),
            )
            .child(toolbar)
            .child(
                div()
                    .id(if self.page == MCP_PAGE {
                        "settings-mcp-scroll"
                    } else {
                        "settings-scroll"
                    })
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(16.0))
                            .p(px(20.0))
                            .children(cards),
                    ),
            )
    }
}
