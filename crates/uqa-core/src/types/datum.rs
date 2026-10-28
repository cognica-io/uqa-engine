//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable encoded SQL datum storage. Core retains byte identity; SQL owns physical interpretation and diagnostics at the consuming operation.

use std::sync::Arc;

use crate::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DatumValue {
    type_oid: u32,
    offset: u32,
    bytes: Arc<Vec<u8>>,
    record_fields: Option<Arc<Vec<RecordDatumField>>>,
}

/// An expanded record's physical field slots. SQL owns type identities and
/// interpretation; the original tuple remains available for ordinary output.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RecordDatumField {
    pub name: String,
    pub type_oid: u32,
    pub type_modifier: i32,
    pub collation_oid: Option<u32>,
    pub length: i64,
    pub alignment: u8,
    pub dropped: bool,
}

impl DatumValue {
    /// Retain an owned tuple and a field's byte address without reading through that address. An out-of-range address remains representable so the eventual SQL consumer can report the corresponding read error.
    pub fn new(type_oid: u32, offset: u32, bytes: Vec<u8>) -> Self {
        Self {
            type_oid,
            offset,
            bytes: Arc::new(bytes),
            record_fields: None,
        }
    }

    /// Transfer admitted physical bytes and reserve their shared allocation header before constructing a retained view. Reading the offset remains the SQL consumer's responsibility.
    pub fn with_control(
        type_oid: u32,
        offset: u32,
        bytes: Produced<Vec<u8>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let header = control.reserve(Self::retained_header_bytes())?;
        let (bytes, memory) = bytes.into_parts();
        let memory = control.combine(header, memory);
        control.finish(Self::new(type_oid, offset, bytes), memory)
    }

    /// A second field view shares the original allocation.
    pub fn field(&self, type_oid: u32, offset: u32) -> Self {
        Self {
            type_oid,
            offset,
            bytes: self.bytes.clone(),
            record_fields: None,
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

    pub fn record_fields(&self) -> Option<&[RecordDatumField]> {
        self.record_fields.as_deref().map(Vec::as_slice)
    }

    /// Preserve an expanded record's descriptor without decoding its fields.
    pub fn with_record_fields(mut self, fields: Vec<RecordDatumField>) -> Self {
        self.record_fields = Some(Arc::new(fields));
        self
    }

    /// Attach admitted field metadata and retain an independent lease on the original backing allocation.
    pub fn with_record_fields_with_control(
        &self,
        fields: Produced<Vec<RecordDatumField>>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, ValueRetentionError> {
        let backing = control.reserve(self.bytes.capacity() + Self::retained_header_bytes())?;
        let header = control.reserve(Self::retained_header_bytes())?;
        let (fields, memory) = fields.into_parts();
        let value = Self {
            type_oid: self.type_oid,
            offset: self.offset,
            bytes: self.bytes.clone(),
            record_fields: Some(Arc::new(fields)),
        };
        control.finish(
            value,
            control.combine(backing, control.combine(header, memory)),
        )
    }

    /// Flatten an expanded record without observing or rewriting its tuple bytes.
    pub fn without_record_fields(mut self) -> Self {
        self.record_fields = None;
        self
    }

    /// Charge each retained view for its full backing allocation. This conservative ownership rule keeps independently retained views bounded even when the original plan is released.
    pub fn retained_bytes(&self) -> usize {
        self.bytes.capacity()
            + Self::retained_header_bytes()
            + self.record_fields.as_ref().map_or(0, |fields| {
                fields.capacity() * size_of::<RecordDatumField>()
                    + Self::retained_header_bytes()
                    + fields
                        .iter()
                        .map(|field| field.name.capacity())
                        .sum::<usize>()
            })
    }

    const fn retained_header_bytes() -> usize {
        size_of::<Vec<u8>>() + 2 * size_of::<usize>()
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

impl crate::memory::ObserveAllocations for DatumValue {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut crate::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), crate::ValueRetentionError> {
        if observer.shared_owner(self.bytes.as_ref())? {
            observer.allocation(size_of::<Vec<u8>>(), size_of::<Vec<u8>>())?;
            observer.buffer::<u8>(self.bytes.len(), self.bytes.capacity())?;
        }
        if let Some(fields) = &self.record_fields {
            if observer.shared_owner(fields.as_ref())? {
                observer.allocation(
                    size_of::<Vec<RecordDatumField>>(),
                    size_of::<Vec<RecordDatumField>>(),
                )?;
                observer.buffer::<RecordDatumField>(fields.len(), fields.capacity())?;
                for field in fields.iter() {
                    observer.child(&field.name)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{MemoryBudget, ProductionVec},
        CancellationToken, Value,
    };

    #[test]
    fn admitted_datum_production_transfers_bytes_and_reserves_its_shared_header() {
        let token = CancellationToken::new();
        let header = DatumValue::retained_header_bytes();
        for limit in [0, header - 1, header, 4096] {
            let memory = MemoryBudget::new(limit);
            let control = ProductionControl::new(&memory, &token, &token);
            let bytes = ProductionVec::new(control).finish().unwrap();
            let result = DatumValue::with_control(25, u32::MAX, bytes, &control);
            assert_eq!(result.is_ok(), limit >= header);
            if let Ok(datum) = result {
                assert_eq!(datum.type_oid(), 25);
                assert_eq!(datum.offset(), u32::MAX);
                assert_eq!(datum.reserved_bytes(), datum.retained_bytes());
            }
            assert_eq!(memory.used(), 0);
        }
        let memory = MemoryBudget::new(4096);
        let control = ProductionControl::new(&memory, &token, &token);
        let mut bytes = ProductionVec::new(control);
        bytes.reserve(128).unwrap();
        bytes.push_copy(7).unwrap();
        let pointer = bytes.as_ptr();
        let datum = DatumValue::with_control(25, 0, bytes.finish().unwrap(), &control).unwrap();
        assert_eq!(pointer, datum.bytes().as_ptr());
        assert_eq!(datum.reserved_bytes(), datum.retained_bytes());
        drop(datum);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn admitted_datum_failure_releases_owned_bytes_and_honors_both_cancellations() {
        for cancelled in [None, Some(false), Some(true)] {
            let memory = MemoryBudget::new(if cancelled.is_none() {
                DatumValue::retained_header_bytes()
            } else {
                4096
            });
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut bytes = ProductionVec::new(control);
            bytes.push_copy(1).unwrap();
            let bytes = bytes.finish().unwrap();
            if let Some(invoking_cancelled) = cancelled {
                if invoking_cancelled {
                    invoking.cancel();
                } else {
                    original.cancel();
                }
            }
            assert!(DatumValue::with_control(25, 0, bytes, &control).is_err());
            assert_eq!(memory.used(), 0);
        }
    }

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

    #[test]
    fn expanded_record_layout_survives_retention_and_spill_without_reaching_tuple_bytes() {
        let fields = vec![RecordDatumField {
            name: "value".into(),
            type_oid: 23,
            type_modifier: -1,
            collation_oid: None,
            length: 4,
            alignment: b'i',
            dropped: false,
        }];
        let datum = DatumValue::new(16_384, u32::MAX, vec![0]).with_record_fields(fields.clone());
        let bytes = datum.retained_bytes();
        let value = Value::Datum(datum.clone());
        let token = CancellationToken::new();
        let denied = MemoryBudget::new(bytes - 1);
        assert!(value.clone_budgeted(&denied, &token).is_err());
        assert_eq!(denied.used(), 0);
        let memory = MemoryBudget::new(bytes);
        let retained = value.clone_budgeted(&memory, &token).unwrap();
        assert_eq!(memory.used(), bytes);
        let encoded = serde_json::to_vec(&*retained).unwrap();
        let restored: Value = serde_json::from_slice(&encoded).unwrap();
        assert!(restored.has_same_representation(&value));
        assert_eq!(datum.record_fields(), Some(fields.as_slice()));
        assert!(datum.field(23, 0).record_fields().is_none());
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
}
