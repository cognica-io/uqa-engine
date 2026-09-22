//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::exact_lookup::FieldPresence;

fn stage(
    overlay: &mut CommandMutationOverlay,
    id: DocId,
    fields: Document,
    control: &StorageReadControl,
) {
    overlay
        .stage(
            "items",
            id,
            Some((Arc::new(fields), DocumentMetadata::with_tuple_xmin(17))),
            control,
        )
        .unwrap();
}

fn document(a: i64, z: &str) -> Document {
    BTreeMap::from([
        ("a".into(), Value::Int(a)),
        ("z".into(), Value::Str(z.into())),
    ])
}

fn find(
    overlays: &mut [CommandMutationOverlay],
    fields: &[&str],
    values: &[Value],
    presence: FieldPresence,
    control: &StorageReadControl,
) -> Result<Option<DocId>, SQLError> {
    CommandMutationOverlay::find_match(
        overlays,
        "items",
        &fields.iter().map(|name| (*name).into()).collect::<Vec<_>>(),
        values,
        presence,
        control,
    )
}

#[test]
fn exact_cache_normalizes_borrowed_columns_and_keeps_missing_null_and_duplicate_rules() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = [CommandMutationOverlay::default()];
    stage(&mut overlays[0], 2, document(1, "same"), &control);
    let mut present = document(1, "same");
    present.insert("nullable".into(), Value::Null);
    stage(&mut overlays[0], 5, present, &control);
    for (fields, values, presence, expected) in [
        (
            vec!["z", "a"],
            vec![Value::Str("same".into()), Value::Bool(true)],
            FieldPresence::Required,
            Some(2),
        ),
        (
            vec!["a", "z"],
            vec![Value::Float(1.0), Value::Str("same".into())],
            FieldPresence::Required,
            Some(2),
        ),
        (
            vec!["nullable"],
            vec![Value::Null],
            FieldPresence::Required,
            Some(5),
        ),
        (
            vec!["nullable"],
            vec![Value::Null],
            FieldPresence::MissingIsNull,
            Some(2),
        ),
        (
            vec!["a", "a"],
            vec![Value::Int(1), Value::Int(1)],
            FieldPresence::Required,
            Some(2),
        ),
        (
            vec!["a", "a"],
            vec![Value::Int(1), Value::Int(2)],
            FieldPresence::Required,
            None,
        ),
    ] {
        assert_eq!(
            find(&mut overlays, &fields, &values, presence, &control).unwrap(),
            expected
        );
    }
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[],
            FieldPresence::Required,
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("XX000")
    );
    drop(overlays);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn nested_frames_mask_old_keys_and_release_their_cache_with_the_command() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = vec![
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    for id in [1, 2, 4] {
        stage(&mut overlays[0], id, document(1, "parent"), &control);
    }
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
    overlays[1].stage("items", 1, None, &control).unwrap();
    stage(&mut overlays[1], 2, document(3, "replacement"), &control);
    stage(&mut overlays[1], 3, document(1, "child"), &control);
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(3)
    );
    drop(overlays.pop());
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
    stage(&mut overlays[0], 1, document(9, "updated"), &control);
    overlays[0].stage("items", 2, None, &control).unwrap();
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(4)
    );
    assert_eq!(
        find(
            &mut overlays,
            &["z"],
            &[Value::Str("updated".into())],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
    assert_eq!(
        overlays[0].documents("items").unwrap()[&1]
            .as_ref()
            .unwrap()
            .metadata
            .tuple_xmin(),
        Some(17)
    );
    drop(overlays);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn later_key_preparation_failure_preserves_all_cached_keys_and_the_previous_row() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = [CommandMutationOverlay::default()];
    stage(&mut overlays[0], 7, document(1, "old"), &control);
    for (field, value) in [("a", Value::Int(1)), ("z", Value::Str("old".into()))] {
        assert_eq!(
            find(
                &mut overlays,
                &[field],
                &[value],
                FieldPresence::Required,
                &control
            )
            .unwrap(),
            Some(7)
        );
    }
    let previous = overlays[0].documents("items").unwrap()[&7]
        .as_ref()
        .unwrap()
        .fields
        .clone();
    let replacement = Arc::new(document(2, &"x".repeat(8192)));
    let before = control.memory().used();
    let adopted = CommandStoredDocument::new(
        Arc::clone(&replacement),
        DocumentMetadata::default(),
        &control,
    )
    .unwrap();
    let adoption = control.memory().used() - before;
    drop(adopted);
    let blocker = control
        .memory()
        .reserve(control.memory().limit() - before - adoption - 2048)
        .unwrap();
    let error = overlays[0]
        .stage(
            "items",
            7,
            Some((replacement, DocumentMetadata::with_tuple_xmin(99))),
            &control,
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    drop(blocker);
    assert_eq!(control.memory().used(), before);
    assert!(std::ptr::eq(
        previous.as_ref(),
        overlays[0].documents("items").unwrap()[&7]
            .as_ref()
            .unwrap()
            .fields
            .as_ref()
    ));
    for (field, value, expected) in [
        ("a", Value::Int(1), Some(7)),
        ("a", Value::Int(2), None),
        ("z", Value::Str("old".into()), Some(7)),
    ] {
        assert_eq!(
            find(
                &mut overlays,
                &[field],
                &[value],
                FieldPresence::Required,
                &control
            )
            .unwrap(),
            expected
        );
    }
    assert_eq!(
        overlays[0].documents("items").unwrap()[&7]
            .as_ref()
            .unwrap()
            .metadata
            .tuple_xmin(),
        Some(17)
    );
}

#[test]
fn failed_index_construction_drops_only_its_unpublished_keys() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = [CommandMutationOverlay::default()];
    stage(
        &mut overlays[0],
        1,
        document(1, &"x".repeat(8192)),
        &control,
    );
    let before = control.memory().used();
    let blocker = control
        .memory()
        .reserve(control.memory().limit() - before - 2048)
        .unwrap();
    let error = find(
        &mut overlays,
        &["z"],
        &[Value::Str("small probe".into())],
        FieldPresence::Required,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    drop(blocker);
    assert_eq!(control.memory().used(), before);
    assert!(overlays[0].tables["items"].exact_indexes.is_empty());
    assert_eq!(
        find(
            &mut overlays,
            &["z"],
            &[Value::Str("x".repeat(8192))],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(1)
    );
}

#[test]
fn cancellation_and_foreign_allowances_cannot_replace_the_original_row_or_keys() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = [CommandMutationOverlay::default()];
    stage(&mut overlays[0], 1, document(1, "old"), &control);
    find(
        &mut overlays,
        &["a"],
        &[Value::Int(1)],
        FieldPresence::Required,
        &control,
    )
    .unwrap();
    let before = control.memory().used();
    control.cancellation().cancel();
    assert_eq!(
        overlays[0]
            .stage("items", 1, None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
    assert_eq!(control.memory().used(), before);
    let foreign = StorageReadControl::with_limit(1024 * 1024);
    assert_eq!(
        overlays[0]
            .stage("items", 1, None, &foreign)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &foreign
        )
        .unwrap_err()
        .sqlstate(),
        Some("XX000")
    );
    assert_eq!(foreign.memory().used(), 0);
    let next = StorageReadControl::new(control.memory(), &uqa_core::CancellationToken::new());
    assert_eq!(
        find(
            &mut overlays,
            &["a"],
            &[Value::Int(1)],
            FieldPresence::Required,
            &next
        )
        .unwrap(),
        Some(1)
    );
}

#[test]
fn shared_command_fields_keep_their_payload_after_cached_keys_are_released() {
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = [CommandMutationOverlay::default()];
    stage(
        &mut overlays[0],
        1,
        document(1, &"x".repeat(4096)),
        &control,
    );
    find(
        &mut overlays,
        &["z"],
        &[Value::Str("x".repeat(4096))],
        FieldPresence::Required,
        &control,
    )
    .unwrap();
    let retained = overlays[0].documents("items").unwrap()[&1]
        .as_ref()
        .unwrap()
        .fields
        .clone();
    let before = control.memory().used();
    drop(overlays);
    assert_eq!(retained["z"], Value::Str("x".repeat(4096)));
    assert!(control.memory().used() >= 4096 && control.memory().used() < before);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn canonical_cache_matches_the_value_equality_oracle_for_normalized_nested_values() {
    use uqa_core::{ArrayValue, DecimalValue, TemporalValue};
    let values = [
        Value::Bool(true),
        Value::Int(1),
        Value::Float(1.0),
        Value::Decimal(DecimalValue::parse("1.0000").unwrap()),
        Value::Float(-0.0),
        Value::Int(0),
        Value::Float(f64::NAN),
        Value::Decimal(DecimalValue::parse("NaN").unwrap()),
        Value::JsonB(r#"{"b":1.00,"a":null,"b":2}"#.into()),
        Value::JsonB(r#"{"a":null,"b":2.0}"#.into()),
        Value::FixedChar("abc  ".into()),
        Value::FixedChar("abc".into()),
        Value::Str("abc".into()),
        Value::Bytes(b"abc".to_vec()),
        Value::Record(vec![("old".into(), Value::Int(1))]),
        Value::Record(vec![("new".into(), Value::Float(1.0))]),
        Value::Array(ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![-1]).unwrap()),
        Value::Array(ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![1]).unwrap()),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 3_600_000_000,
            offset_minutes: 60,
        }),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 0,
            offset_minutes: 0,
        }),
        Value::Map(BTreeMap::from([(
            "nested".into(),
            Value::List(vec![Value::Float(1.0)]),
        )])),
        Value::Map(BTreeMap::from([(
            "nested".into(),
            Value::List(vec![Value::Bool(true)]),
        )])),
        Value::Null,
        Value::Void,
    ];
    let control = StorageReadControl::with_limit(1024 * 1024);
    let mut overlays = [CommandMutationOverlay::default()];
    for (index, value) in values.iter().enumerate() {
        stage(
            &mut overlays[0],
            index as DocId,
            BTreeMap::from([("a".into(), value.clone())]),
            &control,
        );
    }
    for value in &values {
        let expected = values
            .iter()
            .position(|candidate| candidate == value)
            .map(|index| index as DocId);
        assert_eq!(
            find(
                &mut overlays,
                &["a"],
                std::slice::from_ref(value),
                FieldPresence::Required,
                &control
            )
            .unwrap(),
            expected,
            "{value:?}"
        );
    }
}
