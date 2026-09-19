//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage conflict diagnostics and cleanup reach the SQL transaction boundary.

use super::*;
use uqa_storage::{DocumentStore, KeyValueDocumentStore};

#[test]
fn rejected_record_commit_reports_serialization_and_preserves_the_winner() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        root.register_scalar_function_with_options(
            "conflict_probe",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_: &[Value]| {
                callback_calls.fetch_add(1, Ordering::AcqRel);
                Ok(Value::Int(20))
            },
        )
        .unwrap();
        root.sql(
            "CREATE TABLE items(id INTEGER, value INTEGER); INSERT INTO items VALUES (1, 10)",
            &[],
        )
        .unwrap();
        root.sql(
            "BEGIN; UPDATE items SET value = conflict_probe() WHERE id = 1",
            &[],
        )
        .unwrap();
        let peer: Arc<dyn KeyValueStore> = Arc::new(VersionedKeyValueStore::new(
            persistence.clone(),
            None,
            VersionedSessionOptions::default(),
        ));
        // A direct storage participant exercises final validation without taking Engine's SQL row lock. The SQL body has already evaluated its callback and must not be replayed after this independently committed replacement.
        let mut documents = KeyValueDocumentStore::new(peer, "public.items");
        let ids = documents.doc_ids().unwrap();
        assert_eq!(ids.len(), 1);
        let mut winner = documents.get_stored(ids[0]).unwrap().unwrap();
        winner.fields_mut().insert("value".into(), Value::Int(30));
        documents.put_stored(ids[0], winner).unwrap();
        assert_eq!(calls.load(Ordering::Acquire), 1);

        let error = root.sql("COMMIT", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("40001"), "{error}");
        assert_eq!(root.transaction_depth(), 0);
        assert!(root.pending_commit().is_none());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(
            root.sql("SELECT value FROM items", &[]).unwrap().rows[0]["value"],
            Value::Int(30)
        );
        root.sql("UPDATE items SET value = value + 1", &[]).unwrap();
        drop((root, documents));
        let reopened = engine(persistence);
        assert_eq!(
            reopened.sql("SELECT value FROM items", &[]).unwrap().rows[0]["value"],
            Value::Int(31)
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }
}

#[test]
fn rejected_commit_preserves_typed_diagnostics_and_cleans_up_without_replay() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        root.register_scalar_function_with_options(
            "rejection_probe",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_: &[Value]| {
                callback_calls.fetch_add(1, Ordering::AcqRel);
                Ok(Value::Int(1))
            },
        )
        .unwrap();
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        let observer = root.new_session().unwrap();
        for (fault, state) in [
            (REJECT_MEMORY, "53200"),
            (REJECT_CANCELLED, "57014"),
            (REJECT_CONSTRAINT, "23505"),
            (REJECT_OTHER, "XX000"),
            (REJECT_DEPENDENCY, "40001"),
        ] {
            root.sql("BEGIN; INSERT INTO items VALUES (rejection_probe())", &[])
                .unwrap();
            let evaluated = calls.load(Ordering::Acquire);
            *persistence.attempt.lock().unwrap() = None;
            persistence.fault.store(fault, Ordering::Release);
            let error = root.sql("COMMIT", &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{error}");
            if fault == REJECT_CANCELLED {
                assert!(matches!(error, SQLError::Cancelled(_)));
            }
            assert_eq!(root.transaction_depth(), 0);
            assert!(root.pending_commit().is_none());
            assert_eq!(calls.load(Ordering::Acquire), evaluated);
            persistence.fault.store(HEALTHY, Ordering::Release);
            assert_eq!(count(&observer, "items"), Value::Int(0));
            observer.sql("INSERT INTO items VALUES (99)", &[]).unwrap();
            assert_eq!(count(&root, "items"), Value::Int(1));
            root.sql("DELETE FROM items", &[]).unwrap();
        }
        drop((root, observer));
        assert_eq!(count(&engine(persistence), "items"), Value::Int(0));
    }
}

#[test]
fn direct_document_observation_preserves_storage_diagnostics_and_rolls_back_the_write() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql(
            "CREATE TABLE items(body text); CREATE INDEX body_idx ON items USING gin(body)",
            &[],
        )
        .unwrap();
        let observer = root.new_session().unwrap();
        for (fault, state) in [
            (REJECT_MEMORY, "53200"),
            (REJECT_CANCELLED, "57014"),
            (REJECT_DEPENDENCY, "40001"),
        ] {
            persistence.identifier_fault.store(fault, Ordering::Release);
            let error = root
                .add_document(
                    "items",
                    17,
                    [("body".into(), Value::Str("rejected".into()))].into(),
                )
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{error}");
            if fault == REJECT_CANCELLED {
                assert!(matches!(error, SQLError::Cancelled(_)));
            }
            assert_eq!(
                persistence.identifier_fault.load(Ordering::Acquire),
                HEALTHY
            );
            assert_eq!(root.transaction_depth(), 0);
            assert!(root.pending_commit().is_none());
            assert_eq!(count(&observer, "items"), Value::Int(0));
            for session in [&root, &observer] {
                let stats = session.fts_index_stats(Some("items")).unwrap();
                assert_eq!(stats.len(), 1);
                assert_eq!(stats[0].posting_count, 0);
                assert_eq!(stats[0].indexed_doc_count, 0);
                assert_eq!(stats[0].total_field_length, 0);
            }
            root.add_document(
                "items",
                17,
                [("body".into(), Value::Str("accepted".into()))].into(),
            )
            .unwrap();
            assert_eq!(count(&observer, "items"), Value::Int(1));
            let stats = observer.fts_index_stats(Some("items")).unwrap();
            assert_eq!(stats[0].posting_count, 1);
            assert_eq!(stats[0].indexed_doc_count, 1);
            assert_eq!(stats[0].total_field_length, 1);
            root.sql("DELETE FROM items", &[]).unwrap();
        }
        drop((root, observer));
        assert_eq!(count(&engine(persistence), "items"), Value::Int(0));
    }
}

#[test]
fn uncertain_commit_keeps_its_outcome_even_when_the_source_is_a_conflict() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        root.sql("BEGIN; INSERT INTO items VALUES (1)", &[])
            .unwrap();
        persistence
            .fault
            .store(LOSE_CONFLICT_REPLY, Ordering::Release);
        assert_unknown(&root.sql("COMMIT", &[]).unwrap_err());
        let pending = root.pending_commit().unwrap();
        assert_eq!(root.transaction_depth(), 1);
        assert_unknown(&root.sql("COMMIT", &[]).unwrap_err());
        assert_eq!(root.pending_commit(), Some(pending));
        assert_eq!(persistence.aborts.load(Ordering::Acquire), 0);
        persistence.fault.store(HEALTHY, Ordering::Release);
        root.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(root.transaction_depth(), 0);
        assert!(root.pending_commit().is_none());
        assert_eq!(count(&root, "items"), Value::Int(0));
    }
}
