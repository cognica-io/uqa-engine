//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{declaration, ColumnDef, TableConstraintSet};
use crate::{schema::constraint_metadata::identity, SQLError};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

type Declaration = Arc<(Vec<ColumnDef>, TableConstraintSet)>;

fn input(
    declaration: Declaration,
    memory: &MemoryBudget,
    original: &CancellationToken,
) -> Result<ImageDecoder<(), SQLError>, SQLError> {
    let retained = memory.clone();
    let cancellation = original.clone();
    Ok(ImageDecoder::new(
        memory,
        original,
        |mut reader| async move {
            let mut reader = reader.input();
            let control = ProductionControl::new(&retained, &cancellation, reader.cancellation);
            identity::claims::validate_constraint_identities_from_input(
                &declaration.0,
                &declaration.1,
                &control,
                &mut reader,
            )
            .await
            .map_err(super::super::ConstraintMetadataError::into_sql_error)
        },
    )?)
}

fn advance(input: &mut ImageDecoder<(), SQLError>) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

pub(super) fn run(
    columns: Vec<ColumnDef>,
    constraints: TableConstraintSet,
) -> (Result<(), SQLError>, usize) {
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let mut input = input(Arc::new((columns, constraints)), &memory, &original).unwrap();
    for steps in 1..100_000 {
        match advance(&mut input) {
            Ok(None) => (),
            result => {
                assert_eq!(memory.used(), 0);
                assert!(advance(&mut input).is_err());
                return (result.map(|value| value.unwrap()), steps);
            }
        }
    }
    panic!("constraint identity validation stopped progressing");
}

#[test]
fn retained_constraint_identity_validation_yields_for_empty_slots_and_large_key_names() {
    for count in [0, 64, 512] {
        let columns = (0..count)
            .map(|_| ColumnDef::nullable("", crate::ast::ColumnType::Integer))
            .collect();
        let (result, steps) = run(columns, TableConstraintSet::default());
        result.unwrap();
        assert!(steps >= count * 8, "{count}: {steps}");
        assert!(steps <= count * 16 + 16, "{count}: {steps}");
    }
    let (columns, mut constraints) = declaration();
    constraints.key_constraints[0].columns = vec!["column_한글".repeat(16_384)];
    constraints.key_constraints[0].included_columns = vec!["included".repeat(16_384)];
    constraints.hierarchy.partition_inherited_key_constraints = constraints.key_constraints.clone();
    let before = serde_json::to_vec(&(&columns, &constraints)).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let declaration = Arc::new((columns, constraints));
    let mut steps = 0;
    let allocation = allocation_counter::measure(|| {
        let mut input = input(Arc::clone(&declaration), &memory, &original).unwrap();
        while advance(&mut input).unwrap().is_none() {
            steps += 1;
        }
    });
    assert!(steps > 100);
    assert!(allocation.bytes_total < 16_384, "{allocation:?}");
    assert_eq!(allocation.bytes_current, 0);
    assert_eq!(memory.used(), 0);
    assert_eq!(serde_json::to_vec(&*declaration).unwrap(), before);
    for offset in [0, 4095, 131_071] {
        let mut changed = declaration.1.clone();
        changed.hierarchy.partition_inherited_key_constraints[0].included_columns[0]
            .replace_range(offset..=offset, "X");
        let (result, _) = run(declaration.0.clone(), changed);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("partition key provenance does not identify one local constraint"));
    }
}

#[test]
fn retained_constraint_identity_validation_retires_each_unpublished_prefix() {
    let declaration = Arc::new(declaration());
    let (_, steps) = run(declaration.0.clone(), declaration.1.clone());
    for prefix in 0..steps - 1 {
        for terminal in ["cancel", "zero", "drop", "unwind"] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let mut input = input(Arc::clone(&declaration), &memory, &original).unwrap();
            for _ in 0..prefix {
                assert!(advance(&mut input).unwrap().is_none());
            }
            match terminal {
                "cancel" => {
                    original.cancel();
                    assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
                }
                "zero" => {
                    assert!(input
                        .advance(
                            &mut std::io::empty(),
                            ImageDecodeWindow { bytes: 1, steps: 0 }
                        )
                        .is_err());
                }
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _input = input;
                        panic!("abandon constraint validation");
                    }));
                    assert_eq!(memory.used(), 0);
                    continue;
                }
                "drop" => (),
                _ => unreachable!(),
            }
            drop(input);
            assert_eq!(memory.used(), 0, "{terminal} after {prefix}");
        }
    }
}

#[test]
fn retained_constraint_identity_validation_rejects_each_denied_prefix() {
    let declaration = Arc::new(declaration());
    let memory = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let mut complete = input(Arc::clone(&declaration), &memory, &original).unwrap();
    while advance(&mut complete).unwrap().is_none() {}
    let peak = memory.peak();
    for limit in (0..peak).step_by(31).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let result =
            input(Arc::clone(&declaration), &memory, &original).and_then(|mut input| loop {
                if advance(&mut input)?.is_some() {
                    break Ok(());
                }
            });
        assert_eq!(
            result.unwrap_err().sqlstate(),
            Some("53200"),
            "limit {limit}"
        );
        assert_eq!(memory.used(), 0);
    }
}
