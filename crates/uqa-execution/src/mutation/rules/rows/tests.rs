//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use uqa_sql::ColumnType;

#[test]
fn rule_events_spill_original_images_and_replay_selected_ordinals_with_bounded_memory() {
    // Initialize the encrypted-file runtime before checking that event ownership releases its own allocations.
    let warm =
        RuleRows::from_rows([Ok(RuleRowImage::empty())], 0, CancellationToken::new()).unwrap();
    assert!(warm.get(0).is_ok());
    drop(warm);
    let allocated = allocation_counter::measure(|| {
        let mut buffer = RuleRowBuffer::new(64 * 1024, CancellationToken::new());
        for index in 0..2048 {
            let mut row = RuleRowImage::empty();
            row.old_doc_id = Some(index);
            row.new_doc_id = Some(u64::MAX - index);
            row.old = Some(BTreeMap::from([(
                "body".into(),
                Value::Str("o".repeat(8192)),
            )]));
            row.new = Some(BTreeMap::from([(
                "body".into(),
                Value::Str("n".repeat(8192)),
            )]));
            buffer.push(row).unwrap();
        }
        let rows = buffer.finish();
        assert!(rows.0.rows.lock().spilled_bytes() > 32 * 1024 * 1024);
        let retained = rows.clone();
        drop(rows);
        for index in [2047, 0, 1024, 17, 2047] {
            let row = retained.get(index).unwrap();
            assert_eq!(row.old_doc_id, Some(index as u64));
            assert_eq!(row.new_doc_id, Some(u64::MAX - index as u64));
            assert_eq!(row.old.unwrap()["body"], Value::Str("o".repeat(8192)));
            assert_eq!(row.new.unwrap()["body"], Value::Str("n".repeat(8192)));
        }
        assert!(retained.get(2048).is_err());
    });
    // Includes construction of all original images, encoding, replay and shared ownership.
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
    assert_eq!(allocated.bytes_current, 0);
}

#[test]
fn rule_events_preserve_complete_context_schema_lock_lineage_and_absent_images() {
    let schema = RowSchema::with_qualified_types(
        "input",
        vec!["value".into()],
        vec![Some(ColumnType::Text)],
    )
    .with_column_collations([Some(950)]);
    let origin = crate::batch::RowLockOrigin::new("input", "public.source", u64::MAX);
    let mut row = RuleRowImage::empty();
    row.old_storage_table = Some("schema.\"table\"".into());
    row.old_doc_id = Some(u64::MAX);
    row.old = Some(BTreeMap::new());
    row.context = Some(OwnedPhysicalRow::new(
        schema.clone(),
        PhysicalRow::from_values(vec![Value::FixedChar("kept  ".into())])
            .with_lock_origin(origin.clone()),
    ));
    for bytes in [0, 64 * 1024] {
        let rows = RuleRows::from_rows(
            [Ok(RuleRowImage::empty()), Ok(row.clone())],
            bytes,
            CancellationToken::new(),
        )
        .unwrap();
        let empty = rows.get(0).unwrap();
        assert!(empty.old.is_none() && empty.new.is_none() && empty.context.is_none());
        let actual = rows.get(1).unwrap();
        assert_eq!(actual.old_storage_table, row.old_storage_table);
        assert_eq!(actual.old_doc_id, row.old_doc_id);
        assert_eq!(actual.old, Some(BTreeMap::new()));
        assert!(actual.new.is_none());
        let context = actual.context.unwrap();
        assert!(context.schema.shares_index(&schema));
        assert_eq!(
            context.row.value(0),
            Some(&Value::FixedChar("kept  ".into()))
        );
        assert_eq!(context.row.lock_origins(), std::slice::from_ref(&origin));
    }
}

#[test]
fn rule_event_failure_and_cancellation_preserve_the_published_owner() {
    let cancellation = CancellationToken::new();
    let rows = RuleRows::from_rows([Ok(RuleRowImage::empty())], 0, cancellation.clone()).unwrap();
    let retained = rows.clone();
    let failed = RuleRows::from_rows(
        [Ok(RuleRowImage::empty()), Err(invalid("projection failed"))],
        0,
        CancellationToken::new(),
    );
    assert!(failed.is_err());
    assert!(retained.get(0).is_ok());
    cancellation.cancel();
    assert!(rows.get(0).is_err());
    assert!(retained.get(0).is_err());
    let mut buffer = RuleRowBuffer::new(0, cancellation);
    assert!(buffer.push(RuleRowImage::empty()).is_err());
    assert!(buffer.finish().is_empty());
    let bad = PhysicalRow::from_values(vec![Value::Null; WIDTH]);
    assert!(decode(bad, None).is_err());
}

#[test]
fn rule_event_selection_shares_full_ownership_and_replays_fragmented_indices() {
    use crate::mutation::rules::{RuleIndexBuffer, RuleIndices};

    let cancellation = CancellationToken::new();
    let rows = RuleRows::from_rows(
        (0..7).map(|index| {
            let mut row = RuleRowImage::empty();
            row.old_doc_id = Some(index);
            Ok(row)
        }),
        0,
        cancellation.clone(),
    )
    .unwrap();
    let all = RuleIndices::all(rows.len(), cancellation.clone());
    let selected = rows.select_indices(&all, 0, cancellation.clone()).unwrap();
    assert!(Arc::ptr_eq(&rows.0, &selected.0));
    let mut selection = RuleIndexBuffer::new(1, cancellation.clone()).unwrap();
    for index in [6, 2, 2, 0] {
        selection.push(index).unwrap();
    }
    let selected = rows
        .select_indices(&selection.finish().unwrap(), 0, cancellation.clone())
        .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|row| row.unwrap().old_doc_id.unwrap())
            .collect::<Vec<_>>(),
        [6, 2, 2, 0]
    );
    let mut invalid = RuleIndexBuffer::new(1, cancellation.clone()).unwrap();
    invalid.push(rows.len()).unwrap();
    assert!(rows
        .select_indices(&invalid.finish().unwrap(), 0, cancellation.clone())
        .is_err());
    cancellation.cancel();
    assert_eq!(
        rows.select_indices(&all, 0, cancellation)
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014")
    );
}
