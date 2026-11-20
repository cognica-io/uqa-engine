//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join captured routine expressions with one immutable catalog for SQL-owned binding validation.

use super::scalar_bindings::ScalarBindings;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::scalar_bindings::NativeScalarContext,
    routines::{declaration::RoutineTypeCatalog, RoutineResolution},
    SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    types: &dyn RoutineTypeCatalog,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate_with_work(
        catalog,
        routines,
        || types,
        memory,
        cancellation,
        &mut (),
    ))
}

pub(super) async fn validate_with_work<'a>(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    types: impl Fn() -> &'a dyn RoutineTypeCatalog,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl uqa_sql::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let scalar = ScalarBindings::with_work(catalog, memory, cancellation, work).await?;
    let context = NativeScalarContext {
        subject: "routine default",
        routines,
        references: &scalar.references,
        types: &scalar.types,
        binding: &scalar.binding,
    };
    for routine in catalog
        .snapshot()
        .definitions
        .sql_user_functions
        .values()
        .flatten()
    {
        work.checkpoint().await?;
        uqa_sql::routines::native::validate_captured_bindings_with_work(
            routine, &types, &context, &control, work,
        )
        .await?;
    }
    control.check()?;
    scalar.retire(work).await
}
