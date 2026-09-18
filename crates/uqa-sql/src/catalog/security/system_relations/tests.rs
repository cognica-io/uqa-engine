//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    roles::RoleDefinition,
    security::{
        columns::grant_column_acl,
        table::{grant_acl, TableAclPrivilege, TablePrivilegeCheck},
    },
    VirtualRelation,
};

#[test]
fn system_acl_masks_relation_writes_but_preserves_attribute_grants_and_grant_options() {
    let relation = SystemRelation::Projected(VirtualRelation::PgClass);
    let mut security = relation.bootstrap_security();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 20_001;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    let roles = BTreeMap::from([
        ("uqa".into(), RoleDefinition::bootstrap()),
        ("reader".into(), reader),
    ]);
    let memberships = BTreeMap::new();
    for privilege in TableAclPrivilege::ALL {
        grant_acl(&mut security, privilege, &["reader".into()], "uqa", true);
        let check = TablePrivilegeCheck {
            privilege,
            grant_option: false,
        };
        let expected = !matches!(
            privilege,
            TableAclPrivilege::Insert
                | TableAclPrivilege::Update
                | TableAclPrivilege::Delete
                | TableAclPrivilege::Truncate
        );
        assert_eq!(
            has_table_privilege(relation, &security, "reader", check, &roles, &memberships),
            expected,
            "{privilege:?}"
        );
        assert!(has_table_privilege(
            relation,
            &security,
            "reader",
            TablePrivilegeCheck {
                grant_option: true,
                ..check
            },
            &roles,
            &memberships
        ));
        assert!(has_table_privilege(
            relation,
            &security,
            "uqa",
            check,
            &roles,
            &memberships
        ));
    }
    grant_column_acl(
        &mut security,
        "relname",
        TableAclPrivilege::Update,
        &["reader".into()],
        "uqa",
        false,
    );
    let check = TablePrivilegeCheck {
        privilege: TableAclPrivilege::Update,
        grant_option: false,
    };
    assert!(has_column_privilege(
        relation,
        &security,
        "relname",
        "reader",
        check,
        &roles,
        &memberships
    ));
    assert!(!has_column_privilege(
        relation,
        &security,
        "relnamespace",
        "reader",
        check,
        &roles,
        &memberships
    ));
    let settings = SystemRelation::Projected(VirtualRelation::PgSettings);
    assert!(has_table_privilege(
        settings,
        &settings.bootstrap_security(),
        "reader",
        check,
        &roles,
        &memberships
    ));
}

#[test]
fn system_acl_validation_rejects_orphan_roles_wrong_owners_and_missing_columns() {
    let relation = SystemRelation::PgAuthid;
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let baseline = relation.bootstrap_security();
    validate_security(relation, &baseline, &roles).unwrap();
    let mut invalid = baseline.clone();
    invalid.role_owner = "missing".into();
    assert!(validate_security(relation, &invalid, &roles).is_err());
    let mut invalid = baseline.clone();
    grant_acl(
        &mut invalid,
        TableAclPrivilege::Select,
        &["missing".into()],
        "uqa",
        false,
    );
    assert!(validate_security(relation, &invalid, &roles).is_err());
    let mut invalid = baseline;
    grant_column_acl(
        &mut invalid,
        "absent",
        TableAclPrivilege::Select,
        &["PUBLIC".into()],
        "uqa",
        false,
    );
    assert!(validate_security(relation, &invalid, &roles).is_err());
}
