//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::ColumnType;
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

fn columns(numbers: &[Option<i16>]) -> Vec<ColumnDef> {
    numbers
        .iter()
        .map(|number| {
            let mut column = ColumnDef::nullable("large name".repeat(128), ColumnType::Integer);
            column.attribute_number = *number;
            column
        })
        .collect()
}

fn dropped(numbers: &[i16]) -> Vec<DroppedAttribute> {
    numbers
        .iter()
        .map(|number| {
            let mut attribute = DroppedAttribute::from_column(
                &ColumnDef::nullable("", ColumnType::Integer),
                0,
                true,
                0,
            )
            .unwrap();
            attribute.number = *number;
            attribute
        })
        .collect()
}

// The prior set-based contract remains an independent oracle for the fixed-domain representation.
fn reference(columns: &[ColumnDef], dropped: &[DroppedAttribute]) -> Result<(), SQLError> {
    let numbers = columns
        .iter()
        .map(|column| {
            column.attribute_number.ok_or_else(|| {
                SQLError::Internal("relation attributes require initial catalog migration".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let all: BTreeSet<_> = numbers
        .iter()
        .copied()
        .chain(dropped.iter().map(|attribute| attribute.number))
        .collect();
    if all.last().is_some_and(|number| *number > MAX_ATTRIBUTES) {
        return Err(too_many_columns());
    }
    if all.len() != numbers.len() + dropped.len()
        || all
            .iter()
            .copied()
            .ne(1..=i16::try_from(all.len()).unwrap_or(i16::MAX))
        || numbers.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(SQLError::Internal(
            "invalid relation attribute slot layout".into(),
        ));
    }
    Ok(())
}

#[test]
fn retained_attribute_validation_preserves_original_error_order_and_fixed_domain() {
    let choices = [
        None,
        Some(i16::MIN),
        Some(-1),
        Some(0),
        Some(1),
        Some(2),
        Some(3),
        Some(1600),
        Some(1601),
        Some(i16::MAX),
    ];
    let memory = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for a in choices {
        for b in choices {
            for c in choices.into_iter().flatten() {
                let columns = columns(&[a, b]);
                let dropped = dropped(&[c]);
                let actual = validate_with_control(&columns, &dropped, &control)
                    .map_err(|error| error.to_string());
                let expected = reference(&columns, &dropped).map_err(|error| error.to_string());
                assert_eq!(actual, expected, "{a:?}, {b:?}, {c}");
            }
        }
    }
    let all = (1..=MAX_ATTRIBUTES).map(Some).collect::<Vec<_>>();
    let columns = columns(&all);
    let allocations =
        allocation_counter::measure(|| validate_with_control(&columns, &[], &control).unwrap());
    assert_eq!(allocations.count_total, 0);
    assert_eq!(memory.peak(), 0);
    for split in [0, 1, 63, 64, 1599, 1600] {
        validate_with_control(
            &columns[split..],
            &dropped(
                &(1..=i16::try_from(split).unwrap())
                    .rev()
                    .collect::<Vec<_>>(),
            ),
            &control,
        )
        .unwrap();
    }
}

#[test]
fn retained_attribute_validation_bounds_work_and_retires_cancellation() {
    let columns = Arc::new(columns(&(1..=128).map(Some).collect::<Vec<_>>()));
    for prefix in [0, 1, 63, 127, 128, 192, 255] {
        let memory = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let retained = memory.clone();
        let cancellation = original.clone();
        let declaration = Arc::clone(&columns);
        let mut input =
            ImageDecoder::<(), SQLError>::new(&memory, &original, |mut reader| async move {
                let mut reader = reader.input();
                let control = ProductionControl::new(&retained, &cancellation, reader.cancellation);
                validate_from_input(&declaration, &[], &control, &mut reader).await
            })
            .unwrap();
        for _ in 0..prefix {
            assert!(input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap()
                .is_none());
        }
        original.cancel();
        assert_eq!(
            input
                .advance(
                    &mut std::io::empty(),
                    ImageDecodeWindow { bytes: 1, steps: 1 }
                )
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            validate_with_control(
                &columns,
                &[],
                &ProductionControl::new(&memory, &original, &invoking)
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.peak(), 0);
    }
}
