//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalize and validate SQL sequence declarations independently of allocation state.
use crate::ast::{AlterSequence, CreateSequence, SequenceBound, SequenceDataType};
use crate::SQLError;

/// Fully specified SQL sequence options after defaults and ALTER actions are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceDefinition {
    pub start: i64,
    pub increment: i64,
    pub data_type: SequenceDataType,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
    pub cache_size: i64,
}

impl SequenceDefinition {
    #[must_use]
    pub fn initial(start: i64, increment: i64, data_type: SequenceDataType) -> Self {
        let (type_min, type_max) = data_type.bounds();
        Self {
            start,
            increment,
            data_type,
            min_value: if increment > 0 { 1 } else { type_min },
            max_value: if increment > 0 { type_max } else { -1 },
            cycle: false,
            cache_size: 1,
        }
    }

    #[must_use]
    pub fn from_create(sequence: &CreateSequence) -> Self {
        let mut definition = Self::initial(sequence.start, sequence.increment, sequence.data_type);
        definition.min_value = sequence.min_value.unwrap_or(definition.min_value);
        definition.max_value = sequence.max_value.unwrap_or(definition.max_value);
        definition.cycle = sequence.cycle;
        definition.cache_size = sequence.cache_size;
        definition
    }
}

#[must_use]
pub fn altered_sequence_definition(
    mut state: SequenceDefinition,
    alter: &AlterSequence,
) -> SequenceDefinition {
    if let Some(data_type) = alter.data_type {
        let (old_type_min, old_type_max) = state.data_type.bounds();
        let (new_type_min, new_type_max) = data_type.bounds();
        if state.min_value == old_type_min {
            state.min_value = new_type_min;
        }
        if state.max_value == old_type_max {
            state.max_value = new_type_max;
        }
        state.data_type = data_type;
    }
    if let Some(increment) = alter.increment {
        state.increment = increment;
    }
    let (type_min, type_max) = state.data_type.bounds();
    match alter.min_value {
        SequenceBound::Unchanged => {}
        SequenceBound::Default => {
            state.min_value = if state.increment > 0 { 1 } else { type_min };
        }
        SequenceBound::Value(value) => state.min_value = value,
    }
    match alter.max_value {
        SequenceBound::Unchanged => {}
        SequenceBound::Default => {
            state.max_value = if state.increment > 0 { type_max } else { -1 };
        }
        SequenceBound::Value(value) => state.max_value = value,
    }
    if let Some(start_val) = alter.start {
        state.start = start_val;
    }
    if let Some(cycle) = alter.cycle {
        state.cycle = cycle;
    }
    if let Some(cache_size) = alter.cache_size {
        state.cache_size = cache_size;
    }
    state
}

pub fn validate_sequence_definition(
    state: &SequenceDefinition,
    current: Option<i64>,
) -> Result<(), SQLError> {
    let validate_current = current.is_some();
    let current = current.unwrap_or_default();
    let invalid = |message| SQLError::Routine {
        sqlstate: "22023".into(),
        message,
    };
    if state.increment == 0 {
        return Err(invalid("INCREMENT must not be zero".into()));
    }
    if state.cache_size <= 0 {
        return Err(invalid(format!(
            "CACHE ({}) must be greater than zero",
            state.cache_size
        )));
    }
    let (type_min, type_max) = state.data_type.bounds();
    if !(type_min..=type_max).contains(&state.max_value) {
        return Err(invalid(format!(
            "MAXVALUE ({}) is out of range for sequence data type {}",
            state.max_value,
            state.data_type.sql_name()
        )));
    }
    if !(type_min..=type_max).contains(&state.min_value) {
        return Err(invalid(format!(
            "MINVALUE ({}) is out of range for sequence data type {}",
            state.min_value,
            state.data_type.sql_name()
        )));
    }
    if state.min_value >= state.max_value {
        return Err(invalid(format!(
            "MINVALUE ({}) must be less than MAXVALUE ({})",
            state.min_value, state.max_value
        )));
    }
    if state.start < state.min_value {
        return Err(invalid(format!(
            "START value ({}) cannot be less than MINVALUE ({})",
            state.start, state.min_value
        )));
    }
    if state.start > state.max_value {
        return Err(invalid(format!(
            "START value ({}) cannot be greater than MAXVALUE ({})",
            state.start, state.max_value
        )));
    }
    if validate_current && current < state.min_value {
        return Err(invalid(format!(
            "RESTART value ({}) cannot be less than MINVALUE ({})",
            current, state.min_value
        )));
    }
    if validate_current && current > state.max_value {
        return Err(invalid(format!(
            "RESTART value ({}) cannot be greater than MAXVALUE ({})",
            current, state.max_value
        )));
    }
    Ok(())
}
