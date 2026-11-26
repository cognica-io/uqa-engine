//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{RoleSpecification, Statement};

#[test]
fn drop_owned_compiles_role_specifications_and_drop_behavior() {
    for (suffix, cascade) in [("", false), (" RESTRICT", false), (" CASCADE", true)] {
        let sql =
            format!("DROP OWNED BY CURRENT_USER, \"CURRENT_USER\", SESSION_USER, reader{suffix}");
        let Statement::DropOwned(owned) = crate::compile(&sql).unwrap().remove(0) else {
            panic!("expected DROP OWNED")
        };
        assert_eq!(
            owned.roles,
            vec![
                RoleSpecification::CurrentUser,
                RoleSpecification::Named("CURRENT_USER".into()),
                RoleSpecification::SessionUser,
                RoleSpecification::Named("reader".into())
            ]
        );
        assert_eq!(owned.cascade, cascade);
    }
}

#[test]
fn removing_a_grantor_preserves_independent_column_grants() {
    let mut security = TableSecurity::owner("owner");
    let a = AclGrantee::Role("a".into());
    let b = AclGrantee::Role("b".into());
    let select = table::TableAclPrivilege::Select;
    table::grant_acl(&mut security, select, &[a], "owner", true);
    columns::grant_column_acl(
        &mut security,
        "v",
        select,
        std::slice::from_ref(&b),
        "a",
        false,
    );
    columns::grant_column_acl(&mut security, "v", select, &[b], "owner", false);
    let cleaned = relation(&security, &BTreeSet::from(["a".into()])).unwrap();
    let table_acl = cleaned.acl.as_deref().unwrap();
    assert_eq!(table_acl.len(), 1);
    assert_eq!(table_acl[0].role, AclGrantee::from("owner".to_owned()));
    let column_acl = cleaned.column_acls.get("v").unwrap();
    assert_eq!(column_acl.len(), 1);
    assert_eq!(column_acl[0].role, AclGrantee::from("b".to_owned()));
    assert_eq!(column_acl[0].grantor.as_deref(), Some("owner"));
    assert_eq!(relation(&cleaned, &BTreeSet::from(["a".into()])).unwrap(), cleaned);
}
