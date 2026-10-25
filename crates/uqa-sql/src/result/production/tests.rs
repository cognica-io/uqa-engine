//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn cells(count: usize, control: &ProductionControl<'_>) -> Produced<Vec<Value>> {
    let mut values = ProductionVec::new(*control);
    for _ in 0..count {
        values
            .push_produced(
                control
                    .finish(Value::Null, control.empty_reservation())
                    .unwrap(),
            )
            .unwrap();
    }
    values.finish().unwrap()
}

#[test]
fn result_metadata_and_rows_keep_their_allowance_until_release() {
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = cells(3, &control);
    let input_bytes = budget.used();
    let output =
        SQLResult::from_column_with_control("QUERY PLAN", &ColumnType::Text, input, &control)
            .unwrap();
    assert_eq!(output.columns, ["QUERY PLAN"]);
    assert_eq!(output.column_types, [Some(ColumnType::Text)]);
    assert_eq!(output.rows.len(), 3);
    assert_eq!(output.value_at(0, 0), Some(&Value::Null));
    assert!(budget.used() > input_bytes);
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
    let output = SQLResult::from_column_with_control(
        "QUERY PLAN",
        &ColumnType::Json,
        cells(0, &control),
        &control,
    )
    .unwrap();
    assert_eq!(output.kind, SQLResultKind::Rows);
    assert_eq!(output.column_types, [Some(ColumnType::Json)]);
    assert!(output.rows.is_empty());
    assert!(budget.used() > 0);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn repeated_row_labels_are_admitted_before_copying_and_failed_results_release_cells() {
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let name = "n".repeat(4096);
    let input = cells(128, &control);
    let mut failed = false;
    let measured = allocation_counter::measure(|| {
        failed =
            SQLResult::from_column_with_control(&name, &ColumnType::Text, input, &control).is_err();
    });
    assert!(failed);
    assert!(
        measured.bytes_max < 128 * 1024,
        "metadata allocation escaped admission: {measured:?}"
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn result_metadata_obeys_each_original_cancellation_owner() {
    for original_cancelled in [false, true] {
        let budget = MemoryBudget::new(64 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = cells(8, &control);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error =
            SQLResult::from_column_with_control("QUERY PLAN", &ColumnType::Text, input, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn result_publication_moves_original_cell_payloads_without_copying() {
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let text = control.copy_text(&"x".repeat(8192)).unwrap();
    let pointer = text.as_ptr();
    let (text, memory) = text.into_parts();
    let mut input = ProductionVec::new(control);
    input
        .push_produced(control.finish(Value::Str(text), memory).unwrap())
        .unwrap();
    let output = SQLResult::from_column_with_control(
        "QUERY PLAN",
        &ColumnType::Text,
        input.finish().unwrap(),
        &control,
    )
    .unwrap();
    let Some(Value::Str(text)) = output.value_at(0, 0) else {
        panic!("text cell changed representation");
    };
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(text.len(), 8192);
    drop(output);
    assert_eq!(budget.used(), 0);
}
