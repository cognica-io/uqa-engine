//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical input, complete authority/syntax construction and canonical declarations share one retained load-only operation.

mod types;
pub use types::RoutineCatalogTypeNames;

use super::{
    CatalogFacade, CatalogInputLimits, ImageDecodeWindow, ImageDecoder, MemoryBudget,
    StorageBackendError, StorageBackendResult, StorageReadControl,
};
use std::sync::Arc;
use uqa_sql::{
    catalog::{roles::RoleDefinitions, security::schema_registry::SchemaRegistry},
    routines::lifecycle::restoration::{CanonicalRoutineDefinitions, RoutineTypeIdentityInput},
};

mod placeholders;
pub use placeholders::RoutineCatalogPlaceholderInput;

/// The selected immutable schema generation and Execution-owned type lookup remain alive until complete canonicalization or terminal retirement.
pub trait RoutineRestoreSchemaInput: RoutineTypeIdentityInput<Error = StorageBackendError> {
    fn schemas(&self) -> &SchemaRegistry;
}

pub struct RoutineCatalogCanonicalInput {
    input: ImageDecoder<CanonicalRoutineDefinitions, StorageBackendError>,
}

impl RoutineCatalogCanonicalInput {
    pub fn new<R: RoutineRestoreSchemaInput + 'static>(
        catalog: Arc<dyn CatalogFacade>,
        roles: RoleDefinitions,
        names: R,
        limits: CatalogInputLimits,
        memory: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let native = memory.clone();
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                read_canonical(
                    catalog, roles, names, limits, &native, &retained, &mut input,
                )
                .await
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<CanonicalRoutineDefinitions>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn read_canonical<R: RoutineRestoreSchemaInput>(
    catalog: Arc<dyn CatalogFacade>,
    roles: RoleDefinitions,
    mut names: R,
    limits: CatalogInputLimits,
    memory: &MemoryBudget,
    control: &StorageReadControl,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> StorageBackendResult<CanonicalRoutineDefinitions> {
    let document =
        super::read_physical(catalog.as_ref(), limits, false, memory, control, input).await?;
    drop(catalog);
    let authority = document
        .bind_authority_from(&roles, memory, control.cancellation(), input)
        .await?;
    drop(roles);
    let definitions = authority
        .decode_from(memory, control.cancellation(), input)
        .await?;
    definitions
        .canonicalize_from(&mut names, memory, control.cancellation(), input)
        .await
}

#[cfg(test)]
mod tests;
