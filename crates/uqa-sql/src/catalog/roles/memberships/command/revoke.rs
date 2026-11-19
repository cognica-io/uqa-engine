//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Revocation resolves every recipient against the original target graph before publishing changes.

use super::{
    super::{clear_membership_admin, revoke_membership},
    MembershipTarget, MembershipUpdate, RoleBinding, RoleMembershipKey, RoleMembershipOptions,
    SQLError,
};
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};

pub struct MembershipRevocation<'a> {
    target: &'a MembershipTarget,
    before: RoleMemberships,
    after: RoleMemberships,
}

impl<'a> MembershipRevocation<'a> {
    pub fn new(target: &'a MembershipTarget, memberships: &RoleMemberships) -> Self {
        let before = memberships.clone();
        Self {
            target,
            after: before.clone(),
            before,
        }
    }

    /// Returns a warning only when the original graph lacks the recipient, including duplicate or already cascaded requests.
    pub fn member(
        &mut self,
        roles: &RoleDefinitions,
        member: &RoleBinding,
    ) -> Result<Option<crate::SQLNotice>, SQLError> {
        let key = RoleMembershipKey {
            role: self.target.role.identity(),
            member: member.identity(),
            grantor: self.target.grantor.identity(),
        };
        if !self.before.contains_key(&key) {
            return self
                .target
                .membership_notice(roles, member, "has not been granted")
                .map(|message| Some(crate::SQLNotice::warning(message)));
        }
        let options = self.target.options;
        if options == RoleMembershipOptions::default() {
            revoke_membership(&mut self.after, &key, self.target.cascade, true)?;
        } else {
            if options.admin == Some(false)
                && self.after.get(&key).is_some_and(|row| row.admin_option)
            {
                clear_membership_admin(&mut self.after, &key, self.target.cascade)?;
            }
            self.after.edit(&key, |row| {
                if options.inherit == Some(false) {
                    row.inherit_option = false;
                }
                if options.set == Some(false) {
                    row.set_option = false;
                }
                Ok(())
            })?;
        }
        Ok(None)
    }

    pub fn into_updates(self) -> Vec<MembershipUpdate> {
        self.before
            .iter()
            .filter(|(key, _)| key.role == self.target.role.identity())
            .filter_map(|(key, before)| {
                let after = self.after.get(key).cloned();
                (after.as_ref() != Some(before)).then(|| MembershipUpdate {
                    before: before.clone(),
                    after,
                })
            })
            .collect()
    }
}
