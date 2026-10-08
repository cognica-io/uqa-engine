//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable encoded SQL datum storage. Core retains byte identity; SQL owns physical interpretation and diagnostics at the consuming operation.

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DatumValue {
    type_oid: u32,
    offset: u32,
    bytes: Arc<Vec<u8>>,
}

impl DatumValue {
    /// Retain an owned tuple and a field's byte address without reading through that address. An out-of-range address remains representable so the eventual SQL consumer can report the corresponding read error.
    pub fn new(type_oid: u32, offset: u32, bytes: Vec<u8>) -> Self {
        Self {
            type_oid,
            offset,
            bytes: Arc::new(bytes),
        }
    }

    /// A second field view shares the original allocation.
    pub fn field(&self, type_oid: u32, offset: u32) -> Self {
        Self {
            type_oid,
            offset,
            bytes: self.bytes.clone(),
        }
    }

    pub fn type_oid(&self) -> u32 {
        self.type_oid
    }
    pub fn offset(&self) -> u32 {
        self.offset
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Charge each retained view for its full backing allocation. This conservative ownership rule keeps independently retained views bounded even when the original plan is released.
    pub fn retained_bytes(&self) -> usize {
        self.bytes.capacity() + size_of::<Vec<u8>>() + 2 * size_of::<usize>()
    }
}

impl serde::Serialize for DatumValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::Value::Datum(self.clone()).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for DatumValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match crate::Value::deserialize(deserializer)? {
            crate::Value::Datum(value) => Ok(value),
            _ => Err(serde::de::Error::custom(
                "expected a retained physical datum",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::MemoryBudget, CancellationToken, Value};

    #[test]
    fn field_views_retain_original_bytes_identity_and_an_independent_lease() {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend_from_slice(&[2, 0, 0, 0, 3, b'x']);
        let datum = DatumValue::new(1700, 0, bytes);
        let view = datum.field(25, 4);
        assert_eq!(datum.bytes().as_ptr(), view.bytes().as_ptr());
        let memory = MemoryBudget::new(4096);
        let value = Value::Datum(view);
        let retained = value
            .clone_budgeted(&memory, &CancellationToken::new())
            .unwrap();
        assert_eq!(memory.used(), datum.retained_bytes());
        drop(value);
        drop(datum);
        let json = serde_json::to_vec(&*retained).unwrap();
        let restored: Value = serde_json::from_slice(&json).unwrap();
        assert!(restored.has_same_representation(&retained));
        let Value::Datum(restored) = restored else {
            panic!("datum identity was erased")
        };
        assert_eq!(restored.bytes(), &[2, 0, 0, 0, 3, b'x']);
        assert_eq!(restored.offset(), 4);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn raw_datum_tags_reject_invalid_identity_and_preserve_unread_offsets() {
        let value = Value::Datum(DatumValue::new(25, u32::MAX, vec![]));
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(serde_json::from_str::<Value>(&encoded)
            .unwrap()
            .has_same_representation(&value));
        for invalid in [
            r#"{"$uqa_type":"datum","type_oid":-1,"offset":0,"hex":"00"}"#,
            r#"{"$uqa_type":"datum","type_oid":25,"offset":4294967296,"hex":"00"}"#,
            r#"{"$uqa_type":"datum","type_oid":25,"offset":0,"hex":"zz"}"#,
        ] {
            assert!(serde_json::from_str::<Value>(invalid).is_err(), "{invalid}");
        }
    }
}
