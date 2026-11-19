//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve stored view queries and defaults against the private catalog and installed function declarations.

use super::scalar_bindings::ScalarBindings;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::scalar_bindings::{expression_type_with_work, NativeScalarContext},
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
    let snapshot = catalog.snapshot();
    for view in snapshot.definitions.views.values() {
        work.checkpoint().await?;
        control.check()?;
        view.rewrite_definition_ref()
            .validate_function_calls_with_work(
                routines,
                scalar.binding.copy_with_control(&control)?,
                &control,
                work,
            )
            .await?;
    }

    let context = NativeScalarContext {
        subject: "default expression",
        routines,
        references: &scalar.references,
        types: &scalar.types,
        binding: &scalar.binding,
    };
    let empty_row = RowSchema::default();
    let tables = snapshot
        .tables
        .values()
        .map(|table| table.columns.as_slice())
        .chain(
            snapshot
                .definitions
                .foreign_tables
                .values()
                .map(|table| table.columns.as_slice()),
        );
    for columns in tables {
        work.checkpoint().await?;
        control.check()?;
        for column in columns {
            work.checkpoint().await?;
            control.check()?;
            if let Some(default) = &column.default {
                expression_type_with_work(default, &empty_row, &context, &control, work).await?;
            }
        }
    }
    for domain in snapshot.definitions.domains.values() {
        work.checkpoint().await?;
        control.check()?;
        if let Some(default) = &domain.definition.default {
            expression_type_with_work(default, &empty_row, &context, &control, work).await?;
        }
    }
    scalar.retire(work).await
}
