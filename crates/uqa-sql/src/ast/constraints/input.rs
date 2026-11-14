//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored column declarations preserve native syntax without catalog lookup or SQL execution.

use super::ColumnDef;
use crate::{
    ast::stored_input::{Input, InputError, Parts, Values},
    SQLError,
};
use uqa_core::{
    json::JsonToken,
    memory::{MemoryBudget, Produced},
    CancellationToken,
};

mod columns;
mod foreign;
mod foreign_keys;
mod hierarchy;
mod partitions;
mod relation_metadata;
mod sequence_declarations;
mod sequences;
mod table_constraints;
pub(crate) use table_constraints::ParsedTableConstraints;
mod table_entries;

/// Read the existing ordered column JSON with admitted native metadata, scalar schema expressions and value payloads. This preserves declarations; semantic validation belongs to the catalog owner.
pub fn read_stored_columns_json(
    text: &str,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<Vec<ColumnDef>>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    let mut input = Input::new(text, values.control());
    let token = input.next()?;
    let result = input.array(token, |input, token| {
        input.column_definition(token, &values)
    })?;
    input.end()?;
    Ok(result)
}

/// Read stored table constraints and hierarchy without resolving names, evaluating bounds or publishing catalog state.
pub fn read_stored_table_constraints_json(
    text: &str,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<super::TableConstraintSet>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    let mut input = Input::new(text, values.control());
    let token = input.next()?;
    let result = input.table_constraints(token, &values)?;
    input.end()?;
    Ok(result)
}

pub(crate) fn read_stored_table_constraint_fields_json(
    text: &str,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<ParsedTableConstraints, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    let mut input = Input::new(text, values.control());
    let token = input.next()?;
    let result = input.table_constraint_fields(token, &values)?;
    input.end()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
