//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join stored CHECK definitions with original ordinary/foreign rows and domain bases.

use super::scalar_bindings::ScalarBindings;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::{relation_oids::RelationCatalogOids, scalar_bindings::NativeScalarContext},
    routines::RoutineResolution,
    schema::constraints::native::bindings::{validate_domain, validate_table},
    ColumnType, SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let scalar = ScalarBindings::new(catalog, memory, cancellation)?;
    let context = NativeScalarContext {
        subject: "constraint",
        routines,
        references: &scalar.references,
        types: &scalar.types,
        binding: &scalar.binding,
    };
    let snapshot = catalog.snapshot();
    let ordinary = snapshot.tables.iter().map(|(name, table)| {
        (
            name,
            table.columns.as_slice(),
            table.checks.as_slice(),
            Some(table.catalog_oids),
        )
    });
    let foreign = snapshot
        .definitions
        .foreign_tables
        .iter()
        .map(|(name, table)| {
            (
                name,
                table.columns.as_slice(),
                table.checks.as_slice(),
                table.catalog_oids,
            )
        });
    for (name, columns, checks, oids) in ordinary.chain(foreign) {
        control.check()?;
        let row_type = row_type(name, oids, &control)?;
        validate_table(
            name,
            columns,
            checks,
            row_type.as_deref(),
            &context,
            &control,
        )?;
    }
    for domain in snapshot.definitions.domains.values() {
        validate_domain(&domain.definition, &context, &control)?;
    }
    control.check()?;
    Ok(())
}

fn row_type(
    name: &uqa_core::RelationIdentity,
    oids: Option<RelationCatalogOids>,
    control: &ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    let Some(oids) = oids else { return Ok(None) };
    oids.row_type_reference_with_control(name, control)?
        .map(|reference| {
            let (reference, memory) = reference.into_parts();
            control
                .finish(ColumnType::Composite(reference), memory)
                .map_err(Into::into)
        })
        .transpose()
}
