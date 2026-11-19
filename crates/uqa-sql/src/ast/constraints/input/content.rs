//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-owned fields embedded in untagged stored declarations retain generic-content semantics.

use super::{ColumnDef, Input, InputError, JsonToken, MemoryBudget, Produced, SQLError, Values};
use crate::{
    ast::TableCheck,
    catalog::{relation_attributes::DroppedAttribute, relation_oids::RelationCatalogOids},
};
use uqa_core::CancellationToken;

/// Decode original column content with the enclosing declaration's remaining container allowance. Generic content validates discarded fields and preserves signed integer zero before typed selection.
pub fn read_stored_columns_content_json(
    text: &str,
    remaining_depth: usize,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<Vec<ColumnDef>>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    read(text, remaining_depth, &values, |input, token| {
        input.array(token, |input, token| {
            input.column_definition(token, &values)
        })
    })
}

/// Decode CHECK content through the same native expression and identity readers used by ordinary stored table constraints.
pub fn read_stored_checks_content_json(
    text: &str,
    remaining_depth: usize,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<Vec<TableCheck>>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    read(text, remaining_depth, &values, |input, token| {
        input.array(token, |input, token| input.table_check(token, &values))
    })
}

/// Decode physical dropped attributes without changing their order or applying catalog validation.
pub fn read_stored_dropped_attributes_content_json(
    text: &str,
    remaining_depth: usize,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<Vec<DroppedAttribute>>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    read(text, remaining_depth, &values, |input, token| {
        input.array(token, Input::dropped_column)
    })
}

/// Decode the closed relation-OID representation; validity for a particular relation remains with its catalog owner.
pub fn read_stored_relation_oids_content_json(
    text: &str,
    remaining_depth: usize,
    memory: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Result<Produced<RelationCatalogOids>, SQLError> {
    let values = Values {
        memory,
        original,
        invoking,
    };
    read(text, remaining_depth, &values, |input, token| {
        let value = input.relation_oids(token)?;
        input.scalar(value)
    })
}

fn read<'a, T>(
    text: &'a str,
    depth: usize,
    values: &Values<'_>,
    decode: impl FnOnce(&mut Input<'a, '_>, JsonToken<'a>) -> Result<Produced<T>, InputError>,
) -> Result<Produced<T>, SQLError> {
    if !(1..=128).contains(&depth) {
        return Err(InputError::Invalid.into());
    }
    let mut input = Input::content_at_depth(text, values.control(), depth);
    let token = input.next()?;
    input.content(token)?;
    input.end()?;
    let mut input = Input::content_at_depth(text, values.control(), depth);
    let token = input.next()?;
    let result = decode(&mut input, token)?;
    input.end()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
