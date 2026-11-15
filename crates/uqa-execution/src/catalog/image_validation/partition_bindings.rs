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
    semantics::partition::native::{self, NativePartitionContext, NativePartitionTable},
    SQLError,
};

mod values;

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let scalar = ScalarBindings::new(catalog, memory, cancellation)?;
    let scalar = NativeScalarContext {
        subject: "partition",
        routines,
        references: &scalar.references,
        types: &scalar.types,
        binding: &scalar.binding,
    };
    let values = values::PartitionValues::new(
        catalog,
        routines.enum_labels(),
        ProductionControl::new(memory, cancellation, cancellation),
    );
    let context = NativePartitionContext {
        scalar,
        values: Some(&values),
    };
    native::validate(
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
        memory,
        cancellation,
    )
}
