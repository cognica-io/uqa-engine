//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A real published graph, exact-side vectors and uncovered changes survive history restoration.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use uqa_core::Value;

use super::{
    identity::row,
    maintenance::{index, scores, source},
    setup_catalog, FIELD, TABLE,
};
use crate::{
    diskann_index::{
        build::DiskANNTemporaryBudget,
        format::DiskANNGeneration,
        maintenance::{DiskANNChangeStatistics, DiskANNStatisticsRequest},
    },
    key_value::conformance::{expect, expect_eq},
    mvcc::{CommitSequence, VersionError, VersionedPersistence},
    read_control::StorageReadControl,
    PersistentStorageBackend, StorageBackendResult, VectorIndexOpenMode, VectorIndexSpec,
};

const BEFORE: &[(u64, f64)] = &[(1, -1.0), (3, -1.0), (4, 0.0), (5, 1.0)];
const AFTER: &[(u64, f64)] = &[(1, -1.0), (3, 1.0), (4, 0.0)];

/// Compact equality evidence for every visible physical record, including revisions and tombstones. History identity is deliberately separate because restoration must change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskANNRestoreRecords {
    sequence: CommitSequence,
    epoch: Option<u64>,
    records: usize,
    digest: [u8; 32],
}

/// Capture a fresh disposable fixture's complete record image in ordered pages. Do not use this whole-database conformance scan as a product backup implementation.
pub fn diskann_restore_records(
    persistence: &dyn VersionedPersistence,
) -> StorageBackendResult<DiskANNRestoreRecords> {
    let control = StorageReadControl::with_limit(1 << 24);
    let snapshot = persistence
        .snapshot(&control)
        .map_err(VersionError::into_storage_error)?;
    let mut hash = Sha256::new();
    let mut after = Vec::new();
    let mut records = 0;
    loop {
        let mut next = None;
        snapshot
            .visit_prefix(
                b"",
                (!after.is_empty()).then_some(after.as_slice()),
                64,
                &control,
                &mut |key, record| {
                    hash.update((key.len() as u64).to_le_bytes());
                    hash.update(key);
                    hash.update([u8::from(record.revision.is_some())]);
                    hash.update(
                        record
                            .revision
                            .map_or(0, CommitSequence::as_u64)
                            .to_le_bytes(),
                    );
                    hash.update([u8::from(record.value.is_some())]);
                    if let Some(value) = record.value {
                        hash.update((value.len() as u64).to_le_bytes());
                        hash.update(value);
                    }
                    records += 1;
                    next = Some(key.to_vec());
                    Ok(true)
                },
            )
            .map_err(VersionError::into_storage_error)?;
        let Some(next) = next else { break };
        after = next;
    }
    expect(records > 0, "backup fixture has actual persisted records")?;
    Ok(DiskANNRestoreRecords {
        sequence: snapshot.sequence(),
        epoch: snapshot.reclamation_epoch(),
        records,
        digest: hash.finalize().into(),
    })
}

fn statistics(
    backend: &dyn PersistentStorageBackend,
    generation: DiskANNGeneration,
    expected: DiskANNChangeStatistics,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let result = source(backend, control)?.statistics(
        DiskANNStatisticsRequest {
            after: None,
            max_records: 64,
        },
        control,
    )?;
    expect_eq(
        &result.generation,
        &generation,
        "restore preserves the selected generation",
    )?;
    expect_eq(
        &result.outstanding,
        &expected,
        "restore preserves exact uncovered changes",
    )?;
    expect(
        result.next.is_none(),
        "small restore fixture census is complete",
    )
}

/// Seed actual rows, a graph with an exact-side zero vector, and later update/delete/tensor-insert changes. Close every owner before copying the physical file.
pub fn verify_diskann_restore_source(
    backend: &dyn PersistentStorageBackend,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let backend = &*session.backend;
    setup_catalog(&*session.catalog)?;
    session.catalog.save_catalog_index_row(&row([91; 16])?)?;
    let mut documents = backend.document_store(TABLE);
    for id in 1..=4 {
        documents.put(id, BTreeMap::from([("n".into(), Value::Int(id as i64))]))?;
    }
    let mut raw = backend.vector_index(
        TABLE,
        FIELD,
        2,
        VectorIndexSpec::BruteForce,
        VectorIndexOpenMode::Create,
    )?;
    raw.add(1, vec![1.0, -0.0])?;
    raw.add_many(2, vec![vec![0.0, 1.0], vec![-1.0, 0.0]])?;
    raw.add(3, vec![-1.0, 0.0])?;
    raw.add(4, vec![0.0, 0.0])?;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    backend.begin_transaction()?;
    let mut live = index(backend, &temporary, &control, VectorIndexOpenMode::Create)?;
    backend.commit_transaction()?;
    backend.begin_transaction()?;
    live.add(1, vec![-1.0, 0.0])?;
    live.delete(2)?;
    live.add_many(5, vec![vec![0.0, 1.0], vec![1.0, 0.0]])?;
    documents.put(1, BTreeMap::from([("n".into(), Value::Int(11))]))?;
    documents.delete(2)?;
    documents.put(5, BTreeMap::from([("n".into(), Value::Int(5))]))?;
    backend.commit_transaction()?;
    let generation = source(backend, &control)?
        .statistics(
            DiskANNStatisticsRequest {
                after: None,
                max_records: 64,
            },
            &control,
        )?
        .generation;
    verify_diskann_restored(backend, generation)?;
    Ok(generation)
}

fn rows(backend: &dyn PersistentStorageBackend, changed: bool) -> StorageBackendResult<()> {
    let documents = backend.document_store(TABLE);
    for (id, expected) in [
        (1, Some(11)),
        (2, None),
        (3, Some(if changed { 33 } else { 3 })),
        (4, Some(4)),
        (5, if changed { None } else { Some(5) }),
    ] {
        expect_eq(
            &documents.get_field(id, "n")?,
            &expected.map(Value::Int),
            "restored row values",
        )?;
    }
    Ok(())
}

/// Verify a restored or original image with literal results, complete tensor counts, rows and unchanged outstanding-change accounting. This must not rebuild the index.
pub fn verify_diskann_restored(
    backend: &dyn PersistentStorageBackend,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let backend = &*session.backend;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let live = index(backend, &temporary, &control, VectorIndexOpenMode::Restore)?;
    expect_eq(
        &live.index_kind(),
        &"diskann",
        "restore preserves the physical method",
    )?;
    expect_eq(&live.count()?, &5, "restored complete tensor cardinality")?;
    scores(&*live, BEFORE)?;
    rows(backend, false)?;
    statistics(
        backend,
        generation,
        DiskANNChangeStatistics {
            documents: 3,
            vectors: 3,
            vector_bytes: 24,
        },
        &control,
    )
}

/// Write in the restored history, then rebuild while retaining an old query view. New and old vector origins must coexist without relabeling prior data.
pub fn verify_diskann_restored_writes(
    backend: &dyn PersistentStorageBackend,
    generation: DiskANNGeneration,
) -> StorageBackendResult<DiskANNGeneration> {
    verify_diskann_restored(backend, generation)?;
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let backend = &*session.backend;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let mut live = index(backend, &temporary, &control, VectorIndexOpenMode::Restore)?;
    let held = live.snapshot()?;
    let mut documents = backend.document_store(TABLE);
    backend.begin_transaction()?;
    live.add(3, vec![1.0, 0.0])?;
    live.delete(5)?;
    documents.put(3, BTreeMap::from([("n".into(), Value::Int(33))]))?;
    documents.delete(5)?;
    backend.commit_transaction()?;
    scores(&*live, AFTER)?;
    scores(&*held, BEFORE)?;
    statistics(
        backend,
        generation,
        DiskANNChangeStatistics {
            documents: 4,
            vectors: 2,
            vector_bytes: 16,
        },
        &control,
    )?;
    backend.begin_transaction()?;
    live.initialize()?;
    backend.commit_transaction()?;
    let next = source(backend, &control)?
        .statistics(
            DiskANNStatisticsRequest {
                after: None,
                max_records: 64,
            },
            &control,
        )?
        .generation;
    expect_eq(
        &next.database(),
        &generation.database(),
        "restoration preserves the data namespace",
    )?;
    expect_eq(
        &(next.table(), next.index()),
        &(generation.table(), generation.index()),
        "restoration preserves physical owner mappings",
    )?;
    expect(
        next.generation() > generation.generation(),
        "new history cannot reuse a generation",
    )?;
    scores(&*held, BEFORE)?;
    verify_diskann_restored_rebuild(backend, next)?;
    Ok(next)
}

/// Reopening or retrying the original restore request must preserve subsequent writes and the newer selected generation.
pub fn verify_diskann_restored_rebuild(
    backend: &dyn PersistentStorageBackend,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    let session = backend.open_controlled_session(&control)?;
    let backend = &*session.backend;
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let live = index(backend, &temporary, &control, VectorIndexOpenMode::Restore)?;
    expect_eq(
        &live.count()?,
        &3,
        "post-restore complete tensor cardinality",
    )?;
    scores(&*live, AFTER)?;
    rows(backend, true)?;
    statistics(
        backend,
        generation,
        DiskANNChangeStatistics::default(),
        &control,
    )
}
