//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated expression keys follow their staged row through frame masking and spill.

use super::*;

fn row(value: Value, control: &StorageReadControl) -> CommandStoredDocument {
    CommandStoredDocument::new(
        Arc::new(Document::from([("physical".into(), Value::Int(50))])),
        DocumentMetadata::with_tuple_xmin(17),
        control,
    )
    .unwrap()
    .with_index_values(Document::from([("physical".into(), value)]), control)
    .unwrap()
}

fn key(value: i64) -> Value {
    Value::Row(vec![Value::Int(value), Value::Str("tail".into())].into())
}

fn candidates(
    overlays: &mut [CommandMutationOverlay],
    values: &[Value],
    control: &StorageReadControl,
) -> Result<Vec<DocId>, SQLError> {
    CommandMutationOverlay::expression_matches(overlays, "items", "physical", values, control)
        .map(|probe| probe.matches.iter().copied().collect())
}

fn matches(
    overlays: &mut [CommandMutationOverlay],
    value: i64,
    control: &StorageReadControl,
) -> Vec<DocId> {
    candidates(
        overlays,
        &[Value::Int(value), Value::Str("tail".into())],
        control,
    )
    .unwrap()
}

#[test]
fn expression_and_column_names_do_not_alias_and_newer_rows_mask_old_keys() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut overlays = [
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    overlays[0]
        .stage_evaluated("items", 1, Some(row(key(7), &control)), &control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 7, &control), [1]);
    assert_eq!(
        find(
            &mut overlays,
            &["physical"],
            &[Value::Int(50)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
    assert_eq!(
        candidates(
            &mut overlays,
            &[Value::Str("tail".into()), Value::Int(7)],
            &control
        )
        .unwrap()
        .len(),
        0
    );
    overlays[1]
        .stage_evaluated("items", 1, Some(row(key(8), &control)), &control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 7, &control).len(), 0);
    assert_eq!(matches(&mut overlays, 8, &control), [1]);
    overlays[1]
        .stage_evaluated("items", 1, None, &control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 8, &control).len(), 0);
    drop(overlays);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn expression_keys_keep_predicate_null_and_versions_through_spill_publication_and_undo() {
    let control = StorageReadControl::with_limit(4 << 20);
    let mut overlays = [CommandMutationOverlay::default()];
    for (id, value) in [
        (1, key(7)),
        (2, Value::Null),
        (3, Value::Row(vec![Value::Null].into())),
    ] {
        overlays[0]
            .stage_evaluated("items", id, Some(row(value, &control)), &control)
            .unwrap();
    }
    let checkpoint = overlays[0].checkpoint();
    // Build after spilling, then replace both a spilled key and a predicate marker.
    overlays[0]
        .tables
        .as_mut()
        .unwrap()
        .get_mut("items")
        .unwrap()
        .spill(&control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 7, &control), [1]);
    assert_eq!(
        candidates(&mut overlays, &[Value::Null], &control).unwrap(),
        [3]
    );
    CommandMutationOverlay::published_evaluated(
        &mut overlays,
        "items",
        1,
        Some(row(key(9), &control)),
        &control,
    )
    .unwrap();
    overlays[0]
        .stage_evaluated("items", 2, Some(row(key(7), &control)), &control)
        .unwrap();
    overlays[0]
        .stage_evaluated("items", 3, None, &control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 7, &control), [2]);
    assert_eq!(matches(&mut overlays, 9, &control), [1]);
    overlays[0]
        .tables
        .as_mut()
        .unwrap()
        .get_mut("items")
        .unwrap()
        .spill(&control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 9, &control), [1]);
    assert_eq!(
        candidates(&mut overlays, &[Value::Null], &control)
            .unwrap()
            .len(),
        0
    );
    assert!(staged(&overlays[0], 1).published);
    overlays[0].restore(&checkpoint);
    assert_eq!(matches(&mut overlays, 7, &control), [1]);
    assert_eq!(matches(&mut overlays, 9, &control).len(), 0);
    assert_eq!(
        candidates(&mut overlays, &[Value::Null], &control).unwrap(),
        [3]
    );
    assert!(!staged(&overlays[0], 1).published);
    drop(checkpoint);
    drop(overlays);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn failed_expression_key_preparation_preserves_row_and_both_index_namespaces() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut overlays = [CommandMutationOverlay::default()];
    overlays[0]
        .stage_evaluated("items", 1, Some(row(key(7), &control)), &control)
        .unwrap();
    assert_eq!(matches(&mut overlays, 7, &control), [1]);
    assert_eq!(
        find(
            &mut overlays,
            &["physical"],
            &[Value::Int(50)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
    let replacement = row(
        Value::Row(vec![Value::Str("x".repeat(65536))].into()),
        &control,
    );
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used() - 256)
        .unwrap();
    let error = overlays[0]
        .stage_evaluated("items", 1, Some(replacement), &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    drop(held);
    assert_eq!(matches(&mut overlays, 7, &control), [1]);
    assert_eq!(
        find(
            &mut overlays,
            &["physical"],
            &[Value::Int(50)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
}

#[test]
fn fallible_expression_keys_compare_only_visible_rows_in_key_order() {
    let control = StorageReadControl::with_limit(1 << 20);
    let invalid = Value::LegacyVector(
        uqa_core::LegacyVectorValue::try_from_array(
            uqa_core::LegacyVectorKind::Oid,
            uqa_core::ArrayValue::with_lower_bounds(vec![], vec![]).unwrap(),
        )
        .unwrap(),
    );
    let mut overlays = [
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    overlays[0]
        .stage_evaluated(
            "items",
            1,
            Some(row(
                Value::Row(vec![Value::Int(1), invalid.clone()].into()),
                &control,
            )),
            &control,
        )
        .unwrap();
    assert_eq!(
        candidates(&mut overlays, &[Value::Int(2), invalid.clone()], &control)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        candidates(&mut overlays, &[Value::Int(1), invalid.clone()], &control)
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );
    overlays[1]
        .stage_evaluated("items", 1, None, &control)
        .unwrap();
    assert_eq!(
        candidates(&mut overlays, &[Value::Int(1), invalid], &control)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn expression_match_visitor_preserves_frame_order_masking_and_consumer_errors() {
    let control = StorageReadControl::with_limit(4 << 20);
    let mut overlays = [
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    for id in 1..=256 {
        overlays[0]
            .stage_evaluated("items", id, Some(row(key(7), &control)), &control)
            .unwrap();
    }
    overlays[0]
        .tables
        .as_mut()
        .unwrap()
        .get_mut("items")
        .unwrap()
        .spill(&control)
        .unwrap();
    overlays[1]
        .stage_evaluated("items", 7, Some(row(key(8), &control)), &control)
        .unwrap();
    overlays[1]
        .stage_evaluated("items", 9, None, &control)
        .unwrap();
    overlays[1]
        .stage_evaluated("items", 257, Some(row(key(7), &control)), &control)
        .unwrap();
    let values = [Value::Int(7), Value::Str("tail".into())];
    let mut actual = Vec::new();
    let changes = CommandMutationOverlay::visit_expression_matches_with_catalog(
        &mut overlays,
        "items",
        "physical",
        &values,
        &control,
        None,
        &mut |id| {
            actual.push(id);
            Ok(())
        },
    )
    .unwrap();
    let expected: Vec<_> = std::iter::once(257)
        .chain((1..=256).filter(|id| ![7, 9].contains(id)))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        actual,
        candidates(&mut overlays, &values, &control).unwrap()
    );
    for id in [1, 7, 9, 257] {
        assert!(changes.contains_change(id).unwrap());
    }
    assert!(!changes.contains_change(258).unwrap());
    let mut visits = 0;
    let error = CommandMutationOverlay::visit_expression_matches_with_catalog(
        &mut overlays,
        "items",
        "physical",
        &values,
        &control,
        None,
        &mut |_| {
            visits += 1;
            Err(SQLError::Routine {
                sqlstate: "22012".into(),
                message: "capture failed".into(),
            })
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(visits, 1);
}
