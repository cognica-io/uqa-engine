//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Upgrade restoration retains old analysis for indexed documents and future writes.

use super::{execute, fixture, hits, Arc, Engine, Path, RedbStorage, TempDir};
use uqa_storage::PersistentStorageProvider;
use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

const HISTORICAL: &str =
    include_str!("../../../../uqa-analysis/tests/fixtures/unicode-17/keyword.json");
const CONFIG: &str = r#"{"tokenizer":{"type":"keyword"},"token_filters":[{"type":"lowercase"}]}"#;

fn provider(kind: usize, path: &Path) -> Arc<dyn PersistentStorageProvider> {
    match kind {
        0 => Arc::new(SQLiteStorageProvider::new(
            ManagedConnection::open(path).unwrap(),
        )),
        1 => Arc::new(SQLiteKeyValueStorage::open(path).unwrap()),
        2 => Arc::new(RedbStorage::open(path).unwrap()),
        _ => unreachable!(),
    }
}

#[test]
fn persisted_unicode_17_reopens_with_original_bindings_postings_and_new_writes() {
    for kind in 0..3 {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("retained.db");
        {
            let provider = provider(kind, &path);
            let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
            fixture(&engine);
            drop(engine);
            // Install the exact descriptor captured from the previous implementation.
            let session = provider.open_session().unwrap();
            session.backend.begin_transaction().unwrap();
            session
                .catalog
                .save_analyzer_revision(
                    "historical",
                    &serde_json::from_str::<serde_json::Value>(HISTORICAL).unwrap()["descriptor"]
                        ["pipeline"]
                        .to_string(),
                    HISTORICAL,
                )
                .unwrap();
            session.backend.commit_transaction().unwrap();
        }
        let binding;
        {
            let provider = provider(kind, &path);
            let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
            engine
                .set_table_field_analyzer("docs", "body", "historical", "both")
                .unwrap();
            execute(&engine, "INSERT INTO docs VALUES (1, '꟎')");
            assert_eq!(hits(&engine, "docs", "body", "꟏"), [1], "provider {kind}");
            // Changing the mutable name must not replace the field's retained profile.
            engine
                .register_named_analyzer("historical", CONFIG)
                .unwrap();
            binding = provider
                .open_session()
                .unwrap()
                .catalog
                .load_table_field_analyzer_bindings()
                .unwrap();
            assert!(binding[0]
                .2
                .contains("7e7596acb0d4ebb8c93d203d023325e944ede71b631041e9999c44ae1d77637c"));
        }
        let provider = provider(kind, &path);
        let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
        assert_eq!(
            provider
                .open_session()
                .unwrap()
                .catalog
                .load_table_field_analyzer_bindings()
                .unwrap(),
            binding
        );
        assert_eq!(hits(&engine, "docs", "body", "꟏"), [1]);
        execute(&engine, "INSERT INTO docs VALUES (2, '꟎')");
        assert_eq!(hits(&engine, "docs", "body", "꟏"), [1, 2]);
        execute(&engine, "BEGIN");
        execute(&engine, "INSERT INTO docs VALUES (3, '꟎')");
        execute(&engine, "ROLLBACK");
        assert_eq!(hits(&engine, "docs", "body", "꟎"), [1, 2]);
        assert_eq!(
            provider
                .open_session()
                .unwrap()
                .catalog
                .load_table_field_analyzer_bindings()
                .unwrap(),
            binding
        );
    }
}
