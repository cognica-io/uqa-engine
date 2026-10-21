//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact owned-node and tensor-buffer sizes advance with the immutable memory root.

use std::sync::OnceLock;
use uqa_core::{memory::OwnedMap, DocId};

#[derive(Debug, Default)]
pub(super) struct MemoryVectorState {
    pub(super) vectors: OwnedMap<DocId, Vec<Vec<f32>>>,
    payload_bytes: u128,
    pub(super) vector_count: u128,
    pub(super) fingerprint: OnceLock<[u8; 32]>,
}

fn tensor_bytes(vectors: &Vec<Vec<f32>>) -> u128 {
    vectors.capacity() as u128 * size_of::<Vec<f32>>() as u128
        + vectors
            .iter()
            .map(|vector| vector.capacity() as u128 * size_of::<f32>() as u128)
            .sum::<u128>()
}

impl MemoryVectorState {
    pub(super) fn retained_bytes(&self) -> u128 {
        size_of::<Self>() as u128 + self.vectors.allocated_bytes() as u128 + self.payload_bytes
    }

    pub(super) fn replace(&mut self, document: DocId, vectors: Vec<Vec<f32>>) {
        self.fingerprint.take();
        let previous = if vectors.is_empty() {
            self.vectors.remove(&document)
        } else {
            self.payload_bytes += tensor_bytes(&vectors);
            self.vector_count += vectors.len() as u128;
            self.vectors.insert(document, vectors)
        };
        if let Some(previous) = previous {
            self.payload_bytes -= tensor_bytes(&previous);
            self.vector_count -= previous.len() as u128;
        }
    }
}

impl Clone for MemoryVectorState {
    fn clone(&self) -> Self {
        let vectors = self.vectors.clone();
        // Cloned buffers can have smaller capacities; the new root owns its own exact charge.
        let payload_bytes = vectors.values().map(tensor_bytes).sum();
        Self {
            vectors,
            payload_bytes,
            vector_count: self.vector_count,
            fingerprint: self.fingerprint.clone(),
        }
    }
}
