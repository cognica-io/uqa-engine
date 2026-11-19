//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete native input prepares a private placeholder namespace before the Engine publication boundary.

use super::{
    read_canonical, Arc, CatalogFacade, CatalogInputLimits, ImageDecodeWindow, ImageDecoder,
    MemoryBudget, RoleDefinitions, RoutineRestoreSchemaInput, StorageBackendError,
    StorageBackendResult, StorageReadControl,
};
use uqa_core::memory::ProductionControl;
use uqa_sql::routines::lifecycle::{restoration::RoutineRestorePlaceholders, RoutineRegistry};

pub struct RoutineCatalogPlaceholderInput {
    input: ImageDecoder<RoutineRestorePlaceholders, StorageBackendError>,
}

impl RoutineCatalogPlaceholderInput {
    /// Select physical input and immutable authority/type generations, complete original validation, then prepare every placeholder and its original finalization order. The previous namespace remains unchanged through failure, cancellation and abandonment.
    pub fn new<R: RoutineRestoreSchemaInput + 'static>(
        catalog: Arc<dyn CatalogFacade>,
        roles: RoleDefinitions,
        names: R,
        previous: RoutineRegistry,
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
                let definitions = read_canonical(
                    catalog, roles, names, limits, &native, &retained, &mut input,
                )
                .await?;
                definitions
                    .prepare_placeholders_from_input(
                        &previous,
                        &native,
                        retained.cancellation(),
                        &mut input,
                    )
                    .await
                    .map_err(|error| {
                        super::super::definitions::restore_error(
                            error,
                            &ProductionControl::new(
                                &native,
                                retained.cancellation(),
                                input.cancellation,
                            ),
                        )
                    })
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<RoutineRestorePlaceholders>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}
