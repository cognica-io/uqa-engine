//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original definitions and nontransactional value history share one capture boundary.

use super::CapturedSequences;
use crate::catalog::sequence::{
    session::{
        NontransactionalSequenceValue, NontransactionalSequenceValues, PreparedSequenceHistory,
        SessionSequenceValue,
    },
    snapshot::SequenceReadSnapshot,
};
use uqa_core::memory::ProductionControl;
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

/// The transaction owner records every value operation here for rollback. Capture joins only the original durable object and definition incarnations; session observations and private definitions never become image records. The default owner keeps rollback history without retaining definitions and cannot produce a capture.
#[derive(Default)]
pub struct SequenceCaptureSource {
    definitions: Option<SequenceReadSnapshot>,
    values: NontransactionalSequenceValues,
}

impl SequenceCaptureSource {
    pub fn new(definitions: SequenceReadSnapshot) -> Self {
        let values = NontransactionalSequenceValues::with_budget(definitions.catalog.budget());
        Self {
            definitions: Some(definitions),
            values,
        }
    }

    pub fn values(&self) -> &NontransactionalSequenceValues {
        &self.values
    }

    pub fn prepare(
        &mut self,
        object: [u8; 16],
        definition: [u8; 16],
    ) -> Result<PreparedSequenceHistory<'_>, uqa_sql::SQLError> {
        let original = self.definitions.as_ref().is_some_and(|source| {
            source
                .catalog
                .contains_durable_definition(&object, &definition)
        });
        Ok(self.values.prepare((object, definition), original)?)
    }

    pub fn record(
        &mut self,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        session_currval: Option<SessionSequenceValue>,
        defines_lastval: bool,
    ) -> Result<(), uqa_sql::SQLError> {
        self.prepare(value.object_id, definition_generation)?
            .publish(value, session_currval, defines_lastval);
        Ok(())
    }

    pub fn discard_session_values(&mut self) {
        self.values.discard_session_values();
    }
}

impl MemoryCaptureSource for SequenceCaptureSource {
    type Frozen = CapturedMemoryValue<CapturedSequences>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let definitions = self.definitions.as_ref().ok_or_else(|| {
            StorageBackendError::Other("sequence history has no selected definitions".into())
        })?;
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let (catalog, catalog_memory) = definitions
            .catalog
            .capture(&production)
            .map_err(|error| StorageBackendError::backend("sequence catalog capture", error))?
            .into_parts();
        let (values, value_memory) = self.values.capture_original(control.memory())?.into_parts();
        let captured = production
            .finish(
                CapturedSequences {
                    catalog,
                    values: Some(values),
                },
                production.combine(catalog_memory, Some(value_memory)),
            )?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("sequence capture owns its allowance"));
        CapturedMemoryValue::new(captured, control)
    }
}

#[cfg(test)]
mod tests;
