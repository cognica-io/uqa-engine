//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture admits cached complete native roots without visiting graph entities or indexes.

use super::{MemoryGraphStore, StorageBackendResult, StorageReadControl};
use uqa_core::memory::{MemoryError, MemoryReservation};
#[cfg(test)]
use uqa_core::{Value, Vertex};

impl MemoryGraphStore {
    pub(crate) fn image_retention(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<MemoryReservation> {
        control.check()?;
        let bytes = [
            self.vertices.retained_bytes()?,
            self.edges.retained_bytes()?,
            self.graphs.retained_bytes()?,
            self.vertex_membership.retained_bytes()?,
            self.edge_membership.retained_bytes()?,
            self.label_registries.retained_bytes()?,
        ]
        .into_iter()
        .try_fold(size_of::<Self>(), |sum, bytes| {
            sum.checked_add(bytes).ok_or(MemoryError::SizeOverflow)
        })?;
        let memory = control.memory().reserve(bytes)?;
        control.check()?;
        Ok(memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GraphStore;

    #[test]
    fn retained_graph_payload_admission_counts_unused_source_capacity() {
        let mut source = MemoryGraphStore::default();
        source.create_graph("g").unwrap();
        source.add_vertex(Vertex::new(1, "label"), "g").unwrap();
        let control = StorageReadControl::with_limit(2 << 20);
        let initial = source.image_retention(&control).unwrap();
        let initial_bytes = initial.bytes();
        drop(initial);
        let mut value = String::with_capacity(1 << 20);
        value.push('x');
        let capacity = value.capacity();
        let mut vertex = Vertex::new(1, "label");
        vertex
            .properties
            .insert("payload".into(), Value::Str(value));
        source.add_vertex(vertex, "g").unwrap();
        let retained = source.image_retention(&control).unwrap();
        assert!(retained.bytes() >= initial_bytes + capacity);
        drop(retained);
        assert_eq!(control.memory().used(), 0);
        let small = StorageReadControl::with_limit(4096);
        assert!(matches!(
            source.image_retention(&small),
            Err(uqa_storage::StorageBackendError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
    }
}
