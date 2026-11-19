//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::hnsw_index::types::{active_key, HNSWNode, HNSWVector};

pub(in crate::hnsw_index) fn chain(count: u64, control: &StorageReadControl) -> HNSWIndex {
    let mut index =
        HNSWIndex::with_memory(0, crate::HNSWIndexParams::default(), control.memory()).unwrap();
    for id in 1..=count {
        let neighbors = [
            id.checked_sub(1).filter(|id| *id != 0),
            (id < count).then_some(id + 1),
        ]
        .into_iter()
        .flatten()
        .collect();
        let deleted = id % 7 == 0;
        index
            .nodes
            .insert(
                u128::from(id),
                HNSWNode {
                    id,
                    doc_id: id,
                    vector_ordinal: 0,
                    level: 0,
                    deleted,
                    neighbors: vec![neighbors],
                },
                Some(control),
            )
            .unwrap();
        index
            .raw_vectors
            .insert(
                u128::from(id),
                HNSWVector {
                    values: vec![],
                    norm: 0.0,
                },
                Some(control),
            )
            .unwrap();
        index
            .normalized_vectors
            .insert(
                u128::from(id),
                HNSWVector {
                    values: vec![],
                    norm: 1.0,
                },
                Some(control),
            )
            .unwrap();
        if deleted {
            index.deleted_count += 1;
        } else {
            index
                .active
                .insert(active_key(id, 0), id, Some(control))
                .unwrap();
        }
    }
    index.entry_point = (count != 0).then_some(1);
    index.next_node_id = count + 1;
    index
}

fn replace_edges(index: &mut HNSWIndex, id: u64, neighbors: Vec<u64>) {
    let mut node = (*index.nodes.get(u128::from(id)).unwrap().unwrap()).clone();
    node.neighbors[0] = neighbors;
    index.nodes.insert(u128::from(id), node, None).unwrap();
}

#[test]
fn scheduled_topology_checks_keep_all_nodes_and_fixed_work_under_small_admission() {
    let source = StorageReadControl::with_limit(128 << 10);
    let index = chain(512, &source);
    assert!(index.nodes.is_spilled());
    let held = source
        .memory()
        .reserve(source.memory().available())
        .unwrap();
    let mut expected = None;
    for window in [1, 7, 129] {
        let invoking = StorageReadControl::with_limit(32 << 10).with_temporary_limit(8 << 20);
        let mut validation = Validation::new(&index);
        let mut positions = 0;
        loop {
            let step = validation.advance(window, &invoking).unwrap();
            assert!(step.positions <= window);
            assert!(step.ready || step.positions != 0);
            positions += step.positions;
            assert!(positions < 512 * 30);
            if step.ready {
                break;
            }
        }
        assert_eq!(validation.advance(1, &invoking).unwrap().positions, 0);
        assert!(positions > 512 * 4);
        if let Some(expected) = expected {
            assert_eq!(positions, expected);
        } else {
            expected = Some(positions);
        }
        assert_eq!(invoking.memory().used(), 0);
        assert_eq!(invoking.file_budget().unwrap().usage().0, 0);
        assert!(invoking.file_budget().unwrap().usage().1 > 0);
    }
    drop((held, index));
    assert_eq!(source.memory().used(), 0);
}

#[test]
fn scheduled_topology_preserves_identity_edge_and_deleted_reachability_errors() {
    let control = StorageReadControl::with_limit(2 << 20);
    let base = chain(3, &control);
    let mut cases = vec![];
    let mut index = base.clone();
    replace_edges(&mut index, 1, vec![1]);
    index.raw_vectors.remove(3, None).unwrap();
    cases.push((index, "graph and vector identity counts differ"));
    let mut index = base.clone();
    index.next_node_id = 1;
    replace_edges(&mut index, 1, vec![1]);
    cases.push((index, "next node id does not exceed persisted node ids"));
    let mut index = base.clone();
    index.entry_point = None;
    replace_edges(&mut index, 1, vec![1]);
    cases.push((index, "non-empty graph has no valid entry point"));
    let mut index = base.clone();
    replace_edges(&mut index, 1, vec![99; 33]);
    cases.push((index, "node 1 layer 0 contains duplicate or self edges"));
    let mut index = base.clone();
    replace_edges(&mut index, 1, (2..35).collect());
    cases.push((index, "node 1 layer 0 exceeds the degree bound"));
    let mut index = base.clone();
    replace_edges(&mut index, 1, vec![99]);
    replace_edges(&mut index, 2, vec![2]);
    cases.push((index, "node 1 layer 0 references missing node 99"));
    let mut index = base.clone();
    replace_edges(&mut index, 2, vec![]);
    cases.push((index, "edge 1 <-> 2 is invalid at layer 0"));
    let mut index = base.clone();
    replace_edges(&mut index, 2, vec![1]);
    replace_edges(&mut index, 3, vec![]);
    let mut node = (*index.nodes.get(3).unwrap().unwrap()).clone();
    node.deleted = true;
    index.nodes.insert(3, node, None).unwrap();
    cases.push((
        index,
        "layer-zero graph reaches 2 of 3 nodes from entry point 1",
    ));
    for (index, expected) in cases {
        for window in [1, 17] {
            let invoking = StorageReadControl::with_limit(64 << 10);
            let mut validation = Validation::new(&index);
            let error = loop {
                match validation.advance(window, &invoking) {
                    Err(error) => break error,
                    Ok(step) => assert!(!step.ready, "corruption must reject: {expected}"),
                }
            };
            assert_eq!(error.to_string(), format!("corrupt HNSW graph: {expected}"));
            drop(validation);
            assert_eq!(invoking.memory().used(), 0);
        }
    }
}

#[test]
fn scheduled_topology_file_denial_and_cancellation_retire_their_workspace() {
    let source = StorageReadControl::with_limit(128 << 10);
    let index = chain(128, &source);
    let invoking = StorageReadControl::with_limit(32 << 10).with_temporary_limit(0);
    let mut validation = Validation::new(&index);
    let error = loop {
        match validation.advance(1, &invoking) {
            Err(error) => break error,
            Ok(step) => assert!(!step.ready),
        }
    };
    assert!(error.to_string().contains("temporary files require"));
    drop(validation);
    assert_eq!(invoking.memory().used(), 0);
    assert_eq!(invoking.file_budget().unwrap().usage().0, 0);
    let invoking = StorageReadControl::with_limit(32 << 10).with_temporary_limit(4 << 20);
    let mut validation = Validation::new(&index);
    while invoking.file_budget().unwrap().usage().0 == 0 {
        assert!(!validation.advance(1, &invoking).unwrap().ready);
    }
    invoking.cancellation().cancel();
    assert!(validation.advance(1, &invoking).is_err());
    drop(validation);
    assert_eq!(invoking.memory().used(), 0);
    assert_eq!(invoking.file_budget().unwrap().usage().0, 0);
}
