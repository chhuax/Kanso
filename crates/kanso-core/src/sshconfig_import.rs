//! OpenSSH 配置导入（F6，store-import.md §7）：扫描候选与导入规划。
//!
//! 解析与求值由 `kanso-sshconfig` 负责；这里只做两件事：
//! - [`candidates`]：把求值结果和已保存的 Profile 对照，标出“已保存的主机”；
//! - [`plan_import`]：纯函数，根据当前已保存的 Profile 算出要写入的 Profile 与报告。
//!   规划与写库分开，是为了让"全部写入在一个事务里"成立：规划阶段只改内存里的副本，
//!   最后由调用方一次性交给 `Store::upsert_profiles`；任何一步失败都不会留下半截状态。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use kanso_session::EnvSnapshot;
use kanso_sshconfig::{Env, Resolved, ScanResult, StdFs, Warning};
use kanso_types::message::Reason;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::{GroupId, Message, ProfileId};

use crate::api::{CoreError, Result, SshConfigCandidate, SshConfigImportReport, SshConfigScan};
use crate::validate::validate_profile;

/// 求值所需的本机环境，来自 core 创建时的基础环境快照（与 SSH 取 `HOME` 的方式一致）。
#[derive(Clone, Debug)]
pub(crate) struct SshConfigEnv {
    home: Option<PathBuf>,
    local_user: String,
    local_host: String,
    local_uid: u32,
}

impl SshConfigEnv {
    /// 从环境快照取值。
    ///
    /// 不用 libc 查 passwd（工作区禁止 unsafe），所以本地用户名只认 `USER` / `LOGNAME`，
    /// 主机名认 `HOSTNAME`，uid 认 `UID`；取不到时分别为空串 / `localhost` / 0。
    /// 本地用户名为空时，没写 `User` 的别名无法得到用户名，导入阶段会按"用户名为空"告警并跳过，
    /// 不去编造一个用户名。
    pub(crate) fn from_snapshot(env: &EnvSnapshot) -> Self {
        let text = |key: &str| {
            env.get_non_empty(key)
                .map(|v| v.to_string_lossy().into_owned())
        };
        SshConfigEnv {
            home: env.get_non_empty("HOME").map(PathBuf::from),
            local_user: text("USER").or_else(|| text("LOGNAME")).unwrap_or_default(),
            local_host: text("HOSTNAME").unwrap_or_else(|| "localhost".to_owned()),
            local_uid: text("UID").and_then(|v| v.parse().ok()).unwrap_or(0),
        }
    }

    fn to_env(&self) -> Env {
        Env {
            // 显式指定了文件但没有 HOME 时，用空路径：`~` 相关展开会落在相对路径上、通常读不到，
            // 与 ssh 在 HOME 缺失时的"跳过读不到的文件"一致。
            home: self.home.clone().unwrap_or_default(),
            local_user: self.local_user.clone(),
            local_host: self.local_host.clone(),
            local_uid: self.local_uid,
        }
    }
}

/// 一次读取并求值的结果。
pub(crate) struct Loaded {
    path: PathBuf,
    env: Env,
    scan: ScanResult,
}

/// 读取并求值 `path`（`None` 为 `<HOME>/.ssh/config`）。阻塞读盘，调用方应在 `spawn_blocking` 里调用。
///
/// 主文件读不到必须报错：`kanso_sshconfig::scan` 对读不到的主文件返回空结果（那是 ssh 的行为），
/// 但对导入对话框来说“文件不存在”和“文件里没有主机”必须能区分。
pub(crate) fn load(env: &SshConfigEnv, path: Option<&Path>) -> Result<Loaded> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => match &env.home {
            Some(h) => h.join(".ssh").join("config"),
            None => {
                return Err(CoreError::User(Message::ImportReadFailed {
                    path: "~/.ssh/config".to_owned(),
                    reason: Reason::NotFound,
                }));
            }
        },
    };
    if let Err(e) = std::fs::read(&path) {
        return Err(CoreError::User(Message::ImportReadFailed {
            path: path.to_string_lossy().into_owned(),
            reason: crate::real::io_reason(&e),
        }));
    }
    let env = env.to_env();
    let scan = kanso_sshconfig::scan(&path, &StdFs, &env);
    Ok(Loaded { path, env, scan })
}

/// 把 T3 的结构化警告映射成面向用户的消息。`alias` 只用于与别名相关的警告；
/// 文件级警告（`Include` 超限、`Match` 不支持）不用它。
fn warning_message(w: &Warning, alias: &str) -> Message {
    match w {
        Warning::IncludeTooDeep { file, include } => Message::SshConfigIncludeTooDeep {
            file: file.to_string_lossy().into_owned(),
            include: include.to_string_lossy().into_owned(),
        },
        Warning::MatchUnsupported { file } => Message::SshConfigMatchUnsupported {
            file: file.to_string_lossy().into_owned(),
        },
        Warning::InvalidPort { value } => Message::SshConfigInvalidPort {
            alias: alias.to_owned(),
            value: value.clone(),
        },
        Warning::UnknownToken {
            option,
            value: _,
            token,
        } => Message::SshConfigUnknownToken {
            alias: alias.to_owned(),
            option: (*option).to_owned(),
            token: token.to_string(),
        },
    }
}

/// 按 `,` 拆分 ProxyJump 原文，忽略空段。
fn jump_hops(spec: &str) -> Vec<&str> {
    spec.split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect()
}

fn ssh_target_of(p: &Profile) -> Option<&SshTarget> {
    match &p.kind {
        ProfileKind::Ssh(s) => Some(&s.target),
        _ => None,
    }
}

fn same_endpoint(t: &SshTarget, host: &str, port: u16, user: &str) -> bool {
    // 主机名不区分大小写；用户名区分。
    t.host.eq_ignore_ascii_case(host) && t.port == port && t.user == user
}

/// 在已保存的 SSH 会话里找“已保存的主机”（§7.2）：名称等于别名优先，其次主机 + 端口 + 用户名完全相同。
/// `by_name` 为假时只按连接目标找（`[用户@]主机[:端口]` 形式的跳板机没有别名，名称不具备比较意义）。
fn find_saved(
    profiles: &[Profile],
    name: &str,
    by_name: bool,
    host: &str,
    port: u16,
    user: &str,
) -> Option<ProfileId> {
    let mut ssh = profiles
        .iter()
        .filter_map(|p| ssh_target_of(p).map(|t| (p, t)));
    if by_name && let Some((p, _)) = ssh.clone().find(|(p, _)| p.name == name) {
        return Some(p.id);
    }
    ssh.find(|(_, t)| same_endpoint(t, host, port, user))
        .map(|(p, _)| p.id)
}

/// 扫描结果转候选列表与文件级警告。
///
/// 文件级警告不属于任何别名，单独放在 `SshConfigScan::warnings`；导入报告里同样会给出。
pub(crate) fn scan_result(loaded: &Loaded, profiles: &[Profile]) -> SshConfigScan {
    let file_warnings: Vec<Message> = loaded
        .scan
        .warnings
        .iter()
        .map(|w| warning_message(w, ""))
        .collect();
    let candidates: Vec<SshConfigCandidate> = loaded
        .scan
        .hosts
        .iter()
        .map(|r| {
            let mut warnings: Vec<Message> = r
                .warnings
                .iter()
                .map(|w| warning_message(w, &r.alias))
                .collect();
            if r.proxy_jump
                .as_deref()
                .is_some_and(|s| jump_hops(s).len() > 1)
            {
                warnings.push(Message::SshConfigMultiHopIgnored {
                    alias: r.alias.clone(),
                });
            }
            SshConfigCandidate {
                alias: r.alias.clone(),
                host: r.host_name.clone(),
                port: r.port,
                user: r.user.clone(),
                // 显式写了的优先；没写时给出探测到的默认私钥。区分显式/默认只在导入时重新求值判断，
                // 候选结构没有对应字段。
                identity_file: r
                    .identity_file
                    .as_ref()
                    .or(r.default_identity.as_ref())
                    .map(|p| p.to_string_lossy().into_owned()),
                proxy_jump: r.proxy_jump.clone(),
                existing: find_saved(profiles, &r.alias, true, &r.host_name, r.port, &r.user),
                warnings,
            }
        })
        .collect();
    SshConfigScan {
        candidates,
        warnings: file_warnings,
    }
}

/// 要创建 / 更新的连接目标（来自别名求值，或 `[用户@]主机[:端口]` 形式的跳板机）。
struct Target {
    name: String,
    host: String,
    port: u16,
    user: String,
    /// 配置里显式写了的私钥。
    explicit_key: Option<String>,
    /// 没写 `IdentityFile` 时探测到的默认私钥。
    default_key: Option<String>,
}

impl Target {
    fn from_resolved(r: &Resolved) -> Target {
        let s = |p: &PathBuf| p.to_string_lossy().into_owned();
        Target {
            name: r.alias.clone(),
            host: r.host_name.clone(),
            port: r.port,
            user: r.user.clone(),
            explicit_key: r.identity_file.as_ref().map(s),
            default_key: r.default_identity.as_ref().map(s),
        }
    }
}

/// `[用户@]主机[:端口]` 形式的单跳。
struct JumpSpec {
    user: Option<String>,
    host: String,
    port: Option<u16>,
}

/// 解析 `[用户@]主机[:端口]`；IPv6 必须写成 `[地址]:端口` 或 `[地址]`，没有方括号且含多个 `:` 的
/// 整体按主机名处理（裸 IPv6，无端口）。解析不了返回 `None`。
fn parse_jump_spec(s: &str) -> Option<JumpSpec> {
    let s = s.trim();
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) => {
            if u.is_empty() {
                return None;
            }
            (Some(u.to_owned()), r)
        }
        None => (None, s),
    };
    let (host, port) = if let Some(r) = rest.strip_prefix('[') {
        let (addr, tail) = r.split_once(']')?;
        let port = if tail.is_empty() {
            None
        } else {
            Some(tail.strip_prefix(':')?)
        };
        (addr, port)
    } else {
        match rest.split_once(':') {
            Some((h, p)) if !p.contains(':') => (h, Some(p)),
            _ => (rest, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => None,
    };
    Some(JumpSpec {
        user,
        host: host.to_owned(),
        port,
    })
}

struct Planner<'a> {
    loaded: &'a Loaded,
    group: Option<GroupId>,
    /// 当前视图：已保存的 Profile 加上本次新建 / 修改的结果。校验跳板链要在这份视图上做。
    all: Vec<Profile>,
    /// 需要写入的 Profile id，按首次触及的顺序（新记录按此顺序排在最后）。
    touched: Vec<ProfileId>,
    created: HashSet<ProfileId>,
    /// 已计入 `updated` 的已有 Profile，避免两个别名命中同一会话时重复计数。
    counted_updates: HashSet<ProfileId>,
    /// 小写别名 → 本次导入里对应的 Profile，跳板机引用别名时复用。
    alias_ids: HashMap<String, ProfileId>,
    report: SshConfigImportReport,
}

impl Planner<'_> {
    fn warn(&mut self, m: Message) {
        self.report.warnings.push(m);
    }

    fn touch(&mut self, id: ProfileId) {
        if !self.touched.contains(&id) {
            self.touched.push(id);
        }
    }

    fn index_of(&self, id: ProfileId) -> Option<usize> {
        self.all.iter().position(|p| p.id == id)
    }

    /// 新建会话：显式私钥 → 私钥；无显式但探测到默认私钥 → 私钥；都没有 → 密码。
    fn build_new(&self, t: &Target) -> Profile {
        let key = t.explicit_key.clone().or_else(|| t.default_key.clone());
        Profile {
            id: ProfileId::new(),
            name: t.name.clone(),
            color: None,
            group: self.group,
            kind: ProfileKind::Ssh(SshProfile {
                target: SshTarget {
                    host: t.host.clone(),
                    port: t.port,
                    user: t.user.clone(),
                    auth: if key.is_some() {
                        AuthMethod::PrivateKey
                    } else {
                        AuthMethod::Password
                    },
                    private_key_path: key,
                    jump: None,
                    proxy_command: None,
                    keepalive: kanso_types::profile::KeepaliveConfig::default(),
                },
                encoding: EncodingLabel::default(),
                locale: None,
            }),
        }
    }

    /// 校验 `p`（对照当前视图）；失败时记警告并返回 `false`。
    fn check(&mut self, p: &Profile) -> bool {
        match validate_profile(p, &self.all) {
            Ok(()) => true,
            Err(m) => {
                self.warn(m);
                false
            }
        }
    }

    fn create(&mut self, t: &Target) -> Option<ProfileId> {
        let p = self.build_new(t);
        if !self.check(&p) {
            return None;
        }
        let id = p.id;
        self.all.push(p);
        self.touch(id);
        self.created.insert(id);
        Some(id)
    }

    /// 导入用户勾选的别名：命中已有会话则原地更新，否则新建。
    /// 返回 `None` 表示校验失败、已记警告并跳过。
    fn import_selected(&mut self, t: &Target) -> Option<ProfileId> {
        let found = find_saved(&self.all, &t.name, true, &t.host, t.port, &t.user);
        let Some(id) = found else {
            let id = self.create(t)?;
            self.report.added += 1;
            return Some(id);
        };
        let idx = self.index_of(id)?;
        let mut p = self.all[idx].clone();
        if let ProfileKind::Ssh(s) = &mut p.kind {
            // 只更新主机 / 端口 / 用户名。名称、颜色、分组、编码、locale、跳板机都不动。
            s.target.host = t.host.clone();
            s.target.port = t.port;
            s.target.user = t.user.clone();
            // 只有文件里明确写了私钥才改认证方式；默认探测到的私钥不算，否则会把密码认证的会话
            // 改成私钥并丢掉保存的密码。
            if let Some(k) = &t.explicit_key {
                s.target.auth = AuthMethod::PrivateKey;
                s.target.private_key_path = Some(k.clone());
            }
        }
        if !self.check(&p) {
            return None;
        }
        self.all[idx] = p;
        self.touch(id);
        // 命中本次新建的会话（两个别名指向同一目标）不算更新。
        if !self.created.contains(&id) && self.counted_updates.insert(id) {
            self.report.updated += 1;
        }
        Some(id)
    }

    /// 为跳板机取 Profile：已有的原样使用（不更新），没有则新建并计入 `jumps_added`。
    fn jump_profile(&mut self, t: &Target, by_name: bool) -> Option<ProfileId> {
        if let Some(id) = find_saved(&self.all, &t.name, by_name, &t.host, t.port, &t.user) {
            return Some(id);
        }
        let id = self.create(t)?;
        self.report.jumps_added += 1;
        Some(id)
    }

    /// 解析单跳 ProxyJump 并找到 / 创建对应的跳板 Profile。
    fn resolve_hop(&mut self, owner: &str, hop: &str) -> Option<ProfileId> {
        // 不含 `@`、`:`、`[` 且等于文件里某个别名：引用别名（沿用别名自己的求值结果）。
        let loaded = self.loaded;
        let is_plain = !hop.contains(['@', ':', '[', ']']);
        if is_plain
            && let Some(r) = loaded
                .scan
                .hosts
                .iter()
                .find(|h| h.alias.eq_ignore_ascii_case(hop))
        {
            let key = r.alias.to_ascii_lowercase();
            if let Some(id) = self.alias_ids.get(&key) {
                return Some(*id);
            }
            let t = Target::from_resolved(r);
            let id = self.jump_profile(&t, true)?;
            self.alias_ids.insert(key, id);
            return Some(id);
        }
        let Some(spec) = parse_jump_spec(hop) else {
            self.warn(Message::ImportSkippedInvalid {
                name: owner.to_owned(),
                detail: format!("unsupported ProxyJump value {hop:?}"),
            });
            return None;
        };
        // 没写用户 / 端口时，ssh 会对跳板主机再套一遍配置（如 `Host *` 的 User），这里同样求值。
        let r = kanso_sshconfig::resolve(&spec.host, &loaded.path, &StdFs, &loaded.env);
        let mut t = Target::from_resolved(&r);
        t.name = spec.host.clone();
        t.host = spec.host;
        if let Some(u) = spec.user {
            t.user = u;
        }
        if let Some(p) = spec.port {
            t.port = p;
        }
        self.jump_profile(&t, false)
    }

    /// 给已导入的会话设置跳板机（§7.3）。
    fn apply_jump(&mut self, id: ProfileId, r: &Resolved) {
        let Some(spec) = r.proxy_jump.as_deref() else {
            return;
        };
        let Some(idx) = self.index_of(id) else {
            return;
        };
        // 已经有跳板机的会话不被导入覆盖（含手动设置的）。
        if ssh_target_of(&self.all[idx]).is_some_and(|t| t.jump.is_some()) {
            return;
        }
        let hops = jump_hops(spec);
        if hops.len() > 1 {
            // 每个会话只能指定一个跳板机，且不能为某一台主机的跳板链去改别的会话共用的跳板机。
            self.report.multi_hop_ignored += 1;
            self.warn(Message::SshConfigMultiHopIgnored {
                alias: r.alias.clone(),
            });
            return;
        }
        let Some(hop) = hops.first() else {
            return;
        };
        let Some(jump) = self.resolve_hop(&r.alias, hop) else {
            return;
        };
        let mut p = self.all[idx].clone();
        if let ProfileKind::Ssh(s) = &mut p.kind {
            s.target.jump = Some(jump);
        }
        // 成环、链过长等：只对这个会话告警，不设跳板，不让整个导入失败。
        if !self.check(&p) {
            return;
        }
        self.all[idx] = p;
        self.touch(id);
    }
}

/// 规划一次导入：只读 `stored`（当前已保存的全部 Profile），返回需要写入的 Profile（新增与更新，
/// 按顺序）和报告。不写库、不碰凭据（密码不在配置文件里）。
///
/// 分两个阶段：先落实所有勾选别名的会话，再设置跳板机。这样跳板机引用的别名如果本身也被勾选，
/// 会按"勾选的会话"计数，而不会被先当成额外新增的跳板机。
///
/// 已知取舍：自动带入的跳板机自身的 ProxyJump 不再递归导入。
pub(crate) fn plan_import(
    loaded: &Loaded,
    aliases: &[String],
    group: Option<GroupId>,
    stored: Vec<Profile>,
) -> (Vec<Profile>, SshConfigImportReport) {
    let mut pl = Planner {
        loaded,
        group,
        all: stored,
        touched: Vec::new(),
        created: HashSet::new(),
        counted_updates: HashSet::new(),
        alias_ids: HashMap::new(),
        report: SshConfigImportReport {
            added: 0,
            updated: 0,
            jumps_added: 0,
            multi_hop_ignored: 0,
            warnings: Vec::new(),
        },
    };
    for w in &loaded.scan.warnings {
        pl.warn(warning_message(w, ""));
    }

    let mut done: Vec<(ProfileId, &Resolved)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for alias in aliases {
        let key = alias.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            continue;
        }
        let Some(r) = loaded
            .scan
            .hosts
            .iter()
            .find(|h| h.alias.eq_ignore_ascii_case(alias))
        else {
            pl.warn(Message::ImportSkippedInvalid {
                name: alias.clone(),
                detail: "alias not found in ssh config".to_owned(),
            });
            continue;
        };
        for w in &r.warnings {
            pl.warn(warning_message(w, &r.alias));
        }
        if let Some(id) = pl.import_selected(&Target::from_resolved(r)) {
            pl.alias_ids.insert(r.alias.to_ascii_lowercase(), id);
            done.push((id, r));
        }
    }
    for (id, r) in done {
        pl.apply_jump(id, r);
    }

    let writes = pl
        .touched
        .iter()
        .filter_map(|id| pl.all.iter().find(|p| p.id == *id).cloned())
        .collect();
    (writes, pl.report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_spec_forms() {
        let s = parse_jump_spec("bob@h.example:2200").unwrap();
        assert_eq!(
            (s.user.as_deref(), s.host.as_str(), s.port),
            (Some("bob"), "h.example", Some(2200))
        );
        let s = parse_jump_spec("[::1]:2222").unwrap();
        assert_eq!((s.user, s.host.as_str(), s.port), (None, "::1", Some(2222)));
        let s = parse_jump_spec("[fe80::1]").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("fe80::1", None));
        let s = parse_jump_spec("fe80::1").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("fe80::1", None));
        assert!(parse_jump_spec("h:0").is_none());
        assert!(parse_jump_spec("h:abc").is_none());
        assert!(parse_jump_spec("@h").is_none());
        assert!(parse_jump_spec("[::1").is_none());
        assert!(parse_jump_spec("").is_none());
    }

    #[test]
    fn hops_split() {
        assert_eq!(jump_hops("a, b ,,c"), vec!["a", "b", "c"]);
        assert_eq!(jump_hops("a"), vec!["a"]);
    }
}
