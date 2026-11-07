//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{GraphStore, MemoryGraphStore, PathIndexCatalog};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{Edge, Vertex};

mod lifetime;
mod repository;
mod validation;

fn path(labels: &[&str], pairs: &[(u64, u64)]) -> CapturedPath {
    CapturedPath {
        sequence: labels.iter().map(|label| (*label).into()).collect(),
        pairs: pairs.into(),
    }
}

// The format oracle is independent of the production encoder and accepts invalid order for decoder fixtures.
fn frame(paths: &[CapturedPath]) -> Vec<u8> {
    let mut bytes = b"UQAPTH01".to_vec();
    bytes.extend_from_slice(&(paths.len() as u64).to_le_bytes());
    for path in paths {
        bytes.extend_from_slice(&(path.sequence.len() as u64).to_le_bytes());
        for label in &path.sequence {
            bytes.extend_from_slice(&(label.len() as u64).to_le_bytes());
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.extend_from_slice(&(path.pairs.len() as u64).to_le_bytes());
        for (source, target) in &path.pairs {
            bytes.extend_from_slice(&source.to_le_bytes());
            bytes.extend_from_slice(&target.to_le_bytes());
        }
    }
    bytes
}

fn materialized(paths: &[CapturedPath]) -> PathIndex {
    let entries: BTreeMap<_, _> = paths
        .iter()
        .map(|path| (path.sequence.clone(), path.pairs.iter().copied().collect()))
        .collect();
    PathIndex {
        storage: PathStorage::Memory(Arc::new(MemoryPaths::from_entries(entries).unwrap())),
    }
}

fn encode(source: &PathIndex) -> Vec<u8> {
    let mut bytes = Vec::new();
    let written = source
        .write_image(&mut bytes, &StorageReadControl::with_limit(4096))
        .unwrap();
    assert_eq!(written, bytes.len() as u64);
    bytes
}

fn assert_paths(source: &PathIndex, paths: &[CapturedPath]) {
    for path in paths {
        assert!(source.has_path(&path.sequence));
        assert_eq!(
            source.lookup(&path.sequence).unwrap().unwrap(),
            path.pairs.iter().copied().collect()
        );
    }
    assert!(source.lookup(&["not-indexed".into()]).unwrap().is_none());
}

#[test]
fn path_images_preserve_literal_reachability_after_graph_and_source_loss() {
    let mut graph = MemoryGraphStore::new();
    graph.create_graph("g").unwrap();
    for id in 1..=3 {
        graph.add_vertex(Vertex::new(id, "node"), "g").unwrap();
    }
    for edge in [
        Edge::new(1, 1, 2, "knows"),
        Edge::new(2, 2, 3, "knows"),
        Edge::new(3, 3, 1, "a/b"),
    ] {
        graph.add_edge(edge, "g").unwrap();
    }
    let expected = [
        path(&[], &[(1, 1), (2, 2), (3, 3)]),
        path(&["a", "b"], &[]),
        path(&["a/b"], &[(3, 1)]),
        path(&["knows"], &[(1, 2), (2, 3)]),
        path(&["knows", "knows"], &[(1, 3)]),
        path(&["missing"], &[]),
    ];
    let sequences = expected
        .iter()
        .map(|path| path.sequence.clone())
        .collect::<Vec<_>>();
    let source = PathIndex::build(&graph, "g", &sequences).unwrap();
    drop(graph);
    let bytes = encode(&source);
    assert_eq!(bytes, frame(&expected));
    drop(source);
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = PathIndex::read_image(&mut bytes.as_slice(), &control).unwrap();
    assert_paths(&restored, &expected);
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn path_image_framing_distinguishes_empty_labels_sequences_and_all_pair_bits() {
    let expected = [
        path(&[], &[(0, 0), (1, u64::MAX), (u64::MAX, 0)]),
        path(&[""], &[(u64::MAX, u64::MAX)]),
        path(&["", ""], &[]),
        path(&["a", "b"], &[(5, 9)]),
        path(&["a/b"], &[(9, 5)]),
        path(&["é\0", "끝"], &[]),
    ];
    for paths in [&expected[..], &[]] {
        let source = materialized(paths);
        let bytes = frame(paths);
        assert_eq!(encode(&source), bytes);
        for window in [1, 7, 1021, 65536] {
            let control = StorageReadControl::with_limit(4096);
            let mut reader = source.image_reader(&control).unwrap();
            assert_eq!(reader.prepare(1).unwrap().positions, 0);
            assert!(reader.prepare(1).unwrap().ready);
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            let mut output = Vec::new();
            let mut buffer = vec![0; window];
            loop {
                let count = reader.read(&mut buffer).unwrap();
                assert!(count <= 16 * 1024);
                if count == 0 {
                    break;
                }
                output.extend_from_slice(&buffer[..count]);
            }
            assert_eq!(output, bytes);
            assert_eq!(control.memory().used(), 0);
            control.cancellation().cancel();
            assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        }
        let control = StorageReadControl::with_limit(1 << 20);
        let restored = PathIndex::read_image(&mut bytes.as_slice(), &control).unwrap();
        assert_paths(&restored, paths);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
    let literal = b"UQAPTH01\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0r\x01\0\0\0\0\0\0\0\x05\0\0\0\0\0\0\0\x09\0\0\0\0\0\0\0";
    assert_eq!(encode(&materialized(&[path(&["r"], &[(5, 9)])])), literal);
}

#[test]
fn path_image_readers_reject_query_views_without_visiting_graphs() {
    let source = PathIndex {
        storage: PathStorage::ReadView {
            store: Arc::new(crate::GraphStoreHandle::default()),
            graph: "missing".into(),
            sequences: vec![vec!["edge".into()]],
        },
    };
    let control = StorageReadControl::with_limit(4096);
    let mut bytes = Vec::new();
    assert!(source.image_reader(&control).is_err());
    assert!(source.write_image(&mut bytes, &control).is_err());
    assert!(bytes.is_empty());
    assert_eq!(control.memory().used(), 0);
}
