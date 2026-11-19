//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory and persistent restore validate the same immutable catalog before publication.

use crate::{state::SessionNamespace, Engine};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};
use uqa_execution::catalog::{
    image_validation::{DeclarationTypeLookup, RelationshipValidation, VariadicLookup},
    projection::{routine_variadic_array_from_input, RoutineTypeNamespace},
    CatalogReadView, RelationNameResolution,
};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub(super) enum CatalogValidation {
    Capture(ImageDecoder<CatalogReadView, StorageBackendError>),
    Relationships(RelationshipValidation),
}

impl CatalogValidation {
    pub(super) fn begin(
        engine: &Engine,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let selected = engine.select_restore_catalog(&engine.durable.snapshot());
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                selected
                    .read(
                        retained.memory(),
                        retained.cancellation(),
                        &mut reader.input(),
                    )
                    .await
                    .map_err(sql_error)
            },
        )?;
        Ok(Self::Capture(input))
    }

    /// Advance immutable table capture or the next ordered catalog relationship check.
    pub(super) fn advance(
        self,
        engine: &Engine,
        control: &StorageReadControl,
        steps: usize,
    ) -> StorageBackendResult<Option<Self>> {
        control.check()?;
        match self {
            Self::Capture(mut input) => {
                let Some(catalog) =
                    input.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })?
                else {
                    return Ok(Some(Self::Capture(input)));
                };
                let production = ProductionControl::new(
                    control.memory(),
                    control.cancellation(),
                    control.cancellation(),
                );
                let catalog = engine
                    .with_sequence_positions_with_control(catalog, &production)
                    .map_err(sql_error)?;
                Ok(Some(Self::Relationships(RelationshipValidation::new(
                    catalog,
                    control.memory(),
                    control.cancellation(),
                ))))
            }
            Self::Relationships(checks) => {
                let next = checks
                    .advance_with_declaration_types(engine, engine, engine, steps)
                    .map_err(sql_error)?;
                control.check()?;
                Ok(next.map(Self::Relationships))
            }
        }
    }
}

impl DeclarationTypeLookup for Engine {
    fn variadic_lookup(
        &self,
        catalog: &CatalogReadView,
        canonical: Produced<String>,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<VariadicLookup, SQLError> {
        let catalog = catalog.clone();
        let namespace = self.session.namespace_snapshot(self.session_id);
        let original = cancellation.clone();
        let input = ImageDecoder::new(memory, cancellation, |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(input.memory, &original, input.cancellation);
            routine_variadic_array_from_input(
                &catalog, &namespace, &canonical, &control, &mut input,
            )
            .await
        })?;
        Ok(VariadicLookup::Reading(input))
    }
}

impl RoutineTypeNamespace for SessionNamespace {
    type Namespace<'a> = Produced<RelationNameResolution>;
    async fn capture<'a>(
        &'a self,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self::Namespace<'a>, SQLError> {
        self.capture_from_input(control, input).await
    }
}

fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    uqa_execution::storage_errors::catalog_read_error("backup catalog validation", error)
}

#[cfg(test)]
mod tests;
