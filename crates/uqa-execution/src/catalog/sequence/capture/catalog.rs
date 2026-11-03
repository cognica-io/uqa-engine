//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select complete durable sequence records from one immutable catalog generation.

use crate::catalog::sequence::{snapshot::SequenceReadSnapshot, SequenceState};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity,
};
use uqa_sql::{ast::RelationPersistence, catalog::security::BoundSequenceSecurity, SQLError};
use uqa_storage::{
    memory_capture::{CapturedMemoryValue, MemoryCaptureSource},
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult,
};

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedSequence {
    pub relation: RelationIdentity,
    pub object_id: [u8; 16],
    pub catalog_oid: Option<u32>,
    pub persistence: RelationPersistence,
    pub state: SequenceState,
    pub security: BoundSequenceSecurity,
}

fn persistence(
    source: &SequenceReadSnapshot,
    name: &RelationIdentity,
) -> Result<RelationPersistence, SQLError> {
    source
        .persistence
        .get(name)
        .copied()
        .ok_or_else(|| SQLError::Internal("captured sequence has no persistence metadata".into()))
}

/// Join selected definitions with their original identity and authority before copying any output. Temporary records contribute neither names nor payload capacity.
pub fn durable_sequences(
    source: &SequenceReadSnapshot,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<CapturedSequence>>, SQLError> {
    with_values(source, |_, state| state, control)
}

pub(super) fn with_values(
    source: &SequenceReadSnapshot,
    value: impl Fn([u8; 16], SequenceState) -> SequenceState,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<CapturedSequence>>, SQLError> {
    let mut count = 0;
    for name in source.sequences.keys() {
        control.check()?;
        if persistence(source, name)? != RelationPersistence::Temporary {
            count += 1;
        }
    }
    let mut output = ProductionVec::new(*control);
    output.reserve(count)?;
    for (relation, state) in source.sequences.as_ref() {
        control.check()?;
        let persistence = persistence(source, relation)?;
        if persistence == RelationPersistence::Temporary {
            continue;
        }
        let object_id =
            source.object_ids.get(relation).copied().ok_or_else(|| {
                SQLError::Internal("captured sequence has no object identity".into())
            })?;
        let security = source.security.get(relation).ok_or_else(|| {
            SQLError::Internal("captured sequence has no authority metadata".into())
        })?;
        let (schema, schema_memory) = control.copy_text(&relation.schema)?.into_parts();
        let (name, name_memory) = control.copy_text(&relation.name)?.into_parts();
        let (security, security_memory) = security.capture_security(control)?.into_parts();
        output.push_produced(control.finish(
            CapturedSequence {
                relation: RelationIdentity { schema, name },
                object_id,
                catalog_oid: source.catalog_oids.get(&object_id).copied(),
                persistence,
                state: value(object_id, *state),
                security,
            },
            control.combine(control.combine(schema_memory, name_memory), security_memory),
        )?)?;
    }
    Ok(output.finish()?)
}

impl MemoryCaptureSource for SequenceReadSnapshot {
    type Frozen = CapturedMemoryValue<Vec<CapturedSequence>>;

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
