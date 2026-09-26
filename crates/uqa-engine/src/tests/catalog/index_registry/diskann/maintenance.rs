//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
mod automatic;
use std::{path::Path, sync::atomic::Ordering};
use uqa_storage::{
    diskann_index::pages::DiskANNRecordKey, key_value::KeyValueDiskANNStore,
    read_control::StorageReadControl, KeyValueStore,
};

fn open(
    path: &Path,
    provider: usize,
    control: &StorageReadControl,
) -> (Engine, KeyValueDiskANNStore) {
    if provider == 0 {
        let connection = uqa_storage_sqlite::ManagedConnection::open(path).unwrap();
        let engine = Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteStorageProvider::new(connection.clone()),
        ))
        .unwrap();
        connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        let repository = connection.diskann_generations(control).unwrap();
        return (engine, repository);
    }
    let (engine, store): (Engine, Arc<dyn KeyValueStore>) = if provider == 1 {
        let owner = uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap();
        let store = owner.store();
        (
            Engine::from_persistent_provider(Arc::new(owner)).unwrap(),
            store,
        )
    } else {
        let owner = uqa_storage_redb::RedbStorage::open(path).unwrap();
        let store = Arc::new(owner.store());
        (
            Engine::from_persistent_provider(Arc::new(owner)).unwrap(),
            store,
        )
    };
    let repository = KeyValueDiskANNStore::connect(&store, control).unwrap();
    (engine, repository)
}

fn schema_version(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .unwrap()
}

#[test]
fn diskann_sql_vacuum_reclaims_abandoned_generations_without_rewriting_or_losing_queries() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vacuum.db");
        let control = StorageReadControl::with_limit(1 << 20);
        let (engine, repository) = open(&path, provider, &control);
        engine.release_automatic_statistics_client();
        engine
            .session
            .statistics_worker
            .store(true, Ordering::Release);
        sql(&engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES (1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
        let mut stage = repository.allocate_stage(99_999, 99_998, &control).unwrap();
        stage.start(&control).unwrap();
        stage
            .write_record(DiskANNRecordKey::Codes(0), b"abandoned", 64, &control)
            .unwrap();
        let generation = stage.generation();
        drop(stage);
        sql(&engine, "BEGIN");
        assert_eq!(
            engine.sql("VACUUM (invalid)", &[]).unwrap_err().sqlstate(),
            Some("42601")
        );
        sql(&engine, "ROLLBACK");
        assert!(repository.resume_stage(generation, &control).is_ok());
        sql(&engine, "VACUUM (ONLY_DATABASE_STATS)");
        assert!(repository.resume_stage(generation, &control).is_ok());
        let before = (provider != 2).then(|| schema_version(&path));
        sql(&engine, "VACUUM diskann_docs");
        assert!(repository.resume_stage(generation, &control).is_err());
        assert_eq!(kind(&engine), "diskann");
        assert_search(&engine, &[(1, 1.0), (2, 0.0)]);
        if let Some(before) = before {
            assert_eq!(
                schema_version(&path),
                before,
                "ordinary SQL VACUUM must not rewrite the SQLite file"
            );
        }
        sql(&engine, "VACUUM (ANALYZE) diskann_docs");
        assert_search(&engine, &[(1, 1.0), (2, 0.0)]);
    }
}
