//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primary memory graph images preserve evaluated properties and allocation state.

use super::{GraphLabelRegistry, LabelKind, MemoryGraphStore, Partition};
use uqa_core::{memory::Budgeted, Edge, Vertex};
use uqa_storage::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

mod input;
mod output;
mod retention;
#[cfg(test)]
mod tests;
mod validation;

const MAGIC: &[u8; 8] = b"UQAGRA01";

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("invalid memory graph image".into())
}

impl MemoryGraphStore {
    /// Stream the complete primary graph state, including unattached entities, graph memberships, label tombstones and unused allocated identities. Evaluated properties use Core's exact value codec; derived adjacency/label indexes are validated before output and reconstructed by their owner on import. The caller must retain an immutable committed source independently of this encoding operation.
    pub fn write_image(
        &self,
        output: &mut dyn std::io::Write,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        validation::validate(self, control)?;
        output::write(self, output, control)
    }

    /// Reconstruct a fresh unpublished memory store. Complete decoding and relationship validation precede return; errors release the candidate and its admitted payload. The returned owner retains its import allowance until explicitly transferred into an activated database. No SQL, analyzer, expression or user callback is invoked.
    pub fn read_image(
        input: &mut dyn std::io::Read,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        input::read(input, control)
    }
}
