//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primary memory graph images preserve evaluated properties and allocation state.

#[cfg(test)]
use super::GraphLabelRegistry;
use super::{LabelKind, MemoryGraphStore, NativeLabels, Partition};
use uqa_core::{Edge, Vertex};
use uqa_storage::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod capture;
mod input;
mod output;
mod retention;
mod stream;
#[cfg(test)]
mod tests;
mod validation;

const MAGIC: &[u8; 8] = b"UQAGRA01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory graph image".into())
}

impl MemoryGraphStore {
    /// Transfer only future native mutations. Selected child partitions, label registries and membership sets inherit this allowance when their owning mutation boundary prepares a successor; existing payloads and roots retain their input reservations.
    pub fn into_mutation_budget(self, memory: &uqa_core::memory::MemoryBudget) -> Self {
        Self {
            vertices: self.vertices.into_mutation_budget(memory),
            edges: self.edges.into_mutation_budget(memory),
            graphs: self.graphs.into_mutation_budget(memory),
            vertex_membership: self.vertex_membership.into_mutation_budget(memory),
            edge_membership: self.edge_membership.into_mutation_budget(memory),
            label_registries: self.label_registries.into_mutation_budget(memory),
            ..self
        }
    }

    pub(crate) fn into_image_handle(
        mut self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<std::sync::Arc<crate::GraphStoreHandle>> {
        control.check()?;
        self.image_memory = Some(
            control
                .memory()
                .reserve(size_of::<crate::GraphStoreHandle>())?,
        );
        Ok(std::sync::Arc::new(crate::GraphStoreHandle::Memory(self)))
    }

    /// Open an admitted borrowed image reader without visiting graph entities. `prepare` validates bounded positions before output; synchronous `read` completes any remaining preparation. The selected immutable source must outlive the reader.
    pub fn image_reader<'a>(
        &'a self,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<uqa_storage::image_io::ImageReader<'a>> {
        uqa_storage::image_io::ImageReader::new(
            stream::GraphImageCursor::new(self, control),
            control,
        )
    }

    /// Stream the complete primary graph state, including unattached entities, graph memberships, label tombstones and unused allocated identities. Evaluated properties use Core's exact value codec; derived adjacency/label indexes are validated before output and reconstructed by their owner on import. The caller must retain an immutable committed source independently of this encoding operation.
    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        validation::validate(self, control)?;
        output::write(self, output, control)
    }

    /// Reconstruct a fresh unpublished memory store. Complete decoding and relationship validation precede return; errors release the candidate and its admitted payload. The returned native roots retain their import allowance through cloning, mutation and source retirement. No SQL, analyzer, expression or user callback is invoked.
    pub fn read_image(
        input: &mut dyn std::io::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        input::read(input, control)
    }
}
