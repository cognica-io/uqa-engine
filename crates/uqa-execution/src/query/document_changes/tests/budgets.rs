//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn selected(control: &StorageReadControl, rows: &[(DocId, bool)]) -> DocumentSelection {
    let mut result = DocumentSelection::new(control);
    for (id, present) in rows {
        result.insert(*id, *present, control).unwrap();
    }
    result
}

#[test]
fn desired_rows_charge_capacity_and_resolve_repeated_changes_in_evaluation_order() {
    let control = control();
    let probe = Probe::new(&source());
    let desired = selected(
        &control,
        &[
            (4, true),
            (1, false),
            (4, false),
            (1, true),
            (u64::MAX, true),
        ],
    );
    assert!(control.memory().used() > 0);
    let changes = DocumentChanges::default()
        .with_retained(probe.snapshot().unwrap(), desired, &control)
        .unwrap();
    assert_eq!(
        changes.changes().collect::<Vec<_>>(),
        [(1, true), (4, false), (u64::MAX, true)]
    );
    assert!(probe.copies.lock().is_empty());
    assert!(control.memory().used() > 0);
    drop(changes);
    assert_eq!(control.memory().used(), 0);

    let empty = StorageReadControl::with_limit(0);
    let mut rejected = DocumentSelection::new(&empty);
    assert!(rejected.insert(1, true, &empty).is_err());
    assert!(rejected.is_empty());
    assert_eq!(empty.memory().used(), 0);
}

#[test]
fn shared_selection_keeps_its_allowance_through_failed_copy_and_last_reader_drop() {
    let control = control();
    let mut changes = DocumentChanges::from_shared([(1, None), (3, None)], &control).unwrap();
    let original = changes.clone();
    let snapshot = changes.snapshot().unwrap().snapshot().unwrap();
    let retained = control.memory().used();
    assert!(retained > 0);
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    // A caller with a different allowance cannot enlarge a retained selection's original budget.
    let error = changes
        .insert_shared(2, None, &StorageReadControl::with_limit(usize::MAX))
        .unwrap_err();
    assert_eq!(
        crate::storage_errors::storage_error("selection", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(
        changes.changes().collect::<Vec<_>>(),
        [(1, false), (3, false)]
    );
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    changes.insert_shared(2, None, &control).unwrap();
    assert!(control.memory().used() > retained);
    assert_eq!(
        original.changes().collect::<Vec<_>>(),
        [(1, false), (3, false)]
    );
    drop(changes);
    drop(original);
    assert_eq!(control.memory().used(), retained);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn rejected_merge_and_cancellation_preserve_rows_and_reservations() {
    let control = control();
    let mut original = DocumentChanges::from_shared([(1, None)], &control).unwrap();
    let newer = DocumentChanges::from_shared([(2, None)], &control).unwrap();
    let retained = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    assert!(original.extend(newer.clone(), &control).is_err());
    assert_eq!(original.changes().collect::<Vec<_>>(), [(1, false)]);
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    control.cancellation().cancel();
    assert!(matches!(
        original.extend(newer.clone(), &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), retained);
    control.cancellation().reset();
    original.extend(newer, &control).unwrap();
    assert_eq!(
        original.changes().collect::<Vec<_>>(),
        [(1, false), (2, false)]
    );
    drop(original);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn owning_iteration_moves_payloads_and_keeps_capacity_charged_until_iterator_drop() {
    let control = control();
    let row = document(42);
    let Value::Str(payload) = &row.fields()["opaque"] else {
        panic!()
    };
    let address = payload.as_ptr();
    let changes = DocumentChanges::from_rows([(1, Some(row))].into(), &control).unwrap();
    let retained = control.memory().used();
    let mut rows = changes.into_rows();
    let (id, row) = rows.next().unwrap().unwrap();
    assert_eq!(id, 1);
    let row = row.unwrap();
    let Value::Str(payload) = &row.fields()["opaque"] else {
        panic!()
    };
    assert_eq!(payload.as_ptr(), address);
    assert_eq!(control.memory().used(), retained);
    drop(rows);
    assert_eq!(control.memory().used(), 0);
}
