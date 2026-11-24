//! 测试替身与构造器（只在测试里编译）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kanso_core::{CoreError, DeleteImpact};
use kanso_secret::SecretKind;
use kanso_types::mcp::McpAccess;
use kanso_types::profile::{
    AuthMethod, EncodingLabel, Group, LocalProfile, Profile, ProfileKind, SshProfile, SshTarget,
};
use kanso_types::secret::SecretString;
use kanso_types::{GroupId, ProfileId};

use super::backend::{CoreResult, ProfileStore};

pub(crate) fn ssh_target(host: &str, user: &str) -> SshTarget {
    SshTarget {
        host: host.into(),
        port: 22,
        user: user.into(),
        auth: AuthMethod::Password,
        private_key_path: None,
        jump: None,
        proxy_command: None,
        keepalive: kanso_types::profile::KeepaliveConfig::default(),
    }
}

pub(crate) fn ssh_profile(name: &str, host: &str, user: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group: None,
        kind: ProfileKind::Ssh(SshProfile {
            target: ssh_target(host, user),
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

pub(crate) fn sftp_profile(name: &str, host: &str, user: &str) -> Profile {
    Profile {
        kind: ProfileKind::Sftp(ssh_target(host, user)),
        ..ssh_profile(name, host, user)
    }
}

pub(crate) fn local_profile(name: &str, shell: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        color: None,
        group: None,
        kind: ProfileKind::Local(LocalProfile {
            shell_command: shell.into(),
            start_dir: None,
            encoding: EncodingLabel::default(),
            locale: None,
        }),
    }
}

pub(crate) fn group(name: &str) -> Group {
    Group {
        id: GroupId::new(),
        name: name.into(),
    }
}

#[derive(Default)]
pub(crate) struct StoreInner {
    pub profiles: Vec<Profile>,
    pub groups: Vec<Group>,
    /// 每次 `save_profile` 的调用记录。
    pub saved: Vec<Profile>,
    pub deleted_profiles: Vec<ProfileId>,
    pub deleted_groups: Vec<GroupId>,
    pub credentials: Vec<(ProfileId, SecretKind, String)>,
    pub mcp_accesses: HashMap<ProfileId, McpAccess>,
    pub fail_save: Option<CoreError>,
    pub fail_credential: bool,
    pub fail_mcp_access: bool,
    pub impact: Option<DeleteImpact>,
}

/// 内存里的会话库，记录调用。
#[derive(Clone, Default)]
pub(crate) struct FakeStore(pub Rc<RefCell<StoreInner>>);

impl FakeStore {
    pub fn with(profiles: Vec<Profile>, groups: Vec<Group>) -> Self {
        let s = FakeStore::default();
        {
            let mut i = s.0.borrow_mut();
            i.profiles = profiles;
            i.groups = groups;
        }
        s
    }
}

impl ProfileStore for FakeStore {
    fn profiles(&self) -> Vec<Profile> {
        self.0.borrow().profiles.clone()
    }

    fn groups(&self) -> Vec<Group> {
        self.0.borrow().groups.clone()
    }

    fn save_profile(&self, profile: Profile) -> CoreResult<()> {
        let mut i = self.0.borrow_mut();
        if let Some(e) = i.fail_save.clone() {
            return Err(e);
        }
        i.saved.push(profile.clone());
        match i.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile,
            None => i.profiles.push(profile),
        }
        Ok(())
    }

    fn mcp_access(&self, profile: ProfileId) -> McpAccess {
        self.0
            .borrow()
            .mcp_accesses
            .get(&profile)
            .copied()
            .unwrap_or_default()
    }

    fn set_mcp_access(&self, profile: ProfileId, level: McpAccess) -> CoreResult<()> {
        let mut i = self.0.borrow_mut();
        if !i.profiles.iter().any(|p| p.id == profile) {
            return Err(CoreError::NoSuchProfile(profile));
        }
        if i.fail_mcp_access {
            return Err(CoreError::Internal {
                detail: "MCP access write".into(),
            });
        }
        if level == McpAccess::Off {
            i.mcp_accesses.remove(&profile);
        } else {
            i.mcp_accesses.insert(profile, level);
        }
        Ok(())
    }

    async fn delete_profile(&self, id: ProfileId) -> CoreResult<()> {
        let mut i = self.0.borrow_mut();
        if !i.profiles.iter().any(|p| p.id == id) {
            return Err(CoreError::NoSuchProfile(id));
        }
        i.profiles.retain(|p| p.id != id);
        i.deleted_profiles.push(id);
        Ok(())
    }

    fn save_group(&self, group: Group) -> CoreResult<()> {
        let mut i = self.0.borrow_mut();
        match i.groups.iter_mut().find(|g| g.id == group.id) {
            Some(slot) => *slot = group,
            None => i.groups.push(group),
        }
        Ok(())
    }

    fn group_delete_impact(&self, id: GroupId) -> CoreResult<DeleteImpact> {
        let i = self.0.borrow();
        if !i.groups.iter().any(|g| g.id == id) {
            return Err(CoreError::NoSuchGroup(id));
        }
        Ok(i.impact.unwrap_or(DeleteImpact {
            profiles: i.profiles.iter().filter(|p| p.group == Some(id)).count() as u32,
            snippets: 0,
        }))
    }

    async fn delete_group(&self, id: GroupId) -> CoreResult<()> {
        let mut i = self.0.borrow_mut();
        if !i.groups.iter().any(|g| g.id == id) {
            return Err(CoreError::NoSuchGroup(id));
        }
        i.groups.retain(|g| g.id != id);
        i.profiles.retain(|p| p.group != Some(id));
        i.deleted_groups.push(id);
        Ok(())
    }

    async fn set_credential(
        &self,
        profile: ProfileId,
        kind: SecretKind,
        secret: SecretString,
    ) -> CoreResult<()> {
        let mut i = self.0.borrow_mut();
        if i.fail_credential {
            return Err(CoreError::Internal {
                detail: "keychain".into(),
            });
        }
        i.credentials
            .push((profile, kind, secret.expose().to_owned()));
        Ok(())
    }
}
