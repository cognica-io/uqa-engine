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
    entries.push(TableAclEntry {
        role: uqa_core::catalog_acl::AclGrantee::Public,
        ..acl("unused", "owner")
    });
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

#[test]
fn named_public_grantees_and_grantors_are_role_dependencies() {
    for entry in [acl("PUBLIC", "owner"), acl("owner", "PUBLIC")] {
        let mut added = BTreeSet::new();
        added_acl_roles(&[], "owner", &[entry], "owner", &mut added);
        assert_eq!(added, BTreeSet::from(["PUBLIC".into()]));
    }
}

#[test]
fn database_schema_and_sequence_acls_preserve_both_role_references() {
    fn verify<T: AclRoleReferences>(entry: T) {
        let entries = [entry];
        let mut added = BTreeSet::new();
        added_acl_roles(&[], "owner", &entries, "owner", &mut added);
        assert_eq!(added, BTreeSet::from(["reader".into(), "grantor".into()]));
        added.clear();
        added_acl_roles(&entries, "owner", &entries, "owner", &mut added);
        assert!(added.is_empty());
        added_acl_roles(&entries, "owner", &[], "owner", &mut added);
        assert!(added.is_empty());
    }
    verify(DatabaseAclEntry {
        role: "reader".into(),
        grantor: Some("grantor".into()),
        privileges: super::super::database::DatabasePrivileges {
            create: true,
            ..super::super::database::DatabasePrivileges::default()
        },
        grant_options: super::super::database::DatabasePrivileges::default(),
    });
    verify(SchemaAclEntry {
        role: "reader".into(),
        grantor: Some("grantor".into()),
        privileges: uqa_core::catalog_schema::SchemaPrivileges {
            usage: true,
            create: false,
        },
        grant_options: uqa_core::catalog_schema::SchemaPrivileges::default(),
    });
    verify(SequenceAclEntry {
        role: "reader".into(),
        grantor: Some("grantor".into()),
        privileges: uqa_core::catalog_sequence::SequencePrivileges {
            usage: true,
            ..uqa_core::catalog_sequence::SequencePrivileges::default()
        },
        grant_options: uqa_core::catalog_sequence::SequencePrivileges::default(),
    });
}
