//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnDef, TableConstraintSet};
use crate::schema::constraint_metadata::{identity, materialize_constraint_metadata};
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationIdentity};

mod continuation;

fn declaration() -> (Vec<ColumnDef>, TableConstraintSet) {
    let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE child(a integer CONSTRAINT required NOT NULL CONSTRAINT positive CHECK(a > 0) CONSTRAINT inline_fk REFERENCES parent(id), b integer, CONSTRAINT table_fk FOREIGN KEY(b) REFERENCES parent(id), CONSTRAINT unique_a UNIQUE(a), CONSTRAINT positive_b CHECK(b > 0))").unwrap().remove(0) else { panic!("table declaration") };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        checks: table.checks,
        foreign_keys: table.foreign_keys,
        key_constraints: table.key_constraints,
        ..Default::default()
    };
    let mut next = 0_u8;
    materialize_constraint_metadata(
        &RelationIdentity::new("public", "child"),
        &mut columns,
        &mut constraints,
        &mut |_: &str| {
            next += 1;
            Ok([next; 16])
        },
    )
    .unwrap();
    constraints.hierarchy.partition_inherited_key_constraints = constraints.key_constraints.clone();
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    (columns, constraints)
}

fn validate(
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    control: &ProductionControl<'_>,
) -> ConstraintMetadataResult<()> {
    identity::claims::validate_constraint_identities_with_control(columns, constraints, control)
}

fn pair_bytes() -> usize {
    OwnedSet::<[u8; 16]>::entry_bytes() + OwnedSet::<i64>::entry_bytes()
}

#[test]
fn controlled_constraint_identity_validation_borrows_complete_native_syntax() {
    let (mut columns, mut constraints) = declaration();
    columns[0].name = "large column".repeat(100_000);
    constraints.checks[0].name = Some("large check".repeat(100_000));
    let before = serde_json::to_vec(&(&columns, &constraints)).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(6 * pair_bytes());
    let control = ProductionControl::new(&memory, &original, &invoking);
    let allocations =
        allocation_counter::measure(|| validate(&columns, &constraints, &control).unwrap());
    assert_eq!(memory.used(), 0);
    assert_eq!(memory.peak(), 6 * pair_bytes());
    // Each stage owns only its temporary address nodes; the large declaration remains borrowed.
    assert_eq!(
        usize::try_from(allocations.bytes_total).unwrap(),
        9 * pair_bytes() + OwnedSet::<[u8; 16]>::entry_bytes()
    );
    assert_eq!(allocations.bytes_current, 0);
    assert_eq!(
        serde_json::to_vec(&(&columns, &constraints)).unwrap(),
        before
    );
}

#[test]
fn constraint_identity_node_admission_retires_partial_validation() {
    let (columns, constraints) = declaration();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for count in 1..=6 {
        for limit in [
            (count - 1) * pair_bytes() + OwnedSet::<[u8; 16]>::entry_bytes() - 1,
            count * pair_bytes() - 1,
        ] {
            let memory = MemoryBudget::new(limit);
            let error = validate(
                &columns,
                &constraints,
                &ProductionControl::new(&memory, &original, &invoking),
            )
            .unwrap_err();
            assert_eq!(error.into_sql_error().sqlstate(), Some("53200"));
            assert_eq!(memory.used(), 0);
            assert!(memory.peak() <= limit);
        }
    }
}

type Mutation = fn(&mut Vec<ColumnDef>, &mut TableConstraintSet);
const INVALID_CASES: &[(Mutation, &str)] = &[
    (
        |c, _| c[0].not_null_identity = None,
        "NOT NULL constraint requires an initial catalog identity migration",
    ),
    (
        |c, _| c[0].not_null_identity.as_mut().unwrap().oid = 0,
        "invalid NOT NULL constraint catalog identity",
    ),
    (
        |c, _| c[0].not_null = false,
        "nullable column retains a NOT NULL constraint identity",
    ),
    (
        |c, _| c.push(c[0].clone()),
        "duplicate NOT NULL constraint catalog identity",
    ),
    (
        |c, _| c[0].references.as_mut().unwrap().catalog_identity = None,
        "foreign keys require an initial catalog identity migration",
    ),
    (
        |c, _| {
            c[0].references
                .as_mut()
                .unwrap()
                .catalog_identity
                .as_mut()
                .unwrap()
                .oid = 0;
        },
        "invalid foreign-key catalog identity",
    ),
    (
        |c, t| {
            t.foreign_keys[0].catalog_identity = c[0].references.as_ref().unwrap().catalog_identity;
        },
        "duplicate foreign-key catalog identity",
    ),
    (
        |_, t| t.hierarchy.partition_inherited_foreign_keys[0].catalog_identity = None,
        "partition foreign-key provenance does not identify its catalog row",
    ),
    (
        |c, _| c[0].check = None,
        "column without CHECK retains a constraint identity",
    ),
    (
        |c, _| c[0].check_object_id = None,
        "CHECK catalog address has no constraint incarnation",
    ),
    (
        |c, t| t.checks[0].catalog_oid = c[0].not_null_identity.map(|identity| identity.oid),
        "duplicate key or CHECK constraint catalog identity",
    ),
    (
        |_, t| t.key_constraints[0].catalog_identity = None,
        "key constraints require an initial catalog identity migration",
    ),
    (
        |c, _| c[0].check_catalog_oid = None,
        "CHECK constraints require an initial catalog identity migration",
    ),
    (
        |_, t| {
            t.hierarchy
                .partition_inherited_key_constraints
                .push(t.hierarchy.partition_inherited_key_constraints[0].clone());
        },
        "partition key provenance does not identify one local constraint",
    ),
];

#[test]
fn controlled_constraint_identity_validation_preserves_original_diagnostics() {
    for &(mutate, message) in INVALID_CASES {
        let (mut columns, mut constraints) = declaration();
        mutate(&mut columns, &mut constraints);
        let memory = MemoryBudget::new(64 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let before = serde_json::to_vec(&(&columns, &constraints)).unwrap();
        assert_eq!(
            validate(&columns, &constraints, &control)
                .unwrap_err()
                .to_string(),
            message
        );
        assert_eq!(
            identity::claims::validate_constraint_identities(&columns, &constraints)
                .unwrap_err()
                .to_string(),
            message
        );
        let (result, _) = continuation::run(columns.clone(), constraints.clone());
        assert_eq!(
            result.unwrap_err().to_string(),
            crate::SQLError::Internal(message.into()).to_string()
        );
        assert_eq!(memory.used(), 0);
        assert_eq!(
            serde_json::to_vec(&(&columns, &constraints)).unwrap(),
            before
        );
    }
}

#[test]
fn constraint_identity_validation_observes_both_cancellation_owners() {
    let (columns, constraints) = declaration();
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(64 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        for result in [
            validate(&columns, &constraints, &control),
            identity::validate_not_null_identities_with_control(&columns, &control),
            identity::foreign_keys::validate_with_control(&columns, &constraints, &control),
            identity::claims::validate_present_identities_with_control(
                &columns,
                &constraints,
                &control,
            ),
            identity::claims::validate_key_and_check_identities_with_control(
                &columns,
                &constraints,
                &control,
            ),
            identity::keys::validate_provenance_with_control(&constraints, &control),
        ] {
            assert_eq!(
                result.unwrap_err().into_sql_error().sqlstate(),
                Some("57014")
            );
        }
        assert_eq!(memory.peak(), 0);
    }
}

#[test]
fn constraint_identity_claims_release_retained_prefixes_after_cancellation() {
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(64 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut objects = OwnedSet::new();
        assert!(insert(&mut objects, [1_u8; 16], &control).unwrap());
        assert_eq!(memory.used(), OwnedSet::<[u8; 16]>::entry_bytes());
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            insert(&mut objects, [2; 16], &control)
                .unwrap_err()
                .into_sql_error()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(objects.len(), 1);
        drop(objects);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn empty_constraint_identity_validation_has_no_allocation() {
    let memory = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let constraints = TableConstraintSet::default();
    let allocations =
        allocation_counter::measure(|| validate(&[], &constraints, &control).unwrap());
    assert_eq!(allocations.count_total, 0);
    assert_eq!(memory.peak(), 0);
}
