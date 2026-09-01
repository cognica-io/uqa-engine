//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, BTreeSet, GrantRoleStmt, RoleAttribute, RoleDefinition, RoleMembership,
    RoleMembershipKey, RoleMembershipOptions, SQLError, Value, VecDeque,
};

pub(super) fn role_is_superuser(roles: &BTreeMap<String, RoleDefinition>, role: &str) -> bool {
    roles
        .get(role)
        .is_some_and(|definition| definition.has(RoleAttribute::Superuser))
}

pub(super) fn require_role_attribute_authority(
    roles: &BTreeMap<String, RoleDefinition>,
    current: &str,
    attributes: impl IntoIterator<Item = RoleAttribute>,
    action: &str,
) -> Result<(), SQLError> {
    let current_role = roles.get(current).ok_or_else(|| undefined_role(current))?;
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

pub(super) fn role_has_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    memberships.values().any(|membership| {
        membership.member == member && membership.role == role && membership.admin_option
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RolePrivilegeCheck {
    Member,
    Usage,
    Set,
    Admin,
}

pub(super) fn resolve_pg_has_role_identifier(
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

pub(super) fn role_privilege_text(value: &Value) -> Result<&str, SQLError> {
    match value {
        Value::Str(privilege) | Value::FixedChar(privilege) => Ok(privilege),
        _ => Err(SQLError::TypeMismatch(
            "pg_has_role privilege argument must be text".into(),
        )),
    }
}

pub(super) fn parse_pg_has_role_privileges(
    privileges: &str,
) -> Result<Vec<RolePrivilegeCheck>, SQLError> {
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

pub(super) fn pg_has_role_privilege(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    subject: Option<&str>,
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
    match privilege {
        RolePrivilegeCheck::Member => role_reaches(memberships, subject, target, |_| true),
        RolePrivilegeCheck::Usage => role_inherits(roles, memberships, subject, target),
        RolePrivilegeCheck::Set => role_can_set(roles, memberships, subject, target),
        RolePrivilegeCheck::Admin => role_has_transitive_admin(memberships, subject, target),
    }
}

pub(super) fn role_has_transitive_admin(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    let mut queue = VecDeque::from([member.to_string()]);
    let mut visited = BTreeSet::from([member.to_string()]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships
            .values()
            .filter(|membership| membership.member == current)
        {
            if membership.role == role && membership.admin_option {
                return true;
            }
            if visited.insert(membership.role.clone()) {
                queue.push_back(membership.role.clone());
            }
        }
    }
    false
}

pub(super) fn role_reaches(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
    usable: impl Fn(&RoleMembership) -> bool,
) -> bool {
    if member == role {
        return true;
    }
    let mut queue = VecDeque::from([member.to_string()]);
    let mut visited = BTreeSet::from([member.to_string()]);
    while let Some(current) = queue.pop_front() {
        for membership in memberships
            .values()
            .filter(|membership| membership.member == current && usable(membership))
        {
            if membership.role == role {
                return true;
            }
            if visited.insert(membership.role.clone()) {
                queue.push_back(membership.role.clone());
            }
        }
    }
    false
}

pub(crate) fn role_can_set(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    role_is_superuser(roles, member)
        || role_reaches(memberships, member, role, |membership| {
            membership.set_option
        })
}

pub(crate) fn role_inherits(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    member: &str,
    role: &str,
) -> bool {
    role_is_superuser(roles, member)
        || role_reaches(memberships, member, role, |membership| {
            membership.inherit_option
        })
}

pub(super) fn membership_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0LP01".into(),
        message: message.into(),
    }
}

pub(super) fn undefined_role(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("role \"{name}\" does not exist"),
    }
}

pub(super) fn apply_grant_role_statement(
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &str,
    statement: &GrantRoleStmt,
) -> Result<(), SQLError> {
    for role in statement
        .granted_roles
        .iter()
        .chain(statement.grantee_roles.iter())
    {
        if !roles.contains_key(role) {
            return Err(undefined_role(role));
        }
    }
    let grantor = statement.grantor.as_deref().unwrap_or(current);
    if !roles.contains_key(grantor) {
        return Err(undefined_role(grantor));
    }
    if statement.grantor.is_some() && !role_can_set(roles, memberships, current, grantor) {
        return Err(insufficient_privilege(&format!(
            "permission denied to grant privileges as role \"{grantor}\""
        )));
    }
    for role in &statement.granted_roles {
        let superuser_revoke = !statement.is_grant && role_is_superuser(roles, current);
        if !superuser_revoke
            && !role_is_superuser(roles, grantor)
            && !role_has_admin(memberships, grantor, role)
        {
            return Err(insufficient_privilege(&format!(
                "permission denied to {} role \"{role}\"",
                if statement.is_grant {
                    "grant"
                } else {
                    "revoke"
                }
            )));
        }
    }
    for role in &statement.granted_roles {
        for member in &statement.grantee_roles {
            let key = RoleMembershipKey {
                role: role.clone(),
                member: member.clone(),
                grantor: grantor.to_string(),
            };
            if statement.is_grant {
                if role_reaches(memberships, role, member, |_| true) {
                    return Err(membership_error(format!(
                        "role \"{role}\" is a member of role \"{member}\""
                    )));
                }
                insert_membership(memberships, role, member, grantor, statement.options, roles);
            } else if statement.options == RoleMembershipOptions::default() {
                revoke_membership(memberships, &key, statement.cascade, true)?;
            } else if let Some(existing) = memberships.get(&key).cloned() {
                if statement.options.admin == Some(false) && existing.admin_option {
                    clear_membership_admin(memberships, &key, statement.cascade)?;
                }
                if let Some(membership) = memberships.get_mut(&key) {
                    if statement.options.inherit == Some(false) {
                        membership.inherit_option = false;
                    }
                    if statement.options.set == Some(false) {
                        membership.set_option = false;
                    }
                }
            }
        }
    }
    Ok(())
}

pub(super) fn insert_membership(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &str,
    member: &str,
    grantor: &str,
    options: RoleMembershipOptions,
    roles: &BTreeMap<String, RoleDefinition>,
) {
    let key = RoleMembershipKey {
        role: role.to_string(),
        member: member.to_string(),
        grantor: grantor.to_string(),
    };
    if let Some(existing) = memberships.get_mut(&key) {
        if let Some(value) = options.admin {
            existing.admin_option = value;
        }
        if let Some(value) = options.inherit {
            existing.inherit_option = value;
        }
        if let Some(value) = options.set {
            existing.set_option = value;
        }
        return;
    }
    let oid = allocate_role_membership_oid(memberships, &key);
    memberships.insert(
        key,
        RoleMembership {
            oid,
            role: role.to_string(),
            member: member.to_string(),
            grantor: grantor.to_string(),
            admin_option: options.admin.unwrap_or(false),
            inherit_option: options.inherit.unwrap_or_else(|| {
                roles
                    .get(member)
                    .is_some_and(|role| role.has(RoleAttribute::Inherit))
            }),
            set_option: options.set.unwrap_or(true),
        },
    );
}

pub(super) fn allocate_role_membership_oid(
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for part in [&key.role, &key.member, &key.grantor] {
        for byte in part.as_bytes().iter().copied().chain(std::iter::once(0)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
    }
    let mut oid = 2_500_000_000_i64 + i64::try_from(hash % 1_500_000_000).unwrap_or(0);
    while memberships.values().any(|membership| membership.oid == oid) {
        oid = if oid == 3_999_999_999 {
            2_500_000_000
        } else {
            oid + 1
        };
    }
    oid
}

pub(super) fn clear_membership_admin(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
    cascade: bool,
) -> Result<(), SQLError> {
    let Some(existing) = memberships.get_mut(key) else {
        return Ok(());
    };
    existing.admin_option = false;
    revoke_dependent_memberships(memberships, &key.role, &key.member, cascade)
}

pub(super) fn revoke_membership(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    key: &RoleMembershipKey,
    cascade: bool,
    check_dependents: bool,
) -> Result<(), SQLError> {
    let Some(existing) = memberships.remove(key) else {
        return Ok(());
    };
    if check_dependents && existing.admin_option {
        revoke_dependent_memberships(memberships, &existing.role, &existing.member, cascade)?;
    }
    Ok(())
}

pub(super) fn revoke_dependent_memberships(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    role: &str,
    former_admin: &str,
    cascade: bool,
) -> Result<(), SQLError> {
    if role_has_admin(memberships, former_admin, role) {
        return Ok(());
    }
    let dependent = memberships
        .iter()
        .filter(|(_, membership)| membership.role == role && membership.grantor == former_admin)
        .map(|(key, _)| key.clone())
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

pub(super) fn insufficient_privilege(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42501".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::role_oid;
    use super::*;

    fn role(name: &str) -> RoleDefinition {
        RoleDefinition {
            oid: role_oid(name),
            name: name.into(),
            attributes: BTreeSet::new(),
            connection_limit: -1,
        }
    }

    fn membership(
        memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
        role: &str,
        member: &str,
        admin: bool,
        inherit: bool,
        set: bool,
    ) {
        let key = RoleMembershipKey {
            role: role.into(),
            member: member.into(),
            grantor: "uqa".into(),
        };
        memberships.insert(
            key.clone(),
            RoleMembership {
                oid: role_oid(&format!("{role}/{member}")),
                role: key.role,
                member: key.member,
                grantor: key.grantor,
                admin_option: admin,
                inherit_option: inherit,
                set_option: set,
            },
        );
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
        .map(|name| (name.into(), role(name)))
        .chain([("uqa".into(), RoleDefinition::bootstrap())])
        .collect::<BTreeMap<_, _>>();
        let mut memberships = BTreeMap::new();
        membership(&mut memberships, "parent", "middle", false, true, false);
        membership(&mut memberships, "middle", "leaf", false, true, true);
        membership(&mut memberships, "parent", "noinherit", false, false, true);
        membership(&mut memberships, "parent", "admin", true, false, false);
        membership(&mut memberships, "admin", "admin_leaf", false, false, false);

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
