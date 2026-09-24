//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose partial writes while every bound and RHS sees the unchanged input row.

use super::{MutationAssignmentContext, MutationAssignmentTarget, TypedAssignmentTarget};
use crate::{mutation::expressions::eval_mutation_expr, query::CteScope, OwnedPhysicalRow};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ArrayAssignmentError, ArrayValue, Value,
};
use uqa_sql::{
    assignment::{conversion, targets},
    ast::AssignmentStep,
    ColumnType, SQLError, SQLParam, ScalarExpr,
};

pub(super) fn assign_value<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    destination: MutationAssignmentTarget<'_>,
    value: impl FnOnce() -> Result<Value, SQLError>,
    source: Option<&ColumnType>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let MutationAssignmentTarget {
        table,
        target,
        current,
        final_column_write,
        ..
    } = destination;
    if target.is_whole_column() {
        return uqa_sql::assignment::columns::coerce_to_column_type_from(
            services.assignment,
            services.columns,
            table,
            &target.column,
            value()?,
            source,
        );
    }
    let columns = services
        .columns
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read assignment type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let declared = &columns
        .iter()
        .find(|column| column.name == target.column)
        .ok_or_else(|| SQLError::UnknownColumn(target.column.clone()))?
        .ty;
    assign_typed_value(
        services,
        ctes,
        TypedAssignmentTarget {
            target,
            ty: Some(declared),
            current,
            final_column_write,
        },
        value,
        source,
        row,
        params,
    )
}

/// View and table assignments share the same typed partial-write semantics.
pub(super) fn assign_typed_value<S: Clone + 'static>(
    services: MutationAssignmentContext<'_, S>,
    ctes: &CteScope<S>,
    destination: TypedAssignmentTarget<'_>,
    value: impl FnOnce() -> Result<Value, SQLError>,
    source: Option<&ColumnType>,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let TypedAssignmentTarget {
        target,
        ty,
        current,
        final_column_write,
    } = destination;
    let declared = ty.expect("typed assignment has its declared destination");
    if target.is_whole_column() {
        return conversion::coerce_assignment_value(
            services.assignment,
            value()?,
            declared,
            source,
        );
    }
    let required = targets::assignment_value_type(target, declared)?;
    targets::validate_assignment_source(target, &required, source)?;
    targets::validate_assignment_result(target, declared)?;
    let slice = target
        .indirection
        .iter()
        .any(|step| matches!(step, AssignmentStep::Slice { .. }));
    let bound = |expr: &ScalarExpr| -> Result<i32, SQLError> {
        let value = eval_mutation_expr(services.expressions, ctes, expr, row, params)?;
        if value == Value::Null {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: "array subscript in assignment must not be null".into(),
            });
        }
        let value = conversion::coerce_assignment_value(
            services.assignment,
            value,
            &ColumnType::Integer,
            None,
        )?;
        match value {
            Value::Int(value) => i32::try_from(value).map_err(|_| SQLError::Routine {
                sqlstate: "22003".into(),
                message: "integer out of range".into(),
            }),
            _ => Err(SQLError::Internal(
                "integer assignment conversion returned a noninteger".into(),
            )),
        }
    };
    let mut bounds = Vec::with_capacity(target.indirection.len());
    for step in &target.indirection {
        bounds.push(match step {
            AssignmentStep::Index(index) => (slice.then_some(1), Some(bound(index)?)),
            AssignmentStep::Slice { lower, upper } => (
                lower.as_deref().map(&bound).transpose()?,
                upper.as_deref().map(&bound).transpose()?,
            ),
            AssignmentStep::Field(_) => {
                unreachable!("typed array target contains no composite fields")
            }
        });
    }
    let value =
        conversion::coerce_assignment_value(services.assignment, value()?, &required, source)?;
    let result = assign_array_with_control(
        current.unwrap_or(&Value::Null),
        &value,
        &bounds,
        slice,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary array assignment result");
    if final_column_write {
        conversion::coerce_assignment_value(
            services.assignment,
            result,
            declared,
            Some(&targets::array_assignment_type(target, declared)?),
        )
    } else {
        Ok(result)
    }
}

/// SQL NULL containers become empty arrays before assignment; NULL slice sources preserve that container. Keep production admission through both paths.
fn assign_array_with_control(
    current: &Value,
    value: &Value,
    bounds: &[(Option<i32>, Option<i32>)],
    slice: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let empty = if *current == Value::Null {
        Some(
            ArrayValue::try_new_with_control(
                control.finish(Vec::new(), control.empty_reservation())?,
                control,
            )?
            .expect("empty array"),
        )
    } else {
        None
    };
    let array = match current {
        Value::Null => empty.as_deref().expect("NULL array replacement"),
        Value::Array(array) => array,
        _ => {
            return Err(SQLError::Internal(
                "declared array column has a nonarray carrier".into(),
            ))
        }
    };
    if slice && *value == Value::Null {
        return if let Some(empty) = empty {
            let (array, memory) = empty.into_parts();
            Ok(control.finish(Value::Array(array), memory)?)
        } else {
            Ok(control.copy_value(current)?)
        };
    }
    let result = if slice {
        let Value::Array(source) = value else {
            return Err(SQLError::Internal(
                "array slice conversion returned a nonarray".into(),
            ));
        };
        array.assign_slice_with_control(bounds, source, control)
    } else {
        let mut indexes = [0; 6];
        if bounds.len() > indexes.len() {
            return Err(array_error(ArrayAssignmentError::SubscriptCount));
        }
        for (index, (_, upper)) in indexes.iter_mut().zip(bounds) {
            *index =
                upper.ok_or_else(|| SQLError::Internal("element index lost its bound".into()))?;
        }
        array.assign_element_with_control(&indexes[..bounds.len()], value, control)
    }
    .map_err(array_error)?;
    let (array, memory) = result.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

fn array_error(error: ArrayAssignmentError) -> SQLError {
    let code = match &error {
        ArrayAssignmentError::Retention(_) => {
            let ArrayAssignmentError::Retention(error) = error else {
                unreachable!()
            };
            return error.into();
        }
        ArrayAssignmentError::SizeLimit | ArrayAssignmentError::LowerBound(_) => "54000",
        ArrayAssignmentError::ElementShape => "42804",
        _ => "2202E",
    };
    SQLError::Diagnostic {
        sqlstate: code.into(), message: error.to_string(), hint: None,
        detail: matches!(error, ArrayAssignmentError::MissingBounds).then(|| "When assigning to a slice of an empty array value, slice boundaries must be fully specified.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn null_slice_preserves_existing_values_and_materializes_a_null_container() {
        let budget = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let original = Value::Array(
            ArrayValue::with_lower_bounds(vec![Value::Str("retained".repeat(16))], vec![-3])
                .unwrap(),
        );
        for current in [&Value::Null, &original] {
            let output = assign_array_with_control(
                current,
                &Value::Null,
                &[(Some(1), Some(2))],
                true,
                &control,
            )
            .unwrap();
            assert_eq!(budget.used(), output.reserved_bytes());
            assert!(output.reserved_bytes() > 0);
            if *current == Value::Null {
                let Value::Array(array) = &*output else {
                    panic!("NULL slice must produce an array");
                };
                assert!(array.elements().is_empty());
                assert!(array.dimensions().is_empty());
            } else {
                assert_eq!(*output, original);
            }
            drop(output);
            assert_eq!(budget.used(), 0);
        }
        let refused = MemoryBudget::new(0);
        let control = ProductionControl::new(&refused, &token, &token);
        for current in [&Value::Null, &original] {
            assert_eq!(
                assign_array_with_control(
                    current,
                    &Value::Null,
                    &[(Some(1), Some(2))],
                    true,
                    &control
                )
                .unwrap_err()
                .sqlstate(),
                Some("53200")
            );
            assert_eq!(refused.used(), 0);
        }
    }

    #[test]
    fn cancellation_precedes_null_slice_container_copy_and_keeps_the_original_owner() {
        let budget = MemoryBudget::new(8192);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        invoking.cancel();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = assign_array_with_control(
            &Value::Null,
            &Value::Null,
            &[(Some(1), Some(2))],
            true,
            &control,
        )
        .unwrap_err();
        assert!(matches!(error, SQLError::Cancelled(_)));
        assert_eq!(budget.used(), 0);
    }
}
