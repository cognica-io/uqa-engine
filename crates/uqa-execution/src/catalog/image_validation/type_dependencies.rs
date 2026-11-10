//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native catalog type closure joins original declarations and physical row descriptors.

use super::super::{foreign::StoredForeignTable, CatalogReadSnapshot};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{
    ast::{FunctionReturns, RoutineDefaultType},
    catalog::type_references::{TypeReferenceInput, TypeReferences},
    SQLError,
};

pub fn validate(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let d = &snapshot.definitions;
    let rows = snapshot
        .tables
        .iter()
        .map(|(_, table)| table.catalog_oids)
        .chain(d.views.values().map(|view| view.relation_oids()))
        .chain(
            d.foreign_tables
                .values()
                .map(StoredForeignTable::relation_oids),
        );
    let types = TypeReferences::new(
        TypeReferenceInput {
            domains: &d.domains,
            enums: &d.enums,
            composites: &d.composites,
            ranges: &d.ranges,
            shells: &d.shells,
        },
        rows,
        memory,
        cancellation,
    )?;
    for function in d.sql_user_functions.values().flatten() {
        for parameter in &function.def.params {
            types.stored_name(&parameter.type_name)?;
            if let Some(RoutineDefaultType::Concrete(ty)) = &parameter.default_type {
                types.column_type(ty)?;
            }
        }
        match &function.def.returns {
            FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name } => {
                types.stored_name(type_name)?;
            }
            FunctionReturns::None | FunctionReturns::Table => {}
        }
    }
    let members = snapshot
        .tables
        .iter()
        .flat_map(|(_, table)| {
            table
                .columns
                .iter()
                .map(move |column| (table.catalog_oids.reltype(), &column.ty))
        })
        .chain(d.foreign_tables.values().flat_map(|table| {
            table
                .columns
                .iter()
                .map(move |column| (table.relation_oids().reltype(), &column.ty))
        }))
        .chain(
            d.domains
                .values()
                .map(|domain| (domain.oid, &domain.definition.base)),
        )
        .chain(d.composites.values().flat_map(|definition| {
            definition
                .live_attributes()
                .map(move |attribute| (definition.oid, &attribute.ty))
        }))
        .chain(d.ranges.values().flat_map(|definition| {
            [
                (definition.oid, &definition.subtype),
                (definition.multirange.oid, &definition.subtype),
            ]
        }))
        .chain(d.views.values().flat_map(|view| {
            view.materialized_column_types
                .iter()
                .flatten()
                .map(move |ty| (view.relation_oids().reltype(), ty))
        }));
    types.validate_membership(members)?;
    cancellation.check()?;
    Ok(())
}
