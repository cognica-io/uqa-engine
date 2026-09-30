//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Check, grant and revoke the ACL of an object with one grantable privilege, as `PostgreSQL`'s `aclchk.c` does for routine `EXECUTE` and type `USAGE`. An absent ACL is the default ACL: the privilege for PUBLIC and for the owner. Owners always hold grant options.

use crate::ast::ObjectAclEntry;
use crate::catalog::roles::{
    identity::RoleSubject, role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey,
    RoleReference,
};
use crate::SQLError;
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{catalog_acl::AclGrantee, catalog_role::RoleIdentity};

/// Whether a role holding `has_role` memberships may use the object, or grant it when `grant_option` is set.
pub fn privilege_allowed(
    owner: &RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
    grant_option: bool,
    superuser: bool,
    has_role: impl Fn(&RoleIdentity) -> bool,
) -> bool {
    if superuser || (grant_option && has_role(owner)) {
        return true;
    }
    acl.map_or(!grant_option, |acl| {
        acl.iter().any(|entry| {
            (!grant_option || entry.grant_option)
                && ((entry.role.is_none() && !grant_option)
                    || entry.role.as_ref().is_some_and(&has_role))
        })
    })
}

/// Roles whose grant option is reachable from the owner through grant-option entries.
pub fn grant_option_roles(
    owner: RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
) -> BTreeSet<RoleIdentity> {
    let mut reachable = BTreeSet::from([owner]);
    let Some(acl) = acl else {
        return reachable;
    };
    loop {
        let mut changed = false;
        for entry in acl {
            if let Some(role) = entry.role {
                if entry.grant_option && reachable.contains(&entry.grantor) {
                    changed |= reachable.insert(role);
                }
            }
        }
        if !changed {
            return reachable;
        }
    }
}

/// `select_best_grantor`: the owner when the current user inherits it, then the current user's own grant option, then an inherited role's grant option. `None` means the command grants or revokes nothing and warns.
pub fn select_grantor(
    owner: RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Option<RoleIdentity> {
    if role_inherits(roles, memberships, current_user, &owner) {
        return Some(owner);
    }
    let grant_options = grant_option_roles(owner, acl);
    if let Some(identity) = current_user
        .role_definition(roles)
        .map(RoleDefinition::identity)
        .filter(|identity| grant_options.contains(identity))
    {
        return Some(identity);
    }
    acl.and_then(|acl| {
        acl.iter()
            .filter_map(|entry| entry.role)
            .filter(|role| grant_options.contains(role))
            .find(|role| role_inherits(roles, memberships, current_user, role))
    })
}

fn materialize(
    owner: RoleIdentity,
    acl: &mut Option<Vec<ObjectAclEntry>>,
) -> &mut Vec<ObjectAclEntry> {
    acl.get_or_insert_with(|| {
        vec![
            ObjectAclEntry {
                role: None,
                grantor: owner,
                grant_option: false,
            },
            ObjectAclEntry {
                role: Some(owner),
                grantor: owner,
                grant_option: false,
            },
        ]
    })
}

/// The ACL a GRANT or REVOKE stores: `ExecGrant_common` substitutes the default ACL for a missing one and always writes its result, so the default becomes explicit even when the command changes nothing.
#[must_use]
pub fn explicit_acl(
    owner: RoleIdentity,
    mut acl: Option<Vec<ObjectAclEntry>>,
) -> Option<Vec<ObjectAclEntry>> {
    materialize(owner, &mut acl);
    acl
}

/// Add the privilege, merging the grant option into an existing entry from the same grantor.
pub fn grant(
    owner: RoleIdentity,
    acl: &mut Option<Vec<ObjectAclEntry>>,
    grantee: Option<RoleIdentity>,
    grantor: RoleIdentity,
    grant_option: bool,
) {
    let entries = materialize(owner, acl);
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.role == grantee && entry.grantor == grantor)
    {
        entry.grant_option |= grant_option;
    } else {
        entries.push(ObjectAclEntry {
            role: grantee,
            grantor,
            grant_option,
        });
    }
}

/// Remove the privilege or only its grant option; revoking what the grantor did not grant changes nothing, and `ExecGrant_*` warns only when the grantor holds no grant option. Privileges granted through a lost grant option require CASCADE.
pub fn revoke(
    owner: RoleIdentity,
    acl: &mut Option<Vec<ObjectAclEntry>>,
    grantee: Option<RoleIdentity>,
    grantor: RoleIdentity,
    grant_option_only: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let before = grant_option_roles(owner, acl.as_deref());
    let entries = materialize(owner, acl);
    let Some(position) = entries
        .iter()
        .position(|entry| entry.role == grantee && entry.grantor == grantor)
    else {
        return Ok(());
    };
    if grant_option_only {
        if !entries[position].grant_option {
            return Ok(());
        }
        entries[position].grant_option = false;
    } else {
        entries.remove(position);
    }
    revoke_dependents(owner, acl, &before, cascade)
}

fn revoke_dependents(
    owner: RoleIdentity,
    acl: &mut Option<Vec<ObjectAclEntry>>,
    before: &BTreeSet<RoleIdentity>,
    cascade: bool,
) -> Result<(), SQLError> {
    loop {
        let current = grant_option_roles(owner, acl.as_deref());
        let lost = before
            .difference(&current)
            .copied()
            .collect::<BTreeSet<_>>();
        let Some(entries) = acl.as_mut() else {
            return Ok(());
        };
        if lost.is_empty() || !entries.iter().any(|entry| lost.contains(&entry.grantor)) {
            return Ok(());
        }
        if !cascade {
            return Err(SQLError::Diagnostic {
                sqlstate: "2BP01".into(),
                message: "dependent privileges exist".into(),
                detail: None,
                hint: Some("Use CASCADE to revoke them too.".into()),
            });
        }
        entries.retain(|entry| !lost.contains(&entry.grantor));
    }
}

/// `aclnewowner`: the new owner replaces the old one as grantee and grantor, and entries that become identical merge.
pub fn rewrite_owner(
    acl: &mut Option<Vec<ObjectAclEntry>>,
    old_owner: RoleIdentity,
    new_owner: RoleIdentity,
) {
    let Some(entries) = acl.as_mut() else {
        return;
    };
    for entry in entries.iter_mut() {
        if entry.role == Some(old_owner) {
            entry.role = Some(new_owner);
        }
        if entry.grantor == old_owner {
            entry.grantor = new_owner;
        }
    }
    let mut merged: Vec<ObjectAclEntry> = Vec::with_capacity(entries.len());
    for entry in std::mem::take(entries) {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing.role == entry.role && existing.grantor == entry.grantor)
        {
            existing.grant_option |= entry.grant_option;
        } else {
            merged.push(entry);
        }
    }
    *entries = merged;
}

/// Validate the role identities and grant paths of an ACL: every endpoint names a role incarnation, PUBLIC holds no grant option, no grant path repeats, and every grantor's grant option is reachable from the owner.
pub fn validate_acl(
    owner: RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
    object_kind: &str,
) -> Result<(), SQLError> {
    let invalid =
        |message: &str| SQLError::Internal(format!("invalid {object_kind} authority: {message}"));
    if !owner.is_valid() {
        return Err(invalid("missing owner incarnation"));
    }
    let reachable = grant_option_roles(owner, acl);
    let mut paths = BTreeSet::new();
    for entry in acl.into_iter().flatten() {
        if !entry.grantor.is_valid() || entry.role.is_some_and(|role| !role.is_valid()) {
            return Err(invalid("missing ACL endpoint incarnation"));
        }
        if entry.role.is_none() && entry.grant_option {
            return Err(invalid("PUBLIC cannot retain a grant option"));
        }
        if !paths.insert((entry.role, entry.grantor)) {
            return Err(invalid("duplicate ACL grant path"));
        }
        if !reachable.contains(&entry.grantor) {
            return Err(invalid("ACL grantor has no owner-rooted grant option"));
        }
    }
    Ok(())
}

/// The owner and every role named by the ACL.
pub fn acl_roles(owner: RoleIdentity, acl: Option<&[ObjectAclEntry]>) -> BTreeSet<RoleIdentity> {
    let mut identities = BTreeSet::from([owner]);
    for entry in acl.into_iter().flatten() {
        identities.extend(entry.role);
        identities.insert(entry.grantor);
    }
    identities
}

/// Record the names of roles that an ACL change newly references, excluding the owners.
pub fn added_acl_roles(
    before: (RoleIdentity, Option<&[ObjectAclEntry]>),
    after: (RoleIdentity, Option<&[ObjectAclEntry]>),
    roles: &BTreeMap<String, RoleDefinition>,
    object_kind: &str,
    added: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut old = acl_roles(before.0, before.1);
    old.remove(&before.0);
    let mut new = acl_roles(after.0, after.1);
    new.remove(&after.0);
    validate_acl(after.0, after.1, object_kind)?;
    for identity in new.difference(&old) {
        added.insert(
            identity
                .role_name(roles)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "invalid {object_kind} authority: missing ACL incarnation"
                    ))
                })?
                .to_owned(),
        );
    }
    Ok(())
}

/// Bind grantee names to role incarnations; PUBLIC stays unbound.
pub fn bind_grantees(
    grantees: &[AclGrantee],
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<Vec<Option<RoleIdentity>>, SQLError> {
    grantees
        .iter()
        .map(|grantee| {
            grantee
                .role_name()
                .map(|name| {
                    RoleReference::from(name)
                        .bind(roles)
                        .map(|role| role.identity())
                })
                .transpose()
        })
        .collect()
}

/// The WARNING of a GRANT or REVOKE whose current user holds no grant option, naming the object without its schema.
pub fn acl_warning(is_grant: bool, local_name: &str) -> crate::SQLNotice {
    privilege_warning(is_grant, false, local_name)
}

/// `ExecGrant_*`'s warning when a grantor holds none (or not all) of the privileges a `GRANT` or `REVOKE` names: `01007` for grants and `01006` for revocations.
pub fn privilege_warning(is_grant: bool, partial: bool, name: &str) -> crate::SQLNotice {
    let (message, sqlstate) = match (is_grant, partial) {
        (true, true) => (
            format!("not all privileges were granted for \"{name}\""),
            "01007",
        ),
        (true, false) => (
            format!("no privileges were granted for \"{name}\""),
            "01007",
        ),
        (false, true) => (
            format!("not all privileges could be revoked for \"{name}\""),
            "01006",
        ),
        (false, false) => (
            format!("no privileges could be revoked for \"{name}\""),
            "01006",
        ),
    };
    crate::SQLNotice::warning(message).with_sqlstate(sqlstate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(value: u8) -> RoleIdentity {
        RoleIdentity {
            oid: i64::from(value),
            object_id: [value; 16],
        }
    }

    fn grant_entry(grantee: u8, grantor: u8) -> ObjectAclEntry {
        ObjectAclEntry {
            role: Some(role(grantee)),
            grantor: role(grantor),
            grant_option: true,
        }
    }

    #[test]
    fn grant_option_reachability_requires_an_owner_root() {
        let disconnected_cycle = [grant_entry(2, 3), grant_entry(3, 2)];
        assert_eq!(
            grant_option_roles(role(1), Some(&disconnected_cycle)),
            BTreeSet::from([role(1)])
        );
        let rooted_cycle = [grant_entry(2, 1), grant_entry(3, 2), grant_entry(2, 3)];
        assert_eq!(
            grant_option_roles(role(1), Some(&rooted_cycle)),
            BTreeSet::from([role(1), role(2), role(3)])
        );
    }

    #[test]
    fn grant_option_reachability_accepts_an_independent_owner_path() {
        let acl = [
            grant_entry(2, 1),
            grant_entry(3, 2),
            grant_entry(3, 1),
            grant_entry(4, 3),
        ];
        assert_eq!(
            grant_option_roles(role(1), Some(&acl[2..])),
            BTreeSet::from([role(1), role(3), role(4)])
        );
    }

    #[test]
    fn revoking_a_grant_option_with_dependent_grants_requires_cascade() {
        let owner = role(1);
        let mut acl = None;
        grant(owner, &mut acl, Some(role(2)), owner, true);
        grant(owner, &mut acl, Some(role(3)), role(2), false);
        let error = revoke(owner, &mut acl.clone(), Some(role(2)), owner, true, false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2BP01"));
        assert_eq!(error.hint(), Some("Use CASCADE to revoke them too."));
        revoke(owner, &mut acl, Some(role(2)), owner, true, true).unwrap();
        assert_eq!(
            acl.unwrap(),
            [
                ObjectAclEntry {
                    role: None,
                    grantor: owner,
                    grant_option: false
                },
                ObjectAclEntry {
                    role: Some(owner),
                    grantor: owner,
                    grant_option: false
                },
                ObjectAclEntry {
                    role: Some(role(2)),
                    grantor: owner,
                    grant_option: false
                },
            ]
        );
    }
}
