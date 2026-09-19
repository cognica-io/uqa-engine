//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog fixtures execute the SQL mutation descriptions with deterministic test identities.

use super::{command::*, *};
use crate::{
    ast::{GrantRoleStmt, RoleMembershipOptions},
    catalog::roles::{RoleReference, RoleReferenceNames},
};

pub(crate) struct Names<'a>(pub &'a str);
impl RoleReferenceNames for Names<'_> {
    fn outer_role(&self) -> crate::catalog::roles::RoleReference {
        self.current_role()
    }
    fn current_role(&self) -> RoleReference {
        self.0.into()
    }
    fn session_role(&self) -> RoleReference {
        "uqa".into()
    }
}

fn oid(memberships: &BTreeMap<RoleMembershipKey, RoleMembership>) -> i64 {
    memberships
        .values()
        .map(|value| value.oid)
        .max()
        .unwrap_or(30_000)
        + 1
}

pub(crate) fn apply_change(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    change: MembershipChange,
) -> Result<(), SQLError> {
    match change {
        MembershipChange::Insert(insert) => {
            let row = insert.with_oid(oid(memberships))?;
            memberships.insert(row.key(), row);
        }
        MembershipChange::Update(updates) => {
            for update in updates {
                update.apply(memberships)?;
            }
        }
        MembershipChange::Notice { .. } => {}
    }
    Ok(())
}

pub(crate) fn apply_grant_role_statement(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &str,
    statement: &GrantRoleStmt,
) -> Result<(), SQLError> {
    let recipients = MembershipRecipients::bind(&Names(current), roles, statement)?;
    for target in &statement.granted_roles {
        let bound = MembershipTarget::authorize(
            roles,
            memberships,
            current,
            &target.clone().into(),
            &recipients,
            statement,
        )?;
        bound.validate_graph(memberships)?;
        if bound.is_grant {
            for member in &bound.members {
                let change = bound.change_for_member(roles, memberships, member)?;
                apply_change(memberships, change)?;
            }
        } else {
            let mut plan = MembershipRevocation::new(&bound, memberships);
            for member in &bound.members {
                plan.member(roles, member)?;
            }
            for update in plan.into_updates() {
                update.apply(memberships)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn insert_membership(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &str,
    member: &str,
    grantor: &str,
    options: RoleMembershipOptions,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    let target = MembershipTarget {
        role: RoleBinding::from_definition(&roles[role])?,
        grantor: RoleBinding::from_definition(&roles[grantor])?,
        members: vec![RoleBinding::from_definition(&roles[member])?],
        is_grant: true,
        options,
        cascade: false,
    };
    let change = target.change_for_member(roles, memberships, &target.members[0])?;
    apply_change(memberships, change)
}
