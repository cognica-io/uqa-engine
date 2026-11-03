//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original definitions and nontransactional value history share one capture boundary.

use super::{catalog, CapturedSequence};
use crate::catalog::sequence::{
    session::{
        NontransactionalSequenceValue, NontransactionalSequenceValues, SessionSequenceValue,
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
        Self {
            definitions: Some(definitions),
            values: NontransactionalSequenceValues::new(),
        }
    }

    pub fn values(&self) -> &NontransactionalSequenceValues {
        &self.values
    }

    pub fn record(
        &mut self,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        session_currval: Option<SessionSequenceValue>,
        defines_lastval: bool,
    ) {
        if defines_lastval {
            for history in self.values.values_mut() {
                history.defines_lastval = false;
            }
        }
        let history = self.values.entry(value.object_id).or_default();
        let preserves_lastval =
            !defines_lastval && history.object_id == value.object_id && history.defines_lastval;
        history
            .values_by_definition
            .insert(definition_generation, value);
        history.object_id = value.object_id;
        history.session_currval = session_currval;
        history.defines_lastval = defines_lastval || preserves_lastval;
    }

    pub fn discard_session_values(&mut self) {
        for history in self.values.values_mut() {
            history.session_currval = None;
            history.defines_lastval = false;
        }
    }
}

impl MemoryCaptureSource for SequenceCaptureSource {
    type Frozen = CapturedMemoryValue<Vec<CapturedSequence>>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let definitions = self.definitions.as_ref().ok_or_else(|| {
            StorageBackendError::Other("sequence history has no selected definitions".into())
        })?;
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let captured = catalog::with_values(
            definitions,
            |object_id, mut state| {
                if let Some(value) = self.values.get(&object_id).and_then(|history| {
                    history
                        .values_by_definition
                        .get(&state.definition_generation)
                }) {
                    state.current = value.current;
                    state.called = value.called;
                    state.log_count = value.log_count;
                }
                state
            },
            &production,
        )
        .map_err(|error| StorageBackendError::backend("sequence value capture", error))?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("sequence capture owns its allowance"));
        CapturedMemoryValue::new(captured, control)
    }
}

#[cfg(test)]
mod tests;
