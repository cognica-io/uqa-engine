//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type selection and duplicate-column rules for whole and subscripted assignments.

use crate::{
    ast::{AssignmentStep, AssignmentTarget},
    ColumnType, SQLError,
};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

fn error(code: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: code.into(),
        message,
    }
}

/// Whole-column writes may target an open schema. Partial writes require a declared container type before evaluating bounds or values.
pub fn validate_assignment_type<E>(
    target: &AssignmentTarget<E>,
    declared: Option<&ColumnType>,
) -> Result<(), SQLError> {
    if declared.is_none() && !target.is_whole_column() {
        array_assignment_type(target, &ColumnType::Named("unknown".into()))?;
    }
    Ok(())
}

/// Repeated partial targets compose, but a whole-column write cannot share its destination.
pub fn validate_repeated_targets<'a, E: 'a>(
    targets: impl IntoIterator<Item = &'a AssignmentTarget<E>>,
    insert: bool,
) -> Result<(), SQLError> {
    let mut seen = BTreeMap::new();
    for target in targets {
        let whole = target.is_whole_column();
        if let Some(previous) = seen.insert(&target.column, whole) {
            if whole || previous {
                return Err(if insert {
                    error(
                        "42701",
                        format!("column \"{}\" specified more than once", target.column),
                    )
                } else {
                    error(
                        "42601",
                        format!("multiple assignments to same column \"{}\"", target.column),
                    )
                });
            }
        }
    }
    Ok(())
}

/// The array container produced by subscripting, before restoring a domain or legacy vector type.
pub fn array_assignment_type<E>(
    target: &AssignmentTarget<E>,
    declared: &ColumnType,
) -> Result<ColumnType, SQLError> {
    let mut base = declared;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if let Some(AssignmentStep::Field(field)) = target.indirection.first() {
        return Err(error("42804", format!(
            "cannot assign to field \"{field}\" of column \"{}\" because its type {} is not a composite type",
            target.column, declared.sql_name()
        )));
    }
    let array = match base {
        ColumnType::Array(_) => base.clone(),
        ColumnType::Int2Vector => ColumnType::Array(Box::new(ColumnType::SmallInteger)),
        ColumnType::OidVector => ColumnType::Array(Box::new(ColumnType::Oid)),
        _ => {
            return Err(error(
                "42804",
                format!(
                    "cannot subscript type {} because it does not support subscripting",
                    declared.sql_name()
                ),
            ))
        }
    };
    if let Some(AssignmentStep::Field(field)) = target
        .indirection
        .iter()
        .find(|step| matches!(step, AssignmentStep::Field(_)))
    {
        let mut element = &array;
        while let ColumnType::Array(inner) = element {
            element = inner;
        }
        return Err(error("42804", format!(
            "cannot assign to field \"{field}\" of column \"{}\" because its type {} is not a composite type",
            target.column, element.sql_name()
        )));
    }
    if target.indirection.len() > 6 {
        return Err(error(
            "54000",
            format!(
                "number of array dimensions ({}) exceeds the maximum allowed (6)",
                target.indirection.len()
            ),
        ));
    }
    Ok(array)
}

/// Slices consume an array; any number of element subscripts consumes one scalar element.
pub fn assignment_value_type<E>(
    target: &AssignmentTarget<E>,
    declared: &ColumnType,
) -> Result<ColumnType, SQLError> {
    if target.is_whole_column() {
        return Ok(declared.clone());
    }
    let array = array_assignment_type(target, declared)?;
    if target
        .indirection
        .iter()
        .any(|step| matches!(step, AssignmentStep::Slice { .. }))
    {
        Ok(array)
    } else {
        let mut element = &array;
        while let ColumnType::Array(inner) = element {
            element = inner;
        }
        Ok(element.clone())
    }
}

/// Type errors retain `PostgreSQL`'s separate primary message and rewrite hint.
pub fn validate_assignment_source<E>(
    target: &AssignmentTarget<E>,
    required: &ColumnType,
    source: Option<&ColumnType>,
) -> Result<(), SQLError> {
    if let Some(source) = source {
        if !crate::assignment_type_compatible(source, required) {
            let message = if target.is_whole_column() {
                format!(
                    "column \"{}\" is of type {} but expression is of type {}",
                    target.column,
                    required.sql_name(),
                    source.sql_name()
                )
            } else {
                format!("subscripted assignment to \"{}\" requires type {} but expression is of type {}", target.column, required.sql_name(), source.sql_name())
            };
            return Err(SQLError::Diagnostic {
                sqlstate: "42804".into(),
                message,
                detail: None,
                hint: Some("You will need to rewrite or cast the expression.".into()),
            });
        }
    }
    Ok(())
}

/// Subscripting yields an ordinary array; the declared destination must accept that container.
pub fn validate_assignment_result<E>(
    target: &AssignmentTarget<E>,
    declared: &ColumnType,
) -> Result<(), SQLError> {
    if target.is_whole_column() {
        return Ok(());
    }
    let container = array_assignment_type(target, declared)?;
    if !crate::type_resolution::explicit_type_compatible(&container, declared) {
        return Err(error(
            "42846",
            format!(
                "cannot cast type {} to {}",
                container.sql_name(),
                declared.sql_name()
            ),
        ));
    }
    Ok(())
}

pub fn validate_assignment_default<E>(target: &AssignmentTarget<E>) -> Result<(), SQLError> {
    match target.indirection.first() {
        None => Ok(()),
        Some(AssignmentStep::Field(_)) => {
            Err(error("0A000", "cannot set a subfield to DEFAULT".into()))
        }
        Some(_) => Err(error(
            "0A000",
            "cannot set an array element to DEFAULT".into(),
        )),
    }
}
