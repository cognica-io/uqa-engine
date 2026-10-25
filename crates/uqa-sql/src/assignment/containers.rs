//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rebuild typed assignment containers without evaluating SQL operands.

use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ArrayAssignmentError, ArrayValue, Value,
};

pub type SubscriptBound = (Option<i32>, Option<i32>);

/// Shared bound scheduling for native expressions and mutation assignment evaluation.
pub fn evaluate_subscript_bounds(
    dimensions: usize,
    slice: bool,
    control: &ProductionControl<'_>,
    mut bound: impl FnMut(usize, bool) -> Result<Option<i32>, SQLError>,
) -> Result<Produced<Vec<SubscriptBound>>, SQLError> {
    if dimensions > crate::ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS {
        return Err(array_error(ArrayAssignmentError::SubscriptCount));
    }
    let mut bounds = uqa_core::memory::ProductionVec::new(*control);
    for _ in 0..dimensions {
        bounds.push_copy((None, None))?;
    }
    for upper in [true, false].into_iter().take(if slice { 2 } else { 1 }) {
        for (dimension, pair) in bounds.as_mut_slice().iter_mut().enumerate() {
            control.check()?;
            let value = bound(dimension, upper)?;
            if upper {
                pair.1 = value;
            } else {
                pair.0 = value;
            }
        }
    }
    Ok(bounds.finish()?)
}

pub(super) fn field_store(
    type_oid: u32,
    number: i16,
    current: &Value,
    rhs: &Value,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let types = crate::expr::ValueCompositeCatalog(catalog);
    let descriptor = crate::expr::composites::descriptor(Some(&types), type_oid)?;
    if !descriptor
        .attributes
        .iter()
        .any(|attribute| attribute.number == number)
    {
        return Err(SQLError::Internal(
            "field assignment attribute disappeared".into(),
        ));
    }
    let decoded = match current {
        Value::Datum(datum) => Some(crate::expr::datums::read_with_value_catalog_and_control(
            datum, catalog, control,
        )?),
        _ => None,
    };
    let current = decoded.as_deref().unwrap_or(current);
    crate::expr::composites::constructor::construct_current_with_control(
        &descriptor,
        Some(&types),
        control,
        |position| {
            let attribute = &descriptor.attributes[position];
            let value = if attribute.number == number {
                rhs
            } else {
                match current {
                    Value::Null => &Value::Null,
                    Value::Record(record) => record
                        .iter()
                        .find(|(name, _)| *name == &attribute.name)
                        .map_or(&Value::Null, |(_, value)| value),
                    _ => {
                        return Err(SQLError::Internal(
                            "field assignment has a non-composite container".into(),
                        ))
                    }
                }
            };
            Ok(control.copy_value(value)?)
        },
    )
}

/// SQL NULL containers become empty arrays before assignment; NULL slice sources preserve that container. Keep production admission through both paths.
pub fn assign_array_with_control(
    current: &Value,
    value: &Value,
    bounds: &[(Option<i32>, Option<i32>)],
    slice: bool,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let decoded = match current {
        Value::Datum(datum) => Some(crate::expr::datums::read_with_value_catalog_and_control(
            datum, catalog, control,
        )?),
        _ => None,
    };
    let current = decoded.as_deref().unwrap_or(current);
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
    let source = match value {
        Value::Datum(datum) if slice => Some(
            crate::expr::datums::read_with_value_catalog_and_control(datum, catalog, control)?,
        ),
        _ => None,
    };
    let value = source.as_deref().unwrap_or(value);
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

pub fn array_error(error: ArrayAssignmentError) -> SQLError {
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
    fn assignment_upper_bounds_precede_lower_bounds_and_null_stops_before_rhs() {
        let token = CancellationToken::new();
        let budget = MemoryBudget::new(4096);
        let control = ProductionControl::new(&budget, &token, &token);
        let mut reached = Vec::new();
        let values = [
            Value::Null,
            Value::Null,
            Value::Int(1),
            Value::Array(ArrayValue::try_new(vec![Value::Int(7)]).unwrap()),
        ];
        let binding = crate::ast::AssignmentBinding {
            path: vec![crate::ast::AssignmentPathStep::Subscripts {
                first_argument: 1,
                dimensions: 1,
                slice: true,
                bounds: crate::ast::ArraySliceBounds::default(),
            }],
            value_argument: 3,
        };
        let error = super::super::path::evaluate(&binding, values.len(), None, &control, |index| {
            reached.push(index);
            Ok(&values[index])
        })
        .unwrap_err();
        // PostgreSQL 18 evaluates the upper nextval first, reports the NULL lower bound, and never reaches the RHS.
        assert_eq!(error.sqlstate(), Some("22004"));
        assert_eq!(reached, [0, 2, 1]);
        assert_eq!(budget.used(), 0);
        token.cancel();
        reached.clear();
        let error = super::super::path::evaluate(&binding, values.len(), None, &control, |index| {
            reached.push(index);
            Ok(&values[index])
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert!(reached.is_empty());
        assert_eq!(budget.used(), 0);
    }

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
                None,
                &control,
            )
            .unwrap();
            assert_eq!(budget.used(), output.reserved_bytes());
            assert!(output.reserved_bytes() > 0);
            if *current == Value::Null {
                let Value::Array(array) = &*output else {
                    panic!("NULL slice must produce an array");
                };
                assert_eq!(array.elements().len(), 0);
                assert_eq!(array.dimensions().len(), 0);
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
                    None,
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
            None,
            &control,
        )
        .unwrap_err();
        assert!(matches!(error, SQLError::Cancelled(_)));
        assert_eq!(budget.used(), 0);
    }
}
