//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use super::{build_parameters, raw, version, DIMENSIONS, MAX_RECORD, NODES, WORKSPACE};
use crate::diskann_index::{
    format::{DiskANNNode, PAGE_BYTES},
    pages::{DiskANNPageSource, DiskANNReadLimits, DiskANNReader},
};
use crate::diskann_index::{search::DiskANNTraversal, NavigationInput};
use crate::key_value::conformance::{expect, expect_eq};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageBackendResult};

pub(super) fn verify(
    source: Arc<dyn DiskANNPageSource>,
    cached: &DiskANNReader,
    owner: &StorageReadControl,
) -> StorageBackendResult<()> {
    let uncached = DiskANNReader::open(
        source,
        DIMENSIONS,
        build_parameters(),
        DiskANNReadLimits {
            resident_bytes: 16 << 10,
            cache_bytes: 0,
            max_in_flight_page_bytes: PAGE_BYTES * 2,
            max_record_bytes: MAX_RECORD,
        },
        owner,
    )?;
    let mut expected = None;
    for reader in [uncached, cached.clone(), cached.clone()] {
        let query_control = StorageReadControl::with_limit(WORKSPACE);
        let NavigationInput::Navigable(query) =
            NavigationInput::from_raw(DIMENSIONS, &raw(0), &query_control)?
        else {
            return Err(StorageBackendError::Other(
                "fixture query must be navigable".into(),
            ));
        };
        let mut traversal = DiskANNTraversal::new(reader, &query, &query_control)?;
        drop(query);
        // The expected identity/order record is verifier output, separate from the controlled traversal workspace.
        let mut visited = vec![false; NODES as usize];
        let mut order = Vec::new();
        let mut completion = false;
        let mut previous_completion = None;
        loop {
            let nodes = if completion {
                traversal.complete_next_beam()?
            } else {
                traversal.next_beam()?
            };
            if nodes.is_empty() {
                if completion {
                    break;
                }
                completion = true;
                continue;
            }
            expect(
                nodes.len() <= build_parameters().beam_width,
                "configured beam bound",
            )?;
            for node in nodes.iter() {
                let id = node.node_id();
                expect(
                    id < NODES && !visited[id as usize],
                    "each physical identity expands once",
                )?;
                visited[id as usize] = true;
                order.push(id);
                if completion {
                    expect(
                        previous_completion.is_none_or(|last| last < id),
                        "ascending completion order",
                    )?;
                    previous_completion = Some(id);
                }
                check_node(node)?;
            }
        }
        let stats = traversal.stats();
        expect_eq(
            &(stats.approximate_expansions + stats.completion_expansions),
            &NODES,
            "complete physical corpus",
        )?;
        expect(visited.iter().all(|&seen| seen), "no omitted physical node")?;
        expect(
            stats.approximate_expansions > 0 && stats.completion_expansions > 0,
            "both traversal paths exercised",
        )?;
        if let Some((expected_order, expected_stats)) = &expected {
            expect_eq(
                &order,
                expected_order,
                "cache-independent provider expansion order",
            )?;
            expect_eq(
                &stats,
                expected_stats,
                "cache-independent logical work counts",
            )?;
        } else {
            expected = Some((order, stats));
        }
        expect_eq(
            &query_control.memory().used(),
            &0,
            "complete traversal releases query buffers",
        )?;
        expect(
            query_control.memory().peak() <= WORKSPACE,
            "paged query respects original workspace",
        )?;
    }
    Ok(())
}

fn check_node(node: &DiskANNNode) -> StorageBackendResult<()> {
    let id = node.node_id();
    expect_eq(
        &(node.doc_id(), node.ordinal()),
        &(10 + id / 2, (id % 2) as u32),
        "physical traversal retains tensor identity",
    )?;
    expect_eq(
        &node.version(),
        &version(),
        "physical traversal retains origin",
    )?;
    expect(
        node.vector()
            .iter()
            .zip(raw(id))
            .all(|(actual, expected)| actual.to_bits() == expected.to_bits()),
        "physical traversal retains coordinate bits",
    )?;
    Ok(())
}
