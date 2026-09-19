//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured membership authority and pure per-recipient catalog changes.

use super::{
    grants, RoleAttribute, RoleBinding, RoleDefinition, RoleMembership, RoleMembershipKey,
    RoleSubject, SQLError,
};
use crate::{
    ast::{AlterRoleStmt, GrantRoleStmt, RoleMembershipAction, RoleMembershipOptions},
    catalog::roles::{resolve_role_specification, RoleReference, RoleReferenceNames},
};
use std::collections::BTreeMap;

mod revoke;
pub use revoke::MembershipRevocation;

pub fn creator_membership(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    created: &RoleDefinition,
) -> Result<MembershipInsertion, SQLError> {
    let creator = current
        .role_definition(roles)
        .ok_or_else(|| super::insufficient_privilege("permission denied to create role"))?;
    let bootstrap = roles
        .values()
        .find(|role| role.oid == 10)
        .ok_or_else(|| SQLError::Internal("role catalog has no bootstrap superuser".into()))?;
    Ok(MembershipInsertion {
        role: RoleBinding::from_definition(created)?,
        member: RoleBinding::from_definition(creator)?,
        grantor: RoleBinding::from_definition(bootstrap)?,
        admin_option: true,
        inherit_option: false,
        set_option: false,
    })
}

#[derive(Clone)]
pub struct MembershipRecipients {
    pub members: Vec<RoleBinding>,
    pub grantor: Option<RoleBinding>,
}

impl MembershipRecipients {
    /// Resolve the explicit grantor before all recipients; target lookup and authority follow separately in statement order.
    pub fn bind(
        names: &dyn RoleReferenceNames,
        roles: &BTreeMap<String, RoleDefinition>,
        statement: &GrantRoleStmt,
    ) -> Result<Self, SQLError> {
        let grantor = statement
            .grantor
            .as_ref()
            .map(|role| resolve_role_specification(names, role).bind(roles))
            .transpose()?;
        let members = statement
            .grantee_roles
            .iter()
            .map(|role| resolve_role_specification(names, role).bind(roles))
            .collect::<Result<_, _>>()?;
        Ok(Self { members, grantor })
    }
}

#[derive(Clone)]
pub struct MembershipTarget {
    pub role: RoleBinding,
    pub grantor: RoleBinding,
    pub members: Vec<RoleBinding>,
    pub is_grant: bool,
    pub options: RoleMembershipOptions,
    pub cascade: bool,
}

impl MembershipTarget {
    /// ALTER GROUP checks the target and its administration before resolving recipients.
    pub fn authorize_group(
        names: &dyn RoleReferenceNames,
        roles: &BTreeMap<String, RoleDefinition>,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
        current: &(impl RoleSubject + ?Sized),
        statement: &AlterRoleStmt,
    ) -> Result<Self, SQLError> {
        let target = resolve_role_specification(names, &statement.name);
        let role = target.bind(roles)?;
        grants::require_group_authority(roles, memberships, current, &role)?;
        let command = GrantRoleStmt {
            granted_roles: Vec::new(),
            grantee_roles: statement.members.clone(),
            is_grant: statement.membership_action == Some(RoleMembershipAction::Add),
            options: RoleMembershipOptions::default(),
            grantor: None,
            cascade: false,
        };
        let recipients = MembershipRecipients::bind(names, roles, &command)?;
        Self::authorize(roles, memberships, current, &target, &recipients, &command)
    }

    /// Capture authority before execution waits on the target; subsequent graph checks do not repeat this authorization.
    pub fn authorize(
        roles: &BTreeMap<String, RoleDefinition>,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
        current: &(impl RoleSubject + ?Sized),
        target: &RoleReference,
        recipients: &MembershipRecipients,
        statement: &GrantRoleStmt,
    ) -> Result<Self, SQLError> {
        let role = target.bind(roles)?;
        let grantor = grants::select_grantor(
            roles,
            memberships,
            current,
            &role,
            recipients.grantor.as_ref(),
            statement.is_grant,
        )?;
        Ok(Self {
            role,
            grantor,
            members: recipients.members.clone(),
            is_grant: statement.is_grant,
            options: statement.options,
            cascade: statement.cascade,
        })
    }

    pub fn validate_graph(
        &self,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        if self.is_grant {
            grants::validate_grant(
                memberships,
                &self.role,
                &self.grantor,
                &self.members,
                self.options,
            )?;
        }
        Ok(())
    }

    pub fn change_for_member(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
        memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
        member: &RoleBinding,
    ) -> Result<MembershipChange, SQLError> {
        let key = RoleMembershipKey {
            role: self.role.identity(),
            member: member.identity(),
            grantor: self.grantor.identity(),
        };
        if !self.is_grant {
            return Err(SQLError::Internal(
                "role revocation requires a complete target plan".into(),
            ));
        }
        if let Some(before) = memberships.get(&key) {
            let mut after = before.clone();
            if let Some(option) = self.options.admin {
                after.admin_option = option;
            }
            if let Some(option) = self.options.inherit {
                after.inherit_option = option;
            }
            if let Some(option) = self.options.set {
                after.set_option = option;
            }
            return Ok(if after == *before {
                MembershipChange::Notice {
                    level: "NOTICE",
                    message: self.membership_notice(roles, member, "has already been granted")?,
                }
            } else {
                MembershipChange::Update(vec![MembershipUpdate {
                    before: before.clone(),
                    after: Some(after),
                }])
            });
        }
        let inherit = match self.options.inherit {
            Some(value) => value,
            None => member
                .role_definition(roles)
                .ok_or_else(|| {
                    SQLError::Internal(format!("cache lookup failed for role {}", member.oid))
                })?
                .has(RoleAttribute::Inherit),
        };
        Ok(MembershipChange::Insert(MembershipInsertion {
            role: self.role.clone(),
            member: member.clone(),
            grantor: self.grantor.clone(),
            admin_option: self.options.admin.unwrap_or(false),
            inherit_option: inherit,
            set_option: self.options.set.unwrap_or(true),
        }))
    }

    fn membership_notice(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
        member: &RoleBinding,
        action: &str,
    ) -> Result<String, SQLError> {
        let grantor = self.grantor.require_name(roles)?;
        Ok(format!(
            "role \"{}\" {action} membership in role \"{}\" by role \"{grantor}\"",
            member.name, self.role.name
        ))
    }
}

pub enum MembershipChange {
    Insert(MembershipInsertion),
    Update(Vec<MembershipUpdate>),
    Notice {
        level: &'static str,
        message: String,
    },
}

pub struct MembershipUpdate {
    pub before: RoleMembership,
    pub after: Option<RoleMembership>,
}

impl MembershipUpdate {
    pub fn apply(
        &self,
        memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<(), SQLError> {
        let key = self.before.key();
        match memberships.get(&key) {
            Some(current) if current == &self.before => {}
            Some(_) => return Err(SQLError::Internal("tuple concurrently updated".into())),
            None => return Err(SQLError::Internal("tuple concurrently deleted".into())),
        }
        if let Some(after) = &self.after {
            memberships.insert(key, after.clone());
        } else {
            memberships.remove(&key);
        }
        Ok(())
    }
}

/// SQL specifies the tuple; execution supplies its reserved public OID before publication.
pub struct MembershipInsertion {
    pub role: RoleBinding,
    pub member: RoleBinding,
    pub grantor: RoleBinding,
    pub admin_option: bool,
    pub inherit_option: bool,
    pub set_option: bool,
}

impl MembershipInsertion {
    pub fn with_oid(self, oid: i64) -> Result<RoleMembership, SQLError> {
        if oid <= 0 || oid > i64::from(u32::MAX) {
            return Err(SQLError::Internal("invalid role membership OID".into()));
        }
        Ok(RoleMembership {
            oid,
            role: self.role,
            member: self.member,
            grantor: self.grantor,
            admin_option: self.admin_option,
            inherit_option: self.inherit_option,
            set_option: self.set_option,
        })
    }
}

#[cfg(test)]
mod tests;
