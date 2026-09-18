//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

#[test]
fn role_incarnations_reject_missing_duplicate_and_replaced_bootstrap_identities() {
    let mut roles = roles();
    let mut stored = RoleDefinition::bootstrap();
    stored.name = "stored".into();
    stored.oid = 20_001;
    roles.insert(stored.name.clone(), stored);
    assert_eq!(
        validate_role_identities(&roles).unwrap_err(),
        "persisted role object identity is duplicated"
    );
    roles.get_mut("stored").unwrap().object_id = [0; 16];
    assert_eq!(
        validate_role_identities(&roles).unwrap_err(),
        "persisted role `stored` has no object identity"
    );
    roles.get_mut("stored").unwrap().object_id = [1; 16];
    validate_role_identities(&roles).unwrap();
    roles.get_mut("uqa").unwrap().object_id = [2; 16];
    assert_eq!(
        validate_role_identities(&roles).unwrap_err(),
        "persisted bootstrap role has an invalid object identity"
    );
}
fn membership(oid: i64) -> NamedRoleMembership {
    NamedRoleMembership {
        oid,
        role: "uqa".into(),
        member: "uqa".into(),
        grantor: "uqa".into(),
        admin_option: false,
        inherit_option: true,
        set_option: true,
    }
}

#[test]
fn restored_role_keys_are_validated_after_bootstrap_insertion() {
    let mut catalog = BTreeMap::from([("wrong".into(), RoleDefinition::bootstrap())]);
    assert_eq!(
        restore_role_definitions(&mut catalog).unwrap_err(),
        "persisted role key `wrong` does not match role name `uqa`"
    );
    assert_eq!(catalog["uqa"], RoleDefinition::bootstrap());
}

#[test]
fn restored_membership_role_references_are_checked_before_duplicate_oids() {
    let first = membership(31);
    let mut second = membership(31);
    second.grantor = "missing".into();
    let error = restore_named_role_memberships(&roles(), vec![first.clone(), second]).unwrap_err();
    assert_eq!(
        error,
        "persisted role membership `uqa` -> `uqa` has a missing role or grantor"
    );
    assert_eq!(
        restore_named_role_memberships(&roles(), vec![first.clone(), first]).unwrap_err(),
        "persisted role membership OID 31 is duplicated"
    );
}

#[test]
fn distinct_membership_oids_do_not_allow_duplicate_grant_identities() {
    assert_eq!(
        restore_named_role_memberships(&roles(), vec![membership(31), membership(32)]).unwrap_err(),
        "persisted role membership identity is duplicated"
    );
    let restored = restore_named_role_memberships(&roles(), vec![membership(31)]).unwrap();
    assert_eq!(
        restored.values().next(),
        Some(&membership(31).bind(&roles()).unwrap())
    );
}

#[test]
fn restored_role_oids_preserve_stored_identity_and_reject_invalid_or_duplicate_values() {
    let mut stored = RoleDefinition::bootstrap();
    stored.name = "stored".into();
    stored.oid = 4_000_000_001;
    let mut catalog = roles();
    catalog.insert(stored.name.clone(), stored.clone());
    restore_role_definitions(&mut catalog).unwrap();
    assert_eq!(catalog["stored"], stored);
    for oid in [0, -1, i64::from(u32::MAX) + 1] {
        catalog.get_mut("stored").unwrap().oid = oid;
        assert_eq!(
            restore_role_definitions(&mut catalog).unwrap_err(),
            format!("persisted role `stored` has an invalid OID {oid}")
        );
    }
    catalog.get_mut("stored").unwrap().oid = 10;
    assert_eq!(
        restore_role_definitions(&mut catalog).unwrap_err(),
        "persisted role OID 10 is duplicated"
    );
    catalog.remove("stored");
    catalog.get_mut("uqa").unwrap().oid = 16_384;
    assert_eq!(
        restore_role_definitions(&mut catalog).unwrap_err(),
        "persisted bootstrap role must have OID 10"
    );
}

fn bound_membership() -> RoleMembership {
    let mut membership = membership(31).bind(&roles()).unwrap();
    membership.role = RoleBinding {
        name: "target".into(),
        oid: 20_001,
        object_id: [1; 16],
    };
    membership.member = RoleBinding {
        name: "member".into(),
        oid: 20_002,
        object_id: [2; 16],
    };
    membership
}

#[test]
fn membership_restoration_keeps_deleted_endpoint_identities_through_oid_and_name_reuse() {
    let membership = bound_membership();
    let mut catalog = roles();
    for endpoint in [&membership.role, &membership.member] {
        let mut replacement = RoleDefinition::bootstrap();
        replacement.name = endpoint.name.clone();
        replacement.oid = i64::from(endpoint.oid);
        replacement.object_id = [endpoint.object_id[0] + 2; 16];
        catalog.insert(replacement.name.clone(), replacement);
    }
    for catalog in [roles(), catalog] {
        let restored = restore_role_memberships(&catalog, vec![membership.clone()]).unwrap();
        assert_eq!(restored[&membership.key()], membership);
    }
}

#[test]
fn membership_restoration_rejects_missing_grantor_and_malformed_retained_identities() {
    for corruption in 0..7 {
        let mut membership = bound_membership();
        match corruption {
            0 => membership.grantor.object_id = [3; 16],
            1 => membership.grantor = membership.member.clone(),
            2 => membership.role.oid = 0,
            3 => membership.member.object_id = [0; 16],
            4 => membership.role.object_id = membership.member.object_id,
            5 => membership.role.oid = 10,
            6 => membership.role.object_id = RoleDefinition::bootstrap().object_id,
            _ => unreachable!(),
        }
        assert!(
            restore_role_memberships(&roles(), vec![membership]).is_err(),
            "corruption {corruption}"
        );
    }
}

#[test]
fn membership_restoration_resolves_a_renamed_grantor_by_captured_identity() {
    let mut catalog = roles();
    let mut grantor = RoleDefinition::bootstrap();
    grantor.name = "renamed".into();
    grantor.oid = 20_003;
    grantor.object_id = [3; 16];
    let mut membership = bound_membership();
    membership.grantor = RoleBinding::from_definition(&grantor).unwrap();
    membership.grantor.name = "old_name".into();
    catalog.insert(grantor.name.clone(), grantor);
    assert_eq!(
        restore_role_memberships(&catalog, vec![membership.clone()]).unwrap()[&membership.key()],
        membership
    );
}
