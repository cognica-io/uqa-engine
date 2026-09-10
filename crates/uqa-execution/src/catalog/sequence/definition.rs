//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply SQL declarations to mutable sequence allocation state.
use super::SequenceState;
use uqa_sql::ast::{SequenceBound, SequenceDataType, SequenceRestart};
use uqa_sql::schema::sequences::definition::{
    altered_sequence_definition, validate_sequence_definition, SequenceDefinition,
};
use uqa_sql::SQLError;

impl SequenceState {
    #[must_use]
    pub fn definition(&self) -> SequenceDefinition {
        SequenceDefinition {
            start: self.start,
            increment: self.increment,
            data_type: self.data_type,
            min_value: self.min_value,
            max_value: self.max_value,
            cycle: self.cycle,
            cache_size: self.cache_size,
        }
    }

    #[must_use]
    pub fn from_definition(definition: SequenceDefinition) -> Self {
        Self {
            start: definition.start,
            increment: definition.increment,
            data_type: definition.data_type,
            min_value: definition.min_value,
            max_value: definition.max_value,
            cycle: definition.cycle,
            cache_size: definition.cache_size,
            current: definition.start,
            called: false,
            log_count: 0,
            definition_generation: [0; 16],
            owner: None,
        }
    }

    #[must_use]
    pub fn initial(start: i64, increment: i64, data_type: SequenceDataType) -> Self {
        Self::from_definition(SequenceDefinition::initial(start, increment, data_type))
    }

    fn with_definition(mut self, definition: SequenceDefinition) -> Self {
        self.start = definition.start;
        self.increment = definition.increment;
        self.data_type = definition.data_type;
        self.min_value = definition.min_value;
        self.max_value = definition.max_value;
        self.cycle = definition.cycle;
        self.cache_size = definition.cache_size;
        self
    }
}

pub fn altered_sequence_state(
    mut state: SequenceState,
    alter: &uqa_sql::ast::AlterSequence,
) -> Result<SequenceState, SQLError> {
    let resets_log_count = alter.data_type.is_some()
        || alter.increment.is_some()
        || alter.min_value != SequenceBound::Unchanged
        || alter.max_value != SequenceBound::Unchanged
        || alter.cycle.is_some()
        || alter.cache_size.is_some()
        || alter.restart != SequenceRestart::Unchanged;
    state = state.with_definition(altered_sequence_definition(state.definition(), alter));
    if alter.restart != SequenceRestart::Unchanged {
        let restart_val = match alter.restart {
            SequenceRestart::Unchanged => unreachable!("restart action was checked above"),
            SequenceRestart::FromStart => state.start,
            SequenceRestart::With(value) => value,
        };
        state.current = restart_val;
        state.called = false;
    }
    if resets_log_count {
        state.log_count = 0;
    }
    validate_sequence_definition(&state.definition(), Some(state.current))?;
    Ok(state)
}
