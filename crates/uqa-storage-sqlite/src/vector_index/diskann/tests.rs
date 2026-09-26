//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, SQLiteCompressionOptions, SQLiteRecordStore};
use std::path::Path;
use uqa_storage::diskann_index::{DiskANNCanonicalRead, DiskANNCanonicalScorer};
use uqa_storage::mvcc::{CommitStatus, VersionedPersistence, VersionedSessionOptions};

mod binding;
mod changes;
mod corpus;
mod coverage;
mod field_guards;
mod lifecycle;
mod validation;

fn open(path: &Path, mode: u8) -> ManagedConnection {
    let connection = match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "canonical-native-key"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "canonical-native-key",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap();
    bind(&connection);
    connection
}

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn memory() -> ManagedConnection {
    let connection = ManagedConnection::open_in_memory().unwrap();
    bind(&connection);
    connection
}

fn canonical(
    connection: &ManagedConnection,
    table: &str,
    field: &str,
    dimensions: u32,
) -> SQLiteDiskANNCanonical {
    SQLiteDiskANNCanonical::new(connection.clone(), table, field, dimensions).unwrap()
}

fn assert_change(
    source: &RetainedSQLiteDiskANNCanonical,
    document: DocId,
    version: DiskANNVectorVersion,
    control: &StorageReadControl,
) {
    assert_eq!(
        source
            .next_change_after(document.checked_sub(1), control)
            .unwrap(),
        Some(DiskANNChangeIdentity::new(document, version))
    );
}

fn change_count(connection: &ManagedConnection) -> i64 {
    connection
        .with_physical(|sqlite| {
            Ok(sqlite.query_row(
                "SELECT count(*) FROM _uqa_mvcc_native_vector_changes",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

fn assert_tensor(
    source: &RetainedSQLiteDiskANNCanonical,
    document: DocId,
    expected: &[Vec<f32>],
    control: &StorageReadControl,
) -> Option<DiskANNVectorVersion> {
    let origin = source.origin(document, control).unwrap();
    let mut count = 0;
    let actual = source
        .visit_document(document, control, &mut |ordinal, version, vector| {
            assert_eq!(ordinal as usize, count);
            assert_eq!(Some(version), origin);
            assert_eq!(
                vector.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                expected[count]
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>()
            );
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, expected.len());
    assert_eq!(actual, origin);
    actual
}

#[test]
fn native_diskann_canonical_tensors_retain_private_origins_and_reopen_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("canonical-{mode}.db"));
        let control = StorageReadControl::with_limit(1 << 22);
        let tensor = vec![vec![-0.0, f32::MAX], vec![1.0, f32::from_bits(1)]];
        let version = {
            let connection = open(&path, mode);
            let source = canonical(&connection, "docs", "embedding", 2);
            let version = source.replace(1, &tensor, &control).unwrap();
            source.replace(2, &[], &control).unwrap();
            let persistence = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            assert_eq!(version.writer().database(), persistence.database_id());
            assert!(matches!(
                persistence
                    .commit_status(version.writer(), &control)
                    .unwrap(),
                CommitStatus::Committed(_)
            ));
            let retained = source.retain(&control).unwrap();
            assert_change(&retained, 1, version, &control);
            assert_eq!(
                assert_tensor(&retained, 1, &tensor, &control),
                Some(version)
            );
            connection.begin_transaction().unwrap();
            connection.savepoint("before").unwrap();
            let changed = source.replace(1, &[vec![3.0, 4.0]], &control).unwrap();
            let private = source.retain(&control).unwrap();
            connection.rollback_to_savepoint("before").unwrap();
            let empty = source.replace(1, &[], &control).unwrap();
            assert_eq!(empty.writer(), changed.writer());
            assert!(empty.revision() > changed.revision());
            assert_eq!(
                assert_tensor(&source.retain(&control).unwrap(), 1, &[], &control),
                Some(empty)
            );
            connection.rollback_transaction().unwrap();
            assert_change(&private, 1, changed, &control);
            assert_eq!(change_count(&connection), 2);
            assert_eq!(
                assert_tensor(&private, 1, &[vec![3.0, 4.0]], &control),
                Some(changed)
            );
            assert_canonical_physical_rows(&connection);
            drop(persistence);
            drop(source);
            drop(connection);
            assert_eq!(
                assert_tensor(&retained, 1, &tensor, &control),
                Some(version)
            );
            assert!(assert_tensor(&retained, 2, &[], &control).is_some());
            assert_tensor(&private, 1, &[vec![3.0, 4.0]], &control);
            version
        };
        let connection = open(&path, mode);
        let source = canonical(&connection, "docs", "embedding", 2)
            .retain(&control)
            .unwrap();
        assert_eq!(assert_tensor(&source, 1, &tensor, &control), Some(version));
        assert_change(&source, 1, version, &control);
        assert_change(
            &source,
            2,
            source.origin(2, &control).unwrap().unwrap(),
            &control,
        );
        assert!(source.origin(2, &control).unwrap().is_some());
    }
}

fn assert_canonical_physical_rows(connection: &ManagedConnection) {
    connection
        .with_physical(|sqlite| {
            assert_eq!(
                sqlite.query_row("SELECT count(*) FROM _vectors", [], |r| r.get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM _uqa_mvcc_native_vector_origins",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                2
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM _uqa_mvcc_native_diskann_records",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            let raw: Vec<u8> = sqlite.query_row(
                "SELECT vector FROM _vectors WHERE doc_id=1 AND vector_ordinal=0",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(raw, [0, 0, 0, 128, 255, 255, 127, 127]);
            assert!(sqlite
                .execute("DELETE FROM _uqa_mvcc_native_vector_origins", [])
                .is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_diskann_canonical_writers_keep_disjoint_commits_and_reject_same_document_conflicts() {
    for reverse in [false, true] {
        let connection = memory();
        let control = StorageReadControl::with_limit(1 << 22);
        let a = canonical(&connection, "docs", "embedding", 2);
        a.replace(0, &[], &control).unwrap();
        let peer = connection.new_session();
        let b = canonical(&peer, "docs", "embedding", 2);
        connection.begin_transaction().unwrap();
        peer.begin_transaction().unwrap();
        let av = a.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        let bv = b.replace(2, &[vec![0.0, 1.0]], &control).unwrap();
        assert_ne!(av.writer(), bv.writer());
        let (first, last) = if reverse {
            (&peer, &connection)
        } else {
            (&connection, &peer)
        };
        first.commit_transaction().unwrap();
        last.commit_transaction().unwrap();
        let read = a.retain(&control).unwrap();
        assert_eq!(read.origin(1, &control).unwrap(), Some(av));
        assert_eq!(read.origin(2, &control).unwrap(), Some(bv));
        assert_change(&read, 1, av, &control);
        assert_change(&read, 2, bv, &control);
        connection.begin_transaction().unwrap();
        peer.begin_transaction().unwrap();
        let empty = a.replace(1, &[], &control).unwrap();
        let populated = b.replace(1, &[vec![2.0, 0.0]], &control).unwrap();
        first.commit_transaction().unwrap();
        assert!(last.commit_transaction().is_err());
        last.rollback_transaction().unwrap();
        assert_change(
            &a.retain(&control).unwrap(),
            1,
            if reverse { populated } else { empty },
            &control,
        );
        assert_eq!(change_count(&connection), 4);
        assert_tensor(&read, 1, &[vec![1.0, 0.0]], &control);
    }
}

#[test]
fn native_diskann_canonical_streams_tensors_larger_than_query_allowance() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let source = canonical(&connection, "docs", "embedding", 128);
    let tensor = vec![vec![1.0; 128]; 32];
    source.replace(1, &tensor, &control).unwrap();
    let retained = source.retain(&control).unwrap();
    let small = StorageReadControl::with_limit(8192);
    assert_tensor(&retained, 1, &tensor, &small);
    let mut ordinals = 0;
    retained
        .visit_all(&small, &mut |doc, ordinal, _, raw| {
            assert_eq!((doc, ordinal), (1, ordinals));
            assert_eq!(raw, [1.0; 128]);
            ordinals += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(ordinals, 32);
    let scorer = DiskANNCanonicalScorer::new(&retained, &[1.0; 128], &small).unwrap();
    assert_eq!(
        scorer.score_document(1).unwrap().unwrap().vector_count(),
        32
    );
    assert_eq!(
        scorer
            .search_exact_knn(1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(small.memory().used(), 0);
    let empty = canonical(&connection, "docs", "wide", 16384);
    let version = empty.replace(1, &[], &control).unwrap();
    assert_eq!(
        assert_tensor(&empty.retain(&control).unwrap(), 1, &[], &small),
        Some(version)
    );
    assert_eq!(small.memory().used(), 0);
    let tiny = StorageReadControl::with_limit(1);
    assert!(retained.origin(1, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(8192);
    let mut calls = 0;
    assert!(retained
        .visit_document(1, &cancelled, &mut |_, _, _| {
            calls += 1;
            cancelled.cancellation().cancel();
            Ok(())
        })
        .is_err());
    assert_eq!(calls, 1);
    assert_eq!(cancelled.memory().used(), 0);
    let original = StorageReadControl::with_limit(1 << 20);
    let held = source.retain(&original).unwrap();
    original.cancellation().cancel();
    assert!(held.origin(1, &small).is_err());
}
