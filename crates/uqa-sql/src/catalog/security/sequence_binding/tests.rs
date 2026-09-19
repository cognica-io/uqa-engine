//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::sequence::{
    grant_acl, role_has_privilege, AclPrivilege, PrivilegeCheck,
};

fn roles() -> BTreeMap<String, RoleDefinition> {
    ["owner", "grantor", "reader", "PUBLIC"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let mut role = RoleDefinition::bootstrap();
            role.name = name.into();
            role.oid = 20_001 + index as i64;
            role.object_id = [index as u8 + 1; 16];
            role.attributes.clear();
            (name.into(), role)
        })
        .collect()
}

fn security(roles: &BTreeMap<String, RoleDefinition>) -> BoundSequenceSecurity {
    let mut named = SequenceSecurity {
        role_owner: "owner".into(),
        acl: Some(Vec::new()),
    };
    grant_acl(
        &mut named,
        AclPrivilege::Usage,
        &["grantor".into()],
        "owner",
        true,
    );
    grant_acl(
        &mut named,
        AclPrivilege::Usage,
        &["reader".into()],
        "grantor",
        false,
    );
    BoundSequenceSecurity::bind(&named, roles).unwrap()
}

#[test]
fn sequence_endpoints_keep_incarnations_when_all_names_are_reused() {
    let mut roles = roles();
    let bound = security(&roles);
    let original = bound.clone();
    let owner = bound.owner_reference(&roles).unwrap();
    for name in ["owner", "grantor", "reader"] {
        let mut role = roles.remove(name).unwrap();
        let mut replacement = role.clone();
        replacement.oid += 100;
        replacement.object_id[0] += 100;
        roles.insert(name.into(), replacement);
        role.name = format!("renamed_{name}");
        roles.insert(role.name.clone(), role);
    }
    bound.validate(&roles).unwrap();
    let named = bound.resolve(&roles).unwrap();
    assert_eq!(named.role_owner, "renamed_owner");
    assert_eq!(owner.require_name(&roles).unwrap(), "renamed_owner");
    assert!(named
        .acl
        .as_ref()
        .unwrap()
        .iter()
        .any(|entry| entry.grantor.as_deref() == Some("renamed_grantor")));
    for (name, expected) in [
        ("renamed_reader", true),
        ("reader", false),
        ("owner", false),
    ] {
        assert_eq!(
            role_has_privilege(
                &named,
                name,
                PrivilegeCheck {
                    privilege: AclPrivilege::Usage,
                    grant_option: false
                },
                &roles,
                &BTreeMap::new()
            ),
            expected
        );
    }
    assert!(bound.depends_on(roles["renamed_reader"].identity()));
    assert!(!bound.depends_on(roles["reader"].identity()));
    assert_eq!(bound, original);
}

#[test]
fn missing_or_reused_oid_cannot_rebind_any_sequence_endpoint() {
    for name in ["owner", "grantor", "reader"] {
        let mut roles = roles();
        let bound = security(&roles);
        let mut replacement = roles.remove(name).unwrap();
        assert!(bound.resolve(&roles).is_err());
        replacement.object_id[0] += 100;
        roles.insert(name.into(), replacement);
        assert!(bound.validate(&roles).is_err());
    }
}

#[test]
fn sequence_acl_validation_rejects_unrooted_cycles_and_invalid_paths() {
    let roles = roles();
    let valid = security(&roles);
    valid.validate(&roles).unwrap();
    for damage in [
        "cycle",
        "empty",
        "duplicate",
        "public option",
        "missing privilege",
    ] {
        let mut bound = valid.clone();
        let acl = bound.acl.as_mut().unwrap();
        match damage {
            "cycle" => {
                // Remove the owner's grant while keeping a mutually supporting grant-option cycle.
                acl.retain(|entry| entry.role != Some(roles["owner"].identity()));
                acl[0].grantor = roles["reader"].identity();
                acl[1].grant_options.usage = true;
            }
            "empty" => {
                acl[1].privileges = SequencePrivileges::default();
            }
            "duplicate" => {
                acl.push(acl[0].clone());
            }
            "public option" => {
                acl[0].role = None;
            }
            _ => {
                acl[0].privileges.usage = false;
            }
        }
        assert!(bound.validate(&roles).is_err(), "{damage}");
    }
    let mut empty = valid;
    empty.acl = Some(Vec::new());
    empty.validate(&roles).unwrap();
    assert!(!role_has_privilege(
        &empty.resolve(&roles).unwrap(),
        "owner",
        PrivilegeCheck {
            privilege: AclPrivilege::Usage,
            grant_option: false
        },
        &roles,
        &BTreeMap::new()
    ));
    assert!(role_has_privilege(
        &empty.resolve(&roles).unwrap(),
        "owner",
        PrivilegeCheck {
            privilege: AclPrivilege::Usage,
            grant_option: true
        },
        &roles,
        &BTreeMap::new()
    ));
}

#[test]
fn sequence_public_and_named_public_keep_distinct_grants_and_grantors() {
    let mut roles = roles();
    let mut named = SequenceSecurity {
        role_owner: "owner".into(),
        acl: None,
    };
    grant_acl(
        &mut named,
        AclPrivilege::Select,
        &["PUBLIC".into()],
        "owner",
        true,
    );
    grant_acl(
        &mut named,
        AclPrivilege::Select,
        &[AclGrantee::Public],
        "owner",
        false,
    );
    grant_acl(
        &mut named,
        AclPrivilege::Select,
        &["reader".into()],
        "PUBLIC",
        false,
    );
    let bound = BoundSequenceSecurity::bind(&named, &roles).unwrap();
    bound.validate(&roles).unwrap();
    let mut role = roles.remove("PUBLIC").unwrap();
    role.name = "renamed_public".into();
    roles.insert(role.name.clone(), role);
    let named = bound.resolve(&roles).unwrap();
    let acl = named.acl.as_ref().unwrap();
    assert!(acl.iter().any(|entry| entry.role == AclGrantee::Public));
    assert!(acl.iter().any(
        |entry| entry.role.role_name() == Some("renamed_public") && entry.grant_options.select
    ));
    assert!(acl
        .iter()
        .any(|entry| entry.grantor.as_deref() == Some("renamed_public")));
    bound.validate(&roles).unwrap();
    assert_eq!(BoundSequenceSecurity::bind(&named, &roles).unwrap(), bound);
}
