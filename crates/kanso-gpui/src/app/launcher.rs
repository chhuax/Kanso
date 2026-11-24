//! 会话启动器与编辑对话框的视图辅助（纯逻辑，三平台可测）：列表行展开、菜单项与菜单回传 id、
//! 确认文案、颜色选择、编辑错误文字。GPUI 视图在 `ui/launcher.rs`、`ui/session_editor.rs`。
//!
//! 对照 `Sessions/SessionLauncherViewController.swift`（ui-shell.md §4、§5）。

use crate::app::i18n::{label, t};
use kanso_core::{CoreError, DeleteImpact};
use kanso_types::geometry::Rgb;
use kanso_types::message::Message;
use kanso_types::profile::{AuthMethod, Group, Profile, SessionKind};
use kanso_types::{GroupId, ProfileId};

use super::sessions::{EditorError, LauncherSection, RowSubtitle, row_subtitle};
use crate::widgets::menu::MenuItem;

// ---- 核心消息 ----

/// 把核心的用户级消息渲染成界面文字。
pub fn render_message(message: &Message) -> String {
    kanso_l10n::render(message, &super::i18n::languages())
}

/// 核心错误的展示文字：用户级错误渲染其消息，其余给通用提示（英文技术细节只进日志）。
pub fn core_error_text(error: &CoreError) -> String {
    error.user_message().map_or_else(
        || label(Message::UiOperationFailed2 {}).to_owned(),
        render_message,
    )
}

// ---- 列表行 ----

/// 启动器列表里的一行（自上而下）。
// LauncherRow 是渲染期临时构造的列表行，非常驻大数组；Profile 因会话配置增大到 264B，
// Box 化只会让每次构建列表多做一次堆分配，收益有限，故允许变体尺寸差异。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum LauncherRow {
    /// 分组标题（未分组的会话没有标题）。
    Header {
        /// 分组。
        group: Group,
        /// 当前是否折叠。
        collapsed: bool,
    },
    /// 一个会话。
    Profile(Profile),
    /// 分组里没有会话（只在没有搜索词时出现，否则空分组被隐藏）。
    EmptyGroup,
}

/// 把分组区展开成平铺的行。
pub fn rows(sections: &[LauncherSection]) -> Vec<LauncherRow> {
    let mut out = Vec::new();
    for s in sections {
        if let Some(g) = &s.group {
            out.push(LauncherRow::Header {
                group: g.clone(),
                collapsed: s.collapsed,
            });
            if !s.collapsed && s.profiles.is_empty() {
                out.push(LauncherRow::EmptyGroup);
            }
        }
        out.extend(s.profiles.iter().cloned().map(LauncherRow::Profile));
    }
    out
}

/// 某个会话所在的行下标（键盘移动选中后把它滚动到可见）。
pub fn row_index_of(rows: &[LauncherRow], id: ProfileId) -> Option<usize> {
    rows.iter()
        .position(|r| matches!(r, LauncherRow::Profile(p) if p.id == id))
}

/// 行副标题文字：SSH / SFTP 为 `user@host:port`，本地为 shell 命令或"默认 Shell"。
pub fn subtitle_text(profile: &Profile) -> String {
    match row_subtitle(profile) {
        RowSubtitle::Target(t) | RowSubtitle::ShellCommand(t) => t,
        RowSubtitle::DefaultShell => label(Message::UiDefaultShell {}).to_owned(),
    }
}

// ---- 菜单 ----

/// 会话行"…"菜单的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileAction {
    /// 编辑。
    Edit,
    /// 移到分组（`None` = 不分组）。
    Move(Option<GroupId>),
    /// 删除（需确认）。
    Delete,
}

/// 会话行菜单：编辑、移到分组、删除（ui-shell.md §4）。单击行或回车才连接。
pub fn profile_menu(profile: &Profile, groups: &[Group]) -> Vec<MenuItem> {
    let mut moves = vec![
        MenuItem::action("move:none", label(Message::UiNoGroup {}))
            .checked(profile.group.is_none()),
    ];
    moves.extend(groups.iter().map(|g| {
        MenuItem::action(format!("move:{}", g.id), g.name.clone())
            .checked(profile.group == Some(g.id))
    }));
    vec![
        MenuItem::action("edit", label(Message::UiEdit {})),
        MenuItem::submenu(label(Message::UiMoveToGroup {}), moves),
        MenuItem::separator(),
        MenuItem::action("delete", label(Message::UiDelete {})),
    ]
}

/// 解析会话行菜单回传的 id。
pub fn parse_profile_action(id: &str) -> Option<ProfileAction> {
    match id {
        "edit" => Some(ProfileAction::Edit),
        "delete" => Some(ProfileAction::Delete),
        "move:none" => Some(ProfileAction::Move(None)),
        _ => id
            .strip_prefix("move:")
            .and_then(|g| g.parse::<GroupId>().ok())
            .map(|g| ProfileAction::Move(Some(g))),
    }
}

/// 分组标题"…"菜单的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupAction {
    /// 重命名。
    Rename,
    /// 删除（需确认）。
    Delete,
}

/// 分组菜单。
pub fn group_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::action("rename", label(Message::UiRename {})),
        MenuItem::action("delete", label(Message::UiDelete {})),
    ]
}

/// 解析分组菜单回传的 id。
pub fn parse_group_action(id: &str) -> Option<GroupAction> {
    match id {
        "rename" => Some(GroupAction::Rename),
        "delete" => Some(GroupAction::Delete),
        _ => None,
    }
}

/// "已保存的会话"标题旁"…"菜单的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedAction {
    /// 新建分组。
    NewGroup,
    /// 导入 OpenSSH 配置。
    ImportSshConfig,
}

/// "已保存的会话"菜单。
pub fn saved_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::action("new-group", label(Message::UiNewGroup {})),
        MenuItem::action("import-ssh", label(Message::UiImportOpensshConfig2 {})),
    ]
}

/// 解析"已保存的会话"菜单回传的 id。
pub fn parse_saved_action(id: &str) -> Option<SavedAction> {
    match id {
        "new-group" => Some(SavedAction::NewGroup),
        "import-ssh" => Some(SavedAction::ImportSshConfig),
        _ => None,
    }
}

// ---- 确认文案 ----

/// 删除分组前的确认文字：数字来自核心的 `group_delete_impact`，必须与实际删除的一致。
pub fn delete_group_message(impact: &DeleteImpact) -> String {
    t(&Message::UiDeletingThisGroupAlsoDeletes {
        profiles: impact.profiles as i64,
        snippets: impact.snippets as i64,
    })
}

/// 删除会话前的确认文字（只删 Kanso 里保存的会话与凭据，不碰 ssh 配置与密钥文件）。
pub fn delete_profile_message() -> &'static str {
    label(Message::UiOnlySessionAndItsCredentials {})
}

// ---- 编辑对话框 ----

/// 类型的显示名。
pub fn kind_label(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Local => label(Message::UiLocal {}),
        SessionKind::Ssh => "SSH",
        SessionKind::Sftp => "SFTP",
    }
}

/// 认证方式的显示名。
pub fn auth_label(auth: AuthMethod) -> &'static str {
    match auth {
        AuthMethod::Password => label(Message::UiPassword {}),
        AuthMethod::PrivateKey => label(Message::UiPrivateKey {}),
        AuthMethod::Agent => "ssh-agent",
    }
}

/// 色板点击：点未选中的颜色选中它，再点已选中的颜色取消选择。
pub fn toggle_color(current: Option<Rgb>, picked: Rgb) -> Option<Rgb> {
    if current == Some(picked) {
        None
    } else {
        Some(picked)
    }
}

/// 保存失败的提示文字。`render` 把核心的用户级消息渲染成当前语言（视图用 `kanso-l10n`）。
pub fn editor_error_text(error: &EditorError, render: impl Fn(&Message) -> String) -> String {
    let access_error = |e: &CoreError| {
        t(&Message::WinMcpAccessSaveFailed {
            detail: e
                .user_message()
                .map_or_else(|| label(Message::UiSaveFailed {}).to_owned(), &render),
        })
    };
    match error {
        EditorError::NameEmpty => label(Message::UiEnterSessionName {}).to_owned(),
        EditorError::PortInvalid => label(Message::UiPortMustBeNumberFrom {}).to_owned(),
        EditorError::LocaleInvalid => label(Message::UiLocaleMayOnlyContainLetters {}).to_owned(),
        EditorError::KeepaliveIntervalInvalid | EditorError::KeepaliveMaxMissesInvalid => {
            label(Message::UiKeepaliveIntervalInvalid {}).to_owned()
        }
        EditorError::Save(e) => e
            .user_message()
            .map_or_else(|| label(Message::UiSaveFailed {}).to_owned(), render),
        EditorError::Credential { error, mcp_access } => {
            let detail = error.user_message().map(&render).unwrap_or_default();
            let mut message = t(&Message::UiSessionSavedButCredentialsCould { detail });
            if let Some(e) = mcp_access {
                message.push('\n');
                message.push_str(&access_error(e));
            }
            message
        }
        EditorError::McpAccess(e) => access_error(e),
    }
}

/// 命令面板按子序列匹配，优先相邻字符、靠前的起点和短结果；调用者先折叠查询的大小写。
pub fn command_search_score(text: &str, query: &str) -> Option<i64> {
    let text = text.to_lowercase();
    let mut chars = text.chars().enumerate();
    let mut score = text.chars().count() as i64;
    let mut last = None;
    for wanted in query.chars() {
        let (i, _) = chars.find(|(_, ch)| *ch == wanted)?;
        score += i as i64;
        if last.is_none() {
            score += i as i64;
        }
        if last == i.checked_sub(1) && last.is_some() {
            score -= 2;
        }
        last = Some(i);
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::backend::block_on;
    use crate::app::sessions::LauncherModel;
    use crate::app::testutil::{FakeStore, group, local_profile, ssh_profile};

    fn model() -> (LauncherModel<FakeStore>, Group) {
        let g = group("生产");
        let mut p1 = ssh_profile("db", "db.example.com", "root");
        p1.group = Some(g.id);
        let store = FakeStore::with(
            vec![p1, local_profile("本机", "")],
            vec![g.clone(), group("空组")],
        );
        (LauncherModel::new(store), g)
    }

    #[test]
    fn rows_put_ungrouped_first_then_groups_with_headers() {
        let (m, _) = model();
        let r = rows(m.sections());
        assert!(matches!(&r[0], LauncherRow::Profile(p) if p.name == "本机"));
        assert!(
            matches!(&r[1], LauncherRow::Header { group, collapsed: false } if group.name == "生产")
        );
        assert!(matches!(&r[2], LauncherRow::Profile(p) if p.name == "db"));
        assert!(
            matches!(&r[3], LauncherRow::Header { group, collapsed: false } if group.name == "空组")
        );
        assert_eq!(r[4], LauncherRow::EmptyGroup);
        assert_eq!(r.len(), 5);
        let db = m.visible_profiles()[1].id;
        assert_eq!(row_index_of(&r, db), Some(2));
    }

    #[test]
    fn collapsed_group_hides_rows_but_search_reveals_matches() {
        let (mut m, group) = model();
        let profile = m.visible_profiles()[1].id;
        m.select(Some(profile));
        m.toggle_group(group.id);
        assert_ne!(m.selected(), Some(profile));
        let collapsed = rows(m.sections());
        assert!(matches!(
            collapsed[1],
            LauncherRow::Header {
                collapsed: true,
                ..
            }
        ));
        assert!(matches!(collapsed[2], LauncherRow::Header { .. }));
        assert_eq!(row_index_of(&collapsed, profile), None);

        m.set_query("example.com");
        assert_eq!(m.selected(), Some(profile));
        assert_eq!(row_index_of(&rows(m.sections()), profile), Some(1));
        m.set_query("  ");
        assert_eq!(rows(m.sections()), collapsed);
        m.toggle_group(group.id);
        assert_eq!(row_index_of(&rows(m.sections()), profile), Some(2));
    }

    #[test]
    fn subtitles() {
        let (m, _) = model();
        let ps = m.visible_profiles();
        assert_eq!(subtitle_text(ps[0]), "默认 Shell");
        assert_eq!(subtitle_text(ps[1]), "root@db.example.com:22");
    }

    #[test]
    fn profile_menu_round_trips_ids() {
        let (m, g) = model();
        let p = m.visible_profiles()[1].clone();
        let items = profile_menu(&p, m.groups());
        assert_eq!(items.len(), 4);
        let MenuItem::Submenu { items: moves, .. } = &items[1] else {
            panic!("第二项应是子菜单");
        };
        // 不分组 + 两个分组；当前所在分组带勾。
        assert_eq!(moves.len(), 3);
        let ids: Vec<String> = moves
            .iter()
            .filter_map(|i| match i {
                MenuItem::Action { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids[0], "move:none");
        assert!(moves.iter().any(|i| matches!(
            i,
            MenuItem::Action { id, checked: true, .. } if *id == format!("move:{}", g.id)
        )));
        assert_eq!(
            parse_profile_action(&format!("move:{}", g.id)),
            Some(ProfileAction::Move(Some(g.id)))
        );
        assert_eq!(
            parse_profile_action("move:none"),
            Some(ProfileAction::Move(None))
        );
        assert_eq!(parse_profile_action("connect"), None);
        assert_eq!(parse_profile_action("edit"), Some(ProfileAction::Edit));
        assert_eq!(parse_profile_action("delete"), Some(ProfileAction::Delete));
        assert_eq!(parse_profile_action("move:garbage"), None);
        assert_eq!(parse_profile_action("nope"), None);
    }

    #[test]
    fn group_and_saved_menus_parse() {
        assert_eq!(parse_group_action("rename"), Some(GroupAction::Rename));
        assert_eq!(parse_group_action("delete"), Some(GroupAction::Delete));
        assert_eq!(parse_group_action("x"), None);
        assert_eq!(parse_saved_action("new-group"), Some(SavedAction::NewGroup));
        assert_eq!(
            parse_saved_action("import-ssh"),
            Some(SavedAction::ImportSshConfig)
        );
        assert_eq!(group_menu().len(), 2);
        assert_eq!(saved_menu().len(), 2);
    }

    #[test]
    fn delete_group_text_carries_core_numbers() {
        let t = delete_group_message(&DeleteImpact {
            profiles: 3,
            snippets: 2,
        });
        assert!(t.contains("3 个会话"));
        assert!(t.contains("2 条快捷命令"));
    }

    #[test]
    fn color_toggle_selects_and_clears() {
        let a = Rgb { r: 1, g: 2, b: 3 };
        let b = Rgb { r: 9, g: 9, b: 9 };
        assert_eq!(toggle_color(None, a), Some(a));
        assert_eq!(toggle_color(Some(a), a), None);
        assert_eq!(toggle_color(Some(a), b), Some(b));
    }

    #[test]
    fn editor_error_texts() {
        let render = |_: &Message| "核心消息".to_owned();
        assert_eq!(
            editor_error_text(&EditorError::NameEmpty, render),
            "请输入会话名称。"
        );
        assert!(editor_error_text(&EditorError::PortInvalid, render).contains("65535"));
        // 非用户级核心错误给通用文字，不泄露英文技术细节。
        let internal = kanso_core::CoreError::Internal {
            detail: "db locked".into(),
        };
        let t = editor_error_text(&EditorError::Save(internal), render);
        assert_eq!(t, "保存失败。");
        assert!(!t.contains("db locked"));
    }

    #[test]
    fn deleting_through_the_model_is_visible_in_rows() {
        let (mut m, _) = model();
        let id = m.visible_profiles()[0].id;
        block_on(m.delete_profile(id)).unwrap();
        assert!(
            !rows(m.sections())
                .iter()
                .any(|r| matches!(r, LauncherRow::Profile(p) if p.id == id))
        );
    }
}
