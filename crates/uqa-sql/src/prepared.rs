//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared definitions, result descriptors, and SQL EXECUTE argument contracts.

use crate::{ColumnType, RowSchema, SQLError, SQLParam};
mod production;
pub use production::{
    analysis_parameters_with_control, declared_parameter_types_for_plan_with_control,
    declared_parameter_types_with_control,
};
pub mod arguments;
pub mod composites;
pub mod definition;
pub mod dependencies;
pub mod entry;
pub mod ownership;
pub mod planning;
pub mod utility;

pub fn declared_parameter_types(
    resolver: &dyn crate::FunctionTypeResolver,
    logical_plan: &mut crate::plan::UnifiedPlan,
    declared: &[ColumnType],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    declared_parameter_types_with_control(
        resolver,
        logical_plan,
        declared,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary prepared parameter owner".into()))
}

pub fn analyze_prepared_plan(
    routines: &dyn crate::routines::RoutineResolution,
    plan: &crate::plan::UnifiedPlan,
    parameter_types: &[Option<ColumnType>],
    scope: &crate::binding::context::BindingContext<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    let params = analysis_parameters(parameter_types);
    match plan {
        crate::plan::UnifiedPlan::Query(query) => {
            crate::binding::analyze_query_plan_schema(routines, query, &params, scope, None)
                .map(Some)
        }
        crate::plan::UnifiedPlan::Command(command) => {
            crate::binding::analyze_prepared_command_schema(routines, command, &params, scope)
        }
    }
}

pub(crate) fn analysis_parameters(parameter_types: &[Option<ColumnType>]) -> Vec<SQLParam> {
    analysis_parameters_with_control(
        parameter_types,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary analysis parameters")
    .into_uncontrolled()
    .expect("ordinary parameter owner")
}

pub fn prepared_result_schema_matches(left: Option<&RowSchema>, right: Option<&RowSchema>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.columns() == right.columns()
                && left.column_types().len() == right.column_types().len()
                && left
                    .column_types()
                    .iter()
                    .zip(right.column_types())
                    .all(|(left, right)| {
                        prepared_type_identity(left.as_ref())
                            == prepared_type_identity(right.as_ref())
                    })
        }
        _ => false,
    }
}

fn prepared_type_identity(ty: Option<&ColumnType>) -> Option<(u32, i32)> {
    ty.map(|ty| {
        if let ColumnType::Domain { oid, .. } = ty {
            (*oid, -1)
        } else {
            let metadata = crate::catalog::result_type::postgres_result_type(ty);
            (metadata.type_oid, metadata.type_modifier)
        }
    })
}

pub fn statement_error(sqlstate: &str, name: &str, reason: &str) -> SQLError {
    error(sqlstate, format!("prepared statement \"{name}\" {reason}"))
}

pub fn execute_parameter_types(
    name: &str,
    types: Option<Vec<Option<ColumnType>>>,
    argument_count: usize,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let types = types.ok_or_else(|| statement_error("26000", name, "does not exist"))?;
    // SQL EXECUTE ignores argument lists for parameterless definitions; protocol Bind checks its own arity.
    if types.is_empty() {
        return Ok(Vec::new());
    }
    if argument_count != types.len() {
        return Err(error(
            "42601",
            format!("wrong number of parameters for prepared statement \"{name}\""),
        ));
    }
    Ok(types)
}

/// Host binding checks every supplied parameter, including unused utility declarations.
pub fn validate_host_parameter_count(
    name: &str,
    supplied: usize,
    expected: usize,
) -> Result<(), SQLError> {
    if supplied == expected {
        return Ok(());
    }
    Err(error(
        "08P01",
        format!("bind message supplies {supplied} parameters, but prepared statement \"{name}\" requires {expected}"),
    ))
}

pub(super) fn error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
