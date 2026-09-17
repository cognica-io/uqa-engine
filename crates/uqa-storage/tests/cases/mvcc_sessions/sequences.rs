//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence consumers retain the view that supplied their allocation and definition inputs.

use super::occurrences::InterleavedStore;
use super::*;
use uqa_storage::{
    CatalogFacade, KeyValueCatalog, RelationIdentity, SequenceOptions, SequenceReservationResult,
    SequenceRow, SequenceValueReservation,
};

fn sequence() -> SequenceRow {
    SequenceRow {
        relation: RelationIdentity::new("public", "ids"),
        role_owner: "owner".into(),
        acl: None,
        object_id: [1; 16],
        definition_generation: [2; 16],
        start: 1,
        increment: 1,
        current: 1,
        called: false,
        log_count: 0,
        persistence: "p".into(),
        owner: None,
        options: SequenceOptions {
            cache_size: 3,
            ..SequenceOptions::default()
        },
    }
}

fn reserve(
    catalog: &KeyValueCatalog,
) -> uqa_storage::StorageBackendResult<SequenceValueReservation> {
    let SequenceReservationResult::Reserved(reservation) =
        catalog.reserve_sequence_values("ids", [1; 16], [2; 16])?
    else {
        panic!("expected a reservation");
    };
    Ok(reservation)
}

fn assert_conflict(error: StorageBackendError) {
    let StorageBackendError::Backend { source, .. } = error else {
        panic!("expected MVCC conflict: {error}");
    };
    assert!(
        matches!(
            source.downcast_ref::<VersionError>(),
            Some(VersionError::WriteConflict { .. })
        ),
        "expected MVCC conflict: {source}"
    );
}

#[test]
fn private_sequence_provenance_follows_definition_changes_and_savepoint_undo() {
    let persistence = Persistence::new();
    let store = Arc::new(persistence.session(1 << 22));
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_schema("public").unwrap();
    let mut row = sequence();
    catalog.create_sequence_row(&row).unwrap();
    let private = || {
        catalog
            .sequence_has_private_changes(&row.relation, row.object_id)
            .unwrap()
    };
    assert!(!private());
    store.begin_transaction().unwrap();
    store.savepoint("before_definition").unwrap();
    assert!(!private());
    let mut replacement = row.clone();
    replacement.definition_generation = [3; 16];
    catalog.replace_sequence_row(&replacement).unwrap();
    assert!(private());
    store.rollback_to_savepoint("before_definition").unwrap();
    assert!(!private());
    row.relation.name = "ids_suffix".into();
    row.object_id = [4; 16];
    catalog.create_sequence_row(&row).unwrap();
    assert!(catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
    let original = sequence();
    assert!(!catalog
        .sequence_has_private_changes(&original.relation, original.object_id)
        .unwrap());
    store.rollback_transaction().unwrap();
    assert!(!catalog
        .sequence_has_private_changes(&row.relation, row.object_id)
        .unwrap());
}

#[test]
fn sequence_reservation_cannot_reuse_a_block_published_after_its_read() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let b = Arc::new(persistence.session(1 << 22));
    let other = KeyValueCatalog::new(b);
    other.save_schema("public").unwrap();
    other.create_sequence_row(&sequence()).unwrap();
    let wrapper = Arc::new(InterleavedStore::new(a.clone()));
    let catalog = KeyValueCatalog::new(wrapper.clone());
    *wrapper.after_point.lock() = Some(Box::new(move || {
        let reservation = reserve(&other).unwrap();
        assert_eq!((reservation.first_value, reservation.last_value), (1, 3));
    }));
    assert_conflict(reserve(&catalog).expect_err("the first block belongs to the other session"));
    a.rollback_transaction().unwrap();
    let reservation = reserve(&catalog).unwrap();
    assert_eq!((reservation.first_value, reservation.last_value), (4, 6));
}

#[test]
fn sequence_value_mutations_cannot_overwrite_an_intervening_definition() {
    for operation in ["reserve", "set", "replace"] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 22));
        let other = KeyValueCatalog::new(Arc::new(persistence.session(1 << 22)));
        other.save_schema("public").unwrap();
        other.create_sequence_row(&sequence()).unwrap();
        let wrapper = Arc::new(InterleavedStore::new(a.clone()));
        let catalog = KeyValueCatalog::new(wrapper.clone());
        *wrapper.after_point.lock() = Some(Box::new(move || {
            let mut changed = sequence();
            changed.definition_generation = [3; 16];
            changed.current = 100;
            assert!(other.replace_sequence_row(&changed).unwrap());
        }));
        let result = match operation {
            "reserve" => reserve(&catalog).map(|_| ()),
            "set" => catalog
                .set_sequence_value("ids", [1; 16], [2; 16], 50, false, 0)
                .map(|_| ()),
            _ => catalog.replace_sequence_row(&sequence()).map(|_| ()),
        };
        assert_conflict(result.expect_err(operation));
        assert_eq!(
            wrapper
                .evaluations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        a.rollback_transaction().unwrap();
        let saved = catalog.load_sequence_rows().unwrap().remove(0);
        assert_eq!(saved.definition_generation, [3; 16]);
        assert_eq!(saved.current, 100);
    }
}

#[test]
fn sequence_lifecycle_keeps_original_claim_and_value_preconditions() {
    for operation in ["create", "rename", "drop"] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 22));
        let other = KeyValueCatalog::new(Arc::new(persistence.session(1 << 22)));
        other.save_schema("public").unwrap();
        if operation != "create" {
            other.create_sequence_row(&sequence()).unwrap();
        }
        let wrapper = Arc::new(InterleavedStore::new(a.clone()));
        let catalog = KeyValueCatalog::new(wrapper.clone());
        *wrapper.after_evaluation.lock() = Some(Box::new(move || {
            if operation == "create" {
                let mut winner = sequence();
                winner.object_id = [9; 16];
                winner.current = 100;
                assert!(other.create_sequence_row(&winner).unwrap());
            } else {
                assert_eq!(
                    other
                        .set_sequence_value("ids", [1; 16], [2; 16], 100, false, 0)
                        .unwrap(),
                    uqa_storage::SequenceSetValueResult::Set(100)
                );
            }
        }));
        let result = match operation {
            "create" => catalog.create_sequence_row(&sequence()),
            "rename" => catalog.rename_sequence_row("ids", "renamed"),
            _ => catalog.drop_sequence_row("ids"),
        };
        assert_conflict(result.expect_err(operation));
        assert_eq!(
            wrapper
                .evaluations
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        a.rollback_transaction().unwrap();
        let saved = catalog.load_sequence_rows().unwrap().remove(0);
        assert_eq!(saved.relation, sequence().relation);
        assert_eq!(saved.current, 100);
        assert_eq!(
            saved.object_id,
            if operation == "create" {
                [9; 16]
            } else {
                [1; 16]
            }
        );
        let mut destination = sequence();
        destination.relation.name = "renamed".into();
        destination.object_id = [4; 16];
        assert!(catalog.create_sequence_row(&destination).unwrap());
    }
}

#[test]
fn failed_sequence_evaluation_preserves_earlier_private_writes() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let wrapper = Arc::new(InterleavedStore::new(a.clone()));
    let catalog = KeyValueCatalog::new(wrapper.clone());
    catalog.save_schema("public").unwrap();
    catalog.create_sequence_row(&sequence()).unwrap();
    a.begin_transaction().unwrap();
    catalog.save_model("private", "kept").unwrap();
    let token = a.retention_control().cancellation().clone();
    *wrapper.after_evaluation.lock() = Some(Box::new(move || token.cancel()));
    let result = reserve(&catalog);
    a.retention_control().cancellation().reset();
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(
        catalog.load_model("private").unwrap().as_deref(),
        Some("kept")
    );
    let row = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(row.current, 1);
    assert!(!row.called);
    assert_eq!(reserve(&catalog).unwrap().first_value, 1);
    a.commit_transaction().unwrap();
    assert_eq!(
        catalog.load_model("private").unwrap().as_deref(),
        Some("kept")
    );
}

#[test]
fn sequence_consumers_follow_independent_sessions_and_transaction_undo() {
    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 22));
    let b = a.open_session().unwrap();
    uqa_storage::key_value::conformance::verify_sequence_concurrency(&a, &b).unwrap();
    uqa_storage::key_value::conformance::verify_sequence_reopen(&a).unwrap();
}

#[test]
fn sequence_reservation_receipts_resolve_without_replaying_or_rewinding_a_later_block() {
    let persistence = Persistence::new();
    let a = Arc::new(persistence.session(1 << 22));
    let other = KeyValueCatalog::new(Arc::new(persistence.session(1 << 22)));
    other.save_schema("public").unwrap();
    other.create_sequence_row(&sequence()).unwrap();
    let wrapper = Arc::new(InterleavedStore::new(a.clone()));
    let catalog = KeyValueCatalog::new(wrapper.clone());
    persistence.state.lock().commit_fault = CommitFault::LoseReply;
    let error = reserve(&catalog).unwrap_err();
    let pending = a.pending_commit().unwrap();
    assert_eq!(
        error.commit_outcome(),
        Some(CommitErrorOutcome::Indeterminate(pending))
    );
    let reservation = reserve(&other).unwrap();
    assert_eq!((reservation.first_value, reservation.last_value), (4, 6));
    a.commit_transaction().unwrap();
    assert_eq!(
        wrapper
            .evaluations
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(catalog.load_sequence_rows().unwrap()[0].current, 6);
    assert_eq!(reserve(&catalog).unwrap().first_value, 7);
}

#[test]
fn sequence_name_publication_rejects_a_removed_destination_schema() {
    for rename in [false, true] {
        let persistence = Persistence::new();
        let a = Arc::new(persistence.session(1 << 22));
        let other = KeyValueCatalog::new(Arc::new(persistence.session(1 << 22)));
        other.save_schema("public").unwrap();
        other.save_schema("archive").unwrap();
        if rename {
            other.create_sequence_row(&sequence()).unwrap();
        }
        let wrapper = Arc::new(InterleavedStore::new(a.clone()));
        let catalog = KeyValueCatalog::new(wrapper.clone());
        *wrapper.after_evaluation.lock() = Some(Box::new(move || {
            other.drop_schema("archive").unwrap();
        }));
        let mut target = sequence();
        target.relation.schema = "archive".into();
        let result = if rename {
            catalog.rename_sequence_row("ids", "archive.ids")
        } else {
            catalog.create_sequence_row(&target)
        };
        let error = result.expect_err("the destination schema was removed before publication");
        let StorageBackendError::Backend { source, .. } = error else {
            panic!("expected schema dependency conflict: {error}");
        };
        assert!(matches!(
            source.downcast_ref::<VersionError>(),
            Some(VersionError::ReadConflict { .. })
        ));
        a.rollback_transaction().unwrap();
        let rows = catalog.load_sequence_rows().unwrap();
        assert!(rows.iter().all(|row| row.relation.schema == "public"));
        assert_eq!(rows.len(), usize::from(rename));
    }
}
