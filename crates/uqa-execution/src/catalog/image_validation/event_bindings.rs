//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join original event rows and immutable catalog metadata for SQL's rule condition analysis.

use super::scalar_bindings::ScalarBindings;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::{
        events::native::bindings::validate_condition_with_work,
        scalar_bindings::NativeScalarContext,
    },
    routines::RoutineResolution,
    RowSchema, SQLError,
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
        subject: "rule condition",
        routines,
        references: &scalar.references,
        types: &scalar.types,
        binding: &scalar.binding,
    };
    let snapshot = catalog.snapshot();
    for (owner, rules) in snapshot.definitions.rules.iter() {
        work.checkpoint().await?;
        control.check()?;
        let row = if let Some(table) = snapshot.tables.get(owner) {
            RowSchema::with_column_metadata_with_control(
                None,
                table
                    .columns
                    .iter()
                    .map(|column| (column.name.as_str(), Some(&column.ty), column.collation_oid)),
                &control,
            )?
        } else if let Some(view) = snapshot.definitions.views.get(owner) {
            view.rewrite_definition_ref().row_schema_with_control(
                routines,
                scalar.binding.copy_with_control(&control)?,
                &control,
            )?
        } else {
            return Err(SQLError::Internal(
                "native rule owner has no row descriptor".into(),
            ));
        };
        for rule in rules.values() {
            work.checkpoint().await?;
            validate_condition_with_work(rule, &row, &context, &control, work).await?;
            uqa_sql::catalog::events::native::dependencies::validate(
                rule,
                catalog,
                &scalar.binding.resolution,
                &control,
            )?;
        }
    }
    control.check()?;
    scalar.retire(work).await
}

#[cfg(test)]
mod tests;
