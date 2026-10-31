//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::batch::materialization::SelectedRowMaterializer;

#[test]
fn constant_fragments_preserve_positional_projection_and_materialization() {
    for width in 0..24 {
        let source = PhysicalRow::concat(&PhysicalRow::nulls(width), &PhysicalRow::present());
        let mut expected = vec![Value::Null; width];
        expected.push(Value::Bool(true));
        assert_eq!(source.clone().into_physical_values(), expected);
        for prefix in 0..=expected.len() {
            assert_eq!(
                source.clone().into_prefix(prefix).into_physical_values(),
                expected[..prefix]
            );
        }
        let selections = [
            (0..expected.len()).collect::<Vec<_>>(),
            (0..expected.len()).rev().collect(),
            vec![width, width, NULL_SLOT, width + 1, 0],
            Vec::new(),
        ];
        for slots in selections {
            let projected = source.project_slots(&slots);
            let values = slots
                .iter()
                .map(|slot| expected.get(*slot).cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>();
            assert_eq!(projected.clone().into_physical_values(), values);
            let schema = RowSchema::new((0..values.len()).map(|i| format!("v{i}")).collect());
            let rows =
                Batch::from_physical_rows(schema.clone(), vec![projected]).into_result_rows();
            for (i, value) in values.iter().enumerate() {
                assert_eq!(&rows[0][&format!("v{i}")], value);
            }
        }
    }
}

#[test]
fn constant_selection_does_not_allocate_or_copy_shared_stored_payloads() {
    let source = PhysicalRow::nulls(100_000);
    let mut projected = None;
    let allocation = allocation_counter::measure(|| {
        projected = Some(source.project_slots(&[99_999, 0, 2, 2]));
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(
        projected.unwrap().into_physical_values(),
        vec![Value::Null; 4]
    );

    let payload = "large".repeat(65_536);
    let pointer = payload.as_ptr();
    let stored = PhysicalRow::from_values(vec![Value::Str(payload), Value::Int(7)]);
    let source = PhysicalRow::concat(
        &PhysicalRow::concat(&stored, &PhysicalRow::nulls(2)),
        &PhysicalRow::present(),
    );
    // Repeated row-local aliases must still donate the final selected payload.
    let projected = source.project_slots(&[4, 1, 2, 0, 1, 4, NULL_SLOT]);
    drop((source, stored));
    let source = projected;
    let schema = RowSchema::new((0..7).map(|i| format!("v{i}")).collect());
    let materializer = SelectedRowMaterializer::new(
        &schema,
        &[Some(3), Some(0), Some(2), Some(4), Some(5), None],
    );
    let mut values = vec![Value::Null; 6];
    let allocation = allocation_counter::measure(|| {
        materializer.materialize(source, |output, value| values[output] = value);
    });
    assert!(allocation.bytes_total < 8192, "{allocation:?}");
    let Value::Str(payload) = &values[0] else {
        panic!("retained text")
    };
    assert_eq!(payload.as_ptr(), pointer);
    assert_eq!(
        values[1..],
        [
            Value::Bool(true),
            Value::Null,
            Value::Int(7),
            Value::Bool(true),
            Value::Null
        ]
    );
}

#[test]
fn direct_constant_materialization_retires_the_row_without_ownership_attempts() {
    let schema = RowSchema::new(vec!["a".into(), "b".into()]);
    let materializer = SelectedRowMaterializer::new(&schema, &[Some(1), Some(0), None]);
    for mut row in [
        PhysicalRow::nulls(2),
        PhysicalRow::concat(&PhysicalRow::present(), &PhysicalRow::present()),
    ] {
        let expected = row.clone().into_physical_values();
        let mut values = vec![Value::Null; 3];
        let mut copied = false;
        let before = SelectedRowMaterializer::ownership_attempts();
        if row.fragment_count() == 1 {
            copied = materializer.copy_shared_row(&mut row, |output, value| values[output] = value);
            assert!(copied);
            assert_eq!(row.fragment_count(), 0);
        }
        if !copied {
            materializer.materialize(row, |output, value| values[output] = value);
        }
        assert_eq!(
            values,
            [expected[1].clone(), expected[0].clone(), Value::Null]
        );
        assert_eq!(SelectedRowMaterializer::ownership_attempts(), before);
    }
}
