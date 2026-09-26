//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use crate::diskann_index::format::{
    artifact_digest, encode_codebook, encode_page, DiskANNArtifactDigests, DiskANNCoverageBuilder,
    DiskANNGeneration, DiskANNManifest, DiskANNManifestInput, DiskANNNodeInput, DiskANNNodeLayout,
    DiskANNSideEntry, DiskANNSideLayout, DiskANNVectorVersion, PAGE_BYTES, PAGE_HEADER_BYTES,
};
use crate::diskann_index::pages::{
    DiskANNPageSource, DiskANNReadLimits, DiskANNReader, DiskANNRecordKey,
};
use crate::diskann_index::{NavigationInput, PQTrainer, PQTrainingOptions};
use crate::key_value::conformance::{expect, expect_eq};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};

use super::keys::{Keys, Kind};
use super::{
    DiskANNStageStatus, KeyValueDiskANNSource, KeyValueDiskANNStage, KeyValueDiskANNStore,
};

const MAX_RECORD: usize = 65_536;

mod build;
pub use build::{verify_diskann_built_generation, verify_diskann_built_reopen};
mod identifiers;
mod maintenance;
mod mappings;
mod ownership;
mod reclamation;
pub use maintenance::verify_diskann_maintenance;
pub use ownership::verify_diskann_build_ownership;
pub use reclamation::{verify_diskann_reclamation_bounds, verify_diskann_reclamation_reopen};

/// Exercise real physical streams, conditional staging, bounded discard and retained reads on a disposable versioned provider. Returns a sealed generation for a subsequent cold reopen check.
pub fn verify_diskann_generations(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let database = repository.initialize(&control)?;
    expect_eq(
        &repository.initialize(&control)?,
        &database,
        "stable DiskANN data identity",
    )?;
    identifiers::verify(store, &repository, &control)?;
    mappings::verify(store, &control)?;

    store.begin_transaction()?;
    store.put(b"diskann-caller-private", b"uncommitted")?;
    let mut stage = repository.allocate_stage(11, 12, &control)?;
    let generation = stage.generation();
    stage.start(&control)?;
    let fixture = fixture(generation, &control)?;
    write(&stage, &fixture, &control)?;
    expect(
        repository.open_source(generation, &control).is_err(),
        "unsealed source rejected",
    )?;
    expect(
        stage.write_graph_page(0, &fixture.page, &control).is_err(),
        "staging replacement rejected",
    )?;
    let source = stage.seal(fixture.manifest, MAX_RECORD, &control)?;
    expect(
        store.in_transaction(),
        "physical stage preserves caller transaction",
    )?;
    store.rollback_transaction()?;
    expect(
        store.get(b"diskann-caller-private")?.is_none(),
        "caller rollback remains independent",
    )?;
    check_source(source.clone(), &control)?;
    expect(
        stage.discard_step(1, &control).is_err(),
        "sealed generation cannot be discarded",
    )?;
    expect(
        stage
            .write_record(DiskANNRecordKey::Side(1), b"late", MAX_RECORD, &control)
            .is_err(),
        "sealed late writer rejected",
    )?;
    stage.seal(fixture.manifest, MAX_RECORD, &control)?;

    check_retention(&**store, &repository, &source, &fixture.page, &control)?;

    let other = DiskANNGeneration::new(
        [253; 16],
        generation.table(),
        generation.index(),
        generation.generation(),
    )?;
    expect(
        repository.open_source(other, &control).is_err(),
        "cross-database generation rejected",
    )?;
    check_discard(&repository, &control)?;
    check_corrupt_stage(&repository, &control)?;

    control.cancellation().cancel();
    expect(
        matches!(
            source.read_graph_pages(&[0], &control, &mut |_, _| Ok(())),
            Err(StorageBackendError::Cancelled(_))
        ),
        "current source cancellation preserved",
    )?;
    drop(stage);
    drop(repository);
    check_source(source, &StorageReadControl::with_limit(1 << 20))?;
    expect_eq(
        &control.memory().used(),
        &0,
        "DiskANN owners release query reservations",
    )?;
    Ok(generation)
}

fn check_retention(
    store: &dyn KeyValueStore,
    repository: &KeyValueDiskANNStore,
    source: &Arc<KeyValueDiskANNSource>,
    original: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let generation = source.generation();
    let page_key = Keys::new(generation).key(Kind::Graph(0));
    store.put(page_key.as_ref(), &[8; PAGE_BYTES + 1])?;
    store.vacuum()?;
    check_source(source.clone(), control)?;
    let fresh = repository.open_source(generation, control)?;
    let read = StorageReadControl::with_limit(32_768);
    let mut visited = false;
    let result = fresh.read_graph_pages(&[0], &read, &mut |_, _| {
        visited = true;
        Ok(())
    });
    expect(
        matches!(result, Err(StorageBackendError::Memory(_))) && !visited,
        "oversized persisted page fails before visitor",
    )?;
    expect(
        read.memory().peak() < PAGE_BYTES,
        "oversized page was not materialized",
    )?;
    store.delete(page_key.as_ref())?;
    store.vacuum()?;
    check_source(source.clone(), control)?;
    expect(
        repository
            .open_source(generation, control)?
            .read_graph_pages(&[0], control, &mut |_, _| Ok(()))
            .is_err(),
        "missing persisted page rejected",
    )?;
    store.put(page_key.as_ref(), original)
}

/// Verify that no in-process graph, codebook, session, or staging handle is needed after all previous provider owners close.
pub fn verify_diskann_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    expect_eq(
        &repository.data_identity(&control)?,
        &generation.database(),
        "reopened data identity",
    )?;
    let stage = repository.resume_stage(generation, &control)?;
    expect_eq(
        &stage.status(&control)?,
        &Some(DiskANNStageStatus::Sealed),
        "reopened seal",
    )?;
    let source = repository.open_source(generation, &control)?;
    let next = repository.allocate_stage(generation.table(), generation.index(), &control)?;
    expect(
        next.generation().generation() > generation.generation(),
        "generation watermark survives reopen",
    )?;
    drop(next);
    drop(stage);
    drop(repository);
    check_source(source, &control)?;
    expect_eq(
        &control.memory().used(),
        &0,
        "reopened source releases reservations",
    )
}

fn check_discard(
    repository: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut stage = repository.allocate_stage(11, 12, control)?;
    stage.start(control)?;
    let generation = stage.generation();
    for index in 0..3 {
        stage.write_record(
            DiskANNRecordKey::Codes(index),
            b"unsealed",
            MAX_RECORD,
            control,
        )?;
    }
    let obsolete = repository.resume_stage(generation, control)?;
    expect(
        !stage.discard_step(1, control)?,
        "discard deletes only its first bounded page",
    )?;
    expect_eq(
        &stage.status(control)?,
        &Some(DiskANNStageStatus::Discarding),
        "discard fences late writers",
    )?;
    expect(
        obsolete
            .write_record(DiskANNRecordKey::Codes(3), b"late", MAX_RECORD, control)
            .is_err(),
        "resumed stale writer rejected",
    )?;
    expect(
        !stage.discard_step(1, control)?,
        "second bounded discard page",
    )?;
    expect(
        !stage.discard_step(1, control)?,
        "full last discard page retains a resumable state",
    )?;
    expect(
        stage.discard_step(1, control)?,
        "empty tail completes discard",
    )?;
    expect(
        stage.discard_step(1, control)?,
        "discard completion is idempotent",
    )?;
    expect(
        stage.start(control).is_err(),
        "old handle cannot recreate discarded namespace",
    )?;
    expect(
        repository.resume_stage(generation, control).is_err(),
        "discarded generation cannot be resumed",
    )?;
    let next = repository.allocate_stage(11, 12, control)?;
    expect(
        next.generation().generation() > generation.generation(),
        "discard does not reuse allocation",
    )
}

fn check_corrupt_stage(
    repository: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut stage = repository.allocate_stage(11, 12, control)?;
    stage.start(control)?;
    let mut fixture = fixture(stage.generation(), control)?;
    fixture.page[PAGE_HEADER_BYTES] ^= 1;
    write(&stage, &fixture, control)?;
    expect(
        stage.seal(fixture.manifest, MAX_RECORD, control).is_err(),
        "persisted corrupt page fails sealing",
    )?;
    expect_eq(
        &stage.status(control)?,
        &Some(DiskANNStageStatus::Frozen),
        "failed physical seal remains frozen",
    )?;
    expect(
        repository.open_source(stage.generation(), control).is_err(),
        "failed seal remains unavailable",
    )?;
    expect(
        stage.write_graph_page(1, &fixture.page, control).is_err(),
        "frozen stage fences writes",
    )?;
    let mut resumed = repository.resume_stage(stage.generation(), control)?;
    expect(
        resumed.discard_step(64, control)?,
        "failed generation can be boundedly discarded",
    )
}

fn check_source(
    source: Arc<KeyValueDiskANNSource>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let reader = DiskANNReader::open(
        source,
        2,
        parameters(),
        DiskANNReadLimits {
            resident_bytes: MAX_RECORD,
            cache_bytes: PAGE_BYTES * 2,
            max_in_flight_page_bytes: PAGE_BYTES * 2,
            max_record_bytes: MAX_RECORD,
        },
        control,
    )?;
    let node = reader.read_node(0, control)?;
    expect_eq(&node.doc_id(), &10, "persisted node document")?;
    expect_eq(&node.ordinal(), &0, "persisted ordinal")?;
    expect_eq(
        &node.version(),
        &version(),
        "persisted original vector version",
    )?;
    expect_eq(
        &node
            .vector()
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        &vec![1.0_f32.to_bits(), (-0.0_f32).to_bits()],
        "raw vector bits survive persistence",
    )?;
    expect_eq(&reader.code(0), &Some(&[0][..]), "persisted PQ code")?;
    let mut side = 0;
    reader.visit_side(control, &mut |entry| {
        expect_eq(&entry.doc_id(), &20, "persisted side document")?;
        expect_eq(
            &entry.version(),
            &version(),
            "persisted side vector version",
        )?;
        side += 1;
        Ok(())
    })?;
    expect_eq(&side, &1, "complete persisted side stream")
}

fn parameters() -> DiskANNIndexParams {
    DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 8,
        beam_width: 2,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(2).expect("valid dimensions")
    }
}

fn version() -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([4; 16]), 5).expect("nonzero writer"),
        6,
    )
    .expect("nonzero revision")
}

struct Fixture {
    manifest: DiskANNManifest,
    records: Vec<(DiskANNRecordKey, Vec<u8>)>,
    page: Vec<u8>,
}

fn fixture(
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> StorageBackendResult<Fixture> {
    let raw = [1.0, -0.0];
    let NavigationInput::Navigable(nav) = NavigationInput::from_raw(2, &raw, control)? else {
        unreachable!("unit vector")
    };
    let mut trainer = PQTrainer::new(
        2,
        1,
        PQTrainingOptions {
            max_samples: 1,
            max_centroids: 1,
            max_iterations: 1,
            ..PQTrainingOptions::default()
        },
        control,
    )?;
    trainer.observe(&nav)?;
    let book = trainer.finish()?;
    let (codebook, identity) = encode_codebook(generation, &book, control)?;
    let code = book.encode(&nav, control)?;
    let codes = identity.encode_codes(0, &code, control)?;
    let layout = DiskANNNodeLayout::new(2, 2, 1)?;
    let node = layout.encode_node(
        &DiskANNNodeInput {
            node_id: 0,
            doc_id: 10,
            ordinal: 0,
            version: version(),
            vector: &raw,
            neighbors: &[],
        },
        control,
    )?;
    let page = encode_page(generation, layout, 0, &node, control)?;
    let side_layout = DiskANNSideLayout::new(generation, 2, 1)?;
    let entry = DiskANNSideEntry::from_raw(2, 20, 0, version(), &[0.0, -0.0], control)?;
    let side = side_layout.encode(0, &[entry], control)?;
    let mut coverage = DiskANNCoverageBuilder::new(generation, 2)?;
    coverage.push(10, 0, version(), &raw, control)?;
    coverage.push(20, 0, version(), &[0.0, -0.0], control)?;
    let manifest = DiskANNManifest::new(DiskANNManifestInput {
        generation,
        dimensions: 2,
        parameters: parameters(),
        nodes: 1,
        side_vectors: 1,
        entry_node: Some(0),
        coverage: coverage.finish(),
        artifacts: DiskANNArtifactDigests {
            codebook: identity.codebook_digest(),
            codes: artifact_digest(&code, control)?,
            side: artifact_digest(side_layout.decode(0, &side, control)?.bytes(), control)?,
            graph: artifact_digest(&page[PAGE_HEADER_BYTES - 32..PAGE_HEADER_BYTES], control)?,
        },
    })?;
    Ok(Fixture {
        manifest,
        records: vec![
            (DiskANNRecordKey::Codebook, codebook.to_vec()),
            (DiskANNRecordKey::Codes(0), codes.to_vec()),
            (DiskANNRecordKey::Side(0), side.to_vec()),
        ],
        page: page.to_vec(),
    })
}

fn write(
    stage: &KeyValueDiskANNStage,
    fixture: &Fixture,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for (key, bytes) in &fixture.records {
        stage.write_record(*key, bytes, MAX_RECORD, control)?;
    }
    stage.write_graph_page(0, &fixture.page, control)
}
