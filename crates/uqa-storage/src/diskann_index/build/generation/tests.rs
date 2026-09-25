//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{path::Path, sync::Arc};
use uqa_core::memory::MemoryBudget;

use super::*;
use crate::diskann_index::build::tests::{generation, version};
use crate::diskann_index::build::{
    DiskANNMergeOptions, DiskANNPartitionOptions, DiskANNTemporaryBudget,
};
use crate::diskann_index::format::PAGE_BYTES;
use crate::diskann_index::pages::{DiskANNPageSource, DiskANNReadLimits, DiskANNReader};
use crate::vector_index::DiskANNIndexParams;

mod resources;
mod sealing;

fn options() -> DiskANNGenerationOptions {
    DiskANNGenerationOptions {
        training: PQTrainingOptions {
            max_samples: 4,
            max_centroids: 2,
            max_iterations: 3,
            seed: 42,
        },
        code_batch_nodes: 3,
        side_batch_entries: 2,
        max_record_bytes: 32 << 10,
    }
}

fn parameters(dimensions: u32) -> DiskANNIndexParams {
    DiskANNIndexParams {
        max_degree: 3,
        build_list_size: 4,
        search_list_size: 8,
        beam_width: 2,
        pq_bytes: 1,
        ..DiskANNIndexParams::for_dimensions(dimensions).unwrap()
    }
}

fn merged(input: &DiskANNBuildInput, directory: &Path) -> DiskANNMergedGraph {
    let runs = input
        .build_partitions(
            directory,
            parameters(input.dimensions()),
            DiskANNPartitionOptions {
                max_partition_points: 4,
                coarse_training: PQTrainingOptions {
                    max_samples: 4,
                    max_centroids: 3,
                    max_iterations: 3,
                    seed: 42,
                },
                max_depth: 0,
            },
        )
        .unwrap();
    input
        .merge_partitions(
            runs,
            directory,
            DiskANNMergeOptions {
                sort_buffer_records: 4,
            },
        )
        .unwrap()
}

fn capture(
    directory: &Path,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
    dimensions: u32,
    nodes: u64,
    sides: u64,
) -> DiskANNBuildInput {
    DiskANNBuildInput::capture(
        generation(),
        dimensions,
        directory,
        temporary,
        control,
        |visitor| {
            let mut raw = vec![0.0; dimensions as usize];
            for id in 0..nodes {
                raw[0] = if id % 2 == 0 { 1.0 } else { -1.0 };
                raw[1] = -0.0;
                visitor(id / 2 + 10, (id % 2) as u32, version(), &raw)?;
            }
            for id in 0..sides {
                raw[0] = 0.0;
                visitor(1000 + id, 0, version(), &raw)?;
            }
            Ok(())
        },
    )
    .unwrap()
}

fn reader(
    source: Arc<dyn DiskANNPageSource>,
    dimensions: u32,
    control: &StorageReadControl,
) -> DiskANNReader {
    DiskANNReader::open(
        source,
        dimensions,
        parameters(dimensions),
        DiskANNReadLimits {
            resident_bytes: 32 << 10,
            cache_bytes: 0,
            max_in_flight_page_bytes: PAGE_BYTES * 2,
            max_record_bytes: options().max_record_bytes,
        },
        control,
    )
    .unwrap()
}

#[test]
fn complete_streams_preserve_ordinals_raw_bits_versions_batches_and_global_cycle() {
    for (dimensions, nodes) in [(2, 51), (1024, 3)] {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(256 << 10);
        let temporary = DiskANNTemporaryBudget::new(4 << 20);
        let physical = MemoryBudget::new(1 << 20);
        let input = capture(directory.path(), &temporary, &control, dimensions, nodes, 5);
        let graph = merged(&input, directory.path());
        let mut sink = DiskANNMemoryBuilder::new(generation(), &physical);
        let manifest = input
            .write_generation(&graph, options(), &mut sink)
            .unwrap();
        let provenance = manifest.build_provenance().unwrap();
        assert_eq!(
            provenance.adjacency_digest(),
            graph.summary().adjacency_digest
        );
        assert_eq!(provenance.edges(), graph.summary().edges);
        assert_eq!(provenance.entry_sample_points(), nodes);
        assert_eq!(manifest.input().entry_node, Some(0));
        assert_eq!(manifest.input().coverage, input.coverage());
        let source = Arc::new(sink.finish(manifest, &control).unwrap());
        for first in (0..nodes).step_by(3) {
            source
                .read_record(
                    DiskANNRecordKey::Codes(first),
                    options().max_record_bytes,
                    &control,
                    &mut |_| Ok(()),
                )
                .unwrap();
        }
        for first in [0, 2, 4] {
            source
                .read_record(
                    DiskANNRecordKey::Side(first),
                    options().max_record_bytes,
                    &control,
                    &mut |_| Ok(()),
                )
                .unwrap();
        }
        assert!(source
            .read_record(
                DiskANNRecordKey::Codes(1),
                options().max_record_bytes,
                &control,
                &mut |_| Ok(())
            )
            .is_err());
        drop((graph, input));
        assert_eq!(temporary.used(), 0);
        let query = StorageReadControl::with_limit(256 << 10);
        let reader = reader(source.clone(), dimensions, &query);
        for id in 0..nodes {
            let node = reader.read_node(id, &query).unwrap();
            assert_eq!(
                (node.doc_id(), node.ordinal()),
                (id / 2 + 10, (id % 2) as u32)
            );
            assert_eq!(node.version(), version());
            assert_eq!(
                node.vector()[0].to_bits(),
                if id % 2 == 0 { 1.0_f32 } else { -1.0_f32 }.to_bits()
            );
            assert_eq!(node.vector()[1].to_bits(), (-0.0_f32).to_bits());
            assert!(node.vector()[2..].iter().all(|v| v.to_bits() == 0));
            assert!(node.neighbors().contains(&((id + 1) % nodes)));
            assert!(node.neighbors().len() <= 3);
            assert!(reader.code(id).is_some());
        }
        let mut side = Vec::new();
        reader
            .visit_side(&query, &mut |entry| {
                assert_eq!(entry.version(), version());
                side.push((entry.doc_id(), entry.ordinal()));
                Ok(())
            })
            .unwrap();
        assert_eq!(side, (1000..1005).map(|id| (id, 0)).collect::<Vec<_>>());
        drop((reader, source));
        assert_eq!(query.memory().used(), 0);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(physical.used(), 0);
    }
}

#[test]
fn empty_all_side_and_singleton_generations_seal_without_fabricated_graphs() {
    for (nodes, sides) in [(0, 0), (0, 3), (1, 0)] {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(64 << 10);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let physical = MemoryBudget::new(64 << 10);
        let input = capture(directory.path(), &temporary, &control, 2, nodes, sides);
        let graph = merged(&input, directory.path());
        let mut sink = DiskANNMemoryBuilder::new(generation(), &physical);
        let manifest = input
            .write_generation(&graph, options(), &mut sink)
            .unwrap();
        assert_eq!(manifest.input().entry_node, (nodes != 0).then_some(0));
        assert_eq!(manifest.build_provenance().unwrap().edges(), 0);
        let source = Arc::new(sink.finish(manifest, &control).unwrap());
        let reader = reader(source, 2, &control);
        assert_eq!(reader.codebook().is_some(), nodes != 0);
        if nodes != 0 {
            assert!(reader
                .read_node(0, &control)
                .unwrap()
                .neighbors()
                .is_empty());
        }
        let mut visited = 0;
        reader
            .visit_side(&control, &mut |_| {
                visited += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, sides);
        drop((reader, graph, input));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(physical.used(), 0);
        assert_eq!(temporary.used(), 0);
    }
}
