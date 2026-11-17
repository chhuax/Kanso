//! `.kanso` 数据文件导出 / 导入与 OpenSSH 配置导入的界面逻辑（store-import.md §6、§7），
//! 对照 mac 版 `Sessions/DataTransferFlow.swift` 与 `SshConfigImportModel.swift`。
//!
//! 合并、校验、解析规则全在核心（`Core::export_data` / `import_data` / `scan_ssh_config` /
//! `import_ssh_config`）；这里只负责界面要显示的东西：默认文件名、补扩展名、结果摘要文字、
//! 候选主机行与勾选状态。核心消息一律交给 `kanso-l10n` 渲染（语言跟随 [`super::i18n`]）。
//!
//! 本模块不依赖 gpui，三个平台都能编译与测试。

use kanso_core::{ImportReport, SshConfigCandidate, SshConfigImportReport, SshConfigScan};
use kanso_l10n::ArgValue;
use kanso_types::GroupId;
use kanso_types::message::Message;

use super::i18n::{self, label, t};
use crate::widgets::checklist::Checklist;
use crate::widgets::dialog::{ButtonSpec, DialogResult, DialogSpec};

/// 数据文件扩展名（不含点）。
pub const EXTENSION: &str = "kanso";

// ---- 文件名 ----

/// 公历日期（年, 月, 日）。`days` 是自 1970-01-01 起的天数（可为负）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant 的 civil_from_days。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 导出对话框的默认文件名：`Kanso-YYYYMMDD.kanso`（本地日期，固定公历，与 mac 版一致）。
pub fn export_default_name(unix_ms: i64, offset_minutes: i32) -> String {
    let local_secs = unix_ms.div_euclid(1000) + i64::from(offset_minutes) * 60;
    let (y, m, d) = civil_from_days(local_secs.div_euclid(86_400));
    format!("Kanso-{y:04}{m:02}{d:02}.{EXTENSION}")
}

/// 导出时自动补扩展名（spec §6.1）：不区分大小写地已经是 `.kanso` 就原样返回。
pub fn with_extension(path: &str) -> String {
    let has = path
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case(EXTENSION))
        && !path.ends_with(['/', '\\']);
    if has {
        path.to_owned()
    } else {
        format!("{path}.{EXTENSION}")
    }
}

/// 路径里主目录那一段换成 `~`（只用于显示）。
pub fn abbreviate_home(path: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return path.to_owned();
    }
    if path == home {
        return "~".to_owned();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// 默认的 OpenSSH 配置路径（`~/.ssh/config`）。
pub fn default_ssh_config_path(home: &str) -> String {
    let sep = if home.contains('\\') { '\\' } else { '/' };
    format!("{}{sep}.ssh{sep}config", home.trim_end_matches(['/', '\\']))
}

// ---- 结果摘要 ----

/// 导入 / 导出结果页的内容：一句汇总，加可选的明细（警告、被跳过的条目及原因）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResultContent {
    /// 汇总句。
    pub summary: String,
    /// 明细的小标题；`details` 为空时不显示。
    pub details_title: Option<String>,
    /// 明细条目（已渲染）。
    pub details: Vec<String>,
}

impl ResultContent {
    /// 明细拼成多行文字（对话框代码块用）。
    pub fn details_text(&self) -> String {
        self.details.join("\n")
    }
}

fn render(message: &Message) -> String {
    kanso_l10n::render(message, &i18n::languages())
}

fn count(name: &str, v: usize) -> (String, ArgValue) {
    (
        name.to_owned(),
        ArgValue::Int(i64::try_from(v).unwrap_or(i64::MAX)),
    )
}

fn render_key(key: &str, args: &[(String, ArgValue)]) -> String {
    kanso_l10n::render_key(key, args, &i18n::languages())
}

/// `.kanso` 导入结果（spec §6.2：新增、更新、跳过各多少，以及原因）。
pub fn import_result(report: &ImportReport) -> ResultContent {
    let mut summary = render_key(
        "import-report",
        &[
            count("added", report.added as usize),
            count("updated", report.updated as usize),
            count("skipped", report.skipped.len()),
        ],
    );
    if !report.converted.is_empty() {
        summary.push_str(&render_key(
            "import-converted-suffix",
            &[count("count", report.converted.len())],
        ));
    }
    let details_title = if !report.converted.is_empty() {
        Some(render_key("import-converted-and-skipped", &[]))
    } else if !report.skipped.is_empty() {
        Some(label(Message::UiSkipped {}).to_owned())
    } else {
        None
    };
    ResultContent {
        summary,
        details_title,
        details: report
            .converted
            .iter()
            .chain(&report.skipped)
            .map(render)
            .collect(),
    }
}

/// OpenSSH 配置导入结果（spec §7.3）。
pub fn ssh_import_result(report: &SshConfigImportReport) -> ResultContent {
    let summary = render_key(
        "sshconfig-import-report",
        &[
            count("added", report.added as usize),
            count("updated", report.updated as usize),
            count("jumps_added", report.jumps_added as usize),
            count("multi_hop_ignored", report.multi_hop_ignored as usize),
        ],
    );
    ResultContent {
        summary,
        details_title: (!report.warnings.is_empty())
            .then(|| label(Message::UiWarnings {}).to_owned()),
        details: report.warnings.iter().map(render).collect(),
    }
}

/// 导出成功的提示正文。
pub fn export_done_text(path: &str) -> String {
    t(&Message::UiExportedToFileContainsNo {
        path: path.to_string(),
    })
}

/// 导入前确认框的正文：说明合并规则（核心没有"试导入"，规则在 spec §6.2 里是固定的）。
pub fn import_confirm_text() -> &'static str {
    label(Message::UiDataIsMergedById {})
}

// ---- 对话框规格 ----

/// 导入确认框里“导入”按钮的标识。
pub const IMPORT_ID: &str = "import";

/// 只有“好”按钮的提示框。
fn notice(title: &str) -> DialogSpec {
    DialogSpec::new(title)
}

fn ok_button(spec: DialogSpec) -> DialogSpec {
    spec.button(ButtonSpec::primary("ok", label(Message::UiOk {})).cancel())
}

/// 导出成功。
pub fn export_done_spec(path: &str) -> DialogSpec {
    ok_button(notice(label(Message::UiExportComplete {})).text(&export_done_text(path)))
}

/// 导入前确认：说明文件与合并规则，用户确认后才真正导入。
pub fn import_confirm_spec(path: &str) -> DialogSpec {
    notice(label(Message::UiImportData {}))
        .text(label(Message::UiThisFileWillBeImported {}))
        .code(path)
        .text(import_confirm_text())
        .button(ButtonSpec::secondary("cancel", label(Message::UiCancel {})).cancel())
        .button(ButtonSpec::primary(IMPORT_ID, label(Message::UiImport {})))
}

/// 用户是否在导入确认框里点了“导入”。
pub fn import_confirmed(result: &DialogResult) -> bool {
    result.button.as_deref() == Some(IMPORT_ID)
}

/// 导入结果页：汇总句，加被跳过 / 已转换条目的明细。
pub fn import_result_spec(result: &ResultContent) -> DialogSpec {
    let mut spec = notice(label(Message::UiImportComplete {})).text(&result.summary);
    if !result.details.is_empty() {
        if let Some(t) = &result.details_title {
            spec = spec.text(t);
        }
        spec = spec.code(&result.details_text());
    }
    ok_button(spec)
}

/// 操作失败的提示框。
pub fn failure_spec(text: &str) -> DialogSpec {
    ok_button(notice(label(Message::UiOperationFailed {})).text(text))
}

// ---- OpenSSH 配置导入 ----

/// 候选主机的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshRow {
    /// Host 别名。
    pub alias: String,
    /// `用户@主机:端口`；用户名为空时省略 `用户@`。
    pub destination: String,
    /// 私钥路径（原样）。
    pub identity: Option<String>,
    /// `经由 <ProxyJump 原文>`。
    pub jump: Option<String>,
    /// 已有会话匹配：导入时原地更新（界面上要标出来）。
    pub existing: bool,
    /// 该条目自带的警告（已渲染）。
    pub warnings: Vec<String>,
}

impl SshRow {
    fn from_candidate(c: &SshConfigCandidate) -> Self {
        let user = if c.user.is_empty() {
            String::new()
        } else {
            format!("{}@", c.user)
        };
        SshRow {
            alias: c.alias.clone(),
            destination: format!("{user}{}:{}", c.host, c.port),
            identity: c.identity_file.clone().filter(|s| !s.is_empty()),
            jump: c.proxy_jump.as_deref().filter(|s| !s.is_empty()).map(|j| {
                t(&Message::UiVia {
                    jump: j.to_string(),
                })
            }),
            existing: c.existing.is_some(),
            warnings: c.warnings.iter().map(render).collect(),
        }
    }
}

/// 「导入 OpenSSH 配置」对话框的模型：配置文件路径、扫描结果、勾选、目标分组。
#[derive(Clone, Debug)]
pub struct SshImportModel {
    home: String,
    config_path: String,
    rows: Vec<SshRow>,
    checks: Checklist,
    file_warnings: Vec<String>,
    scan_error: Option<String>,
    /// 目标分组；`None` 为顶层。
    pub group: Option<GroupId>,
}

impl SshImportModel {
    /// 默认读 `~/.ssh/config`。
    pub fn new(home: &str) -> Self {
        SshImportModel {
            home: home.to_owned(),
            config_path: default_ssh_config_path(home),
            rows: Vec::new(),
            checks: Checklist::default(),
            file_warnings: Vec::new(),
            scan_error: None,
            group: None,
        }
    }

    /// 配置文件的绝对路径。
    pub fn config_path(&self) -> &str {
        &self.config_path
    }

    /// 换配置文件（调用方随后重新扫描）。
    pub fn set_config_path(&mut self, path: String) {
        self.config_path = path;
    }

    /// 显示用路径（主目录缩成 `~`）。
    pub fn display_path(&self) -> String {
        abbreviate_home(&self.config_path, &self.home)
    }

    /// 采用一次扫描结果。默认全部勾选：已保存的主机也勾选，更新是安全的（核心只改连接目标，spec §7.3）。
    pub fn apply_scan(&mut self, scan: &SshConfigScan) {
        self.scan_error = None;
        self.file_warnings = scan.warnings.iter().map(render).collect();
        self.rows = scan.candidates.iter().map(SshRow::from_candidate).collect();
        self.checks.reset(self.rows.len(), true);
    }

    /// 扫描失败：清空候选并记下原因。
    pub fn apply_scan_error(&mut self, text: String) {
        self.rows.clear();
        self.file_warnings.clear();
        self.checks.reset(0, false);
        self.scan_error = Some(text);
    }

    /// 候选行。
    pub fn rows(&self) -> &[SshRow] {
        &self.rows
    }

    /// 文件级警告（已渲染）。
    pub fn file_warnings(&self) -> &[String] {
        &self.file_warnings
    }

    /// 扫描失败的原因（已渲染）。
    pub fn scan_error(&self) -> Option<&str> {
        self.scan_error.as_deref()
    }

    /// 勾选状态（视图读 / 改）。
    pub fn checks(&self) -> &Checklist {
        &self.checks
    }

    /// 勾选状态（可改）。
    pub fn checks_mut(&mut self) -> &mut Checklist {
        &mut self.checks
    }

    /// 已勾选的别名，按列表顺序。
    pub fn selected_aliases(&self) -> Vec<String> {
        self.checks
            .checked_indices()
            .into_iter()
            .filter_map(|i| self.rows.get(i).map(|r| r.alias.clone()))
            .collect()
    }

    /// 已存在（会被更新）的主机数。
    pub fn existing_count(&self) -> usize {
        self.rows.iter().filter(|r| r.existing).count()
    }

    /// 至少勾选一台才能导入。
    pub fn can_import(&self) -> bool {
        self.checks.count() > 0
    }

    /// 列表上方的概述：找到几台、其中几台已存在。
    pub fn summary_text(&self) -> String {
        t(&Message::UiFoundHostsOfThemAlready {
            found: (self.rows.len()) as i64,
            existing: (self.existing_count()) as i64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::ProfileId;
    use kanso_types::message::Message;

    fn candidate(alias: &str, host: &str, user: &str, existing: bool) -> SshConfigCandidate {
        SshConfigCandidate {
            alias: alias.into(),
            host: host.into(),
            port: 22,
            user: user.into(),
            identity_file: None,
            proxy_jump: None,
            existing: existing.then(ProfileId::new),
            warnings: Vec::new(),
        }
    }

    fn scan(candidates: Vec<SshConfigCandidate>) -> SshConfigScan {
        SshConfigScan {
            candidates,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn default_name_uses_local_date() {
        // 2024-03-05 12:00:00 UTC。
        let ms = 1_709_640_000_000;
        assert_eq!(export_default_name(ms, 0), "Kanso-20240305.kanso");
        // UTC+9 跨到下一天（12:00 + 13h）。
        assert_eq!(
            export_default_name(ms + 3_600_000 * 13 - 9 * 3_600_000, 9 * 60),
            "Kanso-20240306.kanso"
        );
        // UTC-8 仍是前一天的深夜（00:30 UTC - 8h）。
        let ms = 1_709_598_600_000; // 2024-03-05 00:30 UTC
        assert_eq!(export_default_name(ms, -8 * 60), "Kanso-20240304.kanso");
        // 闰日与纪元。
        assert_eq!(export_default_name(0, 0), "Kanso-19700101.kanso");
        assert_eq!(
            export_default_name(1_709_164_800_000, 0),
            "Kanso-20240229.kanso"
        );
        // 纪元之前。
        assert_eq!(export_default_name(-1, 0), "Kanso-19691231.kanso");
    }

    #[test]
    fn extension_is_appended_once() {
        assert_eq!(with_extension("C:\\a\\backup"), "C:\\a\\backup.kanso");
        assert_eq!(with_extension("backup.kanso"), "backup.kanso");
        assert_eq!(with_extension("backup.KANSO"), "backup.KANSO");
        assert_eq!(with_extension("backup.json"), "backup.json.kanso");
        // 只有扩展名没有主名：不当作已有扩展名。
        assert_eq!(with_extension(".kanso"), ".kanso.kanso");
        assert_eq!(with_extension("dir/"), "dir/.kanso");
    }

    #[test]
    fn home_is_abbreviated() {
        assert_eq!(
            abbreviate_home("C:\\Users\\a\\.ssh\\config", "C:\\Users\\a"),
            "~\\.ssh\\config"
        );
        assert_eq!(
            abbreviate_home("/home/a/.ssh/config", "/home/a/"),
            "~/.ssh/config"
        );
        assert_eq!(abbreviate_home("/home/a", "/home/a"), "~");
        // 前缀相同但不是目录边界。
        assert_eq!(abbreviate_home("/home/ab/x", "/home/a"), "/home/ab/x");
        assert_eq!(abbreviate_home("/x", ""), "/x");
        assert_eq!(
            default_ssh_config_path("C:\\Users\\a"),
            "C:\\Users\\a\\.ssh\\config"
        );
        assert_eq!(default_ssh_config_path("/home/a/"), "/home/a/.ssh/config");
    }

    #[test]
    fn import_result_summarizes_and_lists_skipped() {
        let report = ImportReport {
            added: 2,
            updated: 1,
            skipped: vec![Message::ImportWrongExtension {}],
            converted: Vec::new(),
        };
        let r = import_result(&report);
        assert!(
            r.summary.contains('2') && r.summary.contains('1'),
            "{}",
            r.summary
        );
        assert_eq!(r.details.len(), 1);
        assert!(r.details_title.is_some());
        let clean = import_result(&ImportReport {
            added: 0,
            updated: 0,
            skipped: Vec::new(),
            converted: Vec::new(),
        });
        assert!(clean.details.is_empty());
        assert!(clean.details_title.is_none());
    }

    #[test]
    fn ssh_import_result_lists_warnings() {
        let r = ssh_import_result(&SshConfigImportReport {
            added: 3,
            updated: 0,
            jumps_added: 1,
            multi_hop_ignored: 0,
            warnings: Vec::new(),
        });
        assert!(r.details_title.is_none());
        assert!(!r.summary.is_empty());
    }

    #[test]
    fn dialog_specs_have_the_right_buttons() {
        let confirm = import_confirm_spec("C:\\a.kanso");
        assert_eq!(confirm.buttons.len(), 2);
        assert_eq!(confirm.cancel_button(), Some(0));
        assert_eq!(confirm.default_button(), Some(1));
        use crate::widgets::dialog::{Trigger, result_for_button, result_for_cancel};
        assert!(import_confirmed(&result_for_button(
            &confirm,
            1,
            Trigger::Click,
            &[]
        )));
        assert!(!import_confirmed(&result_for_cancel(
            &confirm,
            Trigger::Escape
        )));
        // 结果页：无明细时只有汇总；有明细时带代码块。
        let plain = import_result_spec(&ResultContent {
            summary: "x".into(),
            ..Default::default()
        });
        assert_eq!(plain.body.len(), 1);
        let detailed = import_result_spec(&ResultContent {
            summary: "x".into(),
            details_title: Some("t".into()),
            details: vec!["a".into(), "b".into()],
        });
        assert_eq!(detailed.body.len(), 3);
        assert_eq!(detailed.buttons.len(), 1);
        assert!(export_done_spec("p").buttons[0].is_cancel);
        assert!(!failure_spec("e").body.is_empty());
    }

    #[test]
    fn rows_describe_destination_and_mark_existing() {
        let mut c = candidate("web", "10.0.0.1", "root", true);
        c.identity_file = Some("~/.ssh/id_ed25519".into());
        c.proxy_jump = Some("bastion".into());
        let mut m = SshImportModel::new("/home/a");
        m.apply_scan(&scan(vec![c, candidate("db", "db.local", "", false)]));
        assert_eq!(m.rows()[0].destination, "root@10.0.0.1:22");
        assert_eq!(m.rows()[0].identity.as_deref(), Some("~/.ssh/id_ed25519"));
        assert!(m.rows()[0].jump.is_some());
        assert!(m.rows()[0].existing);
        assert_eq!(m.rows()[1].destination, "db.local:22");
        assert!(!m.rows()[1].existing);
        assert_eq!(m.existing_count(), 1);
    }

    #[test]
    fn selection_defaults_to_all_and_filters_aliases() {
        let mut m = SshImportModel::new("/home/a");
        assert!(!m.can_import());
        m.apply_scan(&scan(vec![
            candidate("a", "a", "u", false),
            candidate("b", "b", "u", true),
            candidate("c", "c", "u", false),
        ]));
        assert_eq!(m.selected_aliases(), vec!["a", "b", "c"]);
        m.checks_mut().toggle(1);
        assert_eq!(m.selected_aliases(), vec!["a", "c"]);
        m.checks_mut().select_none();
        assert!(!m.can_import());
        assert!(m.selected_aliases().is_empty());
    }

    #[test]
    fn rescan_resets_selection_and_error_clears_rows() {
        let mut m = SshImportModel::new("/home/a");
        m.apply_scan(&scan(vec![candidate("a", "a", "u", false)]));
        m.checks_mut().select_none();
        m.apply_scan(&scan(vec![
            candidate("a", "a", "u", false),
            candidate("b", "b", "u", false),
        ]));
        assert_eq!(m.checks().count(), 2);
        m.apply_scan_error("读不了".into());
        assert!(m.rows().is_empty());
        assert_eq!(m.scan_error(), Some("读不了"));
        assert!(!m.can_import());
        m.apply_scan(&scan(vec![candidate("a", "a", "u", false)]));
        assert_eq!(m.scan_error(), None);
    }

    #[test]
    fn config_path_defaults_and_displays_with_tilde() {
        let mut m = SshImportModel::new("/home/a");
        assert_eq!(m.config_path(), "/home/a/.ssh/config");
        assert_eq!(m.display_path(), "~/.ssh/config");
        m.set_config_path("/etc/ssh/ssh_config".into());
        assert_eq!(m.display_path(), "/etc/ssh/ssh_config");
    }
}
