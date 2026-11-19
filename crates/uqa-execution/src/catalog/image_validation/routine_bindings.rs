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
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let scalar = ScalarBindings::new(catalog, memory, cancellation)?;
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
        uqa_sql::routines::native::validate_captured_bindings(routine, types, &context, &control)?;
    }
    Ok(control.check()?)
}
