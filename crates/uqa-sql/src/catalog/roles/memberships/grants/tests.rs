//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::test_support::{apply_grant_role_statement, insert_membership};
use super::super::*;
use crate::ast::{CreateRoleStmt, GrantRoleStmt, RoleMembershipOptions};

fn roles() -> BTreeMap<String, RoleDefinition> {
    let mut roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    for (index, name) in [
        "target",
        "root2",
        "admin",
        "middle",
        "leaf",
        "set_only",
        "delegate",
        "recipient",
        "super_target",
    ]
    .into_iter()
    .enumerate()
    {
        let mut attributes = BTreeSet::from([RoleAttribute::Inherit]);
        if matches!(name, "root2" | "super_target") {
            attributes.insert(RoleAttribute::Superuser);
        }
        roles.insert(
            name.into(),
            RoleDefinition::from_create(
                &CreateRoleStmt {
                    name: name.into(),
                    attributes,
                    connection_limit: -1,
                    in_roles: Vec::new(),
                    role_members: Vec::new(),
                    admin_members: Vec::new(),
                },
                20_000 + i64::try_from(index).unwrap(),
                [u8::try_from(index + 1).unwrap(); 16],
            ),
        );
    }
    roles
}

fn statement(target: &str, members: &[&str], grantor: Option<&str>, admin: bool) -> GrantRoleStmt {
    GrantRoleStmt {
        granted_roles: vec![target.into()],
        grantee_roles: members.iter().map(|s| (*s).into()).collect(),
        grantor: grantor.map(Into::into),
        is_grant: true,
        options: RoleMembershipOptions {
            admin: admin.then_some(true),
            ..RoleMembershipOptions::default()
        },
        cascade: false,
    }
}

fn edge(
    memberships: &mut BTreeMap<RoleMembershipKey, RoleMembership>,
    roles: &BTreeMap<String, RoleDefinition>,
    role: &str,
    member: &str,
    grantor: &str,
    options: RoleMembershipOptions,
) {
    insert_membership(memberships, role, member, grantor, options, roles).unwrap();
}

fn admins(roles: &BTreeMap<String, RoleDefinition>) -> BTreeMap<RoleMembershipKey, RoleMembership> {
    let mut members = BTreeMap::new();
    edge(
        &mut members,
        roles,
        "target",
        "admin",
        "uqa",
        RoleMembershipOptions {
            admin: Some(true),
            inherit: Some(false),
            set: Some(false),
        },
    );
    for (role, member, inherit, set) in [
        ("admin", "middle", true, false),
        ("middle", "leaf", true, false),
        ("admin", "set_only", false, true),
    ] {
        edge(
            &mut members,
            roles,
            role,
            member,
            "uqa",
            RoleMembershipOptions {
                admin: None,
                inherit: Some(inherit),
                set: Some(set),
            },
        );
    }
    members
}

fn key(member: &str, grantor: &str) -> RoleMembershipKey {
    let roles = roles();
    RoleMembershipKey {
        role: roles["target"].identity(),
        member: roles[member].identity(),
        grantor: roles[grantor].identity(),
    }
}

#[test]
fn superuser_default_grantor_is_bootstrap_for_grant_and_revoke() {
    let roles = roles();
    let mut members = admins(&roles);
    let mut grant = statement("target", &["recipient"], None, false);
    apply_grant_role_statement(&roles, &mut members, "root2", &grant).unwrap();
    assert!(members.contains_key(&key("recipient", "uqa")));
    apply_grant_role_statement(&roles, &mut members, "admin", &grant).unwrap();
    grant.is_grant = false;
    apply_grant_role_statement(&roles, &mut members, "root2", &grant).unwrap();
    assert!(!members.contains_key(&key("recipient", "uqa")));
    assert!(members.contains_key(&key("recipient", "admin")));
}

#[test]
fn inherited_admin_selects_the_nearest_actual_admin_as_grantor() {
    let roles = roles();
    let mut members = admins(&roles);
    let grant = statement("target", &["recipient"], None, false);
    apply_grant_role_statement(&roles, &mut members, "leaf", &grant).unwrap();
    assert!(members.contains_key(&key("recipient", "admin")));
    let direct = statement("target", &["leaf"], None, true);
    apply_grant_role_statement(&roles, &mut members, "uqa", &direct).unwrap();
    apply_grant_role_statement(&roles, &mut members, "leaf", &grant).unwrap();
    assert!(members.contains_key(&key("recipient", "leaf")));
}

#[test]
fn explicit_grantor_requires_inherited_privileges_and_its_own_admin_option() {
    let roles = roles();
    let mut members = admins(&roles);
    let grant = statement("target", &["recipient"], Some("admin"), false);
    apply_grant_role_statement(&roles, &mut members, "leaf", &grant).unwrap();
    assert_eq!(
        apply_grant_role_statement(&roles, &mut members, "set_only", &grant)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    let grant = statement("target", &["recipient"], Some("root2"), false);
    assert_eq!(
        apply_grant_role_statement(&roles, &mut members, "root2", &grant)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
}

#[test]
fn explicit_revoke_uses_inherited_grantor_privileges_without_own_admin() {
    let roles = roles();
    let mut members = admins(&roles);
    let mut grant = statement("target", &["recipient"], Some("admin"), false);
    apply_grant_role_statement(&roles, &mut members, "admin", &grant).unwrap();
    grant.is_grant = false;
    let before = members.clone();
    assert_eq!(
        apply_grant_role_statement(&roles, &mut members, "set_only", &grant)
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(members, before);
    apply_grant_role_statement(&roles, &mut members, "leaf", &grant).unwrap();
    assert!(!members.contains_key(&key("recipient", "admin")));
    grant.grantor = Some("root2".into());
    apply_grant_role_statement(&roles, &mut members, "root2", &grant).unwrap();
    assert!(!role_has_admin(
        &members,
        roles["root2"].identity(),
        roles["target"].identity()
    ));
}

#[test]
fn admin_option_cannot_authorize_grant_or_revoke_of_a_superuser_role() {
    let roles = roles();
    let mut members = admins(&roles);
    let admin = statement("super_target", &["admin"], None, true);
    apply_grant_role_statement(&roles, &mut members, "uqa", &admin).unwrap();
    let mut grant = statement("super_target", &["recipient"], None, false);
    for is_grant in [true, false] {
        grant.is_grant = is_grant;
        assert_eq!(
            apply_grant_role_statement(&roles, &mut members, "admin", &grant)
                .unwrap_err()
                .sqlstate(),
            Some("42501")
        );
    }
}

#[test]
fn admin_back_grants_require_a_path_independent_of_all_recipients() {
    let roles = roles();
    let mut members = admins(&roles);
    apply_grant_role_statement(
        &roles,
        &mut members,
        "admin",
        &statement("target", &["delegate"], None, true),
    )
    .unwrap();
    for member in ["admin", "uqa", "delegate"] {
        let grant = statement("target", &[member], None, true);
        assert_eq!(
            apply_grant_role_statement(&roles, &mut members, "delegate", &grant)
                .unwrap_err()
                .sqlstate(),
            Some("0LP01")
        );
    }
    apply_grant_role_statement(
        &roles,
        &mut members,
        "uqa",
        &statement("target", &["delegate"], None, true),
    )
    .unwrap();
    apply_grant_role_statement(
        &roles,
        &mut members,
        "delegate",
        &statement("target", &["admin"], None, true),
    )
    .unwrap();
    assert!(members[&key("admin", "delegate")].admin_option);
}

#[test]
fn admin_back_grant_checks_all_recipients_as_one_proposal() {
    let roles = roles();
    let mut members = BTreeMap::new();
    for (member, grantor) in [
        ("admin", "uqa"),
        ("middle", "uqa"),
        ("delegate", "admin"),
        ("delegate", "middle"),
    ] {
        edge(
            &mut members,
            &roles,
            "target",
            member,
            grantor,
            RoleMembershipOptions {
                admin: Some(true),
                ..RoleMembershipOptions::default()
            },
        );
    }
    for member in ["admin", "middle"] {
        let mut candidate = members.clone();
        apply_grant_role_statement(
            &roles,
            &mut candidate,
            "delegate",
            &statement("target", &[member], None, true),
        )
        .unwrap();
    }
    let before = members.clone();
    assert_eq!(
        apply_grant_role_statement(
            &roles,
            &mut members,
            "delegate",
            &statement("target", &["admin", "middle"], None, true),
        )
        .unwrap_err()
        .sqlstate(),
        Some("0LP01")
    );
    assert_eq!(members, before);
}

#[test]
fn grantor_and_recipient_lookup_precede_per_target_authorization() {
    let roles = roles();
    let mut members = admins(&roles);
    let before = members.clone();
    for (grant, state, message) in [
        (
            statement(
                "missing_target",
                &["missing_member"],
                Some("missing_grantor"),
                false,
            ),
            "42704",
            "role \"missing_grantor\" does not exist",
        ),
        (
            statement("missing_target", &["missing_member"], None, false),
            "42704",
            "role \"missing_member\" does not exist",
        ),
        (
            GrantRoleStmt {
                granted_roles: vec!["target".into(), "missing_target".into()],
                ..statement("target", &["admin"], None, false)
            },
            "42501",
            "permission denied to grant role \"target\"",
        ),
    ] {
        let error =
            apply_grant_role_statement(&roles, &mut members, "recipient", &grant).unwrap_err();
        assert!(
            matches!(error, SQLError::Routine { sqlstate, message: actual } if sqlstate == state && actual == message)
        );
        assert_eq!(members, before);
    }
}
