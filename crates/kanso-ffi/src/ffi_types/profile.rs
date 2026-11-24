//! Profile、分组、Sender 命令及相关几何类型的镜像。

use kanso_types::geometry::{CellSide, GridSize, Rgb, ViewportPoint};
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, KeepaliveConfig, LineEnding, LocalProfile, Locale, Profile,
    ProfileKind, SessionKind, Snippet, SnippetKindScope, SnippetScope, SshProfile, SshTarget,
};

use super::{FfiCoreError, mirror_enum, parse_id};

mirror_enum! {
    /// 会话种类。
    FfiSessionKind <=> SessionKind {
        /// 本地 shell。
        Local,
        /// SSH 终端。
        Ssh,
        /// 纯 SFTP。
        Sftp,
    }
}

mirror_enum! {
    /// SSH 认证方式。
    FfiAuthMethod <=> AuthMethod {
        /// 密码。
        Password,
        /// 私钥文件。
        PrivateKey,
        /// ssh-agent。
        Agent,
    }
}

mirror_enum! {
    /// Sender 命令的行尾。
    FfiLineEnding <=> LineEnding {
        /// 不附加。
        None,
        /// `\n`。
        Lf,
        /// `\r\n`。
        CrLf,
    }
}

mirror_enum! {
    /// Sender 命令按会话种类划分作用域时的种类。
    FfiSnippetKindScope <=> SnippetKindScope {
        /// 本地会话。
        Local,
        /// SSH 会话。
        Ssh,
    }
}

mirror_enum! {
    /// 光标落在单元格的左半还是右半。
    FfiCellSide <=> CellSide {
        /// 左半。
        Left,
        /// 右半。
        Right,
    }
}

/// 终端网格尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiGridSize {
    /// 列数。
    pub cols: u16,
    /// 行数。
    pub rows: u16,
}

impl From<GridSize> for FfiGridSize {
    fn from(g: GridSize) -> Self {
        FfiGridSize {
            cols: g.cols,
            rows: g.rows,
        }
    }
}

impl From<FfiGridSize> for GridSize {
    fn from(g: FfiGridSize) -> Self {
        GridSize {
            cols: g.cols,
            rows: g.rows,
        }
    }
}

/// 视口内的坐标（相对可见区域，不是绝对行号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiViewportPoint {
    /// 列号，从 0 起。
    pub col: u16,
    /// 视口内行号，从 0 起。
    pub row: u16,
    /// 单元格的左半或右半。
    pub side: FfiCellSide,
}

impl From<ViewportPoint> for FfiViewportPoint {
    fn from(p: ViewportPoint) -> Self {
        FfiViewportPoint {
            col: p.col,
            row: p.row,
            side: p.side.into(),
        }
    }
}

impl From<FfiViewportPoint> for ViewportPoint {
    fn from(p: FfiViewportPoint) -> Self {
        ViewportPoint {
            col: p.col,
            row: p.row,
            side: p.side.into(),
        }
    }
}

/// 24 位颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiRgb {
    /// 红。
    pub r: u8,
    /// 绿。
    pub g: u8,
    /// 蓝。
    pub b: u8,
}

impl From<Rgb> for FfiRgb {
    fn from(c: Rgb) -> Self {
        FfiRgb {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

impl From<FfiRgb> for Rgb {
    fn from(c: FfiRgb) -> Self {
        Rgb {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

/// 本地 shell 的配置。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiLocalProfile {
    /// shell 命令；空表示用户默认 shell。
    pub shell_command: String,
    /// 起始目录；`None` 表示用户主目录。
    pub start_dir: Option<String>,
    /// 字符编码标签；空串表示 UTF-8。
    pub encoding: String,
    /// 覆盖的 locale；`None` 表示不覆盖。必须是合法 locale 名，否则转换失败。
    pub locale: Option<String>,
}

/// SSH / SFTP 的连接目标。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSshTarget {
    /// 主机名或地址。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// 用户名。
    pub user: String,
    /// 认证方式。
    pub auth: FfiAuthMethod,
    /// 私钥文件路径（认证方式为私钥时使用）。
    pub private_key_path: Option<String>,
    /// 跳板机 Profile 的 id（UUID 字符串）。
    pub jump: Option<String>,
    /// ProxyCommand；与 `jump` 互斥。
    pub proxy_command: Option<String>,
    /// 保活配置。
    pub keepalive: FfiKeepaliveConfig,
}

/// SSH 保活配置（ssh.md §2.4）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiKeepaliveConfig {
    /// 是否启用协议层 keepalive。
    pub enabled: bool,
    /// 协议层 keepalive 间隔秒数。
    pub interval_secs: u16,
    /// 连续多少次无回应判定断线。
    pub max_misses: u16,
    /// 是否在空闲时向 shell 发送保活字符串。
    pub idle_enabled: bool,
    /// 网络空闲多少秒后发送一次。
    pub idle_interval_secs: u16,
    /// 空闲时发送的字符串（支持 \r \n \t \\ 转义）。
    pub idle_string: String,
}

/// 从 core 配置镜像到 FFI。
impl From<&KeepaliveConfig> for FfiKeepaliveConfig {
    fn from(k: &KeepaliveConfig) -> Self {
        Self {
            enabled: k.enabled,
            interval_secs: k.interval_secs,
            max_misses: k.max_misses,
            idle_enabled: k.idle_enabled,
            idle_interval_secs: k.idle_interval_secs,
            idle_string: k.idle_string.clone(),
        }
    }
}

/// 从 FFI 写回 core 配置。
impl From<FfiKeepaliveConfig> for KeepaliveConfig {
    fn from(k: FfiKeepaliveConfig) -> Self {
        Self {
            enabled: k.enabled,
            interval_secs: k.interval_secs,
            max_misses: k.max_misses,
            idle_enabled: k.idle_enabled,
            idle_interval_secs: k.idle_interval_secs,
            idle_string: k.idle_string,
        }
    }
}

/// SSH 终端的配置。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSshProfile {
    /// 连接目标。
    pub target: FfiSshTarget,
    /// 字符编码标签；空串表示 UTF-8。
    pub encoding: String,
    /// 覆盖的 locale。
    pub locale: Option<String>,
}

/// Profile 的种类与种类专属配置。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiProfileKind {
    /// 本地 shell。
    Local {
        /// 配置。
        profile: FfiLocalProfile,
    },
    /// SSH 终端。
    Ssh {
        /// 配置。
        profile: FfiSshProfile,
    },
    /// 纯 SFTP。
    Sftp {
        /// 连接目标。
        target: FfiSshTarget,
    },
}

/// 会话配置。**不含任何密码或口令**（凭据走 `set_credential`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiProfile {
    /// 持久化 id（UUID 字符串）。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 标签颜色。
    pub color: Option<FfiRgb>,
    /// 所属分组 id（UUID 字符串）；`None` 为顶层。
    pub group: Option<String>,
    /// 种类与专属配置。
    pub kind: FfiProfileKind,
}

/// 分组。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiGroup {
    /// 持久化 id（UUID 字符串）。
    pub id: String,
    /// 显示名。
    pub name: String,
}

/// Sender 命令的作用域。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSnippetScope {
    /// 只属于某个 Profile。
    Profile {
        /// Profile id（UUID 字符串）。
        id: String,
    },
    /// 属于某个分组。
    Group {
        /// 分组 id（UUID 字符串）。
        id: String,
    },
    /// 某一类会话。
    Kind {
        /// 会话种类。
        kind: FfiSnippetKindScope,
    },
    /// 全局。
    Global,
}

/// Sender 命令（快捷片段）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct FfiSnippet {
    /// 持久化 id（UUID 字符串）。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 命令文本。
    pub text: String,
    /// 行尾。
    pub line_ending: FfiLineEnding,
    /// 作用域。
    pub scope: FfiSnippetScope,
}

fn parse_locale(l: Option<String>) -> Result<Option<Locale>, FfiCoreError> {
    l.map(|s| {
        Locale::parse(&s).ok_or_else(|| FfiCoreError::Internal {
            detail: format!("invalid locale `{s}`"),
        })
    })
    .transpose()
}

impl From<&SshTarget> for FfiSshTarget {
    fn from(t: &SshTarget) -> Self {
        FfiSshTarget {
            host: t.host.clone(),
            port: t.port,
            user: t.user.clone(),
            auth: t.auth.into(),
            private_key_path: t.private_key_path.clone(),
            jump: t.jump.map(|id| id.to_string()),
            proxy_command: t.proxy_command.clone(),
            keepalive: (&t.keepalive).into(),
        }
    }
}

impl TryFrom<FfiSshTarget> for SshTarget {
    type Error = FfiCoreError;

    fn try_from(t: FfiSshTarget) -> Result<Self, FfiCoreError> {
        Ok(SshTarget {
            host: t.host,
            port: t.port,
            user: t.user,
            auth: t.auth.into(),
            private_key_path: t.private_key_path,
            jump: t
                .jump
                .as_deref()
                .map(|s| parse_id(s, "profile"))
                .transpose()?,
            proxy_command: t.proxy_command,
            keepalive: t.keepalive.into(),
        })
    }
}

impl From<&LocalProfile> for FfiLocalProfile {
    fn from(profile: &LocalProfile) -> Self {
        Self {
            shell_command: profile.shell_command.clone(),
            start_dir: profile.start_dir.clone(),
            encoding: profile.encoding.as_str().to_owned(),
            locale: profile
                .locale
                .as_ref()
                .map(|locale| locale.as_str().to_owned()),
        }
    }
}

impl TryFrom<FfiLocalProfile> for LocalProfile {
    type Error = FfiCoreError;

    fn try_from(profile: FfiLocalProfile) -> Result<Self, Self::Error> {
        Ok(Self {
            shell_command: profile.shell_command,
            start_dir: profile.start_dir,
            encoding: EncodingLabel::new(&profile.encoding),
            locale: parse_locale(profile.locale)?,
        })
    }
}

impl From<&Profile> for FfiProfile {
    fn from(p: &Profile) -> Self {
        let kind = match &p.kind {
            ProfileKind::Local(l) => FfiProfileKind::Local { profile: l.into() },
            ProfileKind::Ssh(s) => FfiProfileKind::Ssh {
                profile: FfiSshProfile {
                    target: (&s.target).into(),
                    encoding: s.encoding.as_str().to_owned(),
                    locale: s.locale.as_ref().map(|l| l.as_str().to_owned()),
                },
            },
            ProfileKind::Sftp(t) => FfiProfileKind::Sftp { target: t.into() },
        };
        FfiProfile {
            id: p.id.to_string(),
            name: p.name.clone(),
            color: p.color.map(Into::into),
            group: p.group.map(|g| g.to_string()),
            kind,
        }
    }
}

impl TryFrom<FfiProfile> for Profile {
    type Error = FfiCoreError;

    fn try_from(p: FfiProfile) -> Result<Self, FfiCoreError> {
        let kind = match p.kind {
            FfiProfileKind::Local { profile: l } => ProfileKind::Local(l.try_into()?),
            FfiProfileKind::Ssh { profile: s } => ProfileKind::Ssh(SshProfile {
                target: s.target.try_into()?,
                encoding: EncodingLabel::new(&s.encoding),
                locale: parse_locale(s.locale)?,
            }),
            FfiProfileKind::Sftp { target } => ProfileKind::Sftp(target.try_into()?),
        };
        Ok(Profile {
            id: parse_id(&p.id, "profile")?,
            name: p.name,
            color: p.color.map(Into::into),
            group: p
                .group
                .as_deref()
                .map(|s| parse_id(s, "group"))
                .transpose()?,
            kind,
        })
    }
}

impl From<&Group> for FfiGroup {
    fn from(g: &Group) -> Self {
        FfiGroup {
            id: g.id.to_string(),
            name: g.name.clone(),
        }
    }
}

impl TryFrom<FfiGroup> for Group {
    type Error = FfiCoreError;

    fn try_from(g: FfiGroup) -> Result<Self, FfiCoreError> {
        Ok(Group {
            id: parse_id(&g.id, "group")?,
            name: g.name,
        })
    }
}

impl From<&Snippet> for FfiSnippet {
    fn from(s: &Snippet) -> Self {
        FfiSnippet {
            id: s.id.to_string(),
            name: s.name.clone(),
            text: s.text.clone(),
            line_ending: s.line_ending.into(),
            scope: match &s.scope {
                SnippetScope::Profile(id) => FfiSnippetScope::Profile { id: id.to_string() },
                SnippetScope::Group(id) => FfiSnippetScope::Group { id: id.to_string() },
                SnippetScope::Kind(k) => FfiSnippetScope::Kind { kind: (*k).into() },
                SnippetScope::Global => FfiSnippetScope::Global,
            },
        }
    }
}

impl TryFrom<FfiSnippet> for Snippet {
    type Error = FfiCoreError;

    fn try_from(s: FfiSnippet) -> Result<Self, FfiCoreError> {
        Ok(Snippet {
            id: parse_id(&s.id, "snippet")?,
            name: s.name,
            text: s.text,
            line_ending: s.line_ending.into(),
            scope: match s.scope {
                FfiSnippetScope::Profile { id } => SnippetScope::Profile(parse_id(&id, "profile")?),
                FfiSnippetScope::Group { id } => SnippetScope::Group(parse_id(&id, "group")?),
                FfiSnippetScope::Kind { kind } => SnippetScope::Kind(kind.into()),
                FfiSnippetScope::Global => SnippetScope::Global,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::{GroupId, ProfileId, SnippetId};

    fn ssh_profile() -> Profile {
        Profile {
            id: ProfileId::new(),
            name: "prod".into(),
            color: Some(Rgb { r: 1, g: 2, b: 3 }),
            group: Some(GroupId::new()),
            kind: ProfileKind::Ssh(SshProfile {
                target: SshTarget {
                    host: "h".into(),
                    port: 2222,
                    user: "u".into(),
                    auth: AuthMethod::PrivateKey,
                    private_key_path: Some("/k".into()),
                    jump: Some(ProfileId::new()),
                    proxy_command: None,
                    // 非默认值，确保双向转换真的搬运了六个字段。
                    keepalive: KeepaliveConfig {
                        enabled: false,
                        interval_secs: 60,
                        max_misses: 5,
                        idle_enabled: true,
                        idle_interval_secs: 120,
                        idle_string: "\\n".into(),
                    },
                },
                encoding: EncodingLabel::new("gbk"),
                locale: Locale::parse("zh_CN.UTF-8"),
            }),
        }
    }

    #[test]
    fn profile_roundtrip() {
        let p = ssh_profile();
        let back = Profile::try_from(FfiProfile::from(&p)).unwrap();
        assert_eq!(back, p);

        let local = Profile {
            id: ProfileId::new(),
            name: "l".into(),
            color: None,
            group: None,
            kind: ProfileKind::Local(LocalProfile {
                shell_command: "/bin/zsh".into(),
                start_dir: Some("/tmp".into()),
                encoding: EncodingLabel::default(),
                locale: None,
            }),
        };
        assert_eq!(Profile::try_from(FfiProfile::from(&local)).unwrap(), local);

        let sftp = Profile {
            kind: match ssh_profile().kind {
                ProfileKind::Ssh(s) => ProfileKind::Sftp(s.target),
                _ => unreachable!(),
            },
            ..local
        };
        assert_eq!(Profile::try_from(FfiProfile::from(&sftp)).unwrap(), sftp);
    }

    #[test]
    fn ids_are_hyphenated_uuid_strings() {
        let p = ssh_profile();
        let f = FfiProfile::from(&p);
        assert_eq!(f.id, p.id.to_string());
        assert_eq!(f.id.len(), 36);
    }

    #[test]
    fn invalid_id_or_locale_is_internal_error() {
        let mut f = FfiProfile::from(&ssh_profile());
        f.id = "not-a-uuid".into();
        assert!(matches!(
            Profile::try_from(f),
            Err(FfiCoreError::Internal { .. })
        ));

        let mut f = FfiProfile::from(&ssh_profile());
        if let FfiProfileKind::Ssh { profile } = &mut f.kind {
            profile.locale = Some("en US".into());
        }
        assert!(matches!(
            Profile::try_from(f),
            Err(FfiCoreError::Internal { .. })
        ));
    }

    #[test]
    fn snippet_and_group_roundtrip() {
        for scope in [
            SnippetScope::Profile(ProfileId::new()),
            SnippetScope::Group(GroupId::new()),
            SnippetScope::Kind(SnippetKindScope::Ssh),
            SnippetScope::Global,
        ] {
            let s = Snippet {
                id: SnippetId::new(),
                name: "n".into(),
                text: "ls".into(),
                line_ending: LineEnding::CrLf,
                scope,
            };
            assert_eq!(Snippet::try_from(FfiSnippet::from(&s)).unwrap(), s);
        }
        let g = Group {
            id: GroupId::new(),
            name: "g".into(),
        };
        assert_eq!(Group::try_from(FfiGroup::from(&g)).unwrap(), g);
    }

    #[test]
    fn geometry_roundtrip() {
        let p = ViewportPoint {
            col: 3,
            row: 4,
            side: CellSide::Right,
        };
        assert_eq!(ViewportPoint::from(FfiViewportPoint::from(p)), p);
        let g = GridSize { cols: 80, rows: 24 };
        assert_eq!(GridSize::from(FfiGridSize::from(g)), g);
    }
}
