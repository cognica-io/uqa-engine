//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::conformance::{
    seed_diskann_population_upgrade, verify_diskann_population_late_publication,
    verify_diskann_population_lifetimes, verify_diskann_population_upgrade,
    verify_diskann_population_upgrade_reopen, verify_diskann_population_writers,
};

#[test]
fn diskann_population_sqlite_transactions_structural_cleanup_and_corruption_in_every_file_mode() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        for scenario in 0..3 {
            let path = directory.path().join(format!("populations-{scenario}.db"));
            let physical = connection(&path, mode);
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(physical.clone()).unwrap());
            if scenario == 1 {
                verify_diskann_population_late_publication(&store).unwrap();
            } else if scenario == 2 {
                verify_diskann_population_lifetimes(&store).unwrap();
                physical
                    .with_physical(|sqlite| {
                        for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
                            let count: i64 = sqlite.query_row(
                                &format!(
                                    "SELECT count(*) FROM {table} WHERE key >= ?1 AND key < ?2"
                                ),
                                rusqlite::params![
                                    b"\0uqa-diskann-populations-v1\0".as_slice(),
                                    b"\0uqa-diskann-populations-v1\x01".as_slice()
                                ],
                                |row| row.get(0),
                            )?;
                            assert_eq!(count, 0, "population tombstones remain in {table}");
                        }
                        Ok(())
                    })
                    .unwrap();
            } else {
                verify_diskann_population_writers(&store).unwrap();
            }
        }
    }
}

#[test]
fn diskann_population_sqlite_predecessor_upgrade_keeps_undo_late_writes_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("population-upgrade.db");
        let generation = {
            let connection = connection(&path, mode);
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection.clone()).unwrap());
            let generation = seed_diskann_population_upgrade(&store).unwrap();
            let records = crate::mvcc::SQLiteRecordStore::new(&connection).unwrap();
            super::super::downgrade_record_format(&records, 53);
            generation
        };
        {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            verify_diskann_population_upgrade(&store, generation).unwrap();
        }
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_population_upgrade_reopen(&store, generation).unwrap();
    }
}
