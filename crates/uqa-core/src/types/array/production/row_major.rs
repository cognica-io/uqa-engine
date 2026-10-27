//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build a validated shape directly from admitted scalar producers, without an intermediate flat copy.

use super::super::{MAX_DIMENSIONS, MAX_ELEMENTS};
use super::{ArrayValue, Produced, ProductionControl, ProductionVec, Value, ValueRetentionError};

impl ArrayValue {
    /// Construct an ordinary SQL array from row-major scalar producers. Invalid dimensions, bounds or cardinality invoke no producer. A zero dimension gives the dimensionless empty array. Array-valued scalar leaves retain their own dimensions and bounds.
    pub fn from_row_major_with_control<E: From<ValueRetentionError>>(
        dimensions: &[usize],
        lower_bounds: &[i32],
        count: usize,
        control: &ProductionControl<'_>,
        mut element: impl FnMut(usize) -> Result<Produced<Value>, E>,
    ) -> Result<Option<Produced<Self>>, E> {
        control.check()?;
        if dimensions.len() != lower_bounds.len() || dimensions.len() > MAX_DIMENSIONS {
            return Ok(None);
        }
        let mut cardinality = usize::from(!dimensions.is_empty());
        for (length, lower) in dimensions.iter().zip(lower_bounds) {
            control.check()?;
            let Some(length) = i32::try_from(*length).ok() else {
                return Ok(None);
            };
            if lower.checked_add(length).is_none() {
                return Ok(None);
            }
            let Some(product) = cardinality.checked_mul(length as usize) else {
                return Ok(None);
            };
            cardinality = product;
        }
        if count != cardinality || count > MAX_ELEMENTS {
            return Ok(None);
        }
        if cardinality == 0 {
            return Self::try_new_with_control(ProductionVec::new(*control).finish()?, control)
                .map_err(Into::into);
        }
        let mut index = 0;
        let values = dimension(dimensions, &mut index, control, &mut element)?;
        let mut bounds = ProductionVec::new(*control);
        bounds.extend_from_slice(lower_bounds)?;
        Self::with_lower_bounds_with_control(values, bounds.finish()?, control).map_err(Into::into)
    }
}

fn dimension<E: From<ValueRetentionError>>(
    dimensions: &[usize],
    index: &mut usize,
    control: &ProductionControl<'_>,
    element: &mut impl FnMut(usize) -> Result<Produced<Value>, E>,
) -> Result<Produced<Vec<Value>>, E> {
    let mut output = ProductionVec::new(*control);
    output.reserve(dimensions[0])?;
    for _ in 0..dimensions[0] {
        control.check()?;
        let value = if dimensions.len() == 1 {
            let value = element(*index)?;
            *index += 1;
            value
        } else {
            let (values, memory) =
                dimension(&dimensions[1..], index, control, element)?.into_parts();
            control.finish(Value::List(values), memory)?
        };
        output.push_produced(value)?;
    }
    output.finish().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn scalar_array_leaves_preserve_bounds_traversal_copy_and_serialization() {
        let input = [
            Value::Array(
                ArrayValue::with_lower_bounds(vec![Value::Int(7), Value::Int(8)], vec![4]).unwrap(),
            ),
            Value::Array(ArrayValue::try_new(vec![]).unwrap()),
            Value::Null,
        ];
        let budget = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let array = ArrayValue::from_row_major_with_control(&[3], &[-2], 3, &control, |index| {
            control.copy_value(&input[index])
        })
        .unwrap()
        .unwrap();
        assert_eq!(array.dimensions(), [3]);
        let mut elements = array.elements_with_control(&control).unwrap();
        for expected in &input {
            assert_eq!(elements.next_element().unwrap(), Some(expected));
        }
        assert_eq!(elements.next_element().unwrap(), None);
        drop(elements);
        let restored: ArrayValue =
            serde_json::from_str(&serde_json::to_string(&*array).unwrap()).unwrap();
        assert_eq!(restored, *array);
        let nested =
            ArrayValue::try_new(vec![Value::Array(restored.clone()), Value::Array(restored)])
                .unwrap();
        assert_eq!(nested.dimensions(), [2, 3]);
        assert!(
            matches!(nested.element_at(&[2, 1]), Some(Value::Array(inner)) if inner.lower_bounds() == [4])
        );
        let (array, memory) = array.into_parts();
        let value = control.finish(Value::Array(array), memory).unwrap();
        let copy = control.copy_value(&value).unwrap();
        let restored: Value =
            serde_json::from_str(&serde_json::to_string(&*value).unwrap()).unwrap();
        assert_eq!(restored, *copy);
        assert_eq!(
            budget.used(),
            value.reserved_bytes() + copy.reserved_bytes()
        );
        drop((value, copy));
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn validates_before_input_and_releases_every_partial_producer() {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for (dimensions, bounds, count) in [
            (vec![2, 3], vec![1], 6),
            (vec![2], vec![1], 3),
            (vec![1; 7], vec![1; 7], 1),
            (vec![1], vec![i32::MAX], 1),
        ] {
            assert!(
                ArrayValue::from_row_major_with_control::<ValueRetentionError>(
                    &dimensions,
                    &bounds,
                    count,
                    &control,
                    |_| panic!("invalid shape invoked input")
                )
                .unwrap()
                .is_none()
            );
        }
        let held = control.copy_text("retained").unwrap();
        for fail_at in 0..6 {
            let error =
                ArrayValue::from_row_major_with_control(&[2, 3], &[-2, 7], 6, &control, |index| {
                    if index == fail_at {
                        invoking.cancel();
                    }
                    let (text, memory) = control.copy_text("value")?.into_parts();
                    control.finish(Value::Str(text), memory)
                })
                .unwrap_err();
            assert!(matches!(error, ValueRetentionError::Cancelled(_)));
            invoking.reset();
            assert_eq!(budget.used(), held.reserved_bytes());
        }
        let array =
            ArrayValue::from_row_major_with_control(&[2, 3], &[-2, 7], 6, &control, |index| {
                control.finish(Value::Int(index as i64), control.empty_reservation())
            })
            .unwrap()
            .unwrap();
        assert_eq!(array.dimensions(), [2, 3]);
        assert_eq!(array.lower_bounds(), [-2, 7]);
        assert_eq!(array.element_at(&[-1, 9]), Some(&Value::Int(5)));
        assert_eq!(
            budget.used(),
            held.reserved_bytes() + array.reserved_bytes()
        );
        drop(array);
        assert_eq!(budget.used(), held.reserved_bytes());
        let empty = ArrayValue::from_row_major_with_control::<ValueRetentionError>(
            &[2, 0, 3],
            &[1; 3],
            0,
            &control,
            |_| panic!("empty producer"),
        )
        .unwrap()
        .unwrap();
        assert!(empty.dimensions().is_empty());
    }
}
