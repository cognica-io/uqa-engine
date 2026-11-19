//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow complete durable sequence records from independently admitted immutable roots.

use crate::catalog::sequence::{
    registry::CapturedSequenceCatalog,
    session::{NontransactionalSequenceValue, SequenceValueKey},
    snapshot::SequenceReadSnapshot,
    SequenceState,
};
use uqa_core::{
    memory::{Produced, ProductionControl, RetainedMapSnapshot},
    RelationIdentity,
};
use uqa_sql::{ast::RelationPersistence, catalog::security::BoundSequenceSecurity, SQLError};
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedSequence<'a> {
    pub relation: &'a RelationIdentity,
    pub object_id: [u8; 16],
    pub catalog_oid: Option<u32>,
    pub persistence: RelationPersistence,
    pub state: SequenceState,
    pub security: &'a BoundSequenceSecurity,
}

pub struct CapturedSequences {
    pub(super) catalog: CapturedSequenceCatalog,
    pub(super) values: Option<RetainedMapSnapshot<SequenceValueKey, NontransactionalSequenceValue>>,
}

impl CapturedSequences {
    /// The SQL owner encodes complete authority from the same captured generation as the sequence metadata image. Both images are required; neither rebinds a role name.
    pub fn authority_image_reader<'a>(
        &'a self,
        cancellation: &'a uqa_core::CancellationToken,
    ) -> Result<uqa_sql::catalog::image::CatalogImageReader<'a>, SQLError> {
        self.catalog.security.image_reader(cancellation)
    }

    pub fn len(&self) -> usize {
        self.catalog.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.catalog.records.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = CapturedSequence<'_>> {
        self.catalog.records.iter().map(|(relation, record)| {
            let mut state = record.state;
            if let Some(value) = self
                .values
                .as_ref()
                .and_then(|values| values.get(&(record.object_id, state.definition_generation)))
            {
                state.current = value.current;
                state.called = value.called;
                state.log_count = value.log_count;
            }
            CapturedSequence {
                relation,
                object_id: record.object_id,
                catalog_oid: record.catalog_oid,
                persistence: record.persistence,
                state,
                security: self
                    .catalog
                    .security
                    .get(relation)
                    .expect("captured complete sequence authority")
                    .security(),
            }
        })
    }
}

impl std::fmt::Debug for CapturedSequences {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

pub fn durable_sequences(
    source: &SequenceReadSnapshot,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedSequences>, SQLError> {
    let (catalog, memory) = source.catalog.capture(control)?.into_parts();
    Ok(control.finish(
        CapturedSequences {
            catalog,
            values: None,
        },
        memory,
    )?)
}

impl MemoryCaptureSource for SequenceReadSnapshot {
    type Frozen = CapturedMemoryValue<CapturedSequences>;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let value = durable_sequences(self, &production)
            .map_err(|error| StorageBackendError::backend("sequence catalog capture", error))?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("sequence capture uses admitted production"));
        CapturedMemoryValue::new(value, control)
    }
}

#[cfg(test)]
mod tests;
