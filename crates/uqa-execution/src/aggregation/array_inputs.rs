//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `array_agg(anyarray)` transitions, as `PostgreSQL`'s `accumArrayResultArr`: every input is a non-NULL, non-empty array whose dimensions and lower bounds match the first input, and the result adds one leading dimension with lower bound 1.

use super::{SQLError, Value};
use uqa_core::ArrayValue;

/// `PostgreSQL`'s `MAXDIM`.
const MAX_ARRAY_DIMENSIONS: usize = 6;

/// The dimensions and lower bounds fixed by the first accumulated array.
#[derive(Default)]
pub(super) struct ArrayInputShape {
    first: Option<(Vec<usize>, Vec<i32>)>,
}

impl ArrayInputShape {
    /// Check one transition input in the order the transition function sees it.
    pub(super) fn accept(&mut self, value: &Value) -> Result<(), SQLError> {
        let array = match value {
            Value::Null => {
                return Err(SQLError::Routine {
                    sqlstate: "22004".into(),
                    message: "cannot accumulate null arrays".into(),
                })
            }
            Value::Array(array) => array,
            other => {
                return Err(SQLError::Internal(format!(
                    "array_agg over arrays received a non-array input {other:?}"
                )))
            }
        };
        let dimensions = array.dimensions();
        let lower_bounds = array.lower_bounds();
        let Some((first_dimensions, first_lower_bounds)) = &self.first else {
            if dimensions.is_empty() {
                return Err(array_subscript_error("cannot accumulate empty arrays"));
            }
            if dimensions.len() + 1 > MAX_ARRAY_DIMENSIONS {
                return Err(SQLError::Routine {
                    sqlstate: "54000".into(),
                    message: format!(
                        "number of array dimensions ({}) exceeds the maximum allowed ({MAX_ARRAY_DIMENSIONS})",
                        dimensions.len() + 1
                    ),
                });
            }
            self.first = Some((dimensions.to_vec(), lower_bounds.to_vec()));
            return Ok(());
        };
        if first_dimensions.as_slice() != dimensions
            || first_lower_bounds.as_slice() != lower_bounds
        {
            return Err(array_subscript_error(
                "cannot accumulate arrays of different dimensionality",
            ));
        }
        Ok(())
    }

    /// Stack accepted arrays under a new leading dimension that starts at 1, keeping the inputs' lower bounds.
    pub(super) fn stack(values: Vec<Value>) -> Result<Value, SQLError> {
        let mut shape = Self::default();
        for value in &values {
            shape.accept(value)?;
        }
        let Some((_, lower_bounds)) = shape.first else {
            return Ok(Value::Null);
        };
        let lower_bounds = std::iter::once(1).chain(lower_bounds).collect();
        ArrayValue::with_lower_bounds(values, lower_bounds)
            .map(Value::Array)
            .ok_or_else(|| {
                SQLError::Internal("accumulated arrays do not form a rectangular array".into())
            })
    }
}

fn array_subscript_error(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "2202E".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::ArrayInputShape;
    use uqa_core::{ArrayValue, Value};

    fn array(elements: Vec<Value>) -> Value {
        Value::Array(ArrayValue::try_new(elements).unwrap())
    }

    #[test]
    fn inputs_follow_accum_array_result_arr() {
        let mut shape = ArrayInputShape::default();
        assert_eq!(
            shape.accept(&array(Vec::new())).unwrap_err().sqlstate(),
            Some("2202E")
        );
        shape.accept(&array(vec![Value::Int(1)])).unwrap();
        let mismatch = shape
            .accept(&array(vec![Value::Int(1), Value::Int(2)]))
            .unwrap_err();
        assert_eq!(mismatch.sqlstate(), Some("2202E"));
        assert_eq!(
            mismatch.to_string(),
            "cannot accumulate arrays of different dimensionality"
        );
        let null = shape.accept(&Value::Null).unwrap_err();
        assert_eq!(null.sqlstate(), Some("22004"));
        assert_eq!(null.to_string(), "cannot accumulate null arrays");
    }

    #[test]
    fn stacking_keeps_the_input_lower_bounds() {
        let shifted = |values: Vec<Value>| {
            Value::Array(ArrayValue::with_lower_bounds(values, vec![0]).unwrap())
        };
        let Value::Array(stacked) = ArrayInputShape::stack(vec![
            shifted(vec![Value::Int(1), Value::Int(2)]),
            shifted(vec![Value::Int(3), Value::Null]),
        ])
        .unwrap() else {
            panic!("stacked arrays produce an array");
        };
        assert_eq!(stacked.dimensions(), [2, 2]);
        assert_eq!(stacked.lower_bounds(), [1, 0]);
    }
}
