//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::roles::{RoleDefinitions, RoleMemberships};
use crate::{
    ast::{DatabasePrivilege, DatabaseRevokeBehavior, GrantDatabaseStmt},
    catalog::{
        roles::{identity::RoleBinding, RoleReference},
        security::database::{
            apply_database_acl, role_has_database_privilege_check as role_has_privilege,
            DatabaseAclPrivilege, DatabasePrivilegeCheck,
        },
    },
};
use std::sync::Arc;

fn role(name: &str, oid: i64, incarnation: u8) -> RoleDefinition {
    RoleDefinition {
        name: name.into(),
        oid,
        object_id: [incarnation; 16],
        revision: 1,
        attributes: uqa_core::memory::OwnedSet::new(),
        connection_limit: -1,
    }
}

fn fixture() -> (RoleDefinitions, DatabaseSecurity) {
    let roles = RoleDefinitions::try_from_iter(
        [
            role("owner", 20_000, 1),
            role("delegate", 20_001, 2),
            role("reader", 20_002, 3),
        ]
        .into_iter()
        .map(|role| (role.name.clone(), role)),
    )
    .unwrap();
    let create = DatabasePrivileges {
        create: true,
        ..DatabasePrivileges::default()
    };
    let security = DatabaseSecurity {
        role_owner: "owner".into(),
        acl: Some(vec![
            DatabaseAclEntry {
                role: "delegate".into(),
                grantor: None,
                privileges: create,
                grant_options: create,
            },
            DatabaseAclEntry {
                role: "reader".into(),
                grantor: Some("delegate".into()),
                privileges: create,
                grant_options: DatabasePrivileges::default(),
            },
            DatabaseAclEntry {
                role: uqa_core::catalog_acl::AclGrantee::Public,
                grantor: None,
                privileges: DatabasePrivileges {
                    connect: true,
                    ..DatabasePrivileges::default()
                },
                grant_options: DatabasePrivileges::default(),
            },
        ]),
    };
    (roles, security)
}

#[test]
fn database_references_follow_renames_without_rebinding_reused_names_or_oids() {
    let (mut roles, security) = fixture();
    let bound = BoundDatabaseSecurity::bind(&security, &roles).unwrap();
    let original_reader = RoleReference::Bound(Arc::new(
        RoleBinding::from_definition(&roles["reader"]).unwrap(),
    ));
    for name in ["owner", "delegate", "reader"] {
        let mut renamed = roles.get(name).unwrap().clone();
        roles.remove(name).unwrap();
        let oid = renamed.oid;
        let incarnation = renamed.object_id[0] + 10;
        renamed.name = format!("renamed_{name}");
        roles.insert(renamed.name.clone(), renamed).unwrap();
        // Reuse the name with a fresh public OID while the old identity remains live.
        roles
            .insert(name.into(), role(name, oid + 100, incarnation))
            .unwrap();
    }
    let restored = bound.resolve(&roles).unwrap();
    assert_eq!(restored.role_owner, "renamed_owner");
    let acl = restored.acl.as_ref().unwrap();
    assert_eq!(
        (acl[0].role.role_name(), acl[0].grantor.as_deref()),
        (Some("renamed_delegate"), Some("renamed_owner"))
    );
    assert_eq!(
        (acl[1].role.role_name(), acl[1].grantor.as_deref()),
        (Some("renamed_reader"), Some("renamed_delegate"))
    );
    assert_eq!(
        (&acl[2].role, acl[2].grantor.as_deref()),
        (
            &uqa_core::catalog_acl::AclGrantee::Public,
            Some("renamed_owner")
        )
    );
    assert_eq!(
        BoundDatabaseSecurity::bind(&restored, &roles).unwrap(),
        bound
    );
    let check = DatabasePrivilegeCheck {
        privilege: DatabaseAclPrivilege::Create,
        grant_option: false,
    };
    assert!(role_has_privilege(
        &restored,
        &original_reader,
        check,
        &roles,
        &RoleMemberships::new()
    ));
    assert!(!role_has_privilege(
        &restored,
        "reader",
        check,
        &roles,
        &RoleMemberships::new()
    ));

    let original = roles.get("renamed_reader").unwrap().clone();
    roles.remove("renamed_reader").unwrap();
    roles
        .insert(
            "renamed_reader".into(),
            role("renamed_reader", original.oid, 10),
        )
        .unwrap();
    assert!(bound
        .resolve(&roles)
        .unwrap_err()
        .contains("missing role incarnation 20002"));
}

#[test]
fn resolved_grantor_chains_keep_cascade_and_public_privileges_after_rename() {
    let (mut roles, security) = fixture();
    let bound = BoundDatabaseSecurity::bind(&security, &roles).unwrap();
    let mut grantor = roles.get("delegate").unwrap().clone();
    roles.remove("delegate").unwrap();
    grantor.name = "renamed_delegate".into();
    roles.insert(grantor.name.clone(), grantor).unwrap();
    roles
        .insert("delegate".into(), role("delegate", 30_000, 5))
        .unwrap();
    let restored = bound.resolve(&roles).unwrap();
    let statement = GrantDatabaseStmt {
        is_grant: false,
        grant_option: false,
        grant_option_only: false,
        privileges: vec![DatabasePrivilege::Create],
        databases: vec!["uqa".into()],
        grantees: vec!["renamed_delegate".into()],
        grantor: None,
        revoke_behavior: DatabaseRevokeBehavior::Cascade,
    };
    let (revoked, granted) = apply_database_acl(
        &statement,
        &["renamed_delegate".into()],
        &[DatabaseAclPrivilege::Create],
        "owner",
        &roles,
        &RoleMemberships::new(),
        &restored,
    )
    .unwrap();
    assert_eq!(granted, 1);
    assert_eq!(revoked.acl.as_ref().unwrap().len(), 1);
    assert_eq!(
        revoked.acl.as_ref().unwrap()[0].role,
        uqa_core::catalog_acl::AclGrantee::Public
    );
    assert!(role_has_privilege(
        &revoked,
        "reader",
        DatabasePrivilegeCheck {
            privilege: DatabaseAclPrivilege::Connect,
            grant_option: false,
        },
        &roles,
        &RoleMemberships::new(),
    ));
}

#[test]
fn missing_or_replaced_owner_grantee_and_grantor_are_not_resolved_by_name() {
    let (roles, security) = fixture();
    let bound = BoundDatabaseSecurity::bind(&security, &roles).unwrap();
    for name in ["owner", "delegate", "reader"] {
        for replacement in [false, true] {
            let mut changed = roles.clone();
            let old = changed.get(name).unwrap().clone();
            changed.remove(name).unwrap();
            if replacement {
                changed
                    .insert(name.into(), role(name, old.oid, 12))
                    .unwrap();
            }
            assert!(bound
                .resolve(&changed)
                .unwrap_err()
                .contains("missing role incarnation"));
        }
    }
}

#[test]
fn binding_preserves_default_acl_and_rejects_invalid_role_identities() {
    let (mut roles, _) = fixture();
    let security = DatabaseSecurity {
        role_owner: "owner".into(),
        acl: None,
    };
    let bound = BoundDatabaseSecurity::bind(&security, &roles).unwrap();
    assert!(bound.acl.is_none());
    assert_eq!(bound.resolve(&roles).unwrap(), security);
    for oid in [-1, 0, i64::from(u32::MAX) + 1] {
        roles
            .edit("owner", |value| {
                value.oid = oid;
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert!(BoundDatabaseSecurity::bind(&security, &roles).is_err());
        let mut corrupt = bound.clone();
        corrupt.role_owner.oid = oid;
        assert!(corrupt
            .resolve(&roles)
            .unwrap_err()
            .contains("invalid persisted database role identity"));
    }
    roles
        .edit("owner", |value| {
            value.oid = bound.role_owner.oid;
            Ok(())
        })
        .unwrap()
        .unwrap();
    roles
        .edit("owner", |value| {
            value.object_id = [0; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(BoundDatabaseSecurity::bind(&security, &roles).is_err());
    let mut corrupt = bound;
    corrupt.role_owner.object_id = [0; 16];
    assert!(corrupt.resolve(&roles).is_err());
}
