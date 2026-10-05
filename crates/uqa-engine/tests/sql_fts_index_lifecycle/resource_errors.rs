//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 Appendix A defines `out_of_memory` as `53200`. Exercise real storage rebuild failures, including statement undo, with one document instead of a large transaction.

use std::{path::Path, sync::Arc};

use rstest::rstest;
use uqa_engine::Engine;
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

fn open(path: &Path, backend: &str, options: VersionedSessionOptions) -> Engine {
    match backend {
        "native" => {
            let connection = ManagedConnection::open(path).unwrap();
            connection.bind_native_records(options).unwrap();
            Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection)))
        }
        "kv" => Engine::from_persistent_provider(Arc::new(
            SQLiteKeyValueStorage::open_with_options(path, options).unwrap(),
        )),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open_with_options(path, options).unwrap(),
        )),
        _ => unreachable!(),
    }
    .unwrap()
}

#[rstest]
#[case::create("", "CREATE INDEX docs_body ON docs USING gin (body)", "add_fts_field")]
#[case::analyzer(
    "CREATE INDEX docs_body ON docs USING gin (body); SELECT * FROM set_table_analyzer('docs', 'body', 'keyword')",
    "SELECT * FROM set_table_analyzer('docs', 'body', 'whitespace')",
    "set_table_analyzer"
)]
#[case::owner_release(
    "CREATE INDEX docs_body ON docs USING gin (body) WITH (analyzer = 'keyword'); CREATE INDEX docs_copy ON docs USING gin (body)",
    "DROP INDEX docs_body",
    "rebuild FTS analyzer"
)]
#[case::field_removal(
    "CREATE INDEX docs_body ON docs USING gin (body); CREATE INDEX docs_marker ON docs USING gin (marker)",
    "DROP INDEX docs_marker",
    "rebuild FTS index"
)]
fn fts_resource_errors_preserve_sqlstate_and_rollback(
    #[values("native", "kv", "redb")] backend: &str,
    #[case] setup: &str,
    #[case] failing: &str,
    #[case] context: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("text.db");
    let engine = open(&path, backend, VersionedSessionOptions::default());
    engine.sql("CREATE TABLE docs (id integer PRIMARY KEY, body text, marker text); INSERT INTO docs VALUES (1, repeat('payload ', 16384), 'kept')", &[]).unwrap();
    if !setup.is_empty() {
        engine.sql(setup, &[]).unwrap();
    }
    let before = engine
        .sql(
            "SELECT indexname FROM pg_indexes WHERE tablename = 'docs' ORDER BY indexname",
            &[],
        )
        .unwrap()
        .rows;
    let binding = engine.table_field_analyzer("docs", "body").unwrap();
    engine.close().unwrap();
    drop(engine);

    let engine = open(
        &path,
        backend,
        VersionedSessionOptions {
            retained_bytes: 512 << 10,
        },
    );
    engine.sql("BEGIN; SAVEPOINT before_rebuild", &[]).unwrap();
    let error = engine.sql(failing, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"), "{backend}: {error}");
    let message = error.to_string();
    assert!(
        message.contains(context),
        "failure must reach the index rebuild: {message}"
    );
    assert_eq!(message.matches(context).count(), 1, "{message}");
    engine
        .sql("ROLLBACK TO SAVEPOINT before_rebuild; COMMIT", &[])
        .unwrap();
    engine.close().unwrap();
    drop(engine);

    // Reopening also verifies that no failed schema, binding or posting replacement was published.
    let engine = open(&path, backend, VersionedSessionOptions::default());
    assert_eq!(
        engine
            .sql(
                "SELECT indexname FROM pg_indexes WHERE tablename = 'docs' ORDER BY indexname",
                &[]
            )
            .unwrap()
            .rows,
        before
    );
    assert_eq!(
        engine.table_field_analyzer("docs", "body").unwrap(),
        binding
    );
    assert_eq!(
        engine.sql("SELECT marker FROM docs", &[]).unwrap().rows[0]["marker"],
        uqa_core::Value::Str("kept".into())
    );
    if context == "rebuild FTS index" {
        assert_eq!(
            engine
                .sql("SELECT id FROM docs WHERE text_match(marker, 'kept')", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
    }
    engine.sql(failing, &[]).unwrap();
    engine.close().unwrap();
}
