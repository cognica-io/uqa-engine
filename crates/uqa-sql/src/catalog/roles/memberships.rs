//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    identity::{RoleBinding, RoleSubject},
    RoleDefinition, RoleIdentity, RoleMembership, RoleMembershipKey,
};
use crate::ast::RoleAttribute;
use crate::SQLError;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uqa_core::Value;

pub mod command;
mod grants;
#[cfg(test)]
pub(crate) mod test_support;

pub fn role_is_superuser(
    roles: &BTreeMap<String, RoleDefinition>,
    role: &(impl RoleSubject + ?Sized),
) -> bool {
    role.role_definition(roles)
        .is_some_and(|definition| definition.has(RoleAttribute::Superuser))
}

pub fn require_role_attribute_authority(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &(impl RoleSubject + ?Sized),
    attributes: impl IntoIterator<Item = RoleAttribute>,
    action: &str,
) -> Result<(), SQLError> {
    let current_role = current
        .role_definition(roles)
        .ok_or_else(|| insufficient_privilege(&format!("permission denied to {action}")))?;
    if current_role.has(RoleAttribute::Superuser) {
        return Ok(());
    }
    for attribute in attributes {
        let restricted = matches!(
            attribute,
            RoleAttribute::Superuser
                | RoleAttribute::CreateRole
                | RoleAttribute::CreateDb
                | RoleAttribute::Replication
                | RoleAttribute::BypassRls
        );
        if restricted && !current_role.has(attribute) {
            return Err(insufficient_privilege(&format!(
                "permission denied to {action}"
            )));
        }
    }
    Ok(())
}

pub fn role_has_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: RoleIdentity,
    role: RoleIdentity,
) -> bool {
    memberships.values().any(|membership| {
        membership.member.identity() == member
            && membership.role.identity() == role
            && membership.admin_option
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolePrivilegeCheck {
    Member,
    Usage,
    Set,
    Admin,
}

pub fn resolve_pg_has_role_identifier(
    value: &Value,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<Option<String>, SQLError> {
    match value {
        Value::Str(name) | Value::FixedChar(name) => {
            if roles.contains_key(name) {
                Ok(Some(name.clone()))
            } else {
                Err(undefined_role(name))
            }
        }
        Value::Int(oid) => Ok(roles
            .values()
            .find(|role| role.oid == *oid)
            .map(|role| role.name.clone())),
        _ => Err(SQLError::TypeMismatch(
            "pg_has_role role arguments must be name or oid".into(),
        )),
    }
}

pub fn role_privilege_text(value: &Value) -> Result<&str, SQLError> {
    match value {
        Value::Str(privilege) | Value::FixedChar(privilege) => Ok(privilege),
        _ => Err(SQLError::TypeMismatch(
            "pg_has_role privilege argument must be text".into(),
        )),
    }
}

pub fn parse_pg_has_role_privileges(privileges: &str) -> Result<Vec<RolePrivilegeCheck>, SQLError> {
    privileges
        .split(',')
        .map(|privilege| {
            let privilege = privilege.trim();
            if [
                "MEMBER WITH ADMIN OPTION",
                "MEMBER WITH GRANT OPTION",
                "USAGE WITH ADMIN OPTION",
                "USAGE WITH GRANT OPTION",
                "SET WITH ADMIN OPTION",
                "SET WITH GRANT OPTION",
            ]
            .iter()
            .any(|candidate| privilege.eq_ignore_ascii_case(candidate))
            {
                return Ok(RolePrivilegeCheck::Admin);
            }
            if privilege.eq_ignore_ascii_case("MEMBER") {
                Ok(RolePrivilegeCheck::Member)
            } else if privilege.eq_ignore_ascii_case("USAGE") {
                Ok(RolePrivilegeCheck::Usage)
            } else if privilege.eq_ignore_ascii_case("SET") {
                Ok(RolePrivilegeCheck::Set)
            } else {
                Err(SQLError::Routine {
                    sqlstate: "22023".into(),
                    message: format!("unrecognized privilege type: \"{privilege}\""),
                })
            }
        })
        .collect()
}

pub fn pg_has_role_privilege(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    subject: Option<&(impl RoleSubject + ?Sized)>,
    target: Option<&str>,
    privilege: RolePrivilegeCheck,
) -> bool {
    let Some(subject) = subject else {
        return false;
    };
    if role_is_superuser(roles, subject) {
        return true;
    }
    let Some(target) = target else {
        return false;
    };
    let Some(subject) = subject.role_definition(roles) else {
        return false;
    };
    let Some(target) = roles.get(target) else {
        return false;
    };
    let (subject, target) = (subject.identity(), target.identity());
    match privilege {
        RolePrivilegeCheck::Member => role_reaches(memberships, subject, target, |_| true),
        RolePrivilegeCheck::Usage => {
            role_reaches(memberships, subject, target, |edge| edge.inherit_option)
        }
        RolePrivilegeCheck::Set => {
            role_reaches(memberships, subject, target, |edge| edge.set_option)
        }
        RolePrivilegeCheck::Admin => role_has_transitive_admin(memberships, subject, target),
    }
}

pub fn role_has_transitive_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: RoleIdentity,
    role: RoleIdentity,
) -> bool {
    let mut queue = VecDeque::from([member]);
    let mut visited = BTreeSet::from([member]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships
            .values()
            .filter(|membership| membership.member.identity() == current)
        {
            if membership.role.identity() == role && membership.admin_option {
                return true;
            }
            if visited.insert(membership.role.identity()) {
                queue.push_back(membership.role.identity());
            }
        }
    }
    false
}

pub fn role_reaches(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: RoleIdentity,
    role: RoleIdentity,
    usable: impl Fn(&RoleMembership) -> bool,
) -> bool {
    if member == role {
        return true;
    }
    let mut queue = VecDeque::from([member]);
    let mut visited = BTreeSet::from([member]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships
            .values()
            .filter(|membership| membership.member.identity() == current && usable(membership))
        {
            if membership.role.identity() == role {
                return true;
            }
            if visited.insert(membership.role.identity()) {
                queue.push_back(membership.role.identity());
            }
        }
    }
    false
}

pub fn role_can_set(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &(impl RoleSubject + ?Sized),
    role: &str,
) -> bool {
    let Some(member) = member.role_definition(roles) else {
        return false;
    };
    member.has(RoleAttribute::Superuser)
        || roles.get(role).is_some_and(|role| {
            role_reaches(memberships, member.identity(), role.identity(), |edge| {
                edge.set_option
            })
        })
}

pub fn role_inherits(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &(impl RoleSubject + ?Sized),
    role: &(impl RoleSubject + ?Sized),
) -> bool {
    let Some(member) = member.role_definition(roles) else {
        return false;
    };
    member.has(RoleAttribute::Superuser)
        || role.role_definition(roles).is_some_and(|role| {
            role_reaches(memberships, member.identity(), role.identity(), |edge| {
                edge.inherit_option
            })
        })
}

pub fn membership_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0LP01".into(),
        message: message.into(),
    }
}

pub fn undefined_role(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    }
}

pub fn clear_membership_admin(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
    cascade: bool,
) -> Result<(), SQLError> {
    let Some(existing) = memberships.get_mut(key) else {
        return Ok(());
    };
    existing.admin_option = false;
    revoke_dependent_memberships(memberships, key.role, key.member, cascade)
}

pub fn revoke_membership(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
    cascade: bool,
    check_dependents: bool,
) -> Result<(), SQLError> {
    let Some(existing) = memberships.remove(key) else {
        return Ok(());
    };
    if check_dependents && existing.admin_option {
        revoke_dependent_memberships(
            memberships,
            existing.role.identity(),
            existing.member.identity(),
            cascade,
        )?;
    }
    Ok(())
}

pub fn revoke_dependent_memberships(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    role: RoleIdentity,
    former_admin: RoleIdentity,
    cascade: bool,
) -> Result<(), SQLError> {
    if role_has_admin(memberships, former_admin, role) {
        return Ok(());
    }
    let dependent = memberships
        .iter()
        .filter(|(_, membership)| {
            membership.role.identity() == role && membership.grantor.identity() == former_admin
        })
        .map(|(key, _)| *key)
        .collect::<Vec<_>>();
    if dependent.is_empty() {
        return Ok(());
    }
    if !cascade {
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: "dependent privileges exist".into(),
        });
    }
    for key in dependent {
        revoke_membership(memberships, &key, true, true)?;
    }
    Ok(())
}

pub fn insufficient_privilege(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42501".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::insert_membership;
    use super::*;
    use crate::ast::RoleMembershipOptions;

    fn role(name: &str, index: usize) -> RoleDefinition {
        RoleDefinition::from_create(
            &crate::ast::CreateRoleStmt {
                name: name.into(),
                attributes: BTreeSet::new(),
                connection_limit: -1,
                in_roles: Vec::new(),
                role_members: Vec::new(),
                admin_members: Vec::new(),
            },
            20_001 + index as i64,
            [index as u8 + 1; 16],
        )
    }

    fn membership(
        memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
        roles: &BTreeMap<String, RoleDefinition>,
        role: &str,
        member: &str,
        options: (bool, bool, bool),
    ) {
        insert_membership(
            memberships,
            role,
            member,
            "uqa",
            RoleMembershipOptions {
                admin: Some(options.0),
                inherit: Some(options.1),
                set: Some(options.2),
            },
            roles,
        )
        .unwrap();
    }

    #[test]
    fn pg_has_role_privilege_names_include_lists_and_admin_aliases() {
        assert_eq!(
            parse_pg_has_role_privileges(" member, USAGE , set ").unwrap(),
            vec![
                RolePrivilegeCheck::Member,
                RolePrivilegeCheck::Usage,
                RolePrivilegeCheck::Set,
            ]
        );
        for privilege in [
            "MEMBER WITH ADMIN OPTION",
            "USAGE WITH GRANT OPTION",
            "SET WITH ADMIN OPTION",
        ] {
            assert_eq!(
                parse_pg_has_role_privileges(privilege).unwrap(),
                vec![RolePrivilegeCheck::Admin]
            );
        }
        assert_eq!(
            parse_pg_has_role_privileges("ADMIN")
                .unwrap_err()
                .sqlstate(),
            Some("22023")
        );
    }

    #[test]
    fn pg_has_role_checks_member_usage_set_and_transitive_admin_independently() {
        let roles = [
            "parent",
            "middle",
            "leaf",
            "noinherit",
            "admin",
            "admin_leaf",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name.into(), role(name, index)))
        .chain([("uqa".into(), RoleDefinition::bootstrap())])
        .collect::<BTreeMap<_, _>>();
        let mut memberships = BTreeMap::new();
        for (target, member, options) in [
            ("parent", "middle", (false, true, false)),
            ("middle", "leaf", (false, true, true)),
            ("parent", "noinherit", (false, false, true)),
            ("parent", "admin", (true, false, false)),
            ("admin", "admin_leaf", (false, false, false)),
        ] {
            membership(&mut memberships, &roles, target, member, options);
        }

        assert!(pg_has_role_privilege(
            &roles,
            &memberships,
            Some("leaf"),
            Some("parent"),
            RolePrivilegeCheck::Member
        ));
        assert!(pg_has_role_privilege(
            &roles,
            &memberships,
            Some("leaf"),
            Some("parent"),
            RolePrivilegeCheck::Usage
        ));
        assert!(!pg_has_role_privilege(
            &roles,
            &memberships,
            Some("leaf"),
            Some("parent"),
            RolePrivilegeCheck::Set
        ));
        assert!(!pg_has_role_privilege(
            &roles,
            &memberships,
            Some("noinherit"),
            Some("parent"),
            RolePrivilegeCheck::Usage
        ));
        assert!(pg_has_role_privilege(
            &roles,
            &memberships,
            Some("noinherit"),
            Some("parent"),
            RolePrivilegeCheck::Set
        ));
        assert!(pg_has_role_privilege(
            &roles,
            &memberships,
            Some("admin_leaf"),
            Some("parent"),
            RolePrivilegeCheck::Admin
        ));
        assert!(!pg_has_role_privilege(
            &roles,
            &memberships,
            Some("parent"),
            Some("parent"),
            RolePrivilegeCheck::Admin
        ));
        assert!(pg_has_role_privilege(
            &roles,
            &memberships,
            Some("uqa"),
            None,
            RolePrivilegeCheck::Member
        ));
    }
}
