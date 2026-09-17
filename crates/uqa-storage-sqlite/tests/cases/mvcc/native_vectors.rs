//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact, IVF and HNSW public APIs retain canonical/derived views and publish atomic native batches.

#[path = "native_vectors/conflicts.rs"]
mod conflicts;
#[path = "native_vectors/ivf.rs"]
mod ivf;
#[path = "native_vectors/ivf_merging.rs"]
mod ivf_merging;

use std::{sync::mpsc, time::Duration};

use super::{native_tables::schema, open, MODES};
use uqa_storage::{
    mvcc::VersionedSessionOptions, read_control::StorageReadControl, vector_index::VectorIndex,
};
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteHNSWIndex, SQLiteIVFIndex, SQLiteRecordStore,
    SQLiteVectorIndex,
};

const X: [f32; 3] = [1.0, 0.0, 0.0];
const Y: [f32; 3] = [0.0, 1.0, 0.0];
const Z: [f32; 3] = [0.0, 0.0, 1.0];

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IndexKind {
    Exact,
    Ivf,
    Hnsw,
}
const KINDS: [IndexKind; 3] = [IndexKind::Exact, IndexKind::Ivf, IndexKind::Hnsw];

fn index(connection: &ManagedConnection, kind: IndexKind, table: &str) -> Box<dyn VectorIndex> {
    match kind {
        IndexKind::Exact => Box::new(SQLiteVectorIndex::new(
            connection.clone(),
            table,
            "embedding",
            3,
        )),
        IndexKind::Ivf => Box::new(SQLiteIVFIndex::with_params(
            connection.clone(),
            table,
            "embedding",
            3,
            2,
            2,
            2,
        )),
        IndexKind::Hnsw => Box::new(SQLiteHNSWIndex::new(
            connection.clone(),
            table,
            "embedding",
            3,
        )),
    }
}

type Generation = Vec<Vec<Vec<rusqlite::types::Value>>>;
pub(super) fn generation(
    connection: &ManagedConnection,
    kind: IndexKind,
    table: &str,
    field: &str,
) -> Generation {
    connection
        .with_physical(|sqlite| {
            let mut generation = Vec::new();
            let families: &[(&str, &str)] = match kind {
                IndexKind::Exact => &[("_vectors", "doc_id,vector_ordinal")],
                IndexKind::Ivf => &[
                    ("_vectors", "doc_id,vector_ordinal"),
                    ("_ivf_indexes", "field"),
                    ("_ivf_centroids", "centroid_id"),
                    ("_ivf_assignments", "doc_id,vector_ordinal"),
                ],
                IndexKind::Hnsw => &[
                    ("_vectors", "doc_id,vector_ordinal"),
                    ("_hnsw_indexes", "field"),
                    ("_hnsw_nodes", "node_id"),
                    ("_hnsw_edges", "source_node_id,layer,target_node_id"),
                ],
            };
            for (family, order) in families {
                let mut query = sqlite.prepare(&format!(
                    "SELECT * FROM {family} WHERE table_name = ?1 AND field = ?2 ORDER BY {order}"
                ))?;
                let columns = query.column_count();
                generation.push(
                    query
                        .query_map([table, field], |row| {
                            (0..columns)
                                .map(|column| row.get(column))
                                .collect::<rusqlite::Result<Vec<_>>>()
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            Ok(generation)
        })
        .unwrap()
}

fn ids(index: &dyn VectorIndex, query: &[f32]) -> Vec<u64> {
    index
        .search_threshold(query, 0.99)
        .unwrap()
        .doc_ids()
        .collect()
}

fn nearest(index: &dyn VectorIndex, query: &[f32]) -> Vec<u64> {
    index.search_knn(query, 1).unwrap().doc_ids().collect()
}

#[test]
fn native_exact_writers_commit_independent_tensors_before_the_other_transaction_finishes() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("vectors.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            // Handles predating native binding also join the selected logical session.
            let mut a = index(&connection, IndexKind::Exact, "docs");
            a.add(1, X.to_vec()).unwrap();
            a.add(2, Y.to_vec()).unwrap();
            bind(&connection);
            let baseline = a.snapshot().unwrap();
            connection.begin_transaction().unwrap();
            a.add_many(1, vec![Y.to_vec(), Z.to_vec()]).unwrap();
            connection.savepoint("keep").unwrap();
            let first = a.snapshot().unwrap();
            a.add_many(1, vec![Z.to_vec(), X.to_vec()]).unwrap();
            let second = a.snapshot().unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let other = open(mode, &other_path);
                bind(&other);
                other.begin_transaction().unwrap();
                let mut b = index(&other, IndexKind::Exact, "docs");
                b.add_many(2, vec![X.to_vec(), Y.to_vec()]).unwrap();
                other.commit_transaction().unwrap();
                sent.send(ids(&*b, &X)).unwrap();
            });
            assert_eq!(
                received.recv_timeout(Duration::from_secs(20)).unwrap(),
                vec![1, 2]
            );
            writer.join().unwrap();
            assert_eq!(ids(&*a, &X), vec![1]);
            match ending {
                "commit" => connection.commit_transaction().unwrap(),
                "rollback" => connection.rollback_transaction().unwrap(),
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                }
            }
            let expected = if ending == "savepoint" {
                vec![2]
            } else {
                vec![1, 2]
            };
            assert_eq!(ids(&*a, &X), expected);
            assert_eq!(ids(&*baseline, &X), vec![1]);
            assert!(ids(&*first, &X).is_empty());
            assert_eq!(ids(&*second, &X), vec![1]);
            assert_eq!(baseline.count().unwrap(), 2);
            assert_eq!(second.count().unwrap(), 3);
            drop((a, baseline, first, second, connection));
            let reopened = open(mode, &path);
            bind(&reopened);
            let restored = index(&reopened, IndexKind::Exact, "docs");
            assert_eq!(ids(&*restored, &X), expected);
            assert_eq!(
                restored.count().unwrap(),
                if ending == "rollback" { 3 } else { 4 }
            );
        }
    }
}

#[test]
fn native_vector_snapshots_keep_private_and_committed_generations_through_lifecycle() {
    for mode in MODES {
        for kind in KINDS {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("lifecycle.db");
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog.save_table(&schema("docs", 17, 18)).unwrap();
            let mut vectors = index(&connection, kind, "public.docs");
            vectors.add_many(1, vec![X.to_vec(), Y.to_vec()]).unwrap();
            vectors.add(2, Z.to_vec()).unwrap();
            vectors.initialize().unwrap();
            bind(&connection);
            let old = vectors.snapshot().unwrap();
            let other = connection.new_session();
            let observer = index(&other, kind, "public.docs");
            connection.begin_transaction().unwrap();
            vectors.add_many(1, vec![Z.to_vec()]).unwrap();
            connection.savepoint("changed").unwrap();
            let changed = vectors.snapshot().unwrap();
            vectors.clear().unwrap();
            assert_eq!(vectors.count().unwrap(), 0);
            assert_eq!(observer.count().unwrap(), 3);
            connection.rollback_to_savepoint("changed").unwrap();
            assert_eq!(vectors.count().unwrap(), 2);
            vectors.delete(2).unwrap();
            connection.commit_transaction().unwrap();
            assert_eq!(observer.count().unwrap(), 1);
            assert_eq!(old.count().unwrap(), 3);
            assert_eq!(changed.count().unwrap(), 2);
            assert_eq!(nearest(&*old, &X), vec![1]);
            assert_eq!(ids(&*changed, &Z), vec![1, 2]);
            catalog
                .rename_table_data("public.docs", "public.renamed")
                .unwrap();
            assert_eq!(vectors.count().unwrap(), 0);
            let renamed = index(&connection, kind, "public.renamed");
            assert_eq!(ids(&*renamed, &Z), vec![1]);
            let before_drop = renamed.snapshot().unwrap();
            catalog.drop_table_and_data("public.renamed").unwrap();
            assert_eq!(renamed.count().unwrap(), 0);
            assert_eq!(nearest(&*before_drop, &Z), vec![1]);
            assert_eq!(old.count().unwrap(), 3);
            drop((
                old,
                changed,
                before_drop,
                vectors,
                observer,
                renamed,
                other,
                catalog,
                connection,
            ));
            let reopened = open(mode, &path);
            bind(&reopened);
            assert_eq!(index(&reopened, kind, "public.renamed").count().unwrap(), 0);
        }
    }
}

#[test]
fn native_ivf_generations_publish_atomically_and_reopen_with_canonical_vectors() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ivf.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let mut vectors = index(&connection, IndexKind::Ivf, "new\0日本語");
        let empty = vectors.snapshot().unwrap();
        vectors.initialize().unwrap();
        vectors.add(1, X.to_vec()).unwrap();
        vectors.add_many(2, vec![Y.to_vec(), Z.to_vec()]).unwrap();
        let old = vectors.snapshot().unwrap();
        connection.begin_transaction().unwrap();
        vectors.add_many(1, vec![Z.to_vec(), Y.to_vec()]).unwrap();
        let private = vectors.snapshot().unwrap();
        let observer = connection.new_session();
        assert_eq!(
            nearest(&*index(&observer, IndexKind::Ivf, "new\0日本語"), &X),
            vec![1]
        );
        observer.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER fail_vector_publication BEFORE INSERT ON _ivf_indexes BEGIN SELECT RAISE(ABORT, 'injected IVF publication failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert!(connection.in_transaction());
        assert_eq!(
            index(&observer, IndexKind::Ivf, "new\0日本語")
                .count()
                .unwrap(),
            3
        );
        assert!(vectors.add(3, X.to_vec()).is_err());
        observer
            .with_physical(|sqlite| {
                sqlite.execute_batch("DROP TRIGGER fail_vector_publication")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(vectors.count().unwrap(), 4);
        assert_eq!(empty.count().unwrap(), 0);
        assert_eq!(old.count().unwrap(), 3);
        assert_eq!(private.count().unwrap(), 4);
        assert!(ids(&*private, &X).is_empty());
        assert_eq!(ids(&*old, &X), vec![1]);
        drop((vectors, empty, old, private, observer, connection));
        let reopened = open(mode, &path);
        bind(&reopened);
        let mut restored = index(&reopened, IndexKind::Ivf, "new\0日本語");
        assert_eq!(restored.count().unwrap(), 4);
        assert_eq!(nearest(&*restored, &Y), vec![1]);
        let retained = restored.snapshot().unwrap();
        SQLiteIVFIndex::drop_metadata(&reopened, "new\0日本語", "embedding").unwrap();
        assert_eq!(restored.count().unwrap(), 4);
        assert_eq!(nearest(&*retained, &Y), vec![1]);
        assert!(restored
            .search_knn(&Y, 1)
            .unwrap_err()
            .to_string()
            .contains("missing native IVF metadata"));
        restored.initialize().unwrap();
        assert_eq!(nearest(&*restored, &Y), vec![1]);
    }
}

#[test]
fn native_vector_failures_preserve_prior_writes_and_retained_snapshots_are_read_only() {
    for kind in KINDS {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let mut vectors = index(&connection, kind, "docs");
        vectors.add(1, X.to_vec()).unwrap();
        vectors.initialize().unwrap();
        connection.begin_transaction().unwrap();
        vectors.add(2, Y.to_vec()).unwrap();
        for (doc, values) in [
            (u64::MAX, vec![X.to_vec()]),
            (1, vec![X.to_vec(), vec![f32::NAN; 3]]),
            (1, vec![vec![1.0; 2]]),
        ] {
            assert!(vectors.add_many(doc, values).is_err());
            assert_eq!(vectors.count().unwrap(), 2);
            assert_eq!(ids(&*vectors, &X), vec![1]);
        }
        let mut frozen = vectors.snapshot().unwrap();
        let writable = std::sync::Arc::get_mut(&mut frozen).unwrap();
        assert!(writable.add(3, X.to_vec()).is_err());
        assert!(writable.delete(1).is_err());
        assert!(writable.clear().is_err());
        if kind != IndexKind::Exact {
            assert!(writable.initialize().is_err());
        }
        connection.commit_transaction().unwrap();
        assert_eq!(vectors.count().unwrap(), 2);
        vectors.add_many(1, vec![]).unwrap();
        assert_eq!(vectors.count().unwrap(), 1);
        assert!(vectors.search_knn(&[f32::NAN; 3], 1).is_err());
    }
}

#[test]
fn native_ivf_reads_stored_assignments_and_only_selected_vector_payloads() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut raw = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 2);
    raw.add(1, vec![0.0, 1.0]).unwrap();
    raw.add(2, vec![1.0, 0.0]).unwrap();
    connection.with(|sqlite| {
        sqlite.execute_batch("INSERT INTO _ivf_indexes VALUES ('docs','embedding',2,2,1,2,'trained',2,0,2);
            INSERT INTO _ivf_centroids VALUES ('docs','embedding',0,x'0000803f00000000'), ('docs','embedding',1,x'000000000000803f');
            INSERT INTO _ivf_assignments VALUES ('docs','embedding',1,0,0), ('docs','embedding',2,0,1)")?;
        sqlite.execute("UPDATE _vectors SET vector=?1 WHERE doc_id=2", [vec![0_u8; 1 << 20]])?;
        Ok(())
    }).unwrap();
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(16 << 20)).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 64 << 10,
        })
        .unwrap();
    let ivf = SQLiteIVFIndex::open_existing(connection.clone(), "docs", "embedding", 2, 2, 1, 2);
    assert_eq!(nearest(&ivf, &[1.0, 0.0]), vec![1]);
    assert_eq!(ivf.count().unwrap(), 2);
    assert_eq!(raw.count().unwrap(), 2);
    assert!(raw.search_knn(&[1.0, 0.0], 1).is_err());
    assert!(ivf.search_knn(&[0.0, 1.0], 1).is_err());
    assert_eq!(nearest(&ivf, &[1.0, 0.0]), vec![1]);
    let retained = ivf.snapshot().unwrap();
    SQLiteIVFIndex::drop_metadata(&connection, "docs", "embedding").unwrap();
    assert!(ivf.search_knn(&[1.0, 0.0], 1).is_err());
    assert_eq!(nearest(&*retained, &[1.0, 0.0]), vec![1]);
}

#[test]
fn native_vector_budget_failures_do_not_leave_partial_private_replacements() {
    for kind in KINDS {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions {
                retained_bytes: 64 << 10,
            })
            .unwrap();
        let mut vectors = index(&connection, kind, "docs");
        vectors.add(1, X.to_vec()).unwrap();
        vectors.initialize().unwrap();
        connection.begin_transaction().unwrap();
        vectors.add(2, Y.to_vec()).unwrap();
        assert!(vectors.add_many(1, vec![Z.to_vec(); 4096]).is_err());
        assert_eq!(vectors.count().unwrap(), 2);
        assert_eq!(ids(&*vectors, &X), vec![1]);
        connection.commit_transaction().unwrap();
        let mut absent = index(&connection, kind, "absent");
        assert!(absent.add_many(1, vec![Z.to_vec(); 4096]).is_err());
        assert!(!connection.in_transaction());
        assert_eq!(absent.count().unwrap(), 0);
        absent.add(1, X.to_vec()).unwrap();
        assert_eq!(nearest(&*absent, &X), vec![1]);
    }
}

#[test]
fn native_exact_materialization_accounts_for_the_aggregate_vector_collection() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut vectors = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 64);
    for doc in 0..512 {
        vectors.add(doc, vec![1.0; 64]).unwrap();
    }
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(16 << 20)).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 64 << 10,
        })
        .unwrap();
    assert_eq!(vectors.count().unwrap(), 512);
    let error = vectors.search_knn(&[1.0; 64], 1).unwrap_err();
    assert!(
        matches!(error, uqa_storage::StorageBackendError::Memory(_)),
        "{error}"
    );
    assert_eq!(vectors.count().unwrap(), 512);
    vectors.add(0, vec![0.0; 64]).unwrap();
    assert_eq!(vectors.count().unwrap(), 512);
}

#[test]
fn native_same_document_conflicts_preserve_committed_and_retained_vector_generations() {
    for kind in KINDS {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let mut a = index(&connection, kind, "docs");
        a.add(1, X.to_vec()).unwrap();
        a.initialize().unwrap();
        let other = connection.new_session();
        let mut b = index(&other, kind, "docs");
        connection.begin_transaction().unwrap();
        a.add_many(1, vec![Y.to_vec(), Z.to_vec()]).unwrap();
        let private = a.snapshot().unwrap();
        b.add(1, Z.to_vec()).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_eq!(b.count().unwrap(), 1);
        assert_eq!(ids(&*b, &Z), vec![1]);
        connection.rollback_transaction().unwrap();
        assert_eq!(a.count().unwrap(), 1);
        assert!(ids(&*a, &Y).is_empty());
        assert_eq!(private.count().unwrap(), 2);
        assert_eq!(ids(&*private, &Y), vec![1]);
    }
}

#[test]
fn native_independent_ivf_fields_commit_while_another_index_remains_private() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("independent-ivf.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        let mut a = SQLiteIVFIndex::with_params(connection.clone(), "docs", "first", 3, 2, 2, 2);
        let mut b = SQLiteIVFIndex::with_params(connection.clone(), "docs", "second", 3, 2, 2, 2);
        a.add(1, X.to_vec()).unwrap();
        b.add(1, Y.to_vec()).unwrap();
        bind(&connection);
        connection.begin_transaction().unwrap();
        a.add(2, Z.to_vec()).unwrap();
        let other_path = path.clone();
        let (sent, received) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let other = open(mode, &other_path);
            bind(&other);
            let mut index = SQLiteIVFIndex::with_params(other, "docs", "second", 3, 2, 2, 2);
            index.add(2, X.to_vec()).unwrap();
            sent.send(index.count().unwrap()).unwrap();
        });
        assert_eq!(received.recv_timeout(Duration::from_secs(20)).unwrap(), 2);
        writer.join().unwrap();
        assert_eq!(b.count().unwrap(), 1);
        connection.commit_transaction().unwrap();
        assert_eq!(a.count().unwrap(), 2);
        assert_eq!(b.count().unwrap(), 2);
        drop((a, b, connection));
        let reopened = open(mode, &path);
        bind(&reopened);
        for field in ["first", "second"] {
            let index = SQLiteIVFIndex::open_existing(reopened.clone(), "docs", field, 3, 2, 2, 2);
            assert_eq!(index.count().unwrap(), 2);
            assert_eq!(
                nearest(&index, &X),
                if field == "first" { vec![1] } else { vec![2] }
            );
        }
    }
}
