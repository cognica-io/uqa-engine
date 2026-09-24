//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::test_support::{insert_membership, Names};
use super::*;
use crate::ast::RoleSpecification;

fn roles() -> BTreeMap<String, RoleDefinition> {
    let mut roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    for (index, name) in ["target", "member", "grantor", "actor", "CURRENT_USER"]
        .into_iter()
        .enumerate()
    {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_001 + index as i64;
        role.object_id = [index as u8 + 1; 16];
        role.attributes = [RoleAttribute::Inherit].into_iter().collect();
        roles.insert(name.into(), role);
    }
    roles
}

fn statement() -> GrantRoleStmt {
    GrantRoleStmt {
        granted_roles: vec!["target".into()],
        grantee_roles: vec!["member".into()],
        is_grant: true,
        options: RoleMembershipOptions::default(),
        grantor: None,
        cascade: false,
    }
}

fn bind(roles: &BTreeMap<String, RoleDefinition>, statement: &GrantRoleStmt) -> MembershipTarget {
    let recipients = MembershipRecipients::bind(&Names("uqa"), roles, statement).unwrap();
    MembershipTarget::authorize(
        roles,
        &BTreeMap::new(),
        "uqa",
        &"target".into(),
        &recipients,
        statement,
    )
    .unwrap()
}

#[test]
fn captured_grant_authority_and_target_survive_an_admin_revoke_and_name_replacement() {
    let mut roles = roles();
    let mut memberships = BTreeMap::new();
    insert_membership(
        &mut memberships,
        "target",
        "grantor",
        "uqa",
        RoleMembershipOptions {
            admin: Some(true),
            ..RoleMembershipOptions::default()
        },
        &roles,
    )
    .unwrap();
    insert_membership(
        &mut memberships,
        "grantor",
        "actor",
        "uqa",
        RoleMembershipOptions {
            inherit: Some(true),
            ..RoleMembershipOptions::default()
        },
        &roles,
    )
    .unwrap();
    let statement = statement();
    let recipients = MembershipRecipients::bind(&Names("actor"), &roles, &statement).unwrap();
    let bound = MembershipTarget::authorize(
        &roles,
        &memberships,
        "actor",
        &"target".into(),
        &recipients,
        &statement,
    )
    .unwrap();
    assert_eq!(bound.grantor.identity(), roles["grantor"].identity());
    memberships.clear();
    roles.get_mut("target").unwrap().object_id = [99; 16];
    bound.validate_graph(&memberships).unwrap();
    let MembershipChange::Insert(insert) = bound
        .change_for_member(&roles, &memberships, &bound.members[0])
        .unwrap()
    else {
        panic!("new membership")
    };
    assert_eq!(insert.role.identity(), bound.role.identity());
    assert_ne!(insert.role.identity(), roles["target"].identity());
    assert_eq!(insert.grantor.identity(), bound.grantor.identity());
}

#[test]
fn member_default_uses_current_attributes_of_the_captured_identity() {
    for change in ["attributes", "dropped", "replaced", "renamed"] {
        for explicit in [false, true] {
            let mut roles = roles();
            let mut statement = statement();
            statement.options.inherit = explicit.then_some(true);
            let bound = bind(&roles, &statement);
            let mut member = roles.remove("member").unwrap();
            match change {
                "attributes" => {
                    member.attributes.remove(&RoleAttribute::Inherit);
                }
                "replaced" => member.object_id = [99; 16],
                "renamed" => member.name = "renamed".into(),
                "dropped" => {}
                _ => unreachable!(),
            }
            if change != "dropped" {
                roles.insert(member.name.clone(), member);
            }
            let result = bound.change_for_member(&roles, &BTreeMap::new(), &bound.members[0]);
            if !explicit && matches!(change, "dropped" | "replaced") {
                assert_eq!(result.err().unwrap().sqlstate(), Some("XX000"));
            } else {
                let MembershipChange::Insert(insert) = result.unwrap() else {
                    panic!("new membership")
                };
                assert_eq!(insert.inherit_option, explicit || change != "attributes");
                assert_eq!(insert.member.identity(), bound.members[0].identity());
            }
        }
    }
}

#[test]
fn explicit_grantor_binding_precedes_recipients_and_named_keywords_remain_literal() {
    let roles = roles();
    let mut statement = statement();
    statement.grantee_roles = vec!["missing_member".into()];
    statement.grantor = Some("missing_grantor".into());
    let error = MembershipRecipients::bind(&Names("uqa"), &roles, &statement)
        .err()
        .unwrap();
    assert!(error.to_string().contains("missing_grantor"));
    statement.grantor = Some(RoleSpecification::CurrentUser);
    assert!(
        MembershipRecipients::bind(&Names("uqa"), &roles, &statement)
            .err()
            .unwrap()
            .to_string()
            .contains("missing_member")
    );
    statement.grantee_roles = vec!["CURRENT_USER".into(), RoleSpecification::CurrentUser];
    let recipients = MembershipRecipients::bind(&Names("uqa"), &roles, &statement).unwrap();
    assert_eq!(
        recipients.members[0].identity(),
        roles["CURRENT_USER"].identity()
    );
    assert_eq!(recipients.members[1].identity(), roles["uqa"].identity());
    assert_eq!(
        recipients.grantor.unwrap().identity(),
        roles["uqa"].identity()
    );
}

#[test]
fn unchanged_grants_and_absent_revokes_return_distinct_notices_without_an_oid_request() {
    let roles = roles();
    let mut statement = statement();
    let bound = bind(&roles, &statement);
    let MembershipChange::Insert(insert) = bound
        .change_for_member(&roles, &BTreeMap::new(), &bound.members[0])
        .unwrap()
    else {
        panic!("new membership")
    };
    let membership = insert.with_oid(42_000).unwrap();
    let memberships = BTreeMap::from([(membership.key(), membership)]);
    let MembershipChange::Notice { level, message } = bound
        .change_for_member(&roles, &memberships, &bound.members[0])
        .unwrap()
    else {
        panic!("notice")
    };
    assert_eq!(level, "NOTICE");
    assert_eq!(
        message,
        "role \"member\" has already been granted membership in role \"target\" by role \"uqa\""
    );
    statement.is_grant = false;
    let bound = bind(&roles, &statement);
    let message = MembershipRevocation::new(&bound, &BTreeMap::new())
        .member(&roles, &bound.members[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        message,
        "role \"member\" has not been granted membership in role \"target\" by role \"uqa\""
    );
}

#[test]
fn group_checks_target_authority_before_recipients_and_accepts_inherited_admin() {
    let roles = roles();
    let mut memberships = BTreeMap::new();
    let mut statement = AlterRoleStmt {
        name: "missing_target".into(),
        members: vec!["missing_member".into()],
        membership_action: Some(RoleMembershipAction::Add),
        attributes: BTreeMap::new(),
        connection_limit: None,
    };
    let authorize = |memberships: &BTreeMap<_, _>, statement: &AlterRoleStmt| {
        MembershipTarget::authorize_group(&Names("actor"), &roles, memberships, "actor", statement)
    };
    let error = authorize(&memberships, &statement).err().unwrap();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert!(error.to_string().contains("missing_target"));
    statement.name = "target".into();
    let error = authorize(&memberships, &statement).err().unwrap();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert!(error
        .to_string()
        .contains("permission denied to alter role"));
    for (target, member, admin, inherit) in [
        ("target", "grantor", true, false),
        ("grantor", "actor", false, true),
    ] {
        insert_membership(
            &mut memberships,
            target,
            member,
            "uqa",
            RoleMembershipOptions {
                admin: Some(admin),
                inherit: Some(inherit),
                set: Some(false),
            },
            &roles,
        )
        .unwrap();
    }
    let error = authorize(&memberships, &statement).err().unwrap();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert!(error.to_string().contains("missing_member"));
    statement.members = vec!["member".into()];
    for action in [RoleMembershipAction::Add, RoleMembershipAction::Drop] {
        statement.membership_action = Some(action);
        let bound = authorize(&memberships, &statement).unwrap();
        assert_eq!(bound.grantor.identity(), roles["grantor"].identity());
        assert_eq!(bound.is_grant, action == RoleMembershipAction::Add);
    }
}

#[test]
fn revocation_plans_duplicate_recipients_against_original_rows_and_preserves_row_oids() {
    let roles = roles();
    for options in [
        RoleMembershipOptions::default(),
        RoleMembershipOptions {
            admin: Some(false),
            ..RoleMembershipOptions::default()
        },
        RoleMembershipOptions {
            inherit: Some(false),
            ..RoleMembershipOptions::default()
        },
        RoleMembershipOptions {
            set: Some(false),
            ..RoleMembershipOptions::default()
        },
    ] {
        let mut memberships = BTreeMap::new();
        insert_membership(
            &mut memberships,
            "target",
            "member",
            "uqa",
            RoleMembershipOptions {
                admin: Some(true),
                inherit: Some(true),
                set: Some(true),
            },
            &roles,
        )
        .unwrap();
        insert_membership(
            &mut memberships,
            "target",
            "actor",
            "member",
            RoleMembershipOptions::default(),
            &roles,
        )
        .unwrap();
        let original = memberships.clone();
        let mut statement = statement();
        statement.is_grant = false;
        statement.options = options;
        statement.cascade = true;
        let bound = bind(&roles, &statement);
        let mut plan = MembershipRevocation::new(&bound, &memberships);
        for _ in 0..2 {
            assert!(plan.member(&roles, &bound.members[0]).unwrap().is_none());
        }
        assert_eq!(memberships, original);
        for update in plan.into_updates() {
            if let Some(after) = &update.after {
                assert_eq!(after.oid, update.before.oid);
            }
            update.apply(&mut memberships).unwrap();
        }
        let cascade = options.admin == Some(false) || options == RoleMembershipOptions::default();
        assert_eq!(
            memberships.values().any(|row| row.member.name == "actor"),
            !cascade
        );
        assert_eq!(
            memberships.values().any(|row| row.member.name == "member"),
            options != RoleMembershipOptions::default()
        );
    }
}
