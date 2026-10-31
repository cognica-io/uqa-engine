//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional named-record values with immutable, producer-owned names.

use super::{Value, ValueRetentionError};
use crate::memory::{MemoryError, Produced, ProductionControl};
mod descriptor;
pub use descriptor::RecordDescriptor;

pub type RecordIter<'a> = std::iter::Zip<std::slice::Iter<'a, String>, std::slice::Iter<'a, Value>>;
pub type RecordIterMut<'a> =
    std::iter::Zip<std::slice::Iter<'a, String>, std::slice::IterMut<'a, Value>>;

#[derive(Debug, Clone, Default)]
pub struct RecordValue {
    storage: Box<RecordStorage>,
}

#[derive(Debug, Clone, Default)]
struct RecordStorage {
    values: Vec<Value>,
    descriptor: RecordDescriptor,
}

impl RecordValue {
    #[must_use]
    pub fn new(fields: Vec<(String, Value)>) -> Self {
        Self::from_parts(fields, None)
    }

    /// Preserve the ordered names and opaque producer type identity.
    #[must_use]
    pub fn from_parts(fields: Vec<(String, Value)>, type_oid: Option<u32>) -> Self {
        let mut names = Vec::with_capacity(fields.len());
        let mut values = Vec::with_capacity(fields.len());
        for (name, value) in fields {
            names.push(name);
            values.push(value);
        }
        Self::from_validated_parts(values, RecordDescriptor::new(names, type_oid))
    }

    pub fn from_shared_parts(
        values: Vec<Value>,
        descriptor: RecordDescriptor,
    ) -> Result<Self, ValueRetentionError> {
        Self::validate_width(values.len(), descriptor.field_names().len())?;
        Ok(Self::from_validated_parts(values, descriptor))
    }

    pub(in crate::types) fn from_validated_parts(
        values: Vec<Value>,
        descriptor: RecordDescriptor,
    ) -> Self {
        debug_assert_eq!(values.len(), descriptor.field_names().len());
        Self {
            storage: Box::new(RecordStorage { values, descriptor }),
        }
    }

    fn validate_width(values: usize, names: usize) -> Result<(), ValueRetentionError> {
        if values != names {
            return Err(ValueRetentionError::Malformed {
                kind: "record",
                reason: format!("{values} values have {names} field names"),
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn descriptor(&self) -> &RecordDescriptor {
        &self.storage.descriptor
    }
    #[must_use]
    pub fn type_oid(&self) -> Option<u32> {
        self.descriptor().type_oid()
    }
    #[must_use]
    pub fn with_type_oid(mut self, type_oid: Option<u32>) -> Self {
        self.storage.descriptor = self.storage.descriptor.with_type_oid(type_oid);
        self
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.storage.values.len()
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.storage.values.is_empty()
    }
    #[must_use]
    pub fn values(&self) -> &[Value] {
        &self.storage.values
    }
    pub fn values_mut(&mut self) -> &mut [Value] {
        &mut self.storage.values
    }
    pub fn iter(&self) -> RecordIter<'_> {
        self.descriptor().field_names().iter().zip(self.values())
    }
    pub fn iter_mut(&mut self) -> RecordIterMut<'_> {
        self.storage
            .descriptor
            .field_names()
            .iter()
            .zip(self.storage.values.iter_mut())
    }
    #[must_use]
    pub fn get(&self, index: usize) -> Option<(&String, &Value)> {
        Some((
            self.descriptor().field_names().get(index)?,
            self.values().get(index)?,
        ))
    }
    pub fn get_mut(&mut self, index: usize) -> Option<(&String, &mut Value)> {
        Some((
            self.storage.descriptor.field_names().get(index)?,
            self.storage.values.get_mut(index)?,
        ))
    }
    #[must_use]
    pub fn first(&self) -> Option<(&String, &Value)> {
        self.get(0)
    }
    #[must_use]
    pub fn last(&self) -> Option<(&String, &Value)> {
        self.len().checked_sub(1).and_then(|index| self.get(index))
    }

    /// Return independently mutable names and owned values. Internal transforms
    /// should use `into_shared_parts` to retain the descriptor without copying.
    #[must_use]
    pub fn into_parts(self) -> (Vec<(String, Value)>, Option<u32>) {
        let oid = self.type_oid();
        (self.into_iter().collect(), oid)
    }
    #[must_use]
    pub fn into_values(self) -> Vec<Value> {
        self.storage.values
    }
    #[must_use]
    pub fn into_shared_parts(self) -> (Vec<Value>, RecordDescriptor) {
        (self.storage.values, self.storage.descriptor)
    }
    pub fn with_values(mut self, values: Vec<Value>) -> Result<Self, ValueRetentionError> {
        Self::validate_width(values.len(), self.len())?;
        self.storage.values = values;
        Ok(self)
    }
    #[must_use]
    pub fn with_fields(self, fields: Vec<(String, Value)>) -> Self {
        Self::from_parts(fields, self.type_oid())
    }

    /// Split admitted pairs into positional values and immutable names. Admit
    /// every destination before allocation and transfer payload leases unchanged.
    pub fn with_control(
        fields: Produced<Vec<(String, Value)>>,
        type_oid: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let width = fields.len();
        let value_bytes = width
            .checked_mul(size_of::<Value>())
            .ok_or(MemoryError::SizeOverflow)?;
        let name_bytes = RecordDescriptor::field_bytes(width)?;
        let value_memory = control.reserve(value_bytes)?;
        let name_buffer_memory = control.reserve(name_bytes)?;
        let source_bytes = fields
            .capacity()
            .checked_mul(size_of::<(String, Value)>())
            .ok_or(MemoryError::SizeOverflow)?;
        let (fields, mut payload) = fields.into_parts();
        let mut names = Vec::with_capacity(width);
        let mut values = Vec::with_capacity(width);
        let mut names_payload = 0_usize;
        for (name, value) in fields {
            control.check()?;
            names_payload = names_payload
                .checked_add(name.capacity())
                .ok_or(MemoryError::SizeOverflow)?;
            names.push(name);
            values.push(value);
        }
        if let Some(memory) = &mut payload {
            drop(memory.split(source_bytes));
        }
        let name_payload_memory = payload.as_mut().map(|memory| memory.split(names_payload));
        let names = control.finish(
            names,
            control.combine(name_buffer_memory, name_payload_memory),
        )?;
        let descriptor = RecordDescriptor::with_control(names, type_oid, control)?;
        let values = control.finish(values, control.combine(value_memory, payload))?;
        Self::with_descriptor_control(values, descriptor, control)
    }

    /// Reuse producer metadata while retaining its logical charge for this row.
    pub fn with_shared_control(
        values: Produced<Vec<Value>>,
        descriptor: RecordDescriptor,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        Self::validate_width(values.len(), descriptor.field_names().len())?;
        let memory = control.reserve(descriptor.retained_payload_bytes()?)?;
        let descriptor = control.finish(descriptor, memory)?;
        Self::with_descriptor_control(values, descriptor, control)
    }

    /// Transfer separately admitted values and metadata without another copy.
    pub fn with_descriptor_control(
        values: Produced<Vec<Value>>,
        descriptor: Produced<RecordDescriptor>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        Self::validate_width(values.len(), descriptor.field_names().len())?;
        let header = control.reserve(Self::retained_header_bytes())?;
        let (values, memory) = values.into_parts();
        let (descriptor, names_memory) = descriptor.into_parts();
        control.finish(
            Self::from_validated_parts(values, descriptor),
            control.combine(control.combine(header, memory), names_memory),
        )
    }

    #[must_use]
    pub const fn retained_header_bytes() -> usize {
        size_of::<RecordStorage>()
    }
    pub fn retained_buffer_bytes(&self) -> Result<usize, MemoryError> {
        self.storage
            .values
            .capacity()
            .checked_mul(size_of::<Value>())
            .and_then(|bytes| bytes.checked_add(self.descriptor().retained_buffer_bytes().ok()?))
            .and_then(|bytes| bytes.checked_add(Self::retained_header_bytes()))
            .ok_or(MemoryError::SizeOverflow)
    }
}

impl From<Vec<(String, Value)>> for RecordValue {
    fn from(fields: Vec<(String, Value)>) -> Self {
        Self::new(fields)
    }
}

impl FromIterator<(String, Value)> for RecordValue {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(fields: T) -> Self {
        Self::new(fields.into_iter().collect())
    }
}

impl IntoIterator for RecordValue {
    type Item = (String, Value);
    type IntoIter = std::iter::Zip<std::vec::IntoIter<String>, std::vec::IntoIter<Value>>;
    fn into_iter(self) -> Self::IntoIter {
        self.storage
            .descriptor
            .into_field_names()
            .into_iter()
            .zip(self.storage.values)
    }
}
impl<'a> IntoIterator for &'a RecordValue {
    type Item = (&'a String, &'a Value);
    type IntoIter = RecordIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a> IntoIterator for &'a mut RecordValue {
    type Item = (&'a String, &'a mut Value);
    type IntoIter = RecordIterMut<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}
impl PartialEq for RecordValue {
    fn eq(&self, other: &Self) -> bool {
        self.descriptor().field_names() == other.descriptor().field_names()
            && self.values() == other.values()
    }
}
impl Eq for RecordValue {}

impl serde::Serialize for RecordValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeSeq, SerializeStruct};
        struct Fields<'a>(&'a RecordValue);
        impl serde::Serialize for Fields<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
                for field in self.0 {
                    sequence.serialize_element(&field)?;
                }
                sequence.end()
            }
        }
        let mut record = serializer.serialize_struct(
            "TaggedRecord",
            if self.type_oid().is_some() { 3 } else { 2 },
        )?;
        record.serialize_field("$uqa_type", "record")?;
        record.serialize_field("fields", &Fields(self))?;
        if self.type_oid().is_some() {
            record.serialize_field("type_oid", &self.type_oid())?;
        }
        record.end()
    }
}
impl<'de> serde::Deserialize<'de> for RecordValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Record(record) => Ok(record),
            _ => Err(serde::de::Error::custom("expected a tagged record value")),
        }
    }
}
#[cfg(test)]
mod tests;

impl crate::memory::ObserveAllocations for RecordValue {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut crate::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), crate::ValueRetentionError> {
        observer.allocation(size_of::<RecordStorage>(), size_of::<RecordStorage>())?;
        let RecordStorage { values, descriptor } = self.storage.as_ref();
        observer.child(values)?;
        observer.child(descriptor)?;
        Ok(())
    }
}
