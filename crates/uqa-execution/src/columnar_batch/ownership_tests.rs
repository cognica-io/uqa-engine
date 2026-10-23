//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::PhysicalRow;
use std::sync::Arc;

fn text(value: &Value) -> &str {
    let Value::Str(text) = value else {
        panic!("expected text, got {value:?}");
    };
    text
}

#[test]
fn shared_scan_transfer_reuses_projection_without_row_routing_or_ownership_attempts() {
    for width in [2, 40] {
        let names = (0..width)
            .map(|slot| format!("c{slot}"))
            .collect::<Vec<_>>();
        let schema = RowSchema::new(names.clone());
        let source = Arc::new((0..width).map(|slot| Value::Int(slot as i64)).collect());
        let projection: Arc<[usize]> = (0..width).rev().collect();
        let rows = (0..1024)
            .map(|_| PhysicalRow::from_shared_values(Arc::clone(&source), Arc::clone(&projection)))
            .collect();
        let batch = Batch::from_physical_rows(schema, rows);
        let attempts = crate::batch::SelectedRowMaterializer::ownership_attempts();
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(ColumnarBatch::from_batch(&names, batch));
        });
        assert_eq!(
            crate::batch::SelectedRowMaterializer::ownership_attempts(),
            attempts
        );
        assert!(
            allocations.count_total < width as u64 * 4 + 16,
            "{allocations:?}"
        );
        let output = output.unwrap();
        assert_eq!(output.len(), 1024);
        for (slot, column) in output.columns().iter().enumerate() {
            assert_eq!(
                column.values,
                vec![Value::Int((width - slot - 1) as i64); 1024]
            );
        }
        assert_eq!(Arc::strong_count(&source), 1);
    }
}

#[test]
fn shared_scan_transfer_releases_consumed_rows_before_the_last_donation() {
    let value = "shared within the batch".repeat(1024);
    let address = value.as_ptr();
    let values = Arc::new(vec![Value::Str(value), Value::Int(7)]);
    let projection: Arc<[usize]> = Arc::from([1, 0]);
    let rows = (0..8)
        .map(|_| PhysicalRow::from_shared_values(Arc::clone(&values), Arc::clone(&projection)))
        .collect();
    drop(values);
    let names = vec!["number".into(), "text".into(), "absent".into()];
    let schema = RowSchema::new(names[..2].to_vec());
    let attempts = crate::batch::SelectedRowMaterializer::ownership_attempts();
    let result = ColumnarBatch::from_batch(&names, Batch::from_physical_rows(schema, rows));
    assert_eq!(
        crate::batch::SelectedRowMaterializer::ownership_attempts() - attempts,
        1
    );
    assert_eq!(result.columns()[0].values, vec![Value::Int(7); 8]);
    assert_eq!(result.columns()[2].values, vec![Value::Null; 8]);
    let texts = &result.columns()[1].values;
    assert_eq!(text(&texts[7]).as_ptr(), address);
    for value in &texts[..7] {
        assert_eq!(value, &texts[7]);
        assert_ne!(text(value).as_ptr(), address);
    }
}

#[test]
fn owned_columnar_transfer_moves_selected_payloads_without_payload_allocations() {
    let names = vec!["text".into(), "array".into()];
    let schema = RowSchema::new(names.clone());
    let mut addresses = Vec::new();
    let rows = (0..32)
        .map(|index| {
            let string = format!("row {index} {}", "x".repeat(16 * 1024));
            let nested = vec![Value::Str("nested".repeat(4096)), Value::Int(index)];
            addresses.push((string.as_ptr(), nested.as_ptr()));
            PhysicalRow::from_values(vec![
                Value::Str(string),
                Value::Array(uqa_core::ArrayValue::try_new(nested).unwrap()),
            ])
        })
        .collect();
    let batch = Batch::from_physical_rows(schema, rows);
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(ColumnarBatch::from_batch(&names, batch));
    });
    assert!(allocated.bytes_total < 16 * 1024, "{allocated:?}");
    let result = result.unwrap();
    assert_eq!(result.len(), 32);
    for (index, (string, array)) in addresses.into_iter().enumerate() {
        assert_eq!(text(&result.columns()[0].values[index]).as_ptr(), string);
        let Value::Array(values) = &result.columns()[1].values[index] else {
            panic!("expected array");
        };
        assert_eq!(values.elements().as_ptr(), array);
        assert_eq!(values.elements()[1], Value::Int(index as i64));
    }
}

#[test]
fn owned_columnar_transfer_preserves_remapped_fragments_and_duplicate_positions() {
    let source = RowSchema::new(vec!["left".into(), "unused".into(), "right".into()]);
    let schema = RowSchema::select(
        &source,
        &[
            ("value".into(), "right".into()),
            ("left".into(), "left".into()),
            ("value".into(), "left".into()),
        ],
    );
    let left = "left payload".repeat(128);
    let right = "right payload".repeat(128);
    let left_address = left.as_ptr();
    let right_address = right.as_ptr();
    let row = PhysicalRow::from_values(vec![Value::Str(left), Value::Str("unused".into())])
        .append_values(vec![Value::Str(right)]);
    let batch = Batch::from_physical_rows(schema, vec![row]);
    let result = ColumnarBatch::from_batch(
        &[
            "value".into(),
            "left".into(),
            "value".into(),
            "missing".into(),
            "value".into(),
        ],
        batch,
    );
    let columns = result.columns();
    assert_eq!(text(&columns[0].values[0]).as_ptr(), right_address);
    assert_eq!(columns[1].values, columns[2].values);
    assert_ne!(text(&columns[1].values[0]).as_ptr(), left_address);
    assert_eq!(text(&columns[2].values[0]).as_ptr(), left_address);
    assert_eq!(columns[3].values, [Value::Null]);
    assert_eq!(columns[4].values, [Value::Null]);
}

#[test]
fn owned_columnar_transfer_counts_aliases_after_fragment_projection() {
    let first = "selected first".repeat(128);
    let second = "selected second".repeat(128);
    let first_address = first.as_ptr();
    let second_address = second.as_ptr();
    let row = PhysicalRow::from_shared_values(
        Arc::new(vec![Value::Str(first), Value::Str(second), Value::Int(42)]),
        Arc::from([1, 0, 1, usize::MAX]),
    );
    let names = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let batch = Batch::from_physical_rows(RowSchema::new(names.clone()), vec![row]);
    let result = ColumnarBatch::from_batch(&names, batch);
    let columns = result.columns();
    assert_eq!(columns[0].values, columns[2].values);
    assert_ne!(text(&columns[0].values[0]).as_ptr(), second_address);
    assert_eq!(text(&columns[1].values[0]).as_ptr(), first_address);
    assert_eq!(text(&columns[2].values[0]).as_ptr(), second_address);
    assert_eq!(columns[3].values, [Value::Null]);
}

#[test]
fn shared_columnar_transfer_clones_only_selected_payloads() {
    let stored = Arc::new(vec![
        Value::Str("unused".repeat(128 * 1024)),
        Value::Str("selected".into()),
    ]);
    let row = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([0, 1]));
    let source = RowSchema::new(vec!["unused".into(), "selected".into()]);
    let schema = RowSchema::select(&source, &[("result".into(), "selected".into())]);
    let batch = Batch::from_physical_rows(schema, vec![row]);
    let names = vec!["result".into()];
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(ColumnarBatch::from_batch(&names, batch));
    });
    assert!(allocated.bytes_total < 4096, "{allocated:?}");
    let result = result.unwrap();
    assert_eq!(result.columns()[0].values, [Value::Str("selected".into())]);
    assert_eq!(text(&stored[0]).len(), 6 * 128 * 1024);
    assert_eq!(text(&stored[1]), "selected");
    assert_ne!(
        text(&result.columns()[0].values[0]).as_ptr(),
        text(&stored[1]).as_ptr()
    );
    assert_eq!(Arc::strong_count(&stored), 1);
}

#[test]
fn owned_columnar_transfer_preserves_empty_and_zero_column_cardinality() {
    for count in [0, 3] {
        let rows = (0..count)
            .map(|_| PhysicalRow::from_values(vec![Value::Int(42)]))
            .collect();
        let batch = Batch::from_physical_rows(RowSchema::new(vec!["unused".into()]), rows);
        let result = ColumnarBatch::from_batch(&[], batch);
        assert_eq!(result.len(), count);
        assert_eq!(
            result.into_positional_rows(),
            vec![Vec::<Value>::new(); count]
        );
        let batch =
            Batch::from_physical_rows(RowSchema::new(vec![]), vec![PhysicalRow::default(); count]);
        let result = ColumnarBatch::from_batch(&["missing".into()], batch);
        assert_eq!(result.len(), count);
        assert_eq!(result.columns()[0].values, vec![Value::Null; count]);
    }
}

#[test]
fn owned_columnar_transfer_matches_borrowed_across_joined_projection_layouts() {
    for width in [0, 1, 8, 31] {
        let stored = Arc::new(
            (0..width)
                .map(|i| Value::Str(format!("value {i}")))
                .collect::<Vec<_>>(),
        );
        let projection: Arc<[usize]> = (0..width)
            .rev()
            .chain(0..width)
            .chain([usize::MAX])
            .collect();
        let left = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::clone(&projection));
        let right = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::clone(&projection));
        let row = PhysicalRow::concat(&left, &right);
        drop((left, right, stored));
        let width = 2 * projection.len();
        let labels = (0..width)
            .map(|i| format!("label {}", i % 3))
            .collect::<Vec<_>>();
        let mut requested = labels.clone();
        requested.reverse();
        requested.push("absent".into());
        let batch = Batch::from_physical_rows(RowSchema::new(labels), vec![row]);
        let expected = ColumnarBatch::from_borrowed_batch(&requested, &batch);
        assert_eq!(ColumnarBatch::from_batch(&requested, batch), expected);
    }
}

#[test]
fn owned_columnar_transfer_moves_values_shared_only_between_input_fragments() {
    for select_second in [false, true] {
        let first = "first payload".repeat(1024);
        let second = "second payload".repeat(1024);
        let first_address = first.as_ptr();
        let second_address = second.as_ptr();
        let stored = Arc::new(vec![Value::Str(first), Value::Str(second)]);
        let left = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([0]));
        let right = PhysicalRow::from_shared_values(stored, Arc::from([1, 0]));
        let row = PhysicalRow::concat(&left, &right);
        drop((left, right));
        let names = vec!["a".into(), "b".into(), "c".into()];
        let batch = Batch::from_physical_rows(RowSchema::new(names.clone()), vec![row]);
        let requested = if select_second {
            names
        } else {
            vec!["a".into()]
        };
        let result = ColumnarBatch::from_batch(&requested, batch);
        if select_second {
            assert_eq!(result.columns()[0].values, result.columns()[2].values);
            assert_ne!(text(&result.columns()[0].values[0]).as_ptr(), first_address);
            assert_eq!(
                text(&result.columns()[1].values[0]).as_ptr(),
                second_address
            );
            assert_eq!(text(&result.columns()[2].values[0]).as_ptr(), first_address);
        } else {
            assert_eq!(text(&result.columns()[0].values[0]).as_ptr(), first_address);
        }
    }
}
