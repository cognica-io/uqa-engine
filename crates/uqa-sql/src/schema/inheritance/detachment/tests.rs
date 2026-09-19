//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn detached_foreign_keys_keep_catalog_rows_and_child_provenance_with_a_new_family() {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE c(v int CONSTRAINT fk REFERENCES p(v), CONSTRAINT fk2 FOREIGN KEY(v) REFERENCES p(v))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        foreign_keys: table.foreign_keys,
        ..Default::default()
    };
    let mut next = 0;
    crate::schema::constraint_metadata::materialize_constraint_metadata(
        &uqa_core::RelationIdentity::new("public", "c"),
        &mut columns,
        &mut constraints,
        &mut |_: &str| {
            next += 1;
            Ok([next; 16])
        },
    )
    .unwrap();
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    let identities =
        crate::schema::constraint_metadata::identity::claims::identities(&columns, &constraints);
    let old_inline = columns[0].references.as_ref().unwrap().object_id.unwrap();
    let old_table = constraints.foreign_keys[0].object_id.unwrap();
    let changes = split_foreign_key_families(
        "c",
        &mut columns,
        &mut constraints,
        &BTreeMap::from([(old_inline, [91; 16]), (old_table, [92; 16])]),
    )
    .unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(
        columns[0].references.as_ref().unwrap().object_id,
        Some([91; 16])
    );
    assert_eq!(constraints.foreign_keys[0].object_id, Some([92; 16]));
    assert_eq!(
        constraints.foreign_keys,
        constraints.hierarchy.partition_inherited_foreign_keys
    );
    assert_eq!(
        crate::schema::constraint_metadata::identity::claims::identities(&columns, &constraints),
        identities
    );
}

#[test]
fn splitting_a_family_preserves_explicit_modes_on_both_sides() {
    let parent = ConstraintIdentity {
        relation: uqa_core::RelationIdentity::new("public", "p"),
        name: "parent_fk".into(),
        object_id: Some([1; 16]),
    };
    let child = ConstraintIdentity {
        relation: uqa_core::RelationIdentity::new("public", "c"),
        name: "child_fk".into(),
        ..parent.clone()
    };
    let detached = ConstraintIdentity {
        object_id: Some([2; 16]),
        ..child.clone()
    };
    for originally_named in [&parent, &child] {
        let mut named = BTreeMap::from([(originally_named.clone(), true)]);
        preserve_split_constraint_modes(
            &mut named,
            std::slice::from_ref(&parent),
            &[(child.clone(), detached.clone())],
        );
        assert_eq!(named.get(&parent), Some(&true));
        assert_eq!(named.get(&detached), Some(&true));
    }
}
