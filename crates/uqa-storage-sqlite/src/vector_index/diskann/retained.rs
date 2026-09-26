//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded canonical reads borrow native rows and preserve their original visibility and controls.

mod changes;

use super::invalid;
use crate::mvcc::native::{
    decode_record, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
    NativeRecordOwner, NativeSnapshot,
};
use crate::vector_index::{encode_doc_id, native::records::Address, SQLiteVectorIndex};
use rusqlite::types::ValueRef;
use std::sync::Arc;
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation},
    DocId,
};
use uqa_storage::{
    diskann_index::{
        format::{DiskANNCanonicalOrigin, DiskANNVectorVersion, CANONICAL_ORIGIN_BYTES},
        DiskANNCanonicalRead, DiskANNCanonicalVectorVisitor,
    },
    mvcc::VersionError,
    read_control::StorageReadControl,
    StorageBackendResult,
};

/// A fixed native canonical source. Each visitor borrows one vector; it must not reenter this source. Failure invalidates any partial output.
pub struct RetainedSQLiteDiskANNCanonical {
    snapshot: Arc<NativeSnapshot>,
    owner: Option<NativeRecordOwner>,
    table: BudgetedVec<u8>,
    field: BudgetedVec<u8>,
    dimensions: u32,
    control: StorageReadControl,
    _memory: MemoryReservation,
}

impl RetainedSQLiteDiskANNCanonical {
    pub(super) fn capture(
        index: &SQLiteVectorIndex,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control
            .memory()
            .reserve(std::mem::size_of::<Self>() + std::mem::size_of::<NativeSnapshot>())?;
        let snapshot = index
            .conn
            .native_snapshot()?
            .ok_or_else(|| invalid("native canonical source requires a bound session"))?;
        let owner = snapshot.table_owner_controlled(&index.table, control)?;
        let mut table = BudgetedVec::new(control.memory());
        table.extend_from_slice(index.table.as_bytes())?;
        let mut field = BudgetedVec::new(control.memory());
        field.extend_from_slice(index.field.as_bytes())?;
        control.check()?;
        Ok(Self {
            snapshot,
            owner,
            table,
            field,
            dimensions: index.dimensions,
            control: control.clone(),
            _memory: memory,
        })
    }

    /// Return an origin only after verifying its complete contiguous native canonical ordinal set. An origin with zero ordinals denotes an explicit empty replacement.
    pub fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.record(document, control)
            .map(|record| record.map(DiskANNCanonicalOrigin::version))
    }

    /// Stream all visible ordinals with their original coordinate bits, independently of navigation, scoring or document candidate selection.
    pub fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        let Some(record) = self.record(document, control)? else {
            return Ok(None);
        };
        if record.count() == 0 {
            return Ok(Some(record.version()));
        }
        let document = encode_doc_id(document)?;
        let dimensions = usize::try_from(self.dimensions)
            .map_err(|_| invalid("canonical dimensions exceed platform size"))?;
        let bytes = dimensions
            .checked_mul(4)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let mut vector = BudgetedVec::new(control.memory());
        vector.reserve(dimensions)?;
        for ordinal in 0..record.count() {
            self.read_payload(document, Some(ordinal), bytes, control, &mut |value| {
                let value = value.ok_or_else(|| invalid("missing native canonical ordinal"))?;
                if value.len() != bytes {
                    return Err(invalid("native canonical vector width mismatch"));
                }
                vector.clear();
                for (coordinate, chunk) in value.chunks_exact(4).enumerate() {
                    if coordinate.is_multiple_of(1024) {
                        self.check(control)?;
                    }
                    vector.push(f32::from_le_bytes(chunk.try_into().expect("fixed width")))?;
                }
                uqa_storage::vector_index::validate_vector_values_controlled(
                    self.dimensions,
                    &vector,
                    Some(control),
                )?;
                visit(ordinal as u32, record.version(), &vector)
            })?;
        }
        self.check(control)?;
        Ok(Some(record.version()))
    }

    fn record(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNCanonicalOrigin>> {
        self.check(control)?;
        let document = encode_doc_id(document)?;
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        let mut record = None;
        self.read_payload(
            document,
            None,
            CANONICAL_ORIGIN_BYTES,
            control,
            &mut |bytes| {
                record = bytes
                    .map(|bytes| DiskANNCanonicalOrigin::decode(bytes, self.dimensions))
                    .transpose()?;
                Ok(())
            },
        )?;
        let identity =
            Identity::new(Family::Vectors, owner).map_err(VersionError::into_storage_error)?;
        let prefix = identity
            .encode_prefix(
                &[ValueRef::Text(&self.field), ValueRef::Integer(document)],
                control,
            )
            .map_err(VersionError::into_storage_error)?;
        let expected = record.map_or(0, DiskANNCanonicalOrigin::count);
        let mut count = 0;
        self.snapshot
            .view
            .visit_keys(&prefix, None, usize::MAX, control, &mut |key, metadata| {
                self.check(control).map_err(VersionError::Storage)?;
                if metadata.live {
                    let address = Address::decode(key, control)?;
                    if count >= expected
                        || address.identity != identity
                        || *address.field != *self.field
                        || address.numbers[..2] != [document, count as i64]
                    {
                        return Err(VersionError::InvalidEncoding(
                            "native canonical origin or ordinal coverage mismatch",
                        ));
                    }
                    count += 1;
                }
                Ok(true)
            })
            .map_err(VersionError::into_storage_error)?;
        if count != expected {
            return Err(invalid("native canonical replacement count mismatch"));
        }
        self.check(control)?;
        Ok(record)
    }

    fn read_payload(
        &self,
        document: i64,
        ordinal: Option<u64>,
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(Option<&[u8]>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.check(control)?;
        let Some(owner) = self.owner else {
            return visit(None);
        };
        let family = if ordinal.is_some() {
            Family::Vectors
        } else {
            Family::VectorOrigins
        };
        let components = [
            ValueRef::Text(&self.field),
            ValueRef::Integer(document),
            ValueRef::Integer(ordinal.unwrap_or(0) as i64),
        ];
        let identity = Identity::new(family, owner).map_err(VersionError::into_storage_error)?;
        let key = identity
            .encode_key(
                &components[..if ordinal.is_some() { 3 } else { 2 }],
                control,
            )
            .map_err(VersionError::into_storage_error)?;
        let limit = crate::mvcc::native::vector_row_limit(
            self.table.len(),
            self.field.len(),
            max_bytes,
            ordinal.is_some(),
        )
        .map_err(VersionError::into_storage_error)?;
        self.snapshot
            .view
            .visit_value_bounded(&key, limit, control, &mut |record| {
                self.check(control).map_err(VersionError::Storage)?;
                let Some(bytes) = record.and_then(|record| record.value) else {
                    return visit(None).map_err(VersionError::Storage);
                };
                let (actual, row) = decode_record(&key, bytes, control)?;
                if actual != identity || row[0] != ValueRef::Text(&self.table) {
                    return Err(VersionError::InvalidEncoding(
                        "native canonical row scope mismatch",
                    ));
                }
                let payload = row[if ordinal.is_some() { 4 } else { 3 }]
                    .as_blob()
                    .map_err(|_| {
                        VersionError::InvalidEncoding("invalid native canonical payload")
                    })?;
                control.check_value_size(payload.len(), max_bytes)?;
                visit(Some(payload)).map_err(VersionError::Storage)
            })
            .map_err(VersionError::into_storage_error)?;
        self.check(control)
    }

    fn check(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.snapshot.control.check()?;
        self.control.check()?;
        control.check()
    }

    fn next_key_document(
        &self,
        family: Family,
        after: Option<i64>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        let identity = Identity::new(family, owner).map_err(VersionError::into_storage_error)?;
        let prefix = identity
            .encode_prefix(&[ValueRef::Text(&self.field)], control)
            .map_err(VersionError::into_storage_error)?;
        let after_key = after
            .map(|document| {
                let components = [
                    ValueRef::Text(&self.field),
                    ValueRef::Integer(document),
                    ValueRef::Integer(i64::MAX),
                ];
                identity.encode_key(
                    &components[..if family == Family::Vectors { 3 } else { 2 }],
                    control,
                )
            })
            .transpose()
            .map_err(VersionError::into_storage_error)?;
        let mut selected = None;
        self.snapshot
            .view
            .visit_keys(
                &prefix,
                after_key.as_deref(),
                usize::MAX,
                control,
                &mut |key, metadata| {
                    self.check(control).map_err(VersionError::Storage)?;
                    if !metadata.live {
                        return Ok(true);
                    }
                    let address = Address::decode(key, control)?;
                    let document = address.numbers[0];
                    if address.identity != identity
                        || *address.field != *self.field
                        || document < 0
                        || after.is_some_and(|after| document <= after)
                        || (family == Family::Vectors && u32::try_from(address.numbers[1]).is_err())
                    {
                        return Err(VersionError::InvalidEncoding(
                            "invalid native canonical corpus identity",
                        ));
                    }
                    selected = Some(document as DocId);
                    Ok(false)
                },
            )
            .map_err(VersionError::into_storage_error)?;
        Ok(selected)
    }
}

impl DiskANNCanonicalRead for RetainedSQLiteDiskANNCanonical {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.check(control)
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check(control)?;
        if after.is_some_and(|document| document >= i64::MAX as DocId) {
            return Ok(None);
        }
        let after = after.map(|document| document as i64);
        let origin = self.next_key_document(Family::VectorOrigins, after, control)?;
        let vector = self.next_key_document(Family::Vectors, after, control)?;
        self.check(control)?;
        Ok(origin.into_iter().chain(vector).min())
    }

    fn origin(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.origin(document, control)
    }

    fn visit_document(
        &self,
        document: DocId,
        control: &StorageReadControl,
        visit: &mut DiskANNCanonicalVectorVisitor<'_>,
    ) -> StorageBackendResult<Option<DiskANNVectorVersion>> {
        self.visit_document(document, control, visit)
    }
}
