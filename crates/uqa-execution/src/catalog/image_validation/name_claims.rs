//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join native relation and type declarations from one immutable catalog for SQL name validation.

use crate::catalog::CatalogReadSnapshot;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{catalog::namespace_names::NativeNameInput, SQLError};

pub fn validate(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let tables = snapshot.tables.iter().map(|(name, table)| {
        NativeNameInput::RowRelation(name, table.row_type_array_name.as_deref())
    });
    let views = d.views.iter().map(|(name, view)| {
        NativeNameInput::RowRelation(name, view.row_type_array_name.as_deref())
    });
    let foreign = d.foreign_tables.iter().map(|(name, table)| {
        NativeNameInput::RowRelation(name, table.row_type_array_name.as_deref())
    });
    let sequences = d
        .sequences
        .iter()
        .map(|(name, _)| NativeNameInput::Relation(name));
    let indexes = d
        .catalog_indexes
        .iter()
        .map(|(name, _)| NativeNameInput::Relation(name));
    let composites = d.composites.values().map(|definition| {
        NativeNameInput::RowRelation(&definition.identity, Some(&definition.array_name))
    });
    let domains = d.domains.values().map(|definition| {
        NativeNameInput::TypeWithArray(&definition.identity, definition.array_name.as_deref())
    });
    let enums = d.enums.values().map(|definition| {
        NativeNameInput::TypeWithArray(&definition.identity, Some(&definition.array_name))
    });
    let ranges = d.ranges.values().flat_map(|definition| {
        [
            NativeNameInput::TypeWithArray(&definition.identity, Some(&definition.array_name)),
            NativeNameInput::TypeWithArray(
                &definition.multirange.identity,
                Some(&definition.multirange.array_name),
            ),
        ]
    });
    let shells = d
        .shells
        .values()
        .map(|definition| NativeNameInput::Type(&definition.identity));
    uqa_sql::catalog::namespace_names::validate(
        tables
            .chain(views)
            .chain(foreign)
            .chain(sequences)
            .chain(indexes)
            .chain(composites)
            .chain(domains)
            .chain(enums)
            .chain(ranges)
            .chain(shells),
        memory,
        cancellation,
    )
}

#[cfg(test)]
mod tests;
