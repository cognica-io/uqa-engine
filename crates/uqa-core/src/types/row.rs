//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Anonymous row values retain the descriptor of the expression that produced them.

use super::{Value, ValueRetentionError};
use crate::memory::{MemoryError, Produced, ProductionControl};

/// One field's type identity, supplied by the type-system owner. Core carries this metadata without resolving or interpreting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RecordFieldType {
    pub oid: u32,
    pub type_modifier: i32,
}

/// An anonymous row and its optional producer descriptor. A boxed header keeps the inline dynamic value layout independent of descriptor storage.
#[derive(Debug, Clone, Default)]
pub struct RowValue {
    storage: Box<RowStorage>,
}

#[derive(Debug, Clone, Default)]
struct RowStorage {
    values: Vec<Value>,
    field_types: Option<Vec<RecordFieldType>>,
}

impl RowValue {
    #[must_use]
    pub fn new(values: Vec<Value>) -> Self {
        Self::from_validated_parts(values, None)
    }

    /// Retain exactly one type for each value, including null fields.
    pub fn typed(
        values: Vec<Value>,
        field_types: Vec<RecordFieldType>,
    ) -> Result<Self, ValueRetentionError> {
        Self::validate_width(values.len(), field_types.len())?;
        Ok(Self::from_validated_parts(values, Some(field_types)))
    }

    #[must_use]
    pub fn values(&self) -> &[Value] {
        &self.storage.values
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Value] {
        self.values()
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.storage.values.capacity()
    }

    #[must_use]
    pub fn field_types(&self) -> Option<&[RecordFieldType]> {
        self.storage.field_types.as_deref()
    }

    /// Consume the carrier when its descriptor is no longer needed. Use `into_parts` when transforming a typed row.
    #[must_use]
    pub fn into_values(self) -> Vec<Value> {
        self.storage.values
    }

    #[must_use]
    pub fn into_parts(self) -> (Vec<Value>, Option<Vec<RecordFieldType>>) {
        (self.storage.values, self.storage.field_types)
    }

    /// Replace field values while retaining their producer descriptor. A typed row cannot change width through this interface.
    pub fn with_values(mut self, values: Vec<Value>) -> Result<Self, ValueRetentionError> {
        if let Some(types) = &self.storage.field_types {
            Self::validate_width(values.len(), types.len())?;
        }
        self.storage.values = values;
        Ok(self)
    }

    /// Transfer admitted values and reserve the boxed header before allocation.
    pub fn new_with_control(
        values: Produced<Vec<Value>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        Self::produce(values, None, control)
    }

    /// Transfer both admitted buffers without copying or giving the descriptor a separate allowance.
    pub fn typed_with_control(
        values: Produced<Vec<Value>>,
        field_types: Produced<Vec<RecordFieldType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        Self::validate_width(values.len(), field_types.len())?;
        Self::produce(values, Some(field_types), control)
    }

    fn produce(
        values: Produced<Vec<Value>>,
        field_types: Option<Produced<Vec<RecordFieldType>>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        let header = control.reserve(Self::decoded_header_bytes())?;
        let (values, values_memory) = values.into_parts();
        let (field_types, types_memory) = match field_types {
            Some(types) => {
                let (types, memory) = types.into_parts();
                (Some(types), memory)
            }
            None => (None, None),
        };
        let memory = control.combine(control.combine(values_memory, types_memory), header);
        control.finish(Self::from_validated_parts(values, field_types), memory)
    }

    pub(super) fn validate_width(values: usize, types: usize) -> Result<(), ValueRetentionError> {
        if values != types {
            return Err(ValueRetentionError::Malformed {
                kind: "row",
                reason: format!("{values} values have {types} field types"),
            });
        }
        Ok(())
    }

    /// Reserve these boxed-header bytes before transferring separately admitted value and descriptor buffers into a row.
    #[must_use]
    pub const fn retained_header_bytes() -> usize {
        size_of::<RowStorage>()
    }

    pub(super) const fn decoded_header_bytes() -> usize {
        Self::retained_header_bytes()
    }

    /// Values and descriptor have already passed the shape check; callers reserve their buffers and this header before transferring ownership.
    pub(super) fn from_validated_parts(
        values: Vec<Value>,
        field_types: Option<Vec<RecordFieldType>>,
    ) -> Self {
        debug_assert!(field_types
            .as_ref()
            .is_none_or(|types| types.len() == values.len()));
        Self {
            storage: Box::new(RowStorage {
                values,
                field_types,
            }),
        }
    }

    /// Count the boxed header and both owned buffer capacities, excluding nested value payloads. Overflow is reported before an owner admits the retained row.
    pub fn retained_buffer_bytes(&self) -> Result<usize, MemoryError> {
        let values = self
            .storage
            .values
            .capacity()
            .checked_mul(size_of::<Value>());
        let types = self.storage.field_types.as_ref().map_or(Some(0), |types| {
            types.capacity().checked_mul(size_of::<RecordFieldType>())
        });
        values
            .zip(types)
            .and_then(|(values, types)| values.checked_add(types))
            .and_then(|bytes| bytes.checked_add(Self::decoded_header_bytes()))
            .ok_or(MemoryError::SizeOverflow)
    }
}

impl From<Vec<Value>> for RowValue {
    fn from(values: Vec<Value>) -> Self {
        Self::new(values)
    }
}

impl FromIterator<Value> for RowValue {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

impl std::ops::Deref for RowValue {
    type Target = [Value];

    fn deref(&self) -> &Self::Target {
        self.values()
    }
}

impl std::ops::DerefMut for RowValue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.storage.values
    }
}

impl AsRef<[Value]> for RowValue {
    fn as_ref(&self) -> &[Value] {
        self.values()
    }
}

impl IntoIterator for RowValue {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_values().into_iter()
    }
}

impl<'a> IntoIterator for &'a RowValue {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.values().iter()
    }
}

impl<'a> IntoIterator for &'a mut RowValue {
    type Item = &'a mut Value;
    type IntoIter = std::slice::IterMut<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.storage.values.iter_mut()
    }
}

impl PartialEq for RowValue {
    fn eq(&self, other: &Self) -> bool {
        self.values() == other.values()
    }
}

impl Eq for RowValue {}

impl serde::Serialize for RowValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct TaggedRow<'a> {
            #[serde(rename = "$uqa_type")]
            kind: &'static str,
            values: &'a [Value],
            #[serde(skip_serializing_if = "Option::is_none")]
            field_types: Option<&'a [RecordFieldType]>,
        }
        TaggedRow {
            kind: "row",
            values: self.values(),
            field_types: self.field_types(),
        }
        .serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for RowValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Row(row) => Ok(row),
            _ => Err(serde::de::Error::custom("expected a tagged row value")),
        }
    }
}

#[cfg(test)]
mod tests;
