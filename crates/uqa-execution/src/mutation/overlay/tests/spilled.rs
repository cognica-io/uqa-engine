//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::DocumentStore;

/// An allowance that a held reservation keeps more than half used, so that staged rows spill once they hold a sixteenth of it.
fn pressured() -> (StorageReadControl, uqa_core::memory::MemoryReservation) {
    let control = StorageReadControl::with_limit(4 << 20);
    let held = control.memory().reserve((4 << 20) * 9 / 16).unwrap();
    (control, held)
}

/// A row whose `a` is `a`, whose `g` groups it with every tenth row, and whose payload makes it worth spilling.
fn spilled_row(a: i64, id: DocId) -> Document {
    BTreeMap::from([
        ("a".into(), Value::Int(a)),
        ("g".into(), Value::Int(i64::try_from(id % 10).unwrap())),
        (
            "z".into(),
            Value::Str(format!("row {id} {}", "x".repeat(200))),
        ),
    ])
}

fn spills(overlay: &CommandMutationOverlay) -> bool {
    overlay
        .table("items")
        .is_some_and(|table| table.rows.spilled.is_some())
}

fn find_a(
    overlays: &mut [CommandMutationOverlay],
    a: i64,
    control: &StorageReadControl,
) -> Option<DocId> {
    find(
        overlays,
        &["a"],
        &[Value::Int(a)],
        FieldPresence::Required,
        control,
    )
    .unwrap()
}

/// Stage rows `ids` with `a` equal to their identity, which moves the rows in memory into the spilled tier.
fn stage_rows(
    overlay: &mut CommandMutationOverlay,
    ids: std::ops::RangeInclusive<DocId>,
    control: &StorageReadControl,
) {
    for id in ids {
        stage(
            overlay,
            id,
            spilled_row(i64::try_from(id).unwrap(), id),
            control,
        );
    }
}

#[test]
fn spilled_rows_read_back_from_both_tiers_and_keep_their_keys_across_moves() {
    let (control, held) = pressured();
    let mut overlays = [CommandMutationOverlay::default()];
    stage_rows(&mut overlays[0], 1..=2_000, &control);
    assert!(spills(&overlays[0]));
    for id in [1, 777, 2_000] {
        assert_eq!(
            staged(&overlays[0], id).fields.get("a"),
            Some(&Value::Int(i64::try_from(id).unwrap()))
        );
    }
    // The first probe indexes the spilled rows as well as the rows in memory.
    assert_eq!(find_a(&mut overlays, 777, &control), Some(777));
    assert_eq!(find_a(&mut overlays, 2_001, &control), None);
    stage_rows(&mut overlays[0], 2_001..=4_000, &control);
    assert_eq!(find_a(&mut overlays, 3_333, &control), Some(3_333));
    // A row in memory shadows its spilled version and that version's key.
    stage(&mut overlays[0], 5, spilled_row(-5, 5), &control);
    overlays[0].stage("items", 6, None, &control).unwrap();
    assert_eq!(find_a(&mut overlays, 5, &control), None);
    assert_eq!(find_a(&mut overlays, -5, &control), Some(5));
    assert_eq!(find_a(&mut overlays, 6, &control), None);
    // Once the replacements spill too, their spilled keys replace the old ones.
    stage_rows(&mut overlays[0], 4_001..=6_000, &control);
    assert!(overlays[0]
        .table("items")
        .unwrap()
        .rows
        .memory
        .get(&5)
        .is_none());
    assert_eq!(find_a(&mut overlays, 5, &control), None);
    assert_eq!(find_a(&mut overlays, -5, &control), Some(5));
    assert_eq!(find_a(&mut overlays, 6, &control), None);
    assert!(matches!(
        CommandMutationOverlay::row(&overlays, "items", 6, &control).unwrap(),
        Some(None)
    ));
    // Restoring the original key restores its entry.
    stage(&mut overlays[0], 5, spilled_row(5, 5), &control);
    stage_rows(&mut overlays[0], 6_001..=8_000, &control);
    assert_eq!(find_a(&mut overlays, 5, &control), Some(5));
    assert_eq!(find_a(&mut overlays, -5, &control), None);
    drop(overlays);
    assert_eq!(control.memory().used(), held.bytes());
}

#[test]
fn shared_spilled_keys_count_their_rows_and_match_every_visible_one() {
    let (control, held) = pressured();
    let mut overlays = [CommandMutationOverlay::default()];
    stage_rows(&mut overlays[0], 1..=3_000, &control);
    assert!(spills(&overlays[0]));
    let group = |overlays: &mut [CommandMutationOverlay]| {
        let (ids, _memory) = CommandMutationOverlay::matches(
            overlays,
            "items",
            &["g".into()],
            &[Value::Int(3)],
            &control,
        )
        .unwrap()
        .into_parts();
        let mut ids = ids;
        ids.sort_unstable();
        ids
    };
    assert_eq!(group(&mut overlays).len(), 300);
    assert_eq!(
        find(
            &mut overlays,
            &["g"],
            &[Value::Int(3)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(3)
    );
    overlays[0].stage("items", 3, None, &control).unwrap();
    overlays[0].stage("items", 13, None, &control).unwrap();
    stage(&mut overlays[0], 23, spilled_row(23, 24), &control);
    assert_eq!(
        find(
            &mut overlays,
            &["g"],
            &[Value::Int(3)],
            FieldPresence::Required,
            &control
        )
        .unwrap(),
        Some(33)
    );
    stage_rows(&mut overlays[0], 3_001..=5_000, &control);
    let ids = group(&mut overlays);
    assert_eq!(ids.len(), 497);
    assert_eq!(ids[..2], [33, 43]);
    drop(overlays);
    assert_eq!(control.memory().used(), held.bytes());
}

#[test]
fn a_read_view_keeps_the_rows_staged_before_it_while_later_rows_spill() {
    let (control, held) = pressured();
    let mut overlays = [CommandMutationOverlay::default()];
    stage_rows(&mut overlays[0], 1..=2_000, &control);
    let view = CommandMutationOverlay::changes(
        &overlays,
        "items",
        crate::query::document_changes::DocumentChanges::default(),
        &control,
    )
    .unwrap();
    stage(&mut overlays[0], 10, spilled_row(-10, 10), &control);
    overlays[0].stage("items", 11, None, &control).unwrap();
    stage_rows(&mut overlays[0], 2_001..=4_000, &control);
    let seen = view.changes().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(seen.len(), 2_000);
    assert!(seen.iter().all(|(_, present)| *present));
    assert_eq!(view.get_field(10, "a").unwrap(), Some(Value::Int(10)));
    assert!(view.contains_doc_id(11).unwrap());
    let later = CommandMutationOverlay::changes(
        &overlays,
        "items",
        crate::query::document_changes::DocumentChanges::default(),
        &control,
    )
    .unwrap();
    assert_eq!(later.get_field(10, "a").unwrap(), Some(Value::Int(-10)));
    assert!(!later.contains_doc_id(11).unwrap());
    assert_eq!(later.len().unwrap(), 3_999);
    assert_eq!(later.next_doc_ids(Some(10), 2).unwrap(), [12, 13]);
    drop((view, later, overlays));
    assert_eq!(control.memory().used(), held.bytes());
}

#[test]
fn a_newer_command_masks_the_spilled_rows_of_an_older_one() {
    let (control, held) = pressured();
    let mut overlays = [
        CommandMutationOverlay::default(),
        CommandMutationOverlay::default(),
    ];
    stage_rows(&mut overlays[0], 1..=2_000, &control);
    assert!(spills(&overlays[0]));
    stage(&mut overlays[1], 7, spilled_row(-7, 7), &control);
    overlays[1].stage("items", 8, None, &control).unwrap();
    assert_eq!(find_a(&mut overlays, 7, &control), None);
    assert_eq!(find_a(&mut overlays, -7, &control), Some(7));
    assert_eq!(find_a(&mut overlays, 8, &control), None);
    assert_eq!(find_a(&mut overlays, 9, &control), Some(9));
    assert!(CommandMutationOverlay::stages(&overlays, "items", 1_999, &control).unwrap());
    assert!(!CommandMutationOverlay::stages(&overlays, "items", 2_001, &control).unwrap());
    let [older, newer] = &mut overlays;
    drop(std::mem::take(newer));
    assert_eq!(find_a(std::slice::from_mut(older), 7, &control), Some(7));
    drop(overlays);
    assert_eq!(control.memory().used(), held.bytes());
}

#[test]
fn large_spilled_rows_are_read_a_bounded_page_at_a_time() {
    let (control, held) = pressured();
    let mut overlays = [CommandMutationOverlay::default()];
    let large = |id: DocId| {
        BTreeMap::from([
            ("a".into(), Value::Int(i64::try_from(id).unwrap())),
            ("z".into(), Value::Str("y".repeat(200 * 1024))),
        ])
    };
    for id in 1..=24 {
        stage(&mut overlays[0], id, large(id), &control);
    }
    assert!(spills(&overlays[0]));
    let view = CommandMutationOverlay::changes(
        &overlays,
        "items",
        crate::query::document_changes::DocumentChanges::default(),
        &control,
    )
    .unwrap();
    let mut seen = 0;
    for row in view.into_rows() {
        let (id, row) = row.unwrap();
        assert_eq!(
            row.unwrap().fields().get("a"),
            Some(&Value::Int(i64::try_from(id).unwrap()))
        );
        seen += 1;
    }
    assert_eq!(seen, 24);
    assert_eq!(find_a(&mut overlays, 3, &control), Some(3));
    drop(overlays);
    assert_eq!(control.memory().used(), held.bytes());
}
