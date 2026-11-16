//! 条目树求值：枚举别名、按 `ssh <别名>` 的方式取各选项的值。

use std::path::PathBuf;

use crate::fs::ConfigFs;
use crate::parse::{Item, Key, short_host};
use crate::text::{expand_percent, expand_tilde, wildcard_match};
use crate::{Env, Resolved, Warning};

/// 求值过程中各选项“第一次出现的值”。
#[derive(Default)]
struct Acc {
    host_name: Option<String>,
    port: Option<String>,
    user: Option<String>,
    identity: Option<String>,
    /// `IdentityFile` 在 ssh 里可累加（会按顺序依次尝试多个），这里只取第一个：
    /// 导入只需要一把明确指定的私钥。
    proxy_jump: Option<String>,
}

fn set_first(slot: &mut Option<String>, v: &str) {
    if slot.is_none() {
        *slot = Some(v.to_string());
    }
}

/// `Host` 模式列表是否选中 `host`：任一否定模式命中就否决，否则要求某个肯定模式命中。
fn host_matches(patterns: &[String], host: &str) -> bool {
    let mut positive = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if wildcard_match(neg, host, true) {
                return false;
            }
        } else if wildcard_match(p, host, true) {
            positive = true;
        }
    }
    positive
}

/// 顺序扫描条目。
///
/// `inherited` 是进入该文件时的“当前块是否选中”状态。被引入文件开头（第一个 Host 之前）的行
/// 继承引入处所在块的状态；文件内部的 Host/Match 只改变局部状态，**回到外层文件后外层状态原样恢复**
/// （`ssh -G` 实验：Host 块里 Include 一个以别的 `Host` 开头的文件，之后的行仍属于原来的块）。
/// 同样来自实验：处于未选中状态的块里的 Include 根本不会被读取，所以这里也不递归。
fn walk(items: &[Item], host: &str, acc: &mut Acc, inherited: bool) {
    let mut active = inherited;
    for item in items {
        match item {
            Item::Host(pats) => active = host_matches(pats, host),
            Item::Match => active = false,
            Item::Opt(key, v) if active => {
                let slot = match key {
                    Key::HostName => &mut acc.host_name,
                    Key::Port => &mut acc.port,
                    Key::User => &mut acc.user,
                    Key::IdentityFile => &mut acc.identity,
                    Key::ProxyJump => &mut acc.proxy_jump,
                };
                set_first(slot, v);
            }
            Item::Opt(..) => {}
            Item::Include(files) if active => {
                for f in files {
                    walk(f, host, acc, active);
                }
            }
            Item::Include(_) => {}
        }
    }
}

/// 收集所有可导入别名（按首次出现顺序，大小写不敏感去重，保留首次写法）。
/// 不区分块是否会被选中：只要 Host 行里出现了“只匹配一台主机”的模式就算。
pub(crate) fn collect_aliases(items: &[Item], out: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Host(pats) => {
                for p in pats {
                    if p.is_empty() || p.starts_with('!') || p.contains(['*', '?']) {
                        continue;
                    }
                    if !out.iter().any(|a| a.eq_ignore_ascii_case(p)) {
                        out.push(p.clone());
                    }
                }
            }
            Item::Include(files) => files.iter().for_each(|f| collect_aliases(f, out)),
            _ => {}
        }
    }
}

/// 展开 token 并把未识别的转成警告。
fn expand(
    value: &str,
    option: &'static str,
    lookup: &dyn Fn(char) -> Option<String>,
    warnings: &mut Vec<Warning>,
) -> String {
    let (out, bad) = expand_percent(value, lookup);
    for token in bad {
        warnings.push(Warning::UnknownToken {
            option,
            value: value.to_string(),
            token,
        });
    }
    out
}

/// 对一个别名求值。
pub(crate) fn resolve_alias(items: &[Item], alias: &str, fs: &dyn ConfigFs, env: &Env) -> Resolved {
    let mut acc = Acc::default();
    walk(items, alias, &mut acc, true);
    let mut warnings = Vec::new();

    // HostName 只支持 %h（= 别名）与 %%（`ssh -G` 实验：其余 token 一律报 unknown key）。
    let host_name = match &acc.host_name {
        Some(raw) => expand(
            raw,
            "HostName",
            &|c| (c == 'h').then(|| alias.to_string()),
            &mut warnings,
        ),
        None => alias.to_string(),
    };
    // ssh 会把最终主机名转成小写（`ssh -G` 实验确认），DNS 本身也不区分大小写；
    // 保持一致，导入后按主机/端口/用户去重时才不会因为大小写对不上。
    let host_name = host_name.to_lowercase();

    let port = match &acc.port {
        None => 22,
        Some(raw) => match raw.parse::<u16>() {
            Ok(p) if p != 0 => p,
            _ => {
                warnings.push(Warning::InvalidPort { value: raw.clone() });
                22
            }
        },
    };

    let proxy_jump = acc
        .proxy_jump
        .clone()
        .filter(|v| !v.eq_ignore_ascii_case("none"));

    // 与主机、端口、本机相关的 token 共用的部分。
    let common = |c: char| -> Option<String> {
        match c {
            'h' => Some(host_name.clone()),
            'n' => Some(alias.to_string()),
            'p' => Some(port.to_string()),
            'd' => Some(env.home.to_string_lossy().into_owned()),
            'u' => Some(env.local_user.clone()),
            'l' => Some(env.local_host.clone()),
            'L' => Some(short_host(&env.local_host)),
            'i' => Some(env.local_uid.to_string()),
            _ => None,
        }
    };

    // User 不支持 %r（`ssh -G` 实验：报 unknown key），也不支持 %C。
    let user = match &acc.user {
        Some(raw) => expand(raw, "User", &common, &mut warnings),
        None => env.local_user.clone(),
    };

    // IdentityFile 另外支持 %r（远端用户名）与 %j（ProxyJump 原文）。%C 需要哈希，暂不支持。
    // 注意 `ssh -G` 不展开 IdentityFile，所以 token 集合以 ssh_config(5) 手册为准，无法黑盒验证。
    let identity_file = acc.identity.as_ref().map(|raw| {
        let path = expand_tilde(raw, &env.home);
        let s = path.to_string_lossy().into_owned();
        let out = expand(
            &s,
            "IdentityFile",
            &|c| match c {
                'r' => Some(user.clone()),
                'k' => Some(alias.to_string()),
                'j' => Some(proxy_jump.clone().unwrap_or_default()),
                _ => common(c),
            },
            &mut warnings,
        );
        PathBuf::from(out)
    });

    let default_identity = if identity_file.is_none() {
        ["id_ed25519", "id_ecdsa", "id_rsa"]
            .iter()
            .map(|n| env.home.join(".ssh").join(n))
            .find(|p| fs.exists(p))
    } else {
        None
    };

    Resolved {
        alias: alias.to_string(),
        host_name,
        port,
        user,
        identity_file,
        default_identity,
        proxy_jump,
        warnings,
    }
}
