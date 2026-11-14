//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical input, complete authority/syntax construction and canonical declarations share one retained load-only operation.

use super::{
    CatalogFacade, CatalogInputLimits, ImageDecodeWindow, ImageDecoder, MemoryBudget,
    StorageBackendError, StorageBackendResult, StorageReadControl,
};
use std::sync::Arc;
use uqa_sql::{
    catalog::{roles::RoleDefinitions, security::schema_registry::SchemaRegistry},
    routines::lifecycle::restoration::{CanonicalRoutineDefinitions, RoutineTypeIdentityInput},
};

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
        mut names: R,
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
                let document = super::read_physical(
                    catalog.as_ref(),
                    limits,
                    false,
                    &native,
                    &retained,
                    &mut input,
                )
                .await?;
                drop(catalog);
                let authority = document
                    .bind_authority_from(&roles, &native, retained.cancellation(), &mut input)
                    .await?;
                drop(roles);
                let definitions = authority
                    .decode_from(&native, retained.cancellation(), &mut input)
                    .await?;
                definitions
                    .canonicalize_from(&mut names, &native, retained.cancellation(), &mut input)
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

#[cfg(test)]
mod tests;
