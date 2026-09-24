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
    assert!(control.memory().used() > 0);
    assert!(control.memory().used() < retained);
    drop(rows);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn separate_captures_share_the_command_payload_charge_until_the_last_view_drops() {
    let control = control();
    let fields =
        RetainedDocumentFields::new(Arc::new(document(42).into_fields()), &control).unwrap();
    let payload = control.memory().used();
    let capture = || {
        DocumentChanges::from_retained(
            [(
                1,
                Some((fields.clone(), DocumentMetadata::with_tuple_xmin(41))),
            )],
            &control,
        )
        .unwrap()
    };
    let first = capture();
    let selection = control.memory().used() - payload;
    let second = capture();
    assert_eq!(control.memory().used(), payload + selection * 2);
    let reader = first.snapshot().unwrap().snapshot().unwrap();
    drop(fields);
    drop(first);
    assert_eq!(control.memory().used(), payload + selection * 2);
    drop(second);
    assert_eq!(control.memory().used(), payload + selection);
    assert_eq!(reader.get_field(1, "key").unwrap(), Some(Value::Int(42)));
    assert_eq!(
        reader.get_metadata(1).unwrap().unwrap().tuple_xmin(),
        Some(41)
    );
    drop(reader);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn oversized_replacement_keeps_the_original_fields_and_cannot_switch_allowances() {
    let control = control();
    let mut changes =
        DocumentChanges::from_rows([(1, Some(document(42)))].into(), &control).unwrap();
    let original = changes.snapshot().unwrap();
    let retained = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained - 4096)
        .unwrap();
    let other = StorageReadControl::with_limit(usize::MAX);
    let error = changes
        .insert_shared(
            1,
            Some((
                Arc::new(document(99).into_fields()),
                DocumentMetadata::default(),
            )),
            &other,
        )
        .unwrap_err();
    assert_eq!(
        crate::storage_errors::storage_error("payload", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(other.memory().used(), 0);
    drop(full);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(changes.get_field(1, "key").unwrap(), Some(Value::Int(42)));
    assert_eq!(original.get_field(1, "key").unwrap(), Some(Value::Int(42)));
}

#[test]
fn copied_private_payloads_reject_quota_overflow_and_release_capture_scratch() {
    let mut probe = Probe::new(&source());
    probe.allow_copy = true;
    let control = StorageReadControl::with_limit(4096);
    let desired = selected(&control, &[(1, true)]);
    assert!(matches!(
        DocumentChanges::capture_owned(&probe, desired, &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        probe.source.get_field(1, "key").unwrap(),
        Some(Value::Int(10))
    );
}
