//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use rusqlite::params;
use tempfile::tempdir;

use super::SQLiteHNSWIndex;
use crate::sqlite::catalog::Catalog;
use crate::sqlite::vector_index::codec::vector_to_blob;
use crate::sqlite::ManagedConnection;
use crate::vector_index::{HNSWIndexParams, VectorIndex};

fn params() -> HNSWIndexParams {
    HNSWIndexParams {
        m: 4,
        ef_construction: 24,
        ef_search: 24,
        rebuild_threshold: 16,
        seed: 7,
    }
}

fn initialized_index() -> (ManagedConnection, SQLiteHNSWIndex) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(connection.clone()).unwrap();
    let mut index =
        SQLiteHNSWIndex::with_params(connection.clone(), "articles", "embedding", 2, params());
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    index.initialize().unwrap();
    (connection, index)
}

#[test]
fn create_backfill_uses_exact_search_until_graph_initialization() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteHNSWIndex::with_params(connection, "articles", "embedding", 2, params());
    index.add(1, vec![1.0, 0.0]).unwrap();
    assert_eq!(
        index
            .search_knn(&[1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
    index.initialize().unwrap();
    assert!(index.persisted_revision().unwrap().is_some());
}

#[test]
fn graph_persists_and_reopens_without_rebuilding() {
    let (connection, index) = initialized_index();
    let expected = index
        .search_knn(&[1.0, 0.0], 2)
        .unwrap()
        .doc_ids()
        .collect::<Vec<_>>();
    let revision = index.persisted_revision().unwrap().unwrap();
    let reopened =
        SQLiteHNSWIndex::open_existing(connection.clone(), "articles", "embedding", 2, params());
    assert_eq!(
        reopened
            .search_knn(&[1.0, 0.0], 2)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(reopened.persisted_revision().unwrap(), Some(revision));
    let node_count: i64 = connection
        .with(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM _hnsw_nodes
                  WHERE table_name = 'articles' AND field = 'embedding'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(node_count, 2);
}

#[test]
fn repeated_vectors_build_a_connected_persistent_graph() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(connection.clone()).unwrap();
    let params = HNSWIndexParams::default();
    let mut index =
        SQLiteHNSWIndex::with_params(connection.clone(), "messages", "embedding", 2, params);
    for doc_id in 1_u64..=2_000 {
        let vector = if doc_id % 200 == 1 {
            vec![0.9, 0.1]
        } else {
            vec![0.1, 0.9]
        };
        index.add(doc_id, vector).unwrap();
    }
    index.initialize().unwrap();

    let reopened = SQLiteHNSWIndex::open_existing(connection, "messages", "embedding", 2, params);
    assert_eq!(reopened.search_knn(&[0.9, 0.1], 10).unwrap().len(), 10);
}

#[test]
fn cached_generation_observes_committed_revision_changes() {
    let (connection, mut writer) = initialized_index();
    let observer = SQLiteHNSWIndex::open_existing(connection, "articles", "embedding", 2, params());
    observer.search_knn(&[-1.0, 0.0], 1).unwrap();
    writer.add(3, vec![-1.0, 0.0]).unwrap();
    assert_eq!(
        observer
            .search_knn(&[-1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![3]
    );
}

#[test]
fn independent_file_session_observes_committed_revision_changes() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("hnsw-revisions.db");
    let writer_connection = ManagedConnection::open(&database).unwrap();
    let _catalog = Catalog::open(writer_connection.clone()).unwrap();
    let mut writer = SQLiteHNSWIndex::with_params(
        writer_connection.clone(),
        "articles",
        "embedding",
        2,
        params(),
    );
    writer.add(1, vec![1.0, 0.0]).unwrap();
    writer.add(2, vec![0.0, 1.0]).unwrap();
    writer.initialize().unwrap();
    let observer = SQLiteHNSWIndex::open_existing(
        writer_connection.new_session(),
        "articles",
        "embedding",
        2,
        params(),
    );
    observer.search_knn(&[-1.0, 0.0], 1).unwrap();
    writer.add(3, vec![-1.0, 0.0]).unwrap();
    assert_eq!(
        observer
            .search_knn(&[-1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![3]
    );
}

#[test]
fn rollback_invalidates_the_published_session_generation() {
    let (connection, mut index) = initialized_index();
    connection.begin_transaction().unwrap();
    index.add(3, vec![-1.0, 0.0]).unwrap();
    assert_eq!(index.count().unwrap(), 3);
    connection.rollback_transaction().unwrap();
    assert_eq!(index.count().unwrap(), 2);
    assert!(!index
        .search_knn(&[-1.0, 0.0], 2)
        .unwrap()
        .doc_ids()
        .any(|doc_id| doc_id == 3));
}

#[test]
fn metadata_failure_rolls_back_vector_and_graph_changes() {
    let (connection, mut index) = initialized_index();
    connection
        .with(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_hnsw_metadata
                 BEFORE INSERT ON _hnsw_indexes
                 BEGIN
                     SELECT RAISE(ABORT, 'injected HNSW metadata failure');
                 END;",
            )?;
            Ok(())
        })
        .unwrap();
    let error = index.add(3, vec![-1.0, 0.0]).unwrap_err();
    assert!(error.to_string().contains("injected HNSW metadata failure"));
    assert_eq!(index.count().unwrap(), 2);
    assert!(!index
        .search_threshold(&[-1.0, 0.0], 0.999)
        .unwrap()
        .doc_ids()
        .any(|doc_id| doc_id == 3));
}

#[test]
fn restore_rejects_missing_and_corrupt_graph_metadata() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let _catalog = Catalog::open(connection.clone()).unwrap();
    let missing =
        SQLiteHNSWIndex::open_existing(connection.clone(), "articles", "embedding", 2, params());
    assert!(missing.search_knn(&[1.0, 0.0], 1).is_err());

    let (_, initialized) = initialized_index();
    initialized
        .persistent
        .conn
        .with(|conn| {
            conn.execute(
                "INSERT INTO _hnsw_edges
                    (table_name, field, source_node_id, layer, target_node_id)
                 VALUES ('articles', 'embedding', 999, 0, 1)",
                params![],
            )?;
            Ok(())
        })
        .unwrap();
    let corrupt = SQLiteHNSWIndex::open_existing(
        initialized.persistent.conn.clone(),
        "articles",
        "embedding",
        2,
        params(),
    );
    assert!(corrupt
        .search_knn(&[1.0, 0.0], 1)
        .unwrap_err()
        .to_string()
        .contains("edge source 999"));
}

#[test]
fn restore_rejects_an_edge_layer_before_unbounded_allocation() {
    let (connection, initialized) = initialized_index();
    connection
        .with(|conn| {
            conn.execute(
                "INSERT INTO _hnsw_edges
                    (table_name, field, source_node_id, layer, target_node_id)
                 VALUES ('articles', 'embedding', 1, ?1, 2)",
                params![i64::MAX],
            )?;
            Ok(())
        })
        .unwrap();
    let restored = SQLiteHNSWIndex::open_existing(
        initialized.persistent.conn.clone(),
        "articles",
        "embedding",
        2,
        params(),
    );
    let error = restored.search_knn(&[1.0, 0.0], 1).unwrap_err();
    assert!(error.to_string().contains("layer"));
}

#[test]
fn restore_rejects_drift_from_canonical_raw_vectors() {
    let (connection, initialized) = initialized_index();
    connection
        .with(|conn| {
            conn.execute(
                "UPDATE _vectors SET vector = ?1
                  WHERE table_name = 'articles' AND field = 'embedding'
                    AND doc_id = 1 AND vector_ordinal = 0",
                params![vector_to_blob(&[0.5, 0.5]).unwrap()],
            )?;
            Ok(())
        })
        .unwrap();
    let restored = SQLiteHNSWIndex::open_existing(
        initialized.persistent.conn.clone(),
        "articles",
        "embedding",
        2,
        params(),
    );
    let error = restored.search_knn(&[1.0, 0.0], 1).unwrap_err();
    assert!(error
        .to_string()
        .contains("differs from its live graph node"));
}
