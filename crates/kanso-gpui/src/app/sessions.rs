//! 会话配置列表（启动器）与编辑模型。对照 `Sessions/SessionLauncherModel.swift`、
//! `SessionEditorModel.swift`、`SessionStore.swift`（ui-shell.md §4、§5，store-import.md §1–§3）。
//!
//! 校验规则（主机为空、跳板链成环 / 超过 8 跳等）以核心为准：本模块只把核心的错误原样交给视图，
//! 不替核心做校验，也不为互斥悄悄丢掉一边的值。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::iter::Peekable;
use std::str::Chars;

use kanso_core::{CoreError, DeleteImpact};
use kanso_secret::SecretKind;
use kanso_types::geometry::Rgb;
use kanso_types::mcp::McpAccess;
use kanso_types::message::Message;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, KeepaliveConfig, LocalProfile, Locale, Profile, ProfileKind,
    SessionKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::{GroupId, ProfileId};

use super::backend::{CoreResult, ProfileStore};

// ---- 排序与匹配 ----

/// 自然排序比较（忽略大小写，连续数字按数值比较，`host2` < `host10`），
/// 对应 macOS 的 `localizedStandardCompare`。完全相等时按原文兜底，保证排序稳定、刷新后不跳动。
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn digits(it: &mut Peekable<Chars<'_>>) -> String {
        let mut run = String::new();
        while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
            run.push(c);
            it.next();
        }
        run
    }
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(cx), Some(cy)) if cx.is_ascii_digit() && cy.is_ascii_digit() => {
                let (dx, dy) = (digits(&mut x), digits(&mut y));
                let (tx, ty) = (dx.trim_start_matches('0'), dy.trim_start_matches('0'));
                let ord = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(cx), Some(cy)) => {
                let ord = cx.to_lowercase().cmp(cy.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// 会话的 SSH / SFTP 连接目标；本地会话为 `None`。
pub fn ssh_target_of(profile: &Profile) -> Option<&SshTarget> {
    match &profile.kind {
        ProfileKind::Local(_) => None,
        ProfileKind::Ssh(s) => Some(&s.target),
        ProfileKind::Sftp(t) => Some(t),
    }
}

/// 名称 / 主机 / 用户名任一包含搜索词即匹配，不区分大小写；搜索词（去首尾空白后）为空匹配全部。
///
/// 与 mac 的差异：mac 还忽略变音符号，这里只做 Unicode 小写折叠。
pub fn profile_matches(profile: &Profile, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let target = ssh_target_of(profile);
    [
        Some(profile.name.as_str()),
        target.map(|t| t.host.as_str()),
        target.map(|t| t.user.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().contains(&needle))
}

/// 启动器行的副标题。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowSubtitle {
    /// SSH / SFTP：`user@host:port`（用户名为空时 `host:port`）。
    Target(String),
    /// 本地：自定义 shell 命令。
    ShellCommand(String),
    /// 本地：默认 shell（视图显示本地化的"默认 Shell"）。
    DefaultShell,
}

/// 行副标题。
pub fn row_subtitle(profile: &Profile) -> RowSubtitle {
    if let Some(t) = ssh_target_of(profile) {
        let host_port = if t.host.contains(':') {
            format!("[{}]:{}", t.host, t.port)
        } else {
            format!("{}:{}", t.host, t.port)
        };
        return RowSubtitle::Target(if t.user.is_empty() {
            host_port
        } else {
            format!("{}@{host_port}", t.user)
        });
    }
    if let ProfileKind::Local(l) = &profile.kind {
        let command = l.shell_command.trim();
        if !command.is_empty() {
            return RowSubtitle::ShellCommand(command.to_owned());
        }
    }
    RowSubtitle::DefaultShell
}

// ---- 启动器 ----

/// 启动器里的一个分组区：`group` 为 `None` 是未分组的会话（显示在最前，没有标题）。
#[derive(Clone, Debug, PartialEq)]
pub struct LauncherSection {
    /// 分组。
    pub group: Option<Group>,
    /// 当前是否折叠；搜索时匹配项临时展开。
    pub collapsed: bool,
    /// 可见的组内会话（按名称自然排序）。
    pub profiles: Vec<Profile>,
}

/// 会话启动器的模型：搜索、分组归类、选中项、分组与会话的增删改。
///
/// 打开时读取快照（[`reload`](Self::reload)）；搜索只过滤内存。所有写操作成功后自动重新读取。
pub struct LauncherModel<S: ProfileStore> {
    store: S,
    profiles: Vec<Profile>,
    groups: Vec<Group>,
    sections: Vec<LauncherSection>,
    collapsed_groups: HashSet<GroupId>,
    selected: Option<ProfileId>,
    query: String,
    has_any_profile: bool,
}

impl<S: ProfileStore> LauncherModel<S> {
    /// 读取快照并选中第一项。
    pub fn new(store: S) -> Self {
        Self::with_collapsed_groups(store, HashSet::new())
    }

    /// 打开启动器时沿用当前窗口记录的折叠状态。
    pub fn with_collapsed_groups(store: S, collapsed_groups: HashSet<GroupId>) -> Self {
        let mut m = LauncherModel {
            store,
            profiles: Vec::new(),
            groups: Vec::new(),
            sections: Vec::new(),
            collapsed_groups,
            selected: None,
            query: String::new(),
            has_any_profile: false,
        };
        m.reload();
        m.selected = m.visible_profiles().first().map(|p| p.id);
        m
    }

    /// 从核心重新读取并按当前搜索词归类。已选中的会话仍可见则保持选中，否则回到第一项。
    /// 外部也改了会话库（例如导入 OpenSSH 配置）后调用。
    pub fn reload(&mut self) {
        self.profiles = self.store.profiles();
        self.profiles.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        self.groups = self.store.groups();
        self.groups.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        self.has_any_profile = !self.profiles.is_empty();
        self.filter_snapshot();
    }

    fn filter_snapshot(&mut self) {
        let needle = self.query.trim().to_owned();
        let mut ungrouped = Vec::new();
        let mut buckets: Vec<Vec<Profile>> = vec![Vec::new(); self.groups.len()];
        for profile in self.profiles.iter().filter(|p| profile_matches(p, &needle)) {
            // 指向不存在分组的会话按未分组处理，不让它从界面上消失。
            match profile
                .group
                .and_then(|g| self.groups.iter().position(|x| x.id == g))
            {
                Some(i) => buckets[i].push(profile.clone()),
                None => ungrouped.push(profile.clone()),
            }
        }
        let mut sections = Vec::new();
        if !ungrouped.is_empty() {
            sections.push(LauncherSection {
                group: None,
                collapsed: false,
                profiles: ungrouped,
            });
        }
        for (group, members) in self.groups.iter().zip(buckets) {
            // 搜索时隐藏没有结果的分组；平时空分组也要显示，否则新建后无法重命名 / 删除。
            if members.is_empty() && !needle.is_empty() {
                continue;
            }
            let collapsed = needle.is_empty() && self.collapsed_groups.contains(&group.id);
            sections.push(LauncherSection {
                group: Some(group.clone()),
                collapsed,
                profiles: if collapsed { Vec::new() } else { members },
            });
        }
        self.sections = sections;
        if !self
            .visible_profiles()
            .iter()
            .any(|p| Some(p.id) == self.selected)
        {
            self.selected = self.visible_profiles().first().map(|p| p.id);
        }
    }

    // ---- 读 ----

    /// 分组区（从上到下）。
    pub fn sections(&self) -> &[LauncherSection] {
        &self.sections
    }

    /// 库里有没有任何会话（没有时视图显示空状态而不是"没有匹配"）。
    pub fn has_any_profile(&self) -> bool {
        self.has_any_profile
    }

    /// 屏幕上从上到下的会话顺序（键盘上下键按它移动）。
    pub fn visible_profiles(&self) -> Vec<&Profile> {
        self.sections
            .iter()
            .flat_map(|s| s.profiles.iter())
            .collect()
    }

    /// 按 id 取会话（来自快照）。
    pub fn profile(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// 全部分组（按名称排序），"移到分组"菜单用。
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// 搜索词。
    pub fn query(&self) -> &str {
        &self.query
    }

    /// 键盘选中的会话。
    pub fn selected(&self) -> Option<ProfileId> {
        self.selected
    }

    /// 关闭启动器时由窗口记住。搜索过程中保持用户的折叠设置。
    pub fn collapsed_groups(&self) -> &HashSet<GroupId> {
        &self.collapsed_groups
    }

    /// 切换分组，并同步更新键盘可选的会话。
    pub fn toggle_group(&mut self, id: GroupId) {
        if !self.collapsed_groups.remove(&id) {
            self.collapsed_groups.insert(id);
        }
        self.filter_snapshot();
    }

    // ---- 搜索与选中 ----

    /// 设置搜索词。变化后选中第一个结果：回车直接连接最相关的一项。
    pub fn set_query(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query = query.to_owned();
        self.filter_snapshot();
        self.selected = self.visible_profiles().first().map(|p| p.id);
    }

    /// 选中（`None` 取消选中）。
    pub fn select(&mut self, id: Option<ProfileId>) {
        self.selected = id;
    }

    /// 上下键移动选中，到头不循环。
    pub fn move_selection(&mut self, delta: isize) {
        let list: Vec<ProfileId> = self.visible_profiles().iter().map(|p| p.id).collect();
        if list.is_empty() {
            return;
        }
        let current = list
            .iter()
            .position(|id| Some(*id) == self.selected)
            .map_or(if delta > 0 { -1 } else { list.len() as isize }, |i| {
                i as isize
            });
        let next = (current + delta).clamp(0, list.len() as isize - 1);
        self.selected = Some(list[next as usize]);
    }

    // ---- 会话 ----

    /// 移到分组（`None` = 不分组）。其余字段原样写回。会话不存在时什么都不做。
    pub fn move_profile_to_group(
        &mut self,
        id: ProfileId,
        group: Option<GroupId>,
    ) -> CoreResult<()> {
        let Some(mut profile) = self.profile(id).cloned() else {
            return Ok(());
        };
        profile.group = group;
        self.store.save_profile(profile)?;
        self.reload();
        Ok(())
    }

    /// 删除会话（连同凭据）。
    pub async fn delete_profile(&mut self, id: ProfileId) -> CoreResult<()> {
        let result = self.store.delete_profile(id).await;
        self.reload();
        result
    }

    // ---- 分组 ----

    /// 新建分组，名称去掉首尾空白；空名称返回 `Ok(None)` 且不创建。
    pub fn create_group(&mut self, name: &str) -> CoreResult<Option<Group>> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        let group = Group {
            id: GroupId::new(),
            name: name.to_owned(),
        };
        self.store.save_group(group.clone())?;
        self.reload();
        Ok(Some(group))
    }

    /// 重命名分组；空名称或名称没变什么都不做，返回 false。
    pub fn rename_group(&mut self, group: &Group, name: &str) -> CoreResult<bool> {
        let name = name.trim();
        if name.is_empty() || name == group.name {
            return Ok(false);
        }
        self.store.save_group(Group {
            id: group.id,
            name: name.to_owned(),
        })?;
        self.reload();
        Ok(true)
    }

    /// 删除分组前的确认数据：会连带删除多少会话和命令（视图把数字代入确认文字）。
    pub fn delete_group_impact(&self, group: &Group) -> CoreResult<DeleteImpact> {
        self.store.group_delete_impact(group.id)
    }

    /// 删除分组（连同组内会话）。调用方应先用 [`delete_group_impact`](Self::delete_group_impact) 让用户确认。
    pub async fn delete_group(&mut self, group: &Group) -> CoreResult<()> {
        let result = self.store.delete_group(group.id).await;
        self.reload();
        result
    }
}

// ---- 编辑 ----

/// 会话颜色预设：色板里的 16 个固定 RGB（两行，每行 8 个）。`Profile::color` 存的就是固定 RGB，
/// 不随外观变化。已存会话按 RGB 值匹配选中项，调整顺序不影响数据。
pub const COLOR_PRESETS: [Rgb; 16] = [
    Rgb {
        r: 0x3D,
        g: 0x9D,
        b: 0xF2,
    }, // 蓝
    Rgb {
        r: 0x3F,
        g: 0xB9,
        b: 0x50,
    }, // 绿
    Rgb {
        r: 0xE8,
        g: 0xB5,
        b: 0x3C,
    }, // 琥珀
    Rgb {
        r: 0xF0,
        g: 0x48,
        b: 0x3E,
    }, // 红
    Rgb {
        r: 0xB0,
        g: 0x7E,
        b: 0xF5,
    }, // 紫
    Rgb {
        r: 0x2E,
        g: 0xC4,
        b: 0xCF,
    }, // 青
    Rgb {
        r: 0xF2,
        g: 0x6D,
        b: 0x62,
    }, // 珊瑚
    Rgb {
        r: 0x9C,
        g: 0xD0,
        b: 0xFF,
    }, // 浅蓝
    Rgb {
        r: 0xF5,
        g: 0x82,
        b: 0x2A,
    }, // 橙
    Rgb {
        r: 0xE0,
        g: 0x5A,
        b: 0x9C,
    }, // 粉
    Rgb {
        r: 0x7E,
        g: 0xE7,
        b: 0x87,
    }, // 浅绿
    Rgb {
        r: 0x6B,
        g: 0x7D,
        b: 0xFA,
    }, // 靛蓝
    Rgb {
        r: 0x1F,
        g: 0xB7,
        b: 0x9A,
    }, // 蓝绿
    Rgb {
        r: 0xF0,
        g: 0xBE,
        b: 0x6B,
    }, // 浅琥珀
    Rgb {
        r: 0xE3,
        g: 0x6C,
        b: 0xD0,
    }, // 洋红
    Rgb {
        r: 0xCD,
        g: 0xA6,
        b: 0xFA,
    }, // 薰衣草
];

/// 编码选项（encoding-transfer.md §1.1）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingOption {
    /// 保存到配置的 WHATWG 标签（小写；UTF-8 为空串）。
    pub label: String,
    /// 英文显示名；视图应按 `label` 查本地化文案，查不到再用它。
    pub title: String,
}

/// 固定的编码列表（label, 英文名）。
pub const ENCODINGS: [(&str, &str); 10] = [
    ("", "UTF-8 (Default)"),
    ("gb18030", "GB18030 — Simplified Chinese"),
    ("gbk", "GBK — Simplified Chinese"),
    ("big5", "Big5 — Traditional Chinese"),
    ("shift_jis", "Shift_JIS — Japanese"),
    ("euc-jp", "EUC-JP — Japanese"),
    ("euc-kr", "EUC-KR — Korean"),
    (
        "windows-1252",
        "Windows-1252 / ISO-8859-1 — Western European",
    ),
    ("windows-1251", "Windows-1251 — Cyrillic"),
    ("koi8-r", "KOI8-R — Cyrillic"),
];

/// 菜单选项：在固定列表之外，若当前值是手工编辑 / 导入带来的其他标签，也要列出来。
/// 否则弹出菜单选不中任何项，保存时会悄悄把它改成 UTF-8。
pub fn encoding_options(current: &str) -> Vec<EncodingOption> {
    let mut list: Vec<EncodingOption> = ENCODINGS
        .iter()
        .map(|(l, t)| EncodingOption {
            label: (*l).to_owned(),
            title: (*t).to_owned(),
        })
        .collect();
    if !list.iter().any(|o| o.label == current) {
        list.push(EncodingOption {
            label: current.to_owned(),
            title: current.to_owned(),
        });
    }
    list
}

/// 编辑对话框里可显隐的字段行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorField {
    /// 名称。
    Name,
    /// 类型。
    Kind,
    /// 颜色。
    Color,
    /// 分组。
    Group,
    /// MCP 访问级别。
    McpAccess,
    /// Shell 命令行（本地）。
    ShellCommand,
    /// 起始目录（本地）。
    StartDir,
    /// 主机。
    Host,
    /// 端口。
    Port,
    /// 用户名。
    User,
    /// 认证方式。
    Auth,
    /// 密码。
    Password,
    /// 私钥路径。
    KeyPath,
    /// 私钥口令。
    KeyPassphrase,
    /// 跳板机。
    Jump,
    /// ProxyCommand。
    ProxyCommand,
    /// 编码。
    Encoding,
    /// Locale。
    Locale,
    /// 协议层 keepalive 开关。
    KeepaliveEnabled,
    /// 协议层 keepalive 间隔。
    KeepaliveInterval,
    /// 连续无回应次数。
    KeepaliveMaxMisses,
    /// 空闲保活开关。
    IdleEnabled,
    /// 空闲保活间隔。
    IdleInterval,
    /// 空闲保活字符串。
    IdleString,
}

/// 保存失败 / 部分失败的原因（视图用 `CoreError::user_message` 本地化核心错误）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorError {
    /// 名称为空。
    NameEmpty,
    /// 端口不是 1–65535。
    PortInvalid,
    /// Locale 格式不合法（只允许 ASCII 字母数字和 `_ - . @`，长度 1..=64）。
    LocaleInvalid,
    /// 保活间隔不是 1–86400 之间的数字。
    KeepaliveIntervalInvalid,
    /// 连续无回应次数不是 1–65535 之间的数字。
    KeepaliveMaxMissesInvalid,
    /// 核心拒绝保存（校验失败等）。
    Save(CoreError),
    /// 会话已保存，但写凭据失败。MCP 授权也可能同时写失败。
    Credential {
        /// 写凭据的错误。
        error: CoreError,
        /// 写授权的错误。
        mcp_access: Option<Box<CoreError>>,
    },
    /// 会话已保存，但写 MCP 授权失败。
    McpAccess(CoreError),
}

impl EditorError {
    /// 可直接展示的核心消息（只有核心给出用户级错误时有）。
    pub fn core_message(&self) -> Option<&Message> {
        match self {
            EditorError::Save(e)
            | EditorError::Credential { error: e, .. }
            | EditorError::McpAccess(e) => e.user_message(),
            _ => None,
        }
    }
}

/// `save` 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 会话、凭据与授权已保存，对话框可以关闭。
    Saved,
    /// 校验或保存失败，`error()` 有原因；会话没有改动，对话框保持打开。
    Failed,
    /// 会话已保存，但写凭据失败；对话框保持打开让用户重试（重试会覆盖同一个 id，不会产生重复会话）。
    SavedButCredentialFailed,
    /// 会话已保存，但写 MCP 授权失败；对话框保持打开让用户重试。
    SavedButMcpAccessFailed,
}

/// 编辑模型的发行渠道相关选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorOptions {
    /// ProxyCommand 只在 Direct 渠道提供（沙盒渠道不允许启动任意命令，ssh.md §5）。
    pub supports_proxy_command: bool,
    /// 当前构建支持 MCP 服务，允许管理已保存会话的授权。
    pub supports_mcp: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        EditorOptions {
            supports_proxy_command: cfg!(feature = "direct"),
            supports_mcp: false,
        }
    }
}

/// 默认 SSH 端口文字。
pub const DEFAULT_PORT_TEXT: &str = "22";

/// 会话编辑对话框的模型：新建与编辑共用。
///
/// 职责：保存字段值、按类型 / 认证方式 / 渠道决定字段可见性、组装 `Profile`、先保存会话再写凭据。
pub struct SessionEditor<S: ProfileStore> {
    store: S,
    original: Option<Profile>,
    profile_id: ProfileId,
    options: EditorOptions,

    /// 名称。
    pub name: String,
    /// 类型。
    pub kind: SessionKind,
    /// 颜色；`None` 表示不着色。
    pub color: Option<Rgb>,
    /// 所属分组。
    pub group: Option<GroupId>,
    mcp_access: McpAccess,
    mcp_access_changed: bool,

    /// Shell 命令行（本地）。
    pub shell_command: String,
    /// 起始目录（本地）。
    pub start_dir: String,

    /// 主机。
    pub host: String,
    /// 端口输入框文字（用 [`sanitized_port_text`] 过滤输入）。
    pub port_text: String,
    /// 用户名。
    pub user: String,
    /// 认证方式。
    pub auth: AuthMethod,
    /// 新填写的密码；留空表示不修改已保存的密码（核心不会把已存密码回传给界面）。
    pub password: String,
    /// 私钥路径。
    pub key_path: String,
    /// 新填写的私钥口令；规则同 `password`。
    pub key_passphrase: String,
    /// 跳板机（已保存的 SSH 会话）。
    pub jump: Option<ProfileId>,
    /// ProxyCommand。
    pub proxy_command: String,

    /// 编码标签（UTF-8 为空串）。
    pub encoding: String,
    /// Locale（可选）。
    pub locale: String,

    /// 是否启用协议层 keepalive。
    pub keepalive_enabled: bool,
    /// 协议层 keepalive 间隔输入框文字（秒）。
    pub keepalive_interval_text: String,
    /// 连续无回应次数输入框文字。
    pub keepalive_max_misses_text: String,
    /// 是否启用空闲保活字符串。
    pub idle_enabled: bool,
    /// 空闲保活间隔输入框文字（秒）。
    pub idle_interval_text: String,
    /// 空闲时发送的字符串（可含 \r \n \t \\ 转义）。
    pub idle_string: String,

    error: Option<EditorError>,
}

/// 端口输入过滤：只保留 ASCII 数字，最多 5 位。
pub fn sanitized_port_text(text: &str) -> String {
    text.chars().filter(char::is_ascii_digit).take(5).collect()
}

/// 保活间隔（秒）：1..=86400。类型 `u16` 上限 65535，因此实际可填到 65535。
fn parse_keepalive_secs(text: &str) -> Option<u16> {
    text.trim()
        .parse::<u16>()
        .ok()
        .filter(|v| (1..=86400).contains(&u32::from(*v)))
}

impl<S: ProfileStore> SessionEditor<S> {
    /// 新建会话：id 在此一次性生成，保存后凭据写入失败再重试不会产生第二个会话。
    pub fn new(store: S, options: EditorOptions) -> Self {
        Self::build(store, options, None)
    }

    /// 编辑已保存的会话。
    pub fn editing(store: S, options: EditorOptions, profile: Profile) -> Self {
        Self::build(store, options, Some(profile))
    }

    fn build(store: S, options: EditorOptions, original: Option<Profile>) -> Self {
        let mcp_access = original
            .as_ref()
            .filter(|_| options.supports_mcp)
            .map_or(McpAccess::Off, |p| store.mcp_access(p.id));
        let keepalive = KeepaliveConfig::default();
        let mut e = SessionEditor {
            store,
            profile_id: original.as_ref().map_or_else(ProfileId::new, |p| p.id),
            options,
            name: String::new(),
            kind: SessionKind::Local,
            color: None,
            group: None,
            mcp_access,
            mcp_access_changed: false,
            shell_command: String::new(),
            start_dir: String::new(),
            host: String::new(),
            port_text: DEFAULT_PORT_TEXT.to_owned(),
            user: String::new(),
            auth: AuthMethod::Password,
            password: String::new(),
            key_path: String::new(),
            key_passphrase: String::new(),
            jump: None,
            proxy_command: String::new(),
            encoding: String::new(),
            locale: String::new(),
            keepalive_enabled: keepalive.enabled,
            keepalive_interval_text: keepalive.interval_secs.to_string(),
            keepalive_max_misses_text: keepalive.max_misses.to_string(),
            idle_enabled: keepalive.idle_enabled,
            idle_interval_text: keepalive.idle_interval_secs.to_string(),
            idle_string: keepalive.idle_string,
            error: None,
            original: None,
        };
        if let Some(p) = &original {
            e.name = p.name.clone();
            e.color = p.color;
            e.group = p.group;
            match &p.kind {
                ProfileKind::Local(l) => {
                    e.kind = SessionKind::Local;
                    e.shell_command = l.shell_command.clone();
                    e.start_dir = l.start_dir.clone().unwrap_or_default();
                    e.encoding = l.encoding.as_str().to_owned();
                    e.locale = l
                        .locale
                        .as_ref()
                        .map_or(String::new(), |x| x.as_str().to_owned());
                }
                ProfileKind::Ssh(s) => {
                    e.kind = SessionKind::Ssh;
                    e.load_target(&s.target);
                    e.encoding = s.encoding.as_str().to_owned();
                    e.locale = s
                        .locale
                        .as_ref()
                        .map_or(String::new(), |x| x.as_str().to_owned());
                }
                ProfileKind::Sftp(t) => {
                    e.kind = SessionKind::Sftp;
                    e.load_target(t);
                }
            }
        }
        e.original = original;
        e
    }

    fn load_target(&mut self, t: &SshTarget) {
        self.host = t.host.clone();
        self.port_text = t.port.to_string();
        self.user = t.user.clone();
        self.auth = t.auth;
        self.key_path = t.private_key_path.clone().unwrap_or_default();
        self.jump = t.jump;
        self.proxy_command = t.proxy_command.clone().unwrap_or_default();
        self.keepalive_enabled = t.keepalive.enabled;
        self.keepalive_interval_text = t.keepalive.interval_secs.to_string();
        self.keepalive_max_misses_text = t.keepalive.max_misses.to_string();
        self.idle_enabled = t.keepalive.idle_enabled;
        self.idle_interval_text = t.keepalive.idle_interval_secs.to_string();
        self.idle_string = t.keepalive.idle_string.clone();
    }

    /// 是否在编辑已保存的会话。
    pub fn is_editing(&self) -> bool {
        self.original.is_some()
    }

    /// 保存用的 id。
    pub fn profile_id(&self) -> ProfileId {
        self.profile_id
    }

    /// 最近一次保存失败的原因。
    pub fn error(&self) -> Option<&EditorError> {
        self.error.as_ref()
    }

    /// 编辑器显示的 MCP 访问级别。新建会话默认关闭。
    pub fn mcp_access(&self) -> McpAccess {
        self.mcp_access
    }

    /// 只记录用户主动修改的值，避免普通编辑覆盖设置页随后修改的授权。
    pub fn edit_mcp_access(&mut self, level: McpAccess) {
        self.mcp_access = level;
        self.mcp_access_changed = true;
    }

    // ---- 选项 ----

    /// 分组选项（按名称排序）。
    pub fn groups(&self) -> Vec<Group> {
        let mut g = self.store.groups();
        g.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        g
    }

    /// 跳板机候选：其他已保存的 SSH 会话（排除自己；SFTP / 本地不能当跳板）。
    pub fn jump_candidates(&self) -> Vec<Profile> {
        let mut list: Vec<Profile> = self
            .store
            .profiles()
            .into_iter()
            .filter(|p| p.id != self.profile_id && p.kind.kind() == SessionKind::Ssh)
            .collect();
        list.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        list
    }

    /// 编码选项（含当前值）。
    pub fn encoding_options(&self) -> Vec<EncodingOption> {
        encoding_options(&self.encoding)
    }

    /// 颜色选项：16 个预设；已有会话带着预设之外的颜色时也要保留，避免保存时丢失。
    /// "无颜色"通过再次点击已选色取消表达，不在这里。
    pub fn color_options(&self) -> Vec<Rgb> {
        let mut list = COLOR_PRESETS.to_vec();
        if let Some(c) = self.color
            && !list.contains(&c)
        {
            list.push(c);
        }
        list
    }

    // ---- 字段可见性与互斥 ----

    /// SSH 或 SFTP。
    pub fn is_ssh_like(&self) -> bool {
        matches!(self.kind, SessionKind::Ssh | SessionKind::Sftp)
    }

    /// 字段是否显示。
    pub fn is_visible(&self, field: EditorField) -> bool {
        use EditorField::*;
        match field {
            Name | Kind | Color | Group => true,
            McpAccess => self.options.supports_mcp,
            ShellCommand | StartDir => self.kind == SessionKind::Local,
            Host | Port | User | Auth | Jump => self.is_ssh_like(),
            Password => self.is_ssh_like() && self.auth == AuthMethod::Password,
            KeyPath | KeyPassphrase => self.is_ssh_like() && self.auth == AuthMethod::PrivateKey,
            ProxyCommand => self.is_ssh_like() && self.options.supports_proxy_command,
            Encoding | Locale => self.kind != SessionKind::Sftp,
            // 协议层三项 SSH / SFTP 都显示；空闲三项只有 SSH（SFTP 没有 shell channel）。
            KeepaliveEnabled | KeepaliveInterval | KeepaliveMaxMisses => self.is_ssh_like(),
            IdleEnabled | IdleInterval | IdleString => self.kind == SessionKind::Ssh,
        }
    }

    /// 选了跳板机就不能再填 ProxyCommand（ssh.md：二者只能选一个）。
    pub fn is_proxy_command_enabled(&self) -> bool {
        self.jump.is_none()
    }

    /// 填了 ProxyCommand 就不能再选跳板机。
    pub fn is_jump_enabled(&self) -> bool {
        self.proxy_command.trim().is_empty()
    }

    /// 合法端口（1–65535）；不合法为 `None`。
    pub fn port(&self) -> Option<u16> {
        self.port_text
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p >= 1)
    }

    // ---- 组装 ----

    /// 按当前字段组装要保存的 `Profile`。
    ///
    /// 不在这里替核心做主机为空、跳板链之类的校验；也不因互斥悄悄丢掉一边：
    /// 界面已禁用冲突输入，万一同时存在，交给核心报错比默默改数据更安全。
    pub fn assemble(&self) -> Result<Profile, EditorError> {
        let name = self.name.trim().to_owned();
        let locale = match self.locale.trim() {
            "" => None,
            text => Some(Locale::parse(text).ok_or(EditorError::LocaleInvalid)?),
        };
        let kind = match self.kind {
            SessionKind::Local => {
                let start = self.start_dir.trim();
                ProfileKind::Local(LocalProfile {
                    shell_command: self.shell_command.trim().to_owned(),
                    start_dir: (!start.is_empty()).then(|| start.to_owned()),
                    encoding: EncodingLabel::new(&self.encoding),
                    locale,
                })
            }
            SessionKind::Ssh => ProfileKind::Ssh(SshProfile {
                target: self.make_target()?,
                encoding: EncodingLabel::new(&self.encoding),
                locale,
            }),
            SessionKind::Sftp => ProfileKind::Sftp(self.make_target()?),
        };
        Ok(Profile {
            id: self.profile_id,
            name,
            color: self.color,
            group: self.group,
            kind,
        })
    }

    fn make_target(&self) -> Result<SshTarget, EditorError> {
        let port = self.port().ok_or(EditorError::PortInvalid)?;
        let key = self.key_path.trim();
        let proxy = self.proxy_command.trim();
        let keepalive_secs = parse_keepalive_secs(&self.keepalive_interval_text)
            .ok_or(EditorError::KeepaliveIntervalInvalid)?;
        let idle_secs = parse_keepalive_secs(&self.idle_interval_text)
            .ok_or(EditorError::KeepaliveIntervalInvalid)?;
        let max_misses = self
            .keepalive_max_misses_text
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|v| *v >= 1)
            .ok_or(EditorError::KeepaliveMaxMissesInvalid)?;
        Ok(SshTarget {
            host: self.host.trim().to_owned(),
            port,
            user: self.user.trim().to_owned(),
            auth: self.auth,
            // 私钥路径只对私钥认证有意义；其他认证方式不保留旧值，避免配置里残留无效路径。
            private_key_path: (self.auth == AuthMethod::PrivateKey && !key.is_empty())
                .then(|| key.to_owned()),
            jump: self.jump,
            // 非 Direct 渠道没有 ProxyCommand 输入，也不保存它。
            proxy_command: (self.options.supports_proxy_command && !proxy.is_empty())
                .then(|| proxy.to_owned()),
            keepalive: KeepaliveConfig {
                enabled: self.keepalive_enabled,
                interval_secs: keepalive_secs,
                max_misses,
                idle_enabled: self.idle_enabled,
                idle_interval_secs: idle_secs,
                idle_string: self.idle_string.clone(),
            },
        })
    }

    // ---- 保存 ----

    /// 保存会话，成功后再写授权和凭据。
    ///
    /// 顺序不能反：`set_credential` 要求会话已存在（核心会报 `NoSuchProfile`）。
    /// 密码 / 口令留空 = 不修改已保存的值，所以留空时一定不调用 `set_credential`。
    /// 凭据保存成功后清空内存里的明文。
    pub async fn save(&mut self) -> SaveOutcome {
        self.error = None;
        if self.name.trim().is_empty() {
            self.error = Some(EditorError::NameEmpty);
            return SaveOutcome::Failed;
        }
        let profile = match self.assemble() {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                return SaveOutcome::Failed;
            }
        };
        if let Err(e) = self.store.save_profile(profile) {
            self.error = Some(EditorError::Save(e));
            return SaveOutcome::Failed;
        }
        let mcp_error = if self.options.supports_mcp && self.mcp_access_changed {
            let result = self.store.set_mcp_access(self.profile_id, self.mcp_access);
            self.mcp_access_changed = result.is_err();
            result.err()
        } else {
            None
        };
        let credential = if !self.is_ssh_like() {
            None
        } else if self.auth == AuthMethod::Password && !self.password.is_empty() {
            Some((SecretKind::Password, self.password.clone()))
        } else if self.auth == AuthMethod::PrivateKey && !self.key_passphrase.is_empty() {
            Some((SecretKind::KeyPassphrase, self.key_passphrase.clone()))
        } else {
            None
        };
        if let Some((kind, secret)) = credential
            && let Err(e) = self
                .store
                .set_credential(self.profile_id, kind, SecretString::new(secret))
                .await
        {
            self.error = Some(EditorError::Credential {
                error: e,
                mcp_access: mcp_error.map(Box::new),
            });
            return SaveOutcome::SavedButCredentialFailed;
        }
        self.password.clear();
        self.key_passphrase.clear();
        if let Some(e) = mcp_error {
            self.error = Some(EditorError::McpAccess(e));
            return SaveOutcome::SavedButMcpAccessFailed;
        }
        SaveOutcome::Saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::backend::block_on;
    use crate::app::testutil::{FakeStore, group, local_profile, sftp_profile, ssh_profile};

    fn names(m: &LauncherModel<FakeStore>) -> Vec<String> {
        m.visible_profiles()
            .iter()
            .map(|p| p.name.clone())
            .collect()
    }

    // ---- 排序与匹配 ----

    #[test]
    fn natural_order_compares_numbers_by_value_and_ignores_case() {
        let mut v = vec![
            "host10", "Host2", "host1", "alpha", "Beta", "host02", "host2",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            vec![
                "alpha", "Beta", "host1", "Host2", "host02", "host2", "host10"
            ]
        );
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("a1", "a"), Ordering::Greater);
        // 超长数字不溢出。
        assert_eq!(
            natural_cmp("x99999999999999999999999", "x100000000000000000000000"),
            Ordering::Less
        );
        // 中文按字符比较，不 panic。
        let _ = natural_cmp("生产1", "生产2");
    }

    #[test]
    fn matching_name_host_user_case_insensitive() {
        let p = ssh_profile("Prod DB", "db.Example.com", "Admin");
        assert!(profile_matches(&p, ""));
        assert!(profile_matches(&p, "   "));
        assert!(profile_matches(&p, "prod"));
        assert!(profile_matches(&p, "EXAMPLE"));
        assert!(profile_matches(&p, "admin"));
        assert!(!profile_matches(&p, "staging"));
        let local = local_profile("shell", "/bin/zsh");
        assert!(profile_matches(&local, "SHE"));
        assert!(!profile_matches(&local, "zsh"), "本地 shell 命令不参与匹配");
    }

    #[test]
    fn row_subtitles() {
        assert_eq!(
            row_subtitle(&ssh_profile("a", "h", "u")),
            RowSubtitle::Target("u@h:22".into())
        );
        assert_eq!(
            row_subtitle(&ssh_profile("a", "h", "")),
            RowSubtitle::Target("h:22".into())
        );
        assert_eq!(
            row_subtitle(&sftp_profile("a", "::1", "u")),
            RowSubtitle::Target("u@[::1]:22".into())
        );
        assert_eq!(
            row_subtitle(&local_profile("l", " /bin/fish ")),
            RowSubtitle::ShellCommand("/bin/fish".into())
        );
        assert_eq!(
            row_subtitle(&local_profile("l", "  ")),
            RowSubtitle::DefaultShell
        );
    }

    // ---- 启动器 ----

    fn launcher() -> (LauncherModel<FakeStore>, FakeStore, Group, Group) {
        let g_prod = group("Prod");
        let g_dev = group("dev");
        let mut a = ssh_profile("web10", "w10.prod", "root");
        a.group = Some(g_prod.id);
        let mut b = ssh_profile("web2", "w2.prod", "root");
        b.group = Some(g_prod.id);
        let c = local_profile("scratch", "");
        let d = sftp_profile("files", "ftp.corp", "ops");
        let store = FakeStore::with(vec![a, b, c, d], vec![g_prod.clone(), g_dev.clone()]);
        (LauncherModel::new(store.clone()), store, g_prod, g_dev)
    }

    #[test]
    fn sections_ungrouped_first_then_groups_sorted_with_empty_groups() {
        let (m, _, g_prod, g_dev) = launcher();
        let s = m.sections();
        assert_eq!(s.len(), 3);
        assert!(s[0].group.is_none());
        assert_eq!(
            s[0].profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["files", "scratch"]
        );
        assert_eq!(
            s[1].group.as_ref().unwrap().id,
            g_dev.id,
            "dev 排在 Prod 前（忽略大小写）"
        );
        assert!(s[1].profiles.is_empty(), "平时空分组也显示");
        assert_eq!(s[2].group.as_ref().unwrap().id, g_prod.id);
        assert_eq!(
            s[2].profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["web2", "web10"],
            "自然排序"
        );
        assert!(m.has_any_profile());
        assert_eq!(
            m.selected(),
            Some(m.visible_profiles()[0].id),
            "初始选中第一项"
        );
    }

    #[test]
    fn search_filters_hides_empty_groups_and_selects_first_result() {
        let (mut m, _, g_prod, _) = launcher();
        m.set_query("web");
        assert_eq!(names(&m), vec!["web2", "web10"]);
        assert_eq!(m.sections().len(), 1, "搜索时隐藏没有结果的分组与未分组区");
        assert_eq!(m.sections()[0].group.as_ref().unwrap().id, g_prod.id);
        assert_eq!(m.selected(), Some(m.visible_profiles()[0].id));
        m.set_query("ops"); // 按用户名匹配
        assert_eq!(names(&m), vec!["files"]);
        m.set_query("zzz");
        assert!(m.visible_profiles().is_empty());
        assert_eq!(m.selected(), None);
        assert!(m.has_any_profile(), "没有结果不等于库为空");
        m.set_query("");
        assert_eq!(m.visible_profiles().len(), 4);
    }

    #[test]
    fn selection_moves_and_stops_at_ends() {
        let (mut m, _, _, _) = launcher();
        let order: Vec<ProfileId> = m.visible_profiles().iter().map(|p| p.id).collect();
        m.move_selection(-1);
        assert_eq!(m.selected(), Some(order[0]), "到头不循环");
        m.move_selection(1);
        m.move_selection(1);
        assert_eq!(m.selected(), Some(order[2]));
        for _ in 0..10 {
            m.move_selection(1);
        }
        assert_eq!(m.selected(), Some(*order.last().unwrap()));
        m.select(None);
        m.move_selection(1);
        assert_eq!(m.selected(), Some(order[0]), "无选中时向下选第一项");
        m.select(None);
        m.move_selection(-1);
        assert_eq!(
            m.selected(),
            Some(*order.last().unwrap()),
            "无选中时向上选最后一项"
        );
        let empty = LauncherModel::new(FakeStore::default());
        let mut empty = empty;
        empty.move_selection(1);
        assert_eq!(empty.selected(), None);
        assert!(!empty.has_any_profile());
    }

    #[test]
    fn selection_survives_reload_when_still_visible() {
        let (mut m, store, _, _) = launcher();
        let target = m.visible_profiles()[2].id;
        m.select(Some(target));
        m.reload();
        assert_eq!(m.selected(), Some(target));
        store.0.borrow_mut().profiles.retain(|p| p.id != target);
        m.reload();
        assert_eq!(
            m.selected(),
            Some(m.visible_profiles()[0].id),
            "选中项消失后回到第一项"
        );
    }

    #[test]
    fn dangling_group_reference_shows_as_ungrouped() {
        let mut p = ssh_profile("orphan", "h", "u");
        p.group = Some(GroupId::new());
        let m = LauncherModel::new(FakeStore::with(vec![p], vec![]));
        assert!(m.sections()[0].group.is_none());
        assert_eq!(names(&m), vec!["orphan"]);
    }

    #[test]
    fn move_profile_to_group_writes_back_other_fields_unchanged() {
        let (mut m, store, _, g_dev) = launcher();
        let target = m.visible_profiles()[0].clone();
        m.move_profile_to_group(target.id, Some(g_dev.id)).unwrap();
        let saved = store.0.borrow().saved.last().cloned().unwrap();
        assert_eq!(
            saved,
            Profile {
                group: Some(g_dev.id),
                ..target.clone()
            }
        );
        assert_eq!(
            m.sections()
                .iter()
                .find(|s| s.group.as_ref().is_some_and(|g| g.id == g_dev.id))
                .unwrap()
                .profiles
                .len(),
            1
        );
        m.move_profile_to_group(target.id, None).unwrap();
        assert!(m.profile(target.id).unwrap().group.is_none());
        let before = store.0.borrow().saved.len();
        m.move_profile_to_group(ProfileId::new(), None).unwrap();
        assert_eq!(store.0.borrow().saved.len(), before, "不存在的会话不写");
    }

    #[test]
    fn delete_profile_reloads() {
        let (mut m, store, _, _) = launcher();
        let id = m.visible_profiles()[0].id;
        block_on(m.delete_profile(id)).unwrap();
        assert_eq!(store.0.borrow().deleted_profiles, vec![id]);
        assert_eq!(m.visible_profiles().len(), 3);
        assert!(m.profile(id).is_none());
        assert_eq!(
            block_on(m.delete_profile(id)),
            Err(CoreError::NoSuchProfile(id))
        );
    }

    #[test]
    fn group_create_rename_delete() {
        let (mut m, store, g_prod, _) = launcher();
        assert_eq!(m.create_group("   ").unwrap(), None);
        let g = m.create_group("  Ops  ").unwrap().unwrap();
        assert_eq!(g.name, "Ops");
        assert!(m.groups().iter().any(|x| x.id == g.id));
        assert!(!m.rename_group(&g, "Ops").unwrap(), "名称没变不写");
        assert!(!m.rename_group(&g, "  ").unwrap());
        assert!(m.rename_group(&g, " Infra ").unwrap());
        assert!(m.groups().iter().any(|x| x.name == "Infra"));

        // 删除前的确认数据来自核心。
        let impact = m.delete_group_impact(&g_prod).unwrap();
        assert_eq!(
            impact,
            DeleteImpact {
                profiles: 2,
                snippets: 0
            }
        );
        block_on(m.delete_group(&g_prod)).unwrap();
        assert_eq!(store.0.borrow().deleted_groups, vec![g_prod.id]);
        assert_eq!(m.visible_profiles().len(), 2, "组内会话一并删除");
        assert!(m.delete_group_impact(&g_prod).is_err());
    }

    // ---- 编辑 ----

    fn opts(proxy: bool) -> EditorOptions {
        EditorOptions {
            supports_proxy_command: proxy,
            supports_mcp: false,
        }
    }

    fn ssh_editor(store: &FakeStore) -> SessionEditor<FakeStore> {
        let mut e = SessionEditor::new(store.clone(), opts(true));
        e.name = "  box  ".into();
        e.kind = SessionKind::Ssh;
        e.host = " h.example ".into();
        e.user = " me ".into();
        e
    }

    #[test]
    fn field_visibility_follows_kind_auth_and_channel() {
        use EditorField::*;
        let store = FakeStore::default();
        let mut e = SessionEditor::new(store.clone(), opts(true));
        assert!(e.is_visible(ShellCommand) && e.is_visible(StartDir) && !e.is_visible(Host));
        assert!(e.is_visible(Encoding) && e.is_visible(Locale));
        e.kind = SessionKind::Ssh;
        assert!(
            e.is_visible(Host)
                && e.is_visible(Port)
                && e.is_visible(Jump)
                && e.is_visible(ProxyCommand)
        );
        assert!(e.is_visible(Password) && !e.is_visible(KeyPath) && !e.is_visible(ShellCommand));
        e.auth = AuthMethod::PrivateKey;
        assert!(!e.is_visible(Password) && e.is_visible(KeyPath) && e.is_visible(KeyPassphrase));
        e.auth = AuthMethod::Agent;
        assert!(!e.is_visible(Password) && !e.is_visible(KeyPath));
        e.kind = SessionKind::Sftp;
        assert!(!e.is_visible(Encoding) && !e.is_visible(Locale) && e.is_visible(Host));
        let mas = SessionEditor::new(store, opts(false));
        let mut mas = mas;
        mas.kind = SessionKind::Ssh;
        assert!(!mas.is_visible(ProxyCommand), "沙盒渠道没有 ProxyCommand");
    }

    #[test]
    fn keepalive_fields_visibility_and_roundtrip() {
        use EditorField::*;
        let mut e = ssh_editor(&FakeStore::default());
        assert!(
            e.is_visible(KeepaliveEnabled)
                && e.is_visible(KeepaliveInterval)
                && e.is_visible(KeepaliveMaxMisses)
        );
        assert!(
            e.is_visible(IdleEnabled) && e.is_visible(IdleInterval) && e.is_visible(IdleString)
        );
        e.kind = SessionKind::Sftp;
        assert!(e.is_visible(KeepaliveEnabled), "SFTP 也有 SSH 协议层保活");
        assert!(!e.is_visible(IdleEnabled), "SFTP 没有 shell channel");

        let mut e = ssh_editor(&FakeStore::default());
        e.keepalive_enabled = false;
        e.keepalive_interval_text = "120".into();
        e.keepalive_max_misses_text = "5".into();
        e.idle_enabled = true;
        e.idle_interval_text = "600".into();
        e.idle_string = r"\n".into();
        let p = e.assemble().unwrap();
        let ProfileKind::Ssh(s) = &p.kind else {
            unreachable!()
        };
        assert!(!s.target.keepalive.enabled);
        assert_eq!(s.target.keepalive.interval_secs, 120);
        assert_eq!(s.target.keepalive.max_misses, 5);
        assert!(s.target.keepalive.idle_enabled);
        assert_eq!(s.target.keepalive.idle_interval_secs, 600);
        assert_eq!(s.target.keepalive.idle_string, r"\n");

        let e2 = SessionEditor::editing(FakeStore::default(), opts(true), p);
        assert!(!e2.keepalive_enabled);
        assert_eq!(e2.keepalive_interval_text, "120");
        assert_eq!(e2.idle_string, r"\n");
    }

    #[test]
    fn jump_and_proxy_command_are_mutually_exclusive_in_ui() {
        let mut e = ssh_editor(&FakeStore::default());
        assert!(e.is_proxy_command_enabled() && e.is_jump_enabled());
        e.jump = Some(ProfileId::new());
        assert!(!e.is_proxy_command_enabled() && e.is_jump_enabled());
        e.jump = None;
        e.proxy_command = "  ".into();
        assert!(e.is_jump_enabled(), "纯空白不算填了");
        e.proxy_command = "nc %h %p".into();
        assert!(!e.is_jump_enabled());
    }

    #[test]
    fn port_parsing_and_sanitizing() {
        let mut e = ssh_editor(&FakeStore::default());
        for (text, want) in [
            ("22", Some(22)),
            (" 2222 ", Some(2222)),
            ("65535", Some(65535)),
            ("0", None),
            ("65536", None),
            ("", None),
            ("2x", None),
        ] {
            e.port_text = text.into();
            assert_eq!(e.port(), want, "{text:?}");
        }
        assert_eq!(sanitized_port_text("2a2b2"), "222");
        assert_eq!(sanitized_port_text("1234567"), "12345");
        assert_eq!(sanitized_port_text("٣٤"), "", "只保留 ASCII 数字");
    }

    #[test]
    fn assemble_ssh_trims_and_drops_irrelevant_fields() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        e.auth = AuthMethod::Password;
        e.key_path = "/old/key".into(); // 密码认证不保留私钥路径
        e.proxy_command = "  ssh -W %h:%p gw ".into();
        e.encoding = "gbk".into();
        e.locale = " zh_CN.UTF-8 ".into();
        e.color = Some(COLOR_PRESETS[3]);
        let p = e.assemble().unwrap();
        assert_eq!(p.name, "box");
        assert_eq!(p.color, Some(COLOR_PRESETS[3]));
        let ProfileKind::Ssh(s) = &p.kind else {
            panic!()
        };
        assert_eq!(s.target.host, "h.example");
        assert_eq!(s.target.user, "me");
        assert_eq!(s.target.private_key_path, None);
        assert_eq!(s.target.proxy_command.as_deref(), Some("ssh -W %h:%p gw"));
        assert_eq!(s.encoding.as_str(), "gbk");
        assert_eq!(s.locale.as_ref().unwrap().as_str(), "zh_CN.UTF-8");

        e.auth = AuthMethod::PrivateKey;
        let ProfileKind::Ssh(s) = e.assemble().unwrap().kind else {
            panic!()
        };
        assert_eq!(s.target.private_key_path.as_deref(), Some("/old/key"));

        let mut mas = SessionEditor::new(store, opts(false));
        mas.kind = SessionKind::Ssh;
        mas.name = "x".into();
        mas.proxy_command = "nc".into();
        let ProfileKind::Ssh(s) = mas.assemble().unwrap().kind else {
            panic!()
        };
        assert_eq!(
            s.target.proxy_command, None,
            "非 Direct 渠道不保存 ProxyCommand"
        );
    }

    #[test]
    fn assemble_keeps_jump_and_proxy_both_for_core_to_reject() {
        let mut e = ssh_editor(&FakeStore::default());
        e.jump = Some(ProfileId::new());
        e.proxy_command = "nc".into();
        let ProfileKind::Ssh(s) = e.assemble().unwrap().kind else {
            panic!()
        };
        assert!(s.target.jump.is_some() && s.target.proxy_command.is_some());
    }

    #[test]
    fn assemble_local_and_sftp() {
        let mut e = SessionEditor::new(FakeStore::default(), opts(true));
        e.name = "l".into();
        e.shell_command = " /bin/zsh -l ".into();
        e.start_dir = " ".into();
        let ProfileKind::Local(l) = e.assemble().unwrap().kind else {
            panic!()
        };
        assert_eq!(l.shell_command, "/bin/zsh -l");
        assert_eq!(l.start_dir, None);
        assert!(l.encoding.is_utf8() && l.locale.is_none());

        e.kind = SessionKind::Sftp;
        e.host = "h".into();
        e.encoding = "gbk".into();
        assert!(
            matches!(e.assemble().unwrap().kind, ProfileKind::Sftp(_)),
            "SFTP 没有编码"
        );
    }

    #[test]
    fn assemble_rejects_bad_port_and_locale() {
        let mut e = ssh_editor(&FakeStore::default());
        e.port_text = "0".into();
        assert_eq!(e.assemble(), Err(EditorError::PortInvalid));
        e.port_text = "22".into();
        e.locale = "zh CN;rm -rf".into();
        assert_eq!(e.assemble(), Err(EditorError::LocaleInvalid));
        // 本地会话不检查端口。
        let mut l = SessionEditor::new(FakeStore::default(), opts(true));
        l.port_text = "abc".into();
        l.name = "x".into();
        assert!(l.assemble().is_ok());
    }

    #[test]
    fn editing_loads_all_fields_and_keeps_id() {
        let mut p = ssh_profile("prod", "h", "u");
        p.color = Some(Rgb { r: 9, g: 9, b: 9 });
        p.group = Some(GroupId::new());
        if let ProfileKind::Ssh(s) = &mut p.kind {
            s.target.port = 2200;
            s.target.auth = AuthMethod::PrivateKey;
            s.target.private_key_path = Some("/k".into());
            s.target.jump = Some(ProfileId::new());
            s.encoding = EncodingLabel::new("big5");
            s.locale = Locale::parse("en_US.UTF-8");
        }
        let e = SessionEditor::editing(FakeStore::default(), opts(true), p.clone());
        assert!(e.is_editing());
        assert_eq!(e.profile_id(), p.id);
        assert_eq!(e.port_text, "2200");
        assert_eq!(e.key_path, "/k");
        assert_eq!(e.encoding, "big5");
        assert_eq!(
            e.assemble().unwrap(),
            p,
            "不改任何字段，组装结果与原配置一致"
        );
        assert!(
            e.color_options().contains(&Rgb { r: 9, g: 9, b: 9 }),
            "预设之外的颜色保留"
        );
        assert_eq!(e.color_options().len(), 17);
    }

    #[test]
    fn new_editor_generates_id_once() {
        let e = SessionEditor::new(FakeStore::default(), opts(true));
        assert!(!e.is_editing());
        assert_eq!(e.port_text, "22");
        assert_eq!(e.profile_id(), e.profile_id());
        assert_eq!(e.color_options().len(), 16);
    }

    #[test]
    fn encoding_options_include_unknown_current() {
        assert_eq!(encoding_options("").len(), 10);
        let with = encoding_options("iso-8859-2");
        assert_eq!(with.len(), 11);
        assert_eq!(with.last().unwrap().label, "iso-8859-2");
        assert_eq!(ENCODINGS[0].0, "", "UTF-8 存为空串");
    }

    #[test]
    fn candidates_exclude_self_non_ssh_and_are_sorted() {
        let a = ssh_profile("b-jump", "j1", "u");
        let b = ssh_profile("a-jump", "j2", "u");
        let c = sftp_profile("sftp", "s", "u");
        let d = local_profile("local", "");
        let store = FakeStore::with(
            vec![a.clone(), b.clone(), c, d],
            vec![group("z"), group("A")],
        );
        let e = SessionEditor::editing(store.clone(), opts(true), a.clone());
        assert_eq!(
            e.jump_candidates()
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a-jump"]
        );
        assert_eq!(
            e.groups()
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "z"]
        );
        let n = SessionEditor::new(store, opts(true));
        assert_eq!(n.jump_candidates().len(), 2);
    }

    #[test]
    fn save_validates_name_before_anything_else() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        e.name = "   ".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Failed);
        assert_eq!(e.error(), Some(&EditorError::NameEmpty));
        assert!(store.0.borrow().saved.is_empty());
        e.name = "ok".into();
        e.port_text = "99999".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Failed);
        assert_eq!(e.error(), Some(&EditorError::PortInvalid));
        assert!(store.0.borrow().saved.is_empty());
    }

    #[test]
    fn save_stores_profile_then_credential() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        e.password = "s3cret".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        let i = store.0.borrow();
        assert_eq!(i.saved.len(), 1);
        assert_eq!(
            i.credentials,
            vec![(e.profile_id(), SecretKind::Password, "s3cret".to_owned())]
        );
        drop(i);
        assert!(e.password.is_empty(), "成功后清空明文");
        assert_eq!(e.error(), None);
    }

    #[test]
    fn empty_password_does_not_touch_stored_credential() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        assert!(
            store.0.borrow().credentials.is_empty(),
            "留空 = 不修改已保存的密码"
        );
    }

    #[test]
    fn passphrase_only_for_private_key_auth() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        e.auth = AuthMethod::PrivateKey;
        e.key_path = "/k".into();
        e.password = "ignored".into(); // 私钥认证下残留的密码不写
        e.key_passphrase = "pp".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        assert_eq!(
            store.0.borrow().credentials,
            vec![(e.profile_id(), SecretKind::KeyPassphrase, "pp".to_owned())]
        );
        // 本地会话从不写凭据。
        let mut l = SessionEditor::new(store.clone(), opts(true));
        l.name = "l".into();
        l.password = "x".into();
        l.key_passphrase = "y".into();
        assert_eq!(block_on(l.save()), SaveOutcome::Saved);
        assert_eq!(store.0.borrow().credentials.len(), 1);
    }

    #[test]
    fn core_rejection_is_reported_and_nothing_else_happens() {
        let store = FakeStore::default();
        store.0.borrow_mut().fail_save = Some(CoreError::User(Message::ProfileHostEmpty {}));
        let mut e = ssh_editor(&store);
        e.password = "p".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Failed);
        assert_eq!(e.core_error_key(), Some("profile-host-empty".to_owned()));
        assert!(
            store.0.borrow().credentials.is_empty(),
            "会话没存成就不写凭据"
        );
        assert_eq!(e.password, "p", "失败时保留输入");
    }

    #[test]
    fn credential_failure_keeps_dialog_open_and_retry_reuses_id() {
        let store = FakeStore::default();
        store.0.borrow_mut().fail_credential = true;
        let mut e = ssh_editor(&store);
        e.password = "p".into();
        assert_eq!(block_on(e.save()), SaveOutcome::SavedButCredentialFailed);
        assert!(matches!(e.error(), Some(EditorError::Credential { .. })));
        assert_eq!(e.password, "p", "保留输入以便重试");
        store.0.borrow_mut().fail_credential = false;
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        let i = store.0.borrow();
        assert_eq!(i.profiles.len(), 1, "重试覆盖同一个 id，不产生重复会话");
        assert_eq!(i.saved.len(), 2);
        assert_eq!(i.saved[0].id, i.saved[1].id);
    }

    #[test]
    fn grants_are_visible_for_mcp_supported_builds_and_written_after_creation() {
        let store = FakeStore::default();
        let mut options = opts(true);
        assert!(!SessionEditor::new(store.clone(), options).is_visible(EditorField::McpAccess));
        options.supports_mcp = true;
        let mut e = SessionEditor::new(store.clone(), options);
        assert!(e.is_visible(EditorField::McpAccess));
        assert_eq!(e.mcp_access(), McpAccess::Off);
        e.edit_mcp_access(McpAccess::ReadOnly);
        assert_eq!(store.mcp_access(e.profile_id()), McpAccess::Off);
        e.name = "local".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        assert_eq!(store.mcp_access(e.profile_id()), McpAccess::ReadOnly);
    }

    #[test]
    fn untouched_grant_does_not_overwrite_an_external_update() {
        let p = local_profile("local", "");
        let store = FakeStore::with(vec![p.clone()], vec![]);
        store.set_mcp_access(p.id, McpAccess::Full).unwrap();
        let mut e = SessionEditor::editing(
            store.clone(),
            EditorOptions {
                supports_mcp: true,
                ..opts(true)
            },
            p.clone(),
        );
        assert_eq!(e.mcp_access(), McpAccess::Full);
        store.set_mcp_access(p.id, McpAccess::ReadOnly).unwrap();
        e.name = "renamed".into();
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        assert_eq!(store.mcp_access(p.id), McpAccess::ReadOnly);
        e.edit_mcp_access(McpAccess::Off);
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        assert_eq!(store.mcp_access(p.id), McpAccess::Off);
    }

    #[test]
    fn retries_save_the_pending_grant_without_rewriting_a_saved_credential() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        e.options.supports_mcp = true;
        e.password = "p".into();
        e.edit_mcp_access(McpAccess::ReadOnly);
        {
            let mut i = store.0.borrow_mut();
            i.fail_mcp_access = true;
            i.fail_credential = true;
        }
        assert_eq!(block_on(e.save()), SaveOutcome::SavedButCredentialFailed);
        assert!(matches!(
            e.error(),
            Some(EditorError::Credential {
                mcp_access: Some(_),
                ..
            })
        ));
        assert_eq!(e.password, "p");
        store.0.borrow_mut().fail_credential = false;
        assert_eq!(block_on(e.save()), SaveOutcome::SavedButMcpAccessFailed);
        assert!(matches!(e.error(), Some(EditorError::McpAccess(_))));
        assert!(e.password.is_empty());
        store.0.borrow_mut().fail_mcp_access = false;
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        assert_eq!(store.mcp_access(e.profile_id()), McpAccess::ReadOnly);
        let i = store.0.borrow();
        assert_eq!(i.profiles.len(), 1);
        assert_eq!(i.credentials.len(), 1);
    }

    #[test]
    fn editor_roundtrip_through_launcher() {
        let store = FakeStore::default();
        let mut e = ssh_editor(&store);
        assert_eq!(block_on(e.save()), SaveOutcome::Saved);
        let mut m = LauncherModel::new(store.clone());
        assert_eq!(names(&m), vec!["box"]);
        let mut edit = SessionEditor::editing(store, opts(true), m.visible_profiles()[0].clone());
        edit.name = "box2".into();
        assert_eq!(block_on(edit.save()), SaveOutcome::Saved);
        m.reload();
        assert_eq!(names(&m), vec!["box2"]);
    }

    impl<S: ProfileStore> SessionEditor<S> {
        fn core_error_key(&self) -> Option<String> {
            self.error()
                .and_then(EditorError::core_message)
                .map(|m| m.key().to_owned())
        }
    }
}
