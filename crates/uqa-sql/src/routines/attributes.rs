//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The checks `PostgreSQL` makes of routine attributes, each at its own stage: `compute_common_attribute` for each clause in written order, the values `compute_function_attributes` and `AlterFunction` interpret, and the language, transforms, body and ROWS that `CreateFunction` examines.

use super::declaration::RoutineTypeCatalog;
use crate::ast::{
    ColumnType, CreateFunction, FunctionBody, RoutineAttributeClause, RoutineAttributeClauses,
    RoutineBodyError,
};
use crate::SQLError;

fn definition_error(message: impl Into<String>) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P13".into(),
        message: message.into(),
        detail: None,
        hint: None,
    }
}

fn routine_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: sqlstate.into(),
        message: message.into(),
        detail: None,
        hint: None,
    }
}

/// Reject the first clause, in written order, that a procedure cannot have or that repeats an earlier clause, as `compute_common_attribute` and `compute_function_attributes` do.
pub fn check_attribute_clauses(
    clauses: &RoutineAttributeClauses,
    is_procedure: bool,
) -> Result<(), SQLError> {
    let mut seen = Vec::<RoutineAttributeClause>::new();
    for &clause in &clauses.clauses {
        if is_procedure && clause.rejected_by_procedures() {
            return Err(definition_error(
                "invalid attribute in procedure definition",
            ));
        }
        if !clause.repeatable() && seen.contains(&clause) {
            return Err(routine_error("42601", "conflicting or redundant options"));
        }
        seen.push(clause);
    }
    Ok(())
}

/// `COST must be positive`, which `PostgreSQL` checks of the value it stores in a `float4`.
pub fn validate_cost(cost: Option<f32>) -> Result<(), SQLError> {
    if cost.is_some_and(|cost| cost <= 0.0) {
        return Err(routine_error("22023", "COST must be positive"));
    }
    Ok(())
}

/// `ROWS must be positive`, which `PostgreSQL` checks of the value it stores in a `float4`.
pub fn validate_rows(rows: Option<f32>) -> Result<(), SQLError> {
    if rows.is_some_and(|rows| rows <= 0.0) {
        return Err(routine_error("22023", "ROWS must be positive"));
    }
    Ok(())
}

/// `ROWS is not applicable when function does not return a set`.
pub fn validate_rows_applicability(rows: Option<f32>, returns_set: bool) -> Result<(), SQLError> {
    if rows.is_some() && !returns_set {
        return Err(routine_error(
            "22023",
            "ROWS is not applicable when function does not return a set",
        ));
    }
    Ok(())
}

/// The PARALLEL value `interpret_func_parallel` accepts.
pub fn validate_parallel(clauses: &RoutineAttributeClauses) -> Result<(), SQLError> {
    if clauses.invalid_parallel.is_some() {
        return Err(routine_error(
            "42601",
            "parameter \"parallel\" must be SAFE, RESTRICTED, or UNSAFE",
        ));
    }
    Ok(())
}

/// The language `CreateFunction` looks up: a routine without one reports `no language specified`, and only SQL and PL/pgSQL exist.
pub fn validate_routine_language(def: &CreateFunction) -> Result<(), SQLError> {
    if def.language.is_empty() {
        return Err(definition_error("no language specified"));
    }
    if !matches!(def.language.as_str(), "plpgsql" | "sql") {
        return Err(routine_error(
            "42704",
            format!("language \"{}\" does not exist", def.language),
        ));
    }
    Ok(())
}

/// The transforms of `TRANSFORM FOR TYPE`, which `CreateFunction` looks up for the routine's language after it checks LEAKPROOF: each type must exist, and the first reports that no transform exists for its element type, since no transform exists for any type.
pub fn validate_transforms(
    catalog: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    clauses: &RoutineAttributeClauses,
) -> Result<(), SQLError> {
    let Some(transform) = clauses.transform_types.first() else {
        return Ok(());
    };
    let ty = catalog
        .resolve_catalog_column_type(&transform.type_name)
        .ok_or_else(|| {
            routine_error(
                "42704",
                format!("type \"{}\" does not exist", transform.written),
            )
        })?;
    // `get_base_element_type`: an array, or a domain over one, names its element type.
    let element = match &ty {
        ColumnType::Array(element) => element.as_ref(),
        ColumnType::Domain { base, .. } => match base.as_ref() {
            ColumnType::Array(element) => element.as_ref(),
            _ => &ty,
        },
        _ => &ty,
    };
    Err(routine_error(
        "42704",
        format!(
            "transform for type {} language \"{}\" does not exist",
            catalog.format_type(element)?,
            def.language
        ),
    ))
}

/// The body `interpret_AS_clause` accepts: exactly one of an AS body and a SQL-standard body, a SQL-standard body only for SQL, and one AS item for the languages here.
pub fn validate_body_form(
    def: &CreateFunction,
    clauses: &RoutineAttributeClauses,
) -> Result<(), SQLError> {
    match clauses.body_error {
        Some(RoutineBodyError::Missing) => {
            return Err(definition_error("no function body specified"));
        }
        Some(RoutineBodyError::Duplicate) => {
            return Err(definition_error("duplicate function body specified"));
        }
        Some(RoutineBodyError::ExtraAsItems) | None => {}
    }
    if matches!(def.body, FunctionBody::Statements(_)) && def.language != "sql" {
        return Err(definition_error(
            "inline SQL function body only valid for language SQL",
        ));
    }
    if clauses.body_error == Some(RoutineBodyError::ExtraAsItems) {
        return Err(definition_error(format!(
            "only one AS item needed for language \"{}\"",
            def.language
        )));
    }
    Ok(())
}

/// A window function, which `PostgreSQL` creates where every other check passes and which this engine cannot run.
pub fn reject_window_function(
    def: &CreateFunction,
    clauses: &RoutineAttributeClauses,
) -> Result<(), SQLError> {
    if clauses.clauses.contains(&RoutineAttributeClause::Window) {
        return Err(SQLError::Unsupported(format!(
            "{}: WINDOW functions",
            if def.is_procedure {
                "CREATE PROCEDURE"
            } else {
                "CREATE FUNCTION"
            }
        )));
    }
    Ok(())
}
