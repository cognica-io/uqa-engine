//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{PartitionBound, PartitionIdentityOverride};

fn declaration() -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE child(v int GENERATED ALWAYS AS IDENTITY, CONSTRAINT uk UNIQUE(v), CONSTRAINT fk FOREIGN KEY(v) REFERENCES referenced(v))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        key_constraints: table.key_constraints,
        foreign_keys: table.foreign_keys,
        ..Default::default()
    };
    let mut next = 0;
    crate::schema::constraint_metadata::materialize_constraint_metadata(
        &RelationIdentity::new("public", "child"),
        &mut columns,
        &mut constraints,
        &mut |_: &str| {
            next += 1;
            Ok([next; 16])
        },
    )
    .unwrap();
    (columns, constraints)
}

#[test]
fn lost_partition_parent_retains_constraints_and_catalog_addresses_and_restores_identity() {
    let (mut columns, mut constraints) = declaration();
    constraints.hierarchy.parents = vec!["vanished".into()];
    constraints.hierarchy.partition_bound = Some(PartitionBound::Default);
    constraints.hierarchy.partition_inherited_key_constraints = constraints.key_constraints.clone();
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    constraints
        .hierarchy
        .partition_identity_overrides
        .push(PartitionIdentityOverride {
            column: "v".into(),
            original: None,
        });
    let keys = constraints.key_constraints.clone();
    let foreign_keys = constraints.foreign_keys.clone();
    let identities =
        crate::schema::constraint_metadata::identity::claims::identities(&columns, &constraints);
    let repaired = repair_parent_edges(&mut columns, &mut constraints, &BTreeSet::new()).unwrap();
    assert!(repaired.detached_partition);
    assert_eq!(repaired.inherited_identity.len(), 1);
    assert_eq!(constraints.key_constraints, keys);
    assert_eq!(constraints.foreign_keys, foreign_keys);
    assert_eq!(
        crate::schema::constraint_metadata::identity::claims::identities(&columns, &constraints),
        identities
    );
    assert!(columns[0].auto_increment.is_none());
    assert!(!constraints.hierarchy.is_partition());
    assert_eq!(constraints.hierarchy.local_columns, ["v"]);
    assert!(constraints
        .hierarchy
        .partition_inherited_key_constraints
        .is_empty());
    assert!(constraints
        .hierarchy
        .partition_inherited_foreign_keys
        .is_empty());
    assert!(
        !repair_parent_edges(&mut columns, &mut constraints, &BTreeSet::new())
            .unwrap()
            .changed
    );
}

#[test]
fn surviving_parent_order_and_sequence_numbers_are_preserved_when_legacy_names_are_normalized() {
    let (mut columns, mut constraints) = declaration();
    constraints.hierarchy.parents = vec!["first".into(), "missing".into(), "public.last".into()];
    constraints.hierarchy.parent_sequence_numbers = vec![2, 5, 9];
    let existing = BTreeSet::from(["public.first".into(), "public.last".into()]);
    let repaired = repair_parent_edges(&mut columns, &mut constraints, &existing).unwrap();
    assert!(repaired.changed);
    assert!(!repaired.detached_partition);
    assert_eq!(
        constraints.hierarchy.parents,
        ["public.first", "public.last"]
    );
    assert_eq!(constraints.hierarchy.parent_sequence_numbers, [2, 9]);
    assert!(columns[0].auto_increment.is_some());
    assert!(
        !repair_parent_edges(&mut columns, &mut constraints, &existing)
            .unwrap()
            .changed
    );
}
