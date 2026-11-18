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
    catalog::scalar_bindings::{expression_type, NativeScalarContext},
    routines::RoutineResolution,
    RowSchema, SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let scalar = ScalarBindings::new(catalog, memory, cancellation)?;
    let snapshot = catalog.snapshot();
    for view in snapshot.definitions.views.values() {
        control.check()?;
        view.rewrite_definition_ref()
            .validate_function_names_with_control(
                routines,
                scalar.binding.copy_with_control(&control)?,
                &control,
            )?;
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
        control.check()?;
        for column in columns {
            control.check()?;
            if let Some(default) = &column.default {
                expression_type(default, &empty_row, &context, &control)?;
            }
        }
    }
    for domain in snapshot.definitions.domains.values() {
        control.check()?;
        if let Some(default) = &domain.definition.default {
            expression_type(default, &empty_row, &context, &control)?;
        }
    }
    Ok(())
}
