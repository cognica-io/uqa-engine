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
mod sequence_declarations;
mod sequences;

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

#[cfg(test)]
mod tests;
