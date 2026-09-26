//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same backend capability is exercised by common MVCC, `SQLite` layouts and redb.

use super::{
    identity::{row, Resolver},
    runtime::diskann_runtime_fixture_options,
    setup_catalog, FIELD, TABLE,
};
use crate::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        changes::DiskANNPruneRequest,
        format::DiskANNGeneration,
        maintenance::{
            DiskANNChangeStatistics, DiskANNMaintenanceSource, DiskANNStatisticsPage,
            DiskANNStatisticsRequest,
        },
        DiskANNIndexBinding,
    },
    key_value::conformance::{expect, expect_eq},
    read_control::StorageReadControl,
    PersistentStorageBackend, StorageBackendResult, VectorIndex, VectorIndexOpenMode,
    VectorIndexSpec,
};
use std::sync::Arc;

fn binding(control: &StorageReadControl) -> StorageBackendResult<crate::CatalogIndexRow> {
    control.check()?;
    row([91; 16])
}

fn source(
    backend: &dyn PersistentStorageBackend,
    control: &StorageReadControl,
) -> StorageBackendResult<Box<dyn DiskANNMaintenanceSource>> {
    let row = binding(control)?;
    backend.diskann_maintenance_source(
        DiskANNIndexBinding {
            table: TABLE,
            field: FIELD,
            dimensions: 2,
            index: &row.relation,
            resolver: Arc::new(Resolver),
            control,
        },
        8192,
    )
}

fn index(
    backend: &dyn PersistentStorageBackend,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    mode: VectorIndexOpenMode,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    let row = binding(control)?;
    backend.diskann_index(
        DiskANNIndexBinding {
            table: TABLE,
            field: FIELD,
            dimensions: 2,
            index: &row.relation,
            resolver: Arc::new(Resolver),
            control,
        },
        diskann_runtime_fixture_options(2)?,
        temporary,
        mode,
    )
}

fn page() -> DiskANNStatisticsRequest {
    DiskANNStatisticsRequest {
        after: None,
        max_records: usize::MAX,
    }
}

fn scores(index: &dyn VectorIndex, expected: &[(u64, f64)]) -> StorageBackendResult<()> {
    let actual: Vec<_> = index
        .search_knn(&[1.0, 0.0], 10)?
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
        .collect();
    let expected: Vec<_> = expected
        .iter()
        .map(|&(doc, score)| (doc, score.to_bits()))
        .collect();
    expect_eq(&actual, &expected, "literal maintained query scores")
}

/// Use a fresh disposable backend. Census, late writers, old readers, failed admission and competing rebuilds preserve the actual catalog and original transaction completion.
pub fn verify_diskann_maintenance_source(
    backend: &dyn PersistentStorageBackend,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    session.validate_transaction_affinity()?;
    let backend = &*session.backend;
    setup_catalog(&*session.catalog)?;
    session.catalog.save_catalog_index_row(&row([91; 16])?)?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let live = changed_index(backend, &temporary, &control)?;
    let held = live.snapshot()?;
    scores(&*held, &[(1, 1.0), (3, 0.0)])?;
    let captured = source(backend, &control)?;
    let first = start_census(&*captured, &control)?;

    let peer = backend.open_controlled_session(&control)?;
    let mut writer = index(
        &*peer.backend,
        &temporary,
        &control,
        VectorIndexOpenMode::Restore,
    )?;
    peer.backend.begin_transaction()?;
    writer.add(3, vec![-1.0, 0.0])?;
    writer.add(4, vec![1.0, 0.0])?;
    peer.backend.commit_transaction()?;
    finish_census(&*captured, first, &control)?;
    let options = diskann_runtime_fixture_options(2)?;
    expect(
        source(backend, &control)?
            .rebuild(options, &temporary, &control)
            .is_err(),
        "rebuild cannot create its own transaction",
    )?;
    failed_admission(
        backend,
        &*session.catalog,
        &temporary,
        &control,
        first.generation,
    )?;

    backend.begin_transaction()?;
    session
        .catalog
        .set_metadata("maintenance-outer-write", "kept")?;
    captured.rebuild(options, &temporary, &control)?;
    expect(
        backend.in_transaction(),
        "evaluated rebuild leaves publication to its caller",
    )?;
    expect(
        source(backend, &control).is_err(),
        "private construction cannot admit committed maintenance",
    )?;
    backend.commit_transaction()?;
    expect_eq(
        &session
            .catalog
            .get_metadata("maintenance-outer-write")?
            .as_deref(),
        &Some("kept"),
        "outer effects share confirmed publication",
    )?;
    expect_eq(
        &temporary.used(),
        &0,
        "completed construction releases temporary files",
    )?;
    scores(&*held, &[(1, 1.0), (3, 0.0)])?;
    scores(&*live, &[(1, 1.0), (3, -1.0), (4, 1.0)])?;
    late_changes(backend, &control, first)?;
    let generation = replace_and_prune(backend, &*peer.backend, &temporary, &control)?;
    scores(&*held, &[(1, 1.0), (3, 0.0)])?;
    cancelled_admission(backend, &temporary, &control)?;
    Ok(generation)
}

fn changed_index(
    backend: &dyn PersistentStorageBackend,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<Box<dyn VectorIndex>> {
    let mut raw = backend.vector_index(
        TABLE,
        FIELD,
        2,
        VectorIndexSpec::BruteForce,
        VectorIndexOpenMode::Create,
    )?;
    raw.add(1, vec![1.0, 0.0])?;
    raw.add(2, vec![0.0, 1.0])?;
    backend.begin_transaction()?;
    let mut live = index(backend, temporary, control, VectorIndexOpenMode::Create)?;
    backend.commit_transaction()?;
    backend.begin_transaction()?;
    for _ in 0..70 {
        live.add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])?;
    }
    live.add_many(2, vec![])?;
    live.add_many(3, vec![vec![-1.0, 0.0]; 3])?;
    live.add(3, vec![0.0, 1.0])?;
    backend.commit_transaction()?;
    Ok(live)
}

fn start_census(
    captured: &dyn DiskANNMaintenanceSource,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNStatisticsPage> {
    expect(
        captured
            .statistics(
                DiskANNStatisticsRequest {
                    max_records: 0,
                    ..page()
                },
                control,
            )
            .is_err(),
        "zero census page rejected",
    )?;
    expect(
        captured
            .statistics(page(), &StorageReadControl::with_limit(1 << 24))
            .is_err(),
        "fresh allowance cannot replace admitted controls",
    )?;
    let cancellation = uqa_core::CancellationToken::new();
    expect(
        captured
            .statistics(
                page(),
                &StorageReadControl::new(control.memory(), &cancellation),
            )
            .is_err(),
        "shared memory cannot replace the original cancellation signal",
    )?;
    let reservation = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())?;
    expect(
        captured.statistics(page(), control).is_err(),
        "original exhausted allowance rejects census",
    )?;
    drop(reservation);
    let first = captured.statistics(page(), control)?;
    expect_eq(
        &first.examined,
        &64,
        "census clamps the page to 64 actual keys",
    )?;
    expect(
        first.next.is_some(),
        "census retains a bounded continuation",
    )?;
    Ok(first)
}

fn finish_census(
    captured: &dyn DiskANNMaintenanceSource,
    first: DiskANNStatisticsPage,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let last = captured.statistics(
        DiskANNStatisticsRequest {
            after: first.next,
            ..page()
        },
        control,
    )?;
    expect_eq(
        &last.examined,
        &11,
        "later writes do not extend the captured 75 keys",
    )?;
    expect(
        last.next.is_none(),
        "finite census reaches its original end",
    )?;
    expect_eq(
        &first.outstanding.checked_add(last.outstanding)?,
        &DiskANNChangeStatistics {
            documents: 3,
            vectors: 3,
            vector_bytes: 24,
        },
        "complete captured tensors include empty replacements and exclude obsolete mutations",
    )
}

fn late_changes(
    backend: &dyn PersistentStorageBackend,
    control: &StorageReadControl,
    first: DiskANNStatisticsPage,
) -> StorageBackendResult<()> {
    let current = source(backend, control)?;
    expect(
        current
            .statistics(
                DiskANNStatisticsRequest {
                    after: first.next,
                    ..page()
                },
                control,
            )
            .is_err(),
        "an earlier generation cursor cannot enter a successor census",
    )?;
    let current_page = current.statistics(page(), control)?;
    let mut statistics = current_page.outstanding;
    let mut after = current_page.next;
    while after.is_some() {
        let next = current.statistics(DiskANNStatisticsRequest { after, ..page() }, control)?;
        statistics = statistics.checked_add(next.outstanding)?;
        after = next.next;
    }
    expect_eq(
        &statistics,
        &DiskANNChangeStatistics {
            documents: 2,
            vectors: 2,
            vector_bytes: 16,
        },
        "late commits remain uncovered by the captured rebuild",
    )?;
    expect(
        current_page.generation != first.generation,
        "publication selects the constructed successor",
    )
}

fn replace_and_prune(
    backend: &dyn PersistentStorageBackend,
    peer: &dyn PersistentStorageBackend,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    let options = diskann_runtime_fixture_options(2)?;
    let stale = source(backend, control)?;
    let winner = source(peer, control)?;
    peer.begin_transaction()?;
    winner.rebuild(options, temporary, control)?;
    peer.commit_transaction()?;
    backend.begin_transaction()?;
    expect(
        stale.rebuild(options, temporary, control).is_err(),
        "competing publication rejects the captured old head",
    )?;
    backend.rollback_transaction()?;
    let selected = source(backend, control)?;
    let mut request = DiskANNPruneRequest {
        after: None,
        max_records: 64,
    };
    loop {
        backend.begin_transaction()?;
        let result = selected.prune(request, control)?;
        backend.commit_transaction()?;
        let Some(after) = result.next else { break };
        request.after = Some(after);
    }
    let latest = source(backend, control)?.statistics(page(), control)?;
    expect_eq(
        &latest.outstanding,
        &DiskANNChangeStatistics::default(),
        "fully covered corpus has no outstanding work",
    )?;
    expect_eq(
        &latest.examined,
        &0,
        "covered journal was actually reclaimed",
    )?;
    Ok(latest.generation)
}

fn failed_admission(
    backend: &dyn PersistentStorageBackend,
    catalog: &dyn crate::CatalogFacade,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let captured = source(backend, control)?;
    let mut options = diskann_runtime_fixture_options(2)?;
    options.read.resident_bytes = 1;
    backend.begin_transaction()?;
    catalog.set_metadata("maintenance-failed-outer", "kept")?;
    expect(
        captured.rebuild(options, temporary, control).is_err(),
        "new resident generation requires admission before publication",
    )?;
    expect(
        backend.in_transaction(),
        "rejected construction retains the caller transaction",
    )?;
    expect_eq(
        &catalog.get_metadata("maintenance-failed-outer")?.as_deref(),
        &Some("kept"),
        "failed build does not undo preceding caller effects",
    )?;
    backend.rollback_transaction()?;
    expect_eq(
        &source(backend, control)?
            .statistics(page(), control)?
            .generation,
        &generation,
        "failed construction leaves the selected generation unchanged",
    )
}

fn cancelled_admission(
    backend: &dyn PersistentStorageBackend,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let cancellation = uqa_core::CancellationToken::new();
    let original = StorageReadControl::new(control.memory(), &cancellation);
    let sibling = backend.open_controlled_session(&original)?;
    let captured = source(&*sibling.backend, &original)?;
    sibling.backend.begin_transaction()?;
    cancellation.cancel();
    expect(
        captured.statistics(page(), &original).is_err(),
        "census retains original cancellation",
    )?;
    expect(
        captured
            .rebuild(diskann_runtime_fixture_options(2)?, temporary, &original)
            .is_err(),
        "cancelled construction cannot publish",
    )?;
    sibling.backend.rollback_transaction()
}

/// Reopen after all prior provider and query owners are released; the selected head and literal live results survive without rebuilding.
pub fn verify_diskann_maintenance_reopen(
    backend: &dyn PersistentStorageBackend,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let actual = source(&*session.backend, &control)?.statistics(page(), &control)?;
    expect_eq(
        &actual.generation,
        &generation,
        "maintenance head survives actual cold reopen",
    )?;
    expect_eq(
        &actual.outstanding,
        &DiskANNChangeStatistics::default(),
        "cold source preserves complete build coverage",
    )?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    scores(
        &*index(
            &*session.backend,
            &temporary,
            &control,
            VectorIndexOpenMode::Restore,
        )?,
        &[(1, 1.0), (3, -1.0), (4, 1.0)],
    )
}
