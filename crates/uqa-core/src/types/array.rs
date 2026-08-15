//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` SQL arrays with explicit dimension lower bounds.

use super::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayValue {
    elements: Vec<Value>,
    dimensions: Vec<usize>,
    lower_bounds: Vec<i32>,
}

impl ArrayValue {
    pub fn try_new(elements: Vec<Value>) -> Option<Self> {
        let elements = normalize_nested_arrays(elements);
        let dimensions = normalized_shape(&elements)?;
        let lower_bounds = vec![1; dimensions.len()];
        Some(Self {
            elements,
            dimensions,
            lower_bounds,
        })
    }

    pub fn with_lower_bounds(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Option<Self> {
        let elements = normalize_nested_arrays(elements);
        let dimensions = normalized_shape(&elements)?;
        if dimensions.len() != lower_bounds.len() {
            return None;
        }
        Some(Self {
            elements,
            dimensions,
            lower_bounds,
        })
    }

    pub fn elements(&self) -> &[Value] {
        &self.elements
    }

    pub fn into_elements(self) -> Vec<Value> {
        self.elements
    }

    pub fn lower_bounds(&self) -> &[i32] {
        &self.lower_bounds
    }

    pub fn dimensions(&self) -> &[usize] {
        &self.dimensions
    }

    pub fn lower_bound(&self, dimension: usize) -> Option<i32> {
        self.lower_bounds.get(dimension).copied()
    }

    pub fn upper_bound(&self, dimension: usize) -> Option<i64> {
        let lower = i64::from(self.lower_bound(dimension)?);
        let length = i64::try_from(*self.dimensions.get(dimension)?).ok()?;
        (length > 0).then(|| lower + length - 1)
    }

    pub fn with_elements(&self, elements: Vec<Value>) -> Option<Self> {
        Self::with_lower_bounds(elements, self.lower_bounds.clone())
    }
}

fn normalize_nested_arrays(elements: Vec<Value>) -> Vec<Value> {
    elements
        .into_iter()
        .map(|value| match value {
            Value::Array(array) => Value::List(normalize_nested_arrays(array.into_elements())),
            Value::List(values) => Value::List(normalize_nested_arrays(values)),
            other => other,
        })
        .collect()
}

fn normalized_shape(elements: &[Value]) -> Option<Vec<usize>> {
    let shape = array_shape(elements)?;
    if shape.first() == Some(&0) {
        Some(Vec::new())
    } else {
        Some(shape)
    }
}

fn array_shape(elements: &[Value]) -> Option<Vec<usize>> {
    let mut dimensions = vec![elements.len()];
    let mut nested_shape: Option<Vec<usize>> = None;
    let mut has_scalar = false;
    for element in elements {
        if let Value::List(nested) = element {
            let shape = array_shape(nested)?;
            if has_scalar
                || nested_shape
                    .as_ref()
                    .is_some_and(|expected| *expected != shape)
            {
                return None;
            }
            nested_shape = Some(shape);
        } else {
            if nested_shape.is_some() {
                return None;
            }
            has_scalar = true;
        }
    }
    if let Some(shape) = nested_shape {
        dimensions.extend(shape);
    }
    Some(dimensions)
}

impl serde::Serialize for ArrayValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(serde::Serialize)]
        struct EncodedArray<'a> {
            elements: &'a [Value],
            lower_bounds: &'a [i32],
        }

        EncodedArray {
            elements: &self.elements,
            lower_bounds: &self.lower_bounds,
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for ArrayValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct EncodedArray {
            elements: Vec<Value>,
            lower_bounds: Vec<i32>,
        }

        let encoded = EncodedArray::deserialize(deserializer)?;
        Self::with_lower_bounds(encoded.elements, encoded.lower_bounds)
            .ok_or_else(|| serde::de::Error::custom("invalid PostgreSQL array dimensions"))
    }
}
