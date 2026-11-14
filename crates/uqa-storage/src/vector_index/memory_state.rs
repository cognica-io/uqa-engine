//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact owned-node and tensor-buffer sizes advance with the immutable memory root.

use std::sync::OnceLock;
use uqa_core::{
    memory::{MemoryReservation, OwnedMap},
    DocId,
};

#[derive(Debug, Default)]
pub(super) struct MemoryVectorState {
    pub(super) vectors: OwnedMap<DocId, Vec<Vec<f32>>>,
    payload_bytes: u128,
    pub(super) vector_count: u128,
    pub(super) fingerprint: OnceLock<[u8; 32]>,
    pub(super) image_memory: Option<MemoryReservation>,
}

fn tensor_bytes(vectors: &Vec<Vec<f32>>) -> u128 {
    crate::image_io::work::inline(tensor_bytes_from::<std::convert::Infallible>(
        vectors,
        &mut (),
    ))
    .unwrap_or_else(|never| match never {})
}

async fn tensor_bytes_from<E>(
    vectors: &Vec<Vec<f32>>,
    work: &mut impl crate::image_io::work::Work<E>,
) -> Result<u128, E> {
    let mut bytes = vectors.capacity() as u128 * size_of::<Vec<f32>>() as u128;
    for vector in vectors {
        work.checkpoint().await?;
        bytes += vector.capacity() as u128 * size_of::<f32>() as u128;
    }
    Ok(bytes)
}

impl MemoryVectorState {
    pub(super) fn retained_bytes(&self) -> u128 {
        size_of::<Self>() as u128 + self.vectors.allocated_bytes() as u128 + self.payload_bytes
    }

    pub(super) fn replace(&mut self, document: DocId, vectors: Vec<Vec<f32>>) {
        crate::image_io::work::inline(self.replace_from::<std::convert::Infallible>(
            document,
            vectors,
            &mut (),
        ))
        .unwrap_or_else(|never| match never {});
    }

    pub(super) async fn replace_from<E>(
        &mut self,
        document: DocId,
        vectors: Vec<Vec<f32>>,
        work: &mut impl crate::image_io::work::Work<E>,
    ) -> Result<(), E> {
        let new_bytes = tensor_bytes_from(&vectors, work).await?;
        let old_bytes = match self.vectors.get(&document) {
            Some(previous) => tensor_bytes_from(previous, work).await?,
            None => 0,
        };
        self.fingerprint.take();
        let previous = if vectors.is_empty() {
            self.vectors.remove(&document)
        } else {
            self.payload_bytes += new_bytes;
            self.vector_count += vectors.len() as u128;
            self.vectors.insert(document, vectors)
        };
        if let Some(previous) = previous {
            self.payload_bytes -= old_bytes;
            self.vector_count -= previous.len() as u128;
        }
        Ok(())
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
            image_memory: None,
        }
    }
}
