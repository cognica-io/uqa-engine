//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native catalogs register their owning immutable generations in the fresh Engine.

use crate::state::{CatalogCell, CatalogGeneration, DurableCatalogState};
use std::io::Read;
use uqa_core::{image_io::ImageDecodeWindow, memory::MemoryBudget};
use uqa_execution::catalog::sequence::registry::{SequenceCatalog, SequenceCatalogInput};
use uqa_storage::{
    memory_capture::MemoryCaptureRegistry, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};

mod definitions;
use definitions::DefinitionInput;

#[cfg(test)]
mod tests;

pub(super) struct CatalogInput {
    next: u16,
    sequence: Option<SequenceCatalogInput>,
    definition: Option<DefinitionInput>,
}

pub(super) struct Destination<'a> {
    pub registry: &'a MemoryCaptureRegistry,
    pub mutations: &'a MemoryBudget,
    pub control: &'a StorageReadControl,
    pub window: ImageDecodeWindow,
}

impl CatalogInput {
    pub(super) fn new() -> Self {
        Self {
            next: 1,
            sequence: None,
            definition: None,
        }
    }

    pub(super) fn read_step(
        mut self,
        state: &mut DurableCatalogState,
        slot: u16,
        reader: &mut dyn Read,
        destination: &Destination<'_>,
    ) -> StorageBackendResult<(Self, bool)> {
        let Destination {
            registry,
            mutations,
            control,
            ..
        } = *destination;
        control.check()?;
        if slot != self.next {
            return Err(invalid());
        }
        match slot {
            1..=18 | 21..=24 => {
                let mut definition = match self.definition.take() {
                    Some(definition) => definition,
                    None => DefinitionInput::new(slot, destination)?,
                };
                if !definition.advance(state, reader, destination)? {
                    self.definition = Some(definition);
                    return Ok((self, false));
                }
            }
            19 => {
                if self.sequence.is_none() {
                    self.sequence = Some(
                        SequenceCatalogInput::new(
                            control.memory(),
                            mutations,
                            control.cancellation(),
                        )
                        .map_err(super::metadata::sql_error)?,
                    );
                }
                if !self
                    .sequence
                    .as_mut()
                    .expect("active sequence metadata")
                    .advance_metadata(reader, destination.window)
                    .map_err(super::metadata::sql_error)?
                {
                    return Ok((self, false));
                }
            }
            20 => {
                let Some(value) = self
                    .sequence
                    .as_mut()
                    .ok_or_else(invalid)?
                    .advance_authority(reader, destination.window)
                    .map_err(super::metadata::sql_error)?
                else {
                    return Ok((self, false));
                };
                self.sequence = None;
                state.sequences = CatalogCell::capture_source_with_control(
                    CatalogGeneration::with_budget(value, control.memory())?,
                    registry,
                    SequenceCatalog::capture,
                    Some(control),
                )?;
            }
            _ => return Err(invalid()),
        }
        control.check()?;
        self.next += 1;
        Ok((self, true))
    }

    pub(super) fn finish(self) -> StorageBackendResult<()> {
        if self.next != uqa_storage::backup::MEMORY_IMAGE_CATALOG_COUNT + 1
            || self.sequence.is_some()
            || self.definition.is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Other("incomplete or unordered native catalog images".into())
}
