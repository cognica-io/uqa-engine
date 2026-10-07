//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::overlay::CommandMutationOverlay;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{memory::MemoryBudget, Value};
use uqa_storage::{read_control::StorageReadControl, DocumentMetadata};

#[test]
fn indexed_filters_merge_newest_rows_tombstones_and_nulls() {
    let memory = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = StorageReadControl::new(&memory, &cancellation);
    let mut overlays = [
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    for (frame, id, value) in [
        (0, 1, Some(Value::Int(10))),
        (0, 2, Some(Value::Int(10))),
        (0, 4, Some(Value::Int(10))),
        (0, 5, Some(Value::Int(10))),
        (1, 1, Some(Value::Int(20))),
        (1, 2, None),
        (1, 4, Some(Value::Null)),
    ] {
        overlays[frame]
            .stage(
                "t",
                id,
                value.map(|value| {
                    (
                        Arc::new(BTreeMap::from([("v".into(), value)])),
                        DocumentMetadata::default(),
                    )
                }),
                &control,
            )
            .unwrap();
    }
    let changes =
        CommandMutationOverlay::changes(&overlays, "t", DocumentChanges::default(), &control)
            .unwrap();
    // Stored identity 3 survives without a field read; 1, 2 and 4 are masked. Identity 5 exists only in the command.
    for (predicate, stored, expected) in [
        (
            Predicate::Equals(Value::Int(10)),
            vec![1, 2, 3, 4],
            vec![3, 5],
        ),
        (
            Predicate::GreaterThan(Value::Int(15)),
            vec![2, 3],
            vec![1, 3],
        ),
        (Predicate::IsNull, vec![2, 3], vec![3, 4]),
    ] {
        let indexed = PostingList::from_sorted_unchecked(
            stored
                .into_iter()
                .map(|id| PostingEntry::new(id, Payload::default()))
                .collect(),
        );
        let result = merge_changes(
            indexed,
            Some(changes.clone()),
            "v",
            &predicate,
            &cancellation,
        )
        .unwrap();
        assert_eq!(
            result
                .entries()
                .iter()
                .map(|entry| entry.doc_id)
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn cached_equalities_mask_fixed_versions_and_keep_retained_views() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut overlays = [
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    let mut fixed = DocumentChanges::from_rows(
        BTreeMap::from([
            (
                1,
                Some(uqa_storage::StoredDocument::new(BTreeMap::from([(
                    "v".into(),
                    Value::Int(10),
                )]))),
            ),
            (2, None),
            (
                6,
                Some(uqa_storage::StoredDocument::new(BTreeMap::from([(
                    "v".into(),
                    Value::Int(10),
                )]))),
            ),
        ]),
        &control,
    )
    .unwrap();
    for (frame, id, value) in [
        (0, 1, Some(20)),
        (0, 3, Some(10)),
        (0, 4, Some(10)),
        (1, 3, None),
        (1, 5, Some(10)),
    ] {
        overlays[frame]
            .stage(
                "t",
                id,
                value.map(|value| {
                    (
                        Arc::new(BTreeMap::from([("v".into(), Value::Int(value))])),
                        DocumentMetadata::default(),
                    )
                }),
                &control,
            )
            .unwrap();
    }
    let indexed = || {
        PostingList::from_sorted_unchecked(
            [1, 2, 4, 7]
                .into_iter()
                .map(|id| PostingEntry::new(id, Payload::default()))
                .collect(),
        )
    };
    let query = |overlays: &mut [_], fixed: &DocumentChanges| {
        let command = CommandMutationOverlay::column_matches(
            overlays,
            "t",
            &["v".into()],
            &[Value::Int(10)],
            &control,
        )
        .unwrap();
        merge_exact(
            indexed(),
            fixed,
            command,
            "v",
            &Predicate::Equals(Value::Int(10)),
            control.cancellation(),
        )
        .unwrap()
        .entries()
        .iter()
        .map(|row| row.doc_id)
        .collect::<Vec<_>>()
    };
    assert_eq!(query(&mut overlays, &fixed), vec![4, 5, 6, 7]);
    let old = fixed.clone();
    fixed.insert_shared(6, None, &control).unwrap();
    assert_eq!(query(&mut overlays, &fixed), vec![4, 5, 7]);
    assert_eq!(query(&mut overlays, &old), vec![4, 5, 6, 7]);
    let checkpoint = overlays[1].checkpoint();
    overlays[1].stage("t", 5, None, &control).unwrap();
    assert_eq!(query(&mut overlays, &old), vec![4, 6, 7]);
    overlays[1].restore(&checkpoint);
    assert_eq!(query(&mut overlays, &old), vec![4, 5, 6, 7]);
}
