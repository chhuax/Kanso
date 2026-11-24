//! `.kanso` 数据文件的导出与导入（store-import.md §6.1、§6.2）。
//!
//! # 文件格式（formatVersion 1）
//!
//! 顶层是 JSON 对象：`app`（固定 [`APP_MARKER`]）、`formatVersion`、`appVersion`、`exportedAt`
//! （Unix 毫秒）、`groups` / `profiles` / `snippets` 数组、`settings` 对象（键到任意 JSON）。
//! **绝不包含凭据**：本模块的 DTO 里没有任何凭据字段，导入时文件里多出来的字段（包括密码）
//! 因 serde 默认忽略未知字段而被丢弃。
//!
//! # 为什么用独立的 DTO
//!
//! 文件格式是对外承诺，字段名必须稳定；`kanso-types` 的内部类型改字段名不应破坏旧文件，
//! 所以这里的 DTO 单独定义，与数据库行（`config` 列直接序列化 `ProfileKind`）也解耦。
//! 枚举一律用小写 kebab 文本，字段名 camelCase。id 复用 `kanso-types` 的 id 类型，
//! 它们是带连字符的 UUID 字符串，属于稳定表示。
//!
//! # 逐条解析
//!
//! 顶层先解析成 `serde_json::Value`，各数组元素再单独反序列化：单条有问题（字段缺失、未知会话类型、
//! 校验不通过）只跳过这一条并记入报告，不让整个文件失败（旧版曾因一条不支持的类型整个文件导不进来）。

use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use kanso_types::message::Message;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, LineEnding, LocalProfile, Locale, Profile, ProfileKind,
    SessionKind, Snippet, SnippetKindScope, SnippetScope, SshProfile, SshTarget,
};
use kanso_types::{GroupId, ProfileId, SnippetId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::schema::StoreError;
use crate::store::{
    MAX_SNIPPETS, Store, color_from_hex, color_to_hex, groups_on, live_group_ids, live_profile_ids,
    now_ms, profiles_on, snippet_count, snippet_live, snippets_on, upsert_group_on,
    upsert_profile_on, upsert_setting_on, upsert_snippet_on,
};

/// 新导出文件的应用标记；导入另接受旧版 `"Kanso"` 的 1–3 版。
pub const APP_MARKER: &str = "kanso-data";

/// 当前程序认识的最高格式版本。
pub const FORMAT_VERSION: u32 = 1;

/// 数据文件扩展名（不含点）。
pub const EXTENSION: &str = "kanso";

/// 导入时用的 Profile 校验函数：参数是待校验的 Profile 与合并后的全部 Profile。
pub type ProfileValidator = dyn Fn(&Profile, &[Profile]) -> Result<(), Message>;

/// 导入结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOutcome {
    /// 新增条数（分组、会话、Sender 命令之和；设置不计数）。
    pub added: u32,
    /// 更新条数。
    pub updated: u32,
    /// 被跳过的条目及原因。
    pub skipped: Vec<Message>,
    /// 已转换的命令及原因。
    pub converted: Vec<Message>,
}

/// 导入失败（整个文件被拒绝或无法读取）。单条数据的问题不在此列，只进 [`ImportOutcome::skipped`]。
#[derive(Debug)]
pub enum ImportError {
    /// 扩展名不是 `.kanso`。
    WrongExtension,
    /// 不是 Kanso 数据文件（非 JSON、没有应用标记、版本字段缺失等）。
    NotKansoFile,
    /// 文件格式版本高于当前程序认识的版本。
    VersionTooNew {
        /// 文件里的版本。
        version: i64,
        /// 当前程序支持的最高版本。
        supported: i64,
    },
    /// 读文件失败。
    Io(std::io::Error),
    /// 数据库错误；整个导入已回滚。
    Store(StoreError),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::WrongExtension => f.write_str("file extension is not .kanso"),
            ImportError::NotKansoFile => f.write_str("not a Kanso data file"),
            ImportError::VersionTooNew { version, supported } => {
                write!(
                    f,
                    "file format version {version} is newer than supported {supported}"
                )
            }
            ImportError::Io(e) => write!(f, "io error: {e}"),
            ImportError::Store(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<StoreError> for ImportError {
    fn from(e: StoreError) -> Self {
        ImportError::Store(e)
    }
}

impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        ImportError::Store(StoreError::Sqlite(e))
    }
}

/// 确保路径带 `.kanso` 扩展名（不区分大小写）；没有就在文件名末尾追加（`a.json` → `a.json.kanso`）。
pub fn with_kanso_extension(path: &Path) -> PathBuf {
    if has_kanso_extension(path) {
        return path.to_owned();
    }
    let mut s = path.as_os_str().to_owned();
    s.push(".kanso");
    PathBuf::from(s)
}

fn has_kanso_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

// ---- DTO ----

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupDto {
    id: GroupId,
    name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileDto {
    id: ProfileId,
    name: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    group: Option<GroupId>,
    /// `local` / `ssh` / `sftp`。用字符串而不是枚举：未知类型要能识别出来并按"不支持的类型"跳过，
    /// 而不是整条反序列化失败。
    kind: String,
    #[serde(default)]
    config: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalCfg {
    #[serde(default)]
    shell_command: String,
    #[serde(default)]
    start_dir: Option<String>,
    #[serde(default)]
    encoding: String,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AuthDto {
    Password,
    PrivateKey,
    Agent,
}

impl From<AuthDto> for AuthMethod {
    fn from(a: AuthDto) -> Self {
        match a {
            AuthDto::Password => AuthMethod::Password,
            AuthDto::PrivateKey => AuthMethod::PrivateKey,
            AuthDto::Agent => AuthMethod::Agent,
        }
    }
}

impl From<AuthMethod> for AuthDto {
    fn from(a: AuthMethod) -> Self {
        match a {
            AuthMethod::Password => AuthDto::Password,
            AuthMethod::PrivateKey => AuthDto::PrivateKey,
            AuthMethod::Agent => AuthDto::Agent,
        }
    }
}

fn default_port() -> u16 {
    kanso_types::profile::DEFAULT_SSH_PORT
}

fn default_auth() -> AuthDto {
    AuthDto::Password
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetCfg {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    user: String,
    #[serde(default = "default_auth")]
    auth: AuthDto,
    #[serde(default)]
    private_key_path: Option<String>,
    #[serde(default)]
    jump: Option<ProfileId>,
    #[serde(default)]
    proxy_command: Option<String>,
    /// 保活配置；缺省时用默认值（兼容旧导出文件）。
    #[serde(default)]
    keepalive: kanso_types::profile::KeepaliveConfig,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SshCfg {
    #[serde(flatten)]
    target: TargetCfg,
    #[serde(default)]
    encoding: String,
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnippetDto {
    id: SnippetId,
    name: String,
    text: String,
    /// `none` / `lf` / `crlf`。
    line_ending: String,
    #[serde(default)]
    scope: ScopeDto,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopeDto {
    /// `profile` / `group` / `kind` / `global`。
    kind: String,
    /// profile / group 的 id，或 `local` / `ssh`；global 时省略。
    #[serde(default, rename = "ref")]
    target: Option<String>,
}

impl Default for ScopeDto {
    fn default() -> Self {
        Self {
            kind: "global".into(),
            target: None,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportFile {
    app: &'static str,
    format_version: u32,
    app_version: String,
    exported_at: i64,
    groups: Vec<GroupDto>,
    profiles: Vec<ProfileDto>,
    snippets: Vec<SnippetDto>,
    settings: Map<String, Value>,
}

// ---- Profile / Snippet 与 DTO 互转 ----

fn target_to_cfg(t: &SshTarget) -> TargetCfg {
    TargetCfg {
        host: t.host.clone(),
        port: t.port,
        user: t.user.clone(),
        auth: t.auth.into(),
        private_key_path: t.private_key_path.clone(),
        jump: t.jump,
        proxy_command: t.proxy_command.clone(),
        keepalive: t.keepalive.clone(),
    }
}

fn cfg_to_target(c: TargetCfg) -> SshTarget {
    SshTarget {
        host: c.host,
        port: c.port,
        user: c.user,
        auth: c.auth.into(),
        private_key_path: c.private_key_path,
        jump: c.jump,
        proxy_command: c.proxy_command,
        keepalive: c.keepalive,
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, StoreError> {
    serde_json::to_value(v).map_err(|e| StoreError::InvalidData {
        detail: format!("cannot serialize export data: {e}"),
    })
}

fn profile_to_dto(p: &Profile) -> Result<ProfileDto, StoreError> {
    let (kind, config) = match &p.kind {
        ProfileKind::Local(l) => (
            "local",
            to_value(&LocalCfg {
                shell_command: l.shell_command.clone(),
                start_dir: l.start_dir.clone(),
                encoding: l.encoding.as_str().to_owned(),
                locale: l.locale.as_ref().map(|x| x.as_str().to_owned()),
            })?,
        ),
        ProfileKind::Ssh(s) => (
            "ssh",
            to_value(&SshCfg {
                target: target_to_cfg(&s.target),
                encoding: s.encoding.as_str().to_owned(),
                locale: s.locale.as_ref().map(|x| x.as_str().to_owned()),
            })?,
        ),
        ProfileKind::Sftp(t) => ("sftp", to_value(&target_to_cfg(t))?),
    };
    Ok(ProfileDto {
        id: p.id,
        name: p.name.clone(),
        color: p.color.map(color_to_hex),
        group: p.group,
        kind: kind.to_owned(),
        config,
    })
}

/// 解析 locale 文本；非法（含空串）报错，由调用方转成"跳过该条"。
fn parse_locale(s: Option<String>) -> Result<Option<Locale>, String> {
    match s {
        None => Ok(None),
        Some(s) => Locale::parse(&s)
            .map(Some)
            .ok_or_else(|| format!("invalid locale {s:?}")),
    }
}

/// 条目解析失败的原因。
enum ProfileParseError {
    /// 不支持的会话类型。
    UnsupportedKind(String),
    /// 其他（字段缺失、取值非法）。
    Invalid(String),
}

fn dto_to_profile(dto: ProfileDto) -> Result<Profile, ProfileParseError> {
    use ProfileParseError::{Invalid, UnsupportedKind};
    let invalid = |e: serde_json::Error| Invalid(format!("config: {e}"));
    let kind = match dto.kind.as_str() {
        "local" => {
            let c: LocalCfg = serde_json::from_value(dto.config).map_err(invalid)?;
            ProfileKind::Local(LocalProfile {
                shell_command: c.shell_command,
                start_dir: c.start_dir.filter(|d| !d.is_empty()),
                encoding: EncodingLabel::new(&c.encoding),
                locale: parse_locale(c.locale).map_err(Invalid)?,
            })
        }
        "ssh" => {
            let c: SshCfg = serde_json::from_value(dto.config).map_err(invalid)?;
            ProfileKind::Ssh(SshProfile {
                target: cfg_to_target(c.target),
                encoding: EncodingLabel::new(&c.encoding),
                locale: parse_locale(c.locale).map_err(Invalid)?,
            })
        }
        "sftp" => {
            let c: TargetCfg = serde_json::from_value(dto.config).map_err(invalid)?;
            ProfileKind::Sftp(cfg_to_target(c))
        }
        other => return Err(UnsupportedKind(other.to_owned())),
    };
    let color = match dto.color {
        None => None,
        Some(c) => Some(color_from_hex(&c).ok_or_else(|| Invalid(format!("invalid color {c:?}")))?),
    };
    Ok(Profile {
        id: dto.id,
        name: dto.name,
        color,
        group: dto.group,
        kind,
    })
}

fn snippet_to_dto(s: &Snippet) -> SnippetDto {
    let (kind, target) = match &s.scope {
        SnippetScope::Profile(id) => ("profile", Some(id.to_string())),
        SnippetScope::Group(id) => ("group", Some(id.to_string())),
        SnippetScope::Kind(SnippetKindScope::Local) => ("kind", Some("local".to_owned())),
        SnippetScope::Kind(SnippetKindScope::Ssh) => ("kind", Some("ssh".to_owned())),
        SnippetScope::Global => ("global", None),
    };
    SnippetDto {
        id: s.id,
        name: s.name.clone(),
        text: s.text.clone(),
        line_ending: match s.line_ending {
            LineEnding::None => "none",
            LineEnding::Lf => "lf",
            LineEnding::CrLf => "crlf",
        }
        .to_owned(),
        scope: ScopeDto {
            kind: kind.to_owned(),
            target,
        },
    }
}

fn dto_to_snippet(dto: SnippetDto) -> Result<Snippet, String> {
    if dto.text.trim().is_empty() {
        return Err("the command cannot be blank".into());
    }
    let line_ending = match dto.line_ending.as_str() {
        "none" => LineEnding::None,
        "lf" => LineEnding::Lf,
        "crlf" => LineEnding::CrLf,
        other => return Err(format!("invalid lineEnding {other:?}")),
    };
    let target = dto.scope.target.as_deref();
    let scope = match (dto.scope.kind.as_str(), target) {
        ("global", _) => SnippetScope::Global,
        ("profile", Some(r)) => SnippetScope::Profile(
            ProfileId::from_str(r).map_err(|_| format!("invalid scope ref {r:?}"))?,
        ),
        ("group", Some(r)) => SnippetScope::Group(
            GroupId::from_str(r).map_err(|_| format!("invalid scope ref {r:?}"))?,
        ),
        ("kind", Some("local")) => SnippetScope::Kind(SnippetKindScope::Local),
        ("kind", Some("ssh")) => SnippetScope::Kind(SnippetKindScope::Ssh),
        ("kind", Some("sftp")) => SnippetScope::Global,
        (k, r) => return Err(format!("invalid scope {k:?} ref {r:?}")),
    };
    Ok(Snippet {
        id: dto.id,
        name: dto.name,
        text: dto.text,
        line_ending,
        scope,
    })
}

// ---- 导出 ----

/// 不含凭据的导出快照，编码与文件写入不再需要访问数据库。
pub struct ExportSnapshot {
    groups: Vec<Group>,
    profiles: Vec<Profile>,
    snippets: Vec<Snippet>,
    settings: Vec<(String, String)>,
}

impl ExportSnapshot {
    /// 编码并原子写入 `.kanso` 文件。
    pub fn write(self, path: &Path, app_version: &str) -> Result<PathBuf, StoreError> {
        let target = with_kanso_extension(path);
        let mut settings = Map::new();
        for (k, v) in self.settings {
            // 库里的值写入时已校验为 JSON；万一不是（手工改库），跳过而不是让导出失败。
            if let Ok(val) = serde_json::from_str::<Value>(&v) {
                settings.insert(k, val);
            }
        }
        let file = ExportFile {
            app: APP_MARKER,
            format_version: FORMAT_VERSION,
            app_version: app_version.to_owned(),
            exported_at: now_ms(),
            groups: self
                .groups
                .into_iter()
                .map(|g| GroupDto {
                    id: g.id,
                    name: g.name,
                })
                .collect(),
            profiles: self
                .profiles
                .iter()
                .map(profile_to_dto)
                .collect::<Result<_, _>>()?,
            snippets: self.snippets.iter().map(snippet_to_dto).collect(),
            settings,
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| StoreError::InvalidData {
            detail: format!("cannot serialize export file: {e}"),
        })?;

        let mut tmp_name = target.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &target)
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Io(e));
        }
        Ok(target)
    }
}

impl Store {
    /// 读取一致的导出视图，不编码或写入导出文件。
    pub fn export_snapshot(&self) -> Result<ExportSnapshot, StoreError> {
        Ok(ExportSnapshot {
            groups: groups_on(&self.conn)?,
            profiles: profiles_on(&self.conn)?,
            snippets: snippets_on(&self.conn)?,
            settings: self
                .settings_all()?
                .into_iter()
                .filter(|(key, _)| key != crate::LOCAL_SHELL_DEFAULTS)
                .collect(),
        })
    }

    /// 导出全部数据，不含凭据；需要缩短外层持锁范围时分开调用快照与写入。
    pub fn export(&self, path: &Path, app_version: &str) -> Result<PathBuf, StoreError> {
        self.export_snapshot()?.write(path, app_version)
    }

    /// 从 `.kanso` 文件导入（合并规则见模块文档与 store-import.md §6.2）。
    ///
    /// `validate` 是 Profile 保存前的校验（core 的 `validate_profile`；校验逻辑在 core，
    /// store 不能反向依赖它，所以由调用方传入）。参数是待校验的 Profile 与合并后的全部 Profile。
    ///
    /// 合并：按 id 匹配，相同 id 用文件里的替换本地；其余新增；**不删除本地任何数据**。
    /// 凭据不在数据库里，被替换的会话天然保留本地凭据。
    /// 整个导入在一个事务里完成，数据库层面出错则全部回滚。
    pub fn import_file(
        &mut self,
        path: &Path,
        validate: &ProfileValidator,
    ) -> Result<ImportOutcome, ImportError> {
        self.import_data(ImportData::read(path)?, validate)
    }

    /// 合并已经读入并解码的文件数据，整个合并在同一个事务中完成。
    pub fn import_data(
        &mut self,
        data: ImportData,
        validate: &ProfileValidator,
    ) -> Result<ImportOutcome, ImportError> {
        let tx = self.conn.transaction()?;
        let now = now_ms();
        let mut added = 0u32;
        let mut updated = 0u32;
        let mut skipped = data.skipped;
        let mut converted = Vec::new();

        let invalid = |name: String, detail: String| Message::ImportSkippedInvalid { name, detail };

        // 分组。
        let live_groups = live_group_ids(&tx)?;
        let mut imported_groups: HashSet<GroupId> = HashSet::new();
        for g in data.groups {
            if live_groups.contains(&g.id) {
                updated += 1;
            } else {
                added += 1;
            }
            upsert_group_on(
                &tx,
                &Group {
                    id: g.id,
                    name: g.name,
                },
                now,
            )?;
            imported_groups.insert(g.id);
        }

        // 会话：先全部解析，再统一修正引用与校验。
        let live_profiles = live_profile_ids(&tx)?;
        let candidates = data.profiles;
        // 文件里同一个 id 出现多次：后者覆盖前者，避免重复计数。
        let mut seen = HashSet::new();
        let mut dedup: Vec<Profile> = Vec::new();
        for p in candidates.into_iter().rev() {
            if seen.insert(p.id) {
                dedup.push(p);
            }
        }
        dedup.reverse();
        let mut candidates = dedup;

        let all_groups: HashSet<GroupId> = live_groups.union(&imported_groups).copied().collect();
        for p in &mut candidates {
            // 分组不存在 → 顶层（store-import.md §3）。
            p.group = p.group.filter(|g| all_groups.contains(g));
        }
        // 本地已有的会话（含被文件里同 id 版本替换前的）构成校验时的"现有集合"；
        // 候选覆盖同 id 的本地项。
        let local = profiles_on(&tx)?;
        loop {
            let cand_ids: HashSet<ProfileId> = candidates.iter().map(|p| p.id).collect();
            let mut merged: Vec<Profile> = local
                .iter()
                .filter(|p| !cand_ids.contains(&p.id))
                .cloned()
                .collect();
            merged.extend(candidates.iter().cloned());
            let ssh_ids: HashSet<ProfileId> = merged
                .iter()
                .filter(|p| p.kind.kind() == SessionKind::Ssh)
                .map(|p| p.id)
                .collect();
            // 跳板引用失效（目标不存在或不是 SSH）→ 清除（store-import.md §6.2）。
            for p in &mut candidates {
                clear_dangling_jump(p, &ssh_ids);
            }
            // 清除后重新合并再校验，保证校验看到的是最终内容。
            let mut merged: Vec<Profile> = local
                .iter()
                .filter(|p| !cand_ids.contains(&p.id))
                .cloned()
                .collect();
            merged.extend(candidates.iter().cloned());
            // 本轮校验失败的条目一起移除，然后重来：它们消失后，引用它们的跳板需要再次清除。
            let failed: Vec<(ProfileId, Message)> = candidates
                .iter()
                .filter_map(|p| validate(p, &merged).err().map(|m| (p.id, m)))
                .collect();
            if failed.is_empty() {
                break;
            }
            for (id, m) in failed {
                if let Some(pos) = candidates.iter().position(|p| p.id == id) {
                    let p = candidates.remove(pos);
                    skipped.push(invalid(p.name, format!("validation failed: {}", m.key())));
                }
            }
        }
        for p in &candidates {
            if live_profiles.contains(&p.id) {
                updated += 1;
            } else {
                added += 1;
            }
            upsert_profile_on(&tx, p, now)?;
        }

        // Sender 命令：会话与分组都已落库，作用域引用以库里的最终状态为准。
        let live_groups = live_group_ids(&tx)?;
        let live_profiles = live_profile_ids(&tx)?;
        for (snippet, was_sftp) in data.snippets {
            let scope = match snippet.scope {
                SnippetScope::Profile(p) if !live_profiles.contains(&p) => SnippetScope::Global,
                SnippetScope::Group(g) if !live_groups.contains(&g) => SnippetScope::Global,
                other => other,
            };
            let snippet = Snippet { scope, ..snippet };
            let exists = snippet_live(&tx, snippet.id)?;
            if !exists && snippet_count(&tx)? >= MAX_SNIPPETS {
                skipped.push(invalid(
                    snippet.name,
                    format!("snippet limit of {MAX_SNIPPETS} reached"),
                ));
                continue;
            }
            upsert_snippet_on(&tx, &snippet, now)?;
            if was_sftp {
                converted.push(Message::ImportSnippetConverted {
                    name: snippet.name.clone(),
                });
            }
            if exists {
                updated += 1;
            } else {
                added += 1;
            }
        }

        // 设置：同键覆盖，不在本地的键新增。不计入新增 / 更新数。
        for (key, value) in data.settings {
            if key != crate::LOCAL_SHELL_DEFAULTS {
                upsert_setting_on(&tx, &key, &value, now)?;
            }
        }

        tx.commit()?;
        Ok(ImportOutcome {
            added,
            updated,
            skipped,
            converted,
        })
    }
}

/// 已完成文件读取与格式解析的数据，等待在数据库事务内合并。
pub struct ImportData {
    groups: Vec<Group>,
    profiles: Vec<Profile>,
    snippets: Vec<(Snippet, bool)>,
    settings: Vec<(String, String)>,
    skipped: Vec<Message>,
}

impl ImportData {
    /// 读取并逐条解码文件；单条错误仍由合并报告返回。
    pub fn read(path: &Path) -> Result<Self, ImportError> {
        if !has_kanso_extension(path) {
            return Err(ImportError::WrongExtension);
        }
        let bytes = std::fs::read(path).map_err(ImportError::Io)?;
        let root = parse_header(&bytes)?;
        let mut data = Self {
            groups: Vec::new(),
            profiles: Vec::new(),
            snippets: Vec::new(),
            settings: Vec::new(),
            skipped: Vec::new(),
        };
        let invalid = |name: String, detail: String| Message::ImportSkippedInvalid { name, detail };
        for (index, value) in section(&root, "groups", &mut data.skipped) {
            match serde_json::from_value::<GroupDto>(value.clone()) {
                Ok(group) => data.groups.push(Group {
                    id: group.id,
                    name: group.name,
                }),
                Err(error) => data.skipped.push(invalid(
                    entry_name(value, "groups", index),
                    error.to_string(),
                )),
            }
        }
        for (index, value) in section(&root, "profiles", &mut data.skipped) {
            let name = entry_name(value, "profiles", index);
            let profile = match serde_json::from_value::<ProfileDto>(value.clone()) {
                Ok(profile) => profile,
                Err(error) => {
                    data.skipped.push(invalid(name, error.to_string()));
                    continue;
                }
            };
            match dto_to_profile(profile) {
                Ok(profile) => data.profiles.push(profile),
                Err(ProfileParseError::UnsupportedKind(kind)) => data
                    .skipped
                    .push(Message::ImportSkippedUnsupportedKind { name, kind }),
                Err(ProfileParseError::Invalid(detail)) => data.skipped.push(invalid(name, detail)),
            }
        }
        for (index, value) in section(&root, "snippets", &mut data.skipped) {
            match serde_json::from_value::<SnippetDto>(value.clone())
                .map_err(|error| error.to_string())
                .and_then(dto_to_snippet)
            {
                Ok(snippet) => {
                    let was_sftp = value.pointer("/scope/kind").and_then(Value::as_str)
                        == Some("kind")
                        && value.pointer("/scope/ref").and_then(Value::as_str) == Some("sftp");
                    data.snippets.push((snippet, was_sftp));
                }
                Err(detail) => data
                    .skipped
                    .push(invalid(entry_name(value, "snippets", index), detail)),
            }
        }
        if let Some(settings) = root.get("settings") {
            match settings.as_object() {
                Some(settings) => {
                    for (key, value) in settings {
                        if key.is_empty() {
                            data.skipped
                                .push(invalid(key.clone(), "empty setting key".to_owned()));
                        } else {
                            data.settings.push((key.clone(), value.to_string()));
                        }
                    }
                }
                None => data
                    .skipped
                    .push(invalid("settings".to_owned(), "not an object".to_owned())),
            }
        }
        Ok(data)
    }
}

/// 清除指向不存在或非 SSH 会话的跳板引用。
fn clear_dangling_jump(p: &mut Profile, ssh_ids: &HashSet<ProfileId>) {
    let target = match &mut p.kind {
        ProfileKind::Ssh(s) => &mut s.target,
        ProfileKind::Sftp(t) => t,
        ProfileKind::Local(_) => return,
    };
    if target.jump.is_some_and(|j| !ssh_ids.contains(&j)) {
        target.jump = None;
    }
}

/// 解析并检查文件头；返回顶层对象。
fn parse_header(bytes: &[u8]) -> Result<Map<String, Value>, ImportError> {
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(bytes) else {
        return Err(ImportError::NotKansoFile);
    };
    let supported = match root.get("app").and_then(Value::as_str) {
        Some(APP_MARKER) => FORMAT_VERSION,
        Some("Kanso") => 3,
        _ => return Err(ImportError::NotKansoFile),
    };
    let Some(version) = root.get("formatVersion").and_then(Value::as_i64) else {
        return Err(ImportError::NotKansoFile);
    };
    if version < 1 {
        return Err(ImportError::NotKansoFile);
    }
    if version > i64::from(supported) {
        return Err(ImportError::VersionTooNew {
            version,
            supported: i64::from(supported),
        });
    }
    Ok(root)
}

/// 取数组段的元素（带下标）；段缺失视为空；段存在但不是数组时记一条跳过。
fn section<'a>(
    root: &'a Map<String, Value>,
    key: &str,
    skipped: &mut Vec<Message>,
) -> Vec<(usize, &'a Value)> {
    match root.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.iter().enumerate().collect(),
        Some(_) => {
            skipped.push(Message::ImportSkippedInvalid {
                name: key.to_owned(),
                detail: "not an array".to_owned(),
            });
            Vec::new()
        }
    }
}

/// 用于报告的条目名：优先取 `name` 字段，否则用 `段[下标]`。
fn entry_name(v: &Value, section: &str, index: usize) -> String {
    v.get("name")
        .and_then(Value::as_str)
        .map_or_else(|| format!("{section}[{index}]"), str::to_owned)
}
