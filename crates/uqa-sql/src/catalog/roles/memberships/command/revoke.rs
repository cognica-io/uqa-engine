//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Revocation resolves every recipient against the original target graph before publishing changes.

use super::{
    super::{clear_membership_admin, revoke_membership},
    BTreeMap, MembershipTarget, MembershipUpdate, RoleBinding, RoleDefinition, RoleMembership,
    RoleMembershipKey, RoleMembershipOptions, SQLError,
};

pub struct MembershipRevocation<'a> {
    target: &'a MembershipTarget,
    before: BTreeMap<RoleMembershipKey, RoleMembership>,
    after: BTreeMap<RoleMembershipKey, RoleMembership>,
}

impl<'a> MembershipRevocation<'a> {
    pub fn new(
        target: &'a MembershipTarget,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Self {
        let before = memberships
            .iter()
            .filter(|(key, _)| key.role == target.role.identity())
            .map(|(key, value)| (*key, value.clone()))
            .collect::<BTreeMap<_, _>>();
        Self {
            target,
            after: before.clone(),
            before,
        }
    }

    /// Returns a warning only when the original graph lacks the recipient, including duplicate or already cascaded requests.
    pub fn member(
        &mut self,
        roles: &BTreeMap<String, RoleDefinition>,
        member: &RoleBinding,
    ) -> Result<Option<String>, SQLError> {
        let key = RoleMembershipKey {
            role: self.target.role.identity(),
            member: member.identity(),
            grantor: self.target.grantor.identity(),
        };
        if !self.before.contains_key(&key) {
            return self
                .target
                .membership_notice(roles, member, "has not been granted")
                .map(Some);
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
            if let Some(row) = self.after.get_mut(&key) {
                if options.inherit == Some(false) {
                    row.inherit_option = false;
                }
                if options.set == Some(false) {
                    row.set_option = false;
                }
            }
        }
        Ok(None)
    }

    pub fn into_updates(self) -> Vec<MembershipUpdate> {
        self.before
            .into_iter()
            .filter_map(|(key, before)| {
                let after = self.after.get(&key).cloned();
                (after.as_ref() != Some(&before)).then_some(MembershipUpdate { before, after })
            })
            .collect()
    }
}
