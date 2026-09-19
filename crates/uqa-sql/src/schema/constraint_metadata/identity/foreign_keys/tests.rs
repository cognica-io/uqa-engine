//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::constraint_metadata::materialize_constraint_metadata;

fn declaration() -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE child(a integer CONSTRAINT inline_fk REFERENCES parent(id), b integer, CONSTRAINT table_fk FOREIGN KEY(b) REFERENCES parent(id))").unwrap().remove(0) else { panic!("table declaration") };
    (
        table.columns,
        TableConstraintSet {
            foreign_keys: table.foreign_keys,
            ..Default::default()
        },
    )
}

fn allocator() -> impl FnMut(&str) -> ConstraintMetadataResult<[u8; 16]> {
    let mut next = 0;
    move |_| {
        next += 1;
        Ok([next; 16])
    }
}

#[test]
fn legacy_foreign_key_oids_are_preserved_once_and_follow_their_rows_through_renames() {
    let (mut columns, mut constraints) = declaration();
    let migration = LegacyIdentities::capture(&columns, &constraints);
    let mut allocate = allocator();
    let relation = RelationIdentity::new("public", "child");
    materialize_constraint_metadata(&relation, &mut columns, &mut constraints, &mut allocate)
        .unwrap();
    assert!(migration
        .preserve_oids(&relation, &mut columns, &mut constraints)
        .unwrap());
    let before: Vec<_> = identities(&columns, &constraints).flatten().collect();
    for (identity, name) in before.iter().zip(["inline_fk", "table_fk"]) {
        assert_eq!(
            identity.oid,
            crate::catalog::oids::stable_oid("constraint", &format!("public.child.{name}"))
        );
    }
    assert_ne!(before[0].object_id, before[1].object_id);
    columns[0].references.as_mut().unwrap().name = Some("renamed_inline".into());
    constraints.foreign_keys[0].name = Some("renamed_table".into());
    let relation = RelationIdentity::new("moved", "renamed");
    let migration = LegacyIdentities::capture(&columns, &constraints);
    assert!(!materialize_constraint_metadata(
        &relation,
        &mut columns,
        &mut constraints,
        &mut allocate
    )
    .unwrap());
    assert!(!migration
        .preserve_oids(&relation, &mut columns, &mut constraints)
        .unwrap());
    assert_eq!(
        identities(&columns, &constraints)
            .flatten()
            .collect::<Vec<_>>(),
        before
    );
}

#[test]
fn partition_copies_share_enforcement_but_receive_distinct_catalog_identities() {
    let (mut columns, mut parent) = declaration();
    let mut allocate = allocator();
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "parent"),
        &mut columns,
        &mut parent,
        &mut allocate,
    )
    .unwrap();
    let mut child = TableConstraintSet::default();
    child.hierarchy.partition_inherited_foreign_keys =
        crate::schema::inheritance::alter::append_inherited_foreign_keys(
            &mut child.foreign_keys,
            &parent.foreign_keys,
        );
    assert!(child.foreign_keys[0].catalog_identity.is_none());
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "child"),
        &mut [],
        &mut child,
        &mut allocate,
    )
    .unwrap();
    assert_eq!(
        child.foreign_keys[0].object_id,
        parent.foreign_keys[0].object_id
    );
    assert_ne!(
        child.foreign_keys[0].catalog_identity,
        parent.foreign_keys[0].catalog_identity
    );
    assert_eq!(
        child.foreign_keys,
        child.hierarchy.partition_inherited_foreign_keys
    );
    child.foreign_keys[0].name = Some("renamed".into());
    child.foreign_keys[0].validated = false;
    validate(&[], &child).unwrap();
    let object_id = child.foreign_keys[0].object_id.unwrap();
    crate::schema::constraint_changes::foreign_key_target::remove_foreign_key(
        &mut [],
        &mut child,
        object_id,
    )
    .unwrap();
    assert!(child.foreign_keys.is_empty());
}

#[test]
fn current_foreign_key_catalog_identities_reject_missing_invalid_and_duplicate_rows() {
    let (mut columns, mut constraints) = declaration();
    assert!(validate(&columns, &constraints)
        .unwrap_err()
        .to_string()
        .contains("initial catalog identity migration"));
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "child"),
        &mut columns,
        &mut constraints,
        &mut allocator(),
    )
    .unwrap();
    let reference = columns[0]
        .references
        .as_ref()
        .unwrap()
        .catalog_identity
        .unwrap();
    for invalid in [
        None,
        Some(ConstraintCatalogIdentity {
            object_id: [0; 16],
            oid: 22_000,
        }),
        Some(ConstraintCatalogIdentity {
            object_id: [90; 16],
            oid: 0,
        }),
        Some(ConstraintCatalogIdentity {
            object_id: [90; 16],
            oid: i64::from(u32::MAX) + 1,
        }),
        Some(reference),
    ] {
        let mut invalid_constraints = constraints.clone();
        invalid_constraints.foreign_keys[0].catalog_identity = invalid;
        assert!(validate(&columns, &invalid_constraints).is_err());
    }
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    constraints.hierarchy.partition_inherited_foreign_keys[0].catalog_identity = Some(reference);
    assert!(validate(&columns, &constraints)
        .unwrap_err()
        .to_string()
        .contains("provenance"));
}
