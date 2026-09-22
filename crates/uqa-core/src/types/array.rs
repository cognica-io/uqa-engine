//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` SQL arrays with explicit dimension lower bounds.

use super::Value;

mod elements;
mod shape;
pub use elements::{ArrayTraversalError, BudgetedArrayElements};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayValue {
    storage: Box<ArrayStorage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArrayStorage {
    elements: Vec<Value>,
    dimensions: Vec<usize>,
    lower_bounds: Vec<i32>,
}

impl ArrayValue {
    pub fn try_new(elements: Vec<Value>) -> Option<Self> {
        let dimensions = normalized_shape(&elements)?;
        let lower_bounds = vec![1; dimensions.len()];
        Some(Self::from_decoded_parts(elements, dimensions, lower_bounds))
    }

    pub fn with_lower_bounds(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Option<Self> {
        let dimensions = normalized_shape(&elements)?;
        if dimensions.len() != lower_bounds.len() {
            return None;
        }
        Some(Self::from_decoded_parts(elements, dimensions, lower_bounds))
    }

    /// Validate borrowed input before a tagged decoder transfers its values. Rejected tags must preserve the complete original map.
    pub(super) fn decoded_shape(elements: &[Value]) -> Option<Vec<usize>> {
        normalized_shape(elements)
    }

    pub(super) fn decoded_shape_budgeted(
        elements: &[Value],
        memory: &crate::memory::MemoryBudget,
        cancellation: &crate::CancellationToken,
    ) -> Result<Option<crate::memory::Budgeted<Vec<usize>>>, super::ValueRetentionError> {
        shape::budgeted(elements, memory, cancellation)
    }

    /// Reserve this exact boxed layout before consuming validated decoded buffers.
    pub(super) const fn decoded_header_bytes() -> usize {
        size_of::<ArrayStorage>()
    }

    /// Consume the values and dimensions validated by this owner, preserving their existing buffers.
    pub(super) fn from_decoded_parts(
        mut elements: Vec<Value>,
        dimensions: Vec<usize>,
        lower_bounds: Vec<i32>,
    ) -> Self {
        debug_assert_eq!(dimensions.len(), lower_bounds.len());
        normalize_nested_arrays(&mut elements);
        Self {
            storage: Box::new(ArrayStorage {
                elements,
                dimensions,
                lower_bounds,
            }),
        }
    }

    pub fn elements(&self) -> &[Value] {
        &self.storage.elements
    }

    pub fn into_elements(self) -> Vec<Value> {
        self.storage.elements
    }

    pub fn lower_bounds(&self) -> &[i32] {
        &self.storage.lower_bounds
    }

    pub fn dimensions(&self) -> &[usize] {
        &self.storage.dimensions
    }

    pub fn lower_bound(&self, dimension: usize) -> Option<i32> {
        self.storage.lower_bounds.get(dimension).copied()
    }

    pub fn upper_bound(&self, dimension: usize) -> Option<i64> {
        let lower = i64::from(self.lower_bound(dimension)?);
        let length = i64::try_from(*self.storage.dimensions.get(dimension)?).ok()?;
        (length > 0).then(|| lower + length - 1)
    }

    pub fn with_elements(&self, elements: Vec<Value>) -> Option<Self> {
        Self::with_lower_bounds(elements, self.storage.lower_bounds.clone())
    }

    /// Heap bytes used by the boxed array headers. Element buffers are accounted for by callers together with their recursively retained values.
    pub fn retained_header_bytes(&self) -> usize {
        Self::decoded_header_bytes()
    }

    pub(super) fn retained_buffer_bytes(&self) -> Result<usize, crate::memory::MemoryError> {
        let buffers = [
            (self.storage.elements.capacity(), size_of::<Value>()),
            (self.storage.dimensions.capacity(), size_of::<usize>()),
            (self.storage.lower_bounds.capacity(), size_of::<i32>()),
        ];
        buffers
            .into_iter()
            .try_fold(self.retained_header_bytes(), |bytes, (capacity, width)| {
                capacity
                    .checked_mul(width)
                    .and_then(|buffer| bytes.checked_add(buffer))
                    .ok_or(crate::memory::MemoryError::SizeOverflow)
            })
    }
}

fn normalize_nested_arrays(elements: &mut [Value]) {
    for value in elements {
        if matches!(value, Value::Array(_)) {
            let Value::Array(array) = std::mem::take(value) else {
                unreachable!("array variant was checked");
            };
            *value = Value::List(array.into_elements());
        }
        if let Value::List(values) = value {
            normalize_nested_arrays(values);
        }
    }
}

fn normalized_shape(elements: &[Value]) -> Option<Vec<usize>> {
    shape::unbounded(elements)
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
            elements: &self.storage.elements,
            lower_bounds: &self.storage.lower_bounds,
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
