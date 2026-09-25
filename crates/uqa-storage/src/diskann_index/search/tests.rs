//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{
    atomic::{AtomicU8, Ordering as AtomicOrdering},
    Arc, Mutex,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use uqa_core::memory::MemoryBudget;

use super::*;
use crate::diskann_index::{
    format::*, pages::*, NavigationInput, PQCodebook, PQTrainingOptions, PQTrainingSummary,
};
use crate::mvcc::{DatabaseId, StorageTransactionId};
use crate::vector_index::DiskANNIndexParams;

mod resources;

#[derive(Deserialize)]
struct Oracle {
    query: Vec<f32>,
    centroids: Vec<Vec<f64>>,
    labels: Vec<u8>,
    distances: Vec<f64>,
    neighbors: Vec<Vec<u64>>,
    entry: u64,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    list: usize,
    beam: usize,
    rounds: Vec<Round>,
    expanded: Vec<u64>,
    completion: Vec<u64>,
}

#[derive(Deserialize)]
struct Round {
    before: Vec<u64>,
    selected: Vec<u64>,
    after: Vec<u64>,
}

fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../../../tests/fixtures/diskann/beams.json")).unwrap()
}

fn version() -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([9; 16]), 7).unwrap(),
        3,
    )
    .unwrap()
}

fn query(dimensions: u32, control: &StorageReadControl) -> NavigationVector {
    let mut raw = oracle().query;
    raw.resize(dimensions as usize, 0.0);
    let NavigationInput::Navigable(query) =
        NavigationInput::from_raw(dimensions, &raw, control).unwrap()
    else {
        panic!("unit axis query");
    };
    query
}

// A sealed graph with declared axis centroids, not claimed to be the output of the trainer or Vamana builder.
fn fixture(
    dimensions: u32,
    count: usize,
    case: &Case,
    physical: &MemoryBudget,
) -> (DiskANNMemorySource, DiskANNManifest) {
    let data = oracle();
    let control = StorageReadControl::with_limit(1 << 20);
    let generation = DiskANNGeneration::new([1; 16], 2, 3, 4).unwrap();
    let parameters = DiskANNIndexParams {
        max_degree: 3,
        build_list_size: 4,
        search_list_size: case.list,
        beam_width: case.beam,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(dimensions).unwrap()
    };
    let layout = DiskANNNodeLayout::new(dimensions, 3, count as u64).unwrap();
    let mut builder = DiskANNMemoryBuilder::new(generation, physical);
    let mut coverage = DiskANNCoverageBuilder::new(generation, dimensions).unwrap();
    let mut artifacts = write_codes(&mut builder, generation, dimensions, count, &control);
    let mut encoded = Vec::new();
    for id in 0..count {
        let mut raw: Vec<_> = data.centroids[data.labels[id] as usize]
            .iter()
            .map(|&value| value as f32)
            .collect();
        raw.resize(dimensions as usize, -0.0);
        coverage
            .push(
                10 + id as u64 / 2,
                (id % 2) as u32,
                version(),
                &raw,
                &control,
            )
            .unwrap();
        let neighbors: Vec<_> = data.neighbors[id]
            .iter()
            .copied()
            .filter(|&node| node < count as u64)
            .collect();
        encoded.push(
            layout
                .encode_node(
                    &DiskANNNodeInput {
                        node_id: id as u64,
                        doc_id: 10 + id as u64 / 2,
                        ordinal: (id % 2) as u32,
                        version: version(),
                        vector: &raw,
                        neighbors: &neighbors,
                    },
                    &control,
                )
                .unwrap()
                .to_vec(),
        );
    }
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
        let page = encode_page(generation, layout, id, &payload, &control).unwrap();
        digest.update(&page[112..144]);
        builder.write_graph_page(id, &page, &control).unwrap();
    }
    artifacts.graph = digest.finalize().into();
    let manifest = DiskANNManifest::new(DiskANNManifestInput {
        generation,
        dimensions,
        parameters,
        nodes: count as u64,
        side_vectors: 0,
        entry_node: (count != 0).then_some(if count == 9 { data.entry } else { 0 }),
        coverage: coverage.finish(),
        artifacts,
    })
    .unwrap();
    (builder.finish(manifest, &control).unwrap(), manifest)
}

fn write_codes(
    builder: &mut DiskANNMemoryBuilder,
    generation: DiskANNGeneration,
    dimensions: u32,
    count: usize,
    control: &StorageReadControl,
) -> DiskANNArtifactDigests {
    let data = oracle();
    let mut artifacts = DiskANNArtifactDigests::empty();
    if count != 0 {
        let mut centroids = BudgetedVec::new(control.memory());
        for center in data.centroids.iter().take(count.min(4)) {
            for coordinate in 0..dimensions as usize {
                centroids
                    .push(center.get(coordinate).copied().unwrap_or(0.0))
                    .unwrap();
            }
        }
        let book = PQCodebook::restore(
            dimensions,
            1,
            count.min(4) as u16,
            PQTrainingSummary {
                options: PQTrainingOptions {
                    max_samples: 4,
                    max_centroids: 4,
                    max_iterations: 1,
                    seed: 42,
                },
                observed_vectors: count as u64,
                sampled_vectors: count.min(4) as u32,
            },
            centroids,
            control,
        )
        .unwrap();
        let (bytes, identity) = encode_codebook(generation, &book, control).unwrap();
        artifacts.codebook = identity.codebook_digest();
        builder
            .write_record(DiskANNRecordKey::Codebook, &bytes, control)
            .unwrap();
        let codes = &data.labels[..count];
        artifacts.codes = artifact_digest(codes, control).unwrap();
        builder
            .write_record(
                DiskANNRecordKey::Codes(0),
                &identity.encode_codes(0, codes, control).unwrap(),
                control,
            )
            .unwrap();
    }
    artifacts
}

struct Source {
    memory: DiskANNMemorySource,
    reverse: bool,
    fault: AtomicU8,
    requests: Mutex<Vec<Vec<u64>>>,
}

impl Source {
    fn new(memory: DiskANNMemorySource, reverse: bool) -> Self {
        Self {
            memory,
            reverse,
            fault: AtomicU8::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl DiskANNPageSource for Source {
    fn generation(&self) -> DiskANNGeneration {
        self.memory.generation()
    }
    fn capabilities(&self) -> DiskANNReadCapabilities {
        self.memory.capabilities()
    }
    fn read_record(
        &self,
        key: DiskANNRecordKey,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut DiskANNRecordVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.memory.read_record(key, limit, control, visit)
    }
    fn read_graph_pages(
        &self,
        pages: &[u64],
        control: &StorageReadControl,
        visit: &mut DiskANNPageVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.requests.lock().unwrap().push(pages.to_vec());
        match self.fault.load(AtomicOrdering::Relaxed) {
            1 => return Ok(()),
            2 => {
                return Err(uqa_core::memory::MemoryError::Limit {
                    required: 42,
                    limit: 17,
                }
                .into())
            }
            3 => control.cancellation().cancel(),
            _ => (),
        }
        let mut pages = pages.to_vec();
        if self.reverse {
            pages.reverse();
        }
        self.memory.read_graph_pages(&pages, control, visit)
    }
}

fn reader(
    source: Arc<dyn DiskANNPageSource>,
    manifest: &DiskANNManifest,
    cache: usize,
    control: &StorageReadControl,
) -> DiskANNReader {
    DiskANNReader::open(
        source,
        manifest.input().dimensions,
        manifest.input().parameters,
        DiskANNReadLimits {
            resident_bytes: 65_536,
            cache_bytes: cache,
            max_in_flight_page_bytes: 2 * PAGE_BYTES,
            max_record_bytes: 65_536,
        },
        control,
    )
    .unwrap()
}

fn ids(nodes: &[DiskANNNode]) -> Vec<u64> {
    nodes.iter().map(DiskANNNode::node_id).collect()
}

fn check_lookup(
    reader: &DiskANNReader,
    navigation: &NavigationVector,
    distances: &[f64],
    control: &StorageReadControl,
) {
    let lookup = reader
        .codebook()
        .unwrap()
        .lookup(navigation, control)
        .unwrap();
    for (node, distance) in distances.iter().enumerate() {
        assert_eq!(
            lookup
                .estimate(reader.code(node as u64).unwrap(), control)
                .unwrap()
                .get(),
            *distance
        );
    }
    drop(lookup);
}

#[test]
fn independent_beams_keep_frozen_priority_and_complete_each_physical_identity_once() {
    let data = oracle();
    for case in &data.cases {
        let mut expected_stats = None;
        for dimensions in [2, 512, 1024] {
            let physical = MemoryBudget::new(1 << 20);
            let (memory, manifest) = fixture(dimensions, 9, case, &physical);
            for reverse in [false, true] {
                for cache in [0, 2 * PAGE_BYTES + 1024, 20 * PAGE_BYTES] {
                    let owner = StorageReadControl::with_limit(1 << 20);
                    let source = Arc::new(Source::new(memory.clone(), reverse));
                    let reader = reader(source.clone(), &manifest, cache, &owner);
                    let control = StorageReadControl::with_limit(65_536);
                    let navigation = query(dimensions, &control);
                    check_lookup(&reader, &navigation, &data.distances, &control);
                    let mut traversal =
                        DiskANNTraversal::new(reader.clone(), &navigation, &control).unwrap();
                    drop(navigation);
                    let mut expanded = Vec::new();
                    for round in &case.rounds {
                        assert_eq!(
                            traversal
                                .workspace
                                .as_ref()
                                .unwrap()
                                .frontier
                                .iter()
                                .map(|item| item.node)
                                .collect::<Vec<_>>(),
                            round.before
                        );
                        let nodes = traversal.next_beam().unwrap();
                        assert_eq!(ids(&nodes), round.selected);
                        expanded.extend(ids(&nodes));
                        assert_eq!(
                            traversal
                                .workspace
                                .as_ref()
                                .unwrap()
                                .frontier
                                .iter()
                                .map(|item| item.node)
                                .collect::<Vec<_>>(),
                            round.after
                        );
                        for node in nodes.iter() {
                            assert_eq!(
                                (node.doc_id(), node.ordinal()),
                                (10 + node.node_id() / 2, (node.node_id() % 2) as u32)
                            );
                            assert_eq!(node.version(), version());
                        }
                    }
                    assert_eq!(expanded, case.expanded);
                    assert!(traversal.next_beam().unwrap().is_empty());
                    let mut completion = Vec::new();
                    loop {
                        let nodes = traversal.complete_next_beam().unwrap();
                        if nodes.is_empty() {
                            break;
                        }
                        completion.extend(ids(&nodes));
                    }
                    assert_eq!(completion, case.completion);
                    assert_eq!(
                        traversal.stats.approximate_expansions as usize,
                        case.expanded.len()
                    );
                    assert_eq!(
                        traversal.stats.completion_expansions as usize,
                        case.completion.len()
                    );
                    if let Some(expected) = expected_stats {
                        assert_eq!(traversal.stats(), expected);
                    }
                    expected_stats = Some(traversal.stats());
                    assert!(source
                        .requests
                        .lock()
                        .unwrap()
                        .iter()
                        .all(|batch| batch.len() <= 2));
                    // Completion releases lookup, frontier and membership even while the traversal remains retained.
                    assert_eq!(control.memory().used(), 0);
                    drop((traversal, reader, source));
                    assert_eq!(owner.memory().used(), 0);
                }
            }
            drop(memory);
            assert_eq!(physical.used(), 0);
        }
    }
}
