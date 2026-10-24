//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn named_fragmented_output_copies_only_selected_shared_values() {
    let source = RowSchema::new(vec!["selected".into(), "unused".into()]);
    let source = RowSchema::append(&source, &["metadata".into()]);
    let schema = RowSchema::select(&source, &[("selected".into(), "selected".into())]);
    let stored = Arc::new(vec![Value::Int(7), Value::Str("unused".repeat(32768))]);
    let row = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([0, 1]))
        .append_values(vec![Value::Int(99)]);
    let batch = Batch::from_physical_rows(schema, vec![row]);
    let mut output = Vec::new();
    let allocations = allocation_counter::measure(|| output = batch.into_result_rows());

    assert_eq!(
        output,
        vec![BTreeMap::from([("selected".into(), Value::Int(7))])]
    );
    assert_eq!(Arc::strong_count(&stored), 1);
    assert_eq!(stored[1], Value::Str("unused".repeat(32768)));
    assert!(
        allocations.bytes_total <= 8192,
        "selected scalar output copied an unselected shared payload: {} bytes",
        allocations.bytes_total
    );
}

#[test]
fn empty_named_projection_releases_fragments_without_copying_payloads() {
    let source = RowSchema::new(vec!["left".into(), "right".into()]);
    let schema = RowSchema::select(&source, &[]);
    let stored = Arc::new(vec![Value::Str("unselected".repeat(32768))]);
    let row = PhysicalRow::from_shared_values(Arc::clone(&stored), Arc::from([0]))
        .append_values(vec![Value::Str("owned".repeat(32768))]);
    let batch = Batch::from_physical_rows(schema, vec![row]);
    let mut output = Vec::new();
    let allocations = allocation_counter::measure(|| output = batch.into_result_rows());

    assert_eq!(output, vec![BTreeMap::new()]);
    assert_eq!(Arc::strong_count(&stored), 1);
    assert!(
        allocations.bytes_total <= 8192,
        "empty output copied unselected fragmented payloads: {} bytes",
        allocations.bytes_total
    );
}

#[test]
fn named_output_donates_payload_after_discarding_row_local_aliases() {
    let schema = RowSchema::new(vec!["selected".into(), "unused".into(), "alias".into()]);
    let schema = RowSchema::select(&schema, &[("selected".into(), "selected".into())]);
    let payload = "selected".repeat(32768);
    let pointer = payload.as_ptr();
    let source = PhysicalRow::from_values(vec![
        Value::Str(payload),
        Value::Str("unselected".repeat(32768)),
    ]);
    let alias = source.project_slots(&[0]);
    let row = PhysicalRow::concat(&source, &alias);
    drop(source);
    drop(alias);
    let batch = Batch::from_physical_rows(schema, vec![row]);
    let mut output = Vec::new();
    let allocations = allocation_counter::measure(|| output = batch.into_result_rows());

    let Value::Str(value) = &output[0]["selected"] else {
        panic!("selected value must remain text");
    };
    assert_eq!(value.as_ptr(), pointer);
    assert_eq!(value, &"selected".repeat(32768));
    assert!(
        allocations.bytes_total <= 8192,
        "row-local aliases caused retained payload copies: {} bytes",
        allocations.bytes_total
    );
}

#[test]
fn named_output_clones_only_the_repeated_selected_alias() {
    let source_schema = RowSchema::new(vec!["selected".into(), "unused".into(), "alias".into()]);
    let schema = RowSchema::select(
        &source_schema,
        &[
            ("z".into(), "selected".into()),
            ("a".into(), "alias".into()),
        ],
    );
    let payload = "selected".repeat(32768);
    let pointer = payload.as_ptr();
    let payload_bytes = payload.len() as u64;
    let source = PhysicalRow::from_values(vec![
        Value::Str(payload),
        Value::Str("unselected".repeat(32768)),
    ]);
    let alias = source.project_slots(&[0]);
    let row = PhysicalRow::concat(&source, &alias);
    drop(source);
    drop(alias);
    let batch = Batch::from_physical_rows(schema, vec![row]);
    let mut output = Vec::new();
    let allocations = allocation_counter::measure(|| output = batch.into_result_rows());

    let (Value::Str(left), Value::Str(right)) = (&output[0]["a"], &output[0]["z"]) else {
        panic!("selected aliases must remain text");
    };
    assert_eq!(left, right);
    assert_ne!(left.as_ptr(), right.as_ptr());
    assert!(left.as_ptr() == pointer || right.as_ptr() == pointer);
    assert!(
        allocations.bytes_total <= payload_bytes + 8192,
        "two selected aliases must copy only one payload: {} bytes",
        allocations.bytes_total
    );
}
