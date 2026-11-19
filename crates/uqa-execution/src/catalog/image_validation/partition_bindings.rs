//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join immutable table hierarchies and selected scalar identities for SQL's partition predicate.

use super::scalar_bindings::ScalarBindings;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::scalar_bindings::NativeScalarContext,
    routines::RoutineResolution,
    semantics::partition::native::{self, NativePartitionTable},
    SQLError,
};

mod values;

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
    let scalar = ScalarBindings::with_work(catalog, memory, cancellation, work).await?;
    let context = NativeScalarContext {
        subject: "partition",
        routines,
        references: &scalar.references,
        types: &scalar.types,
        binding: &scalar.binding,
    };
    let values = values::PartitionValues::new(
        catalog,
        Some(routines),
        ProductionControl::new(memory, cancellation, cancellation),
    );
    native::validate_with_work(
        catalog
            .snapshot()
            .tables
            .iter()
            .map(|(name, table)| NativePartitionTable {
                name,
                columns: &table.columns,
                hierarchy: &table.hierarchy,
                checks: &table.checks,
            }),
        &context,
        || Some(&values),
        memory,
        cancellation,
        work,
    )
    .await?;
    scalar.retire(work).await
}
