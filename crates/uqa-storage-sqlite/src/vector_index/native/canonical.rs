//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Raw canonical reads preserve the native snapshot even when no `DiskANN` origin existed on it.

use super::{records::Address, SQLiteVectorIndex};
use crate::mvcc::native::{
    decode_record, NativeRecordFamily as Family, NativeRecordIdentity as Identity,
    NativeRecordOwner, NativeSnapshot,
};
use rusqlite::types::ValueRef;
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};
use uqa_storage::{
    mvcc::VersionError,
    read_control::StorageReadControl,
    vector_index::{decode_vector_bytes, VectorRead, VectorReadSnapshot},
    StorageBackendResult,
};

struct Canonical {
    snapshot: Arc<NativeSnapshot>,
    owner: Option<NativeRecordOwner>,
    table: BudgetedVec<u8>,
    field: BudgetedVec<u8>,
    dimensions: u32,
    control: StorageReadControl,
}

pub(in crate::vector_index) fn capture(
    index: &SQLiteVectorIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<VectorReadSnapshot>> {
    control.check()?;
    let Some(snapshot) = index.native_snapshot()? else {
        return Ok(None);
    };
    snapshot.control.check()?;
    let owner = snapshot.table_owner(&index.table)?;
    let mut table = BudgetedVec::new(control.memory());
    table.extend_from_slice(index.table.as_bytes())?;
    let mut field = BudgetedVec::new(control.memory());
    field.extend_from_slice(index.field.as_bytes())?;
    let source = Canonical {
        snapshot,
        owner,
        table,
        field,
        dimensions: index.dimensions,
        control: control.clone(),
    };
    Ok(Some(
        Budgeted::new(source, control.memory().empty_reservation()).into_shared()?,
    ))
}

fn invalid(message: &'static str) -> uqa_storage::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}

impl Canonical {
    fn identity(&self) -> StorageBackendResult<Option<Identity>> {
        self.owner
            .map(|owner| Identity::new(Family::Vectors, owner))
            .transpose()
            .map_err(VersionError::into_storage_error)
    }
    fn address(
        &self,
        identity: Identity,
        key: &[u8],
        control: &StorageReadControl,
    ) -> Result<(DocId, u32), VersionError> {
        let address = Address::decode(key, control)?;
        if address.identity != identity || *address.field != *self.field {
            return Err(VersionError::InvalidEncoding(
                "native canonical vector scope mismatch",
            ));
        }
        let document = u64::try_from(address.numbers[0])
            .map_err(|_| VersionError::InvalidEncoding("invalid native canonical document"))?;
        let ordinal = u32::try_from(address.numbers[1])
            .map_err(|_| VersionError::InvalidEncoding("invalid native canonical ordinal"))?;
        Ok((document, ordinal))
    }
}

impl VectorRead for Canonical {
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.check_control(control)?;
        self.owner
            .map(|owner| {
                self.snapshot
                    .vector_field_fingerprint(owner, &self.field, control)
            })
            .transpose()
    }
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        self.snapshot.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        let Some(identity) = self.identity()? else {
            return Ok(None);
        };
        if after.is_some_and(|after| after >= i64::MAX as u64) {
            return Ok(None);
        }
        let prefix = identity
            .encode_prefix(&[ValueRef::Text(&self.field)], control)
            .map_err(VersionError::into_storage_error)?;
        let after_key = after
            .map(|after| {
                identity.encode_key(
                    &[
                        ValueRef::Text(&self.field),
                        ValueRef::Integer(after as i64),
                        ValueRef::Integer(i64::MAX),
                    ],
                    control,
                )
            })
            .transpose()
            .map_err(VersionError::into_storage_error)?;
        let mut found = None;
        self.snapshot
            .view
            .visit_keys(
                &prefix,
                after_key.as_deref(),
                usize::MAX,
                control,
                &mut |key, metadata| {
                    self.check_control(control).map_err(VersionError::Storage)?;
                    if !metadata.live {
                        return Ok(true);
                    }
                    let (document, _) = self.address(identity, key, control)?;
                    if after.is_some_and(|after| document <= after) {
                        return Err(VersionError::InvalidEncoding(
                            "native vector cursor did not advance",
                        ));
                    }
                    found = Some(document);
                    Ok(false)
                },
            )
            .map_err(VersionError::into_storage_error)?;
        self.check_control(control)?;
        Ok(found)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.check_control(control)?;
        let Some(identity) = self.identity()? else {
            return Ok(0);
        };
        let document = super::super::encode_doc_id(document)?;
        let prefix = identity
            .encode_prefix(
                &[ValueRef::Text(&self.field), ValueRef::Integer(document)],
                control,
            )
            .map_err(VersionError::into_storage_error)?;
        let mut count = 0_u64;
        self.snapshot
            .view
            .visit_keys(&prefix, None, usize::MAX, control, &mut |key, metadata| {
                self.check_control(control).map_err(VersionError::Storage)?;
                if metadata.live {
                    let (actual, ordinal) = self.address(identity, key, control)?;
                    if actual != document as DocId || u64::from(ordinal) != count {
                        return Err(VersionError::InvalidEncoding(
                            "native vector ordinals are not contiguous",
                        ));
                    }
                    count += 1;
                }
                Ok(true)
            })
            .map_err(VersionError::into_storage_error)?;
        self.check_control(control)?;
        Ok(count)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.check_control(control)?;
        let Some(identity) = self.identity()? else {
            return Ok(None);
        };
        let document = super::super::encode_doc_id(document)?;
        let key = identity
            .encode_key(
                &[
                    ValueRef::Text(&self.field),
                    ValueRef::Integer(document),
                    ValueRef::Integer(i64::from(ordinal)),
                ],
                control,
            )
            .map_err(VersionError::into_storage_error)?;
        let bytes = usize::try_from(self.dimensions)
            .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?
            .checked_mul(4)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let limit =
            crate::mvcc::native::vector_row_limit(self.table.len(), self.field.len(), bytes, true)
                .map_err(VersionError::into_storage_error)?;
        let mut result = None;
        self.snapshot
            .view
            .visit_value_bounded(&key, limit, control, &mut |record| {
                self.check_control(control).map_err(VersionError::Storage)?;
                if let Some(bytes) = record.and_then(|record| record.value) {
                    let (actual, row) = decode_record(&key, bytes, control)?;
                    if actual != identity || row[0] != ValueRef::Text(&self.table) {
                        return Err(VersionError::InvalidEncoding(
                            "native vector value scope mismatch",
                        ));
                    }
                    let bytes = row[4].as_blob().map_err(|_| {
                        VersionError::Storage(invalid("invalid native vector payload"))
                    })?;
                    result = Some(
                        decode_vector_bytes(bytes, self.dimensions, control)
                            .map_err(VersionError::Storage)?,
                    );
                }
                Ok(())
            })
            .map_err(VersionError::into_storage_error)?;
        self.check_control(control)?;
        Ok(result)
    }
}
