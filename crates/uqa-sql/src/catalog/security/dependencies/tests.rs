//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::security::TablePrivileges;

fn acl(role: &str, grantor: &str) -> TableAclEntry {
    TableAclEntry {
        role: role.into(),
        grantor: Some(grantor.into()),
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges::default(),
    }
}

#[test]
fn added_acl_dependencies_keep_relation_and_column_objects_separate() {
    let mut before = TableSecurity::owner("owner");
    before.acl = Some(vec![acl("reader", "owner")]);
    before
        .column_acls
        .insert("first".into(), vec![acl("column_reader", "owner")]);
    let mut after = before.clone();
    after.column_acls.insert(
        "second".into(),
        vec![acl("reader", "grantor"), acl("column_reader", "owner")],
    );
    let mut added = BTreeSet::new();
    added_table_acl_roles(&before, &after, &mut added);
    assert_eq!(
        added,
        BTreeSet::from(["reader".into(), "grantor".into(), "column_reader".into()])
    );
    added.clear();
    added_table_acl_roles(&after, &before, &mut added);
    assert!(
        added.is_empty(),
        "removing dependencies does not acquire new role locks"
    );
}

#[test]
fn existing_grantee_or_grantor_and_public_or_owner_do_not_add_dependencies() {
    let before = TableSecurity {
        role_owner: "owner".into(),
        acl: Some(vec![acl("reader", "grantor")]),
        column_acls: std::collections::BTreeMap::new(),
    };
    let mut after = before.clone();
    let entries = after.acl.as_mut().unwrap();
    entries[0].privileges.update = true;
    entries.push(acl("grantor", "reader"));
    entries.push(acl("PUBLIC", "owner"));
    entries.push(acl("owner", "owner"));
    let mut added = BTreeSet::new();
    added_table_acl_roles(&before, &after, &mut added);
    assert!(added.is_empty());
    after
        .acl
        .as_mut()
        .unwrap()
        .push(acl("reader", "new_grantor"));
    added_table_acl_roles(&before, &after, &mut added);
    assert_eq!(added, BTreeSet::from(["new_grantor".into()]));
}
