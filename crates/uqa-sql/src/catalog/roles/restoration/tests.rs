//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}
fn membership(oid: i64) -> RoleMembership {
    RoleMembership {
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
    let error = restore_role_memberships(&roles(), vec![first.clone(), second]).unwrap_err();
    assert_eq!(
        error,
        "persisted role membership `uqa` -> `uqa` has a missing role or grantor"
    );
    assert_eq!(
        restore_role_memberships(&roles(), vec![first.clone(), first]).unwrap_err(),
        "persisted role membership OID 31 is duplicated"
    );
}

#[test]
fn distinct_membership_oids_do_not_allow_duplicate_grant_identities() {
    assert_eq!(
        restore_role_memberships(&roles(), vec![membership(31), membership(32)]).unwrap_err(),
        "persisted role membership identity is duplicated"
    );
    let restored = restore_role_memberships(&roles(), vec![membership(31)]).unwrap();
    assert_eq!(restored.values().next(), Some(&membership(31)));
}
