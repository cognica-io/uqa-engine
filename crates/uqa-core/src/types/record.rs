//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named records retain opaque producer type identity without moving SQL interpretation into Core.

use super::{Value, ValueRetentionError};
use crate::memory::{MemoryError, Produced, ProductionControl};

#[derive(Debug, Clone, Default)]
pub struct RecordValue {
    storage: Box<RecordStorage>,
}

#[derive(Debug, Clone, Default)]
struct RecordStorage {
    fields: Vec<(String, Value)>,
    type_oid: Option<u32>,
}

impl RecordValue {
    #[must_use]
    pub fn new(fields: Vec<(String, Value)>) -> Self {
        Self::from_parts(fields, None)
    }

    /// The SQL owner supplies the tuple's actual type OID. Legacy untyped records keep their original fields and no identity.
    #[must_use]
    pub fn from_parts(fields: Vec<(String, Value)>, type_oid: Option<u32>) -> Self {
        Self {
            storage: Box::new(RecordStorage { fields, type_oid }),
        }
    }

    #[must_use]
    pub fn type_oid(&self) -> Option<u32> {
        self.storage.type_oid
    }

    #[must_use]
    pub fn with_type_oid(mut self, type_oid: Option<u32>) -> Self {
        self.storage.type_oid = type_oid;
        self
    }

    #[must_use]
    pub fn into_parts(self) -> (Vec<(String, Value)>, Option<u32>) {
        (self.storage.fields, self.storage.type_oid)
    }

    #[must_use]
    pub fn with_fields(mut self, fields: Vec<(String, Value)>) -> Self {
        self.storage.fields = fields;
        self
    }

    /// Move admitted fields and reserve the boxed descriptor header before allocation.
    pub fn with_control(
        fields: Produced<Vec<(String, Value)>>,
        type_oid: Option<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        control.check()?;
        let header = control.reserve(Self::retained_header_bytes())?;
        let (fields, memory) = fields.into_parts();
        control.finish(
            Self::from_parts(fields, type_oid),
            control.combine(header, memory),
        )
    }

    #[must_use]
    pub const fn retained_header_bytes() -> usize {
        size_of::<RecordStorage>()
    }

    pub fn retained_buffer_bytes(&self) -> Result<usize, MemoryError> {
        self.storage
            .fields
            .capacity()
            .checked_mul(size_of::<(String, Value)>())
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

impl std::ops::Deref for RecordValue {
    type Target = Vec<(String, Value)>;
    fn deref(&self) -> &Self::Target {
        &self.storage.fields
    }
}

impl std::ops::DerefMut for RecordValue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.storage.fields
    }
}

impl IntoIterator for RecordValue {
    type Item = (String, Value);
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.storage.fields.into_iter()
    }
}

impl<'a> IntoIterator for &'a RecordValue {
    type Item = &'a (String, Value);
    type IntoIter = std::slice::Iter<'a, (String, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.storage.fields.iter()
    }
}

impl<'a> IntoIterator for &'a mut RecordValue {
    type Item = &'a mut (String, Value);
    type IntoIter = std::slice::IterMut<'a, (String, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.storage.fields.iter_mut()
    }
}

impl PartialEq for RecordValue {
    fn eq(&self, other: &Self) -> bool {
        self.storage.fields == other.storage.fields
    }
}
impl Eq for RecordValue {}

impl serde::Serialize for RecordValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct TaggedRecord<'a> {
            #[serde(rename = "$uqa_type")]
            kind: &'static str,
            fields: &'a [(String, Value)],
            #[serde(skip_serializing_if = "Option::is_none")]
            type_oid: Option<u32>,
        }
        TaggedRecord {
            kind: "record",
            fields: self,
            type_oid: self.type_oid(),
        }
        .serialize(serializer)
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
mod tests {
    use super::*;
    use crate::{
        memory::{MemoryBudget, ProductionVec},
        CancellationToken,
    };

    #[test]
    fn named_record_identity_survives_json_and_controlled_copies() {
        let fields = vec![("n".into(), Value::Int(7))];
        let legacy = Value::Record(RecordValue::new(fields.clone()));
        let typed = Value::Record(RecordValue::from_parts(fields, Some(20_000)));
        assert_eq!(legacy, typed);
        assert!(!legacy.has_same_representation(&typed));
        for value in [legacy, typed] {
            let json = serde_json::to_string(&value).unwrap();
            let restored: Value = serde_json::from_str(&json).unwrap();
            assert!(restored.has_same_representation(&value));
            let memory = MemoryBudget::new(4096);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&memory, &token, &token);
            let copied = control.copy_value(&value).unwrap();
            assert!(copied.has_same_representation(&value));
            assert_eq!(memory.used(), copied.reserved_bytes());
            assert!(memory.used() >= RecordValue::retained_header_bytes());
            drop(copied);
            assert_eq!(memory.used(), 0);
        }
    }

    #[test]
    fn record_header_is_admitted_before_allocation_and_released_on_failure() {
        let token = CancellationToken::new();
        for limit in [0, RecordValue::retained_header_bytes() - 1, 4096] {
            let memory = MemoryBudget::new(limit);
            let control = ProductionControl::new(&memory, &token, &token);
            let fields = ProductionVec::new(control).finish().unwrap();
            let result = RecordValue::with_control(fields, Some(20_000), &control);
            assert_eq!(result.is_ok(), limit == 4096);
            if let Ok(value) = result {
                assert_eq!(value.type_oid(), Some(20_000));
                assert_eq!(memory.used(), RecordValue::retained_header_bytes());
            }
            assert_eq!(memory.used(), 0);
        }
        let memory = MemoryBudget::new(4096);
        let control = ProductionControl::new(&memory, &token, &token);
        let fields = ProductionVec::new(control).finish().unwrap();
        token.cancel();
        assert!(RecordValue::with_control(fields, None, &control).is_err());
        assert_eq!(memory.used(), 0);
    }
}
