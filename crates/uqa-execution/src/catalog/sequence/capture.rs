//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected sequence generation preserves allocation position and definition identity separately.

use super::SequenceState;
use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity,
};
use uqa_sql::SQLError;

mod catalog;
pub use catalog::{durable_sequences, CapturedSequence};

pub fn sequences(
    values: &BTreeMap<RelationIdentity, SequenceState>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, SequenceState)>>, SQLError> {
    let mut entries = ProductionVec::new(*control);
    entries.reserve(values.len())?;
    for (name, state) in values {
        let (schema, schema_memory) = control.copy_text(&name.schema)?.into_parts();
        let (name, name_memory) = control.copy_text(&name.name)?.into_parts();
        entries.push_produced(control.finish(
            (RelationIdentity { schema, name }, *state),
            control.combine(schema_memory, name_memory),
        )?)?;
    }
    Ok(entries.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    use uqa_sql::ast::SequenceDataType;

    #[test]
    fn sequence_capture_preserves_extreme_positions_definition_and_owner_identity() {
        let source = BTreeMap::from([(
            RelationIdentity::new("Quoted.Schema", "Sequence.Name"),
            SequenceState {
                start: i64::MIN,
                increment: -3,
                current: i64::MAX,
                called: false,
                log_count: 31,
                data_type: SequenceDataType::default(),
                min_value: i64::MIN,
                max_value: i64::MAX,
                cycle: true,
                cache_size: 13,
                definition_generation: [7; 16],
                owner: Some(uqa_storage::SequenceOwner {
                    table_object_id: [8; 16],
                    column_object_id: [9; 16],
                    dependency: uqa_core::catalog_sequence::SequenceOwnerDependency::Internal,
                }),
            },
        )]);
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let captured = sequences(&source, &control).unwrap();
        assert!(captured.iter().map(|(k, v)| (k, v)).eq(source.iter()));
        drop(source);
        assert_eq!(captured[0].1.current, i64::MAX);
        assert!(!captured[0].1.called);
        drop(captured);
        assert_eq!(memory.used(), 0);
        let value = SequenceState {
            start: 1,
            increment: 1,
            current: 43,
            called: true,
            log_count: 0,
            data_type: SequenceDataType::default(),
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            cache_size: 1,
            definition_generation: [1; 16],
            owner: None,
        };
        let source = BTreeMap::from([(RelationIdentity::new("s", "x".repeat(32768)), value)]);
        assert_eq!(
            sequences(&source, &control).unwrap_err().sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), 0);
        token.cancel();
        assert!(matches!(
            sequences(&source, &control),
            Err(SQLError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
}
