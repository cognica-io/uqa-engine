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
    schema::constraints::native::bindings::{validate_domain_with_work, validate_table_with_work},
    ColumnType, SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate_with_work(
        catalog,
        routines,
        memory,
        cancellation,
        &mut (),
    ))
}

pub(super) async fn validate_with_work(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl uqa_sql::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let scalar = ScalarBindings::with_work(catalog, memory, cancellation, work).await?;
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
        work.checkpoint().await?;
        control.check()?;
        let row_type = row_type(name, oids, &control)?;
        validate_table_with_work(
            name,
            columns,
            checks,
            row_type.as_deref(),
            &context,
            &control,
            work,
        )
        .await?;
    }
    for domain in snapshot.definitions.domains.values() {
        work.checkpoint().await?;
        validate_domain_with_work(&domain.definition, &context, &control, work).await?;
    }
    control.check()?;
    scalar.retire(work).await
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
