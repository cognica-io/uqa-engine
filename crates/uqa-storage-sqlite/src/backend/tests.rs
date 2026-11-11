//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use uqa_analysis::analyzer::standard_analyzer;
use uqa_core::Value;
use uqa_storage::StorageBackendError;

use super::*;
use crate::{Catalog, SQLiteError};

#[test]
fn session_factories_preserve_a_bound_native_sessions_retention_options() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions {
            retained_bytes: 8 << 20,
        })
        .unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    let initial = provider.open_initial_session().unwrap();
    let sibling = provider.open_session().unwrap();
    initial.validate_transaction_affinity().unwrap();
    sibling.validate_transaction_affinity().unwrap();
    assert!(initial.backend.transaction_model().is_versioned());
    assert_eq!(
        initial.backend.transaction_model(),
        sibling.backend.transaction_model()
    );
    assert_ne!(
        initial.backend.transaction_affinity(),
        sibling.backend.transaction_affinity()
    );
}

#[test]
fn session_factory_reads_current_catalog_while_a_sibling_holds_a_writer_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open_compressed(
        &directory.path().join("session-writer-reservation.db"),
        crate::SQLiteCompressionOptions::default(),
    )
    .unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    let writer = provider.open_session().unwrap();
    writer.backend.begin_transaction().unwrap();
    writer
        .catalog
        .set_metadata("private-write", "uncommitted")
        .unwrap();

    let reader = provider.open_session().unwrap();
    assert_eq!(reader.catalog.get_metadata("private-write").unwrap(), None);
    writer.backend.commit_transaction().unwrap();
    assert_eq!(
        reader
            .catalog
            .get_metadata("private-write")
            .unwrap()
            .as_deref(),
        Some("uncommitted")
    );
}

#[test]
fn sqlite_backend_builds_document_index_and_vector_stores() {
    let conn = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(conn.clone()).unwrap();
    let backend = SQLiteStorageBackend::new(conn);

    let mut doc = BTreeMap::new();
    doc.insert("title".to_string(), Value::Str("rust storage".into()));
    let mut docs = backend.document_store("articles");
    docs.put(1, doc).unwrap();
    assert_eq!(
        docs.get_field(1, "title").unwrap(),
        Some(Value::Str("rust storage".into()))
    );

    let mut inv = backend.inverted_index("articles", standard_analyzer("english"));
    inv.add_document(
        1,
        BTreeMap::from([("title".to_string(), "rust storage".to_string())]),
    )
    .unwrap();
    assert_eq!(inv.doc_freq("title", "rust").unwrap(), 1);

    let mut vectors = backend
        .vector_index(
            "articles",
            "embedding",
            2,
            VectorIndexSpec::IVF(uqa_storage::IVFIndexParams {
                nlist: 2,
                nprobe: 1,
                train_threshold: 2,
            }),
            VectorIndexOpenMode::Create,
        )
        .unwrap();
    vectors.add(1, vec![1.0, 0.0]).unwrap();
    let hits = vectors.search_knn(&[1.0, 0.0], 1).unwrap();
    assert_eq!(hits.entries().len(), 1);
    assert_eq!(hits.entries()[0].doc_id, 1);
}

#[test]
fn sqlite_backend_transaction_rolls_back_cross_store_writes() {
    let conn = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(conn.clone()).unwrap();
    let backend = SQLiteStorageBackend::new(conn);
    let mut docs = backend.document_store("articles");
    let mut inv = backend.inverted_index("articles", standard_analyzer("english"));

    backend.begin_transaction().unwrap();
    docs.put(
        1,
        BTreeMap::from([("title".to_string(), Value::Str("rollback".into()))]),
    )
    .unwrap();
    inv.add_document(
        1,
        BTreeMap::from([("title".to_string(), "rollback".to_string())]),
    )
    .unwrap();
    backend.rollback_transaction().unwrap();

    assert_eq!(docs.len().unwrap(), 0);
    assert_eq!(inv.doc_freq("title", "rollback").unwrap(), 0);
}

#[test]
fn sqlite_sessions_isolate_and_atomically_commit_cross_store_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cross-store-isolation.sqlite3");
    let conn = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(conn.clone()).unwrap();
    let writer = SQLiteStorageBackend::new(conn.clone());
    let observer_conn = conn.new_session();
    let observer_catalog = Catalog::open(observer_conn.clone()).unwrap();
    let observer = SQLiteStorageBackend::new(observer_conn);

    let mut writer_docs = writer.document_store("articles");
    let mut writer_inv = writer.inverted_index("articles", standard_analyzer("english"));
    let mut writer_vectors = writer
        .vector_index(
            "articles",
            "embedding",
            2,
            VectorIndexSpec::BruteForce,
            VectorIndexOpenMode::Create,
        )
        .unwrap();
    let observer_docs = observer.document_store("articles");
    let observer_inv = observer.inverted_index("articles", standard_analyzer("english"));
    let observer_vectors = observer
        .vector_index(
            "articles",
            "embedding",
            2,
            VectorIndexSpec::BruteForce,
            VectorIndexOpenMode::Restore,
        )
        .unwrap();

    writer.begin_transaction().unwrap();
    writer_docs
        .put(
            1,
            BTreeMap::from([("title".to_string(), Value::Str("atomic rust".into()))]),
        )
        .unwrap();
    writer_inv
        .add_document(
            1,
            BTreeMap::from([("title".to_string(), "atomic rust".to_string())]),
        )
        .unwrap();
    writer_vectors.add(1, vec![1.0, 0.0]).unwrap();
    catalog
        .save_scoring_params("transactional", r#"{"alpha":1.0}"#)
        .unwrap();

    assert_eq!(writer_docs.len().unwrap(), 1);
    assert_eq!(writer_inv.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(writer_vectors.count().unwrap(), 1);
    assert!(catalog
        .load_scoring_params("transactional")
        .unwrap()
        .is_some());

    assert_eq!(observer_docs.len().unwrap(), 0);
    assert_eq!(observer_inv.doc_freq("title", "rust").unwrap(), 0);
    assert_eq!(observer_vectors.count().unwrap(), 0);
    assert!(observer_catalog
        .load_scoring_params("transactional")
        .unwrap()
        .is_none());

    writer.commit_transaction().unwrap();
    assert_eq!(observer_docs.len().unwrap(), 1);
    assert_eq!(observer_inv.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(observer_vectors.count().unwrap(), 1);
    assert!(observer_catalog
        .load_scoring_params("transactional")
        .unwrap()
        .is_some());
}

#[test]
fn ignored_legacy_index_error_cannot_commit_partial_document_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ignored-index-error.sqlite3");
    let conn = ManagedConnection::open(&path).unwrap();
    let _catalog = Catalog::open(conn.clone()).unwrap();
    let backend = SQLiteStorageBackend::new(conn.clone());
    let observer = conn.new_session();
    let mut docs = backend.document_store("articles");
    let mut vectors = backend
        .vector_index(
            "articles",
            "embedding",
            2,
            VectorIndexSpec::BruteForce,
            VectorIndexOpenMode::Create,
        )
        .unwrap();

    backend.begin_transaction().unwrap();
    docs.put(
        1,
        BTreeMap::from([("title".to_string(), Value::Str("must roll back".into()))]),
    )
    .unwrap();
    conn.with(|connection| {
        connection.execute("DROP TABLE _vectors", [])?;
        Ok(())
    })
    .unwrap();
    // The vector write reports its error directly. Even if a caller
    // ignores that Result, the managed transaction is poisoned and the
    // partial document write cannot commit.
    let ignored = vectors.add(1, vec![1.0, 0.0]);
    assert!(ignored.is_err());
    assert!(matches!(
        backend.commit_transaction(),
        Err(StorageBackendError::Backend { source, .. })
            if matches!(source.downcast_ref::<SQLiteError>(), Some(SQLiteError::TransactionAborted(_)))
    ));

    let stored_docs: i64 = observer
        .with(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM _documents WHERE table_name = 'articles'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let vector_table_exists: i64 = observer
        .with(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_vectors'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(stored_docs, 0);
    assert_eq!(vector_table_exists, 1);
}
