//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RelationshipValidation;
use crate::catalog::{
    image_validation::tests::snapshot,
    test_support::{table_snapshot, NoRoutines},
    CatalogReadView,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, RelationIdentity};
use uqa_sql::{
    ast::{AutoIncrement, ColumnDef, TableConstraintSet, TableHierarchy},
    ColumnType,
};

const WIDTH: usize = 256;

fn inheritance(missing: usize) -> CatalogReadView {
    let mut catalog = snapshot();
    let mut parents = Vec::new();
    for index in 0..WIDTH {
        let name = format!("p{index:04}");
        parents.push(if index == missing {
            "public.absent".into()
        } else {
            format!("public.{name}")
        });
        catalog.tables.insert(
            RelationIdentity::new("public", name),
            table_snapshot(
                (index as u128).to_be_bytes(),
                vec![],
                TableConstraintSet::default(),
            ),
        );
    }
    catalog.tables.insert(
        RelationIdentity::new("public", "child"),
        table_snapshot(
            (WIDTH as u128).to_be_bytes(),
            vec![],
            TableConstraintSet {
                hierarchy: TableHierarchy {
                    parents,
                    ..TableHierarchy::default()
                },
                ..TableConstraintSet::default()
            },
        ),
    );
    CatalogReadView::new(catalog)
}

fn sequences(missing: usize) -> CatalogReadView {
    let mut catalog = snapshot();
    let columns = (0..WIDTH)
        .map(|index| {
            let mut column = ColumnDef::nullable(format!("v{index:04}"), ColumnType::Integer);
            column.object_id = Some((index as u128).to_be_bytes());
            if index == missing {
                column.auto_increment = Some(AutoIncrement {
                    sequence: Some("public.absent".into()),
                    ..AutoIncrement::serial()
                });
            }
            column
        })
        .collect();
    catalog.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([1; 16], columns, TableConstraintSet::default()),
    );
    CatalogReadView::new(catalog)
}

fn windows_until_rejected(catalog: &CatalogReadView, steps: usize, diagnostic: &str) -> usize {
    let memory = MemoryBudget::new(16 << 20);
    let cancel = CancellationToken::new();
    let mut validation = RelationshipValidation::new(catalog.clone(), &memory, &cancel);
    for window in 1..=2_000_000 {
        match validation.advance_with_steps(&NoRoutines, &NoRoutines, steps) {
            Ok(Some(next)) => validation = next,
            Ok(None) => panic!("accepted a missing relationship"),
            Err(error) => {
                assert!(error.to_string().contains(diagnostic), "{error}");
                assert_eq!(memory.used(), 0);
                return window;
            }
        }
    }
    panic!("relationship validation did not complete")
}

fn check_windows(fixture: fn(usize) -> CatalogReadView, diagnostic: &str) {
    let first = fixture(0);
    let last = fixture(WIDTH - 1);
    for steps in [1, 32] {
        let early = windows_until_rejected(&first, steps, diagnostic);
        let late = windows_until_rejected(&last, steps, diagnostic);
        assert!(
            late >= early + (WIDTH - 1) / steps,
            "{steps} steps per call: first relationship failed on call {early}, last on {late}"
        );
    }
}

#[test]
fn private_catalog_validation_yields_between_inheritance_parents() {
    check_windows(inheritance, "missing parent table");
}

#[test]
fn private_catalog_validation_yields_between_implicit_sequence_columns() {
    check_windows(sequences, "missing implicit sequence");
}
