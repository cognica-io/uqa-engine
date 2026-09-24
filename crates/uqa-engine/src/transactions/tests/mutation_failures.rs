//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed direct mutations cannot publish private records without their logical intents.

use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, Arc},
};

use super::{integer_column, Engine, SQLError, StorageBackendError};
use crate::Value;

mod index;

pub(super) fn persistent_engine(provider: usize, path: &std::path::Path) -> Engine {
    let engine = match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine
}

fn apply_text_mutation(engine: &Engine, operation: &str) -> Result<(), SQLError> {
    match operation {
        "insert" => engine.add_document(
            "texts",
            3,
            [
                ("id".into(), Value::Int(3)),
                ("body".into(), Value::Str("changed".into())),
            ]
            .into_iter()
            .collect(),
        ),
        "update" => engine
            .update_document_fields(
                "texts",
                1,
                [("body".into(), Value::Str("changed".into()))]
                    .into_iter()
                    .collect(),
                BTreeMap::new(),
            )
            .map(|_| ()),
        "delete" => engine.delete_document("texts", 1),
        _ => unreachable!(),
    }
}

#[test]
fn direct_text_errors_after_private_mutation_cannot_commit_incomplete_intents() {
    for provider in 0..3 {
        for savepoint in [false, true] {
            for operation in ["insert", "update", "delete"] {
                let directory = tempfile::tempdir().unwrap();
                let engine = persistent_engine(provider, &directory.path().join("mutation.db"));
                engine.sql("CREATE TABLE texts (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX texts_gin ON texts USING gin (body); INSERT INTO texts VALUES (1, 'original')", &[]).unwrap();
                let peer = engine.new_session().unwrap();
                peer.release_automatic_statistics_client();
                peer.session
                    .statistics_worker
                    .store(true, Ordering::Release);
                engine
                    .sql("BEGIN ISOLATION LEVEL SERIALIZABLE", &[])
                    .unwrap();
                engine
                    .sql("INSERT INTO texts VALUES (2, 'prior')", &[])
                    .unwrap();
                if savepoint {
                    engine.savepoint("keep_prior").unwrap();
                }
                peer.sql("INSERT INTO texts VALUES (10, 'peer')", &[])
                    .unwrap();

                let table = engine.require_table("texts").unwrap();
                let native = engine
                    .storage
                    .backend
                    .as_ref()
                    .unwrap()
                    .inverted_index("public.texts", table.analyzer.read().clone());
                let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
                *table.inverted_index.write() = Box::new(index::CancelAfterMutation {
                    index: native,
                    cancellation: engine.runtime.cancellation.clone(),
                    fired: Arc::clone(&fired),
                });
                let result = apply_text_mutation(&engine, operation);
                assert!(fired.load(Ordering::Acquire), "{provider}: {operation}");
                assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
                assert!(engine.transaction_failed());
                engine.reset_cancellation();
                assert_eq!(
                    engine
                        .sql("SELECT id FROM texts", &[])
                        .unwrap_err()
                        .sqlstate(),
                    Some("25P02")
                );
                if savepoint {
                    engine.rollback_to_savepoint("keep_prior").unwrap();
                    engine.release_savepoint("keep_prior").unwrap();
                }
                // COMMIT of an unrecovered failed frame must perform rollback, never publish its partially observed mutation.
                engine.commit().unwrap();
                let expected = if savepoint {
                    vec![1, 2, 10]
                } else {
                    vec![1, 10]
                };
                assert_eq!(
                    integer_column(
                        &peer.sql("SELECT id FROM texts ORDER BY id", &[]).unwrap(),
                        "id"
                    ),
                    expected
                );
                for (term, expected) in [
                    ("original", 1),
                    ("changed", 0),
                    ("prior", usize::from(savepoint)),
                    ("peer", 1),
                ] {
                    assert_eq!(
                        peer.search("texts", "body", term, &crate::ScoringMode::default(), 10)
                            .unwrap()
                            .len(),
                        expected,
                        "{provider}: {operation}: {term}"
                    );
                }
            }
        }
    }
}

fn mutation_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "40001".into(),
        message: "injected mutation failure".into(),
    }
}

fn reject_after_write(engine: &Engine, panic: bool) -> Result<(), SQLError> {
    engine.sql("INSERT INTO mutations VALUES (2)", &[])?;
    assert!(!panic, "mutation callback panic");
    Err(mutation_error())
}

#[test]
fn direct_mutation_error_families_abort_only_their_active_nested_frame() {
    for route in [
        "row",
        "sql",
        "definition",
        "mapped",
        "storage",
        "maintenance",
        "string",
    ] {
        for panic in [false, true] {
            let engine = Engine::new();
            engine
                .sql("CREATE TABLE mutations (id INTEGER PRIMARY KEY)", &[])
                .unwrap();
            engine.begin().unwrap();
            engine.sql("INSERT INTO mutations VALUES (1)", &[]).unwrap();
            engine.begin().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match route {
                "row" => engine
                    .with_implicit_row_write_transaction(
                        "mutations",
                        2,
                        uqa_sql::ast::LockStrength::ForUpdate,
                        |e| reject_after_write(e, panic),
                    )
                    .map_err(|error| {
                        assert_eq!(error.sqlstate(), Some("40001"));
                        error.to_string()
                    }),
                "sql" => engine
                    .with_implicit_transaction(|e| reject_after_write(e, panic))
                    .map_err(|error| {
                        assert_eq!(error.sqlstate(), Some("40001"));
                        error.to_string()
                    }),
                "definition" => engine
                    .with_implicit_definition_transaction(|e| reject_after_write(e, panic))
                    .map_err(|error| {
                        assert_eq!(error.sqlstate(), Some("40001"));
                        error.to_string()
                    }),
                "mapped" => engine
                    .with_implicit_mapped_transaction(
                        |e| reject_after_write(e, panic),
                        std::convert::identity,
                    )
                    .map_err(|error| {
                        assert_eq!(error.sqlstate(), Some("40001"));
                        error.to_string()
                    }),
                "storage" | "maintenance" => {
                    let write = |e: &Engine| {
                        reject_after_write(e, panic)
                            .map_err(|error| StorageBackendError::backend("injected", error))
                    };
                    let result = if route == "storage" {
                        engine.with_implicit_storage_transaction(write)
                    } else {
                        engine.with_storage_maintenance_scope(write)
                    };
                    result.map_err(|error| {
                        assert_eq!(
                            Engine::storage_tx_error("test mutation", &error).sqlstate(),
                            Some("40001")
                        );
                        error.to_string()
                    })
                }
                "string" => engine.with_implicit_string_transaction(|e| {
                    reject_after_write(e, panic).map_err(|error| error.to_string())
                }),
                _ => unreachable!(),
            }));
            if panic {
                let payload = result.unwrap_err();
                assert_eq!(
                    payload.downcast_ref::<&str>(),
                    Some(&"mutation callback panic")
                );
            } else {
                assert!(result
                    .unwrap()
                    .unwrap_err()
                    .contains("injected mutation failure"));
            }
            assert!(engine.transaction_failed(), "{route}");
            engine.commit().unwrap();
            assert_eq!(engine.transaction_depth(), 1);
            assert!(!engine.transaction_failed());
            engine.commit().unwrap();
            assert_eq!(
                integer_column(&engine.sql("SELECT id FROM mutations", &[]).unwrap(), "id"),
                [1]
            );
        }
    }
}
