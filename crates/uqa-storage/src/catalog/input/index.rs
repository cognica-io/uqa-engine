//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adopt original provider fields without copying their admitted buffers.

use crate::{CatalogIndexRow, RelationIdentity};
use uqa_core::memory::Budgeted;

/// Assemble one complete index row. Every field retains its original allocation and reservation; encoded definitions are neither parsed nor normalized here.
pub fn row(
    relation: Budgeted<RelationIdentity>,
    fields: [Budgeted<String>; 4],
    definition: Option<Budgeted<String>>,
) -> Budgeted<CatalogIndexRow> {
    let (relation, mut memory) = relation.into_parts();
    let [index_type, table_name, columns_json, parameters_json] = fields.map(|field| {
        let (field, reservation) = field.into_parts();
        memory.absorb(reservation);
        field
    });
    let definition_json = definition.map(|field| {
        let (field, reservation) = field.into_parts();
        memory.absorb(reservation);
        field
    });
    Budgeted::new(
        CatalogIndexRow {
            relation,
            index_type,
            table_name,
            columns_json,
            parameters_json,
            definition_json,
        },
        memory,
    )
}
