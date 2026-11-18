//! 文件操作的对话框内容与结果解析（docs/spec/filer.md §5.3、§5.4、§6.3）：
//! 删除确认、新建 / 重命名的名称输入、同名冲突（覆盖 / 跳过 / 取消，可应用于其余）。
//! 只生成 [`DialogSpec`] 并把 [`DialogResult`] 翻译成决定，不碰窗口系统，三个平台都能测试。

use crate::app::filer_text::msg;
use crate::app::fspath::{NameError, PathStyle, validate_child_name};
use crate::app::transfer_queue::{Conflict, Decision};
use crate::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec, InputSpec};
use kanso_types::message::Message;

/// 确认 / 通用按钮：取消。
pub const BTN_CANCEL: &str = "cancel";
/// 删除确认。
pub const BTN_DELETE: &str = "delete";
/// 名称输入确认。
pub const BTN_OK: &str = "ok";
/// 冲突：覆盖。
pub const BTN_OVERWRITE: &str = "overwrite";
/// 冲突：全部覆盖。
pub const BTN_OVERWRITE_ALL: &str = "overwrite-all";
/// 冲突：跳过。
pub const BTN_SKIP: &str = "skip";
/// 冲突：全部跳过。
pub const BTN_SKIP_ALL: &str = "skip-all";
/// 名称输入框的标识。
pub const NAME_FIELD: &str = "name";

/// 一个待删除的条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteTarget {
    /// 名称。
    pub name: String,
    /// 完整路径。
    pub path: String,
    /// 目录（含指向目录的链接——但链接优先按链接处理）。
    pub is_dir: bool,
    /// 符号链接：只移除链接，不影响目标。
    pub is_symlink: bool,
}

/// 名称对话框的用途。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    /// 新建文件。
    NewFile,
    /// 新建目录。
    NewFolder,
    /// 重命名。
    Rename,
}

/// 删除确认框（两种界面共用）：显示本地 / 远端身份、路径与不可撤销说明。
/// 默认按钮是“删除”（回车确认），Esc / 取消按钮 / 点遮罩取消。
pub fn delete_spec(targets: &[DeleteTarget], location: &str) -> DialogSpec {
    let mut spec;
    if let [one] = targets {
        let (title, note) = if one.is_symlink {
            (
                Message::UiFilerDeleteLink {},
                Message::UiFilerDeleteLinkNote {},
            )
        } else if one.is_dir {
            (
                Message::UiFilerDeleteDir {},
                Message::UiFilerDeleteDirNote {},
            )
        } else {
            (
                Message::UiFilerDeleteFile {},
                Message::UiFilerDeleteFileNote {},
            )
        };
        spec = DialogSpec::new(&msg(title))
            .text(&msg(Message::UiFilerDeleteWhere {
                location: location.to_string(),
            }))
            .text(&one.name)
            .code(&one.path)
            .text(&msg(note));
    } else {
        spec = DialogSpec::new(&msg(Message::UiFilerDeleteMany {
            count: (targets.len()) as i64,
        }))
        .text(&msg(Message::UiFilerDeleteWhere {
            location: location.to_string(),
        }));
        for target in targets.iter().take(6) {
            spec = spec.code(&target.path);
        }
        if targets.len() > 6 {
            spec = spec.text(&msg(Message::UiFilerDeleteMore {
                count: (targets.len() - 6) as i64,
            }));
        }
        spec = spec.text(&msg(Message::UiFilerDeleteManyNote {}));
    }
    spec.button(ButtonSpec::secondary(BTN_CANCEL, &msg(Message::UiFilerCancel {})).cancel())
        .button(ButtonSpec::danger(BTN_DELETE, &msg(Message::UiFilerDelete {})).default_button())
        .dismiss_on_scrim()
}

/// 是否确认删除（只有明确点了删除按钮或回车默认按钮才算）。
pub fn delete_confirmed(result: &DialogResult) -> bool {
    result.button.as_deref() == Some(BTN_DELETE)
}

/// 新建 / 重命名的名称输入框。`error` 非空时显示在输入框上方（上一次提交被拒绝的原因）。
pub fn name_spec(kind: NameKind, initial: &str, location: &str, error: Option<&str>) -> DialogSpec {
    let title = match kind {
        NameKind::NewFile => Message::UiFilerNewFile {},
        NameKind::NewFolder => Message::UiFilerNewFolder {},
        NameKind::Rename => Message::UiFilerRename {},
    };
    let mut spec = DialogSpec::new(&msg(title)).text(&msg(Message::UiFilerNameWhere {
        location: location.to_string(),
    }));
    if let Some(e) = error {
        spec = spec.text(e);
    }
    spec.input(InputSpec::new(NAME_FIELD, &msg(Message::UiFilerName {})).initial(initial))
        .button(ButtonSpec::secondary(BTN_CANCEL, &msg(Message::UiFilerCancel {})).cancel())
        .button(ButtonSpec::primary(BTN_OK, &msg(Message::UiFilerOk {})).default_button())
}

/// 名称对话框的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameOutcome {
    /// 取消，或重命名时名称没有变化：什么都不做。
    Nothing,
    /// 名称不合规（需要提示后让用户重输）。
    Invalid(NameError),
    /// 合规的新名称。
    Name(String),
}

/// 解析名称对话框结果。`old` 是重命名前的旧名称（新建时为 `None`）；
/// 空名称、取消、未更名都不执行。
pub fn parse_name(style: PathStyle, result: &DialogResult, old: Option<&str>) -> NameOutcome {
    if result.button.as_deref() != Some(BTN_OK) {
        return NameOutcome::Nothing;
    }
    let raw = result.value(NAME_FIELD).unwrap_or_default();
    if raw.trim().is_empty() {
        return NameOutcome::Nothing;
    }
    match validate_child_name(style, raw) {
        Ok(name) if old == Some(name.as_str()) => NameOutcome::Nothing,
        Ok(name) => NameOutcome::Name(name),
        Err(e) => NameOutcome::Invalid(e),
    }
}

/// 名称错误的用户可读文字。
pub fn name_error_text(e: NameError) -> String {
    match e {
        NameError::Empty => msg(Message::UiFilerNameEmpty {}),
        NameError::Reserved => msg(Message::UiFilerNameReserved {}),
        NameError::HasSeparator => msg(Message::UiFilerNameSeparator {}),
        NameError::InvalidChar(c) => msg(Message::UiFilerNameInvalidChar {
            ch: (c.to_string()).to_string(),
        }),
    }
}

/// 同名冲突对话框：覆盖 / 跳过 / 取消；本批还有其余项时再给“全部”两个按钮（应用于其余冲突）。
/// 目录同名默认合并，合并时其中的同名文件仍按所选处理。
pub fn conflict_spec(c: &Conflict) -> DialogSpec {
    let mut spec = DialogSpec::new(&msg(Message::UiFilerConflictTitle {}))
        .text(&msg(if c.item.is_dir {
            Message::UiFilerConflictDir {
                name: c.item.name.clone(),
            }
        } else {
            Message::UiFilerConflictFile {
                name: c.item.name.clone(),
            }
        }))
        .code(&c.item.dest_dir);
    if c.item.is_dir {
        spec = spec.text(&msg(Message::UiFilerConflictMergeNote {}));
    }
    spec = spec.button(ButtonSpec::secondary(BTN_CANCEL, &msg(Message::UiFilerCancel {})).cancel());
    spec = spec
        .button(ButtonSpec::secondary(BTN_SKIP, &msg(Message::UiFilerSkip {})).default_button());
    if c.remaining > 0 {
        spec = spec.button(ButtonSpec::secondary(
            BTN_SKIP_ALL,
            &msg(Message::UiFilerSkipAll {}),
        ));
    }
    spec = spec.button(ButtonSpec::danger(
        BTN_OVERWRITE,
        &msg(Message::UiFilerOverwrite {}),
    ));
    if c.remaining > 0 {
        spec = spec.button(ButtonSpec::danger(
            BTN_OVERWRITE_ALL,
            &msg(Message::UiFilerOverwriteAll {}),
        ));
    }
    spec
}

/// 冲突对话框结果；Esc、点遮罩等一律按取消整批处理（不会悄悄覆盖）。
pub fn conflict_decision(result: &DialogResult) -> Decision {
    match result.button.as_deref() {
        Some(BTN_OVERWRITE) => Decision::Overwrite { all: false },
        Some(BTN_OVERWRITE_ALL) => Decision::Overwrite { all: true },
        Some(BTN_SKIP) => Decision::Skip { all: false },
        Some(BTN_SKIP_ALL) => Decision::Skip { all: true },
        _ => Decision::Cancel,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::transfer_queue::{Kind, TransferItem};
    use crate::widgets::dialog::{BodyBlock, FieldValue, Trigger};

    fn result(button: Option<&str>, name: Option<&str>) -> DialogResult {
        DialogResult {
            button: button.map(str::to_owned),
            trigger: Trigger::Click,
            values: name
                .map(|n| {
                    vec![FieldValue {
                        id: NAME_FIELD.into(),
                        value: n.into(),
                        secure: false,
                    }]
                })
                .unwrap_or_default(),
        }
    }

    fn target(dir: bool, link: bool) -> DeleteTarget {
        DeleteTarget {
            name: "x".into(),
            path: "/srv/x".into(),
            is_dir: dir,
            is_symlink: link,
        }
    }

    fn body_text(spec: &DialogSpec) -> String {
        spec.body
            .iter()
            .map(|b| match b {
                BodyBlock::Text(s) | BodyBlock::Code(s) => s.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn delete_dialog_names_kind_location_path_and_irreversibility() {
        let f = delete_spec(&[target(false, false)], "本地");
        assert_eq!(f.title, "删除文件");
        let text = body_text(&f);
        assert!(text.contains("本地") && text.contains("/srv/x") && text.contains("无法撤销"));
        let d = delete_spec(&[target(true, false)], "prod");
        assert_eq!(d.title, "删除目录");
        assert!(body_text(&d).contains("只能删除空目录"));
        let l = delete_spec(&[target(true, true)], "prod");
        assert_eq!(l.title, "删除链接");
        assert!(body_text(&l).contains("不影响目标"));
        // 回车 = 删除，Esc = 取消。
        let i = f.default_button().unwrap();
        assert_eq!(f.buttons[i].id, BTN_DELETE);
        assert_eq!(f.buttons[f.cancel_button().unwrap()].id, BTN_CANCEL);
    }

    #[test]
    fn only_the_delete_button_confirms() {
        assert!(delete_confirmed(&result(Some(BTN_DELETE), None)));
        assert!(!delete_confirmed(&result(Some(BTN_CANCEL), None)));
        assert!(!delete_confirmed(&result(None, None)));
    }

    #[test]
    fn deleting_many_lists_a_few_and_counts_the_rest() {
        let many: Vec<_> = (0..9).map(|_| target(false, false)).collect();
        let s = delete_spec(&many, "本地");
        assert!(s.title.contains('9'));
        assert!(body_text(&s).contains("还有 3 项"));
    }

    #[test]
    fn name_results() {
        let st = PathStyle::Posix;
        assert_eq!(
            parse_name(st, &result(Some(BTN_OK), Some(" new ")), None),
            NameOutcome::Name("new".into())
        );
        assert_eq!(
            parse_name(st, &result(Some(BTN_OK), Some("   ")), None),
            NameOutcome::Nothing
        );
        assert_eq!(
            parse_name(st, &result(Some(BTN_CANCEL), Some("a")), None),
            NameOutcome::Nothing
        );
        // 重命名未更名不执行。
        assert_eq!(
            parse_name(st, &result(Some(BTN_OK), Some("a")), Some("a")),
            NameOutcome::Nothing
        );
        // 越出当前目录的名称被拒绝。
        assert_eq!(
            parse_name(st, &result(Some(BTN_OK), Some("../b")), Some("a")),
            NameOutcome::Invalid(NameError::HasSeparator)
        );
        assert!(name_error_text(NameError::HasSeparator).contains("分隔符"));
    }

    #[test]
    fn name_dialog_prefills_and_shows_the_last_error() {
        let s = name_spec(NameKind::Rename, "old.txt", "本地", Some("已经存在"));
        assert_eq!(s.inputs[0].initial, "old.txt");
        assert!(body_text(&s).contains("已经存在"));
        assert_eq!(s.buttons[s.default_button().unwrap()].id, BTN_OK);
    }

    fn conflict(dir: bool, remaining: usize) -> Conflict {
        Conflict {
            item: TransferItem {
                kind: Kind::Upload,
                source: "C:\\a".into(),
                name: "a".into(),
                dest_dir: "/srv".into(),
                is_dir: dir,
            },
            remaining,
        }
    }

    #[test]
    fn conflict_dialog_offers_all_buttons_only_with_more_items() {
        let ids = |c: &Conflict| -> Vec<String> {
            conflict_spec(c)
                .buttons
                .iter()
                .map(|b| b.id.clone())
                .collect()
        };
        assert_eq!(
            ids(&conflict(false, 0)),
            vec!["cancel", "skip", "overwrite"]
        );
        assert_eq!(
            ids(&conflict(false, 2)),
            vec!["cancel", "skip", "skip-all", "overwrite", "overwrite-all"]
        );
        assert!(body_text(&conflict_spec(&conflict(true, 0))).contains("合并"));
        // 默认按钮是更安全的“跳过”，不会因为回车而覆盖。
        let s = conflict_spec(&conflict(false, 0));
        assert_eq!(s.buttons[s.default_button().unwrap()].id, BTN_SKIP);
    }

    #[test]
    fn conflict_decisions_default_to_cancel() {
        let d = |b: Option<&str>| conflict_decision(&result(b, None));
        assert_eq!(d(Some(BTN_OVERWRITE)), Decision::Overwrite { all: false });
        assert_eq!(
            d(Some(BTN_OVERWRITE_ALL)),
            Decision::Overwrite { all: true }
        );
        assert_eq!(d(Some(BTN_SKIP)), Decision::Skip { all: false });
        assert_eq!(d(Some(BTN_SKIP_ALL)), Decision::Skip { all: true });
        assert_eq!(d(None), Decision::Cancel);
        assert_eq!(d(Some(BTN_CANCEL)), Decision::Cancel);
    }
}
