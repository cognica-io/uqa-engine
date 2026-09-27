//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field guards retain object ownership and the shared record conflict rules.

use crate::mvcc::native::{
    NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
    NativeSnapshot,
};
use rusqlite::types::ValueRef;
use std::fmt::Write;
use uqa_storage::KeyValueBatch;

impl NativeSnapshot {
    fn vector_field_guard_records(
        &self,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
    ) -> crate::Result<(NativeRecord, NativeRecord)> {
        let prefix = NativeRecordIdentity::new(Family::Vectors, owner)?
            .encode_prefix(&[field], &self.control)?;
        let bytes = prefix
            .len()
            .checked_mul(2)
            .and_then(|size| size.checked_add(48))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let mut memory = self.control.memory().reserve(bytes)?;
        let mut name = String::with_capacity(bytes);
        memory.grow(name.capacity().saturating_sub(bytes))?;
        name.push_str(crate::mvcc::native::VECTOR_FIELD_GUARD_PREFIX);
        for byte in prefix.iter() {
            write!(name, "{byte:02x}").expect("formatting into a string");
        }
        let length = name.len();
        let mut record = |suffix: &str| {
            name.truncate(length);
            name.push_str(suffix);
            NativeRecord::encode(
                Family::Metadata,
                NativeRecordOwner::Database(self.database),
                &[ValueRef::Text(name.as_bytes()), ValueRef::Text(b"1")],
                &self.control,
            )
        };
        let lifetime = record("::lifetime")?;
        let references = record("::references")?;
        Ok((lifetime, references))
    }

    pub(in crate::vector_index) fn vector_field_fingerprint(
        &self,
        owner: NativeRecordOwner,
        field: &[u8],
        control: &uqa_storage::read_control::StorageReadControl,
    ) -> uqa_storage::StorageBackendResult<[u8; 32]> {
        self.control.check()?;
        control.check()?;
        let (_, references) = self.vector_field_guard_records(owner, ValueRef::Text(field))?;
        self.view
            .marker_fingerprint(self.database, references.key(), control)
    }

    pub(in crate::vector_index) fn coordinate_vector_field(
        &self,
        batch: &mut dyn KeyValueBatch,
        owner: NativeRecordOwner,
        field: ValueRef<'_>,
        structural: bool,
    ) -> crate::Result<()> {
        let (lifetime, references) = self.vector_field_guard_records(owner, field)?;
        if structural {
            batch.fence_record(lifetime.key())?;
            batch.fence_record(references.key())?;
        } else {
            batch.require_unchanged(lifetime.key())?;
            batch.touch_marker(references.key(), references.row())?;
        }
        Ok(())
    }
}
