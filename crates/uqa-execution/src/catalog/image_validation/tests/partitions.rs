//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{test_support::NoRoutines, CatalogReadView};
use uqa_sql::ast::{Expr, PartitionBound, PartitionSpec, PartitionStrategy, TableHierarchy};

fn catalog() -> CatalogReadSnapshot {
    let mut snapshot = snapshot();
    for (id, name, hierarchy) in [
        (
            1,
            "parent",
            TableHierarchy {
                partition_spec: Some(PartitionSpec {
                    strategy: PartitionStrategy::List,
                    keys: vec![Expr::Column("id".into())],
                }),
                ..TableHierarchy::default()
            },
        ),
        (
            2,
            "child",
            TableHierarchy {
                parents: vec!["public.parent".into()],
                partition_bound: Some(PartitionBound::List(vec![Expr::Literal(
                    uqa_core::Value::Int(1),
                )])),
                ..TableHierarchy::default()
            },
        ),
    ] {
        snapshot.tables.insert(
            RelationIdentity::new("public", name),
            table_snapshot(
                [id; 16],
                vec![ColumnDef::nullable("id", uqa_sql::ColumnType::Integer)],
                TableConstraintSet {
                    hierarchy,
                    ..TableConstraintSet::default()
                },
            ),
        );
    }
    snapshot
}

#[test]
fn native_partition_bindings_join_original_parent_and_child_layouts() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let input = catalog();
    validate_partition_bindings(
        &CatalogReadView::new(input.clone()),
        &NoRoutines,
        &memory,
        &token,
    )
    .unwrap();
    assert_eq!(memory.used(), 0);
    let mut invalid = input.clone();
    let parent = invalid
        .tables
        .get_mut(&RelationIdentity::new("public", "parent"))
        .unwrap();
    parent.columns = vec![ColumnDef::nullable("id", uqa_sql::ColumnType::Boolean)].into();
    assert!(validate_partition_bindings(
        &CatalogReadView::new(invalid),
        &NoRoutines,
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
    let mut invalid = input;
    let child = invalid
        .tables
        .get_mut(&RelationIdentity::new("public", "child"))
        .unwrap();
    let mut hierarchy = (*child.hierarchy).clone();
    hierarchy.parents[0] = "public.absent".into();
    child.hierarchy = hierarchy.into();
    assert!(validate_partition_bindings(
        &CatalogReadView::new(invalid),
        &NoRoutines,
        &memory,
        &token
    )
    .is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_partition_catalog_validation_retires_denied_and_cancelled_metadata() {
    let catalog = CatalogReadView::new(catalog());
    let mut outcomes = (0, 0);
    for limit in [0, 64, 1024, 8192, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        match validate_partition_bindings(&catalog, &NoRoutines, &memory, &CancellationToken::new())
        {
            Ok(()) => outcomes.0 += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                outcomes.1 += 1;
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(outcomes.0 > 0 && outcomes.1 > 0);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    token.cancel();
    assert!(validate_partition_bindings(&catalog, &NoRoutines, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn retained_partition_bindings_yield_between_list_datum_comparisons() {
    const WIDTH: usize = 96;
    let input = |duplicate: Option<usize>| {
        let mut snapshot = catalog();
        let child = snapshot
            .tables
            .get_mut(&RelationIdentity::new("public", "child"))
            .unwrap();
        let mut hierarchy = (*child.hierarchy).clone();
        let mut entries = (0..WIDTH)
            .map(|value| Expr::Literal(uqa_core::Value::Int(i64::try_from(value).unwrap())))
            .collect::<Vec<_>>();
        if let Some(position) = duplicate {
            entries[position] = entries[0].clone();
        }
        hierarchy.partition_bound = Some(PartitionBound::List(entries));
        child.hierarchy = hierarchy.into();
        CatalogReadView::new(snapshot)
    };
    let memory = MemoryBudget::new(2 << 20);
    let drive = |duplicate, steps, cancel| {
        retained_binding_windows(
            &input(duplicate),
            &memory,
            &CancellationToken::new(),
            steps,
            cancel,
        )
    };
    for steps in [1, 31] {
        let (first, error) = drive(Some(1), steps, None);
        assert!(error
            .unwrap_err()
            .to_string()
            .contains("duplicate LIST datum"));
        let (last, error) = drive(Some(WIDTH - 1), steps, None);
        assert!(error
            .unwrap_err()
            .to_string()
            .contains("duplicate LIST datum"));
        let comparisons = (WIDTH - 2) * (WIDTH - 1) / 2;
        eprintln!(
            "LIST partition, {steps} steps: first {first}, last {last}, peak {}",
            memory.peak()
        );
        assert!(
            last >= first + comparisons / steps,
            "{steps} steps: first {first}, last {last}"
        );
        let (_, cancelled) = drive(None, steps, Some(first + comparisons / (2 * steps)));
        assert_eq!(cancelled.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
    drive(None, 64, None).1.unwrap();
    assert_eq!(memory.used(), 0);
}
