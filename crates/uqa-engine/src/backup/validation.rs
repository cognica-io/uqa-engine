//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory and persistent restore validate the same immutable catalog before publication.

use crate::Engine;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::ProductionControl,
};
use uqa_execution::catalog::{image_validation::RelationshipValidation, CatalogReadView};
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

    /// Advance table capture, one native predicate, or a window of foreign binding validation.
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
                    .advance_with_steps(engine, engine, steps)
                    .map_err(sql_error)?;
                control.check()?;
                Ok(next.map(Self::Relationships))
            }
        }
    }
}

fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error.primary() {
        uqa_sql::SQLError::Cancelled(reason) => (*reason).into(),
        _ => StorageBackendError::backend("backup catalog validation", error),
    }
}

#[cfg(test)]
mod tests;
