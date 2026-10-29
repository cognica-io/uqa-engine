//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the catalog adapter and capture immutable lexical inputs before SQL preparation.

use crate::query::CteScope;
use uqa_core::memory::ProductionControl;
use uqa_sql::{binding::production::ControlledBindingSnapshot, SQLError};

pub fn binding_snapshot_with_control<S: Clone>(
    scope: &CteScope<S>,
    control: &ProductionControl<'_>,
) -> Result<ControlledBindingSnapshot, SQLError> {
    control.check()?;
    let catalog = control
        .finish(scope.catalog_read_view()?, control.empty_reservation())?
        .into_shared(control)?;
    let mut schemas = uqa_core::memory::ProductionVec::new(*control);
    for (name, rows) in &scope.rows {
        control.check()?;
        let schema = scope.recursive_control_width(name).map_or_else(
            || Ok(rows.row_schema().clone()),
            |width| {
                uqa_sql::RowSchema::with_recursive_input_with_control(
                    rows.row_schema(),
                    width,
                    control,
                )
            },
        )?;
        schemas
            .push_produced(control.finish((name.as_str(), schema), control.empty_reservation())?)?;
    }
    ControlledBindingSnapshot::from_inputs_with_control(
        catalog,
        scope.relation_name_resolution_with_control(control)?,
        schemas.iter().map(|(name, schema)| (*name, schema.clone())),
        scope
            .deferred_ctes()
            .iter()
            .map(|(name, plan)| (name.as_str(), plan)),
        scope.non_returning_ctes.iter().map(String::as_str),
        scope.scalar_subqueries.clone(),
        control,
    )
}

pub fn binding_context_with_control<'a, S: Clone>(
    scope: &'a CteScope<S>,
    control: &ProductionControl<'_>,
) -> Result<uqa_sql::binding::production::ControlledBindingContext<'a>, SQLError> {
    let snapshot = binding_snapshot_with_control(scope, control)?;
    Ok(uqa_sql::binding::production::ControlledBindingContext {
        catalog: snapshot.catalog,
        resolution: snapshot.resolution,
        ctes: snapshot.ctes,
        deferred_ctes: snapshot.deferred_ctes,
        non_returning_ctes: snapshot.non_returning_ctes,
        scalar_subqueries: &scope.scalar_subqueries,
    })
}
