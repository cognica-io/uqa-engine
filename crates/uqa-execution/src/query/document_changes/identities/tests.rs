//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const TABLE: RelationGeneration = [7; 16];
const OTHER: RelationGeneration = [9; 16];

fn entries(view: &ChangedIdentitiesView, control: &StorageReadControl) -> Vec<(DocId, bool)> {
    let mut cursor = view.cursor(None);
    let mut entries = Vec::new();
    while let Some(entry) = cursor.next(control).unwrap() {
        entries.push(entry);
    }
    entries
}

#[test]
fn changes_count_against_the_rows_present_before_the_first_change() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut identities = ChangedIdentities::new(&control).unwrap();
    // Inserted, then updated: absent before, present now.
    identities.note(&TABLE, 5, false, true, &control).unwrap();
    identities.note(&TABLE, 5, true, true, &control).unwrap();
    // Updated, then deleted: present before, absent now.
    identities.note(&TABLE, 2, true, true, &control).unwrap();
    identities.note(&TABLE, 2, true, false, &control).unwrap();
    // Inserted, then deleted: absent before and now.
    identities.note(&TABLE, 9, false, true, &control).unwrap();
    identities.note(&TABLE, 9, true, false, &control).unwrap();
    identities.note(&OTHER, 1, true, false, &control).unwrap();
    let counts = identities.counts(&TABLE);
    assert_eq!(
        counts,
        ChangedRowCounts {
            changed: 3,
            before: 1,
            present: 1
        }
    );
    // A snapshot of ten rows loses the deleted one and gains the inserted one.
    assert_eq!(counts.visible_rows(10), Some(10));
    let view = identities.view(&TABLE).unwrap().unwrap();
    assert_eq!(
        entries(&view, &control),
        [(2, false), (5, true), (9, false)]
    );
    assert_eq!(view.presence(5, &control).unwrap(), Some(true));
    assert_eq!(view.presence(2, &control).unwrap(), Some(false));
    assert_eq!(view.presence(3, &control).unwrap(), None);
    assert!(identities.view(&[1; 16]).unwrap().is_none());
    assert_eq!(identities.counts(&OTHER).before, 1);
}

#[test]
fn savepoints_restore_entries_and_counts_and_views_keep_their_moment() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut identities = ChangedIdentities::new(&control).unwrap();
    identities.note(&TABLE, 1, false, true, &control).unwrap();
    let outer = StorageSavepointId::allocate();
    identities.savepoint(outer).unwrap();
    identities.note(&TABLE, 2, false, true, &control).unwrap();
    identities.note(&TABLE, 1, true, false, &control).unwrap();
    let before_rollback = identities.view(&TABLE).unwrap().unwrap();
    let inner = StorageSavepointId::allocate();
    identities.savepoint(inner).unwrap();
    identities.note(&TABLE, 3, false, true, &control).unwrap();
    assert!(identities.rollback_to(outer).unwrap());
    assert_eq!(
        identities.counts(&TABLE),
        ChangedRowCounts {
            changed: 1,
            before: 0,
            present: 1
        }
    );
    let restored = identities.view(&TABLE).unwrap().unwrap();
    assert_eq!(entries(&restored, &control), [(1, true)]);
    // A view taken before the rollback keeps what it saw.
    assert_eq!(entries(&before_rollback, &control), [(1, false), (2, true)]);
    // The inner savepoint went with the rollback; the outer one stays for another.
    assert!(!identities.rollback_to(inner).unwrap());
    identities.note(&TABLE, 4, false, true, &control).unwrap();
    assert!(identities.release(outer).unwrap());
    assert!(!identities.release(outer).unwrap());
    assert_eq!(identities.counts(&TABLE).changed, 2);
}

#[test]
fn many_changes_spill_under_a_small_allowance() {
    let control = StorageReadControl::with_limit(4 << 20);
    let held = control.memory().reserve((4 << 20) * 9 / 16).unwrap();
    let mut identities = ChangedIdentities::new(&control).unwrap();
    for id in 0..60_000 {
        identities
            .note(&TABLE, id, id % 2 == 0, id % 3 != 0, &control)
            .unwrap();
    }
    let view = identities.view(&TABLE).unwrap().unwrap();
    let counts = view.counts();
    assert_eq!(counts.changed, 60_000);
    assert_eq!(counts.before, 30_000);
    assert_eq!(counts.present, 40_000);
    let listed = entries(&view, &control);
    assert_eq!(listed.len(), 60_000);
    assert!(listed.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(view.presence(59_997, &control).unwrap(), Some(false));
    assert_eq!(view.presence(59_999, &control).unwrap(), Some(true));
    drop((view, listed, identities));
    assert_eq!(control.memory().used(), held.bytes());
}
