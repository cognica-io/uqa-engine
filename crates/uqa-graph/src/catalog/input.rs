//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native graph input owns decoding, name adoption and writable destination publication.

use super::GraphCatalog;
use crate::MemoryGraphStore;
use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, Produced};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

impl GraphCatalog {
    /// Decode one complete private graph, retain the original admitted name and header, and select its future mutation allowance. Errors consume the unpublished catalog.
    pub fn read_image(
        self,
        name: Budgeted<String>,
        reader: &mut dyn std::io::Read,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        uqa_core::image_io::decode_image(
            reader,
            control.memory(),
            control.cancellation(),
            async |mut input| self.read_image_from(name, mutations, &mut input).await,
        )
    }

    /// Consume the private catalog through complete native input and publication. The caller forwards the original input channel; failure cannot expose or reuse a partially updated catalog.
    pub async fn read_image_from(
        mut self,
        name: Budgeted<String>,
        mutations: &MemoryBudget,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let control = StorageReadControl::new(input.memory, input.cancellation);
        input.checkpoint().await?;
        control.check()?;
        if !name.budget().shares_allowance(control.memory())
            || !self.graphs.budget().shares_allowance(control.memory())
            || name.reserved_bytes() < name.capacity()
            || self.contains_key(name.as_str())
        {
            return Err(StorageBackendError::Other(
                "invalid graph image admission or identity".into(),
            ));
        }
        let graph = MemoryGraphStore::read_image_from(input)
            .await?
            .into_mutation_budget(mutations)
            .into_image_handle(&control)?;
        let bytes = name
            .capacity()
            .checked_add(graph.image_retained_bytes()?)
            .ok_or(MemoryError::SizeOverflow)?;
        self.graphs.insert(Produced::from(name), graph, bytes)?;
        input.checkpoint().await?;
        Ok(self)
    }

    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            graphs: self.graphs.into_mutation_budget(memory),
            ..self
        }
    }
}
