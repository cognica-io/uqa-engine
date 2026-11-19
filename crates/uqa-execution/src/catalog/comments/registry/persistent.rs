//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh private comment generations retain every physical row before interpreting catalog addresses.

use super::CommentRegistry;
use crate::catalog::comments::{address, PREFIX};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryBudget},
};
use uqa_storage::{
    catalog::{
        input::CatalogInputLimits,
        text_input::{CatalogTextRow, CatalogTextStep},
    },
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

pub struct CommentCatalogInput {
    input: ImageDecoder<CommentRegistry, StorageBackendError>,
}

impl CommentCatalogInput {
    /// Reconstruct a fresh private generation under the caller's selected catalog transaction; runtime refresh retains its existing temporary/private overlay boundary.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        limits: CatalogInputLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let mutations = mutations.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                work.checkpoint().await?;
                let mut source = catalog.begin_metadata_input(PREFIX, limits, &retained)?;
                let mut rows = BudgetedVec::new(retained.memory());
                loop {
                    work.checkpoint().await?;
                    match source.advance()? {
                        CatalogTextStep::Pending => (),
                        CatalogTextStep::Row(row) => rows.push(row)?,
                        CatalogTextStep::Complete => break,
                    }
                }
                drop(source);
                let (rows, _rows_memory) = rows.into_parts();
                let mut registry = CommentRegistry::with_budget(retained.memory());
                for row in rows {
                    work.checkpoint().await?;
                    retained.check()?;
                    let object = address(&row.name).ok_or_else(|| {
                        StorageBackendError::Other("invalid catalog comment address".into())
                    })?;
                    if row.value.is_empty() {
                        return Err(StorageBackendError::Other(
                            "invalid catalog comment text".into(),
                        ));
                    }
                    for chunk in row.value.as_bytes().chunks(4096) {
                        work.checkpoint().await?;
                        retained.check()?;
                        if chunk.contains(&0) {
                            return Err(StorageBackendError::Other(
                                "invalid catalog comment text".into(),
                            ));
                        }
                    }
                    let (CatalogTextRow { name, value }, mut memory) = row.into_parts();
                    let payload = memory.split(value.capacity());
                    drop(name);
                    drop(memory);
                    let bytes = payload.bytes();
                    registry
                        .durable
                        .insert(object, Budgeted::new(value, payload), bytes)?;
                }
                retained.check()?;
                Ok(registry.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<CommentRegistry>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

#[cfg(test)]
mod tests;
