//! 可自定义的快捷键（ui-shell.md §7）：组合键解析 / 格式化 / 规范化、冲突检测、用户覆盖与默认表的合并、序列化。
//!
//! 存储格式与 mac 版 `KeybindingSettings` 相同：`{"v":1,"bindings":{"<命令 id>":"<组合键文本>"}}`，
//! 只存与默认值不同的项，空串表示用户清空；组合键文本如 `ctrl+shift+\`、`alt+f3`（修饰键在前、键在最后）。
//! 命令 id 沿用 mac 的名字（`newLocalShell`、`findNext` …）；Windows 独有的命令用菜单 id（见 [`menus::encode`]）。
//! **设置表的 key 是 [`SETTING_KEY`]（`keybindings-win`），不是 mac 的 `keybindings`**：两个平台的修饰键不同
//! （mac 是 `cmd`），共用同一个 key 会互相读到对方的按键。mac 版读不到这个 key，也就忽略它。
//!
//! 生效方式：[`install`] 把合并结果放进全局 [`Keymap`]，[`crate::app::shortcuts::lookup`] 与
//! [`crate::app::shortcuts::shortcut_text`] 都读它；改绑后再 [`install`] 一次即可，不需要重新注册 GPUI 按键绑定
//! （应用级快捷键本来就不是 GPUI 的 key binding，而是外壳在捕获阶段查表）。
//!
//! 本文件不依赖窗口系统。

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde_json::{Map, Value, json};

use crate::app::launcher::render_message;
use crate::app::menus;
use crate::app::shortcuts::{AppCommand, DEFAULT_BINDINGS};
use crate::keys::KeyInput;
use kanso_types::message::Message;

/// 设置表里的 key（Windows / Linux 专用，与 mac 的 `keybindings` 区分）。
pub const SETTING_KEY: &str = "keybindings-win";
/// 存储版本。
pub const VERSION: u32 = 1;

// ---- 组合键 ----

/// 一个组合键：修饰键 + 规范化的键名。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Ctrl。
    pub ctrl: bool,
    /// Alt。
    pub alt: bool,
    /// Shift。
    pub shift: bool,
    /// 规范化键名：字符键为小写字符（带 Shift 的符号还原成基础字符），命名键为 GPUI 的小写英文名。
    pub key: String,
}

/// 美式键盘上 Shift 后的符号 → 基础字符。
fn unshift(key: &str) -> Option<&'static str> {
    Some(match key {
        "{" => "[",
        "}" => "]",
        "|" => "\\",
        ":" => ";",
        "\"" => "'",
        "<" => ",",
        ">" => ".",
        "?" => "/",
        "~" => "`",
        "_" => "-",
        "+" => "=",
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        _ => return None,
    })
}

/// 命名键（存储与显示用）。
const NAMED_KEYS: &[&str] = &[
    "left",
    "right",
    "up",
    "down",
    "home",
    "end",
    "pageup",
    "pagedown",
    "insert",
    "delete",
    "enter",
    "tab",
    "space",
    "backspace",
    "escape",
];

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

fn is_navigation_key(key: &str) -> bool {
    matches!(
        key,
        "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "insert"
            | "delete"
    )
}

/// 规范化键名：小写；Shift 按下时把符号还原成基础字符；兼容 mac 的别名。
fn normalize_key(key: &str, shift: bool) -> String {
    let lower = key.to_lowercase();
    let lower = match lower.as_str() {
        "return" => "enter".to_owned(),
        "forwarddelete" => "delete".to_owned(),
        "esc" => "escape".to_owned(),
        _ => lower,
    };
    if shift && let Some(base) = unshift(&lower) {
        return base.to_owned();
    }
    lower
}

/// 不能当作按键的纯修饰键名。
fn is_modifier_name(key: &str) -> bool {
    matches!(
        key,
        "control" | "ctrl" | "shift" | "alt" | "meta" | "platform" | "super" | "win" | "capslock"
    )
}

/// 不允许绑定的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// 缺少修饰键（可打印键需要 Ctrl / Alt；导航键需要任一修饰键）。
    NeedsModifier,
    /// 系统保留或固定的组合。
    Reserved,
}

impl Chord {
    /// 构造（键名会被规范化）。
    pub fn new(ctrl: bool, alt: bool, shift: bool, key: &str) -> Chord {
        Chord {
            ctrl,
            alt,
            shift,
            key: normalize_key(key, shift),
        }
    }

    /// 由按键事件构造；Win 键组合与纯修饰键返回 `None`。
    pub fn from_input(input: &KeyInput<'_>) -> Option<Chord> {
        if input.platform || input.key.is_empty() || is_modifier_name(&input.key.to_lowercase()) {
            return None;
        }
        Some(Chord::new(input.ctrl, input.alt, input.shift, input.key))
    }

    /// 存储文本，如 `ctrl+shift+\`。
    pub fn storage(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("ctrl+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        if self.shift {
            s.push_str("shift+");
        }
        s.push_str(&self.key);
        s
    }

    /// 解析存储文本；格式不对（含 mac 的 `cmd+`）返回 `None`。
    pub fn parse(text: &str) -> Option<Chord> {
        let mut rest = text;
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        'outer: loop {
            for (token, flag) in [
                ("ctrl+", &mut ctrl),
                ("alt+", &mut alt),
                ("shift+", &mut shift),
            ] {
                if let Some(r) = rest.strip_prefix(token)
                    && !r.is_empty()
                {
                    *flag = true;
                    rest = r;
                    continue 'outer;
                }
            }
            break;
        }
        let key = normalize_key(rest, false);
        let valid = rest == rest.to_lowercase()
            && (NAMED_KEYS.contains(&key.as_str())
                || is_function_key(&key)
                || key.chars().count() == 1);
        valid.then_some(Chord {
            ctrl,
            alt,
            shift,
            key,
        })
    }

    /// 界面显示，如 `Ctrl+Shift+\`、`Ctrl+Alt+Left`、`F3`。
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        let mut chars = self.key.chars();
        let key = match (chars.next(), chars.next()) {
            (Some(c), None) => c.to_uppercase().to_string(),
            _ => {
                let mut s = self.key.clone();
                s[..1].make_ascii_uppercase();
                s
            }
        };
        parts.push(key);
        parts.join("+")
    }

    fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// 能不能作为绑定。
    ///
    /// - 功能键（F1–F24）可以不带修饰键（默认的 F3 就是）；
    /// - 导航键（方向键、Home 等）至少带一个修饰键；
    /// - 其余键必须带 Ctrl 或 Alt（只带 Shift 会让输入文字变成快捷键，terminal.md §9）。
    /// - Alt+1–Alt+9（固定的切换标签）和 Alt+F4（系统关闭窗口）保留。
    pub fn validate(&self) -> Result<(), Invalid> {
        let ok = if is_function_key(&self.key) {
            true
        } else if is_navigation_key(&self.key) {
            self.has_modifier()
        } else {
            self.ctrl || self.alt
        };
        if !ok {
            return Err(Invalid::NeedsModifier);
        }
        if self.is_reserved() {
            return Err(Invalid::Reserved);
        }
        Ok(())
    }

    fn is_reserved(&self) -> bool {
        let digit = self.key.len() == 1 && matches!(self.key.as_bytes()[0], b'1'..=b'9');
        self.alt && !self.ctrl && !self.shift && (digit || self.key == "f4")
    }
}

// ---- 录制 ----

/// 录制框里一次按键的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Esc：取消录制。
    Cancel,
    /// Backspace / Delete（无修饰键）：清空绑定。
    Clear,
    /// 一个组合键（还需 [`Chord::validate`]）。
    Chord(Chord),
    /// 忽略（纯修饰键、Win 键）。
    Ignore,
}

/// 解释录制时的按键。
pub fn record(input: &KeyInput<'_>) -> Recorded {
    let plain = !(input.ctrl || input.alt || input.shift || input.platform);
    if plain {
        match input.key {
            "escape" => return Recorded::Cancel,
            "backspace" | "delete" => return Recorded::Clear,
            _ => {}
        }
    }
    match Chord::from_input(input) {
        Some(c) => Recorded::Chord(c),
        None => Recorded::Ignore,
    }
}

// ---- 命令目录 ----

/// 命令分组（与菜单一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// 会话。
    Session,
    /// 编辑。
    Edit,
    /// 视图。
    View,
    /// 其他（其他模块新增、目录里没有登记的命令）。
    Other,
}

impl Group {
    /// 显示顺序。
    pub const ALL: [Group; 4] = [Group::Session, Group::Edit, Group::View, Group::Other];
}

/// 一个可改绑的命令。
#[derive(Clone, Debug)]
pub struct Entry {
    /// 存储用的命令 id（发布后不得改名）。
    pub id: String,
    /// 命令。
    pub command: AppCommand,
    /// 分组。
    pub group: Group,
    /// 默认组合键（可以有多个别名；没有为空）。
    pub defaults: Vec<Chord>,
}

/// 显式登记的命令：id（沿用 mac 的 rawValue）、命令、分组。顺序即界面顺序。
const KNOWN: &[(&str, AppCommand, Group)] = &[
    ("newSession", AppCommand::NewSession, Group::Session),
    ("openLauncher", AppCommand::OpenLauncher, Group::Session),
    ("newLocalShell", AppCommand::NewLocalTab, Group::Session),
    ("previousSession", AppCommand::PrevTab, Group::Session),
    ("nextSession", AppCommand::NextTab, Group::Session),
    ("previousSplit", AppCommand::PrevPane, Group::Session),
    ("nextSplit", AppCommand::NextPane, Group::Session),
    ("closeSession", AppCommand::CloseSession, Group::Session),
    ("copy", AppCommand::Copy, Group::Edit),
    ("paste", AppCommand::Paste, Group::Edit),
    ("selectAll", AppCommand::SelectAll, Group::Edit),
    ("clearBuffer", AppCommand::ClearBuffer, Group::Edit),
    ("find", AppCommand::Find, Group::Edit),
    ("findNext", AppCommand::FindNext, Group::Edit),
    ("findPrevious", AppCommand::FindPrev, Group::Edit),
    ("previousCommandBlock", AppCommand::PrevBlock, Group::Edit),
    ("nextCommandBlock", AppCommand::NextBlock, Group::Edit),
    (
        "commandSuggestions",
        AppCommand::ToggleCommandSuggestions,
        Group::Edit,
    ),
    (
        "clearCommandHistory",
        AppCommand::ClearCommandHistory,
        Group::Edit,
    ),
    ("toggleTabBar", AppCommand::ToggleSidebar, Group::View),
    ("toggleFiler", AppCommand::ToggleFiler, Group::View),
    ("toggleSender", AppCommand::ToggleSender, Group::View),
    ("toggleGit", AppCommand::ToggleGit, Group::View),
    ("commandPalette", AppCommand::OpenPalette, Group::View),
    (
        "locateWorkingDirectory",
        AppCommand::LocateFiler,
        Group::View,
    ),
    ("splitRight", AppCommand::SplitRight, Group::View),
    ("splitDown", AppCommand::SplitDown, Group::View),
    ("increaseFontSize", AppCommand::ZoomIn, Group::View),
    ("decreaseFontSize", AppCommand::ZoomOut, Group::View),
    ("resetFontSize", AppCommand::ZoomReset, Group::View),
    ("openSettings", AppCommand::OpenSettings, Group::View),
];

fn default_chords(command: AppCommand) -> Vec<Chord> {
    let mut out: Vec<Chord> = Vec::new();
    for bd in DEFAULT_BINDINGS.iter().filter(|b| b.command == command) {
        let c = Chord::new(bd.ctrl, bd.alt, bd.shift, bd.key);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// 全部可改绑的命令：先是显式登记的，再把默认表里出现、但没登记的命令（别的模块新增的）追加到“其他”组。
/// `SelectTab`（Alt+1–9）固定，不在里面。
pub fn catalog() -> Vec<Entry> {
    let mut out: Vec<Entry> = KNOWN
        .iter()
        .map(|(id, command, group)| Entry {
            id: (*id).to_owned(),
            command: *command,
            group: *group,
            defaults: default_chords(*command),
        })
        .collect();
    for bd in DEFAULT_BINDINGS {
        if matches!(bd.command, AppCommand::SelectTab(_))
            || out.iter().any(|e| e.command == bd.command)
        {
            continue;
        }
        out.push(Entry {
            id: menus::encode(bd.command),
            command: bd.command,
            group: Group::Other,
            defaults: default_chords(bd.command),
        });
    }
    out
}

// ---- 用户覆盖 ----

/// 改绑结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssignResult {
    /// 已绑定。
    Assigned,
    /// 已被另一个命令（id）占用；调用方询问用户后用 [`KeybindingSettings::assign_replacing`] 继续。
    Conflict(String),
    /// 系统保留 / 固定的组合。
    Reserved,
    /// 缺少修饰键。
    Invalid,
}

/// 用户的快捷键覆盖：命令 id → 组合键文本，空串表示清空。只存与默认值不同的项。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeybindingSettings {
    /// 覆盖表。
    pub overrides: BTreeMap<String, String>,
}

impl KeybindingSettings {
    /// 解码；缺失 / 坏数据 / 类型不对的项都忽略（回落默认值）。
    pub fn decode(json: Option<&str>) -> KeybindingSettings {
        let mut overrides = BTreeMap::new();
        let bindings = json
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| v.get("bindings").cloned());
        if let Some(Value::Object(m)) = bindings {
            for (k, v) in m {
                if let Some(s) = v.as_str() {
                    overrides.insert(k, s.to_owned());
                }
            }
        }
        KeybindingSettings { overrides }
    }

    /// 编码。
    pub fn encode(&self) -> String {
        let bindings: Map<String, Value> = self
            .overrides
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        json!({ "v": VERSION, "bindings": bindings }).to_string()
    }

    /// 命令当前生效的组合：有覆盖用覆盖（空串 = 不绑定；解析不了回落默认），否则默认值（含别名）。
    pub fn chords_of(&self, entry: &Entry) -> Vec<Chord> {
        match self.overrides.get(&entry.id) {
            None => entry.defaults.clone(),
            Some(s) if s.is_empty() => Vec::new(),
            Some(s) => match Chord::parse(s) {
                Some(c) => vec![c],
                None => entry.defaults.clone(),
            },
        }
    }

    /// 当前占用该组合的命令（不含 `excluding`）。
    pub fn holder(&self, chord: &Chord, excluding: &str) -> Option<Entry> {
        catalog()
            .into_iter()
            .find(|e| e.id != excluding && self.chords_of(e).contains(chord))
    }

    /// 写入一个命令的组合（`None` = 清空）；与默认值相同时删掉覆盖项。
    fn store(&mut self, entry: &Entry, chords: &[Chord]) {
        if chords == entry.defaults.as_slice() {
            self.overrides.remove(&entry.id);
        } else {
            let first = chords.first().map(Chord::storage).unwrap_or_default();
            self.overrides.insert(entry.id.clone(), first);
        }
    }

    fn entry(id: &str) -> Option<Entry> {
        catalog().into_iter().find(|e| e.id == id)
    }

    /// 尝试绑定；冲突 / 不合法时不修改。
    pub fn assign(&mut self, id: &str, chord: &Chord) -> AssignResult {
        let Some(entry) = Self::entry(id) else {
            return AssignResult::Invalid;
        };
        match chord.validate() {
            Err(Invalid::NeedsModifier) => return AssignResult::Invalid,
            Err(Invalid::Reserved) => return AssignResult::Reserved,
            Ok(()) => {}
        }
        if let Some(other) = self.holder(chord, id) {
            return AssignResult::Conflict(other.id);
        }
        self.store(&entry, std::slice::from_ref(chord));
        AssignResult::Assigned
    }

    /// 用户确认替换：先从占用者那里拿走该组合（它还有别的别名就保留第一个，否则清空），再绑定。
    pub fn assign_replacing(&mut self, id: &str, chord: &Chord) -> AssignResult {
        let Some(entry) = Self::entry(id) else {
            return AssignResult::Invalid;
        };
        match chord.validate() {
            Err(Invalid::NeedsModifier) => return AssignResult::Invalid,
            Err(Invalid::Reserved) => return AssignResult::Reserved,
            Ok(()) => {}
        }
        if let Some(other) = self.holder(chord, id) {
            let rest: Vec<Chord> = self
                .chords_of(&other)
                .into_iter()
                .filter(|c| c != chord)
                .take(1)
                .collect();
            self.store(&other, &rest);
            // 占用者原来有多个别名、拿走一个后剩一个时 store 会写成覆盖项，行为就是“只保留第一个”。
        }
        self.store(&entry, std::slice::from_ref(chord));
        AssignResult::Assigned
    }

    /// 清空：命令不绑定任何键。
    pub fn clear(&mut self, id: &str) {
        if let Some(entry) = Self::entry(id) {
            self.store(&entry, &[]);
        }
    }

    /// 恢复单个命令的默认键。若默认键正被别的命令占用，别的命令的覆盖优先，这里保持不绑定。
    pub fn restore(&mut self, id: &str) {
        self.overrides.remove(id);
    }

    /// 恢复整张表。
    pub fn restore_all(&mut self) {
        self.overrides.clear();
    }

    /// 命令是否与默认值不同。
    pub fn is_customized(&self, id: &str) -> bool {
        self.overrides.contains_key(id)
    }

    /// 合并成可查询的键位表。
    pub fn keymap(&self) -> Keymap {
        Keymap::build(self)
    }
}

// ---- 文字 ----

/// 分组标题。
pub fn group_label(group: Group) -> String {
    render_message(&match group {
        Group::Session => Message::UiSession {},
        Group::Edit => Message::UiEdit2 {},
        Group::View => Message::UiView {},
        Group::Other => Message::UiKeysGroupOther {},
    })
}

/// 命令的显示名。目录里没有专门文案的命令（别的模块新增的）退回命令 id，所以新命令不会消失。
pub fn command_label(entry: &Entry) -> String {
    use AppCommand as C;
    let m = match entry.command {
        C::NewSession => Message::UiNewSession2 {},
        C::OpenLauncher => Message::UiKeysCmdOpenLauncher {},
        C::NewLocalTab => Message::UiNewLocalShell {},
        C::PrevTab => Message::UiPreviousTab {},
        C::NextTab => Message::UiNextTab {},
        C::PrevPane => Message::UiPreviousPane {},
        C::NextPane => Message::UiNextPane {},
        C::CloseSession => Message::UiCloseSession {},
        C::Copy => Message::UiCopy {},
        C::Paste => Message::UiPaste {},
        C::SelectAll => Message::UiSelectAll {},
        C::ClearBuffer => Message::UiClearBuffer {},
        C::Find => Message::UiKeysCmdFind {},
        C::FindNext => Message::UiKeysCmdFindNext {},
        C::FindPrev => Message::UiKeysCmdFindPrev {},
        C::PrevBlock => Message::UiKeysCmdPrevBlock {},
        C::NextBlock => Message::UiKeysCmdNextBlock {},
        C::ToggleCommandSuggestions => Message::UiMenuCommandSuggestions {},
        C::ClearCommandHistory => Message::UiMenuClearHistory {},
        C::ToggleSidebar => Message::UiKeysCmdToggleSidebar {},
        C::ToggleFiler => Message::UiKeysCmdToggleFiler {},
        C::ToggleSender => Message::UiKeysCmdToggleSender {},
        C::ToggleGit => Message::UiGitMenuView {},
        C::OpenPalette => Message::UiKeysCmdOpenPalette {},
        C::LocateFiler => Message::UiFilerMenuLocate {},
        C::SplitRight => Message::UiSplitRight {},
        C::SplitDown => Message::UiSplitDown {},
        C::ZoomIn => Message::UiKeysCmdZoomIn {},
        C::ZoomOut => Message::UiKeysCmdZoomOut {},
        C::ZoomReset => Message::UiKeysCmdZoomReset {},
        C::OpenSettings => Message::UiSettings2 {},
        _ => return entry.id.clone(),
    };
    render_message(&m)
}

/// 一行的当前快捷键文字：多个别名用 ` / ` 连接；没有绑定返回 `None`。
pub fn chords_text(chords: &[Chord]) -> Option<String> {
    if chords.is_empty() {
        None
    } else {
        Some(
            chords
                .iter()
                .map(Chord::display)
                .collect::<Vec<_>>()
                .join(" / "),
        )
    }
}

// ---- 合并后的键位表 ----

/// 生效的键位表：组合键 → 命令，以及命令 → 显示文字。
#[derive(Clone, Debug)]
pub struct Keymap {
    chords: Vec<(Chord, AppCommand)>,
}

impl Keymap {
    /// 只含默认值。
    pub fn defaults() -> Keymap {
        KeybindingSettings::default().keymap()
    }

    /// 默认值 + 用户覆盖。同一组合出现在多个命令上（外来设置造成）时，目录顺序靠前的生效。
    pub fn build(settings: &KeybindingSettings) -> Keymap {
        let mut chords: Vec<(Chord, AppCommand)> = Vec::new();
        let mut push = |c: Chord, cmd: AppCommand| {
            if !chords.iter().any(|(x, _)| *x == c) {
                chords.push((c, cmd));
            }
        };
        for entry in catalog() {
            for c in settings.chords_of(&entry) {
                push(c, entry.command);
            }
        }
        // Alt+1–9 固定。
        for bd in DEFAULT_BINDINGS {
            if matches!(bd.command, AppCommand::SelectTab(_)) {
                push(Chord::new(bd.ctrl, bd.alt, bd.shift, bd.key), bd.command);
            }
        }
        Keymap { chords }
    }

    /// 查命令。Win 键组合永远不命中。
    pub fn lookup(&self, input: &KeyInput<'_>) -> Option<AppCommand> {
        let chord = Chord::from_input(input)?;
        self.chords
            .iter()
            .find(|(c, _)| *c == chord)
            .map(|(_, cmd)| *cmd)
    }

    /// 命令的快捷键文字（菜单提示用），取第一个；没有返回 `None`。
    pub fn text(&self, command: AppCommand) -> Option<String> {
        self.chords
            .iter()
            .find(|(_, cmd)| *cmd == command)
            .map(|(c, _)| c.display())
    }
}

// ---- 全局生效表 ----

static CURRENT: RwLock<Option<Arc<Keymap>>> = RwLock::new(None);

/// 当前生效的键位表（没有安装过就是默认值）。
pub fn current() -> Arc<Keymap> {
    if let Some(k) = CURRENT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return k.clone();
    }
    let mut w = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    w.get_or_insert_with(|| Arc::new(Keymap::defaults()))
        .clone()
}

/// 安装新的键位表（启动时与改绑后调用）。
pub fn install(settings: &KeybindingSettings) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(settings.keymap()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(key: &'static str, ctrl: bool, alt: bool, shift: bool) -> KeyInput<'static> {
        KeyInput {
            key,
            key_char: None,
            ctrl,
            alt,
            shift,
            platform: false,
            is_held: false,
        }
    }

    #[test]
    fn storage_round_trip() {
        for text in [
            "ctrl+shift+\\",
            "alt+f3",
            "ctrl+alt+left",
            "ctrl+=",
            "ctrl+,",
            "f3",
        ] {
            let c = Chord::parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(c.storage(), text);
        }
        // 键本身是 `+`：靠修饰键白名单区分。
        let c = Chord::parse("ctrl++").unwrap();
        assert_eq!((c.ctrl, c.key.as_str()), (true, "+"));
        assert_eq!(c.storage(), "ctrl++");
    }

    #[test]
    fn parse_rejects_bad_text() {
        for bad in [
            "", "cmd+t", "ctrl+", "ctrl+ab", "ctrl+T", "ctrl+f99", "meta+x",
        ] {
            assert!(Chord::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn normalizes_shifted_symbols() {
        assert_eq!(
            Chord::new(true, false, true, "|"),
            Chord::new(true, false, true, "\\")
        );
        assert_eq!(Chord::new(true, false, true, "+").key, "=");
        // 没按 Shift 时不还原。
        assert_eq!(Chord::new(true, false, false, "+").key, "+");
        assert_eq!(Chord::new(false, true, false, "N").key, "n");
    }

    #[test]
    fn display_is_readable() {
        assert_eq!(Chord::new(true, false, true, "t").display(), "Ctrl+Shift+T");
        assert_eq!(
            Chord::new(true, true, false, "left").display(),
            "Ctrl+Alt+Left"
        );
        assert_eq!(Chord::new(false, false, false, "f3").display(), "F3");
    }

    #[test]
    fn validation_rules() {
        let v = |c: Chord| c.validate();
        assert_eq!(
            v(Chord::new(false, false, false, "a")),
            Err(Invalid::NeedsModifier)
        );
        assert_eq!(
            v(Chord::new(false, false, true, "a")),
            Err(Invalid::NeedsModifier)
        );
        assert_eq!(
            v(Chord::new(false, false, false, "left")),
            Err(Invalid::NeedsModifier)
        );
        assert_eq!(v(Chord::new(false, false, true, "left")), Ok(()));
        assert_eq!(v(Chord::new(false, false, false, "f5")), Ok(()));
        assert_eq!(v(Chord::new(true, false, false, "a")), Ok(()));
        assert_eq!(
            v(Chord::new(false, true, false, "3")),
            Err(Invalid::Reserved)
        );
        assert_eq!(v(Chord::new(true, false, true, "c")), Ok(()));
        assert_eq!(v(Chord::new(true, false, true, "v")), Ok(()));
        assert_eq!(v(Chord::new(false, false, true, "insert")), Ok(()));
        assert_eq!(
            v(Chord::new(false, true, false, "f4")),
            Err(Invalid::Reserved)
        );
    }

    #[test]
    fn record_handles_special_keys() {
        assert_eq!(
            record(&input("escape", false, false, false)),
            Recorded::Cancel
        );
        assert_eq!(
            record(&input("backspace", false, false, false)),
            Recorded::Clear
        );
        assert_eq!(
            record(&input("delete", false, false, false)),
            Recorded::Clear
        );
        // 带修饰键的 Esc / Backspace 是普通组合键。
        assert!(matches!(
            record(&input("escape", true, false, false)),
            Recorded::Chord(_)
        ));
        assert_eq!(
            record(&input("control", true, false, false)),
            Recorded::Ignore
        );
        let mut win = input("t", true, false, true);
        win.platform = true;
        assert_eq!(record(&win), Recorded::Ignore);
        assert_eq!(
            record(&input("t", true, false, true)),
            Recorded::Chord(Chord::new(true, false, true, "t"))
        );
    }

    #[test]
    fn catalog_has_unique_ids_and_covers_default_table() {
        let cat = catalog();
        for (i, a) in cat.iter().enumerate() {
            assert!(
                cat[i + 1..].iter().all(|b| b.id != a.id),
                "重复 id {}",
                a.id
            );
            assert!(cat[i + 1..].iter().all(|b| b.command != a.command));
        }
        for bd in DEFAULT_BINDINGS {
            if !matches!(bd.command, AppCommand::SelectTab(_)) {
                assert!(
                    cat.iter().any(|e| e.command == bd.command),
                    "{:?}",
                    bd.command
                );
            }
        }
        assert!(
            !cat.iter()
                .any(|e| matches!(e.command, AppCommand::SelectTab(_)))
        );
    }

    #[test]
    fn default_keymap_matches_default_table() {
        let km = Keymap::defaults();
        for bd in DEFAULT_BINDINGS {
            let i = KeyInput {
                key: bd.key,
                key_char: None,
                ctrl: bd.ctrl,
                alt: bd.alt,
                shift: bd.shift,
                platform: false,
                is_held: false,
            };
            assert_eq!(km.lookup(&i), Some(bd.command), "{bd:?}");
        }
        // Ctrl+Shift+= 在美式键盘上报 `+`，仍然是放大。
        assert_eq!(
            km.lookup(&input("+", true, false, true)),
            Some(AppCommand::ZoomIn)
        );
        assert_eq!(
            km.lookup(&input("|", true, false, true)),
            Some(AppCommand::SplitRight)
        );
    }

    #[test]
    fn default_catalog_chords_are_conflict_free_and_valid() {
        // 默认表里没有两个命令共用一个组合（目录合并后仍然成立）。
        let cat = catalog();
        for (i, a) in cat.iter().enumerate() {
            for c in &a.defaults {
                for b in &cat[i + 1..] {
                    assert!(!b.defaults.contains(c), "{} 与 {} 冲突", a.id, b.id);
                }
            }
        }
    }

    #[test]
    fn override_replaces_all_defaults_of_a_command() {
        let mut s = KeybindingSettings::default();
        let c = Chord::new(true, false, false, "j");
        assert_eq!(s.assign("nextSession", &c), AssignResult::Assigned);
        let km = s.keymap();
        assert_eq!(
            km.lookup(&input("j", true, false, false)),
            Some(AppCommand::NextTab)
        );
        // 原来的 Alt+] 与 Ctrl+Tab 都失效（不能新旧同时生效）。
        assert_eq!(km.lookup(&input("]", false, true, false)), None);
        assert_eq!(km.lookup(&input("tab", true, false, false)), None);
        assert_eq!(km.text(AppCommand::NextTab).as_deref(), Some("Ctrl+J"));
    }

    #[test]
    fn assign_to_default_removes_override() {
        let mut s = KeybindingSettings::default();
        s.assign("find", &Chord::new(true, false, false, "f"));
        assert!(s.is_customized("find"));
        s.assign("find", &Chord::new(true, false, true, "f"));
        assert!(!s.is_customized("find"));
    }

    #[test]
    fn conflict_detected_and_replaced() {
        let mut s = KeybindingSettings::default();
        // Ctrl+Shift+T 默认属于 newLocalShell。
        let c = Chord::new(true, false, true, "t");
        assert_eq!(
            s.assign("find", &c),
            AssignResult::Conflict("newLocalShell".into())
        );
        assert!(!s.is_customized("find"));
        assert_eq!(s.assign_replacing("find", &c), AssignResult::Assigned);
        let km = s.keymap();
        assert_eq!(
            km.lookup(&input("t", true, false, true)),
            Some(AppCommand::Find)
        );
        assert_eq!(
            s.overrides.get("newLocalShell").map(String::as_str),
            Some("")
        );
        assert_eq!(km.text(AppCommand::NewLocalTab), None);
    }

    #[test]
    fn replacing_one_alias_keeps_the_other() {
        let mut s = KeybindingSettings::default();
        // Ctrl+Tab 是 nextSession 的别名之一。
        let c = Chord::new(true, false, false, "tab");
        assert_eq!(s.assign_replacing("find", &c), AssignResult::Assigned);
        let km = s.keymap();
        assert_eq!(
            km.lookup(&input("tab", true, false, false)),
            Some(AppCommand::Find)
        );
        assert_eq!(
            km.lookup(&input("]", false, true, false)),
            Some(AppCommand::NextTab)
        );
    }

    #[test]
    fn clipboard_actions_drop_old_aliases_and_keep_menu_labels_in_sync() {
        let mut settings = KeybindingSettings::default();
        let shift_ins = Chord::new(false, false, true, "insert");
        assert_eq!(
            settings.assign("copy", &shift_ins),
            AssignResult::Conflict("paste".into())
        );
        assert_eq!(
            settings.assign_replacing("copy", &shift_ins),
            AssignResult::Assigned
        );
        settings.clear("selectAll");
        let keymap = KeybindingSettings::decode(Some(&settings.encode())).keymap();
        assert_eq!(
            keymap.lookup(&input("insert", false, false, true)),
            Some(AppCommand::Copy)
        );
        assert_eq!(
            keymap.lookup(&input("v", true, false, true)),
            Some(AppCommand::Paste)
        );
        assert_eq!(keymap.lookup(&input("c", true, false, true)), None);
        assert_eq!(keymap.lookup(&input("a", true, false, true)), None);
        assert_eq!(keymap.text(AppCommand::Copy), Some(shift_ins.display()));
        assert_eq!(
            keymap.text(AppCommand::Paste).as_deref(),
            Some("Ctrl+Shift+V")
        );
        settings.clear("paste");
        assert_eq!(settings.keymap().text(AppCommand::Paste), None);
        settings.restore_all();
        assert_eq!(
            settings.keymap().lookup(&input("a", true, false, true)),
            Some(AppCommand::SelectAll)
        );
    }

    #[test]
    fn invalid_and_reserved_do_not_change_anything() {
        let mut s = KeybindingSettings::default();
        assert_eq!(
            s.assign("find", &Chord::new(false, false, true, "a")),
            AssignResult::Invalid
        );
        assert_eq!(
            s.assign("find", &Chord::new(false, true, false, "5")),
            AssignResult::Reserved
        );
        assert_eq!(
            s.assign("nope", &Chord::new(true, false, false, "a")),
            AssignResult::Invalid
        );
        assert!(s.overrides.is_empty());
    }

    #[test]
    fn clear_restore_and_restore_all() {
        let mut s = KeybindingSettings::default();
        s.clear("clearBuffer");
        assert_eq!(s.keymap().lookup(&input("k", false, true, false)), None);
        s.assign("find", &Chord::new(true, false, false, "f"));
        s.restore("clearBuffer");
        assert_eq!(
            s.keymap().lookup(&input("k", false, true, false)),
            Some(AppCommand::ClearBuffer)
        );
        assert!(s.is_customized("find"));
        s.restore_all();
        assert!(s.overrides.is_empty());
        // 没有默认键的命令：清空等于默认，不留覆盖项。
        s.clear("commandSuggestions");
        assert!(s.overrides.is_empty());
    }

    #[test]
    fn unbound_by_default_command_can_be_bound() {
        let mut s = KeybindingSettings::default();
        assert_eq!(
            s.assign("commandSuggestions", &Chord::new(true, true, false, "s")),
            AssignResult::Assigned
        );
        assert_eq!(
            s.keymap().lookup(&input("s", true, true, false)),
            Some(AppCommand::ToggleCommandSuggestions)
        );
    }

    #[test]
    fn fixed_tab_shortcuts_survive_any_override() {
        let mut s = KeybindingSettings::default();
        s.clear("nextSession");
        assert_eq!(
            s.keymap().lookup(&input("1", false, true, false)),
            Some(AppCommand::SelectTab(0))
        );
    }

    #[test]
    fn serialization_matches_mac_format_and_is_lenient() {
        let mut s = KeybindingSettings::default();
        s.assign("find", &Chord::new(true, false, false, "f"));
        s.clear("clearBuffer");
        let json = s.encode();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["bindings"]["find"], "ctrl+f");
        assert_eq!(v["bindings"]["clearBuffer"], "");
        assert_eq!(KeybindingSettings::decode(Some(&json)), s);
        // 坏数据 / 缺失 / 未知字段 / 类型不对。
        assert_eq!(
            KeybindingSettings::decode(None),
            KeybindingSettings::default()
        );
        assert_eq!(
            KeybindingSettings::decode(Some("{bad")),
            KeybindingSettings::default()
        );
        let odd = KeybindingSettings::decode(Some(
            r#"{"v":9,"extra":1,"bindings":{"find":"ctrl+f","x":5,"y":null}}"#,
        ));
        assert_eq!(odd.overrides.len(), 1);
    }

    #[test]
    fn unparsable_override_falls_back_to_default() {
        let s = KeybindingSettings::decode(Some(r#"{"bindings":{"find":"cmd+f"}}"#));
        assert_eq!(
            s.keymap().lookup(&input("f", true, false, true)),
            Some(AppCommand::Find)
        );
    }

    #[test]
    fn duplicate_chords_from_foreign_settings_first_wins() {
        let s = KeybindingSettings::decode(Some(
            r#"{"bindings":{"newSession":"ctrl+j","find":"ctrl+j"}}"#,
        ));
        assert_eq!(
            s.keymap().lookup(&input("j", true, false, false)),
            Some(AppCommand::NewSession)
        );
    }

    #[test]
    fn every_catalog_entry_has_a_label() {
        for e in catalog() {
            assert!(!command_label(&e).is_empty());
            if e.group != Group::Other {
                // 登记过的命令不能退回 id。
                assert_ne!(command_label(&e), e.id, "{}", e.id);
            }
        }
        for g in Group::ALL {
            assert!(!group_label(g).is_empty());
        }
    }

    #[test]
    fn win_key_never_matches() {
        let mut i = input("t", true, false, true);
        i.platform = true;
        assert_eq!(Keymap::defaults().lookup(&i), None);
    }
}
