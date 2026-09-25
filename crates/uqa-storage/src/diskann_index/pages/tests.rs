//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryBudget;

use super::*;
use crate::diskann_index::{format::*, NavigationInput, PQTrainer, PQTrainingOptions};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::vector_index::DiskANNIndexParams;

mod faults;
mod resources;

fn generation() -> DiskANNGeneration {
    DiskANNGeneration::new([2; 16], 3, 4, 5).unwrap()
}
fn version() -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([1; 16]), 6).unwrap(),
        7,
    )
    .unwrap()
}
fn limits(cache: usize) -> DiskANNReadLimits {
    DiskANNReadLimits {
        resident_bytes: 65_536,
        cache_bytes: cache,
        max_in_flight_page_bytes: PAGE_BYTES * 2,
        max_record_bytes: 65_536,
    }
}

fn parameters(dimensions: u32) -> DiskANNIndexParams {
    DiskANNIndexParams {
        max_degree: 2,
        build_list_size: 4,
        search_list_size: 8,
        pq_bytes: 1,
        beam_width: 2,
        ..DiskANNIndexParams::for_dimensions(dimensions).unwrap()
    }
}

struct Fixture {
    manifest: DiskANNManifest,
    records: Vec<(DiskANNRecordKey, Vec<u8>)>,
    graph: Vec<Vec<u8>>,
}

fn fixture(dimensions: u32, nodes: usize, sides: usize) -> Fixture {
    let control = StorageReadControl::with_limit(1_048_576);
    let mut coverage = DiskANNCoverageBuilder::new(generation(), dimensions).unwrap();
    let layout = DiskANNNodeLayout::new(dimensions, 2, nodes as u64).unwrap();
    let mut raw = vec![0.0; dimensions as usize];
    let mut trainer = PQTrainer::new(
        dimensions,
        1,
        PQTrainingOptions {
            max_samples: 2,
            max_centroids: 2,
            max_iterations: 1,
            ..PQTrainingOptions::default()
        },
        &control,
    )
    .unwrap();
    let mut encoded = Vec::new();
    for id in 0..nodes {
        raw[0] = if id % 2 == 0 { 1.0 } else { -1.0 };
        let NavigationInput::Navigable(nav) =
            NavigationInput::from_raw(dimensions, &raw, &control).unwrap()
        else {
            panic!("unit vector");
        };
        trainer.observe(&nav).unwrap();
        coverage
            .push(id as u64 + 10, 0, version(), &raw, &control)
            .unwrap();
        let neighbor = [(id as u64 + 1) % nodes as u64];
        let neighbors = if nodes == 1 { &[][..] } else { &neighbor[..] };
        encoded.push(
            layout
                .encode_node(
                    &DiskANNNodeInput {
                        node_id: id as u64,
                        doc_id: id as u64 + 10,
                        ordinal: 0,
                        version: version(),
                        vector: &raw,
                        neighbors,
                    },
                    &control,
                )
                .unwrap()
                .to_vec(),
        );
    }
    let mut records = Vec::new();
    let mut artifacts = DiskANNArtifactDigests::empty();
    if nodes != 0 {
        let book = trainer.finish().unwrap();
        let (bytes, identity) = encode_codebook(generation(), &book, &control).unwrap();
        artifacts.codebook = identity.codebook_digest();
        records.push((DiskANNRecordKey::Codebook, bytes.to_vec()));
        let mut codes = Vec::new();
        for id in 0..nodes {
            raw[0] = if id % 2 == 0 { 1.0 } else { -1.0 };
            let NavigationInput::Navigable(nav) =
                NavigationInput::from_raw(dimensions, &raw, &control).unwrap()
            else {
                unreachable!()
            };
            codes.extend_from_slice(&book.encode(&nav, &control).unwrap());
        }
        artifacts.codes = artifact_digest(&codes, &control).unwrap();
        for (first, chunk) in codes.chunks(3).enumerate() {
            records.push((
                DiskANNRecordKey::Codes((first * 3) as u64),
                identity
                    .encode_codes((first * 3) as u64, chunk, &control)
                    .unwrap()
                    .to_vec(),
            ));
        }
    }
    let (graph, digest) = graph_pages(layout, &encoded, &control);
    artifacts.graph = digest;
    artifacts.side = side_records(dimensions, sides, &mut coverage, &mut records, &control);
    let manifest = DiskANNManifest::new(DiskANNManifestInput {
        generation: generation(),
        dimensions,
        parameters: parameters(dimensions),
        nodes: nodes as u64,
        side_vectors: sides as u64,
        entry_node: (nodes != 0).then_some(0),
        coverage: coverage.finish(),
        artifacts,
    })
    .unwrap();
    Fixture {
        manifest,
        records,
        graph,
    }
}

fn graph_pages(
    layout: DiskANNNodeLayout,
    encoded: &[Vec<u8>],
    control: &StorageReadControl,
) -> (Vec<Vec<u8>>, [u8; 32]) {
    let mut graph = Vec::new();
    let mut digest = Sha256::new();
    for id in 0..layout.page_count() {
        let shape = layout.page_shape(id).unwrap();
        let payload = if shape.fragments == 1 {
            encoded[shape.first_node as usize..(shape.first_node + u64::from(shape.slots)) as usize]
                .concat()
        } else {
            let start = shape.fragment_index as usize * PAGE_PAYLOAD_BYTES;
            encoded[shape.first_node as usize][start..start + shape.payload_bytes as usize].to_vec()
        };
        let page = encode_page(generation(), layout, id, &payload, control).unwrap();
        digest.update(&page[112..144]);
        graph.push(page.to_vec());
    }
    (graph, digest.finalize().into())
}

fn side_records(
    dimensions: u32,
    sides: usize,
    coverage: &mut DiskANNCoverageBuilder,
    records: &mut Vec<(DiskANNRecordKey, Vec<u8>)>,
    control: &StorageReadControl,
) -> [u8; 32] {
    let raw = vec![0.0; dimensions as usize];
    let layout = DiskANNSideLayout::new(generation(), dimensions, sides as u64).unwrap();
    let mut digest = Sha256::new();
    for index in 0..sides {
        coverage
            .push(1000 + index as u64, 0, version(), &raw, control)
            .unwrap();
        let entry = DiskANNSideEntry::from_raw(
            dimensions,
            1000 + index as u64,
            0,
            version(),
            &raw,
            control,
        )
        .unwrap();
        let bytes = layout.encode(index as u64, &[entry], control).unwrap();
        digest.update(
            layout
                .decode(index as u64, &bytes, control)
                .unwrap()
                .bytes(),
        );
        records.push((DiskANNRecordKey::Side(index as u64), bytes.to_vec()));
    }
    digest.finalize().into()
}

impl Fixture {
    fn memory(
        &self,
        budget: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNMemorySource> {
        let mut builder = DiskANNMemoryBuilder::new(generation(), budget);
        for (key, bytes) in &self.records {
            builder.write_record(*key, bytes, control)?;
        }
        for (id, page) in self.graph.iter().enumerate() {
            builder.write_graph_page(id as u64, page, control)?;
        }
        builder.finish(self.manifest, control)
    }
    fn reader(
        &self,
        source: Arc<dyn DiskANNPageSource>,
        limits: DiskANNReadLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNReader> {
        DiskANNReader::open(
            source,
            self.manifest.input().dimensions,
            self.manifest.input().parameters,
            limits,
            control,
        )
    }
    fn sealer(&self, control: &StorageReadControl) -> DiskANNArtifactSealer {
        let mut seal = DiskANNArtifactSealer::new(self.manifest, control).unwrap();
        for (key, bytes) in &self.records {
            match key {
                DiskANNRecordKey::Codebook => seal.codebook(bytes).unwrap(),
                DiskANNRecordKey::Codes(first) => seal.code_batch(*first, bytes).unwrap(),
                DiskANNRecordKey::Side(first) => seal.side_batch(*first, bytes).unwrap(),
                DiskANNRecordKey::Manifest => unreachable!(),
            }
        }
        seal
    }
}

struct Counted {
    source: DiskANNMemorySource,
    graph_reads: AtomicUsize,
    records: AtomicUsize,
    largest_batch: AtomicUsize,
}

impl Counted {
    fn new(source: DiskANNMemorySource) -> Self {
        Self {
            source,
            graph_reads: AtomicUsize::new(0),
            records: AtomicUsize::new(0),
            largest_batch: AtomicUsize::new(0),
        }
    }
}

impl DiskANNPageSource for Counted {
    fn generation(&self) -> DiskANNGeneration {
        self.source.generation()
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        self.source.capabilities()
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.records.fetch_add(1, Ordering::Relaxed);
        self.source.read_record(key, limit, control, visit)
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.graph_reads.fetch_add(pages.len(), Ordering::Relaxed);
        self.largest_batch.fetch_max(pages.len(), Ordering::Relaxed);
        self.source.read_graph_pages(pages, control, visit)
    }
}

#[test]
fn opening_loads_only_resident_metadata_and_reads_large_nodes_with_bounded_pages() {
    let fixture = fixture(1024, 3, 2);
    let physical = MemoryBudget::new(1_048_576);
    let work = StorageReadControl::with_limit(1_048_576);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &work).unwrap()));
    let mut limits = limits(0);
    limits.max_in_flight_page_bytes = PAGE_BYTES;
    let reader = fixture.reader(source.clone(), limits, &work).unwrap();
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 0);
    assert!(source.records.load(Ordering::Relaxed) > 1);
    assert_eq!(reader.capabilities().read_concurrency(), 1);
    let query = StorageReadControl::with_limit(65_536);
    for id in 0..3 {
        let node = reader.read_node(id, &query).unwrap();
        assert_eq!(node.doc_id(), id + 10);
        assert_eq!(node.vector().len(), 1024);
        assert_eq!(node.vector()[0], if id % 2 == 0 { 1.0 } else { -1.0 });
        assert!(reader.code(id).is_some());
    }
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 6);
    assert_eq!(source.largest_batch.load(Ordering::Relaxed), 1);
    assert!(reader.code(3).is_none());
    let mut side = Vec::new();
    reader
        .visit_side(&query, &mut |entry| {
            side.push(entry.doc_id());
            Ok(())
        })
        .unwrap();
    assert_eq!(side, [1000, 1001]);
    assert_eq!(query.memory().used(), 0);
    drop(source);
    let retained = reader.clone();
    drop(reader);
    work.cancellation().cancel();
    assert_eq!(retained.read_node(0, &query).unwrap().doc_id(), 10);
    drop(retained);
    assert_eq!(work.memory().used(), 0);
    assert_eq!(physical.used(), 0);
}

#[test]
fn cache_hits_eviction_and_pinned_pages_preserve_original_memory_ownership() {
    let fixture = fixture(512, 3, 0);
    let physical = MemoryBudget::new(1_048_576);
    let owner = StorageReadControl::with_limit(65_536);
    let source = Arc::new(Counted::new(fixture.memory(&physical, &owner).unwrap()));
    let reader = fixture
        .reader(source.clone(), limits(PAGE_BYTES + 1024), &owner)
        .unwrap();
    let query = StorageReadControl::with_limit(65_536);
    let mut pages = reader.read_pages(&[0], &query).unwrap();
    let pinned = pages.pop().unwrap();
    drop(pages);
    assert_eq!(query.memory().used(), 0);
    assert!(reader.cache_bytes() >= PAGE_BYTES);
    drop(reader.read_pages(&[0], &query).unwrap());
    assert_eq!(source.graph_reads.load(Ordering::Relaxed), 1);
    drop(reader.read_pages(&[1], &query).unwrap());
    assert!(reader.cache_bytes() >= PAGE_BYTES && reader.cache_bytes() <= PAGE_BYTES + 1024);
    assert_eq!(pinned.id(), 0);
    drop(pinned);
    drop(reader.read_pages(&[1], &query).unwrap());
    let mut pages = reader.read_pages(&[1], &query).unwrap();
    let last = pages.pop().unwrap();
    drop(pages);
    drop(source);
    drop(reader);
    assert_eq!(physical.used(), 0);
    assert!(owner.memory().used() >= PAGE_BYTES);
    assert_eq!(query.memory().used(), 0);
    drop(last);
    assert_eq!(owner.memory().used(), 0);
}

#[test]
fn physical_sealing_requires_complete_ordered_streams_and_rejects_recovery_after_failure() {
    let fixture = fixture(1024, 2, 1);
    let control = StorageReadControl::with_limit(65_536);
    let mut seal = fixture.sealer(&control);
    for (id, page) in fixture.graph.iter().enumerate() {
        seal.graph_page(id as u64, page).unwrap();
    }
    assert_eq!(seal.finish().unwrap().manifest(), &fixture.manifest);
    assert!(fixture.sealer(&control).finish().is_err());
    let mut seal = fixture.sealer(&control);
    assert!(seal.graph_page(1, &fixture.graph[1]).is_err());
    assert!(seal.graph_page(0, &fixture.graph[0]).is_err());
    assert!(seal.finish().is_err());
    let mut bad = fixture.graph[0].clone();
    bad[150] ^= 1;
    let mut seal = fixture.sealer(&control);
    assert!(seal.graph_page(0, &bad).is_err());
    assert!(seal.finish().is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn empty_and_all_side_sources_skip_graph_and_codebook_loading() {
    for sides in [0, 2] {
        let fixture = fixture(3, 0, sides);
        let physical = MemoryBudget::new(16_384);
        let control = StorageReadControl::with_limit(16_384);
        let source = Arc::new(Counted::new(fixture.memory(&physical, &control).unwrap()));
        let mut limits = limits(0);
        limits.max_in_flight_page_bytes = 0;
        let reader = fixture.reader(source.clone(), limits, &control).unwrap();
        assert_eq!(source.records.load(Ordering::Relaxed), 1);
        assert!(reader.codebook().is_none());
        assert!(reader.read_node(0, &control).is_err());
        assert!(reader.read_pages(&[], &control).unwrap().is_empty());
        let mut count = 0;
        reader
            .visit_side(&control, &mut |_| {
                count += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(count, sides);
        assert_eq!(source.graph_reads.load(Ordering::Relaxed), 0);
    }
}
