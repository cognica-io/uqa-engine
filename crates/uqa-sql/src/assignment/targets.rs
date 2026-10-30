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

mod types;
pub use types::{assignment_input_type_with_control, field_assignment_types_with_control};

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

/// One level of an assignment target: a composite field, or the consecutive subscripts that `transformAssignmentSubscripts` applies together.
pub enum AssignmentLevel<'a, E> {
    Field(&'a str),
    Subscripts(&'a [AssignmentStep<E>]),
}

impl<E> AssignmentLevel<'_, E> {
    /// Whether a subscript group writes a slice.
    pub fn is_slice(&self) -> bool {
        match self {
            Self::Field(_) => false,
            Self::Subscripts(steps) => steps
                .iter()
                .any(|step| matches!(step, AssignmentStep::Slice { .. })),
        }
    }
}

/// Group an assignment's indirection into fields and subscript groups, in order.
pub fn assignment_levels<E>(steps: &[AssignmentStep<E>]) -> Vec<AssignmentLevel<'_, E>> {
    level_iter(steps).collect()
}

fn level_iter<E>(steps: &[AssignmentStep<E>]) -> impl Iterator<Item = AssignmentLevel<'_, E>> {
    let mut remaining = steps;
    std::iter::from_fn(move || {
        let first = remaining.first()?;
        if let AssignmentStep::Field(field) = first {
            remaining = &remaining[1..];
            Some(AssignmentLevel::Field(field))
        } else {
            let count = remaining
                .iter()
                .take_while(|step| !matches!(step, AssignmentStep::Field(_)))
                .count();
            let (group, tail) = remaining.split_at(count);
            remaining = tail;
            Some(AssignmentLevel::Subscripts(group))
        }
    })
}

/// Whether an assignment writes a composite field somewhere along its indirection.
pub fn has_field_step<E>(target: &AssignmentTarget<E>) -> bool {
    target
        .indirection
        .iter()
        .any(|step| matches!(step, AssignmentStep::Field(_)))
}

/// The type each level of a field assignment applies to, followed by the type the assigned value must have, as `transformAssignmentIndirection` resolves them: a field names an attribute of a composite type or of a domain over one, and a subscript group selects an array element, or keeps the array for a slice.
pub fn field_assignment_types<E>(
    target: &AssignmentTarget<E>,
    declared: &ColumnType,
    composites: Option<&dyn crate::expr::composites::CompositeTypeCatalog>,
) -> Result<Vec<ColumnType>, SQLError> {
    Ok(field_assignment_types_with_control(
        target,
        declared,
        composites,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary assignment types"))
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
            // `transformAssignmentIndirection` names the innermost field it assigns through.
            let levels = assignment_levels(&target.indirection);
            let name = levels
                .iter()
                .rev()
                .find_map(|level| match level {
                    AssignmentLevel::Field(field) => Some(*field),
                    AssignmentLevel::Subscripts(_) => None,
                })
                .unwrap_or(&target.column);
            let message = match levels.last() {
                None => format!(
                    "column \"{}\" is of type {} but expression is of type {}",
                    target.column,
                    required.display_name(),
                    source.display_name()
                ),
                Some(AssignmentLevel::Field(field)) => format!(
                    "subfield \"{field}\" is of type {} but expression is of type {}",
                    required.display_name(),
                    source.display_name()
                ),
                Some(AssignmentLevel::Subscripts(_)) => format!(
                    "subscripted assignment to \"{name}\" requires type {} but expression is of type {}",
                    required.display_name(),
                    source.display_name()
                ),
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
    // A field write rebuilds the column's own composite value.
    if target.is_whole_column() || has_field_step(target) {
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
