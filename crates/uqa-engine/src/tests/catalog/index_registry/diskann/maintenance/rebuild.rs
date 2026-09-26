//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{assert_search, automatic::stop, open, sql, Arc, Engine, Ordering, StorageReadControl};
use std::time::{Duration, Instant};
use uqa_execution::maintenance::diskann::DiskANNJournalMaintenance;
use uqa_storage::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        maintenance::{
            DiskANNChangeStatistics, DiskANNMaintenanceSource, DiskANNStatisticsRequest,
        },
        DiskANNIndexBinding,
    },
    RelationIdentity,
};

fn source(engine: &Engine, control: &StorageReadControl) -> Box<dyn DiskANNMaintenanceSource> {
    let session = engine
        .storage
        .backend
        .as_ref()
        .unwrap()
        .open_controlled_session(control)
        .unwrap();
    session
        .backend
        .diskann_maintenance_source(
            DiskANNIndexBinding {
                table: "public.diskann_docs",
                field: "embedding",
                dimensions: 2,
                index: &RelationIdentity::new("public", "diskann_idx"),
                resolver: Arc::new(
                    uqa_execution::catalog::index::diskann::DiskANNIndexIdentityResolver,
                ),
                control,
            },
            8 << 20,
        )
        .unwrap()
}

fn census(
    engine: &Engine,
    control: &StorageReadControl,
) -> uqa_storage::diskann_index::maintenance::DiskANNStatisticsPage {
    let page = source(engine, control)
        .statistics(
            DiskANNStatisticsRequest {
                after: None,
                max_records: 64,
            },
            control,
        )
        .unwrap();
    assert!(page.next.is_none(), "fixture fits one bounded census page");
    page
}

fn step(
    engine: &Engine,
    maintenance: &mut DiskANNJournalMaintenance,
) -> uqa_storage::StorageBackendResult<()> {
    let backend = engine.storage.backend.as_ref().unwrap();
    maintenance.step(
        engine.durable.catalog_indexes.snapshot(),
        backend.change_version().unwrap(),
        engine,
        &**backend,
    )
}

fn drain(engine: &Engine, maintenance: &mut DiskANNJournalMaintenance) {
    for _ in 0..40 {
        step(engine, maintenance).unwrap();
    }
    let status = maintenance.status();
    assert!(
        status.phase.is_none() && !status.pending_completion && status.last_error.is_none(),
        "{status:?}"
    );
}

fn setup(engine: &Engine) {
    stop(engine);
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding tensor(2)); INSERT INTO diskann_docs VALUES (1,ARRAY[ARRAY[1.0,0.0]]),(2,ARRAY[ARRAY[0.0,1.0]]),(3,ARRAY[ARRAY[0.0,1.0]]); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    sql(engine, "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[0.0,1.0],ARRAY[0.0,1.0]] WHERE id=1; DELETE FROM diskann_docs WHERE id=2");
}

#[test]
fn diskann_rebuild_scheduler_preserves_captured_counts_late_writes_and_reopen() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scheduled.db");
        let control = StorageReadControl::with_limit(8 << 20);
        let generation = {
            let (engine, _repository) = open(&path, provider, &control);
            setup(&engine);
            let before = census(&engine, &control);
            assert_eq!(
                before.outstanding,
                DiskANNChangeStatistics {
                    documents: 2,
                    vectors: 2,
                    vector_bytes: 16
                }
            );
            let retained = engine
                .try_table("diskann_docs")
                .unwrap()
                .unwrap()
                .vector_indexes
                .read()
                .get("embedding")
                .unwrap()
                .snapshot()
                .unwrap();
            let old = retained.search_knn(&[1.0, 0.0], 10).unwrap();
            let mut maintenance = DiskANNJournalMaintenance::with_rebuilds(
                &control,
                &engine.session.diskann_temporary,
                crate::DiskANNRebuildPolicy::new(2, u64::MAX).unwrap(),
            )
            .unwrap();
            step(&engine, &mut maintenance).unwrap();
            maintenance
                .set_rebuild_policy(crate::DiskANNRebuildPolicy::new(3, u64::MAX).unwrap())
                .unwrap();
            step(&engine, &mut maintenance).unwrap();
            assert_eq!(
                maintenance.status().last_census.unwrap().changes,
                before.outstanding
            );
            assert_eq!(
                maintenance.status().phase,
                Some(crate::DiskANNMaintenancePhase::Rebuilding)
            );
            sql(&engine, "UPDATE diskann_docs SET embedding=ARRAY[ARRAY[1.0,0.0]] WHERE id=1; INSERT INTO diskann_docs VALUES(4,ARRAY[ARRAY[-1.0,0.0]])");
            step(&engine, &mut maintenance).unwrap();
            assert_eq!(maintenance.status().completed_rebuilds, 1);
            let after = census(&engine, &control);
            assert_ne!(after.generation, before.generation);
            assert_eq!(
                after.outstanding,
                DiskANNChangeStatistics {
                    documents: 2,
                    vectors: 2,
                    vector_bytes: 16
                }
            );
            assert_eq!(retained.search_knn(&[1.0, 0.0], 10).unwrap(), old);
            assert_search(&engine, &[(1, 1.0), (3, 0.0), (4, -1.0)]);
            drain(&engine, &mut maintenance);
            assert_eq!(maintenance.status().completed_rebuilds, 1, "the first capture retained its admitted policy; later admissions use the replacement");
            maintenance
                .set_rebuild_policy(crate::DiskANNRebuildPolicy::new(2, u64::MAX).unwrap())
                .unwrap();
            drain(&engine, &mut maintenance);
            assert_eq!(maintenance.status().completed_rebuilds, 2);
            assert_eq!(retained.search_knn(&[1.0, 0.0], 10).unwrap(), old);
            let selected = census(&engine, &control);
            assert_eq!(
                (selected.examined, selected.outstanding),
                (0, DiskANNChangeStatistics::default())
            );
            assert_eq!(engine.session.diskann_temporary.used(), 0);
            selected.generation
        };
        let (engine, _repository) = open(&path, provider, &control);
        stop(&engine);
        assert_eq!(census(&engine, &control).generation, generation);
        assert_search(&engine, &[(1, 1.0), (3, 0.0), (4, -1.0)]);
    }
}

#[test]
fn diskann_rebuild_byte_policy_changes_revisit_pending_work_without_new_writes() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(8 << 20);
        let (engine, _repository) = open(&directory.path().join("policy.db"), provider, &control);
        setup(&engine);
        let before = census(&engine, &control).generation;
        let mut maintenance = DiskANNJournalMaintenance::with_rebuilds(
            &control,
            &engine.session.diskann_temporary,
            crate::DiskANNRebuildPolicy::new(u64::MAX, 17).unwrap(),
        )
        .unwrap();
        drain(&engine, &mut maintenance);
        assert_eq!(maintenance.status().completed_rebuilds, 0);
        assert_eq!(census(&engine, &control).generation, before);
        let version = engine
            .storage
            .backend
            .as_ref()
            .unwrap()
            .change_version()
            .unwrap();
        maintenance
            .set_rebuild_policy(crate::DiskANNRebuildPolicy::new(u64::MAX, 16).unwrap())
            .unwrap();
        assert_eq!(
            engine
                .storage
                .backend
                .as_ref()
                .unwrap()
                .change_version()
                .unwrap(),
            version
        );
        drain(&engine, &mut maintenance);
        assert_eq!(maintenance.status().completed_rebuilds, 1);
        assert_ne!(census(&engine, &control).generation, before);
        sql(&engine, "DELETE FROM diskann_docs");
        assert_eq!(
            census(&engine, &control).outstanding,
            DiskANNChangeStatistics {
                documents: 2,
                vectors: 0,
                vector_bytes: 0
            }
        );
        maintenance
            .set_rebuild_policy(crate::DiskANNRebuildPolicy::new(2, u64::MAX).unwrap())
            .unwrap();
        drain(&engine, &mut maintenance);
        assert_eq!(
            maintenance.status().completed_rebuilds,
            2,
            "empty replacements still trigger an empty successor"
        );
        let empty = census(&engine, &control);
        assert_eq!(
            (empty.examined, empty.outstanding),
            (0, DiskANNChangeStatistics::default())
        );
        assert_search(&engine, &[]);
    }
}

#[test]
fn diskann_rebuild_scheduler_cannot_replace_an_exhausted_temporary_allowance() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(8 << 20);
        let (engine, _repository) = open(&directory.path().join("quota.db"), provider, &control);
        setup(&engine);
        let before = census(&engine, &control).generation;
        let temporary = DiskANNTemporaryBudget::new(1);
        let mut maintenance = DiskANNJournalMaintenance::with_rebuilds(
            &control,
            &temporary,
            crate::DiskANNRebuildPolicy::new(1, u64::MAX).unwrap(),
        )
        .unwrap();
        step(&engine, &mut maintenance).unwrap();
        step(&engine, &mut maintenance).unwrap();
        let error = step(&engine, &mut maintenance).unwrap_err();
        assert!(error.to_string().contains("temporary storage"), "{error}");
        assert_eq!(maintenance.status().completed_rebuilds, 0);
        assert!(!maintenance.status().pending_completion);
        assert_eq!(census(&engine, &control).generation, before);
        assert_eq!(temporary.used(), 0);
        assert_search(&engine, &[(1, 0.0), (3, 0.0)]);
    }
}

fn background(provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("background.db");
    let control = StorageReadControl::with_limit(8 << 20);
    let (engine, _repository) = open(&path, provider, &control);
    setup(&engine);
    let before = census(&engine, &control).generation;
    let policy = crate::DiskANNRebuildPolicy::new(2, u64::MAX).unwrap();
    let sibling = engine.new_session().unwrap();
    stop(&sibling);
    engine.set_diskann_rebuild_policy(policy);
    assert_eq!(sibling.diskann_rebuild_policy(), policy);
    drop(sibling);
    engine
        .session
        .statistics_worker
        .store(false, Ordering::Release);
    engine.start_automatic_statistics();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = engine.automatic_diskann_maintenance_status();
        assert!(status.last_error.is_none(), "{status:?}");
        if status.completed_rebuilds == 1 && status.completed_passes > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background reconstruction did not complete: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    stop(&engine);
    let after = census(&engine, &control);
    assert_ne!(after.generation, before);
    assert_eq!(
        (after.examined, after.outstanding),
        (0, DiskANNChangeStatistics::default())
    );
    assert_search(&engine, &[(1, 0.0), (3, 0.0)]);
}

#[test]
fn diskann_automatic_rebuild_native_sqlite() {
    background(0);
}

#[test]
fn diskann_automatic_rebuild_sqlite_key_value() {
    background(1);
}

#[test]
fn diskann_automatic_rebuild_redb() {
    background(2);
}
