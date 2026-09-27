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
fn diskann_population_redb_transactions_structural_cleanup_and_corruption() {
    use redb::ReadableDatabase;
    let directory = tempfile::tempdir().unwrap();
    for scenario in 0..3 {
        let owner = crate::RedbStorage::open(
            directory
                .path()
                .join(format!("populations-{scenario}.redb")),
        )
        .unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        if scenario == 1 {
            verify_diskann_population_late_publication(&store).unwrap();
        } else if scenario == 2 {
            verify_diskann_population_lifetimes(&store).unwrap();
            let records = owner.record_store().unwrap();
            let read = records.database.begin_read().unwrap();
            let lower = b"\0uqa-diskann-populations-v1\0".as_slice();
            let upper = b"\0uqa-diskann-populations-v1\x01".as_slice();
            assert_eq!(
                read.open_table(super::super::super::HEADS)
                    .unwrap()
                    .range(lower..upper)
                    .unwrap()
                    .count(),
                0
            );
            assert_eq!(
                read.open_table(super::super::super::VERSIONS)
                    .unwrap()
                    .range((lower, 0)..(upper, 0))
                    .unwrap()
                    .count(),
                0
            );
        } else {
            verify_diskann_population_writers(&store).unwrap();
        }
    }
}

#[test]
fn diskann_population_redb_predecessor_upgrade_keeps_undo_late_writes_and_cold_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("population-upgrade.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        let generation = seed_diskann_population_upgrade(&store).unwrap();
        let records = owner.record_store().unwrap();
        let transaction = super::super::super::physical_writer(&records.database).unwrap();
        transaction
            .open_table(super::super::super::METADATA)
            .unwrap()
            .insert("format", 52_u64.to_be_bytes().as_slice())
            .unwrap();
        transaction.commit().unwrap();
        generation
    };
    {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
        verify_diskann_population_upgrade(&store, generation).unwrap();
    }
    let owner = crate::RedbStorage::open(&path).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    verify_diskann_population_upgrade_reopen(&store, generation).unwrap();
}
