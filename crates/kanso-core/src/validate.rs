//! Profile 保存前的校验，纯函数，不依赖存储与运行时。
//!
//! mock 与真实实现共用同一份逻辑（store-import.md §3、ssh.md §5），
//! 所以放在独立模块里，不挂在任何一种 backend 上。

use std::collections::HashSet;

use kanso_types::profile::{Profile, ProfileKind, SshTarget};
use kanso_types::{Message, ProfileId};

/// 跳板链允许的最大跳数（ssh.md §5）。
pub const MAX_JUMP_HOPS: usize = 8;

/// 取出 SSH / SFTP Profile 的连接目标；本地 Profile 返回 `None`。
fn ssh_target(profile: &Profile) -> Option<&SshTarget> {
    match &profile.kind {
        ProfileKind::Ssh(p) => Some(&p.target),
        ProfileKind::Sftp(t) => Some(t),
        ProfileKind::Local(_) => None,
    }
}

/// 校验即将保存的 `profile`。`all` 是当前已保存的全部 Profile，
/// 其中可能含有 `profile` 自己的旧版本；链上遇到自己时一律以 `profile` 参数为准（见下）。
///
/// 检查顺序：主机为空、用户名为空、跳板机与 ProxyCommand 冲突、跳板链。
/// 跳板链出错必须报错，不能悄悄改成直连（store-import.md §3）。
///
/// 返回的 `Message` 可直接展示给用户。本地 Profile 没有需要校验的内容。
pub fn validate_profile(profile: &Profile, all: &[Profile]) -> Result<(), Message> {
    let Some(target) = ssh_target(profile) else {
        return Ok(());
    };
    if target.host.trim().is_empty() {
        return Err(Message::ProfileHostEmpty {});
    }
    if target.user.trim().is_empty() {
        return Err(Message::ProfileUserEmpty {});
    }
    let has_proxy_command = target
        .proxy_command
        .as_deref()
        .is_some_and(|c| !c.trim().is_empty());
    if target.jump.is_some() && has_proxy_command {
        return Err(Message::JumpProxyCommandConflict {});
    }

    // 沿跳板链向外走。`visited` 初始含自己：链回到自己即成环。
    // 不能只靠跳数上限兜住成环，否则成环会被误报成“过长”。
    let mut visited: HashSet<ProfileId> = HashSet::from([profile.id]);
    let mut hops = 0usize;
    let mut next = target.jump;
    while let Some(id) = next {
        if visited.contains(&id) {
            // 成环时报链上被重复引用的那个 Profile；是自己则用要保存的名字。
            let name = if id == profile.id {
                profile.name.clone()
            } else {
                all.iter()
                    .find(|p| p.id == id)
                    .map_or_else(|| id.to_string(), |p| p.name.clone())
            };
            return Err(Message::JumpCycle { profile: name });
        }
        let Some(jump) = all.iter().find(|p| p.id == id) else {
            return Err(Message::JumpMissing {
                profile: id.to_string(),
            });
        };
        let Some(jump_target) = (match &jump.kind {
            ProfileKind::Ssh(p) => Some(&p.target),
            _ => None,
        }) else {
            return Err(Message::JumpNotSsh {
                profile: jump.name.clone(),
            });
        };
        hops += 1;
        if hops > MAX_JUMP_HOPS {
            return Err(Message::JumpTooLong {
                max_hops: MAX_JUMP_HOPS as i64,
            });
        }
        visited.insert(id);
        next = jump_target.jump;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanso_types::profile::{AuthMethod, EncodingLabel, LocalProfile, SshProfile};

    fn target(host: &str, user: &str, jump: Option<ProfileId>) -> SshTarget {
        SshTarget {
            host: host.into(),
            port: 22,
            user: user.into(),
            auth: AuthMethod::Password,
            private_key_path: None,
            jump,
            proxy_command: None,
            keepalive: kanso_types::profile::KeepaliveConfig::default(),
        }
    }

    fn ssh(name: &str, t: SshTarget) -> Profile {
        Profile {
            id: ProfileId::new(),
            name: name.into(),
            color: None,
            group: None,
            kind: ProfileKind::Ssh(SshProfile {
                target: t,
                encoding: EncodingLabel::default(),
                locale: None,
            }),
        }
    }

    fn local(name: &str) -> Profile {
        Profile {
            id: ProfileId::new(),
            name: name.into(),
            color: None,
            group: None,
            kind: ProfileKind::Local(LocalProfile {
                shell_command: String::new(),
                start_dir: None,
                encoding: EncodingLabel::default(),
                locale: None,
            }),
        }
    }

    fn set_jump(p: &mut Profile, jump: Option<ProfileId>) {
        if let ProfileKind::Ssh(s) = &mut p.kind {
            s.target.jump = jump;
        }
    }

    #[test]
    fn empty_host_and_user() {
        let p = ssh("a", target("  ", "u", None));
        assert_eq!(validate_profile(&p, &[]), Err(Message::ProfileHostEmpty {}));
        let p = ssh("a", target("h", "", None));
        assert_eq!(validate_profile(&p, &[]), Err(Message::ProfileUserEmpty {}));
        let sftp = Profile {
            kind: ProfileKind::Sftp(target("", "u", None)),
            ..local("s")
        };
        assert_eq!(
            validate_profile(&sftp, &[]),
            Err(Message::ProfileHostEmpty {})
        );
    }

    #[test]
    fn local_profile_is_always_valid() {
        assert_eq!(validate_profile(&local("l"), &[]), Ok(()));
    }

    #[test]
    fn jump_and_proxy_command_conflict() {
        let j = ssh("j", target("jh", "u", None));
        let mut t = target("h", "u", Some(j.id));
        t.proxy_command = Some("nc %h %p".into());
        let p = ssh("a", t);
        assert_eq!(
            validate_profile(&p, &[j]),
            Err(Message::JumpProxyCommandConflict {})
        );
    }

    #[test]
    fn jump_missing() {
        let ghost = ProfileId::new();
        let p = ssh("a", target("h", "u", Some(ghost)));
        assert_eq!(
            validate_profile(&p, &[]),
            Err(Message::JumpMissing {
                profile: ghost.to_string()
            })
        );
    }

    #[test]
    fn jump_not_ssh() {
        let l = local("loc");
        let p = ssh("a", target("h", "u", Some(l.id)));
        assert_eq!(
            validate_profile(&p, &[l]),
            Err(Message::JumpNotSsh {
                profile: "loc".into()
            })
        );
    }

    #[test]
    fn jump_cycle_through_self() {
        // a -> b -> a：保存 a 时，库里的 b 已经指向 a。
        let mut a = ssh("a", target("ha", "u", None));
        let b = ssh("b", target("hb", "u", Some(a.id)));
        set_jump(&mut a, Some(b.id));
        assert_eq!(
            validate_profile(&a, std::slice::from_ref(&b)),
            Err(Message::JumpCycle {
                profile: "a".into()
            })
        );
        // 自己跳自己。
        let mut s = ssh("s", target("hs", "u", None));
        let sid = s.id;
        set_jump(&mut s, Some(sid));
        assert!(matches!(
            validate_profile(&s, &[]),
            Err(Message::JumpCycle { .. })
        ));
    }

    #[test]
    fn jump_cycle_not_involving_self() {
        // 库里已有 b <-> c 的环，保存的 a 指向 b。
        let mut b = ssh("b", target("hb", "u", None));
        let c = ssh("c", target("hc", "u", Some(b.id)));
        set_jump(&mut b, Some(c.id));
        let a = ssh("a", target("ha", "u", Some(b.id)));
        assert_eq!(
            validate_profile(&a, &[b, c]),
            Err(Message::JumpCycle {
                profile: "b".into()
            })
        );
    }

    fn chain(len: usize) -> (Profile, Vec<Profile>) {
        let mut all: Vec<Profile> = Vec::new();
        let mut prev: Option<ProfileId> = None;
        for i in 0..len {
            let p = ssh(&format!("j{i}"), target("h", "u", prev));
            prev = Some(p.id);
            all.push(p);
        }
        (ssh("a", target("h", "u", prev)), all)
    }

    #[test]
    fn jump_too_long_and_boundary() {
        let (p, all) = chain(MAX_JUMP_HOPS);
        assert_eq!(validate_profile(&p, &all), Ok(()));
        let (p, all) = chain(MAX_JUMP_HOPS + 1);
        assert_eq!(
            validate_profile(&p, &all),
            Err(Message::JumpTooLong { max_hops: 8 })
        );
    }

    #[test]
    fn valid_two_hop_chain() {
        let (p, all) = chain(2);
        assert_eq!(validate_profile(&p, &all), Ok(()));
    }
}
