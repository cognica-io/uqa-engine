//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical-vector and live graph-node consistency checks.

use std::collections::BTreeMap;

use uqa_core::DocId;

use crate::hnsw_index::HNSWNodeSnapshot;
use crate::sqlite::{Result as SQLiteResult, SQLiteError};

pub(super) fn validate_canonical_vectors(
    canonical: &[(DocId, u32, Vec<f32>)],
    nodes: &[HNSWNodeSnapshot],
) -> SQLiteResult<()> {
    let mut live = BTreeMap::<(DocId, u32), &[f32]>::new();
    for node in nodes.iter().filter(|node| !node.deleted) {
        let key = (node.doc_id, node.vector_ordinal);
        if live.insert(key, &node.raw_vector).is_some() {
            return Err(corrupt(&format!(
                "duplicate live graph vector {}:{}",
                node.doc_id, node.vector_ordinal
            )));
        }
    }
    for (doc_id, ordinal, vector) in canonical {
        let Some(graph_vector) = live.remove(&(*doc_id, *ordinal)) else {
            return Err(corrupt(&format!(
                "canonical vector {doc_id}:{ordinal} has no live graph node"
            )));
        };
        if !same_bits(graph_vector, vector) {
            return Err(corrupt(&format!(
                "canonical vector {doc_id}:{ordinal} differs from its live graph node"
            )));
        }
    }
    if let Some(((doc_id, ordinal), _)) = live.first_key_value() {
        return Err(corrupt(&format!(
            "live graph node {doc_id}:{ordinal} has no canonical vector"
        )));
    }
    Ok(())
}

fn same_bits(left: &[f32], right: &[f32]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

fn corrupt(message: &str) -> SQLiteError {
    SQLiteError::StorageBackend(format!("corrupt HNSW graph: {message}"))
}
